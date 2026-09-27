//! Minimal read-only smart-HTTP adapter for one bound native Git view.
//!
//! The adapter deliberately implements only the two stateless upload-pack
//! endpoints. It closes each HTTP/1.1 connection after one response, which is
//! standards-compliant and keeps request parsing small and bounded.

use std::future::Future;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::Semaphore;
use tokio::task::JoinSet;
use tokio::time::timeout;

use crate::{BlobStore, GitFetchError, GitFetchService, MetadataStore};

const MAX_HTTP_HEADERS: usize = 64 * 1024;
const STREAM_BUFFER_BYTES: usize = 1024 * 1024;

/// Production resource and deadline controls for the smart-HTTP listener.
#[derive(Clone)]
#[non_exhaustive]
pub struct GitHttpOptions {
    /// Maximum accepted connections being handled simultaneously.
    pub max_connections: usize,
    /// Maximum requests generating packs simultaneously.
    pub max_pack_generations: usize,
    /// Aggregate bytes retained for upload-pack request bodies.
    pub max_inflight_request_bytes: usize,
    /// Per-pack bounded pipe between generation and socket writes.
    pub response_buffer_bytes: usize,
    /// Total time allowed to receive request headers.
    pub header_timeout: Duration,
    /// Total time allowed to queue and receive a request body.
    pub request_body_timeout: Duration,
    /// Maximum time without progress while reading request bytes.
    pub idle_timeout: Duration,
    /// Total lifetime of one accepted HTTP request.
    pub total_request_timeout: Duration,
    /// Maximum time allowed for one response socket write.
    pub response_write_timeout: Duration,
    /// Total time allowed for pack generation, including output backpressure.
    pub pack_generation_timeout: Duration,
    /// Time to drain accepted connections after listener shutdown.
    pub graceful_shutdown_timeout: Duration,
    /// Permit binding directly to a non-loopback address.
    ///
    /// Casita does not provide TLS or client authentication. Set this only
    /// behind a trusted network boundary or an authenticating TLS proxy.
    pub allow_non_loopback: bool,
    /// Optional structured completion hook, called once per accepted socket.
    pub observer: Option<Arc<dyn GitHttpObserver>>,
}

impl std::fmt::Debug for GitHttpOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("GitHttpOptions")
            .field("max_connections", &self.max_connections)
            .field("max_pack_generations", &self.max_pack_generations)
            .field(
                "max_inflight_request_bytes",
                &self.max_inflight_request_bytes,
            )
            .field("response_buffer_bytes", &self.response_buffer_bytes)
            .field("header_timeout", &self.header_timeout)
            .field("request_body_timeout", &self.request_body_timeout)
            .field("idle_timeout", &self.idle_timeout)
            .field("total_request_timeout", &self.total_request_timeout)
            .field("response_write_timeout", &self.response_write_timeout)
            .field("pack_generation_timeout", &self.pack_generation_timeout)
            .field("graceful_shutdown_timeout", &self.graceful_shutdown_timeout)
            .field("allow_non_loopback", &self.allow_non_loopback)
            .field("observer", &self.observer.as_ref().map(|_| "configured"))
            .finish()
    }
}

impl Default for GitHttpOptions {
    fn default() -> Self {
        Self {
            max_connections: 64,
            max_pack_generations: 2,
            max_inflight_request_bytes: 16 * 1024 * 1024,
            response_buffer_bytes: STREAM_BUFFER_BYTES,
            header_timeout: Duration::from_secs(10),
            request_body_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(15),
            total_request_timeout: Duration::from_secs(10 * 60),
            response_write_timeout: Duration::from_secs(30),
            pack_generation_timeout: Duration::from_secs(10 * 60),
            graceful_shutdown_timeout: Duration::from_secs(30),
            allow_non_loopback: false,
            observer: None,
        }
    }
}

/// Receives a structured outcome after an accepted connection finishes.
pub trait GitHttpObserver: Send + Sync + 'static {
    /// Record one connection outcome. Implementations must return promptly.
    fn observe(&self, event: &GitHttpEvent);
}

impl<F> GitHttpObserver for F
where
    F: Fn(&GitHttpEvent) + Send + Sync + 'static,
{
    fn observe(&self, event: &GitHttpEvent) {
        self(event);
    }
}

/// Structured completion event for logging or metrics adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitHttpEvent {
    /// Remote socket address.
    pub peer: SocketAddr,
    /// Final request or connection outcome.
    pub outcome: GitHttpOutcome,
    /// Wall-clock time spent handling this connection.
    pub elapsed: Duration,
}

/// Final outcome for one accepted smart-HTTP connection.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitHttpOutcome {
    /// One request was answered with the given status.
    Served {
        /// HTTP method.
        method: String,
        /// Exact request target.
        target: String,
        /// HTTP response status.
        status: u16,
    },
    /// The configured simultaneous-connection limit rejected the socket.
    ConnectionLimit,
    /// A configured deadline elapsed.
    TimedOut(GitHttpTimeout),
    /// The peer disconnected or another socket operation failed.
    IoFailure,
    /// Request syntax, authorization, repository, or pack processing failed.
    RequestFailure,
}

