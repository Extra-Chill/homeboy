//! Record snapshot entries under the spelling Git tracks for them.
//!
//! On a case-insensitive filesystem (for example macOS APFS) a repository that
//! tracks `tests/fixtures/a.php` and `tests/Fixtures/B.php` has a single
//! physical directory, so the content walk reports every file under whichever
//! spelling the filesystem returns. Materialized on a case-sensitive runner,
//! those paths no longer match the tracked tree. When the snapshot source is a
//! Git checkout, each walked path is mapped to the spelling Git reports for it
//! (exact match first, then a unique case-insensitive full-path match) before
//! it enters the manifest inventory, the content identity, and the selection.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use sha2::{Digest, Sha256};

use homeboy_core::error::{Error, Result};

use super::{
    content_identity_hasher, hash_directory_entry, hash_file_entry_header, hash_symlink_entry,
    read_file_chunks, snapshot_construction_failure, WorkspaceContentManifest,
    WorkspaceContentManifestEntry, WorkspaceContentWalk,
};
use crate::workspace::control::WorkspaceControl;

/// The paths Git reports for a checkout rooted at `path`: tracked index
/// entries plus untracked, non-ignored files. `None` when `path` is not the
/// root of a Git work tree.
pub(super) fn git_reported_paths(
    path: &Path,
    control: &WorkspaceControl,
) -> Result<Option<Vec<String>>> {
    control.checkpoint()?;
    let toplevel = match control.git(path, &["rev-parse", "--show-toplevel"]) {
        Ok(toplevel) => toplevel,
        Err(error) => {
            // Propagate cancellation; any other failure means "not Git".
            control.checkpoint()?;
            let _ = error;
            return Ok(None);
        }
    };
    let (Ok(toplevel), Ok(root)) = (Path::new(&toplevel).canonicalize(), path.canonicalize())
    else {
        return Ok(None);
    };
    if toplevel != root {
        return Ok(None);
    }
    let args = [
        "-c",
        "core.quotepath=off",
        "ls-files",
        "-z",
        "--cached",
        "--others",
        "--exclude-standard",
    ];
    let output = control.output_exact(
        Command::new("git").args(args).current_dir(path),
        "read snapshot Git path spelling",
    )?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(
            crate::workspace::util::git_failure_evidence(path, &args, &output),
        ));
    }
    let paths = output
        .stdout
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| String::from_utf8_lossy(path).into_owned())
        .collect::<BTreeSet<_>>();
    Ok(Some(paths.into_iter().collect()))
}

fn fold(path: &str) -> String {
    path.to_lowercase()
}

/// Map each walked manifest path to the spelling Git reports for it.
///
/// Returns one element per walked entry: `Some(path)` for the spelling to
/// record, or `None` for a walked directory whose spelling Git tracks only as
/// several case variants (each variant is re-derived from its tracked
/// descendants). Fails closed when a walked file folds to more than one
/// tracked path.
pub(super) fn tracked_spelling_for_walk(
    known: &[String],
    walked: &[WorkspaceContentManifestEntry],
) -> Result<Vec<Option<String>>> {
    let known_paths = known.iter().map(String::as_str).collect::<HashSet<_>>();
    let mut known_directories = HashSet::new();
    for path in known {
        let mut ancestor = Path::new(path.as_str()).parent();
        while let Some(directory) = ancestor.filter(|dir| !dir.as_os_str().is_empty()) {
            if !known_directories.insert(directory.to_string_lossy().into_owned()) {
                break;
            }
            ancestor = directory.parent();
        }
    }
    let walked_paths = walked
        .iter()
        .map(|entry| entry.path.as_str())
        .collect::<HashSet<_>>();
    // Only tracked spellings that the walk did not report exactly are
    // candidates for a case-insensitive match.
    let mut folded_paths: HashMap<String, Vec<&str>> = HashMap::new();
    for path in known_paths
        .iter()
        .filter(|path| !walked_paths.contains(*path))
    {
        folded_paths.entry(fold(path)).or_default().push(path);
    }
    let mut folded_directories: HashMap<String, Vec<&str>> = HashMap::new();
    for path in known_directories
        .iter()
        .map(String::as_str)
        .chain(known_paths.iter().copied())
        .filter(|path| !walked_paths.contains(path))
    {
        folded_directories.entry(fold(path)).or_default().push(path);
    }

    walked
        .iter()
        .map(|entry| {
            let path = entry.path.as_str();
            if entry.kind == "directory" {
                if known_directories.contains(path) || known_paths.contains(path) {
                    return Ok(Some(path.to_string()));
                }
                return Ok(
                    match folded_directories.get(&fold(path)).map(Vec::as_slice) {
                        None => Some(path.to_string()),
                        Some([only]) => Some((*only).to_string()),
                        Some(_) => None,
                    },
                );
            }
            if known_paths.contains(path) {
                return Ok(Some(path.to_string()));
            }
            match folded_paths.get(&fold(path)).map(Vec::as_slice) {
                None => Ok(Some(path.to_string())),
                Some([only]) => Ok(Some((*only).to_string())),
                Some(candidates) => {
                    let mut candidates = candidates.to_vec();
                    candidates.sort_unstable();
                    Err(ambiguous_spelling(path, &candidates))
                }
            }
        })
        .collect()
}

