use homeboy_engine_primitives::content_hash;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use glob_match::glob_match;
use sha2::{Digest, Sha256};

use homeboy_core::engine::shell;
use homeboy_core::error::{Error, Result};

use super::super::{Runner, RunnerKind};
use super::control::WorkspaceControl;
use super::materializer::{WorkspaceMaterializationOperation, WorkspaceMaterializer};
use super::types::{ByteFileCounts, SnapshotStats, SnapshotTransferStats};
use super::util::{
    git_output, run_shell_capture, run_shell_command, run_shell_command_before,
    shell_command_for_runner, ssh_args, ssh_client_for_runner,
};
use homeboy_lab_contract::lab::handoff::{
    runner_workspace_metadata_path, runner_workspace_state_dir,
};

#[cfg(test)]
mod test_snapshot_directory_discovery_hook {
    use std::path::{Path, PathBuf};
    use std::sync::{Mutex, OnceLock};

    type Hooks = std::collections::HashMap<PathBuf, Box<dyn FnOnce() + Send>>;

    static HOOK: OnceLock<Mutex<Hooks>> = OnceLock::new();

    pub(crate) struct Guard(PathBuf);

    pub(crate) fn install(path: PathBuf, action: impl FnOnce() + Send + 'static) -> Guard {
        let mut hook = HOOK
            .get_or_init(|| Mutex::new(Hooks::new()))
            .lock()
            .expect("snapshot discovery hook lock");
        assert!(
            !hook.contains_key(&path),
            "snapshot discovery hook is already installed for this path"
        );
        hook.insert(path.clone(), Box::new(action));
        Guard(path)
    }

    pub(crate) fn run(path: &Path) {
        let action = HOOK
            .get_or_init(|| Mutex::new(Hooks::new()))
            .lock()
            .expect("snapshot discovery hook lock")
            .remove(path);
        if let Some(action) = action {
            action();
        }
    }

    #[test]
    fn independent_discovery_hooks_do_not_replace_or_clear_each_other() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::sync::{Arc, Barrier};

        let roots = tempfile::tempdir().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let installed = Arc::new(Barrier::new(2));
        std::thread::scope(|scope| {
            for index in 0..2 {
                let path = roots.path().join(index.to_string());
                let calls = calls.clone();
                let installed = installed.clone();
                scope.spawn(move || {
                    let _guard = install(path.clone(), move || {
                        calls.fetch_add(1, Ordering::SeqCst);
                    });
                    installed.wait();
                    run(&path);
                });
            }
        });
        assert_eq!(calls.load(Ordering::SeqCst), 2);
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            HOOK.get_or_init(|| Mutex::new(Hooks::new()))
                .lock()
                .expect("snapshot discovery hook lock")
                .remove(&self.0);
        }
    }
}

#[cfg(test)]
pub(crate) fn register_after_snapshot_directory_discovery_hook(
    path: std::path::PathBuf,
    action: impl FnOnce() + Send + 'static,
) -> test_snapshot_directory_discovery_hook::Guard {
    test_snapshot_directory_discovery_hook::install(path, action)
}

// The workspace-content *identity spec* (permission-policy names, the default
// policy, the policy -> algorithm-marker mapping, and the content manifest
// types) lives in `homeboy-runner-contract`'s `workspace_content_identity` so the
// controller-declare and runner-verify paths derive it from one place. The
// filesystem traversal that *applies* the spec stays here. Re-exported so
// existing `crate::workspace::snapshot::*` / `crate::*` paths keep resolving.
pub use homeboy_runner_contract::workspace_content_identity::{
    workspace_content_hash_algorithm, WorkspaceContentManifest, WorkspaceContentManifestEntry,
    WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY, WORKSPACE_CONTENT_PERMISSION_PORTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_EXECUTABLE,
    WORKSPACE_CONTENT_PERMISSION_UNIX_OWNER_EXECUTABLE,
};

pub(crate) fn snapshot_identity(
    local_path: &Path,
    excludes: &[String],
    includes: &[String],
) -> Result<String> {
    snapshot_identity_controlled(local_path, excludes, includes, &WorkspaceControl::default())
}

pub(super) fn snapshot_identity_controlled(
    local_path: &Path,
    excludes: &[String],
    includes: &[String],
    control: &WorkspaceControl,
) -> Result<String> {
    Ok(snapshot_content_controlled(local_path, excludes, includes, control)?.0)
}

/// The portable content digest is the sole snapshot tree identity.
pub(super) fn snapshot_content_controlled(
    local_path: &Path,
    excludes: &[String],
    includes: &[String],
    control: &WorkspaceControl,
) -> Result<(String, SnapshotStats, WorkspaceContentManifest, String)> {
    homeboy_core::observation::phase_timing::time(
        "stage.lab.source_materialization.content_manifest_hash",
        || {
            control.checkpoint()?;
            let repository_integrity_evidence =
                homeboy_core::repository_integrity::collect_operator_policy_evidence(local_path)?;
            homeboy_core::repository_integrity::verify_tracked_symlink_portability(
                local_path,
                "HEAD",
                repository_integrity_evidence.as_ref(),
            )?;
            let effective_excludes = effective_snapshot_excludes(excludes.to_vec(), includes);
            let walk = workspace_content_walk_controlled(
                local_path,
                &effective_excludes,
                WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
                control,
            )?;
            let stats = walk.stats;
            Ok((
                walk.content_identity.clone(),
                stats,
                walk.manifest,
                walk.content_identity,
            ))
        },
    )
}