/// Deadline stage reported by [`GitHttpOutcome::TimedOut`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum GitHttpTimeout {
    /// Receiving HTTP headers.
    Headers,
    /// Waiting for aggregate body budget or receiving the request body.
    RequestBody,
    /// A socket read made no progress.
    Idle,
    /// The complete request exceeded its lifetime.
    TotalRequest,
    /// A socket response write made no progress.
    ResponseWrite,
    /// Pack generation exceeded its lifetime.
    PackGeneration,
    /// Accepted connections did not drain during shutdown.
    GracefulShutdown,
}

#[derive(Debug)]
struct ServedRequest {
    method: String,
    target: String,
    status: u16,
}

/// Listen forever and serve one exact native Git view below `route`.
///
/// `route` must be an absolute path without query or trailing slash, normally
/// `/<view>.git`. Dropping or aborting the returned future stops the listener.
pub async fn serve_git_smart_http<PS, SS>(
    listener: TcpListener,
    route: String,
    service: GitFetchService<PS, SS>,
) -> Result<(), GitHttpError>
where
    PS: BlobStore + Clone + Send + Sync + 'static,
    SS: MetadataStore + Clone + Send + Sync + 'static,
{
    serve_git_smart_http_with_shutdown(
        listener,
        route,
        service,
        GitHttpOptions::default(),
        std::future::pending(),
    )
    .await
}

/// Serve smart HTTP with explicit production limits and graceful shutdown.
///
/// Once `shutdown` resolves, the listener stops accepting connections and
/// waits up to [`GitHttpOptions::graceful_shutdown_timeout`] for accepted
/// requests to finish.
#[tracing::instrument(name = "git.http.serve_with_shutdown", skip_all)]
pub async fn serve_git_smart_http_with_shutdown<PS, SS, F>(
    listener: TcpListener,
    route: String,
    service: GitFetchService<PS, SS>,
    options: GitHttpOptions,
    shutdown: F,
) -> Result<(), GitHttpError>
where
    PS: BlobStore + Clone + Send + Sync + 'static,
    SS: MetadataStore + Clone + Send + Sync + 'static,
    F: Future<Output = ()>,
{
    validate_route(&route)?;
    options.validate(service.limits().max_request_bytes)?;
    let local_address = listener.local_addr()?;
    if !local_address.ip().is_loopback() && !options.allow_non_loopback {
        return Err(GitHttpError::NonLoopbackBind(local_address));
    }
    let connections = Arc::new(Semaphore::new(options.max_connections));
    let pack_generations = Arc::new(Semaphore::new(options.max_pack_generations));
    let request_bytes = Arc::new(Semaphore::new(options.max_inflight_request_bytes));
    let mut tasks = JoinSet::new();
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => break,
            completed = tasks.join_next(), if !tasks.is_empty() => {
                let _ = completed;
            }
            accepted = listener.accept() => {
                let (stream, peer) = accepted?;
                let Ok(connection_permit) = connections.clone().try_acquire_owned() else {
                    if let Some(observer) = &options.observer {
                        observer.observe(&GitHttpEvent {
                            peer,
                            outcome: GitHttpOutcome::ConnectionLimit,
                            elapsed: Duration::ZERO,
                        });
                    }
                    drop(stream);
                    continue;
                };
                let route = route.clone();
                let service = service.clone();
                let options = options.clone();
                let pack_generations = pack_generations.clone();
                let request_bytes = request_bytes.clone();
                tasks.spawn(async move {
                    let started = Instant::now();
                    let result = timeout(
                        options.total_request_timeout,
                        handle_connection(
                            stream,
                            &route,
                            &service,
                            &options,
                            pack_generations,
                            request_bytes,
                        ),
                    )
                    .await;
                    let outcome = match result {
                        Ok(Ok(served)) => GitHttpOutcome::Served {
                            method: served.method,
                            target: served.target,
                            status: served.status,
                        },
                        Err(_) => GitHttpOutcome::TimedOut(GitHttpTimeout::TotalRequest),
                        Ok(Err(GitHttpError::Timeout(stage))) => GitHttpOutcome::TimedOut(stage),
                        Ok(Err(GitHttpError::Io(_))) => GitHttpOutcome::IoFailure,
                        Ok(Err(_)) => GitHttpOutcome::RequestFailure,
                    };
                    if let Some(observer) = &options.observer {
                        observer.observe(&GitHttpEvent {
                            peer,
                            outcome,
                            elapsed: started.elapsed(),
                        });
                    }
                    drop(connection_permit);
                });
            }
        }
    }

    let drain = async { while tasks.join_next().await.is_some() {} };
    if timeout(options.graceful_shutdown_timeout, drain)
        .await
        .is_err()
    {
        tasks.abort_all();
        while tasks.join_next().await.is_some() {}
        return Err(GitHttpError::Timeout(GitHttpTimeout::GracefulShutdown));
    }
    Ok(())
}

