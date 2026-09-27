//! Runtime tracing configuration for command execution.

use tracing_subscriber::fmt::format::FmtSpan;

use super::{Error, LogFormat, usage_error};

pub(super) fn init_tracing(filter: Option<&str>, format: LogFormat) -> Result<(), Error> {
    let filter = filter
        .map(ToOwned::to_owned)
        .or_else(|| std::env::var("RUST_LOG").ok())
        .unwrap_or_else(|| "warn".to_owned());
    let filter = tracing_subscriber::EnvFilter::try_new(filter)
        .map_err(|error| usage_error(format!("invalid trace filter: {error}")))?;

    match format {
        LogFormat::Compact => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .compact()
            .try_init()
            .map_err(|error| usage_error(format!("could not initialize tracing: {error}")))?,
        LogFormat::Json => tracing_subscriber::fmt()
            .with_env_filter(filter)
            .with_writer(std::io::stderr)
            .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
            .json()
            .try_init()
            .map_err(|error| usage_error(format!("could not initialize tracing: {error}")))?,
    }
    Ok(())
}
