use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::Path;

use glob_match::glob_match;
use sha2::{Digest, Sha256};

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};

use super::super::{Runner, RunnerKind};
use super::materializer::{WorkspaceMaterializationOperation, WorkspaceMaterializer};
use super::types::{ByteFileCounts, SnapshotStats, SnapshotTransferStats};
use super::util::{
    git_output, hex_prefix, owner_capture_shell, owner_restore_shell, parent_remote_path,
    run_shell_capture, run_shell_command, shell_command_for_runner, ssh_args,
    ssh_client_for_runner, tar_exclude_args,
};

const RUNNER_WORKSPACE_METADATA_FILE: &str = ".homeboy/runner-workspace.json";
const LAB_AT_FILES_DIRECTORY: &str = ".homeboy/lab-at-files";
/// Runner-owned paths that Homeboy materializes *onto* a workspace after
/// transport. They exist only on the runner side, never in the controller's
/// source tree, so they must be excluded from every workspace content-identity
/// computation — otherwise the runner and controller hash different trees and
/// materialization verification fails spuriously.
///
/// This is the single source of truth: the content-hash traversals and the
/// pre-sync collision check all derive from it, so a new reserved path cannot
/// leak into one hash algorithm while being excluded from another (the drift
/// that produced #9003 and left the v1 traversal missing `lab-at-files`).
const RESERVED_RUNNER_WORKSPACE_PATHS: &[&str] =
    &[RUNNER_WORKSPACE_METADATA_FILE, LAB_AT_FILES_DIRECTORY];

/// Whether `relative` (a `/`-normalized workspace-relative path) is a
/// runner-owned materialization artifact that must never contribute to a
/// workspace content identity.
fn is_reserved_runner_workspace_path(relative: &str) -> bool {
    RESERVED_RUNNER_WORKSPACE_PATHS
        .iter()
        .any(|reserved| relative == *reserved)
}

// The workspace-content *identity spec* (permission-policy names, the default
// policy, the policy -> algorithm-marker mapping, and the content manifest
// types) lives in the leaf `homeboy-source-snapshot-contract` crate so the
// controller-declare and runner-verify paths derive it from one place. The
// filesystem traversal that *applies* the spec stays here. Re-exported so
// existing `crate::workspace::snapshot::*` / `crate::*` paths keep resolving.
pub use homeboy_source_snapshot_contract::workspace_content_identity::{
    workspace_content_hash_algorithm, WorkspaceContentManifest, WorkspaceContentManifestEntry,
    WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY, WORKSPACE_CONTENT_PERMISSION_PORTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
};

pub(crate) const WORKSPACE_CONTENT_DIAGNOSTIC_PATH_LIMIT: usize = 192;

// The SSH path passes this script through `sh -c` after shell-quoting it. A
// single quote can expand from one byte to five bytes at that layer. 16 KiB
// therefore remains below Linux's 128 KiB single-argument limit even in that
// worst case, while leaving room for the SSH invocation and its environment.
const INCREMENTAL_PREPARE_COMMAND_MAX_BYTES: usize = 16 * 1024;

// Fixed shell syntax, owner capture, UUID, and repeated path references. The
// preflight deliberately overestimates so it can reject before allocating a
// command proportional to the manifest.
const INCREMENTAL_PREPARE_COMMAND_FIXED_OVERHEAD_BYTES: usize = 4 * 1024;

pub(crate) fn snapshot_identity(
    local_path: &Path,
    excludes: &[String],
    includes: &[String],
) -> Result<String> {
    let head =
        git_output(local_path, &["rev-parse", "HEAD"]).unwrap_or_else(|_| "nogit".to_string());
    let status = git_output(local_path, &["status", "--porcelain=v1"])
        .unwrap_or_else(|_| "nogit".to_string());
    let diff = git_output(local_path, &["diff", "--binary", "HEAD"]).unwrap_or_default();
    let staged =
        git_output(local_path, &["diff", "--cached", "--binary", "HEAD"]).unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(local_path.display().to_string().as_bytes());
    hasher.update(head.as_bytes());
    hasher.update(status.as_bytes());
    hasher.update(diff.as_bytes());
    hasher.update(staged.as_bytes());
    hash_snapshot_tree(local_path, local_path, excludes, includes, &mut hasher)?;
    Ok(format!("snapshot:{}", hex_prefix(&hasher.finalize(), 16)))
}

/// Stable v2 digest of the files a snapshot materializes. Unlike
/// `snapshot_identity`, this is portable across controller and runner paths.
/// The declared permission policy is part of the algorithm marker so a digest
/// cannot be interpreted under a different platform capability contract.
pub(crate) fn workspace_content_hash(path: &Path, excludes: &[String]) -> Result<String> {
    workspace_content_hash_for_policy(path, excludes, WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY)
}

