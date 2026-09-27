//! Bounded asynchronous streaming over the frozen Casitar v1 framing.

use std::collections::{BTreeMap, BTreeSet};
use std::io;

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use super::{
    CASITAR_MAGIC, CasitarError, CasitarFrameHeader, CasitarHeader, MAX_CASITAR_HEADER_BYTES,
    MAX_CASITAR_RECORD_BYTES, PAYLOAD_FRAME_HEADER_BYTES, PROLOGUE_BYTES,
    RECORD_FRAME_PREFIX_BYTES, TAG_PAYLOAD, TAG_RECORD,
};
use crate::{BlobId, ObjectKey, ObjectRecord, RepositoryErrorCategory};

/// Default ceiling for payload and record catalogs held by a streaming codec.
pub const DEFAULT_MAX_CASITAR_STREAM_ITEMS: usize = 1_000_000;
/// Default scratch buffer used while copying and hashing a payload body.
pub const DEFAULT_CASITAR_STREAM_BUFFER_BYTES: usize = 64 * 1024;

/// Deployment bounds applied in addition to the frozen v1 format ceilings.
///
/// Payload and archive byte defaults remain permissive because repository
/// deployments already choose their own maximum payload size. Callers that
/// accept untrusted uploads should set finite byte ceilings appropriate to the
/// deployment. Payload bytes are streamed and never allocated as one buffer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CasitarStreamLimits {
    /// Largest header body this operation accepts.
    pub max_header_bytes: usize,
    /// Largest encoded logical record this operation accepts.
    pub max_record_bytes: usize,
    /// Largest single plaintext payload body this operation accepts.
    pub max_payload_bytes: u64,
    /// Largest sum of plaintext payload bodies this operation accepts.
    pub max_total_payload_bytes: u64,
    /// Largest complete archive stream this operation accepts.
    pub max_archive_bytes: u64,
    /// Largest number of distinct payload frames this operation accepts.
    pub max_payloads: usize,
    /// Largest number of distinct logical record frames this operation accepts.
    pub max_records: usize,
    /// Scratch bytes used to copy and hash one payload body.
    pub read_buffer_bytes: usize,
}

impl Default for CasitarStreamLimits {
    fn default() -> Self {
        Self {
            max_header_bytes: MAX_CASITAR_HEADER_BYTES,
            max_record_bytes: MAX_CASITAR_RECORD_BYTES,
            max_payload_bytes: u64::MAX,
            max_total_payload_bytes: u64::MAX,
            max_archive_bytes: u64::MAX,
            max_payloads: DEFAULT_MAX_CASITAR_STREAM_ITEMS,
            max_records: DEFAULT_MAX_CASITAR_STREAM_ITEMS,
            read_buffer_bytes: DEFAULT_CASITAR_STREAM_BUFFER_BYTES,
        }
    }
}

impl CasitarStreamLimits {
    pub(super) fn validate(self) -> Result<Self, CasitarStreamError> {
        if self.read_buffer_bytes == 0 {
            return Err(CasitarStreamError::InvalidLimits(
                "read_buffer_bytes must be greater than zero",
            ));
        }
        Ok(self)
    }
}

/// Exact structural counts observed in a complete Casitar stream.
///
/// These counts do not claim that records reproduce under namespace
/// verification or that the declared roots form complete closures.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CasitarStats {
    /// Complete encoded header bytes, including magic and header length.
    pub header_bytes: u64,
    /// Number of distinct verified payload frames.
    pub payloads: u64,
    /// Sum of verified plaintext payload body bytes.
    pub payload_bytes: u64,
    /// Number of distinct canonical record frames.
    pub records: u64,
    /// Sum of canonical object-record encoding bytes.
    pub record_bytes: u64,
    /// Complete stream bytes through and including the end marker.
    pub archive_bytes: u64,
    /// BLAKE3 of the complete canonical stream, available only after strict EOF.
    pub archive_digest: Option<crate::Digest>,
}

/// One nonterminal frame discovered by [`CasitarReader::next_frame`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CasitarReadFrame {
    /// A plaintext body must now be consumed with
    /// [`CasitarReader::read_payload_to`].
    Payload {
        /// Expected BLAKE3 identity of the pending body.
        payload: BlobId,
        /// Exact number of pending plaintext bytes.
        size: u64,
    },
    /// One canonical logical record whose payload was already verified.
    Record(ObjectRecord),
}

/// Streaming Casitar I/O, sequence, or deployment-limit failure.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CasitarStreamError {
    /// Frozen v1 framing is malformed or exceeds a durable ceiling.
    #[error(transparent)]
    Format(#[from] CasitarError),
    /// Reading a source or writing a destination failed.
    #[error("Casitar stream I/O failed: {0}")]
    Io(#[from] io::Error),
    /// A caller-selected stream bound was exceeded.
    #[error("Casitar stream {field} {actual} exceeds operation limit {limit}")]
    Limit {
        /// Name of the bounded quantity.
        field: &'static str,
        /// Observed or declared value.
        actual: u64,
        /// Caller-selected operation ceiling.
        limit: u64,
    },
    /// The stream limits cannot drive a correct bounded codec.
    #[error("invalid Casitar stream limits: {0}")]
    InvalidLimits(&'static str),
    /// A second frame repeated one payload identity.
    #[error("duplicate Casitar payload frame for {0}")]
    DuplicatePayload(BlobId),
    /// A second frame repeated one logical object key.
    #[error("duplicate Casitar record frame for {0}")]
    DuplicateRecord(ObjectKey),
    /// A payload frame appeared after the canonical payload section ended.
    #[error("Casitar payload frame {0} appears after the record section began")]
    PayloadAfterRecord(BlobId),
    /// Payload identities were not in strictly ascending canonical order.
    #[error("Casitar payload frames are not strictly ordered: {current} follows {previous}")]
    NonCanonicalPayloadOrder {
        /// Immediately preceding payload identity.
        previous: BlobId,
        /// Descending payload identity that was rejected.
        current: BlobId,
    },
    /// Logical object keys were not in strictly ascending canonical order.
    #[error("Casitar record frames are not strictly ordered: {current} follows {previous}")]
    NonCanonicalRecordOrder {
        /// Immediately preceding logical object key.
        previous: ObjectKey,
        /// Descending logical object key that was rejected.
        current: ObjectKey,
    },
    /// A record appeared before its payload frame completed verification.
    #[error("Casitar record {record} appears before payload {payload}")]
    RecordBeforePayload {
        /// Logical record that violated stream order.
        record: ObjectKey,
        /// Required preceding payload.
        payload: BlobId,
    },
    /// A record's declared payload length disagrees with its payload frame.
    #[error(
        "Casitar record {record} declares payload size {record_size}, but frame {payload} has {frame_size}"
    )]
    RecordPayloadSizeMismatch {
        /// Logical record carrying the mismatched declaration.
        record: ObjectKey,
        /// Shared physical payload identity.
        payload: BlobId,
        /// Size established by the verified payload frame.
        frame_size: u64,
        /// Size declared by the logical record.
        record_size: u64,
    },
    /// A payload body's complete plaintext hash did not match its frame.
    #[error("Casitar payload identity mismatch: expected {expected}, got {actual}")]
    PayloadIdentityMismatch {
        /// Identity declared by the frame.
        expected: BlobId,
        /// Identity computed from the complete body.
        actual: BlobId,
    },
    /// A writer's payload source ended before its declared length.
    #[error("Casitar payload source ended at {actual} bytes; expected {expected}")]
    PayloadSourceTooShort {
        /// Declared exact source length.
        expected: u64,
        /// Bytes read before EOF.
        actual: u64,
    },
    /// A writer's payload source contained bytes beyond its declared length.
    #[error("Casitar payload source contains bytes beyond declared size {0}")]
    PayloadSourceTooLong(u64),
    /// The caller tried to advance while a payload body was pending.
    #[error("Casitar payload body for {0} must be consumed before the next frame")]
    PayloadPending(BlobId),
    /// The caller requested payload bytes when no payload frame was pending.
    #[error("no Casitar payload body is pending")]
    NoPayloadPending,
    /// Bytes followed the explicit end marker.
    #[error("trailing bytes after Casitar end marker")]
    TrailingArchiveBytes,
    /// The caller tried to extract a reader before consuming end and EOF.
    #[error("Casitar stream has not reached its end marker and EOF")]
    NotFinished,
    /// An earlier partial I/O or validation failure made continuation unsafe.
    #[error("Casitar stream is unusable after an earlier failure")]
    Poisoned,
}