/// Return Git exclusion pathspecs with the same root and glob semantics as the
/// snapshot traversal. A malformed Git pathspec is deliberately left to Git to
/// reject so callers that validate provenance fail closed.
pub(crate) fn snapshot_git_exclude_pathspecs(excludes: &[String]) -> Vec<String> {
    homeboy_core::source_snapshot::git_exclude_pathspecs(excludes)
}

/// Stable v2 digest of the files a snapshot materializes, portable across
/// controller and runner paths and also used as `snapshot_identity`.
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
    Ok(workspace_content_manifest_and_hash_for_policy(path, excludes, policy)?.1)
}

/// Collect the content manifest and digest in one traversal. Snapshot staging
/// must never compare an inventory from one filesystem instant with a digest
/// from another while runner-owned cleanup is active.
pub(crate) fn workspace_content_manifest_and_hash_for_policy(
    path: &Path,
    excludes: &[String],
    policy: &str,
) -> Result<(WorkspaceContentManifest, String)> {
    workspace_content_manifest_and_hash_controlled(
        path,
        excludes,
        policy,
        &WorkspaceControl::default(),
    )
}

pub(super) fn workspace_content_manifest_and_hash_controlled(
    path: &Path,
    excludes: &[String],
    policy: &str,
    control: &WorkspaceControl,
) -> Result<(WorkspaceContentManifest, String)> {
    homeboy_core::observation::phase_timing::time(
        "stage.lab.source_materialization.content_manifest_hash",
        || workspace_content_manifest_and_hash_unspanned(path, excludes, policy, control),
    )
}

pub(super) fn workspace_content_manifest_and_hash_unspanned(
    path: &Path,
    excludes: &[String],
    policy: &str,
    control: &WorkspaceControl,
) -> Result<(WorkspaceContentManifest, String)> {
    let walk = workspace_content_walk_controlled(path, excludes, policy, control)?;
    Ok((walk.manifest, walk.content_identity))
}

struct WorkspaceContentWalk {
    stats: SnapshotStats,
    manifest: WorkspaceContentManifest,
    content_identity: String,
}

fn workspace_content_walk_controlled(
    path: &Path,
    excludes: &[String],
    policy: &str,
    control: &WorkspaceControl,
) -> Result<WorkspaceContentWalk> {
    control.checkpoint()?;
    let (algorithm, executable_capability) =
        workspace_content_hash_contract(policy).ok_or_else(|| {
            Error::validation_invalid_argument(
                "permission_policy",
                "workspace content hash policy is unsupported on this platform",
                Some(policy.to_string()),
                None,
            )
        })?;
    let mut hasher = content_identity_hasher(&algorithm);
    let root = content_hash_root(path)?;
    let mut manifest = WorkspaceContentManifest {
        entry_count: 0,
        entries: Vec::new(),
    };
    let mut stats = SnapshotStats { files: 0, bytes: 0 };
    let membership = SnapshotMembership::new(&root, excludes, &[])?;
    ContentHashTraversal {
        membership: &membership,
        hasher: &mut hasher,
        stats: &mut stats,
        executable_capability,
        manifest: Some(&mut manifest),
        control,
    }
    .collect(&root, Path::new(""), &mut vec![root.clone()])?;
    for entry in &mut manifest.entries {
        let source = root.join(&entry.path);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            entry.mode = Some(
                fs::metadata(&source)
                    .map(|m| m.permissions().mode() & 0o777)
                    .unwrap_or(0o644),
            );
        }
        if fs::symlink_metadata(&source).is_ok_and(|m| m.file_type().is_symlink()) {
            let target =
                fs::read_link(&source).map_err(|e| Error::internal_io(e.to_string(), None))?;
            if entry.kind == "symlink"
                || source.canonicalize().is_ok_and(|p| {
                    p.strip_prefix(&root)
                        .is_ok_and(|rel| membership.includes(rel))
                })
            {
                entry.symlink_target = Some(target.to_string_lossy().into_owned());
            }
        }
    }
    Ok(WorkspaceContentWalk {
        stats,
        manifest,
        content_identity: format!("sha256:{:x}", hasher.finalize()),
    })
}

/// A deterministic, content-free inventory of every path a snapshot materializes.
/// It is persisted with the immutable snapshot and used to derive explicit
/// incremental transport and deletion sets.
pub(crate) fn workspace_content_manifest_for_policy(
    path: &Path,
    excludes: &[String],
    policy: &str,
) -> Result<WorkspaceContentManifest> {
    Ok(workspace_content_manifest_and_hash_for_policy(path, excludes, policy)?.0)
}

fn content_hash_root(path: &Path) -> Result<std::path::PathBuf> {
    path.canonicalize().map_err(|err| {
        Error::internal_io(err.to_string(), Some("canonicalize workspace".to_string()))
    })
}

// The content identity encoding. The walk and Git respelling both hash
// through these encoders so the two can never disagree on the format.
fn content_identity_hasher(algorithm: &str) -> Sha256 {
    let mut hasher = Sha256::new();
    hasher.update(algorithm.as_bytes());
    hasher.update(b"\0");
    hasher
}

fn hash_directory_entry(hasher: &mut Sha256, relative: &str) {
    hasher.update(relative.as_bytes());
    hasher.update(b"\0dir\0");
}

