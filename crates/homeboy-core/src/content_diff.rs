//! Generic byte-level directory comparison and safe source-to-destination application.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use homeboy_engine_primitives::content_hash;
use serde::Serialize;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ContentChangeKind {
    Addition,
    Modification,
    Deletion,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct ContentChange {
    pub path: String,
    pub kind: ContentChangeKind,
    pub bytes: u64,
}

/// Compare source content with destination content. Excludes are relative glob
/// patterns, supplied by the caller's component policy.
pub fn compare(
    source: &Path,
    destination: &Path,
    excludes: &[String],
) -> crate::Result<Vec<ContentChange>> {
    let source = collect(source, excludes)?;
    let destination = collect(destination, excludes)?;
    let paths: BTreeSet<_> = source.keys().chain(destination.keys()).cloned().collect();
    Ok(paths
        .into_iter()
        .filter_map(|path| match (source.get(&path), destination.get(&path)) {
            (Some(from), None) => Some(ContentChange {
                path,
                kind: ContentChangeKind::Addition,
                bytes: from.bytes,
            }),
            (None, Some(to)) => Some(ContentChange {
                path,
                kind: ContentChangeKind::Deletion,
                bytes: to.bytes,
            }),
            (Some(from), Some(to)) if from.digest != to.digest => Some(ContentChange {
                path,
                kind: ContentChangeKind::Modification,
                bytes: from.bytes,
            }),
            _ => None,
        })
        .collect())
}

/// Make destination byte-identical to source for the supplied comparison.
pub fn apply(source: &Path, destination: &Path, changes: &[ContentChange]) -> crate::Result<()> {
    for change in changes {
        let target = contained(destination, &change.path)?;
        match change.kind {
            ContentChangeKind::Deletion => {
                if target.exists() {
                    fs::remove_file(&target).map_err(io_error)?;
                }
            }
            ContentChangeKind::Addition | ContentChangeKind::Modification => {
                let input = contained(source, &change.path)?;
                let parent = target.parent().expect("contained path has parent");
                fs::create_dir_all(parent).map_err(io_error)?;
                fs::copy(input, target).map_err(io_error)?;
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Entry {
    digest: String,
    bytes: u64,
}

fn collect(root: &Path, excludes: &[String]) -> crate::Result<BTreeMap<String, Entry>> {
    if !root.is_dir() {
        return Err(crate::Error::validation_invalid_argument(
            "path",
            format!("{} is not a directory", root.display()),
            None,
            None,
        ));
    }
    let mut entries = BTreeMap::new();
    visit(root, root, excludes, &mut entries)?;
    Ok(entries)
}

fn visit(
    root: &Path,
    directory: &Path,
    excludes: &[String],
    entries: &mut BTreeMap<String, Entry>,
) -> crate::Result<()> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        let relative = path
            .strip_prefix(root)
            .map_err(io_error)?
            .to_string_lossy()
            .replace('\\', "/");
        if excluded(&relative, excludes) {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if metadata.is_dir() {
            visit(root, &path, excludes, entries)?;
        } else if metadata.is_file() {
            let bytes = metadata.len();
            entries.insert(
                relative,
                Entry {
                    digest: digest(&path)?,
                    bytes,
                },
            );
        }
    }
    Ok(())
}

pub fn excluded(path: &str, excludes: &[String]) -> bool {
    path == ".git"
        || path.starts_with(".git/")
        || excludes.iter().any(|pattern| {
            let pattern = pattern.trim_matches('/');
            !pattern.is_empty()
                && (path == pattern
                    || path
                        .strip_prefix(pattern)
                        .is_some_and(|suffix| suffix.starts_with('/'))
                    || glob_match::glob_match(pattern, path))
        })
}

fn contained(root: &Path, relative: &str) -> crate::Result<PathBuf> {
    crate::resolve_contained_local_path(root, relative, "path")
}

fn digest(path: &Path) -> crate::Result<String> {
    content_hash::sha256_file(path)
}

fn io_error(error: impl std::fmt::Display) -> crate::Error {
    crate::Error::internal_io(
        error.to_string(),
        Some("compare component content".to_string()),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compares_applies_binary_add_modify_delete_and_excludes() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(&source).expect("source");
        fs::create_dir_all(&destination).expect("destination");
        fs::write(source.join("added"), [0, 1, 255]).expect("added");
        fs::write(source.join("changed"), "remote").expect("changed");
        fs::write(destination.join("changed"), "local").expect("changed");
        fs::write(destination.join("deleted"), "local only").expect("deleted");
        fs::write(source.join("ignored"), "remote").expect("ignored");
        fs::write(destination.join("ignored"), "local").expect("ignored");
        let changes = compare(&source, &destination, &["ignored".to_string()]).expect("compare");
        assert_eq!(
            changes
                .iter()
                .map(|change| (&change.path, &change.kind))
                .collect::<Vec<_>>(),
            vec![
                (&"added".to_string(), &ContentChangeKind::Addition),
                (&"changed".to_string(), &ContentChangeKind::Modification),
                (&"deleted".to_string(), &ContentChangeKind::Deletion),
            ]
        );
        apply(&source, &destination, &changes).expect("apply");
        assert_eq!(
            fs::read(destination.join("added")).expect("binary"),
            vec![0, 1, 255]
        );
        assert!(!destination.join("deleted").exists());
        assert_eq!(
            fs::read_to_string(destination.join("ignored")).expect("ignored"),
            "local"
        );
    }

    #[test]
    fn directory_style_excludes_bound_recursive_collection() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("generated/nested")).expect("source");
        fs::create_dir_all(&destination).expect("destination");
        fs::write(source.join("generated/nested/output"), "remote").expect("generated");

        assert!(compare(&source, &destination, &["generated/".to_string()])
            .expect("compare")
            .is_empty());
    }
}
