//! Rig toolchain command-step PATH hook.
//!
//! Building the exec environment for an extension command step can prepend the
//! rig toolchain's discovered bin directories (home bin dirs, nvm node bins,
//! absolute toolchain dirs) to `PATH`. That path assembly is rig-toolchain
//! behavior, so it is inverted behind this provider: core owns exec-env
//! construction, the rig layer supplies the command-step PATH.
//!
//! With no provider registered (no rig layer present) the no-op contributes no
//! path, so the exec env's `PATH` is left unchanged.

use std::ffi::OsString;
use std::sync::Mutex;

/// Supplies the rig toolchain command-step PATH.
pub trait RigToolchainProvider: Send + Sync {
    /// The `PATH` value (rig toolchain bin dirs prepended to the current PATH)
    /// for an extension command step, or `None` when no toolchain path applies.
    fn command_step_path(&self) -> Option<OsString>;
}

struct NoopProvider;

impl RigToolchainProvider for NoopProvider {
    fn command_step_path(&self) -> Option<OsString> {
        None
    }
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn RigToolchainProvider>>> {
    static PROVIDER: Mutex<Option<Box<dyn RigToolchainProvider>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the rig toolchain provider. Called once at startup by the rig layer.
pub fn register_rig_toolchain_provider(provider: Box<dyn RigToolchainProvider>) {
    let mut slot = provider_slot().lock().expect("rig toolchain provider lock");
    *slot = Some(provider);
}

/// The rig toolchain command-step PATH via the registered provider (or none when
/// the rig layer is absent).
pub(crate) fn command_step_path() -> Option<OsString> {
    let slot = provider_slot().lock().expect("rig toolchain provider lock");
    match slot.as_deref() {
        Some(provider) => provider.command_step_path(),
        None => NoopProvider.command_step_path(),
    }
}
