use crate::config;
use crate::error::{Error, ErrorCode, Result};
use crate::output::MergeOutput;
use crate::paths;
use serde::Deserialize;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use homeboy_error::{ActionSafety, ExecutableAction};
use homeboy_extension_contract::ExtensionManifest;

mod api;
mod manifest;
mod summary;

pub use api::{api_descriptor, list_api, negotiate_api, resolve_api};
pub use manifest::{
    deployment_provider_layered_input, deployment_providers, structured_sidecar_schema_version,
    structured_sidecars,
};
pub use summary::{list_summaries, list_summaries_with, ActionSummary, ExtensionSummary};

pub const EXTENSION_RELINK_ACTION_ID: &str = "extension.relink";
pub const EXTENSION_UNINSTALL_ACTION_ID: &str = "extension.uninstall";

/// Exact lifecycle actions for repairing a dangling extension registration.
pub fn broken_extension_link_repair_actions(id: &str) -> Vec<ExecutableAction> {
    vec![
        ExecutableAction::new(
            EXTENSION_RELINK_ACTION_ID,
            format!("Relink extension '{id}'"),
            "homeboy",
            ["extension", "relink", id, "<path>"],
            ActionSafety::Mutating,
        ),
        ExecutableAction::new(
            EXTENSION_UNINSTALL_ACTION_ID,
            format!("Remove stale extension registration '{id}'"),
            "homeboy",
            ["extension", "uninstall", id],
            ActionSafety::Mutating,
        ),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrokenExtensionLink {
    pub id: String,
    pub path: PathBuf,
    pub target: PathBuf,
}

/// A safe, machine-readable explanation for an installed extension that cannot
/// be loaded. Manifest values are intentionally never included in this shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtensionManifestFailure {
    pub id: String,
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub category: &'static str,
    pub diagnostic: &'static str,
    pub symlink_target: Option<PathBuf>,
}

#[derive(Debug, Clone)]
pub enum DiscoveredExtension {
    Valid(Box<ExtensionManifest>),
    Invalid(ExtensionManifestFailure),
}

/// Load one installed extension manifest below an already-resolved config root.
///
/// This is the rooted primitive [`load_extension`] delegates to (#7505). Both
/// the broken-link probe and the manifest read resolve from the SAME root, so a
/// caller reading an injected home can never pair an injected manifest with an
/// ambient link diagnosis.
pub fn load_extension_in_root(config_root: &Path, id: &str) -> Result<ExtensionManifest> {
    if let Some(link) = broken_extension_link_in_root(config_root, id) {
        return Err(broken_extension_error(&link));
    }

    let extension_dir = paths::extension_in_root(config_root, id);
    load_extension_at(id, &extension_dir).map_err(|failure| manifest_failure_error(&failure))
}

/// Ambient sibling of [`load_extension_in_root`]: resolves the process config
/// root once and delegates.
pub fn load_extension(id: &str) -> Result<ExtensionManifest> {
    load_extension_in_root(&paths::homeboy()?, id)
}

/// Load a manifest from an injected config root when one is supplied, and from
/// the ambient process root otherwise.
///
/// `None` means "this whole resolution is ambient"; `Some(root)` means "this
/// whole resolution is rooted". It exists so a resolver that reads several
/// manifests carries ONE boundary decision for all of them instead of
/// interleaving rooted and ambient reads — the half-injected split #7505 exists
/// to prevent. It is never a per-read choice.
pub(crate) fn load_extension_in_optional_root(
    config_root: Option<&Path>,
    id: &str,
) -> Result<ExtensionManifest> {
    match config_root {
        Some(config_root) => load_extension_in_root(config_root, id),
        None => load_extension(id),
    }
}

/// Valid manifests only, from an already-produced discovery result.
///
/// Shared by the ambient and rooted list loaders so the two cannot drift in
/// which discovery outcomes they consider loadable.
fn valid_manifests(discovered: Vec<DiscoveredExtension>) -> Vec<ExtensionManifest> {
    discovered
        .into_iter()
        .filter_map(|extension| match extension {
            DiscoveredExtension::Valid(manifest) => Some(*manifest),
            DiscoveredExtension::Invalid(_) => None,
        })
        .collect()
}

/// Every loadable installed extension below an already-resolved config root.
pub fn load_all_extensions_in_root(config_root: &Path) -> Result<Vec<ExtensionManifest>> {
    Ok(valid_manifests(discover_extensions_in_root(config_root)))
}

/// Ambient sibling of [`load_all_extensions_in_root`].
///
/// Deliberately routed through the ambient [`discover_extensions`] rather than
/// resolving the root with `?`: an unresolvable config root has always yielded
/// an empty extension list here, not an error, and several callers depend on
/// that degrade.
pub fn load_all_extensions() -> Result<Vec<ExtensionManifest>> {
    Ok(valid_manifests(discover_extensions()))
}

/// Discover every installed extension directory below an already-resolved
/// config root, including manifests that are malformed or incompatible with
/// this Homeboy version.
pub fn discover_extensions_in_root(config_root: &Path) -> Vec<DiscoveredExtension> {
    discover_extensions_at(&paths::extensions_in_root(config_root))
}

/// Discover every installed extension directory, including manifests that are
/// malformed or incompatible with this Homeboy version.
pub fn discover_extensions() -> Vec<DiscoveredExtension> {
    let Ok(extensions_dir) = paths::extensions() else {
        return Vec::new();
    };
    discover_extensions_at(&extensions_dir)
}

/// Discovery over an already-resolved extensions directory. The one place the
/// directory walk lives, so ambient and rooted discovery cannot diverge.
fn discover_extensions_at(extensions_dir: &Path) -> Vec<DiscoveredExtension> {
    let Ok(entries) = std::fs::read_dir(extensions_dir) else {
        return Vec::new();
    };

    let entries = entries
        .flatten()
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    let shared_asset_roots = declared_shared_asset_roots(&entries);
    let mut extensions = entries
        .into_iter()
        .filter(|path| {
            !path
                .file_name()
                .is_some_and(|name| shared_asset_roots.contains(name))
        })
        .filter_map(discover_extension_entry)
        .collect::<Vec<_>>();
    extensions
        .sort_by(|left, right| discovered_extension_id(left).cmp(discovered_extension_id(right)));
    extensions
}

#[derive(Deserialize)]
struct ExtensionRootManifest {
    #[serde(default)]
    shared_assets: Vec<SharedAssetDeclaration>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum SharedAssetDeclaration {
    Path(String),
    Object { path: String },
}

impl SharedAssetDeclaration {
    fn path(self) -> String {
        match self {
            Self::Path(path) | Self::Object { path } => path,
        }
    }
}

/// Shared assets live beside installed extensions but are declared by a source
/// root, not extension manifests. Copied installs retain that declaration in
/// their private metadata; linked installs retain it at their resolved source.
fn declared_shared_asset_roots(entries: &[PathBuf]) -> BTreeSet<std::ffi::OsString> {
    entries
        .iter()
        .flat_map(|entry| shared_asset_manifest_paths(entry))
        .flat_map(|manifest| shared_asset_roots_from_manifest(&manifest))
        .collect()
}

fn shared_asset_manifest_paths(entry: &Path) -> Vec<PathBuf> {
    let mut manifests = vec![entry.join(".homeboy-extension-root.json")];
    if std::fs::symlink_metadata(entry).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        if let Ok(resolved) = std::fs::canonicalize(entry) {
            if let Some(source_root) = resolved.parent() {
                manifests.push(source_root.join("homeboy-extension-root.json"));
            }
        }
    }
    manifests
}

fn shared_asset_roots_from_manifest(manifest: &Path) -> Vec<std::ffi::OsString> {
    std::fs::read_to_string(manifest)
        .ok()
        .and_then(|raw| serde_json::from_str::<ExtensionRootManifest>(&raw).ok())
        .into_iter()
        .flat_map(|manifest| manifest.shared_assets)
        .filter_map(|asset| {
            let asset = asset.path();
            let path = Path::new(&asset);
            path.components()
                .next()
                .and_then(|component| match component {
                    std::path::Component::Normal(root) => Some(root.to_os_string()),
                    _ => None,
                })
        })
        .collect()
}

fn discover_extension_entry(path: PathBuf) -> Option<DiscoveredExtension> {
    let id = path.file_name()?.to_string_lossy().to_string();
    let metadata = std::fs::symlink_metadata(&path).ok()?;
    if metadata.file_type().is_symlink() && !path.exists() {
        let target = std::fs::read_link(&path).ok();
        return Some(DiscoveredExtension::Invalid(ExtensionManifestFailure {
            manifest_path: path.join(format!("{id}.json")),
            id,
            path,
            category: "target_missing",
            diagnostic: "The linked extension target does not exist.",
            symlink_target: target,
        }));
    }
    if !path.is_dir() {
        return None;
    }

    match load_extension_at(&id, &path) {
        Ok(manifest) => Some(DiscoveredExtension::Valid(Box::new(manifest))),
        Err(failure) => Some(DiscoveredExtension::Invalid(*failure)),
    }
}

fn discovered_extension_id(extension: &DiscoveredExtension) -> &str {
    match extension {
        DiscoveredExtension::Valid(manifest) => &manifest.id,
        DiscoveredExtension::Invalid(failure) => &failure.id,
    }
}

fn load_extension_at(
    id: &str,
    extension_dir: &Path,
) -> std::result::Result<ExtensionManifest, Box<ExtensionManifestFailure>> {
    let manifest_path = extension_dir.join(format!("{id}.json"));
    let content = std::fs::read_to_string(&manifest_path).map_err(|_| {
        Box::new(ExtensionManifestFailure {
            id: id.to_string(),
            path: extension_dir.to_path_buf(),
            manifest_path: manifest_path.clone(),
            category: if manifest_path.exists() {
                "manifest_unreadable"
            } else {
                "manifest_missing"
            },
            diagnostic: if manifest_path.exists() {
                "The extension manifest could not be read."
            } else {
                "The extension manifest is missing."
            },
            symlink_target: None,
        })
    })?;
    let value: serde_json::Value = serde_json::from_str(&content).map_err(|_| {
        Box::new(ExtensionManifestFailure {
            id: id.to_string(),
            path: extension_dir.to_path_buf(),
            manifest_path: manifest_path.clone(),
            category: "manifest_json_malformed",
            diagnostic: "The extension manifest contains malformed JSON.",
            symlink_target: None,
        })
    })?;
    let mut manifest: ExtensionManifest = serde_json::from_value(value).map_err(|_| {
        Box::new(ExtensionManifestFailure {
            id: id.to_string(),
            path: extension_dir.to_path_buf(),
            manifest_path: manifest_path.clone(),
            category: "manifest_deserialize_incompatible",
            diagnostic: "The extension manifest does not match the supported schema.",
            symlink_target: None,
        })
    })?;
    manifest.id = id.to_string();
    manifest.validate_notification_transports().map_err(|_| {
        Box::new(ExtensionManifestFailure {
            id: id.to_string(),
            path: extension_dir.to_path_buf(),
            manifest_path,
            category: "manifest_validation_incompatible",
            diagnostic: "The extension manifest contains unsupported configuration.",
            symlink_target: None,
        })
    })?;
    manifest.extension_path = Some(extension_dir.to_string_lossy().to_string());
    Ok(manifest)
}

fn manifest_failure_error(failure: &ExtensionManifestFailure) -> Error {
    Error::new(
        ErrorCode::ConfigInvalidValue,
        format!("Extension '{}' has an invalid manifest", failure.id),
        serde_json::json!({
            "id": failure.id,
            "path": failure.path,
            "manifest_path": failure.manifest_path,
            "category": failure.category,
            "diagnostic": failure.diagnostic,
        }),
    )
    .with_hint(format!(
        "Repair the manifest at {} and run homeboy extension show {} again.",
        failure.manifest_path.display(),
        failure.id
    ))
}

/// Broken extension links below an already-resolved config root.
pub fn broken_extension_links_in_root(config_root: &Path) -> Vec<BrokenExtensionLink> {
    broken_extension_links_at(&paths::extensions_in_root(config_root))
}

fn broken_extension_links_at(extensions_dir: &Path) -> Vec<BrokenExtensionLink> {
    let Ok(entries) = std::fs::read_dir(extensions_dir) else {
        return Vec::new();
    };

    let mut links: Vec<BrokenExtensionLink> = entries
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let id = path.file_name()?.to_string_lossy().to_string();
            broken_extension_link_at(&path, &id)
        })
        .collect();
    links.sort_by(|a, b| a.id.cmp(&b.id));
    links
}

