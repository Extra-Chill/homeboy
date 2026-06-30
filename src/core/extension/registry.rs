use crate::core::config;
use crate::core::error::{Error, ErrorCode, Result};
use crate::core::output::MergeOutput;
use crate::core::paths;
use std::path::PathBuf;

use super::manifest::ExtensionManifest;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct BrokenExtensionLink {
    pub(crate) id: String,
    pub(crate) path: PathBuf,
    pub(crate) target: PathBuf,
}

pub fn load_extension(id: &str) -> Result<ExtensionManifest> {
    if let Some(link) = broken_extension_link(id) {
        return Err(broken_extension_error(&link));
    }

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

pub(crate) fn broken_extension_links() -> Vec<BrokenExtensionLink> {
    let Ok(dir) = paths::extensions() else {
        return Vec::new();
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return Vec::new();
    };

    let mut links: Vec<BrokenExtensionLink> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let metadata = std::fs::symlink_metadata(&path).ok()?;
            if !metadata.file_type().is_symlink() || path.exists() {
                return None;
            }

            Some(BrokenExtensionLink {
                id: path.file_name()?.to_string_lossy().to_string(),
                target: std::fs::read_link(&path).ok()?,
                path,
            })
        })
        .collect();
    links.sort_by(|a, b| a.id.cmp(&b.id));
    links
}

fn broken_extension_link(id: &str) -> Option<BrokenExtensionLink> {
    let path = paths::extension(id).ok()?;
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if !metadata.file_type().is_symlink() || path.exists() {
        return None;
    }

    Some(BrokenExtensionLink {
        id: id.to_string(),
        target: std::fs::read_link(&path).ok()?,
        path,
    })
}

fn broken_extension_error(link: &BrokenExtensionLink) -> Error {
    Error::new(
        ErrorCode::ExtensionNotFound,
        format!(
            "Extension '{}' is linked but its target is missing",
            link.id
        ),
        serde_json::json!({
            "id": link.id,
            "error": "target_missing",
            "path": link.path.to_string_lossy(),
            "target": link.target.to_string_lossy(),
        }),
    )
    .with_hint(format!(
        "Relink it with: homeboy extension relink {} <path>",
        link.id
    ))
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
        .map(|p| std::fs::symlink_metadata(p).is_ok_and(|m| m.file_type().is_symlink()))
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_load_extension() {
        crate::test_support::with_isolated_home(|_| {
            assert!(load_extension("missing-extension").is_err());
        });
    }

    #[test]
    fn test_load_all_extensions() {
        crate::test_support::with_isolated_home(|_| {
            assert!(load_all_extensions().unwrap().is_empty());
        });
    }

    #[test]
    fn test_find_extension_by_tool() {
        crate::test_support::with_isolated_home(|_| {
            assert!(find_extension_by_tool("missing-tool").is_none());
        });
    }

    #[test]
    fn test_find_extension_for_file_ext() {
        crate::test_support::with_isolated_home(|_| {
            assert!(find_extension_for_file_ext("rs", "unknown-capability").is_none());
        });
    }

    #[test]
    fn test_extension_path() {
        let path = extension_path("missing-extension");
        assert!(path.ends_with("missing-extension"));
    }

    #[test]
    fn test_available_extension_ids() {
        crate::test_support::with_isolated_home(|_| {
            assert!(available_extension_ids().is_empty());
        });
    }

    #[test]
    fn test_save_manifest() {
        let _save_manifest: fn(&ExtensionManifest) -> Result<()> = save_manifest;
    }

    #[test]
    fn test_merge() {
        let _merge: fn(Option<&str>, &str, &[String]) -> Result<MergeOutput> = merge;
    }

    #[test]
    fn test_is_extension_linked() {
        crate::test_support::with_isolated_home(|_| {
            assert!(!is_extension_linked("missing-extension"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn test_broken_extension_link_detects_missing_symlink_target() {
        crate::test_support::with_isolated_home(|_| {
            let extensions_dir = paths::extensions().unwrap();
            std::fs::create_dir_all(&extensions_dir).unwrap();
            let link = extensions_dir.join("sample-runtime");
            let target = extensions_dir.join("missing-sample-runtime");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            let broken = broken_extension_link("sample-runtime").expect("broken link");
            assert_eq!(broken.id, "sample-runtime");
            assert_eq!(broken.path, link);
            assert_eq!(broken.target, target);
            assert!(is_extension_linked("sample-runtime"));

            let err = load_extension("sample-runtime").expect_err("broken link error");
            assert_eq!(err.code, ErrorCode::ExtensionNotFound);
            assert_eq!(err.details["error"], "target_missing");
            assert!(err.message.contains("target is missing"));
            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains("homeboy extension relink sample-runtime")));
        });
    }

    #[cfg(unix)]
    #[test]
    fn test_broken_extension_links_lists_missing_symlink_targets() {
        crate::test_support::with_isolated_home(|_| {
            let extensions_dir = paths::extensions().unwrap();
            std::fs::create_dir_all(&extensions_dir).unwrap();
            let link = extensions_dir.join("sample-runtime");
            let target = extensions_dir.join("missing-sample-runtime");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            let broken = broken_extension_links();
            assert_eq!(broken.len(), 1);
            assert_eq!(broken[0].id, "sample-runtime");
            assert_eq!(broken[0].target, target);
        });
    }
}