pub(crate) fn workspace_content_hash_for_policy(
    path: &Path,
    excludes: &[String],
    policy: &str,
) -> Result<String> {
    let (algorithm, executable_capability) =
        workspace_content_hash_contract(policy).ok_or_else(|| {
            Error::validation_invalid_argument(
                "permission_policy",
                "workspace content hash policy is unsupported on this platform",
                Some(policy.to_string()),
                None,
            )
        })?;
    let mut hasher = Sha256::new();
    hasher.update(algorithm.as_bytes());
    hasher.update(b"\0");
    let root = content_hash_root(path)?;
    collect_content_hash_entries_v2(
        &root,
        &root,
        Path::new(""),
        excludes,
        &mut vec![root.clone()],
        &mut hasher,
        executable_capability,
        None,
    )?;
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

/// A deterministic, content-free inventory of every path a snapshot materializes.
/// It is persisted with the immutable snapshot and used to derive explicit
/// incremental transport and deletion sets.
pub(crate) fn workspace_content_manifest_for_policy(
    path: &Path,
    excludes: &[String],
    policy: &str,
) -> Result<WorkspaceContentManifest> {
    let executable_capability = match workspace_content_hash_contract(policy) {
        Some((_, capability)) => capability,
        None => {
            return Err(Error::validation_invalid_argument(
                "permission_policy",
                "workspace content hash policy is unsupported on this platform",
                Some(policy.to_string()),
                None,
            ));
        }
    };
    let root = content_hash_root(path)?;
    let mut manifest = WorkspaceContentManifest {
        entry_count: 0,
        entries: Vec::new(),
    };
    // Use the authoritative v2 traversal so diagnostics and the content hash
    // have identical symlink, exclusion, and metadata behavior.
    let mut hasher = Sha256::new();
    collect_content_hash_entries_v2(
        &root,
        &root,
        Path::new(""),
        excludes,
        &mut vec![root.clone()],
        &mut hasher,
        executable_capability,
        Some(&mut manifest),
    )?;
    Ok(manifest)
}

/// Exact historical v1 algorithm for controllers that emitted
/// `homeboy/lab-workspace-verification/v1` metadata.
pub(crate) fn workspace_content_hash_v1(path: &Path, excludes: &[String]) -> Result<String> {
    let mut entries = Vec::new();
    let root = content_hash_root(path)?;
    collect_content_hash_entries_v1(
        &root,
        &root,
        Path::new(""),
        excludes,
        &mut vec![root.clone()],
        &mut entries,
    )?;
    entries.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    hasher.update(b"homeboy-workspace-content-v1\0");
    for (relative, kind, mode, contents) in entries {
        hasher.update(relative.as_bytes());
        hasher.update(kind.as_bytes());
        hasher.update(mode.to_le_bytes());
        hasher.update((contents.len() as u64).to_le_bytes());
        hasher.update(contents);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn content_hash_root(path: &Path) -> Result<std::path::PathBuf> {
    path.canonicalize().map_err(|err| {
        Error::internal_io(err.to_string(), Some("canonicalize workspace".to_string()))
    })
}

fn collect_content_hash_entries_v2(
    root: &Path,
    path: &Path,
    logical: &Path,
    excludes: &[String],
    ancestors: &mut Vec<std::path::PathBuf>,
    hasher: &mut Sha256,
    executable_capability: ExecutableCapability,
    mut manifest: Option<&mut WorkspaceContentManifest>,
) -> Result<()> {
    let mut children = fs::read_dir(path)
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
    children.sort_by_key(|entry| entry.path());
    for entry in children {
        let entry_path = entry.path();
        let relative_path = logical.join(entry.file_name());
        let relative = relative_path.to_string_lossy().replace('\\', "/");
        let is_runner_metadata_directory = relative == ".homeboy";
        if relative == ".git"
            || is_excluded(root, &root.join(&relative_path), excludes, &[])
            || is_reserved_runner_workspace_path(&relative)
        {
            continue;
        }
        let link_metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        // A symlink whose target resolves is dereferenced so its target content
        // stays provenance-bound (target drift changes the hash). A symlink whose
        // target is intentionally unavailable on the controller (e.g. a tracked
        // `blogs.dir -> /nfs`) is a valid Git workspace shape: bind its target
        // text deterministically instead of refusing the whole hash. (#8374)
        let resolved = if link_metadata.file_type().is_symlink() {
            match entry_path.canonicalize() {
                Ok(resolved) => resolved,
                Err(_) => {
                    let target = fs::read_link(&entry_path).map_err(|err| {
                        Error::internal_io(
                            err.to_string(),
                            Some("read sync symlink target".to_string()),
                        )
                    })?;
                    let target = target.to_string_lossy().replace('\\', "/");
                    hasher.update(relative.as_bytes());
                    hasher.update(b"\0symlink\0");
                    hasher.update((target.len() as u64).to_le_bytes());
                    hasher.update(target.as_bytes());
                    record_workspace_content_manifest_entry(
                        &mut manifest,
                        relative,
                        "symlink",
                        Some(format!("sha256:{:x}", Sha256::digest(target.as_bytes()))),
                        Some(target.len() as u64),
                        None,
                    );
                    continue;
                }
            }
        } else {
            entry_path.clone()
        };
        let metadata = fs::metadata(&resolved).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        if metadata.is_dir() {
            let canonical = resolved
                .canonicalize()
                .map_err(|err| Error::internal_io(err.to_string(), None))?;
            if ancestors.contains(&canonical) {
                return Err(Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused a symlink cycle",
                    Some(entry_path.display().to_string()),
                    None,
                ));
            }
            // The runner adds `.homeboy/runner-workspace.json` after transport.
            // Recurse through the directory so user-owned children remain bound,
            // but omit the transport-owned container entry and record itself.
            if !is_runner_metadata_directory {
                hasher.update(relative.as_bytes());
                hasher.update(b"\0dir\0");
                record_workspace_content_manifest_entry(
                    &mut manifest,
                    relative.clone(),
                    "directory",
                    None,
                    None,
                    None,
                );
            }
            ancestors.push(canonical);
            collect_content_hash_entries_v2(
                root,
                &resolved,
                &relative_path,
                excludes,
                ancestors,
                hasher,
                executable_capability,
                manifest.as_deref_mut(),
            )?;
            ancestors.pop();
        } else if metadata.is_file() {
            let contents = fs::read(&resolved).map_err(|err| {
                Error::internal_io(err.to_string(), Some("read sync file".to_string()))
            })?;
            hasher.update(relative.as_bytes());
            hasher.update(b"\0file\0");
            if let Some(executable) = executable_capability.value(&metadata) {
                hasher.update([executable as u8]);
            }
            hasher.update((contents.len() as u64).to_le_bytes());
            hasher.update(&contents);
            record_workspace_content_manifest_entry(
                &mut manifest,
                relative,
                "file",
                Some(format!("sha256:{:x}", Sha256::digest(&contents))),
                Some(contents.len() as u64),
                executable_capability.owner_value(&metadata),
            );
        }
    }
    Ok(())
}

fn record_workspace_content_manifest_entry(
    manifest: &mut Option<&mut WorkspaceContentManifest>,
    path: String,
    kind: &str,
    sha256: Option<String>,
    bytes: Option<u64>,
    owner_executable: Option<bool>,
) {
    let Some(manifest) = manifest.as_deref_mut() else {
        return;
    };
    manifest.entry_count += 1;
    manifest.entries.push(WorkspaceContentManifestEntry {
        path,
        kind: kind.to_string(),
        sha256,
        bytes,
        owner_executable,
    });
}

#[cfg(unix)]
fn file_has_any_execute_bit(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

#[cfg(unix)]
fn file_is_owner_executable(metadata: &fs::Metadata) -> bool {
    use std::os::unix::fs::PermissionsExt;
    // Snapshot extraction may apply the runner's umask to group/other bits.
    // The owner execute bit is the portable capability required to run a file.
    metadata.permissions().mode() & 0o100 != 0
}

#[cfg(not(unix))]
fn file_has_any_execute_bit(_metadata: &fs::Metadata) -> bool {
    false
}

#[cfg(not(unix))]
fn file_is_owner_executable(_metadata: &fs::Metadata) -> bool {
    false
}

#[derive(Clone, Copy)]
enum ExecutableCapability {
    None,
    Any,
    Owner,
}

impl ExecutableCapability {
    fn value(self, metadata: &fs::Metadata) -> Option<bool> {
        match self {
            Self::None => None,
            Self::Any => Some(file_has_any_execute_bit(metadata)),
            Self::Owner => Some(file_is_owner_executable(metadata)),
        }
    }
    fn owner_value(self, metadata: &fs::Metadata) -> Option<bool> {
        (!matches!(self, Self::None)).then(|| file_is_owner_executable(metadata))
    }
}

fn workspace_content_hash_contract(policy: &str) -> Option<(String, ExecutableCapability)> {
    let capability = match policy {
        WORKSPACE_CONTENT_PERMISSION_PORTABLE => ExecutableCapability::None,
        WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE if cfg!(unix) => ExecutableCapability::Any,
        WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE if cfg!(unix) => {
            ExecutableCapability::Owner
        }
        _ => return None,
    };
    workspace_content_hash_algorithm(policy).map(|algorithm| (algorithm, capability))
}

#[cfg(unix)]
fn mode_bits(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn mode_bits(_metadata: &fs::Metadata) -> u32 {
    0
}

fn collect_content_hash_entries_v1(
    root: &Path,
    path: &Path,
    logical: &Path,
    excludes: &[String],
    ancestors: &mut Vec<std::path::PathBuf>,
    entries: &mut Vec<(String, &'static str, u32, Vec<u8>)>,
) -> Result<()> {
    let mut children = fs::read_dir(path)
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
    children.sort_by_key(|entry| entry.path());
    for entry in children {
        let entry_path = entry.path();
        let relative_path = logical.join(entry.file_name());
        let relative = relative_path.to_string_lossy().replace('\\', "/");
        let is_runner_metadata_directory = relative == ".homeboy";
        if relative == ".git"
            || is_excluded(root, &root.join(&relative_path), excludes, &[])
            || is_reserved_runner_workspace_path(&relative)
        {
            continue;
        }
        let link_metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        // Resolvable symlinks are dereferenced (target content stays bound); a
        // tracked symlink with an unavailable target binds its target text
        // instead of failing the hash. See the v2 collector for rationale. (#8374)
        let resolved = if link_metadata.file_type().is_symlink() {
            match entry_path.canonicalize() {
                Ok(resolved) => resolved,
                Err(_) => {
                    let target = fs::read_link(&entry_path).map_err(|err| {
                        Error::internal_io(
                            err.to_string(),
                            Some("read sync symlink target".to_string()),
                        )
                    })?;
                    let target = target.to_string_lossy().replace('\\', "/");
                    entries.push((relative, "\0symlink\0", 0, target.into_bytes()));
                    continue;
                }
            }
        } else {
            entry_path.clone()
        };
        let metadata = fs::metadata(&resolved).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        if metadata.is_dir() {
            let canonical = resolved
                .canonicalize()
                .map_err(|err| Error::internal_io(err.to_string(), None))?;
            if ancestors.contains(&canonical) {
                return Err(Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused a symlink cycle",
                    Some(entry_path.display().to_string()),
                    None,
                ));
            }
            if !is_runner_metadata_directory {
                entries.push((relative, "\0dir\0", mode_bits(&metadata), Vec::new()));
            }
            ancestors.push(canonical);
            collect_content_hash_entries_v1(
                root,
                &resolved,
                &relative_path,
                excludes,
                ancestors,
                entries,
            )?;
            ancestors.pop();
        } else if metadata.is_file() {
            let contents = fs::read(&resolved).map_err(|err| {
                Error::internal_io(err.to_string(), Some("read sync file".to_string()))
            })?;
            entries.push((relative, "\0file\0", mode_bits(&metadata), contents));
        }
    }
    Ok(())
}

pub(crate) fn local_snapshot_stats(
    path: &Path,
    excludes: &[String],
    includes: &[String],
) -> Result<SnapshotStats> {
    let mut stats = SnapshotStats { files: 0, bytes: 0 };
    collect_stats(path, path, excludes, includes, &mut stats)?;
    Ok(stats)
}

fn hash_snapshot_tree(
    root: &Path,
    path: &Path,
    excludes: &[String],
    includes: &[String],
    hasher: &mut Sha256,
) -> Result<()> {
    let mut entries = fs::read_dir(path)
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
    entries.sort_by_key(|entry| entry.path());

    for entry in entries {
        let entry_path = entry.path();
        if is_excluded(root, &entry_path, excludes, includes) {
            continue;
        }
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        let rel = entry_path
            .strip_prefix(root)
            .unwrap_or(&entry_path)
            .to_string_lossy();
        hasher.update(rel.as_bytes());
        if metadata.is_dir() {
            hasher.update(b"/dir");
            hash_snapshot_tree(root, &entry_path, excludes, includes, hasher)?;
        } else if metadata.is_file() {
            hasher.update(b"/file");
            hasher.update(metadata.len().to_le_bytes());
            let contents = fs::read(&entry_path).map_err(|err| {
                Error::internal_io(err.to_string(), Some("read sync file".to_string()))
            })?;
            hasher.update(contents);
        }
    }
    Ok(())
}

fn collect_stats(
    root: &Path,
    path: &Path,
    excludes: &[String],
    includes: &[String],
    stats: &mut SnapshotStats,
) -> Result<()> {
    for entry in fs::read_dir(path).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
    })? {
        let entry = entry.map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
        let entry_path = entry.path();
        if is_excluded(root, &entry_path, excludes, includes) {
            continue;
        }
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        if metadata.is_dir() {
            collect_stats(root, &entry_path, excludes, includes, stats)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(metadata.len());
        }
    }
    Ok(())
}

pub(super) fn is_excluded(
    root: &Path,
    path: &Path,
    excludes: &[String],
    includes: &[String],
) -> bool {
    let rel = path.strip_prefix(root).unwrap_or(path).to_string_lossy();
    let rel = rel.trim_start_matches('/');
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("");
    if includes.iter().any(|pattern| {
        pattern == rel || pattern == name || glob_match(pattern, rel) || glob_match(pattern, name)
    }) {
        return false;
    }
    excludes.iter().any(|pattern| {
        let root_anchored = pattern.starts_with("./");
        let pattern = pattern.trim_start_matches("./");
        let directory_pattern = pattern.trim_end_matches('/');
        pattern == rel
            || directory_pattern == rel
            || glob_match(pattern, rel)
            || (!root_anchored && (pattern == name || glob_match(pattern, name)))
    })
}

pub(crate) fn materialize_snapshot(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
) -> Result<()> {
    match runner.kind {
        RunnerKind::Local => materialize_snapshot_piped(
            local_path,
            &format!(
                "sh -c {}",
                shell::quote_arg(&snapshot_install_command(remote_path))
            ),
            excludes,
            "materialize local workspace snapshot",
        ),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                materialize_snapshot_piped(
                    local_path,
                    &format!(
                        "sh -c {}",
                        shell::quote_arg(&snapshot_install_command(remote_path))
                    ),
                    excludes,
                    "materialize local workspace snapshot",
                )
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let remote_command = snapshot_install_command(remote_path);
                let target = format!(
                    "ssh {ssh_args} {remote} {remote_command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    remote_command = shell::quote_arg(&remote_command),
                );
                materialize_snapshot_piped(
                    local_path,
                    &target,
                    excludes,
                    "materialize SSH workspace snapshot",
                )
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SnapshotManifestDelta {
    retained_paths: Vec<String>,
    pub(crate) changed_paths: Vec<String>,
    pub(crate) deleted_paths: Vec<String>,
    pub(crate) replaced_paths: Vec<String>,
    pub(crate) changed_file_paths: Vec<String>,
    pub(crate) transfer: SnapshotTransferStats,
}

pub(crate) fn snapshot_manifest_delta(
    controller: &WorkspaceContentManifest,
    seed: &WorkspaceContentManifest,
) -> Result<SnapshotManifestDelta> {
    validate_workspace_content_manifest(controller)?;
    validate_workspace_content_manifest(seed)?;
    let index = |manifest: &WorkspaceContentManifest| -> Result<BTreeMap<String, WorkspaceContentManifestEntry>> {
        let mut entries = BTreeMap::new();
        for entry in &manifest.entries {
            if entries.insert(entry.path.clone(), entry.clone()).is_some() {
                return Err(Error::internal_unexpected(
                    "workspace snapshot content manifest contains duplicate or invalid paths",
                ));
            }
        }
        Ok(entries)
    };
    let controller_entries = index(controller)?;
    let seed_entries = index(seed)?;
    let changed_paths = controller_entries
        .iter()
        .filter_map(|(path, entry)| (seed_entries.get(path) != Some(entry)).then(|| path.clone()))
        .collect::<Vec<_>>();
    let replaced_paths = changed_paths
        .iter()
        .filter(|path| {
            seed_entries
                .get(*path)
                .is_some_and(|seed_entry| seed_entry.kind != controller_entries[*path].kind)
        })
        .cloned()
        .collect::<Vec<_>>();
    let changed_file_paths = changed_paths
        .iter()
        .filter(|path| controller_entries[*path].kind == "file")
        .cloned()
        .collect::<Vec<_>>();
    let deleted_paths = seed_entries
        .keys()
        .filter(|path| !controller_entries.contains_key(*path))
        .cloned()
        .collect::<Vec<_>>();
    let count_files = |entries: Vec<&WorkspaceContentManifestEntry>| {
        entries
            .into_iter()
            .fold(ByteFileCounts::default(), |mut counts, entry| {
                if entry.kind == "file" {
                    counts.files += 1;
                    counts.bytes = counts.bytes.saturating_add(entry.bytes.unwrap_or_default());
                }
                counts
            })
    };
    let transferred = count_files(
        changed_paths
            .iter()
            .filter_map(|path| controller_entries.get(path))
            .collect(),
    );
    let final_size = count_files(controller_entries.values().collect());
    Ok(SnapshotManifestDelta {
        retained_paths: controller_entries.keys().cloned().collect(),
        changed_paths,
        deleted_paths,
        replaced_paths,
        changed_file_paths,
        transfer: SnapshotTransferStats {
            reused: ByteFileCounts {
                files: final_size.files.saturating_sub(transferred.files),
                bytes: final_size.bytes.saturating_sub(transferred.bytes),
            },
            transferred,
            final_size,
        },
    })
}

fn validate_workspace_content_manifest(manifest: &WorkspaceContentManifest) -> Result<()> {
    if manifest.entry_count != manifest.entries.len() {
        return Err(Error::internal_unexpected(
            "workspace snapshot content manifest is incomplete or corrupt",
        ));
    }
    for entry in &manifest.entries {
        let path = Path::new(&entry.path);
        let valid_path = !entry.path.is_empty()
            && !entry.path.contains('\0')
            && !path.is_absolute()
            && path
                .components()
                .all(|component| matches!(component, std::path::Component::Normal(_)));
        let valid_file = entry.kind == "file"
            && entry.bytes.is_some()
            && entry.sha256.as_deref().is_some_and(|hash| {
                hash.len() == 71
                    && hash.starts_with("sha256:")
                    && hash[7..].bytes().all(|byte| byte.is_ascii_hexdigit())
            });
        if !valid_path
            || !(entry.kind == "directory" || valid_file)
            || (entry.kind == "directory"
                && (entry.sha256.is_some()
                    || entry.bytes.is_some()
                    || entry.owner_executable.is_some()))
        {
            return Err(Error::internal_unexpected(
                "workspace snapshot content manifest contains invalid entries",
            ));
        }
    }
    Ok(())
}

/// Materialize an immutable snapshot by linking unchanged content from a
/// compatible runner-local seed, deleting paths absent from the controller
/// manifest, and transporting only the manifest's explicit changed paths.
pub(crate) fn materialize_snapshot_incremental(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    seed_path: &str,
    excludes: &[String],
    delta: &SnapshotManifestDelta,
) -> Result<SnapshotTransferStats> {
    let temporary = format!("{}.tmp-{}", remote_path, uuid::Uuid::new_v4());
    if !incremental_prepare_command_fits(remote_path, &temporary, seed_path, delta) {
        materialize_snapshot(runner, local_path, remote_path, excludes)?;
        return Ok(SnapshotTransferStats {
            reused: ByteFileCounts::default(),
            transferred: delta.transfer.final_size.clone(),
            final_size: delta.transfer.final_size.clone(),
        });
    }
    let prepare = incremental_prepare_command(remote_path, &temporary, seed_path, delta);
    if prepare.len() > INCREMENTAL_PREPARE_COMMAND_MAX_BYTES {
        materialize_snapshot(runner, local_path, remote_path, excludes)?;
        return Ok(SnapshotTransferStats {
            reused: ByteFileCounts::default(),
            transferred: delta.transfer.final_size.clone(),
            final_size: delta.transfer.final_size.clone(),
        });
    }
    let finalize = incremental_finalize_command(remote_path, &temporary);
    let result = match runner.kind {
        RunnerKind::Local => {
            run_shell_command(&prepare, "prepare incremental local workspace snapshot")
                .and_then(|_| {
                    materialize_changed_paths(
                        local_path,
                        &local_extract_command(&temporary),
                        &delta.changed_paths,
                        "materialize incremental local workspace delta",
                    )
                })
                .and_then(|_| {
                    run_shell_command(&finalize, "finalize incremental local workspace snapshot")
                })
        }
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                run_shell_command(&prepare, "prepare incremental local workspace snapshot")
                    .and_then(|_| {
                        materialize_changed_paths(
                            local_path,
                            &local_extract_command(&temporary),
                            &delta.changed_paths,
                            "materialize incremental local workspace delta",
                        )
                    })
                    .and_then(|_| {
                        run_shell_command(
                            &finalize,
                            "finalize incremental local workspace snapshot",
                        )
                    })
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let remote_shell = |script: &str| {
                    format!(
                        "ssh {} {} {}",
                        ssh_args(&client),
                        shell::quote_arg(&remote),
                        shell::quote_arg(script),
                    )
                };
                run_shell_command(
                    &remote_shell(&prepare),
                    "prepare incremental SSH workspace snapshot",
                )
                .and_then(|_| {
                    materialize_changed_paths(
                        local_path,
                        &remote_extract_command(&client, &remote, &temporary),
                        &delta.changed_paths,
                        "materialize incremental SSH workspace delta",
                    )
                })
                .and_then(|_| {
                    run_shell_command(
                        &remote_shell(&finalize),
                        "finalize incremental SSH workspace snapshot",
                    )
                })
            }
        }
    };
    if result.is_err() {
        cleanup_incremental_temporary(runner, &temporary);
    }
    result.map(|_| delta.transfer.clone())
}

/// Stage a delta over a prepared workspace while retaining paths outside the
/// included source manifest, such as dependencies and generated runtime assets.
pub(crate) fn materialize_prepared_workspace_update(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    delta: &SnapshotManifestDelta,
    expected_lease: &str,
    metadata_json: &str,
) -> Result<SnapshotTransferStats> {
    let temporary = format!("{}.tmp-{}", remote_path, uuid::Uuid::new_v4());
    let backup = format!("{}.previous-{}", remote_path, uuid::Uuid::new_v4());
    let lock = format!("{}.update-lock", remote_path);
    // `mkdir` is an atomic filesystem primitive on the local runner and over
    // SSH. Holding this directory across all phases gives a runner-neutral CAS
    // boundary without relying on controller-local advisory locks.
    let acquire_lock = format!("mkdir {}", shell::quote_arg(&lock));
    let release_lock = format!("rmdir {}", shell::quote_arg(&lock));
    run_shell_command(
        &shell_command_for_runner(runner, &acquire_lock)?,
        "acquire prepared workspace update lock",
    )?;
    let removals = delta
        .deleted_paths
        .iter()
        .chain(delta.replaced_paths.iter())
        .chain(delta.changed_file_paths.iter())
        .cloned()
        .collect::<Vec<_>>();
    let mut removal_list = tempfile::NamedTempFile::new().map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("stage prepared workspace removals".to_string()),
        )
    })?;
    for path in &removals {
        removal_list
            .write_all(path.as_bytes())
            .and_then(|_| removal_list.write_all(&[0]))
            .map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("stage prepared workspace removals".to_string()),
                )
            })?;
    }
    let removal_path = format!("{temporary}/.homeboy/update-removals");
    let metadata_path = format!("{temporary}/{RUNNER_WORKSPACE_METADATA_FILE}");
    let prepare = format!(
        "rm -rf {temporary} {backup} && mkdir -p {temporary} && {seed} && mkdir -p {metadata_parent}",
        temporary = shell::quote_arg(&temporary),
        backup = shell::quote_arg(&backup),
        seed = seed_snapshot_command(remote_path, &temporary),
        metadata_parent = shell::quote_arg(&format!("{temporary}/.homeboy")),
    );
    let finalize = format!(
        "grep -qF -- {lease} {live_metadata} && mv {remote} {backup} && if mv {temporary} {remote}; then rm -rf {backup} || true; else if mv {backup} {remote}; then exit 1; else printf '%s\\n' 'prepared workspace promotion failed; original remains at {backup}' >&2; exit 2; fi; fi",
        lease = shell::quote_arg(&format!("\"workspace_lease\": \"{expected_lease}\"")),
        live_metadata = shell::quote_arg(&format!("{remote_path}/{RUNNER_WORKSPACE_METADATA_FILE}")),
        remote = shell::quote_arg(remote_path), temporary = shell::quote_arg(&temporary), backup = shell::quote_arg(&backup),
    );
    let remove = format!(
        "cd {} && xargs -0 -n 1 rm -rf -- < {} && rm -f {}",
        shell::quote_arg(&temporary),
        shell::quote_arg(&removal_path),
        shell::quote_arg(&removal_path),
    );
    let result = match runner.kind {
        RunnerKind::Local => {
            let result = run_shell_command(&prepare, "update prepared local workspace")
                .and_then(|_| {
                    fs::copy(removal_list.path(), &removal_path)
                        .map(|_| ())
                        .map_err(|err| {
                            Error::internal_io(
                                err.to_string(),
                                Some("stage prepared workspace removals".to_string()),
                            )
                        })
                })
                .and_then(|_| {
                    fs::write(&metadata_path, metadata_json).map_err(|err| {
                        Error::internal_io(
                            err.to_string(),
                            Some("stage prepared workspace metadata".to_string()),
                        )
                    })
                })
                .and_then(|_| run_shell_command(&remove, "update prepared local workspace"))
                .and_then(|_| {
                    materialize_changed_paths(
                        local_path,
                        &local_extract_command(&temporary),
                        &delta.changed_paths,
                        "update prepared local workspace",
                    )
                })
                .and_then(|_| run_shell_command(&finalize, "update prepared local workspace"));
            result
        }
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                run_shell_command(&prepare, "update prepared local workspace")
                    .and_then(|_| {
                        fs::copy(removal_list.path(), &removal_path)
                            .map(|_| ())
                            .map_err(|err| {
                                Error::internal_io(
                                    err.to_string(),
                                    Some("stage prepared workspace removals".to_string()),
                                )
                            })
                    })
                    .and_then(|_| {
                        fs::write(&metadata_path, metadata_json).map_err(|err| {
                            Error::internal_io(
                                err.to_string(),
                                Some("stage prepared workspace metadata".to_string()),
                            )
                        })
                    })
                    .and_then(|_| run_shell_command(&remove, "update prepared local workspace"))
                    .and_then(|_| {
                        materialize_changed_paths(
                            local_path,
                            &local_extract_command(&temporary),
                            &delta.changed_paths,
                            "update prepared local workspace",
                        )
                    })
                    .and_then(|_| run_shell_command(&finalize, "update prepared local workspace"))
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let remote_shell = |script: &str| {
                    format!(
                        "ssh {} {} {}",
                        ssh_args(&client),
                        shell::quote_arg(&remote),
                        shell::quote_arg(script)
                    )
                };
                run_shell_command(&remote_shell(&prepare), "update prepared SSH workspace")
                    .and_then(|_| {
                        let output = client
                            .upload_file(&removal_list.path().display().to_string(), &removal_path);
                        output.success.then_some(()).ok_or_else(|| {
                            Error::internal_unexpected(format!(
                                "stage prepared workspace removals failed: {}",
                                output.stderr.trim()
                            ))
                        })
                    })
                    .and_then(|_| {
                        let metadata = tempfile::NamedTempFile::new().map_err(|err| {
                            Error::internal_io(
                                err.to_string(),
                                Some("stage prepared workspace metadata".to_string()),
                            )
                        })?;
                        fs::write(metadata.path(), metadata_json).map_err(|err| {
                            Error::internal_io(
                                err.to_string(),
                                Some("stage prepared workspace metadata".to_string()),
                            )
                        })?;
                        let output = client
                            .upload_file(&metadata.path().display().to_string(), &metadata_path);
                        output.success.then_some(()).ok_or_else(|| {
                            Error::internal_unexpected(format!(
                                "stage prepared workspace metadata failed: {}",
                                output.stderr.trim()
                            ))
                        })
                    })
                    .and_then(|_| {
                        run_shell_command(&remote_shell(&remove), "update prepared SSH workspace")
                    })
                    .and_then(|_| {
                        materialize_changed_paths(
                            local_path,
                            &remote_extract_command(&client, &remote, &temporary),
                            &delta.changed_paths,
                            "update prepared SSH workspace",
                        )
                    })
                    .and_then(|_| {
                        run_shell_command(&remote_shell(&finalize), "update prepared SSH workspace")
                    })
            }
        }
    };
    if result.is_err() {
        cleanup_incremental_temporary(runner, &temporary);
    }
    let release_result = run_shell_command(
        &shell_command_for_runner(runner, &release_lock)?,
        "release prepared workspace update lock",
    );
    result.and(release_result).map(|_| delta.transfer.clone())
}