fn hash_symlink_entry(hasher: &mut Sha256, relative: &str, target: &str) {
    hasher.update(relative.as_bytes());
    hasher.update(b"\0symlink\0");
    hasher.update((target.len() as u64).to_le_bytes());
    hasher.update(target.as_bytes());
}

/// Hash a file entry's header; the caller streams its content afterwards.
fn hash_file_entry_header(
    hasher: &mut Sha256,
    relative: &str,
    executable: Option<bool>,
    bytes: u64,
) {
    hasher.update(relative.as_bytes());
    hasher.update(b"\0file\0");
    if let Some(executable) = executable {
        hasher.update([executable as u8]);
    }
    hasher.update(bytes.to_le_bytes());
}

struct ContentHashTraversal<'a> {
    membership: &'a SnapshotMembership<'a>,
    hasher: &'a mut Sha256,
    stats: &'a mut SnapshotStats,
    executable_capability: ExecutableCapability,
    manifest: Option<&'a mut WorkspaceContentManifest>,
    control: &'a WorkspaceControl,
}

impl ContentHashTraversal<'_> {
    fn collect(
        &mut self,
        path: &Path,
        logical: &Path,
        ancestors: &mut Vec<std::path::PathBuf>,
    ) -> Result<()> {
        self.control.checkpoint()?;
        let mut children = match fs::read_dir(path) {
            Ok(children) => children,
            Err(err) => {
                return Err(Error::internal_io(
                    err.to_string(),
                    Some("read sync directory".to_string()),
                ));
            }
        }
        .map(|entry| {
            self.control.checkpoint()?;
            entry.map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some("read sync directory entry".to_string()),
                )
            })
        })
        .collect::<Result<Vec<_>>>()?;
        children.sort_by_key(|entry| entry.path());
        for entry in children {
            self.control.checkpoint()?;
            let entry_path = entry.path();
            let relative_path = logical.join(entry.file_name());
            let relative = relative_path.to_string_lossy().replace('\\', "/");
            if !self.membership.includes(&relative_path) {
                continue;
            }
            #[cfg(test)]
            test_snapshot_directory_discovery_hook::run(&entry_path);
            let link_metadata = match fs::symlink_metadata(&entry_path) {
                Ok(metadata) => metadata,
                Err(err) => {
                    return Err(Error::internal_io(
                        err.to_string(),
                        Some("read sync file metadata".to_string()),
                    ));
                }
            };
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
                        hash_symlink_entry(self.hasher, &relative, &target);
                        record_workspace_content_manifest_entry(
                            &mut self.manifest,
                            relative,
                            "symlink",
                            Some(format!(
                                "sha256:{}",
                                content_hash::sha256_hex(target.as_bytes())
                            )),
                            Some(target.len() as u64),
                            None,
                        );
                        continue;
                    }
                }
            } else {
                entry_path.clone()
            };
            let metadata = match fs::metadata(&resolved) {
                Ok(metadata) => metadata,
                Err(err) => {
                    return Err(Error::internal_io(
                        err.to_string(),
                        Some("read sync file metadata".to_string()),
                    ));
                }
            };
            if metadata.is_dir() {
                let canonical = match resolved.canonicalize() {
                    Ok(canonical) => canonical,
                    Err(err) => return Err(Error::internal_io(err.to_string(), None)),
                };
                if ancestors.contains(&canonical) {
                    return Err(Error::validation_invalid_argument(
                        "workspace",
                        "workspace content hash refused a symlink cycle",
                        Some(entry_path.display().to_string()),
                        None,
                    ));
                }
                hash_directory_entry(self.hasher, &relative);
                record_workspace_content_manifest_entry(
                    &mut self.manifest,
                    relative.clone(),
                    "directory",
                    None,
                    None,
                    None,
                );
                ancestors.push(canonical);
                self.collect(&resolved, &relative_path, ancestors)?;
                ancestors.pop();
            } else if metadata.is_file() {
                let hash_executable = self.executable_capability.value(&metadata);
                hash_file_entry_header(self.hasher, &relative, hash_executable, metadata.len());
                self.stats.files += 1;
                self.stats.bytes = self.stats.bytes.saturating_add(metadata.len());
                let mut file_hash = Sha256::new();
                let bytes = read_file_chunks(&resolved, self.control, |chunk| {
                    self.hasher.update(chunk);
                    file_hash.update(chunk);
                    Ok(())
                })?;
                if bytes != metadata.len() {
                    return Err(snapshot_construction_failure(
                        "content",
                        &resolved,
                        None,
                        "source file size changed during hashing",
                    ));
                }
                record_workspace_content_manifest_entry(
                    &mut self.manifest,
                    relative,
                    "file",
                    Some(format!("sha256:{:x}", file_hash.finalize())),
                    Some(bytes),
                    self.executable_capability.owner_value(&metadata),
                );
            }
        }
        Ok(())
    }
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
        mode: None,
        symlink_target: None,
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

pub(crate) fn local_snapshot_stats(
    path: &Path,
    excludes: &[String],
    includes: &[String],
) -> Result<SnapshotStats> {
    local_snapshot_stats_controlled(path, excludes, includes, &WorkspaceControl::default())
}

pub(super) fn local_snapshot_stats_controlled(
    path: &Path,
    excludes: &[String],
    includes: &[String],
    control: &WorkspaceControl,
) -> Result<SnapshotStats> {
    let mut stats = SnapshotStats { files: 0, bytes: 0 };
    let membership = SnapshotMembership::new(path, excludes, includes)?;
    collect_stats(path, path, &membership, &mut stats, control)?;
    Ok(stats)
}

