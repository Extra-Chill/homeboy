//! Extension-manifest access for the audit engine, inverted behind a provider.
//!
//! The audit engine needs a little data from installed extension manifests —
//! provided file extensions, the audit detector rules, test-mapping config,
//! doc-claim ignore patterns, the topology script, and the extension path. It
//! used to reach that by calling `crate::extension::{load_extension,
//! load_all_extensions}` directly, which coupled `code_audit` to the
//! `extension` feature layer and blocked extracting audit into its own crate.
//!
//! Instead, audit defines the slim view it needs (`AuditExtensionManifest`) plus
//! a provider trait; the extension layer registers an implementation at startup
//! (same pattern as the runner-evidence / tunnel provider hooks). When no
//! provider is registered — e.g. audit running standalone — the no-op provider
//! yields no manifests, which every call site already treats as "no extension
//! contributed anything".

use std::sync::Mutex;

use homeboy_audit_contract::AuditConfig;

use homeboy_audit_contract::TestMappingConfig;

/// The slim, owned view of an extension manifest that the audit engine needs.
///
/// Owning the data (rather than borrowing an `ExtensionManifest`) keeps the
/// provider boundary clean: the audit engine never sees the full extension type.
#[derive(Debug, Clone, Default)]
pub struct AuditExtensionManifest {
    /// Extension id.
    pub id: String,
    /// Absolute path to the extension directory, if installed on disk.
    pub extension_path: Option<String>,
    /// File extensions this extension can fingerprint/handle.
    pub provided_file_extensions: Vec<String>,
    /// Per-detector audit configuration the extension ships, if any.
    pub audit_detector_rules: Option<AuditConfig>,
    /// Test source/mapping configuration, if the extension declares one.
    pub test_mapping: Option<TestMappingConfig>,
    /// Prose doc-claim patterns the extension asks doc-drift to ignore.
    pub audit_ignore_claim_patterns: Vec<String>,
    /// Relative path to the extension's test-topology script, if any.
    pub topology_script: Option<String>,
}

/// The manifest-access contract the audit engine depends on. Implemented by the
/// extension layer and registered at startup; audit calls it without depending
/// on extension behavior.
pub trait AuditExtensionManifestProvider: Send + Sync {
    /// Load every installed extension's audit view.
    fn load_all(&self) -> Vec<AuditExtensionManifest>;

    /// Load a single extension's audit view by id.
    fn load(&self, id: &str) -> Option<AuditExtensionManifest>;
}

/// Default provider used when no extension layer is registered: no extensions,
/// so the audit engine behaves exactly as it does on a component with no
/// installed extensions.
struct NoopProvider;

impl AuditExtensionManifestProvider for NoopProvider {
    fn load_all(&self) -> Vec<AuditExtensionManifest> {
        Vec::new()
    }

    fn load(&self, _id: &str) -> Option<AuditExtensionManifest> {
        None
    }
}

static PROVIDER: Mutex<Option<Box<dyn AuditExtensionManifestProvider>>> = Mutex::new(None);

/// Register the audit manifest provider. Called once at binary startup by the
/// extension layer (via the CLI). Replaces any previously registered provider.
pub fn register_audit_extension_manifest_provider(
    provider: Box<dyn AuditExtensionManifestProvider>,
) {
    let mut guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    *guard = Some(provider);
}

/// Load every installed extension's audit view via the registered provider.
pub(crate) fn load_all_audit_manifests() -> Vec<AuditExtensionManifest> {
    with_provider(|p| p.load_all())
}

/// Load a single extension's audit view via the registered provider.
pub(crate) fn load_audit_manifest(id: &str) -> Option<AuditExtensionManifest> {
    with_provider(|p| p.load(id))
}

fn with_provider<T>(f: impl FnOnce(&dyn AuditExtensionManifestProvider) -> T) -> T {
    let guard = PROVIDER
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    match guard.as_ref() {
        Some(provider) => f(provider.as_ref()),
        None => f(&NoopProvider),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn noop_provider_yields_no_manifests() {
        let noop = NoopProvider;
        assert!(noop.load_all().is_empty());
        assert!(noop.load("anything").is_none());
    }
}
