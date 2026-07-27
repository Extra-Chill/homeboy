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

    /// Build the component, returning (exit_code, error_message). Used by
    /// artifact-input resolution to build a producer component on demand.
    fn build_component(&self, component: &Component) -> (Option<i32>, Option<String>);
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ComponentBuildRunner,
    /// Register the component-build runner. Called once at startup by the extension
    /// layer.
    register: pub fn register_component_build_runner,
    /// Run `f` against the registered runner, or `None` when none is registered.
    /// This registry has no no-op implementation: each dispatch function below
    /// reports its own "no runner registered" outcome.
    with_optional: fn with_provider,
}

/// Build the component (exit_code, error) via the registered provider.
pub fn build_component(component: &Component) -> (Option<i32>, Option<String>) {
    with_provider(|runner| match runner {
        Some(runner) => runner.build_component(component),
        None => (
            None,
            Some("no component-build runner registered".to_string()),
        ),
    })
}

/// Whether the registered provider can build the component.
pub fn can_build(component: &Component) -> bool {
    with_provider(|runner| match runner {
        Some(runner) => runner.can_build(component),
        None => false,
    })
}

/// Build a component through the registered provider. Returns
/// `(json_result, exit_code)`.
pub fn run_component_build(component: &Component) -> Result<(Value, i32)> {
    with_provider(|runner| match runner {
        Some(runner) => runner.run_component_build(component),
        None => Err(Error::internal_io(
            "no component-build runner registered; the extension subsystem is not available",
            None,
        )),
    })
}