/// Smart-HTTP listener or request failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum GitHttpError {
    /// Socket I/O failed.
    #[error(transparent)]
    Io(#[from] std::io::Error),
    /// Configured repository route is unsafe or ambiguous.
    #[error("invalid Git HTTP route: {0}")]
    InvalidRoute(String),
    /// HTTP request syntax or framing is invalid.
    #[error("invalid Git HTTP request: {0}")]
    InvalidRequest(String),
    /// Bound fetch service rejected the Git request.
    #[error(transparent)]
    Fetch(#[from] GitFetchError),
    /// A configured server deadline elapsed.
    #[error("Git HTTP {0:?} deadline exceeded")]
    Timeout(GitHttpTimeout),
    /// Server resource options are internally inconsistent.
    #[error("invalid Git HTTP options: {0}")]
    InvalidOptions(String),
    /// Direct non-loopback exposure was not explicitly enabled.
    #[error("Git HTTP refuses non-loopback bind {0} without explicit trust-boundary opt-in")]
    NonLoopbackBind(SocketAddr),
}

impl GitHttpOptions {
    fn validate(&self, max_request_bytes: usize) -> Result<(), GitHttpError> {
        for (name, value) in [
            ("max_connections", self.max_connections),
            ("max_pack_generations", self.max_pack_generations),
        ] {
            if value == 0 {
                return Err(GitHttpError::InvalidOptions(format!(
                    "{name} must be greater than zero"
                )));
            }
            if value > Semaphore::MAX_PERMITS {
                return Err(GitHttpError::InvalidOptions(format!(
                    "{name} exceeds Tokio's semaphore limit"
                )));
            }
        }
        if self.response_buffer_bytes == 0 {
            return Err(GitHttpError::InvalidOptions(
                "response_buffer_bytes must be greater than zero".into(),
            ));
        }
        if self.max_inflight_request_bytes < max_request_bytes {
            return Err(GitHttpError::InvalidOptions(format!(
                "max_inflight_request_bytes ({}) must cover max_request_bytes ({max_request_bytes})",
                self.max_inflight_request_bytes
            )));
        }
        if self.max_inflight_request_bytes > u32::MAX as usize
            || self.max_inflight_request_bytes > Semaphore::MAX_PERMITS
        {
            return Err(GitHttpError::InvalidOptions(
                "max_inflight_request_bytes exceeds the semaphore permit limit".into(),
            ));
        }
        self.max_pack_generations
            .checked_mul(self.response_buffer_bytes)
            .ok_or_else(|| {
                GitHttpError::InvalidOptions(
                    "aggregate streaming response buffer size overflows usize".into(),
                )
            })?;
        let deadlines = [
            ("header_timeout", self.header_timeout),
            ("request_body_timeout", self.request_body_timeout),
            ("idle_timeout", self.idle_timeout),
            ("total_request_timeout", self.total_request_timeout),
            ("response_write_timeout", self.response_write_timeout),
            ("pack_generation_timeout", self.pack_generation_timeout),
            ("graceful_shutdown_timeout", self.graceful_shutdown_timeout),
        ];
        if let Some((name, _)) = deadlines.into_iter().find(|(_, value)| value.is_zero()) {
            return Err(GitHttpError::InvalidOptions(format!(
                "{name} must be greater than zero"
            )));
        }
        Ok(())
    }
}

fn validate_route(route: &str) -> Result<(), GitHttpError> {
    if !route.starts_with('/')
        || route == "/"
        || route.ends_with('/')
        || route.contains('?')
        || route.contains('#')
        || route.contains("..")
        || route.bytes().any(|byte| byte <= 0x20 || byte == 0x7f)
    {
        return Err(GitHttpError::InvalidRoute(route.to_owned()));
    }
    Ok(())
}

#[tracing::instrument(name = "git.http.request", skip_all)]
async fn handle_connection<PS, SS>(
    mut stream: TcpStream,
    route: &str,
    service: &GitFetchService<PS, SS>,
    options: &GitHttpOptions,
    pack_generations: Arc<Semaphore>,
    request_bytes: Arc<Semaphore>,
) -> Result<ServedRequest, GitHttpError>
where
    PS: BlobStore + Clone,
    SS: MetadataStore + Clone,
{
    let mut request = Vec::new();
    let header_end = timeout(options.header_timeout, async {
        loop {
            if request.len() >= MAX_HTTP_HEADERS {
                return Ok(None);
            }
            let mut buffer = [0u8; 4096];
            let read = timeout(options.idle_timeout, stream.read(&mut buffer))
                .await
                .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::Idle))??;
            if read == 0 {
                return Err(GitHttpError::InvalidRequest(
                    "connection ended before headers".into(),
                ));
            }
            request.extend_from_slice(&buffer[..read]);
            if let Some(offset) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                return Ok(Some(offset + 4));
            }
        }
    })
    .await
    .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::Headers))??;
    let Some(header_end) = header_end else {
        write_response(
            &mut stream,
            431,
            "text/plain; charset=utf-8",
            b"request headers too large\n",
            options,
        )
        .await?;
        return Ok(ServedRequest {
            method: String::new(),
            target: String::new(),
            status: 431,
        });
    };
    let headers = std::str::from_utf8(&request[..header_end])
        .map_err(|_| GitHttpError::InvalidRequest("headers are not UTF-8/ASCII".into()))?;
    let mut lines = headers.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| GitHttpError::InvalidRequest("missing request line".into()))?;
    let mut parts = request_line.split_ascii_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| GitHttpError::InvalidRequest("missing method".into()))?
        .to_owned();
    let target = parts
        .next()
        .ok_or_else(|| GitHttpError::InvalidRequest("missing request target".into()))?
        .to_owned();
    if parts.next() != Some("HTTP/1.1") || parts.next().is_some() {
        return Err(GitHttpError::InvalidRequest(
            "only one HTTP/1.1 request line is accepted".into(),
        ));
    }
    let mut content_length = None;
    let mut expect_continue = false;
    for line in lines.filter(|line| !line.is_empty()) {
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| GitHttpError::InvalidRequest("malformed header".into()))?;
        let value = value.trim();
        if name.eq_ignore_ascii_case("content-length") {
            if content_length.is_some() {
                return Err(GitHttpError::InvalidRequest(
                    "duplicate Content-Length".into(),
                ));
            }
            content_length = Some(
                value
                    .parse::<usize>()
                    .map_err(|_| GitHttpError::InvalidRequest("invalid Content-Length".into()))?,
            );
        } else if name.eq_ignore_ascii_case("transfer-encoding") {
            return Err(GitHttpError::InvalidRequest(
                "Transfer-Encoding is not supported".into(),
            ));
        } else if name.eq_ignore_ascii_case("expect") && value.eq_ignore_ascii_case("100-continue")
        {
            expect_continue = true;
        }
    }

    if method == "GET" {
        let expected = format!("{route}/info/refs?service=git-upload-pack");
        if target != expected {
            write_response(
                &mut stream,
                404,
                "text/plain; charset=utf-8",
                b"not found\n",
                options,
            )
            .await?;
            return Ok(ServedRequest {
                method,
                target,
                status: 404,
            });
        }
        let status = match service.info_refs() {
            Ok(body) => {
                write_response(
                    &mut stream,
                    200,
                    "application/x-git-upload-pack-advertisement",
                    &body,
                    options,
                )
                .await?;
                200
            }
            Err(error) => write_fetch_error(&mut stream, error, options).await?,
        };
        return Ok(ServedRequest {
            method,
            target,
            status,
        });
    }

    if method != "POST" || target != format!("{route}/git-upload-pack") {
        write_response(
            &mut stream,
            404,
            "text/plain; charset=utf-8",
            b"not found\n",
            options,
        )
        .await?;
        return Ok(ServedRequest {
            method,
            target,
            status: 404,
        });
    }
    let content_length = content_length
        .ok_or_else(|| GitHttpError::InvalidRequest("POST requires Content-Length".into()))?;
    if content_length > service.limits().max_request_bytes {
        write_response(
            &mut stream,
            413,
            "text/plain; charset=utf-8",
            b"upload-pack request too large\n",
            options,
        )
        .await?;
        return Ok(ServedRequest {
            method,
            target,
            status: 413,
        });
    }
    if expect_continue {
        write_all_timed(
            &mut stream,
            b"HTTP/1.1 100 Continue\r\n\r\n",
            options.response_write_timeout,
        )
        .await?;
    }
    let already = request.len() - header_end;
    if already > content_length {
        return Err(GitHttpError::InvalidRequest(
            "request carries bytes beyond Content-Length".into(),
        ));
    }
    let body_permits = u32::try_from(content_length)
        .map_err(|_| GitHttpError::InvalidRequest("request body length exceeds u32".into()))?;
    let _body_budget = timeout(options.request_body_timeout, async {
        let permit = if body_permits == 0 {
            None
        } else {
            Some(
                request_bytes
                    .acquire_many_owned(body_permits)
                    .await
                    .map_err(|_| {
                        GitHttpError::InvalidOptions("request byte budget closed".into())
                    })?,
            )
        };
        request.resize(header_end + content_length, 0);
        let mut received = already;
        while received < content_length {
            let read = timeout(
                options.idle_timeout,
                stream.read(&mut request[header_end + received..header_end + content_length]),
            )
            .await
            .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::Idle))??;
            if read == 0 {
                return Err(GitHttpError::InvalidRequest(
                    "connection ended before request body".into(),
                ));
            }
            received += read;
        }
        Ok::<_, GitHttpError>(permit)
    })
    .await
    .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::RequestBody))??;
    match service.prepare_upload_pack(&request[header_end..]).await {
        Ok(prepared) => {
            let pack_deadline = prepared
                .done()
                .then(|| tokio::time::Instant::now() + options.pack_generation_timeout);
            let _pack_permit = if let Some(deadline) = pack_deadline {
                Some(
                    tokio::time::timeout_at(deadline, pack_generations.acquire_owned())
                        .await
                        .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::PackGeneration))?
                        .map_err(|_| {
                            GitHttpError::InvalidOptions("pack semaphore closed".into())
                        })?,
                )
            } else {
                None
            };
            write_streaming_upload_pack(&mut stream, service, prepared, options, pack_deadline)
                .await?;
            Ok(ServedRequest {
                method,
                target,
                status: 200,
            })
        }
        Err(error) => {
            let status = write_fetch_error(&mut stream, error, options).await?;
            Ok(ServedRequest {
                method,
                target,
                status,
            })
        }
    }
}