fn saturating_shell_quote_upper_bound(value: &str) -> usize {
    // `quote_arg` can render each apostrophe as `'\\''` and add delimiters.
    const SHELL_META: &[char] = &[
        ' ', '\t', '\n', '\'', '"', '\\', '$', '`', '!', '*', '?', '[', ']', '(', ')', '{', '}',
        '<', '>', '|', '&', ';', '#', '~',
    ];
    if value.contains(SHELL_META) {
        value.len().saturating_mul(5).saturating_add(2)
    } else {
        value.len()
    }
}

pub(super) fn incremental_prepare_command_fits(
    remote_path: &str,
    temporary: &str,
    seed_path: &str,
    delta: &SnapshotManifestDelta,
) -> bool {
    incremental_prepare_command_preflight_bytes(remote_path, temporary, seed_path, delta)
        <= INCREMENTAL_PREPARE_COMMAND_MAX_BYTES
}

fn incremental_prepare_command_preflight_bytes(
    remote_path: &str,
    temporary: &str,
    seed_path: &str,
    delta: &SnapshotManifestDelta,
) -> usize {
    let parent = parent_remote_path(remote_path);
    let mut bytes = INCREMENTAL_PREPARE_COMMAND_FIXED_OVERHEAD_BYTES;

    // Account for every path embedded in the outer command, including the
    // nested `sh -c` cleanup predicate for retained paths.
    for path in &delta.retained_paths {
        bytes = bytes.saturating_add(
            saturating_shell_quote_upper_bound(path)
                .saturating_mul(5)
                .saturating_add(64),
        );
    }
    for path in delta
        .deleted_paths
        .iter()
        .chain(delta.replaced_paths.iter())
        .chain(delta.changed_file_paths.iter())
    {
        bytes = bytes.saturating_add(saturating_shell_quote_upper_bound(path).saturating_add(64));
    }

    // These values are repeated in the generated setup, clone, cleanup, and
    // removal clauses. Multiplying their conservative quoted size keeps the
    // check independent of controller or runner path length.
    bytes = bytes.saturating_add(saturating_shell_quote_upper_bound(&parent).saturating_mul(4));
    bytes = bytes.saturating_add(saturating_shell_quote_upper_bound(temporary).saturating_mul(12));
    bytes = bytes.saturating_add(saturating_shell_quote_upper_bound(seed_path).saturating_mul(2));
    bytes
}

