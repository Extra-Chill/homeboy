//! Per-child workload invocation isolation.

use crate::engine::run_dir::RunDir;
use crate::error::{Error, Result};
use crate::paths;
use homeboy_engine_primitives::fs_index_lock::{FsIndexLock, FsIndexLockConfig};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

const INVOCATION_INDEX_LOCK: FsIndexLockConfig = FsIndexLockConfig::index("invocation lease");
const PORT_POOL_START: u16 = 20_000;
const PORT_POOL_END: u16 = 60_999;

mod child;
mod errors;
mod runtime;
pub use child::{
    cleanup_invocation_children_in_root, cleanup_stale_child_records_in_root,
    register_child_process, register_child_process_in_root, InvocationChildGuard,
    InvocationChildRecord,
};
pub use runtime::{
    enforce_path_budget, invocation_runtime_root, short_invocation_id,
    HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, SOCKET_HEADROOM_BYTES, SUN_PATH_CAPACITY,
};

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InvocationRequirements {
    pub port_range_size: Option<u16>,
    pub named_leases: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvocationEnv {
    pub id: String,
    pub state_dir: PathBuf,
    pub artifact_dir: PathBuf,
    pub tmp_dir: PathBuf,
    pub port_base: Option<u16>,
    pub port_max: Option<u16>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationPortRange {
    pub base: u16,
    pub max: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InvocationContext {
    pub id: String,
    pub state_dir: PathBuf,
    pub artifact_dir: PathBuf,
    pub tmp_dir: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub port_range: Option<InvocationPortRange>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub named_leases: Vec<String>,
}

#[derive(Debug)]
pub struct InvocationGuard {
    env: InvocationEnv,
    lease_id: Option<String>,
    named_leases: Vec<String>,
    /// State and artifact directories remain short-lived siblings beneath the
    /// socket-safe invocation root. Workload temp is instead a managed runtime
    /// entry so cleanup can resolve its durable owner after this guard exits.
    cleanup_paths: [PathBuf; 2],
    runtime_tmp_dir: PathBuf,
    runtime_tmp_alias: Option<PathBuf>,
    runtime_tmp_pin: Option<super::temp::RuntimeTempPin>,
    /// The config root this lease was taken in, captured at acquire time.
    ///
    /// The lease is a claim/release pair: `acquire` writes it and `Drop`
    /// removes it. Resolving the root independently at each end lets a repoint
    /// between them release nothing and leak the lease — which also leaks its
    /// port range and its named leases for the life of the process, since
    /// `refresh_lease_index` only reclaims a lease whose *pid* is gone (#7505).
    config_root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InvocationLease {
    invocation_id: String,
    /// Full UUID retained for traceability across logs and observation
    /// records. Path components use [`short_invocation_id`] instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invocation_uuid: Option<String>,
    pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    linux_starttime_ticks: Option<u64>,
    started_at: String,
    port_base: Option<u16>,
    port_max: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    named_leases: Vec<String>,
}

impl InvocationGuard {
    fn lease_process_identity_matches(lease: &InvocationLease) -> bool {
        if !crate::process::pid_is_running(lease.pid) {
            return false;
        }
        match lease.linux_starttime_ticks {
            Some(expected) => crate::process::linux_process_starttime_ticks(lease.pid)
                .ok()
                .flatten()
                .is_some_and(|actual| actual == expected),
            None => !cfg!(target_os = "linux"),
        }
    }

    pub(crate) fn lease_is_active(invocation_id: &str) -> bool {
        let Ok(config_root) = paths::homeboy() else {
            return false;
        };
        Self::lease_is_active_in_root(&config_root, invocation_id)
    }

    /// [`InvocationGuard::lease_is_active`] against an already-resolved config
    /// root.
    pub(crate) fn lease_is_active_in_root(config_root: &Path, invocation_id: &str) -> bool {
        let path = lease_path_in_root(config_root, invocation_id);
        let Ok(Some(lease)) = decode_lease_file(&path) else {
            return false;
        };
        lease.invocation_id == invocation_id && Self::lease_process_identity_matches(&lease)
    }

    /// Acquire an isolated invocation environment.
    ///
    /// State and artifact directories live under a short, platform-aware root
    /// (see [`invocation_runtime_root`]) so downstream workloads can place
    /// UNIX sockets under them without bespoke path-length defense. The
    /// temporary directory instead lives under managed runtime temp, is bound
    /// to this invocation lease, and is exported as `TMPDIR` for child tools.
    pub fn acquire(run_dir: &RunDir, requirements: &InvocationRequirements) -> Result<Self> {
        let _ = run_dir; // retained for API compatibility (see doc comment)

        // Resolved once and then threaded through every config-rooted step of
        // this acquire — the child-record sweep, the index lock, the lease
        // index, and the lease file — and retained on the guard so `Drop`
        // releases the lease in the same installation it was claimed in.
        //
        // The *runtime* root (state/artifact/socket dirs) is deliberately not
        // derived from this value; see `invocation_runtime_root`.
        let config_root = paths::homeboy()?;
        cleanup_stale_child_records_in_root(&config_root)?;

        let uuid = uuid::Uuid::new_v4();
        let short = short_invocation_id();
        // Public id keeps the legacy `inv-` prefix so log scrapers and
        // existing string matchers (rigs, runners, child records) keep
        // working. The path component does not include the prefix.
        let id = format!("inv-{}", short);
        let runtime_root = invocation_runtime_root()?;
        // STATE_DIR is the leaf the workload owns: the invocation root
        // itself. ARTIFACT_DIR is its `.a` sibling so it cannot collide with
        // workload-created subdirs under STATE_DIR. Removing the `s/a` subdir
        // layer reclaims 2 bytes of `sockaddr_un` budget per
        // invocation and — more importantly — gives downstream workloads
        // exclusive ownership of the leaf they bind sockets under, so
        // no extra workload-id segment is needed under STATE_DIR.
        let state_dir = runtime_root.join(&short);
        let artifact_dir = runtime_root.join(format!("{short}.a"));
        // Enforce the sockaddr_un budget before creating anything on disk
        // so callers fail fast with a clear error instead of much later in
        // a downstream workload's UDS bind. STATE_DIR is the leaf
        // workloads will append socket names to, so its budget is the one
        // that matters most; check both socket-capable directories.
        for dir in [&state_dir, &artifact_dir] {
            enforce_path_budget(dir)?;
        }

        for dir in [&state_dir, &artifact_dir] {
            fs::create_dir_all(dir)
                .map_err(|e| errors::invocation_dir_create_error(dir, &runtime_root, e))?;
        }

        let mut port_base = None;
        let mut port_max = None;
        let _lock = acquire_invocation_index_lock_in_root(&config_root)?;
        fs::create_dir_all(invocation_leases_dir_in_root(&config_root)).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to create invocation lease directory: {}",
                e
            ))
        })?;
        let live_leases = refresh_lease_index_in_root(&config_root)?;
        validate_named_leases_in_root(&config_root, &id, &requirements.named_leases)?;

        if let Some(size) = requirements.port_range_size {
            let (base, max) = allocate_port_range(size, &live_leases)?;
            port_base = Some(base);
            port_max = Some(max);
        }

        let lease = InvocationLease {
            invocation_id: id.clone(),
            invocation_uuid: Some(uuid.to_string()),
            pid: std::process::id(),
            linux_starttime_ticks:
                crate::process::linux_process_starttime_ticks(std::process::id())
                    .ok()
                    .flatten(),
            started_at: chrono::Utc::now().to_rfc3339(),
            port_base,
            port_max,
            named_leases: requirements.named_leases.clone(),
        };
        write_lease_in_root(&config_root, &lease)?;
        if let Err(error) = run_dir.bind_invocation(&id) {
            let _ = fs::remove_file(lease_path_in_root(&config_root, &id));
            return Err(error);
        }

        let (runtime_tmp_dir, runtime_tmp_pin) =
            match super::temp::managed_run_temp_dir_for_producer(
                "homeboy-invocation-tmp",
                Some("invocation"),
            ) {
                Ok(temp) => temp,
                Err(error) => {
                    let _ = fs::remove_file(lease_path_in_root(&config_root, &id));
                    let _ = fs::remove_dir_all(&state_dir);
                    let _ = fs::remove_dir_all(&artifact_dir);
                    return Err(error);
                }
            };
        if let Err(error) = super::temp::bind_run_dir_owner(&runtime_tmp_dir, None, Some(&id)) {
            let _ = fs::remove_dir_all(&runtime_tmp_dir);
            let _ = fs::remove_file(lease_path_in_root(&config_root, &id));
            let _ = fs::remove_dir_all(&state_dir);
            let _ = fs::remove_dir_all(&artifact_dir);
            return Err(error);
        }
        let (exported_tmp_dir, runtime_tmp_alias) =
            match exported_runtime_tmp_dir(&runtime_root, &short, &runtime_tmp_dir) {
                Ok(exported) => exported,
                Err(error) => {
                    let _ = fs::remove_dir_all(&runtime_tmp_dir);
                    let _ = fs::remove_file(lease_path_in_root(&config_root, &id));
                    let _ = fs::remove_dir_all(&state_dir);
                    let _ = fs::remove_dir_all(&artifact_dir);
                    return Err(error);
                }
            };
        let cleanup_paths = [state_dir.clone(), artifact_dir.clone()];
        Ok(Self {
            env: InvocationEnv {
                id: id.clone(),
                state_dir,
                artifact_dir,
                tmp_dir: exported_tmp_dir,
                port_base,
                port_max,
            },
            lease_id: Some(id),
            named_leases: requirements.named_leases.clone(),
            cleanup_paths,
            runtime_tmp_dir,
            runtime_tmp_alias,
            runtime_tmp_pin: Some(runtime_tmp_pin),
            config_root,
        })
    }

    pub fn env_vars(&self) -> Vec<(String, String)> {
        let context = self.context();
        let mut vars = vec![
            ("HOMEBOY_INVOCATION_ID".to_string(), self.env.id.clone()),
            (
                "HOMEBOY_INVOCATION_STATE_DIR".to_string(),
                self.env.state_dir.to_string_lossy().to_string(),
            ),
            (
                "HOMEBOY_INVOCATION_ARTIFACT_DIR".to_string(),
                self.env.artifact_dir.to_string_lossy().to_string(),
            ),
            (
                "HOMEBOY_INVOCATION_TMP_DIR".to_string(),
                self.env.tmp_dir.to_string_lossy().to_string(),
            ),
            // Toolchains conventionally honor TMPDIR, unlike the Homeboy
            // invocation-specific name. Exporting both makes their temporary
            // bytes land under the same durable owner record.
            (
                "TMPDIR".to_string(),
                self.env.tmp_dir.to_string_lossy().to_string(),
            ),
            (
                "HOMEBOY_INVOCATION_CONTEXT_JSON".to_string(),
                serde_json::to_string(&context).expect("serialize invocation context"),
            ),
        ];
        if let (Some(base), Some(max)) = (self.env.port_base, self.env.port_max) {
            vars.push(("HOMEBOY_INVOCATION_PORT_BASE".to_string(), base.to_string()));
            vars.push(("HOMEBOY_INVOCATION_PORT_MAX".to_string(), max.to_string()));
        }
        vars
    }

    pub fn context(&self) -> InvocationContext {
        InvocationContext {
            id: self.env.id.clone(),
            state_dir: self.env.state_dir.clone(),
            artifact_dir: self.env.artifact_dir.clone(),
            tmp_dir: self.env.tmp_dir.clone(),
            port_range: match (self.env.port_base, self.env.port_max) {
                (Some(base), Some(max)) => Some(InvocationPortRange { base, max }),
                _ => None,
            },
            named_leases: self.named_leases.clone(),
        }
    }

    pub fn preserve_artifacts(&self, run_dir: &RunDir) -> Result<Option<PathBuf>> {
        if !self.env.artifact_dir.exists() {
            return Ok(None);
        }

        let target = run_dir
            .path()
            .join("invocations")
            .join(&self.env.id)
            .join("artifacts");

        if target.exists() {
            fs::remove_dir_all(&target).map_err(|e| {
                Error::internal_io(
                    format!("Failed to replace preserved invocation artifacts: {e}"),
                    Some(target.display().to_string()),
                )
            })?;
        }

        crate::io::copy_tree(
            &self.env.artifact_dir,
            &target,
            "invocation.artifacts.preserve",
            crate::io::EntryPolicy::CopyRegularFilesOnly,
        )?;
        self.preserve_artifact_manifest(&target)?;
        Ok(Some(target))
    }

    fn preserve_artifact_manifest(&self, target: &Path) -> Result<()> {
        let source = &self.env.artifact_dir;
        let manifest = if source
            .join(crate::artifact_manifest::ARTIFACT_MANIFEST_FILE)
            .is_file()
        {
            let manifest = crate::artifact_manifest::read_manifest_from_root(source)?;
            // Validate against the copied target so the manifest describes the
            // preserved artifact tree, not only the pre-copy invocation directory.
            let entries = manifest.validate_under(target)?;
            crate::artifact_manifest::ArtifactManifest::new(
                entries.into_iter().map(|entry| entry.entry).collect(),
            )
        } else {
            crate::artifact_manifest::manifest_for_existing_files(target)?
        };
        crate::artifact_manifest::write_manifest_to_root(target, &manifest)
    }
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        // Best-effort cleanup of the short-lived invocation directories and
        // the socket-safe alias to durable managed temp.
        if let Some(alias) = &self.runtime_tmp_alias {
            let _ = fs::remove_file(alias);
        }
        // Decoupled from any caller-provided `RunDir` cleanup so concurrent
        // invocations do not accumulate stale state under the short
        // platform runtime root.
        for path in &self.cleanup_paths {
            let _ = fs::remove_dir_all(path);
        }
        // The temp tree is intentionally retained as terminal managed storage.
        // A killed parent leaves its active metadata/pin behind; a normal
        // teardown releases the pin and records a reclaimable terminal state.
        super::temp::mark_run_dir_succeeded(&self.runtime_tmp_dir);
        self.runtime_tmp_pin.take();

        let Some(id) = &self.lease_id else {
            return;
        };
        // The root captured at acquire, not a fresh resolution: releasing the
        // lease anywhere other than where it was claimed releases nothing.
        let Ok(_lock) = acquire_invocation_index_lock_in_root(&self.config_root) else {
            return;
        };
        let path = lease_path_in_root(&self.config_root, id);
        let Ok(Some(lease)) = decode_lease_file(&path) else {
            return;
        };
        if lease.pid == std::process::id() && Self::lease_process_identity_matches(&lease) {
            let _ = fs::remove_file(path);
        }
    }
}