fn collect_stats(
    root: &Path,
    path: &Path,
    membership: &SnapshotMembership<'_>,
    stats: &mut SnapshotStats,
    control: &WorkspaceControl,
) -> Result<()> {
    control.checkpoint()?;
    for entry in fs::read_dir(path).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read sync directory".to_string()))
    })? {
        control.checkpoint()?;
        let entry = entry.map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("read sync directory entry".to_string()),
            )
        })?;
        let entry_path = entry.path();
        if !membership.includes(entry_path.strip_prefix(root).unwrap_or(&entry_path)) {
            continue;
        }
        let metadata = entry.metadata().map_err(|err| {
            Error::internal_io(err.to_string(), Some("read sync file metadata".to_string()))
        })?;
        if metadata.is_dir() {
            collect_stats(root, &entry_path, membership, stats, control)?;
        } else if metadata.is_file() {
            stats.files += 1;
            stats.bytes = stats.bytes.saturating_add(metadata.len());
        }
    }
    Ok(())
}

/// Shared membership for traversal and stats. The resulting manifest is also
/// the membership list for full archives and incremental deltas.
struct SnapshotMembership<'a> {
    excludes: &'a [String],
    includes: &'a [String],
    tracked: Option<BTreeSet<PathBuf>>,
}

impl<'a> SnapshotMembership<'a> {
    fn new(root: &Path, excludes: &'a [String], includes: &'a [String]) -> Result<Self> {
        let output = std::process::Command::new("git")
            .arg("-C")
            .arg(root)
            .args(["ls-files", "-z", "--cached"])
            .output()
            .map_err(|error| Error::internal_io(error.to_string(), None))?;
        let tracked = output.status.success().then(|| {
            let mut paths = BTreeSet::new();
            for bytes in output
                .stdout
                .split(|byte| *byte == 0)
                .filter(|p| !p.is_empty())
            {
                #[cfg(unix)]
                let path = {
                    use std::os::unix::ffi::OsStrExt;
                    PathBuf::from(std::ffi::OsStr::from_bytes(bytes))
                };
                #[cfg(not(unix))]
                let path = PathBuf::from(String::from_utf8_lossy(bytes).into_owned());
                for ancestor in path.ancestors().filter(|p| !p.as_os_str().is_empty()) {
                    paths.insert(ancestor.to_path_buf());
                }
            }
            paths
        });
        Ok(Self {
            excludes,
            includes,
            tracked,
        })
    }

    fn includes(&self, relative: &Path) -> bool {
        if relative.starts_with(".git") {
            return false;
        }
        if self
            .tracked
            .as_ref()
            .is_some_and(|paths| paths.contains(relative))
        {
            return true;
        }
        // Opening an excluded directory for tracked content must not admit its
        // untracked descendants.
        !relative
            .ancestors()
            .filter(|p| !p.as_os_str().is_empty())
            .any(|path| is_excluded(Path::new(""), path, self.excludes, self.includes))
    }
}

