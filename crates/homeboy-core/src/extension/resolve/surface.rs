use std::path::Path;

use homeboy_extension_contract::ExtensionManifest;

use super::{resolve_owner, resolve_owner_in_root};
use crate::component::Component;
use crate::error::Result;
use crate::extension::catalog::{
    load_all_extensions, load_all_extensions_in_root, load_extension_in_optional_root,
};

/// Ownership surface for `remote_path` auto-resolution.
pub const REMOTE_PATH_SURFACE: &str = "remote_path";
/// Ownership surface for `deploy.since_tag` placeholder rewriting.
pub const SINCE_TAG_SURFACE: &str = "since_tag";
/// Ownership surface for `provides.file_extensions` dispatch.
pub const FILE_EXTENSIONS_SURFACE: &str = "provides.file_extensions";

/// Extension-owned behavior required for a file-type provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileExtensionCapability {
    Fingerprint,
    Refactor,
    Audit,
}

impl FileExtensionCapability {
    pub const fn label(self) -> &'static str {
        match self {
            Self::Fingerprint => "fingerprint",
            Self::Refactor => "refactor",
            Self::Audit => "audit",
        }
    }

    fn is_provided_by(self, manifest: &ExtensionManifest) -> bool {
        match self {
            Self::Fingerprint => manifest.fingerprint_script().is_some(),
            Self::Refactor => manifest.refactor_script().is_some(),
            Self::Audit => manifest.test_mapping().is_some(),
        }
    }
}

/// Resolve a file-type provider from the extensions linked to `component`.
///
/// A single match wins directly. Multiple matches use the same explicit
/// `capability_extensions` and `composition.includes` ownership rule as every
/// other contested extension surface.
pub fn resolve_file_extension(
    component: &Component,
    file_extension: &str,
    capability: FileExtensionCapability,
) -> Result<Option<ExtensionManifest>> {
    let manifests = linked_file_extension_candidates(component, file_extension, capability, None)?;
    select_file_extension_owner(component, manifests, None)
}

/// [`resolve_file_extension`] against an already-resolved config root.
pub fn resolve_file_extension_in_root(
    config_root: &Path,
    component: &Component,
    file_extension: &str,
    capability: FileExtensionCapability,
) -> Result<Option<ExtensionManifest>> {
    let manifests =
        linked_file_extension_candidates(component, file_extension, capability, Some(config_root))?;
    select_file_extension_owner(component, manifests, Some(config_root))
}

/// Context-free fallback for provider hooks that have not yet been passed a
/// component identity. Selection is deterministic over the installed catalog.
pub fn find_installed_file_extension(
    file_extension: &str,
    capability: FileExtensionCapability,
) -> Option<ExtensionManifest> {
    load_all_extensions()
        .ok()?
        .into_iter()
        .find(|manifest| handles_file_extension(manifest, file_extension, capability))
}

/// Rooted sibling of [`find_installed_file_extension`].
pub fn find_installed_file_extension_in_root(
    config_root: &Path,
    file_extension: &str,
    capability: FileExtensionCapability,
) -> Option<ExtensionManifest> {
    load_all_extensions_in_root(config_root)
        .ok()?
        .into_iter()
        .find(|manifest| handles_file_extension(manifest, file_extension, capability))
}

fn linked_file_extension_candidates(
    component: &Component,
    file_extension: &str,
    capability: FileExtensionCapability,
    config_root: Option<&Path>,
) -> Result<Vec<ExtensionManifest>> {
    let Some(extensions) = component.extensions.as_ref() else {
        return Ok(Vec::new());
    };

    let mut manifests = Vec::new();
    let mut first_failure = None;
    let mut extension_ids = extensions.keys().collect::<Vec<_>>();
    extension_ids.sort();
    for extension_id in extension_ids {
        match load_extension_in_optional_root(config_root, extension_id) {
            Ok(manifest) if handles_file_extension(&manifest, file_extension, capability) => {
                manifests.push(manifest);
            }
            Ok(_) => {}
            Err(error) if first_failure.is_none() => first_failure = Some(error),
            Err(_) => {}
        }
    }
    if manifests.is_empty() {
        if let Some(error) = first_failure {
            return Err(error);
        }
    }
    Ok(manifests)
}

fn select_file_extension_owner(
    component: &Component,
    mut manifests: Vec<ExtensionManifest>,
    config_root: Option<&Path>,
) -> Result<Option<ExtensionManifest>> {
    match manifests.len() {
        0 => Ok(None),
        1 => Ok(manifests.pop()),
        _ => {
            let candidates = manifests
                .iter()
                .map(|manifest| manifest.id.clone())
                .collect::<Vec<_>>();
            let owner = match config_root {
                Some(config_root) => resolve_owner_in_root(
                    config_root,
                    component,
                    FILE_EXTENSIONS_SURFACE,
                    &candidates,
                )?,
                None => resolve_owner(component, FILE_EXTENSIONS_SURFACE, &candidates)?,
            };
            Ok(manifests.into_iter().find(|manifest| manifest.id == owner))
        }
    }
}

fn handles_file_extension(
    manifest: &ExtensionManifest,
    file_extension: &str,
    capability: FileExtensionCapability,
) -> bool {
    manifest.handles_file_extension(file_extension) && capability.is_provided_by(manifest)
}
