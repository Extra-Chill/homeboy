//! Component extension-install runner provider hook.
//!
//! Core's setup flow installs a component's extensions from a source (git URL,
//! local path, etc.). Installing extensions is extension behavior (it clones or
//! links repos and materializes manifests), so it is inverted behind this
//! provider: core owns the setup flow and the result envelope, the extension
//! layer supplies the install execution.
//!
//! With no provider registered (no extension subsystem present) the no-op
//! returns a not-supported error, so callers degrade gracefully.

use std::path::PathBuf;

use crate::component::Component;
use crate::{Error, Result};

/// A single installed extension (core-owned result envelope).
#[derive(Debug, Clone)]
pub struct InstalledExtensionResult {
    pub extension_id: String,
    pub url: String,
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub source_revision: Option<String>,
}

/// Result of installing a component's extensions from a source.
#[derive(Debug, Clone)]
pub struct ComponentInstallResult {
    pub component_id: String,
    pub source: String,
    pub installed: Vec<InstalledExtensionResult>,
    pub skipped: Vec<String>,
}

/// Installs a component's extensions from a source. Supplied by the extension
/// layer; consumed by core setup.
pub trait ComponentInstallRunner: Send + Sync {
    fn install_for_component(
        &self,
        component: &Component,
        source: &str,
    ) -> Result<ComponentInstallResult>;
}

homeboy_engine_primitives::provider_registry! {
    provider: dyn ComponentInstallRunner,
    /// Register the component-install runner. Called once at startup by the
    /// extension layer.
    register: pub fn register_component_install_runner,
    /// Run `f` against the registered runner, or `None` when none is registered.
    /// This registry has no no-op implementation: the dispatch function below
    /// reports its own "extension subsystem not available" error.
    with_optional: fn with_provider,
}

/// Install a component's extensions from a source through the registered
/// provider.
pub fn install_for_component(
    component: &Component,
    source: &str,
) -> Result<ComponentInstallResult> {
    with_provider(|runner| match runner {
        Some(runner) => runner.install_for_component(component, source),
        None => Err(Error::internal_io(
            "no component-install runner registered; the extension subsystem is not available",
            None,
        )),
    })
}
