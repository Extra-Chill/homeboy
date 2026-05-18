use crate::core::config;
use crate::core::error::Result;
use crate::core::output::MergeOutput;
use crate::core::paths;
use std::path::PathBuf;

use super::manifest::ExtensionManifest;

pub fn load_extension(id: &str) -> Result<ExtensionManifest> {
    let mut manifest = config::load::<ExtensionManifest>(id)?;
    let extension_dir = paths::extension(id)?;
    manifest.extension_path = Some(extension_dir.to_string_lossy().to_string());
    Ok(manifest)
}

pub fn load_all_extensions() -> Result<Vec<ExtensionManifest>> {
    let extensions = config::list::<ExtensionManifest>()?;
    let mut extensions_with_paths = Vec::new();
    for mut extension in extensions {
        let extension_dir = paths::extension(&extension.id)?;
        extension.extension_path = Some(extension_dir.to_string_lossy().to_string());
        extensions_with_paths.push(extension);
    }
    Ok(extensions_with_paths)
}

pub fn find_extension_by_tool(tool: &str) -> Option<ExtensionManifest> {
    load_all_extensions().ok().and_then(|extensions| {
        extensions
            .into_iter()
            .find(|m| m.cli.as_ref().is_some_and(|c| c.tool == tool))
    })
}

/// Find a extension that handles a given file extension and has a specific capability script.
///
/// Looks through all installed extensions for one whose `provides.file_extensions` includes
/// the given extension and whose `scripts` has the requested capability configured.
///
/// Returns the extension manifest with `extension_path` populated.
pub fn find_extension_for_file_ext(ext: &str, capability: &str) -> Option<ExtensionManifest> {
    load_all_extensions().ok().and_then(|extensions| {
        extensions.into_iter().find(|m| {
            if !m.handles_file_extension(ext) {
                return false;
            }
            match capability {
                "fingerprint" => m.fingerprint_script().is_some(),
                "refactor" => m.refactor_script().is_some(),
                "audit" => m.test_mapping().is_some(),
                _ => false,
            }
        })
    })
}

pub fn extension_path(id: &str) -> PathBuf {
    paths::extension(id).unwrap_or_else(|_| PathBuf::from(id))
}

pub fn available_extension_ids() -> Vec<String> {
    config::list_ids::<ExtensionManifest>().unwrap_or_default()
}

pub fn save_manifest(manifest: &ExtensionManifest) -> Result<()> {
    config::save(manifest)
}

pub fn merge(id: Option<&str>, json_spec: &str, replace_fields: &[String]) -> Result<MergeOutput> {
    config::merge::<ExtensionManifest>(id, json_spec, replace_fields)
}

/// Check if a extension is a symlink (linked, not installed).
pub fn is_extension_linked(extension_id: &str) -> bool {
    paths::extension(extension_id)
        .map(|p| p.is_symlink())
        .unwrap_or(false)
}