#[cfg(unix)]
fn exported_runtime_tmp_dir(
    runtime_root: &Path,
    short: &str,
    runtime_tmp_dir: &Path,
) -> Result<(PathBuf, Option<PathBuf>)> {
    let alias = runtime_root.join(format!("{short}.t"));
    enforce_path_budget(&alias)?;
    std::os::unix::fs::symlink(runtime_tmp_dir, &alias).map_err(|error| {
        Error::internal_io(
            format!(
                "Failed to create invocation temp alias {} -> {}: {error}",
                alias.display(),
                runtime_tmp_dir.display()
            ),
            Some("invocation.tmp.alias".to_string()),
        )
    })?;
    Ok((alias.clone(), Some(alias)))
}

#[cfg(not(unix))]
fn exported_runtime_tmp_dir(
    _runtime_root: &Path,
    _short: &str,
    runtime_tmp_dir: &Path,
) -> Result<(PathBuf, Option<PathBuf>)> {
    Ok((runtime_tmp_dir.to_path_buf(), None))
}

fn validate_named_leases_in_root(
    config_root: &Path,
    invocation_id: &str,
    wanted: &[String],
) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    for lease in refresh_lease_index_in_root(config_root)? {
        for name in wanted {
            if lease.named_leases.contains(name) {
                return Err(Error::validation_invalid_argument(
                    "named_lease",
                    format!(
                        "Homeboy invocation lease '{}' is already held by invocation '{}' (pid {})",
                        name, lease.invocation_id, lease.pid
                    ),
                    Some(invocation_id.to_string()),
                    Some(vec![name.clone()]),
                ));
            }
        }
    }
    Ok(())
}