#[tracing::instrument(name = "git.http.stream_pack", skip_all)]
async fn write_streaming_upload_pack<PS, SS>(
    stream: &mut TcpStream,
    service: &GitFetchService<PS, SS>,
    prepared: crate::git::fetch::PreparedUploadPack,
    options: &GitHttpOptions,
    pack_deadline: Option<tokio::time::Instant>,
) -> Result<(), GitHttpError>
where
    PS: BlobStore + Clone,
    SS: MetadataStore + Clone,
{
    let content_length = prepared.response_bytes();
    write_response_header(
        stream,
        200,
        "application/x-git-upload-pack-result",
        content_length,
        options,
    )
    .await?;
    let aggregate_transport_chunks = prepared.uses_cached_pack();
    let (mut producer, mut consumer) = tokio::io::duplex(options.response_buffer_bytes);
    let generate = async {
        let result = if let Some(deadline) = pack_deadline {
            tokio::time::timeout_at(
                deadline,
                service.write_prepared_upload_pack_to(prepared, &mut producer),
            )
            .await
            .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::PackGeneration))?
        } else {
            service
                .write_prepared_upload_pack_to(prepared, &mut producer)
                .await
        };
        producer.shutdown().await?;
        result.map_err(GitHttpError::Fetch)
    };
    let send = async {
        let mut buffer = vec![0u8; options.response_buffer_bytes];
        loop {
            // Side-band payloads arrive in roughly 64 KiB writes. Fill one
            // transport buffer before emitting an HTTP chunk so a multi-GiB
            // pack does not turn into three timed socket writes per side-band
            // frame. EOF still flushes the final partial chunk.
            let mut read = 0;
            while read < buffer.len() {
                let next = consumer.read(&mut buffer[read..]).await?;
                if next == 0 {
                    break;
                }
                read += next;
                if !aggregate_transport_chunks {
                    break;
                }
            }
            if read == 0 {
                break;
            }
            if content_length.is_some() {
                write_all_timed(stream, &buffer[..read], options.response_write_timeout).await?;
            } else {
                write_all_timed(
                    stream,
                    format!("{read:x}\r\n").as_bytes(),
                    options.response_write_timeout,
                )
                .await?;
                write_all_timed(stream, &buffer[..read], options.response_write_timeout).await?;
                write_all_timed(stream, b"\r\n", options.response_write_timeout).await?;
            }
        }
        Ok::<(), GitHttpError>(())
    };
    tokio::try_join!(generate, send)?;
    if content_length.is_none() {
        write_all_timed(stream, b"0\r\n\r\n", options.response_write_timeout).await?;
    }
    timeout(options.response_write_timeout, stream.shutdown())
        .await
        .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::ResponseWrite))??;
    Ok(())
}