fn broken_extension_link_in_root(config_root: &Path, id: &str) -> Option<BrokenExtensionLink> {
    broken_extension_link_at(&paths::extension_in_root(config_root, id), id)
}

/// Ambient sibling of [`broken_extension_link_in_root`], used only by this
/// module's tests. `#[cfg(test)]` for the same reason as
/// `load_standalone_components`: its one remaining caller is a test, so a lib
/// build sees dead code under `-D warnings` (#7505).
#[cfg(test)]
fn broken_extension_link(id: &str) -> Option<BrokenExtensionLink> {
    broken_extension_link_at(&paths::extension(id).ok()?, id)
}

/// The link diagnosis for an already-resolved extension directory path.
fn broken_extension_link_at(path: &Path, id: &str) -> Option<BrokenExtensionLink> {
    let metadata = std::fs::symlink_metadata(path).ok()?;
    if !metadata.file_type().is_symlink() || path.exists() {
        return None;
    }

    Some(BrokenExtensionLink {
        id: id.to_string(),
        target: std::fs::read_link(path).ok()?,
        path: path.to_path_buf(),
    })
}

fn broken_extension_error(link: &BrokenExtensionLink) -> Error {
    let error = Error::new(
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
        "Relink it with `homeboy extension relink {} <path>` or remove the stale registration with `homeboy extension uninstall {}`.",
        link.id, link.id
    ));

    broken_extension_link_repair_actions(&link.id)
        .into_iter()
        .fold(error, Error::with_action)
}

