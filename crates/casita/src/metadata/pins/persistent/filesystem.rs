//! Filesystem requirements of the shared Linux/macOS checkpoint protocol.
use super::*;

impl FilePinStore {
    /// Rustix maps EXCHANGE to Linux RENAME_EXCHANGE and macOS RENAME_SWAP.
    /// Never emulate this with two renames: a crash could remove the active name.
    pub(super) fn exchange_checkpoint(&self, spare: &std::path::Path) -> std::io::Result<()> {
        #[cfg(test)]
        if self
            .local()
            .map_err(std::io::Error::other)?
            .deny_exchange
            .load(std::sync::atomic::Ordering::Relaxed)
        {
            return Err(unsupported_exchange());
        }
        rustix::fs::renameat_with(
            rustix::fs::CWD,
            &self.path,
            rustix::fs::CWD,
            spare,
            rustix::fs::RenameFlags::EXCHANGE,
        )
        .map_err(|error| match error {
            rustix::io::Errno::NOSYS | rustix::io::Errno::OPNOTSUPP | rustix::io::Errno::INVAL => {
                unsupported_exchange()
            }
            error => error.into(),
        })
    }
}

fn unsupported_exchange() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "local pin checkpoints require filesystem atomic exchange (RENAME_EXCHANGE/RENAME_SWAP)",
    )
}