fn cleanup_incremental_temporary(runner: &Runner, temporary: &str) {
    let command = format!("rm -rf -- {}", shell::quote_arg(temporary));
    match runner.kind {
        RunnerKind::Local => {
            let _ = run_shell_command(&command, "clean incremental workspace temporary");
        }
        RunnerKind::Ssh => {
            if let Ok((_server, client)) = ssh_client_for_runner(runner) {
                if client.is_local {
                    let _ = run_shell_command(&command, "clean incremental workspace temporary");
                } else {
                    let remote = format!("{}@{}", client.user, client.host);
                    let command = format!(
                        "ssh {} {} {}",
                        ssh_args(&client),
                        shell::quote_arg(&remote),
                        shell::quote_arg(&command),
                    );
                    let _ =
                        run_shell_command(&command, "clean incremental SSH workspace temporary");
                }
            }
        }
    }
}

fn incremental_prepare_command(
    remote_path: &str,
    temporary: &str,
    seed_path: &str,
    delta: &SnapshotManifestDelta,
) -> String {
    let parent = parent_remote_path(remote_path);
    let removals = delta
        .deleted_paths
        .iter()
        .chain(delta.replaced_paths.iter())
        // The seed is hard-link cloned. Unlink changed files before tar writes
        // them so a new snapshot can never mutate its immutable seed inode.
        .chain(delta.changed_file_paths.iter())
        .map(|path| {
            format!(
                "rm -rf -- {}/{}",
                shell::quote_arg(temporary),
                shell::quote_arg(path)
            )
        })
        .collect::<Vec<_>>()
        .join(" && ");
    let retain = delta
        .retained_paths
        .iter()
        .map(|path| format!("[ \"$relative\" = {} ]", shell::quote_arg(path)))
        .collect::<Vec<_>>()
        .join(" || ");
    let retain = if retain.is_empty() {
        "false".to_string()
    } else {
        retain
    };
    let cleanup = format!(
        "find {temporary} -mindepth 1 -depth -exec sh -c {script} sh {temporary} {{}} +",
        temporary = shell::quote_arg(temporary),
        script = shell::quote_arg(&format!(
            "root=$1; shift; for path do relative=${{path#\"$root\"/}}; if ! ({retain}); then rm -rf -- \"$path\"; fi; done"
        )),
    );
    format!(
        "{owner_capture} ; mkdir -p {parent} && rm -rf {temporary} && mkdir -p {temporary} && {seed} && {cleanup} {removals}",
        owner_capture = owner_capture_shell(&parent),
        parent = shell::quote_arg(&parent),
        temporary = shell::quote_arg(&temporary),
        seed = seed_snapshot_command(seed_path, temporary),
        cleanup = cleanup,
        removals = if removals.is_empty() { String::new() } else { format!(" && {removals}") },
    )
}