/// Whether an extension exposes `tool` as its CLI entrypoint.
fn provides_tool(manifest: &ExtensionManifest, tool: &str) -> bool {
    manifest.cli.as_ref().is_some_and(|c| c.tool == tool)
}

/// Find an extension by CLI tool below an already-resolved config root.
pub fn find_extension_by_tool_in_root(config_root: &Path, tool: &str) -> Option<ExtensionManifest> {
    load_all_extensions_in_root(config_root)
        .ok()?
        .into_iter()
        .find(|m| provides_tool(m, tool))
}

pub fn find_extension_by_tool(tool: &str) -> Option<ExtensionManifest> {
    load_all_extensions()
        .ok()?
        .into_iter()
        .find(|m| provides_tool(m, tool))
}

pub fn extension_path(id: &str) -> PathBuf {
    paths::extension(id).unwrap_or_else(|_| PathBuf::from(id))
}

/// Installed extension ids below an already-resolved config root.
pub fn available_extension_ids_in_root(config_root: &Path) -> Vec<String> {
    config::list_ids_in_root::<ExtensionManifest>(config_root).unwrap_or_default()
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

/// Check if a extension is a symlink (linked, not installed) below an
/// already-resolved config root.
pub fn is_extension_linked_in_root(config_root: &Path, extension_id: &str) -> bool {
    path_is_symlink(&paths::extension_in_root(config_root, extension_id))
}

/// Check if a extension is a symlink (linked, not installed).
pub fn is_extension_linked(extension_id: &str) -> bool {
    paths::extension(extension_id)
        .map(|path| path_is_symlink(&path))
        .unwrap_or(false)
}

fn path_is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink())
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
    fn discovery_reports_malformed_json_without_manifest_contents() {
        crate::test_support::with_isolated_home(|_| {
            let extension_dir = paths::extensions().unwrap().join("malformed");
            std::fs::create_dir_all(&extension_dir).unwrap();
            std::fs::write(extension_dir.join("malformed.json"), "{not json").unwrap();

            let discovered = discover_extensions();
            let DiscoveredExtension::Invalid(failure) = &discovered[0] else {
                panic!("malformed manifest must be reported");
            };
            assert_eq!(failure.id, "malformed");
            assert_eq!(failure.category, "manifest_json_malformed");
            assert_eq!(
                failure.diagnostic,
                "The extension manifest contains malformed JSON."
            );
            assert!(!failure.diagnostic.contains("not json"));
        });
    }

    #[test]
    fn discovery_skips_shared_asset_roots_declared_by_a_copied_extension() {
        crate::test_support::with_isolated_home(|_| {
            let extensions_dir = paths::extensions().unwrap();
            let extension_dir = extensions_dir.join("fixture");
            std::fs::create_dir_all(&extension_dir).unwrap();
            std::fs::write(
                extension_dir.join("fixture.json"),
                r#"{"name":"Fixture","version":"1.0.0"}"#,
            )
            .unwrap();
            std::fs::write(
                extension_dir.join(".homeboy-extension-root.json"),
                r#"{"shared_assets":["agent-runtimes",{"path":"dependency-adapters"},"scripts/lib"]}"#,
            )
            .unwrap();
            for root in ["agent-runtimes", "dependency-adapters", "scripts"] {
                std::fs::create_dir_all(extensions_dir.join(root)).unwrap();
            }

            let discovered = discover_extensions();
            assert_eq!(discovered.len(), 1);
            let DiscoveredExtension::Valid(extension) = &discovered[0] else {
                panic!("fixture extension must be discovered");
            };
            assert_eq!(extension.id, "fixture");
        });
    }

    #[cfg(unix)]
    #[test]
    fn discovery_skips_shared_asset_roots_declared_by_a_linked_extension() {
        crate::test_support::with_isolated_home(|home| {
            let source_root = home.path().join("source");
            let source = source_root.join("fixture");
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(
                source.join("fixture.json"),
                r#"{"name":"Fixture","version":"1.0.0"}"#,
            )
            .unwrap();
            std::fs::write(
                source_root.join("homeboy-extension-root.json"),
                r#"{"shared_assets":["scripts/lib"]}"#,
            )
            .unwrap();
            let extensions_dir = paths::extensions().unwrap();
            std::fs::create_dir_all(&extensions_dir).unwrap();
            std::os::unix::fs::symlink(&source, extensions_dir.join("fixture")).unwrap();
            std::fs::create_dir_all(extensions_dir.join("scripts")).unwrap();

            assert_eq!(discover_extensions().len(), 1);
        });
    }

    #[test]
    fn discovery_reports_deserialize_incompatible_notification_transport() {
        crate::test_support::with_isolated_home(|_| {
            let extension_dir = paths::extensions().unwrap().join("deserialize");
            std::fs::create_dir_all(&extension_dir).unwrap();
            std::fs::write(
                extension_dir.join("deserialize.json"),
                r#"{"name":"Fixture","version":"1.0.0","notification_transports":[{"id":"notify","command":"secret-command"}]}"#,
            )
            .unwrap();

            let discovered = discover_extensions();
            let DiscoveredExtension::Invalid(failure) = &discovered[0] else {
                panic!("deserialize-incompatible manifest must be reported");
            };
            assert_eq!(failure.category, "manifest_deserialize_incompatible");
            assert_eq!(
                failure.diagnostic,
                "The extension manifest does not match the supported schema."
            );
            assert!(!failure.diagnostic.contains("secret-command"));
        });
    }

    #[test]
    fn discovery_reports_validation_incompatible_notification_transport_schema() {
        crate::test_support::with_isolated_home(|_| {
            let extension_dir = paths::extensions().unwrap().join("validation");
            std::fs::create_dir_all(&extension_dir).unwrap();
            std::fs::write(
                extension_dir.join("validation.json"),
                r#"{"name":"Fixture","version":"1.0.0","notification_transports":[{"schema":"homeboy/notification-transport/v2","id":"notify","command":["secret-command"]}]}"#,
            )
            .unwrap();

            let discovered = discover_extensions();
            let DiscoveredExtension::Invalid(failure) = &discovered[0] else {
                panic!("validation-incompatible manifest must be reported");
            };
            assert_eq!(failure.category, "manifest_validation_incompatible");
            assert_eq!(
                failure.diagnostic,
                "The extension manifest contains unsupported configuration."
            );
            assert!(!failure.diagnostic.contains("secret-command"));

            let error = load_extension("validation").expect_err("invalid manifest must not load");
            assert_eq!(error.code, ErrorCode::ConfigInvalidValue);
            assert_eq!(
                error.details["category"],
                "manifest_validation_incompatible"
            );
            assert_eq!(error.details["diagnostic"], failure.diagnostic);
            assert!(error.details.to_string().contains("validation.json"));
            assert!(!error.details.to_string().contains("secret-command"));
        });
    }

    #[test]
    fn load_rejects_legacy_no_test_markers_without_echoing_values() {
        crate::test_support::with_isolated_home(|_| {
            for id in ["empty-markers", "whitespace-marker"] {
                let extension_dir = paths::extensions().unwrap().join(id);
                std::fs::create_dir_all(&extension_dir).unwrap();
                std::fs::write(
                    extension_dir.join(format!("{id}.json")),
                    format!(
                        r#"{{"name":"Fixture","version":"1.0.0","test":{{"no_tests_applicable":{{"evidence_markers":["super-secret-marker"]}}}}}}"#
                    ),
                )
                .unwrap();

                let error = load_extension(id).expect_err("invalid test policy should be rejected");
                assert!(
                    !error.to_string().contains("super-secret-marker"),
                    "manifest marker values must not appear in diagnostics: {error}"
                );
            }
        });
    }

    #[test]
    fn test_find_extension_by_tool() {
        crate::test_support::with_isolated_home(|_| {
            assert!(find_extension_by_tool("missing-tool").is_none());
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
            assert!(err.hints.iter().any(|hint| hint
                .message
                .contains("homeboy extension uninstall sample-runtime")));
            let actions: Vec<ExecutableAction> =
                serde_json::from_value(err.details[homeboy_error::ACTIONS_DETAILS_KEY].clone())
                    .expect("typed repair actions");
            assert_eq!(
                actions
                    .iter()
                    .map(|action| action.id.as_str())
                    .collect::<Vec<_>>(),
                vec![EXTENSION_RELINK_ACTION_ID, EXTENSION_UNINSTALL_ACTION_ID]
            );
            assert_eq!(
                actions[0].args,
                ["extension", "relink", "sample-runtime", "<path>"]
            );
            assert_eq!(
                actions[1].args,
                ["extension", "uninstall", "sample-runtime"]
            );
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

            let broken = broken_extension_links_in_root(&paths::homeboy().expect("config root"));
            assert_eq!(broken.len(), 1);
            assert_eq!(broken[0].id, "sample-runtime");
            assert_eq!(broken[0].target, target);
        });
    }

    #[cfg(unix)]
    #[test]
    fn discovery_reports_undeclared_broken_extension_links() {
        crate::test_support::with_isolated_home(|_| {
            let extensions_dir = paths::extensions().unwrap();
            std::fs::create_dir_all(&extensions_dir).unwrap();
            std::os::unix::fs::symlink(
                extensions_dir.join("missing-extension"),
                extensions_dir.join("broken-extension"),
            )
            .unwrap();

            let discovered = discover_extensions();
            let DiscoveredExtension::Invalid(failure) = &discovered[0] else {
                panic!("broken extension link must be reported");
            };
            assert_eq!(failure.id, "broken-extension");
            assert_eq!(failure.category, "target_missing");
        });
    }
}

impl crate::config::ConfigEntity for ExtensionManifest {
    const ENTITY_TYPE: &'static str = "extension";
    const DIR_NAME: &'static str = "extensions";

    fn id(&self) -> &str {
        &self.id
    }
    fn set_id(&mut self, id: String) {
        self.id = id;
    }
    fn not_found_error(id: String, suggestions: Vec<String>) -> crate::Error {
        crate::Error::extension_not_found(id, suggestions)
    }

    /// Extensions are directory-backed: `{config_root}/extensions/{id}/{id}.json`.
    fn config_path_in_root(config_root: &std::path::Path, id: &str) -> std::path::PathBuf {
        crate::paths::extension_manifest_in_root(config_root, id)
    }
}
