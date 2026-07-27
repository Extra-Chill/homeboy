//! Rig toolchain command-step PATH hook.
//!
//! Building the exec environment for an extension command step can prepend the
//! rig toolchain's declared bin directories to `PATH`. That path assembly is
//! rig-toolchain behavior, so it is inverted behind this provider: core owns
//! exec-env construction, the rig layer supplies the command-step PATH.
//!
//! `rig_id` selects whose declaration applies. `None` means "no rig context is
//! known here", which resolves to Homeboy's built-in default. That is the
//! override seam: a caller with rig context can opt a specific rig's
//! `toolchain` declaration into the exec environment instead of inheriting a
//! process-global guess.
//!
//! With no provider registered (no rig layer present) the no-op contributes no
//! path, so the exec env's `PATH` is left unchanged.

use std::ffi::OsString;

/// Supplies the rig toolchain command-step PATH.
pub trait RigToolchainProvider: Send + Sync {
    /// The `PATH` value (rig toolchain bin dirs prepended to the current PATH)
    /// for an extension command step, or `None` when no toolchain path applies.
    ///
    /// `rig_id` names the rig whose `toolchain` declaration applies; `None`
    /// falls back to the built-in default.
    fn command_step_path(&self, rig_id: Option<&str>) -> Option<OsString>;
}

struct NoopProvider;

impl RigToolchainProvider for NoopProvider {
    fn command_step_path(&self, _rig_id: Option<&str>) -> Option<OsString> {
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
pub fn command_step_path(rig_id: Option<&str>) -> Option<OsString> {
    with_provider(|p| p.command_step_path(rig_id))
}