fn incremental_finalize_command(remote_path: &str, temporary: &str) -> String {
    let parent = parent_remote_path(remote_path);
    format!(
        "mv {} {} && {}",
        shell::quote_arg(temporary),
        shell::quote_arg(remote_path),
        owner_restore_shell(&parent, remote_path),
    )
}

fn seed_snapshot_command(seed_path: &str, destination: &str) -> String {
    // A hard-link clone gives the new immutable snapshot its own directory
    // without copying unchanged bytes. If the runner filesystem cannot link
    // across its storage boundary, retain correctness with a local copy while
    // transfer metrics continue to report controller-to-runner content only.
    format!(
        "(cp -al {seed}/. {destination}/ 2>/dev/null || cp -a {seed}/. {destination}/) && rm -f {destination}/{metadata}",
        seed = shell::quote_arg(seed_path),
        destination = shell::quote_arg(destination),
        metadata = shell::quote_arg(RUNNER_WORKSPACE_METADATA_FILE),
    )
}

fn materialize_changed_paths(
    local_path: &Path,
    target_command: &str,
    changed_paths: &[String],
    action: &str,
) -> Result<()> {
    if changed_paths.is_empty() {
        return Ok(());
    }
    let mut list = tempfile::NamedTempFile::new()
        .map_err(|err| Error::internal_io(err.to_string(), Some(action.to_string())))?;
    for path in changed_paths {
        list.write_all(path.as_bytes())
            .and_then(|_| list.write_all(&[0]))
            .map_err(|err| Error::internal_io(err.to_string(), Some(action.to_string())))?;
    }
    let command = format!(
        "COPYFILE_DISABLE=1 tar --no-xattrs -h -C {} --null -T {} -cf - | {}",
        shell::quote_arg(&local_path.display().to_string()),
        shell::quote_arg(&list.path().display().to_string()),
        target_command,
    );
    run_shell_command(&command, action)
}