fn allocate_port_range(size: u16, live_leases: &[InvocationLease]) -> Result<(u16, u16)> {
    if size == 0 {
        return Err(Error::validation_invalid_argument(
            "port_range_size",
            "must be >= 1",
            None,
            None,
        ));
    }
    let size = size as u32;
    let pool_start = PORT_POOL_START as u32;
    let pool_end = PORT_POOL_END as u32;
    if size > pool_end - pool_start + 1 {
        return Err(Error::validation_invalid_argument(
            "port_range_size",
            format!("{} exceeds Homeboy invocation port pool capacity", size),
            None,
            None,
        ));
    }

    let mut ranges: Vec<(u32, u32)> = live_leases
        .iter()
        .filter_map(|lease| Some((lease.port_base? as u32, lease.port_max? as u32)))
        .collect();
    ranges.sort();

    let mut candidate = pool_start;
    for (base, max) in ranges {
        if candidate + size - 1 < base {
            return Ok((candidate as u16, (candidate + size - 1) as u16));
        }
        if candidate <= max {
            candidate = max + 1;
        }
    }

    if candidate + size - 1 <= pool_end {
        return Ok((candidate as u16, (candidate + size - 1) as u16));
    }

    Err(Error::validation_invalid_argument(
        "port_range_size",
        "no free Homeboy invocation port range is available on this machine",
        None,
        None,
    ))
}