async fn write_fetch_error(
    stream: &mut TcpStream,
    error: GitFetchError,
    options: &GitHttpOptions,
) -> Result<u16, GitHttpError> {
    let status = match error {
        GitFetchError::UnauthorizedOid(_) => 403,
        GitFetchError::Protocol(_) | GitFetchError::Limit(_) => 400,
        _ => 500,
    };
    write_response(
        stream,
        status,
        "text/plain; charset=utf-8",
        format!("{error}\n").as_bytes(),
        options,
    )
    .await?;
    Ok(status)
}

async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    options: &GitHttpOptions,
) -> Result<(), GitHttpError> {
    write_response_header(stream, status, content_type, Some(body.len()), options).await?;
    write_all_timed(stream, body, options.response_write_timeout).await?;
    timeout(options.response_write_timeout, stream.shutdown())
        .await
        .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::ResponseWrite))??;
    Ok(())
}

async fn write_response_header(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    content_length: Option<usize>,
    options: &GitHttpOptions,
) -> Result<(), GitHttpError> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        413 => "Content Too Large",
        431 => "Request Header Fields Too Large",
        _ => "Internal Server Error",
    };
    let framing = content_length.map_or_else(
        || "Transfer-Encoding: chunked\r\n".to_owned(),
        |length| format!("Content-Length: {length}\r\n"),
    );
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\n{framing}Cache-Control: no-cache\r\nConnection: close\r\n\r\n"
    );
    write_all_timed(stream, header.as_bytes(), options.response_write_timeout).await
}

