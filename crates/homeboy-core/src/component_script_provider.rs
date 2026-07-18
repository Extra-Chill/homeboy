//! Component-script runner provider hook.
//!
//! Core's dependency resolution (`deps_provider`) needs to run a component's
//! declared dependency scripts. Running component scripts is extension
//! execution behavior (it drives the extension runner), so it is inverted
//! behind this provider: core owns the dependency-resolution flow and the
//! result envelope, the extension layer supplies the script execution.
//!
//! With no provider registered (no extension subsystem present) the no-op
//! returns a not-supported error, so callers degrade gracefully.

use std::path::Path;
use std::sync::Mutex;

use homeboy_extension_contract::{ExtensionCapability, ExtensionPhaseTiming};

use crate::component::Component;
use crate::engine::resource::ExtensionChildResourceSummary;
use crate::{Error, Result};

/// Result of running a component's scripts for a capability.
///
/// A behavior-free envelope owned by core so the provider trait (and its core
/// callers) can name the return type without depending on the extension layer.
#[derive(Debug, Clone)]
pub struct ComponentScriptOutput {
    pub exit_code: i32,
    pub success: bool,
    pub stdout: String,
    pub stderr: String,
    pub timed_out: bool,
    pub child_resource: Option<ExtensionChildResourceSummary>,
    pub extension_phase_timings: Vec<ExtensionPhaseTiming>,
}

/// Runs a component's declared scripts for a capability. Supplied by the
/// extension layer; consumed by core dependency resolution.
pub trait ComponentScriptRunner: Send + Sync {
    fn run_component_scripts_with_env(
        &self,
        component: &Component,
        capability: ExtensionCapability,
        source_path: &Path,
        passthrough: bool,
        extra_env: &[(String, String)],
        script_args: &[String],
    ) -> Result<ComponentScriptOutput>;
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn ComponentScriptRunner>>> {
    static PROVIDER: Mutex<Option<Box<dyn ComponentScriptRunner>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the component-script runner. Called once at startup by the
/// extension layer.
pub fn register_component_script_runner(provider: Box<dyn ComponentScriptRunner>) {
    let mut slot = provider_slot()
        .lock()
        .expect("component script runner lock");
    *slot = Some(provider);
}

/// Run a component's scripts for a capability through the registered provider.
pub fn run_component_scripts_with_env(
    component: &Component,
    capability: ExtensionCapability,
    source_path: &Path,
    passthrough: bool,
    extra_env: &[(String, String)],
    script_args: &[String],
) -> Result<ComponentScriptOutput> {
    let slot = provider_slot()
        .lock()
        .expect("component script runner lock");
    match slot.as_deref() {
        Some(provider) => provider.run_component_scripts_with_env(
            component,
            capability,
            source_path,
            passthrough,
            extra_env,
            script_args,
        ),
        None => Err(Error::internal_io(
            "no component-script runner registered; the extension subsystem is not available",
            None,
        )),
    }
}
