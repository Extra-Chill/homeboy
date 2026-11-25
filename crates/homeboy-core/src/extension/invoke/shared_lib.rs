//! Resolution and validation of the shared runner script library
//! (`HOMEBOY_SHARED_LIB_DIR`).
//!
//! Runners source helpers such as `runner-harness.sh` from the shared library
//! that installs materialize at `~/.config/homeboy/extensions/scripts/lib`.
//! Exporting that directory merely because it exists let a stale or partly
//! synced copy fail a run halfway through (#18426). Resolution now checks the
//! active directory against the canonical set of shared scripts before
//! dispatch:
//!
//! - The canonical set is the extension's own source-root `scripts/lib` tree
//!   when it is reachable (linked/dev installs, monorepo source roots), else
//!   the integrity manifest recorded when the active copy was materialized.
//! - When the active copy is incomplete or stale and the canonical source tree
//!   is reachable, resolution falls back to that complete source tree.
//! - Otherwise resolution reports the missing/stale files and the remediation
//!   command so callers fail before dispatch.

use std::path::{Path, PathBuf};

use homeboy_core::error::{Error, Result};

use crate::extension::shared_asset_integrity::{self, Drift};

/// Declared shared asset path that holds the runner script library.
pub(crate) const SHARED_LIB_ASSET: &str = "scripts/lib";

/// Outcome of resolving the shared runner script library.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SharedLibResolution {
    /// No shared library is installed or reachable.
    Absent,
    /// A complete library directory to export.
    Ready {
        dir: PathBuf,
        /// Set when the active copy was incomplete/stale and resolution fell
        /// back to the canonical source tree.
        fallback_from: Option<(PathBuf, Drift)>,
    },
    /// The active library is incomplete or stale and no complete location is
    /// reachable.
    Broken { dir: PathBuf, drift: Drift },
}

impl SharedLibResolution {
    /// Directory to export as `HOMEBOY_SHARED_LIB_DIR`, if any. A broken
    /// directory is still exported so runner guards can name it; capability
    /// runs refuse to dispatch earlier via [`ensure_shared_lib_ready`].
    pub(crate) fn export_dir(&self) -> Option<&Path> {
        match self {
            Self::Absent => None,
            Self::Ready { dir, .. } | Self::Broken { dir, .. } => Some(dir),
        }
    }
}

/// Active shared library location beneath a Homeboy config root.
pub(crate) fn active_shared_lib_dir(config_root: &Path) -> PathBuf {
    homeboy_core::paths::extensions_in_root(config_root)
        .join("scripts")
        .join("lib")
}