async fn write_all_timed(
    stream: &mut TcpStream,
    bytes: &[u8],
    deadline: Duration,
) -> Result<(), GitHttpError> {
    timeout(deadline, stream.write_all(bytes))
        .await
        .map_err(|_| GitHttpError::Timeout(GitHttpTimeout::ResponseWrite))??;
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    use std::process::Command;

    use super::*;
    use crate::{
        CanonicalRefName, GitFetchLimits, GitFetchRequest, NativeGitImportOptions,
        repository::Repository,
    };

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unmodified_git_client_clones_and_checks_out_a_shallow_view() {
        let source = tempfile::tempdir().unwrap();
        let git = |directory: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .current_dir(directory)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(source.path(), &["init", "-q", "-b", "main"]);
        std::fs::write(source.path().join("hello.txt"), b"first\n").unwrap();
        let mut randomish = Vec::with_capacity(256 * 1024);
        let mut state = 0x1234_5678u32;
        for _ in 0..randomish.capacity() {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            randomish.push(state as u8);
        }
        std::fs::write(source.path().join("random.bin"), &randomish).unwrap();
        git(source.path(), &["add", "hello.txt", "random.bin"]);
        git(source.path(), &["commit", "-q", "-m", "first"]);
        std::fs::write(source.path().join("hello.txt"), b"second\n").unwrap();
        git(source.path(), &["commit", "-q", "-am", "second"]);
        git(source.path(), &["gc", "--quiet"]);

        let repository = Repository::memory().unwrap();
        repository
            .import_native_git_view(
                source.path(),
                &NativeGitImportOptions {
                    view_name: "origin".into(),
                    refs: Vec::new(),
                    ..NativeGitImportOptions::default()
                },
            )
            .await
            .unwrap();
        let (_, imported_view) = crate::read_git_view(&repository, "origin")
            .await
            .unwrap()
            .unwrap();
        assert!(
            imported_view.pack.is_some(),
            "an exact source-native pack should be retained for full clones"
        );
        let uncached_repository = Repository::memory().unwrap();
        uncached_repository
            .import_native_git_view(
                source.path(),
                &NativeGitImportOptions {
                    view_name: "uncached".into(),
                    max_cached_pack_bytes: 0,
                    ..NativeGitImportOptions::default()
                },
            )
            .await
            .unwrap();
        let (_, uncached_view) = crate::read_git_view(&uncached_repository, "uncached")
            .await
            .unwrap()
            .unwrap();
        assert!(
            uncached_view.pack.is_none(),
            "a zero cache limit must not retain the exact source pack"
        );
        let service = GitFetchService::bind(&repository, "origin", GitFetchLimits::default())
            .await
            .unwrap();
        let source_pack_path = std::fs::read_dir(source.path().join(".git/objects/pack"))
            .unwrap()
            .map(Result::unwrap)
            .map(|entry| entry.path())
            .find(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "pack")
            })
            .unwrap();
        let main = imported_view
            .resolve_ref(&CanonicalRefName::try_from("refs/heads/main").unwrap())
            .unwrap();
        let cached = service
            .build_pack(&GitFetchRequest {
                wants: vec![main.native_id().to_vec()],
                haves: Vec::new(),
                depth: None,
                done: true,
                multi_ack_detailed: false,
                side_band_64k: false,
            })
            .await
            .unwrap();
        assert_eq!(cached.pack, std::fs::read(source_pack_path).unwrap());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let events = Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed = events.clone();
        let mut options = GitHttpOptions {
            max_connections: 1,
            max_pack_generations: 1,
            response_buffer_bytes: 1024,
            header_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(1),
            ..GitHttpOptions::default()
        };
        options.observer = Some(Arc::new(move |event: &GitHttpEvent| {
            observed.lock().unwrap().push(event.clone());
        }));
        let (shutdown_send, shutdown_receive) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(serve_git_smart_http_with_shutdown(
            listener,
            "/origin.git".into(),
            service,
            options,
            async {
                let _ = shutdown_receive.await;
            },
        ));

        // One deliberately idle client occupies the only connection permit;
        // the next accepted socket is rejected without spawning more work.
        let mut held = TcpStream::connect(address).await.unwrap();
        held.write_all(b"G").await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
        let mut byte = [0u8; 1];
        for _ in 0..32 {
            // Keep the permit occupied while testing overload, even when a
            // busy runner needs longer than one idle interval for the burst.
            held.write_all(b" ").await.unwrap();
            let mut excess = TcpStream::connect(address).await.unwrap();
            assert_eq!(
                timeout(Duration::from_secs(1), excess.read(&mut byte))
                    .await
                    .unwrap()
                    .unwrap(),
                0
            );
        }
        assert_eq!(
            timeout(Duration::from_secs(2), held.read(&mut byte))
                .await
                .unwrap()
                .unwrap(),
            0
        );

        let checkout_parent = tempfile::tempdir().unwrap();
        let checkout = checkout_parent.path().join("clone");
        let output = Command::new("git")
            .current_dir(checkout_parent.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args([
                "clone",
                "--quiet",
                "--depth",
                "1",
                &format!("http://{address}/origin.git"),
                checkout.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git clone: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(checkout.join("hello.txt")).unwrap(),
            b"second\n"
        );
        assert_eq!(
            std::fs::read(checkout.join("random.bin")).unwrap(),
            randomish
        );
        let count = git(&checkout, &["rev-list", "--count", "HEAD"]);
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "1");

        let full_checkout = checkout_parent.path().join("full-clone");
        let output = Command::new("git")
            .current_dir(checkout_parent.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args([
                "clone",
                "--quiet",
                &format!("http://{address}/origin.git"),
                full_checkout.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        shutdown_send.send(()).unwrap();
        server.await.unwrap().unwrap();
        {
            let events = events.lock().unwrap();
            assert_eq!(
                events
                    .iter()
                    .filter(|event| event.outcome == GitHttpOutcome::ConnectionLimit)
                    .count(),
                32
            );
            assert!(events.iter().any(|event| {
                matches!(event.outcome, GitHttpOutcome::Served { status: 200, .. })
            }));
            assert!(
                events.iter().any(|event| {
                    event.outcome == GitHttpOutcome::TimedOut(GitHttpTimeout::Idle)
                })
            );
        }
        assert!(
            output.status.success(),
            "full git clone: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(full_checkout.join("hello.txt")).unwrap(),
            b"second\n"
        );
        let count = git(&full_checkout, &["rev-list", "--count", "HEAD"]);
        assert_eq!(String::from_utf8_lossy(&count.stdout).trim(), "2");

        std::fs::write(source.path().join("hello.txt"), b"third\n").unwrap();
        git(source.path(), &["commit", "-q", "-am", "third"]);
        let tip = git(source.path(), &["rev-parse", "HEAD"]);
        let tip = String::from_utf8_lossy(&tip.stdout).trim().to_owned();
        repository
            .import_native_git_view(
                source.path(),
                &NativeGitImportOptions {
                    view_name: "origin".into(),
                    refs: Vec::new(),
                    ..NativeGitImportOptions::default()
                },
            )
            .await
            .unwrap();
        let service = GitFetchService::bind(&repository, "origin", GitFetchLimits::default())
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_git_smart_http(
            listener,
            "/origin.git".into(),
            service,
        ));
        let url = format!("http://{address}/origin.git");
        git(
            &full_checkout,
            &[
                "fetch",
                "--quiet",
                &url,
                "+refs/heads/main:refs/remotes/origin/main",
            ],
        );
        server.abort();
        let _ = server.await;

        let fetched = git(&full_checkout, &["rev-parse", "refs/remotes/origin/main"]);
        assert_eq!(String::from_utf8_lossy(&fetched.stdout).trim(), tip);
        git(
            &full_checkout,
            &["fsck", "--full", "--strict", "--no-progress"],
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn unmodified_git_client_clones_a_sha256_view() {
        let source = tempfile::tempdir().unwrap();
        let git = |directory: &std::path::Path, args: &[&str]| {
            let output = Command::new("git")
                .current_dir(directory)
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .env("GIT_AUTHOR_DATE", "2026-01-01T00:00:00Z")
                .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "git {args:?}: {}",
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        git(
            source.path(),
            &["init", "-q", "--object-format=sha256", "-b", "main"],
        );
        std::fs::write(source.path().join("hello.txt"), b"sha256\n").unwrap();
        git(source.path(), &["add", "hello.txt"]);
        git(source.path(), &["commit", "-q", "-m", "first"]);

        let repository = Repository::memory().unwrap();
        repository
            .import_native_git_view(
                source.path(),
                &NativeGitImportOptions {
                    view_name: "sha256".into(),
                    refs: Vec::new(),
                    ..NativeGitImportOptions::default()
                },
            )
            .await
            .unwrap();
        let service = GitFetchService::bind(&repository, "sha256", GitFetchLimits::default())
            .await
            .unwrap();
        assert_eq!(service.object_format(), crate::GitObjectFormat::Sha256);
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_git_smart_http(
            listener,
            "/sha256.git".into(),
            service,
        ));

        let checkout_parent = tempfile::tempdir().unwrap();
        let checkout = checkout_parent.path().join("clone");
        let output = Command::new("git")
            .current_dir(checkout_parent.path())
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args([
                "clone",
                "--quiet",
                &format!("http://{address}/sha256.git"),
                checkout.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        server.abort();
        assert!(
            output.status.success(),
            "SHA-256 git clone: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            std::fs::read(checkout.join("hello.txt")).unwrap(),
            b"sha256\n"
        );
        let format = git(&checkout, &["rev-parse", "--show-object-format"]);
        assert_eq!(String::from_utf8_lossy(&format.stdout).trim(), "sha256");
    }

    /// Exercise the full smart-HTTP update path at the scale which exposed
    /// regressions in practice. The fixture stays outside the repository: a
    /// shared bare clone reuses its object database while its private ref is
    /// moved from `HEAD^` to `HEAD`.
    ///
    /// Run with a complete local Nixpkgs checkout, for example:
    /// `CASITA_NIXPKGS_REPOSITORY=/path/to/nixpkgs cargo test --all-features
    /// cold_fetches_nixpkgs_then_one_commit -- --ignored --nocapture`.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    #[ignore = "requires a complete local nixpkgs clone and substantial disk, memory, and time"]
    async fn cold_fetches_nixpkgs_then_one_commit() {
        const VIEW: &str = "nixpkgs";
        const REF: &str = "refs/heads/casita-scale";

        let nixpkgs = std::env::var_os("CASITA_NIXPKGS_REPOSITORY")
            .map(std::path::PathBuf::from)
            .expect("set CASITA_NIXPKGS_REPOSITORY to a complete local nixpkgs checkout");

        let git = |repository: &std::path::Path, args: &[&str]| {
            let mut command = Command::new("git");
            command
                .env("GIT_CONFIG_GLOBAL", "/dev/null")
                .env("GIT_CONFIG_SYSTEM", "/dev/null")
                .env("GIT_OPTIONAL_LOCKS", "0");
            if repository.join("HEAD").is_file() && repository.join("objects").is_dir() {
                command.arg(format!("--git-dir={}", repository.display()));
            } else {
                command.arg("-C").arg(repository);
            }
            let output = command.args(args).output().unwrap();
            assert!(
                output.status.success(),
                "git -C {} {args:?}: {}",
                repository.display(),
                String::from_utf8_lossy(&output.stderr)
            );
            output
        };
        let output_text = |output: std::process::Output| {
            String::from_utf8(output.stdout).unwrap().trim().to_owned()
        };

        let tip = output_text(git(&nixpkgs, &["rev-parse", "HEAD"]));
        let parent = output_text(git(&nixpkgs, &["rev-parse", "HEAD^"]));
        assert_eq!(
            output_text(git(
                &nixpkgs,
                &["rev-list", "--count", &format!("{parent}..{tip}")]
            )),
            "1",
            "the fixture must advance the selected ref by exactly one commit"
        );

        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("nixpkgs-source.git");
        let output = Command::new("git")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_SYSTEM", "/dev/null")
            .args(["clone", "--quiet", "--shared", "--bare"])
            .arg(&nixpkgs)
            .arg(&source)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "git clone --shared --bare {}: {}",
            nixpkgs.display(),
            String::from_utf8_lossy(&output.stderr)
        );
        git(&source, &["symbolic-ref", "HEAD", REF]);
        git(&source, &["update-ref", REF, &parent]);

        let repository = Repository::local(temp.path().join("casita")).await.unwrap();
        let import_options = NativeGitImportOptions {
            view_name: VIEW.into(),
            refs: vec![CanonicalRefName::try_from(REF).unwrap()],
            ..NativeGitImportOptions::default()
        };
        let cold_import = repository
            .import_native_git_view(&source, &import_options)
            .await
            .unwrap();
        assert!(
            cold_import.objects > 10_000,
            "the fixture is unexpectedly small (only {} reachable objects)",
            cold_import.objects
        );

        // Use a manual, generously bounded test: Nixpkgs's cold pack exceeds
        // the production default response cap, by design.
        let limits = GitFetchLimits {
            max_pack_bytes: 8 * 1024 * 1024 * 1024,
            ..GitFetchLimits::default()
        };
        let service = GitFetchService::bind(&repository, VIEW, limits.clone())
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_git_smart_http(
            listener,
            "/nixpkgs.git".into(),
            service,
        ));

        let client = temp.path().join("client.git");
        git(temp.path(), &["init", "--bare", client.to_str().unwrap()]);
        let url = format!("http://{address}/nixpkgs.git");
        let refspec = format!("+{REF}:{REF}");
        git(&client, &["fetch", "--quiet", &url, &refspec]);
        assert_eq!(output_text(git(&client, &["rev-parse", REF])), parent);
        server.abort();
        let _ = server.await;

        // Move exactly the private fixture ref, then re-import and fetch the
        // one-commit delta into the existing client object database.
        git(&source, &["update-ref", REF, &tip]);
        repository
            .import_native_git_view(&source, &import_options)
            .await
            .unwrap();
        let service = GitFetchService::bind(&repository, VIEW, limits)
            .await
            .unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(serve_git_smart_http(
            listener,
            "/nixpkgs.git".into(),
            service,
        ));
        let url = format!("http://{address}/nixpkgs.git");
        git(&client, &["fetch", "--quiet", &url, &refspec]);
        server.abort();
        let _ = server.await;

        assert_eq!(output_text(git(&client, &["rev-parse", REF])), tip);
        git(&client, &["fsck", "--full", "--strict", "--no-progress"]);
    }
}
