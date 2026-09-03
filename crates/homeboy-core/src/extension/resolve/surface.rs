use super::{resolve_owner, CapabilityCatalog};
use crate::component::Component;
use crate::error::{Error, Result};

/// Ownership surface for `remote_path` auto-resolution.
pub const REMOTE_PATH_SURFACE: &str = "remote_path";
/// Ownership surface for `deploy.since_tag` placeholder rewriting.
pub const SINCE_TAG_SURFACE: &str = "since_tag";
/// Ownership surface for `provides.file_extensions` dispatch.
pub const FILE_EXTENSIONS_SURFACE: &str = "provides.file_extensions";

/// Resolve an open v1 capability from the extensions linked to `component`.
///
/// A single match wins directly. Multiple matches use the same explicit
/// `capability_extensions` and `composition.includes` ownership rule as every
/// other contested extension surface.
pub fn resolve_file_capability_provider(
    component: &Component,
    capability_id: &str,
) -> Result<Option<String>> {
    let Some(extensions) = component.extensions.as_ref() else {
        return Ok(None);
    };
    if extensions.is_empty() {
        return Ok(None);
    }
    let catalog = CapabilityCatalog::load()?;
    let (mut candidates, failures) = catalog.candidates(extensions.keys(), capability_id);

    match candidates.len() {
        0 if failures.is_empty() => Ok(None),
        0 => Err(Error::validation_invalid_argument(
            "extension",
            format!("No readable linked extension provides capability '{capability_id}'"),
            None,
            Some(
                failures
                    .into_iter()
                    .map(|(id, detail)| format!("Extension '{id}' failed to load: {detail}"))
                    .collect(),
            ),
        )),
        1 => Ok(candidates.pop()),
        _ => resolve_owner(component, FILE_EXTENSIONS_SURFACE, &candidates).map(Some),
    }
}

/// Context-free deterministic fallback for consumers without a component identity.
pub fn find_installed_capability_provider(capability_id: &str) -> Option<String> {
    CapabilityCatalog::load()
        .ok()?
        .providers(capability_id)
        .into_iter()
        .next()
}