fn local_extract_command(destination: &str) -> String {
    format!("tar --no-xattrs -xf - -C {}", shell::quote_arg(destination))
}

fn remote_extract_command(
    client: &homeboy_core::server::SshClient,
    remote: &str,
    destination: &str,
) -> String {
    format!(
        "ssh {} {} {}",
        ssh_args(client),
        shell::quote_arg(remote),
        shell::quote_arg(&local_extract_command(destination)),
    )
}

pub(crate) fn materialize_snapshot_git(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    snapshot: &str,
) -> Result<SyntheticCheckoutIdentity> {
    materialize_snapshot(runner, local_path, remote_path, excludes)?;
    let source_dirty = git_output(local_path, &["status", "--porcelain=v1"])
        .map(|status| !status.trim().is_empty())
        .unwrap_or(false);
    initialize_synthetic_git_checkout(runner, local_path, remote_path, snapshot, source_dirty)
}

/// Apply a filtered controller snapshot over an existing runner checkout while
/// preserving its runner-created Git directory. The overlay is staged before
/// replacing worktree content so an interrupted transfer cannot leave a
/// partially extracted archive behind.
pub(crate) fn materialize_snapshot_overlay(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
) -> Result<()> {
    let target = format!(
        "sh -c {}",
        shell::quote_arg(&snapshot_overlay_install_command(remote_path))
    );
    match runner.kind {
        RunnerKind::Local => materialize_snapshot_piped(
            local_path,
            &target,
            excludes,
            "apply local Git workspace snapshot overlay",
        ),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                materialize_snapshot_piped(
                    local_path,
                    &target,
                    excludes,
                    "apply local Git workspace snapshot overlay",
                )
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let command = snapshot_overlay_install_command(remote_path);
                materialize_snapshot_piped(
                    local_path,
                    &format!(
                        "ssh {} {} {}",
                        ssh_args(&client),
                        shell::quote_arg(&remote),
                        shell::quote_arg(&command),
                    ),
                    excludes,
                    "apply SSH Git workspace snapshot overlay",
                )
            }
        }
    }
}

