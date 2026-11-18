//! Integrity markers for materialized shared extension assets.
//!
//! Copy-mode installs materialize declared shared trees (for example
//! `scripts/lib`) under `~/.config/homeboy/extensions/`. Runners source files
//! from those trees at execution time, so a tree that was only partly synced,
//! or later edited/truncated, fails halfway through a run (#18426).
//!
//! Each copied tree records a manifest of its regular files and their content
//! hashes. Resolution compares the live tree against that manifest (or against
//! the canonical source tree when one is reachable) to detect missing and
//! stale files before dispatch.

use std::collections::BTreeMap;
use std::path::Path;

use homeboy_core::error::{Error, Result};
use homeboy_engine_primitives::content_hash;
use serde::{Deserialize, Serialize};

/// Integrity manifest written into each copied shared asset tree.
pub(crate) const MANIFEST_FILE: &str = ".homeboy-shared-asset-manifest.json";
/// Owner provenance written into each copied shared asset tree.
pub(crate) const SOURCE_URL_FILE: &str = ".source-url";

/// Relative file path (forward slashes) → SHA-256 of its content.
pub(crate) type FileDigests = BTreeMap<String, String>;

#[derive(Debug, Serialize, Deserialize)]
struct Manifest {
    version: u32,
    files: FileDigests,
}

/// Files that are missing or whose content differs from the expected set.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct Drift {
    pub(crate) missing: Vec<String>,
    pub(crate) stale: Vec<String>,
}

impl Drift {
    pub(crate) fn is_clean(&self) -> bool {
        self.missing.is_empty() && self.stale.is_empty()
    }
}

fn is_metadata_file(relative: &str) -> bool {
    relative == MANIFEST_FILE || relative == SOURCE_URL_FILE
}

/// Hash every regular file under `dir`, keyed by `/`-separated relative path.
/// Generated provenance/integrity metadata is excluded.
pub(crate) fn snapshot(dir: &Path) -> Result<FileDigests> {
    fn visit(root: &Path, dir: &Path, out: &mut FileDigests) -> Result<()> {
        let entries = std::fs::read_dir(dir).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read shared asset tree {}", dir.display())),
            )
        })?;
        for entry in entries {
            let entry = entry.map_err(|error| {
                Error::internal_io(error.to_string(), Some("read shared asset entry".into()))
            })?;
            let path = entry.path();
            // Follow symlinks: the runner sources whatever the link resolves to.
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            if meta.is_dir() {
                visit(root, &path, out)?;
            } else if meta.is_file() {
                let Ok(relative) = path.strip_prefix(root) else {
                    continue;
                };
                let relative = relative
                    .components()
                    .map(|part| part.as_os_str().to_string_lossy())
                    .collect::<Vec<_>>()
                    .join("/");
                if is_metadata_file(&relative) {
                    continue;
                }
                out.insert(relative, content_hash::sha256_file(&path)?);
            }
        }
        Ok(())
    }

    let mut out = FileDigests::new();
    visit(dir, dir, &mut out)?;
    Ok(out)
}

/// Record the integrity manifest for a freshly materialized tree.
pub(crate) fn write_manifest(dir: &Path) -> Result<()> {
    let manifest = Manifest {
        version: 1,
        files: snapshot(dir)?,
    };
    let raw = serde_json::to_string_pretty(&manifest).map_err(|error| {
        Error::internal_unexpected(format!("serialize shared asset manifest: {error}"))
    })?;
    std::fs::write(dir.join(MANIFEST_FILE), raw).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write shared asset manifest".to_string()),
        )
    })
}

/// Read the integrity manifest recorded for a tree, if any. Trees materialized
/// before markers existed, or symlinked to a live source, have none.
pub(crate) fn read_manifest(dir: &Path) -> Option<FileDigests> {
    let raw = std::fs::read_to_string(dir.join(MANIFEST_FILE)).ok()?;
    serde_json::from_str::<Manifest>(&raw)
        .ok()
        .map(|manifest| manifest.files)
}

/// Compare the live tree at `dir` against `expected`. Extra files are ignored:
/// only what the canonical set requires must be present and current.
pub(crate) fn drift(expected: &FileDigests, dir: &Path) -> Drift {
    let mut drift = Drift::default();
    for (relative, digest) in expected {
        let path = dir.join(relative);
        if !path.is_file() {
            drift.missing.push(relative.clone());
            continue;
        }
        match content_hash::sha256_file(&path) {
            Ok(actual) if &actual == digest => {}
            _ => drift.stale.push(relative.clone()),
        }
    }
    drift
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_round_trips_and_detects_missing_and_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("nested")).unwrap();
        std::fs::write(dir.path().join("a.sh"), "a\n").unwrap();
        std::fs::write(dir.path().join("nested/b.sh"), "b\n").unwrap();
        std::fs::write(dir.path().join(SOURCE_URL_FILE), "url").unwrap();
        write_manifest(dir.path()).unwrap();

        let expected = read_manifest(dir.path()).expect("manifest");
        assert_eq!(
            expected.keys().cloned().collect::<Vec<_>>(),
            vec!["a.sh".to_string(), "nested/b.sh".to_string()]
        );
        assert!(drift(&expected, dir.path()).is_clean());

        std::fs::remove_file(dir.path().join("nested/b.sh")).unwrap();
        std::fs::write(dir.path().join("a.sh"), "changed\n").unwrap();
        assert_eq!(
            drift(&expected, dir.path()),
            Drift {
                missing: vec!["nested/b.sh".into()],
                stale: vec!["a.sh".into()],
            }
        );
    }
}
