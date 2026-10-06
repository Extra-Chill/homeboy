//! Rig toolchain command-step PATH hook.
//!
//! Building the exec environment for an extension command step prepends the
//! rig layer's built-in toolchain bin directories to `PATH`. That path assembly
//! lives in the rig crate, so it is inverted behind this provider: core owns
//! exec-env construction, the rig layer supplies the command-step PATH.
//!
//! With no provider registered (no rig layer present) the no-op contributes no
//! path, so the exec env's `PATH` is left unchanged.

use std::ffi::OsString;

/// Supplies the rig toolchain command-step PATH.
pub trait RigToolchainProvider: Send + Sync {
    /// The `PATH` value (toolchain bin dirs prepended to the current PATH) for
    /// an extension command step, or `None` when no toolchain path applies.
    fn command_step_path(&self) -> Option<OsString>;
}

struct NoopProvider;

impl RigToolchainProvider for NoopProvider {
    fn command_step_path(&self) -> Option<OsString> {
        None
    }
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn RigToolchainProvider,
    noop: NoopProvider,
    /// Register the rig toolchain provider. Called once at startup by the rig layer.
    register: pub fn register_rig_toolchain_provider,
    /// Run `f` against the registered provider, or the no-op provider if none
    /// is registered.
    with: fn with_provider,
}

/// The rig toolchain command-step PATH via the registered provider (or none when
/// the rig layer is absent).
pub fn command_step_path() -> Option<OsString> {
    with_provider(|p| p.command_step_path())
}