/// Identity of the synthetic git checkout materialized for a `snapshot-git`
/// sync. Surfaced as run evidence so write-capable agent-task dispatches can
/// trace the dirty controller-side worktree back to the synthetic commit that
/// carries the snapshot into the runner workspace.
#[derive(Debug, Clone)]
pub(crate) struct SyntheticCheckoutIdentity {
    /// Commit SHA of the synthetic checkout created in the runner workspace.
    pub(crate) synthetic_commit: String,
    pub(crate) synthetic_ref: String,
    pub(crate) synthetic_tree: String,
}

fn initialize_synthetic_git_checkout(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    snapshot: &str,
    source_dirty: bool,
) -> Result<SyntheticCheckoutIdentity> {
    let remote_url = git_output(local_path, &["config", "--get", "remote.origin.url"]).ok();
    let source_head = git_output(local_path, &["rev-parse", "HEAD"]).ok();
    let command = synthetic_git_checkout_command(
        remote_path,
        snapshot,
        remote_url.as_deref(),
        source_head.as_deref(),
        source_dirty,
    );

    match runner.kind {
        RunnerKind::Local => {
            run_shell_command(&command, "initialize synthetic snapshot git checkout")?;
        }
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                run_shell_command(&command, "initialize synthetic snapshot git checkout")?;
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let ssh_command = format!(
                    "ssh {ssh_args} {remote} {command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    command = shell::quote_arg(&command),
                );
                run_shell_command(
                    &ssh_command,
                    "initialize SSH synthetic snapshot git checkout",
                )?;
            }
        }
    }

    // The readback is the provenance contract for later harvest verification.
    // A sync without it must fail rather than return an unverifiable workspace.
    let synthetic_commit = synthetic_checkout_value(runner, remote_path, "rev-parse HEAD")?;
    let synthetic_ref = synthetic_checkout_value(runner, remote_path, "symbolic-ref --quiet HEAD")?;
    let synthetic_tree = synthetic_checkout_value(runner, remote_path, "rev-parse HEAD^{tree}")?;

    Ok(SyntheticCheckoutIdentity {
        synthetic_commit,
        synthetic_ref,
        synthetic_tree,
    })
}

pub(super) fn synthetic_checkout_value(
    runner: &Runner,
    remote_path: &str,
    command: &str,
) -> Result<String> {
    let value = match runner.kind {
        RunnerKind::Local => run_shell_capture(&format!(
            "git -C {} {command}",
            shell::quote_arg(remote_path)
        )),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                run_shell_capture(&format!(
                    "git -C {} {command}",
                    shell::quote_arg(remote_path)
                ))
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let remote_command = format!(
                    "git -C {remote_path} {command}",
                    remote_path = shell::quote_arg(remote_path),
                );
                let ssh_command = format!(
                    "ssh {ssh_args} {remote} {remote_command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    remote_command = shell::quote_arg(&remote_command),
                );
                run_shell_capture(&ssh_command)
            }
        }
    };
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            Error::internal_io(
                format!(
                "could not read `{command}` from synthetic snapshot-git checkout at `{remote_path}`"
            ),
                Some("capture synthetic snapshot-git checkout provenance".to_string()),
            )
        })
}

fn synthetic_git_checkout_command(
    remote_path: &str,
    snapshot: &str,
    remote_url: Option<&str>,
    source_head: Option<&str>,
    source_dirty: bool,
) -> String {
    let remote_path = shell::quote_arg(remote_path);
    let snapshot = shell::quote_arg(snapshot);
    let source_head = shell::quote_arg(source_head.unwrap_or("unknown"));
    let set_remote = remote_url
        .filter(|value| !value.trim().is_empty())
        .map(|value| {
            format!(
                " && git -C {remote_path} remote add origin {remote_url}",
                remote_url = shell::quote_arg(value)
            )
        })
        .unwrap_or_default();

    format!(
        "git -C {remote_path} init && git -C {remote_path} config user.email homeboy-snapshot@localhost && git -C {remote_path} config user.name 'Homeboy Snapshot' && git -C {remote_path} add -A -- . ':(exclude).homeboy/runner-workspace.json' ':(exclude).homeboy/lab-at-files/**' && env GIT_AUTHOR_NAME='Homeboy Snapshot' GIT_AUTHOR_EMAIL=homeboy-snapshot@localhost GIT_COMMITTER_NAME='Homeboy Snapshot' GIT_COMMITTER_EMAIL=homeboy-snapshot@localhost GIT_AUTHOR_DATE='1970-01-01T00:00:00Z' GIT_COMMITTER_DATE='1970-01-01T00:00:00Z' git -C {remote_path} commit --allow-empty -m {message} --no-gpg-sign && git -C {remote_path} notes --ref=homeboy-snapshot add -m {note} HEAD{set_remote}",
        message = shell::quote_arg(&format!("Homeboy snapshot {snapshot}")),
        note = shell::quote_arg(&format!("snapshot_identity={snapshot}\nsource_head={source_head}\nsource_dirty={source_dirty}")),
    )
}