impl CasitarStreamError {
    /// Stable frontend category for this structural stream failure.
    pub fn category(&self) -> RepositoryErrorCategory {
        use RepositoryErrorCategory as Category;

        match self {
            Self::Io(_) => Category::Backend,
            Self::Limit { .. }
            | Self::InvalidLimits(_)
            | Self::PayloadPending(_)
            | Self::NoPayloadPending
            | Self::NotFinished => Category::InvalidInput,
            Self::Format(_)
            | Self::DuplicatePayload(_)
            | Self::DuplicateRecord(_)
            | Self::PayloadAfterRecord(_)
            | Self::NonCanonicalPayloadOrder { .. }
            | Self::NonCanonicalRecordOrder { .. }
            | Self::RecordBeforePayload { .. }
            | Self::RecordPayloadSizeMismatch { .. }
            | Self::PayloadIdentityMismatch { .. }
            | Self::PayloadSourceTooShort { .. }
            | Self::PayloadSourceTooLong(_)
            | Self::TrailingArchiveBytes
            | Self::Poisoned => Category::InvalidData,
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct PendingPayload {
    payload: BlobId,
    size: u64,
}

/// Bounded asynchronous reader for one frozen Casitar v1 stream.
pub struct CasitarReader<R> {
    input: R,
    header: CasitarHeader,
    limits: CasitarStreamLimits,
    stats: CasitarStats,
    payloads: BTreeMap<BlobId, u64>,
    records: BTreeSet<ObjectKey>,
    last_payload: Option<BlobId>,
    last_record: Option<ObjectKey>,
    archive_hasher: blake3::Hasher,
    pending: Option<PendingPayload>,
    finished: bool,
    failed: bool,
}

impl<R> CasitarReader<R>
where
    R: AsyncRead + Unpin,
{
    /// Read and validate the bounded canonical archive header.
    #[tracing::instrument(name = "casitar.reader.open", level = "debug", skip_all)]
    pub async fn open(
        mut input: R,
        limits: CasitarStreamLimits,
    ) -> Result<Self, CasitarStreamError> {
        let limits = limits.validate()?;
        let mut encoded = vec![0u8; PROLOGUE_BYTES];
        read_exact_or_truncated(&mut input, &mut encoded).await?;
        if &encoded[..CASITAR_MAGIC.len()] != CASITAR_MAGIC {
            return Err(CasitarError::InvalidMagic.into());
        }

        let declared = u64::from_le_bytes(
            encoded[CASITAR_MAGIC.len()..PROLOGUE_BYTES]
                .try_into()
                .expect("the prologue contains an exact u64"),
        );
        check_frozen_limit("header bytes", declared, MAX_CASITAR_HEADER_BYTES)?;
        check_operation_limit("header bytes", declared, limits.max_header_bytes)?;
        let body_len = usize::try_from(declared).map_err(|_| CasitarError::LengthOverflow)?;
        let header_len = PROLOGUE_BYTES
            .checked_add(body_len)
            .ok_or(CasitarError::LengthOverflow)?;
        check_operation_limit(
            "archive bytes",
            usize_to_u64(header_len)?,
            limits.max_archive_bytes,
        )?;
        encoded.resize(header_len, 0);
        read_exact_or_truncated(&mut input, &mut encoded[PROLOGUE_BYTES..]).await?;

        let (header, consumed) = CasitarHeader::decode_prefix(&encoded)?;
        debug_assert_eq!(consumed, encoded.len());
        let header_bytes = usize_to_u64(encoded.len())?;
        tracing::debug!(
            roots = header.roots().len(),
            header_bytes,
            "Casitar reader opened"
        );
        Ok(Self {
            input,
            header,
            limits,
            stats: CasitarStats {
                header_bytes,
                archive_bytes: header_bytes,
                ..CasitarStats::default()
            },
            payloads: BTreeMap::new(),
            records: BTreeSet::new(),
            last_payload: None,
            last_record: None,
            archive_hasher: {
                let mut hasher = blake3::Hasher::new();
                hasher.update(&encoded);
                hasher
            },
            pending: None,
            finished: false,
            failed: false,
        })
    }

    /// Canonical roots declared by the archive header.
    pub fn header(&self) -> &CasitarHeader {
        &self.header
    }

    /// Structural progress through the bytes consumed so far.
    pub fn stats(&self) -> CasitarStats {
        self.stats
    }

    /// Decode the next frame header.
    ///
    /// `Ok(None)` means the explicit end marker and strict EOF were consumed.
    /// A payload result must be followed by [`read_payload_to`](Self::read_payload_to)
    /// before this method is called again.
    pub async fn next_frame(&mut self) -> Result<Option<CasitarReadFrame>, CasitarStreamError> {
        self.ensure_usable()?;
        if let Some(pending) = self.pending {
            return Err(CasitarStreamError::PayloadPending(pending.payload));
        }
        if self.finished {
            return Ok(None);
        }

        // Mark the reader unusable before the first await. If the future is
        // cancelled after consuming only part of a frame, dropping it leaves
        // the reader poisoned rather than pretending the stream is aligned.
        self.failed = true;
        let result = self.next_frame_inner().await;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }

    async fn next_frame_inner(&mut self) -> Result<Option<CasitarReadFrame>, CasitarStreamError> {
        let mut tag = [0u8; 1];
        self.read_accounted(&mut tag).await?;
        match tag[0] {
            0 => {
                let mut trailing = [0u8; 1];
                match self.input.read(&mut trailing).await {
                    Ok(0) => {
                        self.finished = true;
                        self.stats.archive_digest =
                            Some(crate::Digest::from(self.archive_hasher.finalize()));
                        tracing::debug!(
                            payloads = self.stats.payloads,
                            records = self.stats.records,
                            archive_bytes = self.stats.archive_bytes,
                            "Casitar reader reached verified end"
                        );
                        Ok(None)
                    }
                    Ok(_) => Err(CasitarStreamError::TrailingArchiveBytes),
                    Err(error) => Err(CasitarStreamError::Io(error)),
                }
            }
            TAG_PAYLOAD => {
                let mut encoded = [0u8; PAYLOAD_FRAME_HEADER_BYTES];
                encoded[0] = TAG_PAYLOAD;
                self.read_accounted(&mut encoded[1..]).await?;
                let (frame, consumed) = CasitarFrameHeader::decode_prefix(&encoded)?;
                debug_assert_eq!(consumed, encoded.len());
                let CasitarFrameHeader::Payload { payload, size } = frame else {
                    unreachable!("the payload tag decoded as another frame")
                };

                check_operation_limit("payload bytes", size, self.limits.max_payload_bytes)?;
                let total = self
                    .stats
                    .payload_bytes
                    .checked_add(size)
                    .ok_or(CasitarError::LengthOverflow)?;
                check_operation_limit(
                    "total payload bytes",
                    total,
                    self.limits.max_total_payload_bytes,
                )?;
                self.check_archive_add(size)?;
                check_next_count(
                    "payload count",
                    self.payloads.len(),
                    self.limits.max_payloads,
                )?;
                if self.payloads.contains_key(&payload) {
                    return Err(CasitarStreamError::DuplicatePayload(payload));
                }
                if self.last_record.is_some() {
                    return Err(CasitarStreamError::PayloadAfterRecord(payload));
                }
                if let Some(previous) = self.last_payload
                    && payload < previous
                {
                    return Err(CasitarStreamError::NonCanonicalPayloadOrder {
                        previous,
                        current: payload,
                    });
                }
                self.last_payload = Some(payload);
                self.pending = Some(PendingPayload { payload, size });
                Ok(Some(CasitarReadFrame::Payload { payload, size }))
            }
            TAG_RECORD => {
                let mut prefix = [0u8; RECORD_FRAME_PREFIX_BYTES];
                prefix[0] = TAG_RECORD;
                self.read_accounted(&mut prefix[1..]).await?;
                let declared = u64::from_le_bytes(
                    prefix[1..]
                        .try_into()
                        .expect("the record prefix contains an exact u64"),
                );
                check_frozen_limit("record bytes", declared, MAX_CASITAR_RECORD_BYTES)?;
                check_operation_limit("record bytes", declared, self.limits.max_record_bytes)?;
                let record_len =
                    usize::try_from(declared).map_err(|_| CasitarError::LengthOverflow)?;
                let frame_len = RECORD_FRAME_PREFIX_BYTES
                    .checked_add(record_len)
                    .ok_or(CasitarError::LengthOverflow)?;
                // Reject the remaining frame body before allocating it. The
                // prefix is already included in `archive_bytes`.
                self.check_archive_add(declared)?;
                let mut encoded = vec![0u8; frame_len];
                encoded[..RECORD_FRAME_PREFIX_BYTES].copy_from_slice(&prefix);
                self.read_accounted(&mut encoded[RECORD_FRAME_PREFIX_BYTES..])
                    .await?;
                let (frame, consumed) = CasitarFrameHeader::decode_prefix(&encoded)?;
                debug_assert_eq!(consumed, encoded.len());
                let CasitarFrameHeader::Record(record) = frame else {
                    unreachable!("the record tag decoded as another frame")
                };

                check_next_count("record count", self.records.len(), self.limits.max_records)?;
                if self.records.contains(record.key()) {
                    return Err(CasitarStreamError::DuplicateRecord(record.key().clone()));
                }
                if let Some(previous) = &self.last_record
                    && record.key() < previous
                {
                    return Err(CasitarStreamError::NonCanonicalRecordOrder {
                        previous: previous.clone(),
                        current: record.key().clone(),
                    });
                }
                let Some(&payload_size) = self.payloads.get(&record.payload()) else {
                    return Err(CasitarStreamError::RecordBeforePayload {
                        record: record.key().clone(),
                        payload: record.payload(),
                    });
                };
                if payload_size != record.payload_size() {
                    return Err(CasitarStreamError::RecordPayloadSizeMismatch {
                        record: record.key().clone(),
                        payload: record.payload(),
                        frame_size: payload_size,
                        record_size: record.payload_size(),
                    });
                }

                self.last_record = Some(record.key().clone());
                self.records.insert(record.key().clone());
                self.stats.records = checked_increment(self.stats.records)?;
                self.stats.record_bytes = self
                    .stats
                    .record_bytes
                    .checked_add(declared)
                    .ok_or(CasitarError::LengthOverflow)?;
                Ok(Some(CasitarReadFrame::Record(record)))
            }
            other => Err(CasitarError::InvalidFrameTag(other).into()),
        }
    }

    /// Stream, hash, and verify the pending payload body into `output`.
    pub async fn read_payload_to<W>(&mut self, output: &mut W) -> Result<(), CasitarStreamError>
    where
        W: AsyncWrite + Unpin,
    {
        self.ensure_usable()?;
        let Some(pending) = self.pending.take() else {
            return Err(CasitarStreamError::NoPayloadPending);
        };

        // Taking `pending` changes the reader state. Poison before awaiting so
        // cancellation cannot expose a reader whose body position is unknown.
        self.failed = true;
        let result = self.read_payload_to_inner(pending, output).await;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }

    async fn read_payload_to_inner<W>(
        &mut self,
        pending: PendingPayload,
        output: &mut W,
    ) -> Result<(), CasitarStreamError>
    where
        W: AsyncWrite + Unpin,
    {
        let mut remaining = pending.size;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; self.limits.read_buffer_bytes];
        while remaining != 0 {
            let wanted = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = self.input.read(&mut buffer[..wanted]).await?;
            if read == 0 {
                return Err(CasitarError::Truncated.into());
            }
            output.write_all(&buffer[..read]).await?;
            hasher.update(&buffer[..read]);
            self.archive_hasher.update(&buffer[..read]);
            remaining -= read as u64;
            self.stats.archive_bytes = self
                .stats
                .archive_bytes
                .checked_add(read as u64)
                .ok_or(CasitarError::LengthOverflow)?;
        }

        let actual = BlobId::new(hasher.finalize().into());
        if actual != pending.payload {
            return Err(CasitarStreamError::PayloadIdentityMismatch {
                expected: pending.payload,
                actual,
            });
        }
        self.payloads.insert(pending.payload, pending.size);
        self.stats.payloads = checked_increment(self.stats.payloads)?;
        self.stats.payload_bytes = self
            .stats
            .payload_bytes
            .checked_add(pending.size)
            .ok_or(CasitarError::LengthOverflow)?;
        Ok(())
    }

    /// Recover the input and final structural statistics after strict EOF.
    pub fn into_inner(self) -> Result<(R, CasitarStats), CasitarStreamError> {
        if self.failed {
            return Err(CasitarStreamError::Poisoned);
        }
        if let Some(pending) = self.pending {
            return Err(CasitarStreamError::PayloadPending(pending.payload));
        }
        if !self.finished {
            return Err(CasitarStreamError::NotFinished);
        }
        Ok((self.input, self.stats))
    }

    fn ensure_usable(&self) -> Result<(), CasitarStreamError> {
        if self.failed {
            Err(CasitarStreamError::Poisoned)
        } else {
            Ok(())
        }
    }

    async fn read_accounted(&mut self, output: &mut [u8]) -> Result<(), CasitarStreamError> {
        self.check_archive_add(usize_to_u64(output.len())?)?;
        read_exact_or_truncated(&mut self.input, output).await?;
        self.archive_hasher.update(output);
        self.stats.archive_bytes = self
            .stats
            .archive_bytes
            .checked_add(usize_to_u64(output.len())?)
            .ok_or(CasitarError::LengthOverflow)?;
        Ok(())
    }

    fn check_archive_add(&self, bytes: u64) -> Result<(), CasitarStreamError> {
        let total = self
            .stats
            .archive_bytes
            .checked_add(bytes)
            .ok_or(CasitarError::LengthOverflow)?;
        check_operation_limit("archive bytes", total, self.limits.max_archive_bytes)
    }
}

/// Bounded asynchronous writer for one frozen Casitar v1 stream.
pub struct CasitarWriter<W> {
    output: W,
    header: CasitarHeader,
    limits: CasitarStreamLimits,
    stats: CasitarStats,
    payloads: BTreeMap<BlobId, u64>,
    records: BTreeSet<ObjectKey>,
    last_payload: Option<BlobId>,
    last_record: Option<ObjectKey>,
    archive_hasher: blake3::Hasher,
    failed: bool,
}

impl<W> CasitarWriter<W>
where
    W: AsyncWrite + Unpin,
{
    /// Write the canonical archive header to a new output stream.
    #[tracing::instrument(name = "casitar.writer.open", level = "debug", skip_all)]
    pub async fn new(
        mut output: W,
        header: CasitarHeader,
        limits: CasitarStreamLimits,
    ) -> Result<Self, CasitarStreamError> {
        let limits = limits.validate()?;
        let encoded = header.encode();
        let body_bytes = encoded
            .len()
            .checked_sub(PROLOGUE_BYTES)
            .expect("a constructed header includes its prologue");
        check_operation_limit(
            "header bytes",
            usize_to_u64(body_bytes)?,
            limits.max_header_bytes,
        )?;
        let header_bytes = usize_to_u64(encoded.len())?;
        let finishable_bytes = header_bytes
            .checked_add(1)
            .ok_or(CasitarError::LengthOverflow)?;
        check_operation_limit("archive bytes", finishable_bytes, limits.max_archive_bytes)?;
        output.write_all(&encoded).await?;

        let mut archive_hasher = blake3::Hasher::new();
        archive_hasher.update(&encoded);

        tracing::debug!(
            roots = header.roots().len(),
            header_bytes,
            "Casitar writer opened"
        );
        Ok(Self {
            output,
            header,
            limits,
            stats: CasitarStats {
                header_bytes,
                archive_bytes: header_bytes,
                ..CasitarStats::default()
            },
            payloads: BTreeMap::new(),
            records: BTreeSet::new(),
            last_payload: None,
            last_record: None,
            archive_hasher,
            failed: false,
        })
    }

    /// Canonical roots written in the archive header.
    pub fn header(&self) -> &CasitarHeader {
        &self.header
    }

    /// Structural progress written so far.
    pub fn stats(&self) -> CasitarStats {
        self.stats
    }

    /// Write one distinct complete plaintext payload frame.
    ///
    /// The source must end exactly at `size`; its bytes are hashed while they
    /// are copied and must reproduce `payload`.
    pub async fn write_payload<R>(
        &mut self,
        payload: BlobId,
        size: u64,
        input: &mut R,
    ) -> Result<(), CasitarStreamError>
    where
        R: AsyncRead + Unpin,
    {
        self.ensure_usable()?;
        check_operation_limit("payload bytes", size, self.limits.max_payload_bytes)?;
        let total_payload = self
            .stats
            .payload_bytes
            .checked_add(size)
            .ok_or(CasitarError::LengthOverflow)?;
        check_operation_limit(
            "total payload bytes",
            total_payload,
            self.limits.max_total_payload_bytes,
        )?;
        check_next_count(
            "payload count",
            self.payloads.len(),
            self.limits.max_payloads,
        )?;
        if self.payloads.contains_key(&payload) {
            return Err(CasitarStreamError::DuplicatePayload(payload));
        }
        if self.last_record.is_some() {
            return Err(CasitarStreamError::PayloadAfterRecord(payload));
        }
        if let Some(previous) = self.last_payload
            && payload < previous
        {
            return Err(CasitarStreamError::NonCanonicalPayloadOrder {
                previous,
                current: payload,
            });
        }

        let frame = CasitarFrameHeader::Payload { payload, size }.encode()?;
        let frame_and_body = usize_to_u64(frame.len())?
            .checked_add(size)
            .and_then(|bytes| bytes.checked_add(1))
            .ok_or(CasitarError::LengthOverflow)?;
        self.check_archive_add(frame_and_body)?;
        // Any cancellation from this point may have emitted a partial frame.
        self.failed = true;
        if let Err(error) = self.output.write_all(&frame).await {
            return Err(error.into());
        }
        self.archive_hasher.update(&frame);
        self.stats.archive_bytes = self
            .stats
            .archive_bytes
            .checked_add(usize_to_u64(frame.len())?)
            .ok_or(CasitarError::LengthOverflow)?;

        let mut remaining = size;
        let mut copied = 0u64;
        let mut hasher = blake3::Hasher::new();
        let mut buffer = vec![0u8; self.limits.read_buffer_bytes];
        while remaining != 0 {
            let wanted = usize::try_from(remaining)
                .unwrap_or(usize::MAX)
                .min(buffer.len());
            let read = match input.read(&mut buffer[..wanted]).await {
                Ok(read) => read,
                Err(error) => return Err(error.into()),
            };
            if read == 0 {
                return Err(CasitarStreamError::PayloadSourceTooShort {
                    expected: size,
                    actual: copied,
                });
            }
            if let Err(error) = self.output.write_all(&buffer[..read]).await {
                return Err(error.into());
            }
            hasher.update(&buffer[..read]);
            self.archive_hasher.update(&buffer[..read]);
            copied += read as u64;
            remaining -= read as u64;
            self.stats.archive_bytes = self
                .stats
                .archive_bytes
                .checked_add(read as u64)
                .ok_or(CasitarError::LengthOverflow)?;
        }

        let mut extra = [0u8; 1];
        match input.read(&mut extra).await {
            Ok(0) => {}
            Ok(_) => return Err(CasitarStreamError::PayloadSourceTooLong(size)),
            Err(error) => return Err(error.into()),
        }

        let actual = BlobId::new(hasher.finalize().into());
        if actual != payload {
            return Err(CasitarStreamError::PayloadIdentityMismatch {
                expected: payload,
                actual,
            });
        }
        self.payloads.insert(payload, size);
        self.last_payload = Some(payload);
        self.stats.payloads = checked_increment(self.stats.payloads)?;
        self.stats.payload_bytes = total_payload;
        self.failed = false;
        Ok(())
    }

    /// Write one distinct canonical logical record after its payload.
    pub async fn write_record(&mut self, record: &ObjectRecord) -> Result<(), CasitarStreamError> {
        self.ensure_usable()?;
        check_next_count("record count", self.records.len(), self.limits.max_records)?;
        if self.records.contains(record.key()) {
            return Err(CasitarStreamError::DuplicateRecord(record.key().clone()));
        }
        if let Some(previous) = &self.last_record
            && record.key() < previous
        {
            return Err(CasitarStreamError::NonCanonicalRecordOrder {
                previous: previous.clone(),
                current: record.key().clone(),
            });
        }
        let Some(&payload_size) = self.payloads.get(&record.payload()) else {
            return Err(CasitarStreamError::RecordBeforePayload {
                record: record.key().clone(),
                payload: record.payload(),
            });
        };
        if payload_size != record.payload_size() {
            return Err(CasitarStreamError::RecordPayloadSizeMismatch {
                record: record.key().clone(),
                payload: record.payload(),
                frame_size: payload_size,
                record_size: record.payload_size(),
            });
        }

        let frame = CasitarFrameHeader::Record(record.clone()).encode()?;
        let record_bytes = frame
            .len()
            .checked_sub(RECORD_FRAME_PREFIX_BYTES)
            .expect("a record frame includes its prefix");
        check_operation_limit(
            "record bytes",
            usize_to_u64(record_bytes)?,
            self.limits.max_record_bytes,
        )?;
        let frame_and_end = usize_to_u64(frame.len())?
            .checked_add(1)
            .ok_or(CasitarError::LengthOverflow)?;
        self.check_archive_add(frame_and_end)?;
        // A cancelled write may leave only a frame prefix in the output.
        self.failed = true;
        if let Err(error) = self.output.write_all(&frame).await {
            return Err(error.into());
        }
        self.archive_hasher.update(&frame);

        self.last_record = Some(record.key().clone());
        self.records.insert(record.key().clone());
        self.stats.records = checked_increment(self.stats.records)?;
        self.stats.record_bytes = self
            .stats
            .record_bytes
            .checked_add(usize_to_u64(record_bytes)?)
            .ok_or(CasitarError::LengthOverflow)?;
        self.stats.archive_bytes = self
            .stats
            .archive_bytes
            .checked_add(usize_to_u64(frame.len())?)
            .ok_or(CasitarError::LengthOverflow)?;
        self.failed = false;
        Ok(())
    }

    /// Write the explicit end marker, flush, and return the output and stats.
    #[tracing::instrument(name = "casitar.writer.finish", level = "debug", skip_all)]
    pub async fn finish(mut self) -> Result<(W, CasitarStats), CasitarStreamError> {
        self.ensure_usable()?;
        self.check_archive_add(1)?;
        if let Err(error) = self.output.write_all(&[0]).await {
            return Err(error.into());
        }
        self.archive_hasher.update(&[0]);
        if let Err(error) = self.output.flush().await {
            return Err(error.into());
        }
        self.stats.archive_bytes = self
            .stats
            .archive_bytes
            .checked_add(1)
            .ok_or(CasitarError::LengthOverflow)?;
        self.stats.archive_digest = Some(crate::Digest::from(self.archive_hasher.finalize()));
        tracing::debug!(
            payloads = self.stats.payloads,
            records = self.stats.records,
            archive_bytes = self.stats.archive_bytes,
            "Casitar writer finished"
        );
        Ok((self.output, self.stats))
    }

    fn ensure_usable(&self) -> Result<(), CasitarStreamError> {
        if self.failed {
            Err(CasitarStreamError::Poisoned)
        } else {
            Ok(())
        }
    }

    fn check_archive_add(&self, bytes: u64) -> Result<(), CasitarStreamError> {
        let total = self
            .stats
            .archive_bytes
            .checked_add(bytes)
            .ok_or(CasitarError::LengthOverflow)?;
        check_operation_limit("archive bytes", total, self.limits.max_archive_bytes)
    }
}

async fn read_exact_or_truncated<R>(
    input: &mut R,
    output: &mut [u8],
) -> Result<(), CasitarStreamError>
where
    R: AsyncRead + Unpin,
{
    match input.read_exact(output).await {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
            Err(CasitarError::Truncated.into())
        }
        Err(error) => Err(error.into()),
    }
}

fn check_frozen_limit(
    field: &'static str,
    actual: u64,
    limit: usize,
) -> Result<(), CasitarStreamError> {
    if actual > usize_to_u64(limit)? {
        Err(CasitarError::LengthLimit {
            field,
            actual,
            limit: usize_to_u64(limit)?,
        }
        .into())
    } else {
        Ok(())
    }
}

fn check_operation_limit(
    field: &'static str,
    actual: u64,
    limit: impl TryInto<u64>,
) -> Result<(), CasitarStreamError> {
    let limit = limit.try_into().unwrap_or(u64::MAX);
    if actual > limit {
        Err(CasitarStreamError::Limit {
            field,
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn check_next_count(
    field: &'static str,
    current: usize,
    limit: usize,
) -> Result<(), CasitarStreamError> {
    let actual = current.checked_add(1).ok_or(CasitarError::LengthOverflow)?;
    check_operation_limit(field, usize_to_u64(actual)?, limit)
}

fn checked_increment(value: u64) -> Result<u64, CasitarStreamError> {
    value
        .checked_add(1)
        .ok_or_else(|| CasitarError::LengthOverflow.into())
}

fn usize_to_u64(value: usize) -> Result<u64, CasitarStreamError> {
    u64::try_from(value).map_err(|_| CasitarError::LengthOverflow.into())
}

#[cfg(test)]
mod tests {
    use std::future::Future;
    use std::pin::Pin;
    use std::task::{Context, Poll};

    use bytes::Bytes;
    use tokio::io::ReadBuf;

    use super::*;
    use crate::{Digest, NamespaceId};

    #[derive(Debug, Default)]
    struct VecWriter(Vec<u8>);

    impl AsyncWrite for VecWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            self.0.extend_from_slice(buffer);
            Poll::Ready(Ok(buffer.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    struct FragmentedReader {
        bytes: std::io::Cursor<Vec<u8>>,
        fragment: usize,
    }

    impl FragmentedReader {
        fn new(bytes: Vec<u8>, fragment: usize) -> Self {
            Self {
                bytes: std::io::Cursor::new(bytes),
                fragment,
            }
        }
    }

    impl AsyncRead for FragmentedReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let position = self.bytes.position() as usize;
            let bytes = self.bytes.get_ref();
            if position == bytes.len() {
                return Poll::Ready(Ok(()));
            }
            let length = self
                .fragment
                .min(buffer.remaining())
                .min(bytes.len() - position);
            buffer.put_slice(&bytes[position..position + length]);
            self.bytes.set_position((position + length) as u64);
            Poll::Ready(Ok(()))
        }
    }

    struct StallingReader {
        bytes: std::io::Cursor<Vec<u8>>,
        readable: usize,
    }

    impl StallingReader {
        fn new(bytes: Vec<u8>, readable: usize) -> Self {
            Self {
                bytes: std::io::Cursor::new(bytes),
                readable,
            }
        }
    }

    impl AsyncRead for StallingReader {
        fn poll_read(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &mut ReadBuf<'_>,
        ) -> Poll<io::Result<()>> {
            let position = self.bytes.position() as usize;
            let bytes = self.bytes.get_ref();
            if position == bytes.len() {
                return Poll::Ready(Ok(()));
            }
            if position >= self.readable {
                return Poll::Pending;
            }
            let length = buffer
                .remaining()
                .min(bytes.len() - position)
                .min(self.readable - position);
            buffer.put_slice(&bytes[position..position + length]);
            self.bytes.set_position((position + length) as u64);
            Poll::Ready(Ok(()))
        }
    }

    #[derive(Debug)]
    struct StallingWriter {
        bytes: Vec<u8>,
        writable: usize,
    }

    impl StallingWriter {
        fn new(writable: usize) -> Self {
            Self {
                bytes: Vec::new(),
                writable,
            }
        }
    }

    impl AsyncWrite for StallingWriter {
        fn poll_write(
            mut self: Pin<&mut Self>,
            _cx: &mut Context<'_>,
            buffer: &[u8],
        ) -> Poll<io::Result<usize>> {
            if self.bytes.len() >= self.writable {
                return Poll::Pending;
            }
            let length = buffer.len().min(self.writable - self.bytes.len());
            self.bytes.extend_from_slice(&buffer[..length]);
            Poll::Ready(Ok(length))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }

        fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<io::Result<()>> {
            Poll::Ready(Ok(()))
        }
    }

    fn key(namespace: &str, byte: u8) -> ObjectKey {
        ObjectKey::new(
            NamespaceId::try_from(namespace).unwrap(),
            Bytes::from(vec![byte; 32]),
        )
        .unwrap()
    }

    fn limits() -> CasitarStreamLimits {
        CasitarStreamLimits {
            read_buffer_bytes: 2,
            ..CasitarStreamLimits::default()
        }
    }

    async fn sample_archive() -> (Vec<u8>, CasitarStats, Vec<ObjectRecord>, Vec<u8>) {
        let body = b"shared payload".to_vec();
        let payload = BlobId::new(Digest::hash(&body));
        let records = vec![
            ObjectRecord::new(
                key("example.first.v1", 1),
                payload,
                body.len() as u64,
                vec![],
            )
            .unwrap(),
            ObjectRecord::new(
                key("example.second.v1", 2),
                payload,
                body.len() as u64,
                vec![],
            )
            .unwrap(),
        ];
        let header =
            CasitarHeader::new(records.iter().map(|record| record.key().clone()).collect())
                .unwrap();
        let mut writer = CasitarWriter::new(VecWriter::default(), header, limits())
            .await
            .unwrap();
        let mut source = std::io::Cursor::new(body.clone());
        writer
            .write_payload(payload, body.len() as u64, &mut source)
            .await
            .unwrap();
        for record in &records {
            writer.write_record(record).await.unwrap();
        }
        let (output, stats) = writer.finish().await.unwrap();
        (output.0, stats, records, body)
    }

    #[tokio::test]
    async fn writer_and_fragmented_reader_roundtrip_with_exact_stats() {
        let (archive, expected_stats, records, body) = sample_archive().await;
        assert_eq!(expected_stats.archive_digest, Some(Digest::hash(&archive)));
        let mut reader = CasitarReader::open(FragmentedReader::new(archive, 1), limits())
            .await
            .unwrap();
        assert_eq!(reader.header().roots().len(), 2);

        let Some(CasitarReadFrame::Payload { size, .. }) = reader.next_frame().await.unwrap()
        else {
            panic!("expected payload frame")
        };
        assert_eq!(size, body.len() as u64);
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::PayloadPending(_))
        ));
        let mut payload = VecWriter::default();
        reader.read_payload_to(&mut payload).await.unwrap();
        assert_eq!(payload.0, body);
        for expected in records {
            assert_eq!(
                reader.next_frame().await.unwrap(),
                Some(CasitarReadFrame::Record(expected))
            );
        }
        assert_eq!(reader.next_frame().await.unwrap(), None);
        assert_eq!(reader.next_frame().await.unwrap(), None);
        let (_, actual_stats) = reader.into_inner().unwrap();
        assert_eq!(actual_stats, expected_stats);
        assert_eq!(actual_stats.payloads, 1);
        assert_eq!(actual_stats.records, 2);
        assert_eq!(
            actual_stats.archive_bytes as usize,
            actual_stats.header_bytes as usize
                + 41
                + body.len()
                + actual_stats.record_bytes as usize
                + 2 * 9
                + 1
        );
    }

    async fn consume_archive(bytes: Vec<u8>) -> Result<CasitarStats, CasitarStreamError> {
        let mut reader = CasitarReader::open(FragmentedReader::new(bytes, 3), limits()).await?;
        while let Some(frame) = reader.next_frame().await? {
            if matches!(frame, CasitarReadFrame::Payload { .. }) {
                reader.read_payload_to(&mut tokio::io::sink()).await?;
            }
        }
        let (_, stats) = reader.into_inner()?;
        Ok(stats)
    }

    fn append_payload_frame(archive: &mut Vec<u8>, payload: BlobId, body: &[u8]) {
        archive.extend_from_slice(
            &CasitarFrameHeader::Payload {
                payload,
                size: body.len() as u64,
            }
            .encode()
            .unwrap(),
        );
        archive.extend_from_slice(body);
    }

    fn encoded_payload_frame(payload: BlobId, body: &[u8]) -> Vec<u8> {
        let mut frame = Vec::new();
        append_payload_frame(&mut frame, payload, body);
        frame
    }

    fn two_ordered_payloads() -> Vec<(BlobId, &'static [u8])> {
        let mut payloads = vec![
            (BlobId::new(Digest::hash(b"alpha")), b"alpha".as_slice()),
            (BlobId::new(Digest::hash(b"bravo")), b"bravo".as_slice()),
        ];
        payloads.sort_by_key(|(payload, _)| *payload);
        payloads
    }

    #[tokio::test]
    async fn every_truncated_archive_prefix_fails() {
        let (archive, _, _, _) = sample_archive().await;
        for length in 0..archive.len() {
            assert!(
                consume_archive(archive[..length].to_vec()).await.is_err(),
                "accepted truncated prefix {length}"
            );
        }
        assert!(consume_archive(archive).await.is_ok());
    }

    #[tokio::test]
    async fn reader_rejects_trailing_bytes_and_duplicate_payloads() {
        let (mut archive, _, _, _) = sample_archive().await;
        archive.push(0xff);
        assert!(matches!(
            consume_archive(archive).await,
            Err(CasitarStreamError::TrailingArchiveBytes)
        ));

        let body = b"x";
        let payload = BlobId::new(Digest::hash(body));
        let header = CasitarHeader::new(vec![key("example.root.v1", 1)]).unwrap();
        let frame = CasitarFrameHeader::Payload { payload, size: 1 }
            .encode()
            .unwrap();
        let mut duplicate = header.encode();
        duplicate.extend_from_slice(&frame);
        duplicate.extend_from_slice(body);
        duplicate.extend_from_slice(&frame);
        duplicate.extend_from_slice(body);
        duplicate.push(0);

        let mut reader = CasitarReader::open(std::io::Cursor::new(duplicate), limits())
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await.unwrap(),
            Some(CasitarReadFrame::Payload { .. })
        ));
        reader
            .read_payload_to(&mut tokio::io::sink())
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::DuplicatePayload(id)) if id == payload
        ));
    }

    #[tokio::test]
    async fn reader_rejects_duplicate_records_and_payload_identity_mismatch() {
        let body = b"abc";
        let payload = BlobId::new(Digest::hash(body));
        let root = key("example.root.v1", 1);
        let record = ObjectRecord::new(root.clone(), payload, body.len() as u64, vec![]).unwrap();
        let mut archive = CasitarHeader::new(vec![root.clone()]).unwrap().encode();
        archive.extend_from_slice(
            &CasitarFrameHeader::Payload {
                payload,
                size: body.len() as u64,
            }
            .encode()
            .unwrap(),
        );
        archive.extend_from_slice(body);
        let record_frame = CasitarFrameHeader::Record(record).encode().unwrap();
        archive.extend_from_slice(&record_frame);
        archive.extend_from_slice(&record_frame);
        archive.push(0);

        let mut reader = CasitarReader::open(std::io::Cursor::new(archive), limits())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        reader
            .read_payload_to(&mut tokio::io::sink())
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await.unwrap(),
            Some(CasitarReadFrame::Record(_))
        ));
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::DuplicateRecord(key)) if key == root
        ));

        let wrong = BlobId::new(Digest::from([0x55; 32]));
        let mut archive = CasitarHeader::new(vec![key("example.root.v1", 2)])
            .unwrap()
            .encode();
        archive.extend_from_slice(
            &CasitarFrameHeader::Payload {
                payload: wrong,
                size: body.len() as u64,
            }
            .encode()
            .unwrap(),
        );
        archive.extend_from_slice(body);
        archive.push(0);
        let mut reader = CasitarReader::open(std::io::Cursor::new(archive), limits())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        assert!(matches!(
            reader.read_payload_to(&mut tokio::io::sink()).await,
            Err(CasitarStreamError::PayloadIdentityMismatch {
                expected,
                actual,
            }) if expected == wrong && actual == payload
        ));
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::Poisoned)
        ));
    }

    #[tokio::test]
    async fn reader_rejects_noncanonical_frame_order() {
        let root = key("example.root.v1", 1);
        let header = CasitarHeader::new(vec![root.clone()]).unwrap();
        let payloads = two_ordered_payloads();

        let mut descending_payloads = header.encode();
        append_payload_frame(&mut descending_payloads, payloads[1].0, payloads[1].1);
        append_payload_frame(&mut descending_payloads, payloads[0].0, payloads[0].1);
        descending_payloads.push(0);
        let mut reader = CasitarReader::open(std::io::Cursor::new(descending_payloads), limits())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        reader
            .read_payload_to(&mut tokio::io::sink())
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::NonCanonicalPayloadOrder {
                previous,
                current,
            }) if previous == payloads[1].0 && current == payloads[0].0
        ));

        let record = ObjectRecord::new(
            root.clone(),
            payloads[0].0,
            payloads[0].1.len() as u64,
            vec![],
        )
        .unwrap();
        let mut payload_after_record = header.encode();
        append_payload_frame(&mut payload_after_record, payloads[0].0, payloads[0].1);
        payload_after_record
            .extend_from_slice(&CasitarFrameHeader::Record(record).encode().unwrap());
        append_payload_frame(&mut payload_after_record, payloads[1].0, payloads[1].1);
        payload_after_record.push(0);
        let mut reader = CasitarReader::open(std::io::Cursor::new(payload_after_record), limits())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        reader
            .read_payload_to(&mut tokio::io::sink())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::PayloadAfterRecord(payload))
                if payload == payloads[1].0
        ));

        let first = key("example.record.v1", 1);
        let second = key("example.record.v1", 2);
        let first_record = ObjectRecord::new(
            first.clone(),
            payloads[0].0,
            payloads[0].1.len() as u64,
            vec![],
        )
        .unwrap();
        let second_record = ObjectRecord::new(
            second.clone(),
            payloads[0].0,
            payloads[0].1.len() as u64,
            vec![],
        )
        .unwrap();
        let mut descending_records = CasitarHeader::new(vec![first.clone(), second.clone()])
            .unwrap()
            .encode();
        append_payload_frame(&mut descending_records, payloads[0].0, payloads[0].1);
        descending_records
            .extend_from_slice(&CasitarFrameHeader::Record(second_record).encode().unwrap());
        descending_records
            .extend_from_slice(&CasitarFrameHeader::Record(first_record).encode().unwrap());
        descending_records.push(0);
        let mut reader = CasitarReader::open(std::io::Cursor::new(descending_records), limits())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        reader
            .read_payload_to(&mut tokio::io::sink())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::NonCanonicalRecordOrder {
                previous,
                current,
            }) if previous == second && current == first
        ));
    }

    #[tokio::test]
    async fn only_the_canonical_frame_permutation_is_accepted() {
        let payloads = two_ordered_payloads();
        let first = key("example.record.v1", 1);
        let second = key("example.record.v1", 2);
        let records = [
            ObjectRecord::new(
                first.clone(),
                payloads[0].0,
                payloads[0].1.len() as u64,
                vec![],
            )
            .unwrap(),
            ObjectRecord::new(
                second.clone(),
                payloads[1].0,
                payloads[1].1.len() as u64,
                vec![],
            )
            .unwrap(),
        ];
        let frames = [
            encoded_payload_frame(payloads[0].0, payloads[0].1),
            encoded_payload_frame(payloads[1].0, payloads[1].1),
            CasitarFrameHeader::Record(records[0].clone())
                .encode()
                .unwrap(),
            CasitarFrameHeader::Record(records[1].clone())
                .encode()
                .unwrap(),
        ];
        let header = CasitarHeader::new(vec![first, second]).unwrap();

        for a in 0..frames.len() {
            for b in 0..frames.len() {
                for c in 0..frames.len() {
                    for d in 0..frames.len() {
                        let permutation = [a, b, c, d];
                        if permutation.iter().copied().collect::<BTreeSet<_>>().len()
                            != frames.len()
                        {
                            continue;
                        }
                        let mut archive = header.encode();
                        for index in permutation {
                            archive.extend_from_slice(&frames[index]);
                        }
                        archive.push(0);

                        assert_eq!(
                            consume_archive(archive).await.is_ok(),
                            permutation == [0, 1, 2, 3],
                            "unexpected validity for frame permutation {permutation:?}"
                        );
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn record_requires_a_preceding_payload_with_the_same_size() {
        let body = b"abc";
        let payload = BlobId::new(Digest::hash(body));
        let root = key("example.root.v1", 1);
        let record = ObjectRecord::new(root.clone(), payload, 4, vec![]).unwrap();
        let mut archive = CasitarHeader::new(vec![root]).unwrap().encode();
        archive.extend_from_slice(&CasitarFrameHeader::Record(record.clone()).encode().unwrap());
        archive.push(0);
        let mut reader = CasitarReader::open(std::io::Cursor::new(archive), limits())
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::RecordBeforePayload { .. })
        ));

        let mut archive = CasitarHeader::new(vec![record.key().clone()])
            .unwrap()
            .encode();
        archive.extend_from_slice(
            &CasitarFrameHeader::Payload {
                payload,
                size: body.len() as u64,
            }
            .encode()
            .unwrap(),
        );
        archive.extend_from_slice(body);
        archive.extend_from_slice(&CasitarFrameHeader::Record(record).encode().unwrap());
        archive.push(0);
        let mut reader = CasitarReader::open(std::io::Cursor::new(archive), limits())
            .await
            .unwrap();
        reader.next_frame().await.unwrap();
        reader
            .read_payload_to(&mut tokio::io::sink())
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::RecordPayloadSizeMismatch { .. })
        ));
    }

    #[tokio::test]
    async fn writer_rejects_duplicate_order_and_source_length_errors() {
        let body = b"abc";
        let payload = BlobId::new(Digest::hash(body));
        let root = key("example.root.v1", 1);
        let record = ObjectRecord::new(root.clone(), payload, 3, vec![]).unwrap();
        let header = CasitarHeader::new(vec![root]).unwrap();

        let mut writer = CasitarWriter::new(VecWriter::default(), header.clone(), limits())
            .await
            .unwrap();
        assert!(matches!(
            writer.write_record(&record).await,
            Err(CasitarStreamError::RecordBeforePayload { .. })
        ));
        let mut source = std::io::Cursor::new(body.as_slice());
        writer.write_payload(payload, 3, &mut source).await.unwrap();
        let mut duplicate = std::io::Cursor::new(body.as_slice());
        assert!(matches!(
            writer.write_payload(payload, 3, &mut duplicate).await,
            Err(CasitarStreamError::DuplicatePayload(id)) if id == payload
        ));
        writer.write_record(&record).await.unwrap();
        assert!(matches!(
            writer.write_record(&record).await,
            Err(CasitarStreamError::DuplicateRecord(key)) if key == record.key().clone()
        ));
        writer.finish().await.unwrap();

        let mut writer = CasitarWriter::new(VecWriter::default(), header.clone(), limits())
            .await
            .unwrap();
        let mut short = std::io::Cursor::new(&body[..2]);
        assert!(matches!(
            writer.write_payload(payload, 3, &mut short).await,
            Err(CasitarStreamError::PayloadSourceTooShort { .. })
        ));
        assert!(matches!(
            writer.write_record(&record).await,
            Err(CasitarStreamError::Poisoned)
        ));

        let mut writer = CasitarWriter::new(VecWriter::default(), header, limits())
            .await
            .unwrap();
        let mut long = std::io::Cursor::new(b"abcd".as_slice());
        assert!(matches!(
            writer.write_payload(payload, 3, &mut long).await,
            Err(CasitarStreamError::PayloadSourceTooLong(3))
        ));

        let mut writer = CasitarWriter::new(
            VecWriter::default(),
            CasitarHeader::new(vec![key("example.root.v1", 2)]).unwrap(),
            limits(),
        )
        .await
        .unwrap();
        let wrong = BlobId::new(Digest::from([0x55; 32]));
        let mut exact = std::io::Cursor::new(body.as_slice());
        assert!(matches!(
            writer.write_payload(wrong, 3, &mut exact).await,
            Err(CasitarStreamError::PayloadIdentityMismatch {
                expected,
                actual,
            }) if expected == wrong && actual == payload
        ));
    }

    #[tokio::test]
    async fn writer_rejects_noncanonical_frame_order() {
        let payloads = two_ordered_payloads();
        let first = key("example.record.v1", 1);
        let second = key("example.record.v1", 2);
        let header = CasitarHeader::new(vec![first.clone(), second.clone()]).unwrap();

        let mut writer = CasitarWriter::new(VecWriter::default(), header.clone(), limits())
            .await
            .unwrap();
        let mut higher = std::io::Cursor::new(payloads[1].1);
        writer
            .write_payload(payloads[1].0, payloads[1].1.len() as u64, &mut higher)
            .await
            .unwrap();
        let mut lower = std::io::Cursor::new(payloads[0].1);
        assert!(matches!(
            writer
                .write_payload(payloads[0].0, payloads[0].1.len() as u64, &mut lower)
                .await,
            Err(CasitarStreamError::NonCanonicalPayloadOrder {
                previous,
                current,
            }) if previous == payloads[1].0 && current == payloads[0].0
        ));

        let first_record = ObjectRecord::new(
            first.clone(),
            payloads[0].0,
            payloads[0].1.len() as u64,
            vec![],
        )
        .unwrap();
        let second_record = ObjectRecord::new(
            second.clone(),
            payloads[0].0,
            payloads[0].1.len() as u64,
            vec![],
        )
        .unwrap();
        let mut writer = CasitarWriter::new(VecWriter::default(), header.clone(), limits())
            .await
            .unwrap();
        let mut body = std::io::Cursor::new(payloads[0].1);
        writer
            .write_payload(payloads[0].0, payloads[0].1.len() as u64, &mut body)
            .await
            .unwrap();
        writer.write_record(&second_record).await.unwrap();
        assert!(matches!(
            writer.write_record(&first_record).await,
            Err(CasitarStreamError::NonCanonicalRecordOrder {
                previous,
                current,
            }) if previous == second && current == first
        ));

        let mut writer = CasitarWriter::new(VecWriter::default(), header, limits())
            .await
            .unwrap();
        let mut first_body = std::io::Cursor::new(payloads[0].1);
        writer
            .write_payload(payloads[0].0, payloads[0].1.len() as u64, &mut first_body)
            .await
            .unwrap();
        writer.write_record(&first_record).await.unwrap();
        let mut second_body = std::io::Cursor::new(payloads[1].1);
        assert!(matches!(
            writer
                .write_payload(
                    payloads[1].0,
                    payloads[1].1.len() as u64,
                    &mut second_body,
                )
                .await,
            Err(CasitarStreamError::PayloadAfterRecord(payload))
                if payload == payloads[1].0
        ));
    }

    #[tokio::test]
    async fn cancelled_partial_io_poisons_reader_and_writer() {
        let body = b"abc";
        let payload = BlobId::new(Digest::hash(body));
        let root = key("example.root.v1", 1);
        let record = ObjectRecord::new(root.clone(), payload, body.len() as u64, vec![]).unwrap();
        let header = CasitarHeader::new(vec![root]).unwrap();
        let header_bytes = header.encode().len();
        let payload_frame = CasitarFrameHeader::Payload {
            payload,
            size: body.len() as u64,
        }
        .encode()
        .unwrap();
        let mut archive = header.encode();
        archive.extend_from_slice(&payload_frame);
        archive.extend_from_slice(body);
        archive.push(0);

        // Cancelling frame-header decoding after the tag was consumed leaves
        // the stream between framing boundaries.
        let mut reader = CasitarReader::open(
            StallingReader::new(archive.clone(), header_bytes + 1),
            limits(),
        )
        .await
        .unwrap();
        let waker = futures::task::noop_waker_ref();
        let mut context = Context::from_waker(waker);
        let mut next = Box::pin(reader.next_frame());
        assert!(matches!(next.as_mut().poll(&mut context), Poll::Pending));
        drop(next);
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::Poisoned)
        ));

        // The same rule applies after a payload body was only partly copied.
        let readable = header_bytes + payload_frame.len() + 1;
        let mut reader = CasitarReader::open(StallingReader::new(archive, readable), limits())
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await.unwrap(),
            Some(CasitarReadFrame::Payload { .. })
        ));
        let mut output = VecWriter::default();
        let mut copy = Box::pin(reader.read_payload_to(&mut output));
        assert!(matches!(copy.as_mut().poll(&mut context), Poll::Pending));
        drop(copy);
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::Poisoned)
        ));

        // A writer cancelled after emitting only the payload tag is likewise
        // unable to resume at a trustworthy frame boundary.
        let output = StallingWriter::new(header_bytes + 1);
        let mut writer = CasitarWriter::new(output, header, limits()).await.unwrap();
        let mut source = std::io::Cursor::new(body.as_slice());
        let mut write = Box::pin(writer.write_payload(payload, body.len() as u64, &mut source));
        assert!(matches!(write.as_mut().poll(&mut context), Poll::Pending));
        drop(write);
        assert!(matches!(
            writer.write_record(&record).await,
            Err(CasitarStreamError::Poisoned)
        ));
    }

    #[tokio::test]
    async fn operation_limits_apply_before_large_bodies_or_catalog_growth() {
        let (archive, _, _, _) = sample_archive().await;
        let payload_limits = CasitarStreamLimits {
            max_payload_bytes: 2,
            ..limits()
        };
        let mut reader = CasitarReader::open(std::io::Cursor::new(archive), payload_limits)
            .await
            .unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::Limit {
                field: "payload bytes",
                ..
            })
        ));

        let (archive, _, _, _) = sample_archive().await;
        let mut reader = CasitarReader::open(
            std::io::Cursor::new(archive),
            CasitarStreamLimits {
                max_payloads: 0,
                ..limits()
            },
        )
        .await
        .unwrap();
        assert!(matches!(
            reader.next_frame().await,
            Err(CasitarStreamError::Limit {
                field: "payload count",
                ..
            })
        ));

        assert!(matches!(
            CasitarReader::open(
                std::io::Cursor::new(
                    CasitarHeader::new(vec![key("example.root.v1", 1)])
                        .unwrap()
                        .encode()
                ),
                CasitarStreamLimits {
                    read_buffer_bytes: 0,
                    ..limits()
                }
            )
            .await,
            Err(CasitarStreamError::InvalidLimits(_))
        ));

        let header = CasitarHeader::new(vec![key("example.root.v1", 3)]).unwrap();
        let max_archive_bytes = header.encode().len() as u64;
        assert!(matches!(
            CasitarWriter::new(
                VecWriter::default(),
                header,
                CasitarStreamLimits {
                    max_archive_bytes,
                    ..limits()
                }
            )
            .await,
            Err(CasitarStreamError::Limit {
                field: "archive bytes",
                ..
            })
        ));
    }
}