pub(crate) fn is_excluded(
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
        // `path/**` denotes the complete directory subtree. Match its root too
        // so traversal and tar do not retain an empty directory after a late
        // context artifact appears beneath it.
        let directory_pattern = pattern.trim_end_matches("/**").trim_end_matches('/');
        pattern == rel
            || directory_pattern == rel
            // A Git-style `**/directory/` rule also includes that directory at
            // the workspace root. Tar already applies this interpretation.
            || directory_pattern
                .strip_prefix("**/")
                .is_some_and(|unrooted| unrooted == rel)
            || glob_match(pattern, rel)
            || glob_match(directory_pattern, rel)
            || (!root_anchored && (pattern == name || glob_match(pattern, name)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_identity_includes_tracked_context_despite_excludes() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        std::fs::write(workspace.path().join("AGENTS.md"), "context\n").expect("context file");
        git(workspace.path(), &["init", "--quiet", "-b", "main"]);
        git(workspace.path(), &["add", "--all"]);
        git(
            workspace.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        );
        let excludes = vec!["AGENTS.md".to_string()];
        let baseline =
            snapshot_identity(workspace.path(), &excludes, &[]).expect("baseline identity");

        std::fs::write(workspace.path().join("AGENTS.md"), "changed context\n")
            .expect("change context");
        assert_ne!(
            snapshot_identity(workspace.path(), &excludes, &[]).expect("tracked identity"),
            baseline,
            "tracked context must alter the snapshot identity"
        );

        std::fs::write(workspace.path().join("file.txt"), "changed source\n")
            .expect("change source");
        assert_ne!(
            snapshot_identity(workspace.path(), &excludes, &[]).expect("changed identity"),
            baseline,
            "non-excluded source drift must alter the snapshot identity"
        );
    }

    #[test]
    fn snapshot_copy_preserves_tracked_excluded_files_only() {
        let source = tempfile::tempdir().unwrap();
        let destination = tempfile::tempdir().unwrap();
        fs::create_dir(source.path().join("sub")).unwrap();
        fs::create_dir(source.path().join("vendor")).unwrap();
        fs::write(source.path().join(".DS_Store"), b"tracked metadata").unwrap();
        fs::write(source.path().join("sub/.env.example"), b"EXAMPLE=public\n").unwrap();
        fs::write(source.path().join("vendor/tracked.php"), b"<?php tracked\n").unwrap();
        git(source.path(), &["init", "--quiet"]);
        git(source.path(), &["add", "."]);
        git(
            source.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.com",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        );
        // Materialize working-tree bytes, not the committed blob.
        fs::write(source.path().join(".DS_Store"), b"modified metadata").unwrap();
        let secrets = [
            ".env",
            ".env.local",
            "sub/.env.production",
            "wp-config.php",
            "sub/wp-config.php",
        ];
        for secret in secrets {
            fs::write(source.path().join(secret), b"SECRET=private\n").unwrap();
        }
        fs::write(source.path().join("vendor/x.php"), b"untracked dependency").unwrap();
        let mut excludes = homeboy_runner_contract::source_snapshot::default_sync_excludes();
        excludes.push("vendor/".to_string());
        copy_snapshot_to_directory(source.path(), destination.path(), &excludes).unwrap();
        assert_eq!(
            fs::read(destination.path().join(".DS_Store")).unwrap(),
            b"modified metadata"
        );
        assert_eq!(
            fs::read(destination.path().join("sub/.env.example")).unwrap(),
            b"EXAMPLE=public\n"
        );
        assert_eq!(
            fs::read(destination.path().join("vendor/tracked.php")).unwrap(),
            b"<?php tracked\n"
        );
        for secret in secrets {
            assert!(
                !destination.path().join(secret).exists(),
                "bundled untracked {secret}"
            );
        }
        assert!(!destination.path().join("vendor/x.php").exists());
    }

    use homeboy_core::test_support::run_git_command as git;
}

pub(crate) fn materialize_snapshot(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
) -> Result<()> {
    materialize_snapshot_with_scratch(runner, local_path, remote_path, excludes, None)
}

/// Materialize with an explicitly admitted controller scratch filesystem.  The
/// archive preparation happens on the controller, so this is intentionally an
/// invocation-scoped shell environment rather than a process-global `TMPDIR`.
pub(crate) fn materialize_snapshot_with_scratch(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    scratch: Option<&Path>,
) -> Result<()> {
    materialize_snapshot_with_scratch_before(
        runner,
        local_path,
        remote_path,
        excludes,
        scratch,
        None,
    )
}

pub(crate) fn materialize_snapshot_with_scratch_before(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    scratch: Option<&Path>,
    deadline: Option<Instant>,
) -> Result<()> {
    materialize_snapshot_with_scratch_controlled(
        runner,
        local_path,
        remote_path,
        excludes,
        scratch,
        &WorkspaceControl::before(deadline),
    )
}

pub(crate) fn materialize_snapshot_with_scratch_controlled(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    scratch: Option<&Path>,
    control: &WorkspaceControl,
) -> Result<()> {
    match runner.kind {
        RunnerKind::Local => stream_snapshot_controlled(
            local_path,
            &format!(
                "sh -c {}",
                shell::quote_arg(&snapshot_install_command(remote_path))
            ),
            excludes,
            "materialize local workspace snapshot",
            scratch,
            control,
        ),
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                stream_snapshot_controlled(
                    local_path,
                    &format!(
                        "sh -c {}",
                        shell::quote_arg(&snapshot_install_command(remote_path))
                    ),
                    excludes,
                    "materialize local workspace snapshot",
                    scratch,
                    control,
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
                stream_snapshot_controlled(
                    local_path,
                    &target,
                    excludes,
                    "materialize SSH workspace snapshot",
                    scratch,
                    control,
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
        .filter(|(path, entry)| seed_entries.get(*path) != Some(*entry))
        .map(|(path, _)| path.clone())
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
            missing_blobs: transferred.files as u64,
            missing_bytes: transferred.bytes,
            total_blobs: final_size.files as u64,
            total_bytes: final_size.bytes,
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
    // Hold one directory across all phases of the runner-neutral CAS. Use
    // create_dir locally: some mkdir implementations report success to both
    // racing creators (uutils 0.8.0), violating the exclusivity contract.
    let local_lock = match runner.kind {
        RunnerKind::Local => true,
        RunnerKind::Ssh => ssh_client_for_runner(runner)?.1.is_local,
    };
    let acquire_lock = format!("mkdir {}", shell::quote_arg(&lock));
    let release_lock = format!("rmdir {}", shell::quote_arg(&lock));
    if local_lock {
        fs::create_dir(&lock).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("acquire prepared workspace update lock".into()),
            )
        })?;
    } else {
        run_shell_command(
            &shell_command_for_runner(runner, &acquire_lock)?,
            "acquire prepared workspace update lock",
        )?;
    }
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
    // Update staging lives in the Homeboy-owned state sibling, never inside
    // the workspace tree that is promoted into place.
    let state_dir = runner_workspace_state_dir(remote_path);
    let update_id = uuid::Uuid::new_v4();
    let removal_path = format!("{state_dir}/update-removals-{update_id}");
    let live_metadata_path = runner_workspace_metadata_path(remote_path);
    let metadata_path = format!("{live_metadata_path}.update-{update_id}");
    let prepare = format!(
        "rm -rf {temporary} {backup} && mkdir -p {temporary} {state_dir} && {seed}",
        temporary = shell::quote_arg(&temporary),
        backup = shell::quote_arg(&backup),
        state_dir = shell::quote_arg(&state_dir),
        seed = seed_snapshot_command(remote_path, &temporary),
    );
    let finalize = format!(
        "tr -d '[:space:]' < {live_metadata} | grep -qF -- {lease} && mv {remote} {backup} && if mv {temporary} {remote}; then rm -rf {backup} || true; mv -f {staged_metadata} {live_metadata}; else if mv {backup} {remote}; then exit 1; else printf '%s\\n' 'prepared workspace promotion failed; original remains at {backup}' >&2; exit 2; fi; fi",
        lease = shell::quote_arg(&format!("\"workspace_lease\":\"{expected_lease}\"")),
        live_metadata = shell::quote_arg(&live_metadata_path),
        staged_metadata = shell::quote_arg(&metadata_path),
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
        cleanup_incremental_temporary(runner, &temporary, false);
    }
    let release_result = if local_lock {
        fs::remove_dir(&lock).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("release prepared workspace update lock".into()),
            )
        })
    } else {
        run_shell_command(
            &shell_command_for_runner(runner, &release_lock)?,
            "release prepared workspace update lock",
        )
    };
    result.and(release_result).map(|_| delta.transfer.clone())
}

