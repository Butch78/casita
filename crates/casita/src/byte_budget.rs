//! Shared byte-weighted admission control for native storage operations.

use std::sync::Arc;

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Admission granularity. Rounding upward makes the configured limit
/// conservative while avoiding one semaphore permit per byte.
const UNIT_BYTES: usize = 64 * 1024;

/// A cloneable byte-weighted budget shared by all users of one backend or
/// operation.
#[derive(Clone)]
pub(crate) struct ByteBudget {
    semaphore: Arc<Semaphore>,
    units: u32,
}

impl ByteBudget {
    pub(crate) fn new(bytes: usize) -> Self {
        let units = units_for(bytes).max(1);
        Self {
            semaphore: Arc::new(Semaphore::new(units as usize)),
            units,
        }
    }

    /// Reserve enough units for `bytes`. An individual request larger than the
    /// complete budget reserves the complete budget instead of waiting forever.
    #[tracing::instrument(
        name = "byte_budget.reserve",
        level = "debug",
        skip_all,
        fields(requested_bytes = bytes)
    )]
    pub(crate) async fn reserve(&self, bytes: usize) -> OwnedSemaphorePermit {
        let units = units_for(bytes).max(1).min(self.units);
        let permit = self
            .semaphore
            .clone()
            .acquire_many_owned(units)
            .await
            .expect("Casita byte budgets are never closed");
        tracing::debug!(reserved_units = units, "byte budget reserved");
        permit
    }

    /// Reserve enough units for `bytes`, but only when the budget can serve
    /// them right now. A caller that already holds a reservation, or that
    /// another reader waits behind, uses this instead of waiting: budget held
    /// across a suspension point cannot be waited on without risking a cycle.
    pub(crate) fn try_reserve(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        let units = units_for(bytes).max(1).min(self.units);
        self.semaphore.clone().try_acquire_many_owned(units).ok()
    }

    /// Bytes the budget can serve without waiting. Racy by nature, so callers
    /// treat it as a hint and still handle a refused reservation.
    pub(crate) fn free_bytes(&self) -> usize {
        self.semaphore
            .available_permits()
            .saturating_mul(UNIT_BYTES)
    }
}

fn units_for(bytes: usize) -> u32 {
    let units = bytes.saturating_add(UNIT_BYTES - 1) / UNIT_BYTES;
    u32::try_from(units).unwrap_or(u32::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_oversized_reservation_uses_the_whole_budget_without_deadlock() {
        let budget = ByteBudget::new(UNIT_BYTES * 2);
        let whole = budget.reserve(usize::MAX).await;
        assert_eq!(budget.semaphore.available_permits(), 0);
        drop(whole);
        assert_eq!(budget.semaphore.available_permits(), 2);
        let _permit = budget.reserve(1).await;
    }

    #[tokio::test]
    async fn a_refused_reservation_reports_the_room_that_is_left() {
        let budget = ByteBudget::new(UNIT_BYTES * 4);
        let held = budget.try_reserve(UNIT_BYTES * 3).expect("room for three");
        assert_eq!(budget.free_bytes(), UNIT_BYTES);
        assert!(budget.try_reserve(UNIT_BYTES * 2).is_none());
        let rest = budget.try_reserve(UNIT_BYTES).expect("room for one");
        assert_eq!(budget.free_bytes(), 0);
        assert!(budget.try_reserve(1).is_none());
        drop((held, rest));
        assert_eq!(budget.free_bytes(), UNIT_BYTES * 4);
    }
}
