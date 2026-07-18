//! Component-build runner provider hook.
//!
//! Core's dependency resolution (`deps`) rebuilds a component after applying a
//! dependency update. Running a component's build is extension execution
//! behavior (it drives the extension runner through the Build capability), so it
//! is inverted behind this provider: core owns the dependency-update flow, the
//! extension layer supplies the build execution.
//!
//! The build result is returned pre-serialized as JSON plus the exit code, since
//! that is all core's rebuild flow needs (it forwards the JSON to its command
//! result and gates on the exit code).
//!
//! With no provider registered (no extension subsystem present) the no-op
//! returns a not-supported error, so callers degrade gracefully.

use std::sync::Mutex;

use serde_json::Value;

use crate::component::Component;
use crate::{Error, Result};

/// Runs a component's build for the dependency-rebuild flow. Supplied by the
/// extension layer; consumed by core dependency resolution.
pub trait ComponentBuildRunner: Send + Sync {
    /// Build the component. Returns `(json_result, exit_code)`.
    fn run_component_build(&self, component: &Component) -> Result<(Value, i32)>;

    /// Whether the component has a resolvable build command (used to decide
    /// if a dependency-build lifecycle step should run at all).
    fn can_build(&self, component: &Component) -> bool;
}

fn provider_slot() -> &'static Mutex<Option<Box<dyn ComponentBuildRunner>>> {
    static PROVIDER: Mutex<Option<Box<dyn ComponentBuildRunner>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the component-build runner. Called once at startup by the extension
/// layer.
pub fn register_component_build_runner(provider: Box<dyn ComponentBuildRunner>) {
    let mut slot = provider_slot().lock().expect("component build runner lock");
    *slot = Some(provider);
}

/// Whether the registered provider can build the component.
pub fn can_build(component: &Component) -> bool {
    let slot = provider_slot().lock().expect("component build runner lock");
    match slot.as_deref() {
        Some(provider) => provider.can_build(component),
        None => false,
    }
}

/// Build a component through the registered provider. Returns
/// `(json_result, exit_code)`.
pub fn run_component_build(component: &Component) -> Result<(Value, i32)> {
    let slot = provider_slot().lock().expect("component build runner lock");
    match slot.as_deref() {
        Some(provider) => provider.run_component_build(component),
        None => Err(Error::internal_io(
            "no component-build runner registered; the extension subsystem is not available",
            None,
        )),
    }
}