fn cleanup_incremental_temporary(runner: &Runner, temporary: &str, bounded: bool) {
    let command = format!("rm -rf -- {}", shell::quote_arg(temporary));
    let deadline = bounded.then(|| Instant::now() + Duration::from_secs(5));
    match runner.kind {
        RunnerKind::Local => {
            let _ = run_shell_command_before(
                &command,
                "clean incremental workspace temporary",
                deadline,
            );
        }
        RunnerKind::Ssh => {
            if let Ok((_server, client)) = ssh_client_for_runner(runner) {
                if client.is_local {
                    let _ = run_shell_command_before(
                        &command,
                        "clean incremental workspace temporary",
                        deadline,
                    );
                } else {
                    let remote = format!("{}@{}", client.user, client.host);
                    let command = format!(
                        "ssh {} {} {}",
                        ssh_args(&client),
                        shell::quote_arg(&remote),
                        shell::quote_arg(&command),
                    );
                    let _ = run_shell_command_before(
                        &command,
                        "clean incremental SSH workspace temporary",
                        deadline,
                    );
                }
            }
        }
    }
}

fn seed_snapshot_command(seed_path: &str, destination: &str) -> String {
    // A hard-link clone gives the new immutable snapshot its own directory
    // without copying unchanged bytes. If the runner filesystem cannot link
    // across its storage boundary, retain correctness with a local copy while
    // transfer metrics continue to report controller-to-runner content only.
    format!(
        "(cp -al {seed}/. {destination}/ 2>/dev/null || cp -a {seed}/. {destination}/)",
        seed = shell::quote_arg(seed_path),
        destination = shell::quote_arg(destination),
    )
}

fn materialize_changed_paths(
    local_path: &Path,
    target_command: &str,
    changed_paths: &[String],
    action: &str,
) -> Result<()> {
    materialize_changed_paths_before(local_path, target_command, changed_paths, action, None)
}

fn materialize_changed_paths_before(
    local_path: &Path,
    target_command: &str,
    changed_paths: &[String],
    action: &str,
    deadline: Option<Instant>,
) -> Result<()> {
    materialize_changed_paths_controlled(
        local_path,
        target_command,
        changed_paths,
        action,
        &WorkspaceControl::before(deadline),
    )
}

fn materialize_changed_paths_controlled(
    local_path: &Path,
    target_command: &str,
    changed_paths: &[String],
    action: &str,
    control: &WorkspaceControl,
) -> Result<()> {
    control.checkpoint()?;
    if changed_paths.is_empty() {
        return Ok(());
    }
    let mut list = tempfile::NamedTempFile::new()
        .map_err(|err| Error::internal_io(err.to_string(), Some(action.to_string())))?;
    for path in changed_paths {
        control.checkpoint()?;
        list.write_all(path.as_bytes())
            .and_then(|_| list.write_all(&[0]))
            .map_err(|err| Error::internal_io(err.to_string(), Some(action.to_string())))?;
    }
    let command = format!(
        "{} -h --no-recursion -C {} --null -T {} -cf - | {}",
        super::util::PORTABLE_TAR_CREATE,
        shell::quote_arg(&local_path.display().to_string()),
        shell::quote_arg(&list.path().display().to_string()),
        target_command,
    );
    control.shell(&command, action)
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
    materialize_snapshot_git_controlled(
        runner,
        local_path,
        remote_path,
        excludes,
        snapshot,
        &WorkspaceControl::default(),
    )
}

