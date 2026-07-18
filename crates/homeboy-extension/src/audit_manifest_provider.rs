//! Extension-side implementation of the audit manifest provider.
//!
//! The audit engine (`code_audit`) defines `AuditExtensionManifestProvider` and
//! calls it without depending on extension behavior. This module implements that
//! trait by loading real `ExtensionManifest`s and projecting each into the slim
//! `AuditExtensionManifest` view audit needs. It is registered at binary startup
//! by the CLI, mirroring the runner-evidence / tunnel provider hooks.

use super::manifest::ExtensionManifest;
use homeboy_core::code_audit::extension_manifests::{
    register_audit_extension_manifest_provider, AuditExtensionManifest,
    AuditExtensionManifestProvider,
};
use homeboy_core::extension_store::{load_all_extensions, load_extension};

/// Project a loaded `ExtensionManifest` into the audit-relevant view.
fn project(manifest: &ExtensionManifest) -> AuditExtensionManifest {
    AuditExtensionManifest {
        id: manifest.id.clone(),
        extension_path: manifest.extension_path.clone(),
        provided_file_extensions: manifest.provided_file_extensions().to_vec(),
        audit_detector_rules: manifest.audit_detector_rules().cloned(),
        test_mapping: manifest.test_mapping().cloned(),
        audit_ignore_claim_patterns: manifest.audit_ignore_claim_patterns().to_vec(),
        topology_script: manifest.topology_script().map(str::to_string),
    }
}

struct ExtensionManifestProvider;

impl AuditExtensionManifestProvider for ExtensionManifestProvider {
    fn load_all(&self) -> Vec<AuditExtensionManifest> {
        load_all_extensions()
            .unwrap_or_default()
            .iter()
            .map(project)
            .collect()
    }

    fn load(&self, id: &str) -> Option<AuditExtensionManifest> {
        load_extension(id).ok().map(|m| project(&m))
    }
}

/// Register the extension-backed audit manifest provider. Called once at binary
/// startup by the CLI.
pub fn register() {
    register_audit_extension_manifest_provider(Box::new(ExtensionManifestProvider));
}