fn same_location(left: &Path, right: &Path) -> bool {
    match (std::fs::canonicalize(left), std::fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

/// The extension's canonical `scripts/lib` source tree, when its source root is
/// reachable and distinct from the active copy.
fn canonical_source_dir(extension_path: Option<&Path>, active: &Path) -> Option<PathBuf> {
    let extension_path = extension_path?;
    let mut candidates = vec![extension_path.to_path_buf()];
    if let Ok(resolved) = std::fs::canonicalize(extension_path) {
        if resolved != extension_path {
            candidates.push(resolved);
        }
    }
    candidates.into_iter().find_map(|candidate| {
        crate::extension::lifecycle::shared_assets_for_extension_source(&candidate)
            .into_iter()
            .find(|(asset, source)| {
                asset.trim_matches('/') == SHARED_LIB_ASSET
                    && source.is_dir()
                    && !same_location(source, active)
            })
            .map(|(_, source)| source)
    })
}

/// Resolve the shared runner library for an extension, validating the active
/// copy against the canonical set of shared scripts.
pub(crate) fn resolve_shared_lib_dir(
    config_root: &Path,
    extension_path: Option<&Path>,
) -> SharedLibResolution {
    let active = active_shared_lib_dir(config_root);
    let canonical = canonical_source_dir(extension_path, &active);

    if !active.is_dir() {
        return match canonical {
            Some(dir) => SharedLibResolution::Ready {
                dir,
                fallback_from: None,
            },
            None => SharedLibResolution::Absent,
        };
    }

    let expected = match &canonical {
        Some(source) => shared_asset_integrity::snapshot(source).ok(),
        None => shared_asset_integrity::read_manifest(&active),
    };
    let Some(expected) = expected else {
        // Installed copy without an integrity manifest and no reachable source:
        // nothing to validate against.
        return SharedLibResolution::Ready {
            dir: active,
            fallback_from: None,
        };
    };

    let drift = shared_asset_integrity::drift(&expected, &active);
    if drift.is_clean() {
        return SharedLibResolution::Ready {
            dir: active,
            fallback_from: None,
        };
    }

    match canonical {
        Some(dir) => SharedLibResolution::Ready {
            dir,
            fallback_from: Some((active, drift)),
        },
        None => SharedLibResolution::Broken { dir: active, drift },
    }
}

/// Remediation command that re-materializes an extension's shared assets.
pub(crate) fn remediation_command(extension_id: &str) -> String {
    format!("homeboy extension update {extension_id} --force")
}

fn describe(label: &str, files: &[String]) -> Option<String> {
    (!files.is_empty()).then(|| format!("{label}: {}", files.join(", ")))
}

/// Fail before dispatch when the shared runner library is incomplete or stale
/// and no complete location is reachable.
pub(crate) fn ensure_shared_lib_ready(extension_id: &str, extension_path: &Path) -> Result<()> {
    let Ok(config_root) = homeboy_core::paths::homeboy() else {
        return Ok(());
    };
    match resolve_shared_lib_dir(&config_root, Some(extension_path)) {
        SharedLibResolution::Broken { dir, drift } => Err(broken_error(extension_id, &dir, &drift)),
        _ => Ok(()),
    }
}

fn broken_error(extension_id: &str, dir: &Path, drift: &Drift) -> Error {
    let details = [
        describe("missing", &drift.missing),
        describe("stale", &drift.stale),
    ]
    .into_iter()
    .flatten()
    .collect::<Vec<_>>()
    .join("; ");
    let command = remediation_command(extension_id);
    Error::validation_invalid_argument(
        homeboy_extension_contract::exec_context::SHARED_LIB_DIR,
        format!(
            "Shared runner script library is incomplete or stale at {} ({details}). Re-sync it with `{command}`.",
            dir.display()
        ),
        Some(dir.to_string_lossy().to_string()),
        None,
    )
    .with_hint(format!(
        "Run `{command}` to re-materialize the shared scripts, or reinstall the extension that owns them."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn write_lib(dir: &Path, files: &[(&str, &str)]) {
        fs::create_dir_all(dir).unwrap();
        for (name, body) in files {
            fs::write(dir.join(name), body).unwrap();
        }
    }

    /// Source root with a declared `scripts/lib` and an extension directory.
    fn source_root(root: &Path, files: &[(&str, &str)]) -> PathBuf {
        fs::write(
            root.join("homeboy-extension-root.json"),
            r#"{"shared_assets":["scripts/lib"]}"#,
        )
        .unwrap();
        write_lib(&root.join("scripts/lib"), files);
        let extension = root.join("fixture");
        fs::create_dir_all(&extension).unwrap();
        extension
    }

    const COMPLETE: &[(&str, &str)] = &[
        ("runner-harness.sh", "harness\n"),
        ("test-result-adapters.sh", "adapters\n"),
    ];

    #[test]
    fn complete_active_dir_is_exported() {
        let config = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let extension = source_root(source.path(), COMPLETE);
        let active = active_shared_lib_dir(config.path());
        write_lib(&active, COMPLETE);

        assert_eq!(
            resolve_shared_lib_dir(config.path(), Some(&extension)),
            SharedLibResolution::Ready {
                dir: active,
                fallback_from: None
            }
        );
    }

    #[test]
    fn incomplete_active_dir_falls_back_to_canonical_source() {
        let config = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let extension = source_root(source.path(), COMPLETE);
        let active = active_shared_lib_dir(config.path());
        write_lib(&active, &[("test-result-adapters.sh", "old adapters\n")]);

        let resolution = resolve_shared_lib_dir(config.path(), Some(&extension));
        assert_eq!(
            resolution,
            SharedLibResolution::Ready {
                dir: source.path().join("scripts/lib"),
                fallback_from: Some((
                    active,
                    Drift {
                        missing: vec!["runner-harness.sh".into()],
                        stale: vec!["test-result-adapters.sh".into()],
                    }
                )),
            }
        );
    }

    #[test]
    fn missing_active_dir_uses_canonical_source() {
        let config = tempfile::tempdir().unwrap();
        let source = tempfile::tempdir().unwrap();
        let extension = source_root(source.path(), COMPLETE);

        assert_eq!(
            resolve_shared_lib_dir(config.path(), Some(&extension)),
            SharedLibResolution::Ready {
                dir: source.path().join("scripts/lib"),
                fallback_from: None
            }
        );
    }

    #[test]
    fn manifest_backed_copy_missing_files_is_broken_without_source() {
        let config = tempfile::tempdir().unwrap();
        let active = active_shared_lib_dir(config.path());
        write_lib(&active, COMPLETE);
        shared_asset_integrity::write_manifest(&active).unwrap();
        fs::remove_file(active.join("runner-harness.sh")).unwrap();
        let snapshot = tempfile::tempdir().unwrap();

        let resolution = resolve_shared_lib_dir(config.path(), Some(snapshot.path()));
        let SharedLibResolution::Broken { dir, drift } = &resolution else {
            panic!("expected broken resolution, got {resolution:?}");
        };
        assert_eq!(dir, &active);
        assert_eq!(drift.missing, vec!["runner-harness.sh".to_string()]);

        let message = broken_error("fixture", dir, drift).to_string();
        assert!(message.contains("runner-harness.sh"), "{message}");
        assert!(
            message.contains("homeboy extension update fixture --force"),
            "{message}"
        );
    }

    #[test]
    fn manifest_backed_complete_copy_is_ready() {
        let config = tempfile::tempdir().unwrap();
        let active = active_shared_lib_dir(config.path());
        write_lib(&active, COMPLETE);
        shared_asset_integrity::write_manifest(&active).unwrap();

        assert_eq!(
            resolve_shared_lib_dir(config.path(), None),
            SharedLibResolution::Ready {
                dir: active,
                fallback_from: None
            }
        );
    }

    #[test]
    fn copy_without_manifest_or_source_is_trusted() {
        let config = tempfile::tempdir().unwrap();
        let active = active_shared_lib_dir(config.path());
        write_lib(&active, &[("runner-harness.sh", "harness\n")]);

        assert_eq!(
            resolve_shared_lib_dir(config.path(), None),
            SharedLibResolution::Ready {
                dir: active,
                fallback_from: None
            }
        );
    }

    #[test]
    fn absent_without_active_or_source() {
        let config = tempfile::tempdir().unwrap();
        assert_eq!(
            resolve_shared_lib_dir(config.path(), None),
            SharedLibResolution::Absent
        );
    }
}
