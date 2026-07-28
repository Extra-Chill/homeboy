//! Generic byte-level directory comparison and safe source-to-destination application.
//!
//! This module owns the one directory walk every tree comparison in the
//! codebase is built on. Recovery (`harvest`) and deploy drift detection used
//! to each carry a privately written walker, and the two could return
//! contradictory answers for the same directory pair because their divergences
//! were accidents of two implementations rather than decisions (#10290). The
//! walk now lives in [`scan_tree`] and every remaining divergence is a named
//! field on [`TreeScanOptions`], so a behavioural difference between call sites
//! is reviewable instead of invisible.

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
            (Some(from), Some(to)) if !from.matches(to) => Some(ContentChange {
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

/// What a tree scan records, and what it leaves out.
///
/// Recovery and deploy walk the same shape of directory but are answering
/// different questions about it, so the facts each one needs differ. Every
/// difference is a field here rather than a separate walker: two call sites can
/// still behave differently, but only by declaring that they do.
#[derive(Debug, Clone, Default)]
pub struct TreeScanOptions {
    /// Relative glob and directory patterns removed from the scan. `.git` is
    /// always removed regardless of this set.
    pub excludes: Vec<String>,
    /// Record symlinks as entries carrying their link target. When false a
    /// symlink is neither followed nor recorded, so link-only drift is
    /// invisible to the caller.
    pub record_symlinks: bool,
    /// Record the executable bit as part of each file's identity.
    pub record_executable_mode: bool,
    /// Prune this product's own transport scratch files at any depth. They are
    /// created by the tooling performing the comparison, never by the content
    /// being compared, so no side of a comparison should report them as drift.
    pub prune_runtime_artifacts: bool,
}

/// Whether a scanned path is a regular file or a symlink.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeEntryKind {
    File,
    Symlink,
}

impl TreeEntryKind {
    /// Single-character tag used by manifest wire formats and digests.
    pub fn tag(self) -> char {
        match self {
            TreeEntryKind::File => 'f',
            TreeEntryKind::Symlink => 'l',
        }
    }
}

/// One scanned path and the facts recorded about it.
#[derive(Debug, Clone)]
pub struct TreeEntry {
    pub kind: TreeEntryKind,
    /// Executable bits in octal (see [`executable_mode_tag`]), or `"0"` when
    /// the scan does not record mode, the entry is a symlink, or the platform
    /// has no mode concept.
    pub mode: String,
    /// SHA-256 hex digest for files; the link target for symlinks.
    pub value: String,
    /// Filesystem byte length, or `0` when the entry did not come from a
    /// filesystem walk. Deliberately excluded from [`TreeEntry::matches`]:
    /// manifests parsed from a remote probe or an archive carry content
    /// identity without a stat size, and comparing a missing size against a
    /// real one would report drift on identical content.
    pub bytes: u64,
}

impl TreeEntry {
    /// Whether two entries describe the same content at the same path.
    pub fn matches(&self, other: &Self) -> bool {
        self.kind == other.kind && self.mode == other.mode && self.value == other.value
    }
}

/// The mode semantic that survives a deploy.
///
/// Deploy normalizes ownership and group-write/setgid bits on the target, so
/// only executability is stable enough to compare. Rendering it here — rather
/// than in each manifest producer — is what keeps a locally walked tree, a
/// remotely probed tree, and an archive expressing the same bits identically.
pub fn executable_mode_tag(mode: u32) -> String {
    format!("{:o}", mode & 0o111)
}

/// Whether a relative path is one of this product's own transport scratch
/// files, at any depth.
pub fn runtime_artifact(path: &str) -> bool {
    let prefix = crate::product_identity::PRODUCT_IDENTITY.artifact_prefix;
    path.split('/').any(|part| part.starts_with(prefix))
}

/// Walk `root` and record one entry per path, honouring `options`.
///
/// Entries are keyed by `/`-separated path relative to `root`, so the result is
/// directly comparable with a manifest produced for a different root.
pub fn scan_tree(
    root: &Path,
    options: &TreeScanOptions,
) -> crate::Result<BTreeMap<String, TreeEntry>> {
    if !root.is_dir() {
        return Err(crate::Error::validation_invalid_argument(
            "path",
            format!("{} is not a directory", root.display()),
            None,
            None,
        ));
    }
    let mut entries = BTreeMap::new();
    scan_directory(root, root, options, &mut entries)?;
    Ok(entries)
}

fn collect(root: &Path, excludes: &[String]) -> crate::Result<BTreeMap<String, TreeEntry>> {
    scan_tree(
        root,
        &TreeScanOptions {
            excludes: excludes.to_vec(),
            prune_runtime_artifacts: true,
            ..TreeScanOptions::default()
        },
    )
}

fn scan_directory(
    root: &Path,
    directory: &Path,
    options: &TreeScanOptions,
    entries: &mut BTreeMap<String, TreeEntry>,
) -> crate::Result<()> {
    for entry in fs::read_dir(directory).map_err(io_error)? {
        let path = entry.map_err(io_error)?.path();
        let relative = path
            .strip_prefix(root)
            .map_err(io_error)?
            .to_string_lossy()
            .replace('\\', "/");
        if excluded(&relative, &options.excludes)
            || (options.prune_runtime_artifacts && runtime_artifact(&relative))
        {
            continue;
        }
        let metadata = fs::symlink_metadata(&path).map_err(io_error)?;
        if metadata.file_type().is_symlink() {
            if !options.record_symlinks {
                continue;
            }
            let target = fs::read_link(&path)
                .map_err(io_error)?
                .to_string_lossy()
                .to_string();
            entries.insert(
                relative,
                TreeEntry {
                    kind: TreeEntryKind::Symlink,
                    mode: "0".to_string(),
                    value: target,
                    bytes: 0,
                },
            );
        } else if metadata.is_dir() {
            scan_directory(root, &path, options, entries)?;
        } else if metadata.is_file() {
            entries.insert(
                relative,
                TreeEntry {
                    kind: TreeEntryKind::File,
                    mode: entry_mode(&metadata, options),
                    value: digest(&path)?,
                    bytes: metadata.len(),
                },
            );
        }
    }
    Ok(())
}

fn entry_mode(metadata: &fs::Metadata, options: &TreeScanOptions) -> String {
    if !options.record_executable_mode {
        return "0".to_string();
    }
    #[cfg(unix)]
    {
        executable_mode_tag(std::os::unix::fs::PermissionsExt::mode(
            &metadata.permissions(),
        ))
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        "0".to_string()
    }
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

    /// #10290: deploy's manifest pruned this product's transport scratch files
    /// and recovery's comparison did not, so the two disagreed about the same
    /// directory pair. The scratch files belong to the tooling doing the
    /// comparing; neither answer should ever have included them.
    #[test]
    fn recovery_comparison_prunes_this_products_transport_scratch_files() {
        let temp = tempfile::tempdir().expect("temp");
        let source = temp.path().join("source");
        let destination = temp.path().join("destination");
        fs::create_dir_all(source.join("nested")).expect("source");
        fs::create_dir_all(&destination).expect("destination");
        let scratch = format!(
            "{}upload.tmp",
            crate::product_identity::PRODUCT_IDENTITY.artifact_prefix
        );
        fs::write(source.join(&scratch), "remote").expect("scratch");
        fs::write(source.join("nested").join(&scratch), "remote").expect("nested scratch");
        fs::write(destination.join(&scratch), "local").expect("scratch");

        assert!(compare(&source, &destination, &[])
            .expect("compare")
            .is_empty());
        assert!(runtime_artifact(&scratch));
        assert!(runtime_artifact(&format!("nested/{scratch}")));
        assert!(!runtime_artifact("nested/payload.txt"));
    }

    /// The recorded facts are options, not accidents: the same tree scanned
    /// with recovery's options and with deploy's options must differ only in
    /// the ways those options declare.
    #[test]
    fn scan_options_decide_which_facts_a_walk_records() {
        let temp = tempfile::tempdir().expect("temp");
        let root = temp.path().join("tree");
        fs::create_dir_all(&root).expect("root");
        fs::write(root.join("file"), "bytes").expect("file");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root.join("file"), fs::Permissions::from_mode(0o755))
                .expect("mode");
            std::os::unix::fs::symlink("file", root.join("link")).expect("link");
        }

        let recovery = scan_tree(
            &root,
            &TreeScanOptions {
                prune_runtime_artifacts: true,
                ..TreeScanOptions::default()
            },
        )
        .expect("recovery scan");
        let deployed = scan_tree(
            &root,
            &TreeScanOptions {
                record_symlinks: true,
                record_executable_mode: true,
                prune_runtime_artifacts: true,
                ..TreeScanOptions::default()
            },
        )
        .expect("deploy scan");

        assert_eq!(recovery["file"].kind, TreeEntryKind::File);
        assert_eq!(recovery["file"].bytes, "bytes".len() as u64);
        assert_eq!(recovery["file"].mode, "0");
        assert_eq!(recovery["file"].value, deployed["file"].value);

        #[cfg(unix)]
        {
            assert_eq!(deployed["file"].mode, executable_mode_tag(0o755));
            assert!(!recovery.contains_key("link"));
            assert_eq!(deployed["link"].kind, TreeEntryKind::Symlink);
            assert_eq!(deployed["link"].value, "file");
            assert_eq!(deployed["link"].mode, "0");
        }
    }

    /// Byte length is provenance, not identity. A manifest parsed from a remote
    /// probe or an archive has no stat size, so folding size into the match
    /// predicate would report drift on byte-identical content.
    #[test]
    fn entry_identity_excludes_filesystem_byte_length() {
        let walked = TreeEntry {
            kind: TreeEntryKind::File,
            mode: executable_mode_tag(0o755),
            value: "a".repeat(64),
            bytes: 42,
        };
        let parsed = TreeEntry {
            bytes: 0,
            ..walked.clone()
        };
        assert!(walked.matches(&parsed));
        assert!(!walked.matches(&TreeEntry {
            mode: "0".to_string(),
            ..walked.clone()
        }));
        assert!(!walked.matches(&TreeEntry {
            kind: TreeEntryKind::Symlink,
            ..walked.clone()
        }));
    }

    #[test]
    fn executable_mode_tag_keeps_only_bits_that_survive_a_deploy() {
        // Deploy normalizes ownership and group-write bits, so 0644 and 0664
        // are the same deployed file while 0755 is not.
        assert_eq!(executable_mode_tag(0o644), "0");
        assert_eq!(executable_mode_tag(0o664), "0");
        assert_eq!(executable_mode_tag(0o755), "111");
        assert_eq!(executable_mode_tag(0o775), "111");
    }
}