fn refresh_lease_index_in_root(config_root: &Path) -> Result<Vec<InvocationLease>> {
    let mut live = Vec::new();
    for path in invocation_lease_files_in_root(config_root)? {
        let Some(lease) = decode_lease_file(&path)? else {
            continue;
        };
        if InvocationGuard::lease_process_identity_matches(&lease) {
            live.push(lease);
        } else {
            remove_stale_invocation_lease(&path)?;
        }
    }
    Ok(live)
}

fn invocation_lease_files_in_root(config_root: &Path) -> Result<Vec<PathBuf>> {
    let dir = invocation_leases_dir_in_root(config_root);
    if !dir.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    for entry in fs::read_dir(&dir).map_err(|e| {
        Error::internal_unexpected(format!("Failed to read invocation lease directory: {}", e))
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_unexpected(format!("Failed to read invocation lease entry: {}", e))
        })?;
        let path = entry.path();
        if path.extension().is_some_and(|ext| ext == "json") {
            files.push(path);
        }
    }
    files.sort();
    Ok(files)
}

fn remove_stale_invocation_lease(path: &Path) -> Result<()> {
    fs::remove_file(path).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to remove stale invocation lease {}: {}",
                path.display(),
                e
            ),
            Some("invocation.lease.stale".to_string()),
        )
    })
}

