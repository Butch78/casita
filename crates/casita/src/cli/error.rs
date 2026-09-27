//! CLI diagnostics and process exit classification.

pub(super) type Error = Box<dyn std::error::Error + Send + Sync + 'static>;

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub(super) struct UsageError(pub(super) String);

/// A child's failure is its own result, not a Casita diagnostic.
#[derive(Debug, thiserror::Error)]
#[error("application exited with {0}")]
pub(super) struct ApplicationExit(pub(super) std::process::ExitStatus);

pub(super) fn usage_error(message: impl Into<String>) -> Error {
    Box::new(UsageError(message.into()))
}

pub(super) fn stable_error_category(
    error: &(dyn std::error::Error + 'static),
) -> casita::experimental::RepositoryErrorCategory {
    use casita::experimental::RepositoryErrorCategory as Category;

    let mut current = Some(error);
    while let Some(error) = current {
        if let Some(error) = error.downcast_ref::<casita::Error>() {
            return error.kind();
        }
        if let Some(error) = error.downcast_ref::<casita::experimental::GitViewError>() {
            return error.category();
        }
        if let Some(error) = error.downcast_ref::<casita::experimental::TransferError>() {
            return error.category();
        }
        if let Some(error) = error.downcast_ref::<casita::experimental::RepositoryError>() {
            return error.category();
        }
        if let Some(error) = error.downcast_ref::<casita::experimental::CasitarImportError>() {
            return error.category();
        }
        if let Some(error) = error.downcast_ref::<casita::experimental::CasitarExportError>() {
            return error.category();
        }
        if let Some(error) = error.downcast_ref::<casita::experimental::CasitarStreamError>() {
            return error.category();
        }
        if let Some(error) = error.downcast_ref::<casita::experimental::TarImportError>() {
            return error.category();
        }
        if error.is::<casita::experimental::ObjectKeyError>()
            || error.is::<casita::experimental::RootNameError>()
            || error.is::<casita::experimental::DigestError>()
        {
            return Category::InvalidInput;
        }
        if let Some(error) = error.downcast_ref::<std::io::Error>() {
            return if error.kind() == std::io::ErrorKind::AlreadyExists {
                Category::DestinationConflict
            } else {
                Category::Backend
            };
        }
        current = error.source();
    }
    Category::Backend
}

pub(super) fn error_exit_code(error: &Error) -> u8 {
    if error.is::<UsageError>() { 2 } else { 1 }
}
