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
    run_shell_capture, run_shell_command, ssh_args, ssh_client_for_runner, tar_exclude_args,
};

const RUNNER_WORKSPACE_METADATA_FILE: &str = ".homeboy/runner-workspace.json";
pub(crate) const WORKSPACE_CONTENT_PERMISSION_PORTABLE: &str = "portable-content-only";
pub(crate) const WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE: &str = "unix-executable";
pub(crate) const WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE: &str = "unix-owner-executable";
pub(crate) const WORKSPACE_CONTENT_DIAGNOSTIC_PATH_LIMIT: usize = 192;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct WorkspaceContentManifest {
    pub entry_count: usize,
    pub entries: Vec<WorkspaceContentManifestEntry>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct WorkspaceContentManifestEntry {
    pub path: String,
    pub kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_executable: Option<bool>,
}

#[cfg(unix)]
pub(crate) const WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY: &str =
    WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE;
#[cfg(not(unix))]
pub(crate) const WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY: &str =
    WORKSPACE_CONTENT_PERMISSION_PORTABLE;

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

pub(crate) fn workspace_content_hash_algorithm(policy: &str) -> Option<String> {
    match policy {
        WORKSPACE_CONTENT_PERMISSION_PORTABLE => {
            Some("homeboy-workspace-content-v2+portable-content-only".to_string())
        }
        WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE if cfg!(unix) => {
            Some("homeboy-workspace-content-v2+unix-executable".to_string())
        }
        WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE if cfg!(unix) => {
            Some("homeboy-workspace-content-v3+unix-owner-executable".to_string())
        }
        _ => None,
    }
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
            || relative == RUNNER_WORKSPACE_METADATA_FILE
        {
            continue;
        }
        let link_metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        let resolved = if link_metadata.file_type().is_symlink() {
            entry_path.canonicalize().map_err(|err| {
                Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused an unresolved symlink",
                    Some(err.to_string()),
                    None,
                )
            })?
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
            || relative == RUNNER_WORKSPACE_METADATA_FILE
        {
            continue;
        }
        let link_metadata = fs::symlink_metadata(&entry_path).map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        let resolved = if link_metadata.file_type().is_symlink() {
            entry_path.canonicalize().map_err(|err| {
                Error::validation_invalid_argument(
                    "workspace",
                    "workspace content hash refused an unresolved symlink",
                    Some(err.to_string()),
                    None,
                )
            })?
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
    delta: &SnapshotManifestDelta,
) -> Result<SnapshotTransferStats> {
    let temporary = format!("{}.tmp-{}", remote_path, uuid::Uuid::new_v4());
    let prepare = incremental_prepare_command(remote_path, &temporary, seed_path, delta);
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
    format!(
        "{owner_capture} ; mkdir -p {parent} && rm -rf {temporary} && mkdir -p {temporary} && {seed} {removals}",
        owner_capture = owner_capture_shell(&parent),
        parent = shell::quote_arg(&parent),
        temporary = shell::quote_arg(&temporary),
        seed = seed_snapshot_command(seed_path, temporary),
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
        destination = destination,
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
    let source_dirty = !git_output(local_path, &["status", "--porcelain=v1"])?
        .trim()
        .is_empty();
    initialize_synthetic_git_checkout(runner, local_path, remote_path, snapshot, source_dirty)
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
        "git -C {remote_path} init && git -C {remote_path} config user.email homeboy-snapshot@localhost && git -C {remote_path} config user.name 'Homeboy Snapshot' && git -C {remote_path} add -A && env GIT_AUTHOR_NAME='Homeboy Snapshot' GIT_AUTHOR_EMAIL=homeboy-snapshot@localhost GIT_COMMITTER_NAME='Homeboy Snapshot' GIT_COMMITTER_EMAIL=homeboy-snapshot@localhost GIT_AUTHOR_DATE='1970-01-01T00:00:00Z' GIT_COMMITTER_DATE='1970-01-01T00:00:00Z' git -C {remote_path} commit --allow-empty -m {message} --no-gpg-sign && git -C {remote_path} notes --ref=homeboy-snapshot add -m {note} HEAD{set_remote}",
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
    let metadata_path = local_path.join(RUNNER_WORKSPACE_METADATA_FILE);
    match fs::symlink_metadata(&metadata_path) {
        Ok(_) => Err(Error::validation_invalid_argument(
            "workspace",
            "source workspace contains the reserved runner metadata path `.homeboy/runner-workspace.json`; remove or rename it before syncing",
            Some(metadata_path.display().to_string()),
            Some(vec![
                "Remove or rename the source file before syncing; Homeboy writes this path only after runner materialization.".to_string(),
            ]),
        )),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some("inspect reserved runner metadata path".to_string()),
        )),
    }
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
    // `-h`/`--dereference` follows symlinks and archives their target contents
    // instead of recording the link itself. Controller-native workspaces often
    // wire local dependencies into the tree via symlinks (e.g. a `.ci/<dep>`
    // entry pointing at a sibling checkout/worktree). Archiving those as plain
    // links produces a runner snapshot whose links dangle, so embedded plan
    // paths that traverse a symlinked dependency resolve to missing files on the
    // runner. Dereferencing materializes the real dependency contents into the
    // snapshot so offloaded plans find them at the remapped path (#3913).
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
        "COPYFILE_DISABLE=1 tar --no-xattrs -h -C {src} {exclude} -cf -",
        src = shell::quote_arg(&local_path.display().to_string()),
        exclude = tar_exclude_args(&excludes),
    );

    if root_anchored.is_empty() {
        return format!("{archive} . | {target_command}");
    }

    let root_filter = root_anchored
        .iter()
        .map(|pattern| format!("! -path {}", shell::quote_arg(pattern)))
        .collect::<Vec<_>>()
        .join(" ");
    format!(
        "(cd {src} && find . -mindepth 1 -maxdepth 1 {root_filter} -print0) | {archive} --null -T - | {target_command}",
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