fn ambiguous_spelling(walked: &str, candidates: &[&str]) -> Error {
    Error::validation_invalid_argument(
        "workspace",
        format!(
            "snapshot path `{walked}` matches more than one Git-tracked path case-insensitively ({}); \
             refusing to guess which spelling the snapshot should record",
            candidates
                .iter()
                .map(|candidate| format!("`{candidate}`"))
                .collect::<Vec<_>>()
                .join(" and ")
        ),
        Some(walked.to_string()),
        None,
    )
}

/// One entry of the respelled snapshot, in content-identity order.
struct RespelledEntry {
    path: String,
    /// Index into the walk, or `None` for a directory derived from the
    /// tracked ancestors of respelled entries.
    walked: Option<usize>,
}

/// Apply Git's spelling to a completed content walk. Returns the manifest,
/// the content identity, and the physical source of each manifest entry.
pub(super) fn apply_git_spelling(
    root: &Path,
    walk: WorkspaceContentWalk,
    known: Option<&[String]>,
    control: &WorkspaceControl,
) -> Result<(WorkspaceContentManifest, String, Vec<PathBuf>)> {
    let walked_sources = |manifest: &WorkspaceContentManifest| {
        manifest
            .entries
            .iter()
            .map(|entry| root.join(&entry.path))
            .collect::<Vec<_>>()
    };
    let Some(known) = known else {
        let sources = walked_sources(&walk.manifest);
        return Ok((walk.manifest, walk.content_identity, sources));
    };
    let spelling = tracked_spelling_for_walk(known, &walk.manifest.entries)?;
    let unchanged = spelling
        .iter()
        .zip(&walk.manifest.entries)
        .all(|(spelled, entry)| spelled.as_deref() == Some(entry.path.as_str()));
    if unchanged {
        let sources = walked_sources(&walk.manifest);
        return Ok((walk.manifest, walk.content_identity, sources));
    }

    let mut respelled: BTreeMap<PathBuf, RespelledEntry> = BTreeMap::new();
    for (index, spelled) in spelling.iter().enumerate() {
        control.checkpoint()?;
        let Some(spelled) = spelled else { continue };
        let entry = &walk.manifest.entries[index];
        if let Some(existing) = respelled.get(Path::new(spelled.as_str())) {
            let existing = existing
                .walked
                .map(|index| &walk.manifest.entries[index])
                .expect("only walked entries are recorded before ancestors");
            // Case-variant spellings of one physical directory collapse.
            if entry.kind == "directory" && existing.kind == "directory" {
                continue;
            }
            let existing = existing.path.as_str();
            return Err(Error::validation_invalid_argument(
                "workspace",
                format!(
                    "snapshot paths `{existing}` and `{}` both map to Git-tracked path `{spelled}`",
                    entry.path
                ),
                Some(spelled.clone()),
                None,
            ));
        }
        respelled.insert(
            PathBuf::from(spelled),
            RespelledEntry {
                path: spelled.clone(),
                walked: Some(index),
            },
        );
    }
    // Every recorded entry needs its parent directories under the same
    // spelling, exactly as a walk of the correctly-cased tree reports them.
    let paths = respelled.keys().cloned().collect::<Vec<_>>();
    for path in paths {
        let mut ancestor = path.parent();
        while let Some(directory) = ancestor.filter(|dir| !dir.as_os_str().is_empty()) {
            if respelled.contains_key(directory) {
                break;
            }
            respelled.insert(
                directory.to_path_buf(),
                RespelledEntry {
                    path: directory.to_string_lossy().replace('\\', "/"),
                    walked: None,
                },
            );
            ancestor = directory.parent();
        }
    }

    // `Path` ordering is component-wise, which is the walk's depth-first
    // order over byte-sorted siblings.
    let mut hasher = content_identity_hasher(&walk.algorithm);
    let mut manifest = WorkspaceContentManifest {
        entry_count: 0,
        entries: Vec::with_capacity(respelled.len()),
    };
    let mut sources = Vec::with_capacity(respelled.len());
    for (path, entry) in respelled {
        control.checkpoint()?;
        let Some(index) = entry.walked else {
            hash_directory_entry(&mut hasher, &entry.path);
            manifest.entries.push(WorkspaceContentManifestEntry {
                path: entry.path,
                kind: "directory".to_string(),
                sha256: None,
                bytes: None,
                owner_executable: None,
            });
            sources.push(root.join(path));
            continue;
        };
        let walked = &walk.manifest.entries[index];
        let record = &walk.records[index];
        match walked.kind.as_str() {
            "directory" => hash_directory_entry(&mut hasher, &entry.path),
            "symlink" => {
                let target = record.symlink_target.as_deref().unwrap_or_default();
                hash_symlink_entry(&mut hasher, &entry.path, target);
            }
            _ => {
                let bytes = walked.bytes.unwrap_or_default();
                hash_file_entry_header(&mut hasher, &entry.path, record.hash_executable, bytes);
                let mut file_hash = Sha256::new();
                let read = read_file_chunks(&record.source, control, |chunk| {
                    hasher.update(chunk);
                    file_hash.update(chunk);
                    Ok(())
                })?;
                if read != bytes
                    || walked.sha256.as_deref()
                        != Some(format!("sha256:{:x}", file_hash.finalize()).as_str())
                {
                    return Err(snapshot_construction_failure(
                        "content",
                        &record.source,
                        None,
                        "source file changed during hashing",
                    ));
                }
            }
        }
        manifest.entries.push(WorkspaceContentManifestEntry {
            path: entry.path,
            ..walked.clone()
        });
        sources.push(root.join(&walked.path));
    }
    manifest.entry_count = manifest.entries.len() as _;
    Ok((manifest, format!("sha256:{:x}", hasher.finalize()), sources))
}
