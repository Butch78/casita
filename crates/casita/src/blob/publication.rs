//! Ownership of one durable catalog candidate until its metadata commit resolves.
use crate::error::Error;

/// The acknowledged metadata outcome for a prepared payload catalog.
#[doc(hidden)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CatalogOutcome {
    Committed,
    Aborted,
}

type Completion = Box<dyn FnOnce(CatalogOutcome) -> Result<(), Error> + Send>;

/// Durable catalog bytes and the operation that accepts or restores their
/// unpublished changes. Dropping this value aborts preparation synchronously.
///
/// The caller must retain this value until the metadata commit outcome is
/// known, even if its own waiter is cancelled. Only call `commit` after metadata
/// has acknowledged the exact candidate returned by `catalog`.
#[doc(hidden)]
#[must_use = "a prepared catalog must remain owned until metadata commit resolves"]
pub struct PreparedCatalog {
    catalog: Option<Vec<u8>>,
    completion: Option<Completion>,
}

impl PreparedCatalog {
    /// Create a candidate after all its physical dependencies are durable.
    /// Aborting must restore unpublished changes without asynchronous I/O.
    pub fn new(
        catalog: Option<Vec<u8>>,
        completion: impl FnOnce(CatalogOutcome) -> Result<(), Error> + Send + 'static,
    ) -> Self {
        Self {
            catalog,
            completion: Some(Box::new(completion)),
        }
    }

    /// A completed durability flush with no catalog change to publish.
    pub fn unchanged() -> Self {
        Self {
            catalog: None,
            completion: None,
        }
    }

    /// The exact candidate to include in the atomic metadata mutation.
    pub fn catalog(&self) -> Option<&[u8]> {
        self.catalog.as_deref()
    }

    /// Keep staging or adapter protection through completion, including abort.
    pub fn with_protection(mut self, protection: impl Send + 'static) -> Self {
        let completion = self.completion.take();
        self.completion = Some(Box::new(move |outcome| {
            let _protection = protection;
            match completion {
                Some(completion) => completion(outcome),
                None => Ok(()),
            }
        }));
        self
    }

    /// Accept the candidate after metadata acknowledges it. A completion error
    /// does not undo that durable metadata commit or trigger an abort callback.
    pub fn commit(mut self) -> Result<(), Error> {
        self.resolve(CatalogOutcome::Committed)
    }

    /// Restore the candidate's changes after metadata rejects the commit.
    pub fn abort(mut self) -> Result<(), Error> {
        self.resolve(CatalogOutcome::Aborted)
    }

    fn resolve(&mut self, outcome: CatalogOutcome) -> Result<(), Error> {
        match self.completion.take() {
            Some(completion) => completion(outcome),
            None => Ok(()),
        }
    }
}

impl Drop for PreparedCatalog {
    fn drop(&mut self) {
        if let Err(error) = self.resolve(CatalogOutcome::Aborted) {
            tracing::error!(%error, "prepared catalog abort failed");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    };

    struct Protection(Arc<AtomicBool>);
    impl Drop for Protection {
        fn drop(&mut self) {
            self.0.store(false, Ordering::SeqCst);
        }
    }

    #[test]
    fn resolution_is_once_and_precedes_protection_release() {
        for outcome in [
            Some(CatalogOutcome::Committed),
            Some(CatalogOutcome::Aborted),
            None,
        ] {
            let held = Arc::new(AtomicBool::new(true));
            let observations = Arc::new(Mutex::new(Vec::new()));
            let observed = observations.clone();
            let protected = held.clone();
            let candidate = PreparedCatalog::new(Some(vec![1]), move |outcome| {
                assert!(protected.load(Ordering::SeqCst));
                observed.lock().unwrap().push(outcome);
                Ok(())
            })
            .with_protection(Protection(held.clone()));
            match outcome {
                Some(CatalogOutcome::Committed) => candidate.commit().unwrap(),
                Some(CatalogOutcome::Aborted) => candidate.abort().unwrap(),
                None => drop(candidate),
            }
            assert_eq!(
                *observations.lock().unwrap(),
                vec![outcome.unwrap_or(CatalogOutcome::Aborted)]
            );
            assert!(!held.load(Ordering::SeqCst));
        }
    }

    #[test]
    fn failed_commit_completion_does_not_abort_acknowledged_metadata() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let calls = observed.clone();
        let candidate = PreparedCatalog::new(Some(vec![1]), move |outcome| {
            calls.lock().unwrap().push(outcome);
            Err(std::io::Error::other("injected completion failure").into())
        });
        assert!(candidate.commit().is_err());
        assert_eq!(*observed.lock().unwrap(), vec![CatalogOutcome::Committed]);
    }
}
