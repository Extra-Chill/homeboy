//! Control-plane HTTP provider hook.
//!
//! Core owns the versioned HTTP adapter. The orchestration service that
//! assembles [`ControlPlaneRun`] lives in the agent-task layer and registers
//! here so core stays agent-task-agnostic.

use homeboy_control_plane_contract::{
    ControlPlaneCapabilities, ControlPlaneError, ControlPlaneRun, RunId,
};

/// Supplies control-plane capabilities and run reads to the HTTP adapter.
pub trait ControlPlaneProvider: Send + Sync {
    fn capabilities(&self) -> ControlPlaneCapabilities {
        ControlPlaneCapabilities::this_build()
    }

    fn run(&self, requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
        Err(ControlPlaneError::not_found(
            format!("agent-task run not found: {requested_id}"),
            "homeboy agent-task active",
        ))
    }
}

struct NoopProvider;

impl ControlPlaneProvider for NoopProvider {}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ControlPlaneProvider,
    noop: NoopProvider,
    /// Register the control-plane orchestration provider. Called once at
    /// startup by the agent-task layer.
    register: pub fn register_control_plane_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

pub fn capabilities() -> ControlPlaneCapabilities {
    with_provider(|provider| provider.capabilities())
}

pub fn run(requested_id: &RunId) -> Result<ControlPlaneRun, ControlPlaneError> {
    with_provider(|provider| provider.run(requested_id))
}