fn decode_lease_file(path: &Path) -> Result<Option<InvocationLease>> {
    if !path.exists() {
        return Ok(None);
    }
    let content = fs::read_to_string(path).map_err(|e| read_lease_error(path, e))?;
    if content.trim().is_empty() {
        return Ok(None);
    }
    let parsed = serde_json::from_str::<InvocationLease>(&content).map_err(|e| {
        Error::validation_invalid_json(e, Some(parse_context(path)), Some(json_excerpt(&content)))
    })?;
    Ok(Some(parsed))
}

fn read_lease_error(path: &Path, error: std::io::Error) -> Error {
    Error::internal_unexpected(format!(
        "Failed to read invocation lease {}: {}",
        path.display(),
        error
    ))
}

fn parse_context(path: &Path) -> String {
    format!("parse invocation lease {}", path.display())
}

fn json_excerpt(content: &str) -> String {
    content.chars().take(200).collect()
}

fn write_lease_in_root(config_root: &Path, lease: &InvocationLease) -> Result<()> {
    let json = serde_json::to_string_pretty(lease).map_err(|e| {
        Error::internal_unexpected(format!("Failed to serialize invocation lease: {}", e))
    })?;
    fs::write(lease_path_in_root(config_root, &lease.invocation_id), json).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to write invocation lease for '{}': {}",
            lease.invocation_id, e
        ))
    })
}

