//! Agent-owned storage the daemon reclaims on its maintenance cadence.
//!
//! Core cannot depend on the agent-task subsystem that owns Cook attempt
//! scratch, so the subsystem registers one bounded reclamation pass here.

use std::time::Instant;

use crate::error::Result;

pub trait StorageMaintenanceProvider: Send + Sync {
    /// Reclaim what the owner's retention policy allows, stopping by `deadline`.
    fn reclaim(&self, deadline: Instant) -> Result<()>;
}

struct NoopProvider;

impl StorageMaintenanceProvider for NoopProvider {
    fn reclaim(&self, _deadline: Instant) -> Result<()> {
        Ok(())
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn StorageMaintenanceProvider,
    noop: NoopProvider,
    register: pub fn register_storage_maintenance_provider,
    with: pub(crate) fn with_provider,
}