pub(crate) fn copy_snapshot_to_directory(
    local_path: &Path,
    destination: &Path,
    excludes: &[String],
) -> Result<()> {
    materialize_snapshot_piped(
        local_path,
        &format!(
            "sh -c {}",
            shell::quote_arg(&snapshot_install_command(
                &destination.display().to_string()
            ))
        ),
        excludes,
        "prepare local workspace snapshot",
    )
}

pub(crate) fn ensure_no_runner_workspace_metadata_collision(local_path: &Path) -> Result<()> {
    for reserved in RESERVED_RUNNER_WORKSPACE_PATHS.iter().copied() {
        let reserved_path = local_path.join(reserved);
        match fs::symlink_metadata(&reserved_path) {
            Ok(_) => {
                return Err(Error::validation_invalid_argument(
                    "workspace",
                    format!(
                        "source workspace contains the reserved runner path `{reserved}`; remove or rename it before syncing"
                    ),
                    Some(reserved_path.display().to_string()),
                    Some(vec![
                        "Remove or rename the source path before syncing; Homeboy owns this path on materialized runner workspaces.".to_string(),
                    ]),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("inspect reserved runner workspace path".to_string()),
                ));
            }
        }
    }
    Ok(())
}

fn materialize_snapshot_piped(
    local_path: &Path,
    target_command: &str,
    excludes: &[String],
    action: &str,
) -> Result<()> {
    let command = snapshot_archive_command(local_path, target_command, excludes);
    run_shell_command(&command, action)
}

pub(super) fn snapshot_archive_command(
    local_path: &Path,
    target_command: &str,
    excludes: &[String],
) -> String {
    // A Git-backed snapshot overlays this archive onto an exact checkout. Keep
    // links whose resolved targets stay in the source tree so tracked Git links
    // retain their identity. Materialize only external dependency links: their
    // targets are unavailable at the runner, but plans may traverse them (#3913).
    let root_anchored = excludes
        .iter()
        .filter(|pattern| pattern.starts_with("./"))
        .collect::<Vec<_>>();
    let excludes = excludes
        .iter()
        .filter(|pattern| !pattern.starts_with("./"))
        .cloned()
        .collect::<Vec<_>>();
    let archive = format!(
        "COPYFILE_DISABLE=1 tar --no-xattrs -C \"$stage/source\" {exclude} -cf -",
        exclude = tar_exclude_args(&excludes),
    );
    let source_archive = format!(
        "COPYFILE_DISABLE=1 tar --no-xattrs -C {src} {exclude} -cf -",
        src = shell::quote_arg(&local_path.display().to_string()),
        exclude = tar_exclude_args(&excludes),
    );
    let prepare = |source_stream: &str| {
        format!(
        "root=$(pwd -P) && stage=$(mktemp -d \"${{TMPDIR:-/tmp}}/homeboy-snapshot.XXXXXX\") && trap 'rm -rf \"$stage\"' EXIT && mkdir -p \"$stage/source\" && {source_stream} | tar --no-xattrs -C \"$stage/source\" -xf - && export root stage && find \"$stage/source\" -type l -exec sh -c {resolve} sh {{}} \\;",
        resolve = shell::quote_arg(&format!("stage_link=$1; relative=${{stage_link#\"$stage/source\"/}}; original=\"$root/$relative\"; target=$(realpath \"$original\") || exit; case \"$target\" in \"$root\"|\"$root\"/*) ;; *) rm -f \"$stage_link\" && mkdir -p \"$(dirname \"$stage_link\")\" && COPYFILE_DISABLE=1 tar --no-xattrs -h -C \"$root\" {} -cf - \"$relative\" | tar --no-xattrs -C \"$stage/source\" -xf - ;; esac", tar_exclude_args(&excludes))),
    )
    };

    if root_anchored.is_empty() {
        let prepare = prepare(&format!("{source_archive} ."));
        return format!(
            "(cd {src} && {prepare} && {archive} .) | {target_command}",
            src = shell::quote_arg(&local_path.display().to_string())
        );
    }

    let root_filter = root_anchored
        .iter()
        .map(|pattern| format!("! -path {}", shell::quote_arg(pattern)))
        .collect::<Vec<_>>()
        .join(" ");
    let root_input = format!("find . -mindepth 1 -maxdepth 1 {root_filter} -print0",);
    let prepare = prepare(&format!("({root_input}) | {source_archive} --null -T -"));
    format!(
        "(cd {src} && {prepare} && ({root_input}) | {archive} --null -T -) | {target_command}",
        src = shell::quote_arg(&local_path.display().to_string()),
    )
}

pub(crate) fn effective_snapshot_excludes(
    excludes: Vec<String>,
    includes: &[String],
) -> Vec<String> {
    if includes.is_empty() {
        return excludes;
    }

    excludes
        .into_iter()
        .filter(|exclude| !includes_override_exclude(includes, exclude))
        .collect()
}

fn includes_override_exclude(includes: &[String], exclude: &str) -> bool {
    let excluded_name = exclude
        .trim_start_matches("./")
        .trim_end_matches("/**")
        .trim_end_matches('/');
    if excluded_name.is_empty() || excluded_name.contains('*') || excluded_name.contains('/') {
        return false;
    }

    includes.iter().any(|include| {
        include
            .trim_start_matches("./")
            .split('/')
            .any(|segment| segment == excluded_name)
    })
}

pub(super) fn snapshot_install_command(remote_path: &str) -> String {
    WorkspaceMaterializer::new(remote_path)
        .capture_owner()
        .op(WorkspaceMaterializationOperation::EnsureParent)
        .op(WorkspaceMaterializationOperation::CleanupOnExit(vec![
            "\"$tmp\"".to_string(),
        ]))
        .op(WorkspaceMaterializationOperation::RecreateTempDir)
        .op(WorkspaceMaterializationOperation::ExtractTarStdinToTemp)
        .op(WorkspaceMaterializationOperation::AtomicReplaceTemp)
        .restore_owner()
        .command()
}

fn snapshot_overlay_install_command(remote_path: &str) -> String {
    let remote_path = shell::quote_arg(remote_path);
    format!(
        "dest={remote_path}; tmp=\"${{dest}}.overlay.$$\"; rm -rf \"$tmp\" && mkdir -p \"$tmp\" && trap 'rm -rf \"$tmp\"' EXIT && tar -C \"$tmp\" -xf - && find \"$dest\" -mindepth 1 -maxdepth 1 ! -name .git -exec rm -rf {{}} + && cp -a \"$tmp\"/. \"$dest\"/"
    )
}