/// Lease file below an already-resolved config root.
fn lease_path_in_root(config_root: &Path, invocation_id: &str) -> PathBuf {
    invocation_leases_dir_in_root(config_root).join(format!(
        "{}.json",
        paths::sanitize_path_segment(invocation_id)
    ))
}

/// Lease index directory below an already-resolved config root.
fn invocation_leases_dir_in_root(config_root: &Path) -> PathBuf {
    config_root.join("invocation-leases")
}

type InvocationIndexLock = FsIndexLock;

/// Block until the invocation lease index lock is held. Released on drop.
///
/// Mechanics (mkdir-lock, mtime stale reclaim, the shared 30s/100-attempt/20ms
/// tuning) live in `homeboy_engine_primitives::fs_index_lock`, which
/// `homeboy-rig`'s lease index also uses.
fn acquire_invocation_index_lock_in_root(config_root: &Path) -> Result<InvocationIndexLock> {
    FsIndexLock::acquire_in(
        &invocation_leases_dir_in_root(config_root),
        INVOCATION_INDEX_LOCK,
    )
}

#[cfg(test)]
#[path = "../../../../tests/core/engine/invocation_test.rs"]
mod invocation_test;

#[cfg(test)]
mod audit_coverage_tests {
    use super::*;
    use crate::engine::run_dir::RunDir;
    use crate::test_support::with_isolated_home;

    #[test]
    fn test_env_vars() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
                .expect("invocation guard");
            let env = guard.env_vars();

            assert!(env.iter().any(|(key, _)| key == "HOMEBOY_INVOCATION_ID"));
            assert!(env
                .iter()
                .any(|(key, _)| key == "HOMEBOY_INVOCATION_TMP_DIR"));
        });
    }

    #[test]
    fn preserve_artifacts_copies_before_guard_cleanup() {
        with_isolated_home(|_| {
            let run_dir = RunDir::create().expect("run dir");
            let original_artifact_path;
            let preserved_path;
            {
                let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
                    .expect("invocation guard");
                original_artifact_path = guard.env.artifact_dir.join("nested/result.json");
                fs::create_dir_all(original_artifact_path.parent().expect("artifact parent"))
                    .expect("mkdir");
                fs::write(&original_artifact_path, b"{\"ok\":true}").expect("artifact");

                preserved_path = guard
                    .preserve_artifacts(&run_dir)
                    .expect("preserve artifacts")
                    .expect("preserved path")
                    .join("nested/result.json");

                assert!(original_artifact_path.is_file());
                assert!(preserved_path.is_file());
            }

            assert!(!original_artifact_path.exists());
            assert_eq!(
                fs::read_to_string(&preserved_path).expect("read preserved artifact"),
                "{\"ok\":true}"
            );
            run_dir.cleanup();
        });
    }
}