pub(super) fn materialize_snapshot_git_controlled(
    runner: &Runner,
    local_path: &Path,
    remote_path: &str,
    excludes: &[String],
    snapshot: &str,
    control: &WorkspaceControl,
) -> Result<SyntheticCheckoutIdentity> {
    materialize_snapshot_with_scratch_controlled(
        runner,
        local_path,
        remote_path,
        excludes,
        None,
        control,
    )?;
    let source_dirty = git_output(local_path, &["status", "--porcelain=v1"])
        .map(|status| !status.trim().is_empty())
        .unwrap_or(false);
    initialize_synthetic_git_checkout(
        runner,
        local_path,
        remote_path,
        snapshot,
        source_dirty,
        control,
    )
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
    control: &WorkspaceControl,
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
            control.shell(&command, "initialize synthetic snapshot git checkout")?;
        }
        RunnerKind::Ssh => {
            let (_server, client) = ssh_client_for_runner(runner)?;
            if client.is_local {
                control.shell(&command, "initialize synthetic snapshot git checkout")?;
            } else {
                let remote = format!("{}@{}", client.user, client.host);
                let ssh_command = format!(
                    "ssh {ssh_args} {remote} {command}",
                    ssh_args = ssh_args(&client),
                    remote = shell::quote_arg(&remote),
                    command = shell::quote_arg(&command),
                );
                control.shell(
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

pub(super) fn synthetic_git_checkout_command(
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
        "git -C {remote_path} init && git -C {remote_path} config user.email homeboy-snapshot@localhost && git -C {remote_path} config user.name 'Homeboy Snapshot' && git -C {remote_path} add -A -- . && env GIT_AUTHOR_NAME='Homeboy Snapshot' GIT_AUTHOR_EMAIL=homeboy-snapshot@localhost GIT_COMMITTER_NAME='Homeboy Snapshot' GIT_COMMITTER_EMAIL=homeboy-snapshot@localhost GIT_AUTHOR_DATE='1970-01-01T00:00:00Z' GIT_COMMITTER_DATE='1970-01-01T00:00:00Z' git -C {remote_path} commit --allow-empty -m {message} --no-gpg-sign && git -C {remote_path} notes --ref=homeboy-snapshot add -m {note} HEAD{set_remote}",
        message = shell::quote_arg(&format!("Homeboy snapshot {snapshot}")),
        note = shell::quote_arg(&format!("snapshot_identity={snapshot}\nsource_head={source_head}\nsource_dirty={source_dirty}")),
    )
}

pub(crate) fn copy_snapshot_to_directory(
    local_path: &Path,
    destination: &Path,
    excludes: &[String],
) -> Result<()> {
    stream_snapshot(
        local_path,
        &format!(
            "sh -c {}",
            shell::quote_arg(&snapshot_install_command(
                &destination.display().to_string()
            ))
        ),
        excludes,
        "prepare local workspace snapshot",
        None,
    )
}

/// A replay-only controller artifact. Its directory is the sole source passed
/// to transport after the mutable workspace has been read and sealed.
#[derive(Debug)]
pub(crate) struct ImmutableReplaySnapshot {
    stage: tempfile::TempDir,
    pub(crate) identity: String,
}

impl ImmutableReplaySnapshot {
    pub(crate) fn path(&self) -> PathBuf {
        self.stage.path().join("source")
    }
}

/// Seal a replay artifact using exactly the selected transfer filter. Replay
/// deliberately has a stricter policy than ordinary snapshot staging: a link
/// has no stable bytes at this boundary, and `tar -h` must never choose its
/// target after the attestation has completed.
pub(crate) fn immutable_replay_snapshot(
    source: &Path,
    excludes: &[String],
) -> Result<ImmutableReplaySnapshot> {
    let stage = tempfile::tempdir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create replay snapshot stage".to_string()),
        )
    })?;
    let staged = stage.path().join("source");
    fs::create_dir(&staged).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create replay snapshot source".to_string()),
        )
    })?;
    let membership = SnapshotMembership::new(source, excludes, &[])?;
    reject_replay_symlinks(source, source, &membership)?;
    let copied = materialize_replay_archive(source, &staged, excludes);
    reject_replay_symlinks(source, source, &membership)?;
    copied?;
    // Recheck after copying so a directory-to-link race cannot turn an
    // attested replay source into a different staged object.
    reject_replay_symlinks(source, source, &membership)?;
    reject_replay_symlinks(
        &staged,
        &staged,
        &SnapshotMembership::new(&staged, &[], &[])?,
    )?;
    let identity = replay_artifact_identity(&staged, excludes)?;
    Ok(ImmutableReplaySnapshot { stage, identity })
}

fn materialize_replay_archive(source: &Path, staged: &Path, excludes: &[String]) -> Result<()> {
    copy_snapshot_to_directory(source, staged, excludes)
}

fn replay_symlink_error(path: &Path) -> Error {
    Error::validation_invalid_argument(
        "workspace",
        "Lab replay artifact refused a symlink; replay artifacts require materialized regular files and directories",
        Some(path.display().to_string()),
        Some(vec!["Replace the symlink with workspace-owned files, or reissue the command as a new Lab run.".to_string()]),
    )
}

pub(crate) fn replay_artifact_identity(path: &Path, excludes: &[String]) -> Result<String> {
    let content = workspace_content_hash(path, excludes)?;
    let mut policy = excludes.to_vec();
    policy.sort();
    policy.dedup();
    let mut hasher = Sha256::new();
    hasher.update(b"homeboy/lab-replay-artifact/v1\0");
    hasher.update(content.as_bytes());
    hasher.update(b"\0");
    for exclude in &policy {
        hasher.update(exclude.as_bytes());
        hasher.update(b"\0");
    }
    serde_json::to_string(&serde_json::json!({
        "schema": "homeboy/lab-replay-artifact/v2",
        "digest": format!("sha256:{:x}", hasher.finalize()),
        "excludes": policy,
    }))
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize replay artifact identity".to_string()),
        )
    })
}

