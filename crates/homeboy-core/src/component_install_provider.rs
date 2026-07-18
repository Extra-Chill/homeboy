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
use std::sync::Mutex;

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

fn provider_slot() -> &'static Mutex<Option<Box<dyn ComponentInstallRunner>>> {
    static PROVIDER: Mutex<Option<Box<dyn ComponentInstallRunner>>> = Mutex::new(None);
    &PROVIDER
}

/// Register the component-install runner. Called once at startup by the
/// extension layer.
pub fn register_component_install_runner(provider: Box<dyn ComponentInstallRunner>) {
    let mut slot = provider_slot()
        .lock()
        .expect("component install runner lock");
    *slot = Some(provider);
}

/// Install a component's extensions from a source through the registered
/// provider.
pub fn install_for_component(
    component: &Component,
    source: &str,
) -> Result<ComponentInstallResult> {
    let slot = provider_slot()
        .lock()
        .expect("component install runner lock");
    match slot.as_deref() {
        Some(provider) => provider.install_for_component(component, source),
        None => Err(Error::internal_io(
            "no component-install runner registered; the extension subsystem is not available",
            None,
        )),
    }
}
