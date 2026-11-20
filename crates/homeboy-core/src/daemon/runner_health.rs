//! Daemon-driven runner health observation (#18538).
//!
//! A runner whose unit failed or keeps restarting is otherwise noticed only
//! when someone looks. The runner layer owns the probe, incident state and
//! notification; the daemon only supplies the cadence, as it does for storage
//! maintenance.

use crate::error::Result;

pub trait RunnerHealthProvider: Send + Sync {
    /// Observe every runner this host manages and announce health transitions.
    fn observe(&self) -> Result<()>;
}

struct NoopProvider;

impl RunnerHealthProvider for NoopProvider {
    fn observe(&self) -> Result<()> {
        Ok(())
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn RunnerHealthProvider,
    noop: NoopProvider,
    register: pub fn register_runner_health_provider,
    with: pub fn with_provider,
}