fn reject_replay_symlinks(
    root: &Path,
    path: &Path,
    membership: &SnapshotMembership<'_>,
) -> Result<()> {
    for entry in fs::read_dir(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read replay snapshot directory".to_string()),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read replay snapshot entry".to_string()),
            )
        })?;
        let entry_path = entry.path();
        if !membership.includes(entry_path.strip_prefix(root).unwrap_or(&entry_path)) {
            continue;
        }
        let metadata = fs::symlink_metadata(&entry_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("inspect replay snapshot entry".to_string()),
            )
        })?;
        if metadata.file_type().is_symlink() {
            return Err(replay_symlink_error(&entry_path));
        }
        if metadata.is_dir() {
            reject_replay_symlinks(root, &entry_path, membership)?;
        }
    }
    Ok(())
}

pub(super) fn stream_snapshot(
    local_path: &Path,
    target_command: &str,
    excludes: &[String],
    action: &str,
    scratch: Option<&Path>,
) -> Result<()> {
    stream_snapshot_controlled(
        local_path,
        target_command,
        excludes,
        action,
        scratch,
        &WorkspaceControl::default(),
    )
}

pub(super) fn stream_snapshot_controlled(
    local_path: &Path,
    target_command: &str,
    excludes: &[String],
    action: &str,
    scratch: Option<&Path>,
    control: &WorkspaceControl,
) -> Result<()> {
    let _ = scratch;
    let (manifest, _) = workspace_content_manifest_and_hash_controlled(
        local_path,
        excludes,
        WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY,
        control,
    )
    .map_err(|e| {
        snapshot_construction_failure(
            "content",
            local_path,
            None,
            &format!("{action}: {}; {}", e.message, e.details),
        )
    })?;
    super::sync::blobs::pipe(
        |writer| {
            let mut archive = tar::Builder::new(writer);
            let mut links = Vec::<PathBuf>::new();
            for entry in &manifest.entries {
                let relative = Path::new(&entry.path);
                if links.iter().any(|p| relative.starts_with(p)) {
                    continue;
                }
                let path = local_path.join(relative);
                if entry.symlink_target.is_some() {
                    archive
                        .append_path_with_name(&path, relative)
                        .map_err(|e| Error::internal_io(e.to_string(), None))?;
                    links.push(relative.to_path_buf());
                } else if entry.kind == "file" {
                    super::sync::blobs::Source::new(path, entry.sha256.clone().unwrap())?.append(
                        &mut archive,
                        relative,
                        control,
                    )?;
                } else {
                    archive
                        .append_dir(relative, path)
                        .map_err(|e| Error::internal_io(e.to_string(), None))?;
                }
            }
            archive
                .finish()
                .map_err(|e| Error::internal_io(e.to_string(), None))
        },
        |reader| {
            let output = control.output_with_stdin(
                std::process::Command::new("sh").args(["-c", target_command]),
                reader,
                action,
            )?;
            if output.status.success() {
                Ok(())
            } else {
                Err(Error::internal_io(
                    String::from_utf8_lossy(&output.stderr).to_string(),
                    Some(action.into()),
                ))
            }
        },
    )
}

const SNAPSHOT_COPY_CHUNK_BYTES: usize = 64 * 1024;

pub(super) fn snapshot_file_sha256(path: &Path, control: &WorkspaceControl) -> Result<String> {
    let mut hash = Sha256::new();
    read_file_chunks(path, control, |chunk| {
        hash.update(chunk);
        Ok(())
    })?;
    Ok(format!("{:x}", hash.finalize()))
}

fn read_file_chunks(
    path: &Path,
    control: &WorkspaceControl,
    mut consume: impl FnMut(&[u8]) -> Result<()>,
) -> Result<u64> {
    control.checkpoint()?;
    let mut file = fs::File::open(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some("read snapshot file".to_string()))
    })?;
    let mut buffer = [0; SNAPSHOT_COPY_CHUNK_BYTES];
    let mut bytes = 0;
    loop {
        control.checkpoint()?;
        let count = file.read(&mut buffer).map_err(|error| {
            Error::internal_io(error.to_string(), Some("read snapshot file".to_string()))
        })?;
        if count == 0 {
            return Ok(bytes);
        }
        consume(&buffer[..count])?;
        bytes += count as u64;
    }
}

pub(super) fn snapshot_construction_failure(
    declaration_id: &str,
    source: &Path,
    staging_output: Option<&Path>,
    reason: &str,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "workspace_snapshot",
        format!("Lab workspace snapshot construction failed before SSH transport: {reason}"),
        Some(format!("{declaration_id}: {}", source.display())),
        Some(vec![
            "Rebuild the Homeboy snapshot staging set, then replay the Cook; component dependency installation cannot repair controller-side snapshot inputs.".to_string(),
        ]),
    )
    .with_retryable(false);
    error.details["classification"] = serde_json::json!("snapshot_construction");
    error.details["declaration_id"] = serde_json::json!(declaration_id);
    error.details["source_identity"] = serde_json::json!(source.display().to_string());
    error.details["staging_output"] = staging_output
        .map(|path| serde_json::json!(path.display().to_string()))
        .unwrap_or(serde_json::Value::Null);
    error.details["reason"] = serde_json::json!(reason);
    error.details["recovery"] = serde_json::json!({
        "owner": "durable_lifecycle",
        "action": "project_lifecycle_recovery",
        "reason": "No safe in-place recovery is known until the durable lifecycle owner evaluates this failed snapshot record.",
        "remediation": "Correct the controller-side snapshot inputs, then start the replacement lifecycle run selected by its owner.",
    });
    error
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
