//! Per-child workload invocation isolation.

use crate::core::engine::run_dir::RunDir;
use crate::core::error::{Error, Result};
use crate::core::paths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, SystemTime};

const LOCK_NAME: &str = ".index.lock";
const LOCK_STALE_AFTER: Duration = Duration::from_secs(30);
const LOCK_ATTEMPTS: usize = 100;
const LOCK_SLEEP: Duration = Duration::from_millis(20);
const PORT_POOL_START: u16 = 20_000;
const PORT_POOL_END: u16 = 60_999;

mod child;
mod errors;
mod runtime;
pub use child::{
    cleanup_invocation_children, cleanup_stale_child_records, register_child_process,
    InvocationChildGuard, InvocationChildRecord,
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
    /// Sibling invocation directories (state, artifact, tmp) under
    /// [`invocation_runtime_root`]. All three are removed on `Drop` so
    /// concurrent invocations do not accumulate stale state on disk.
    /// Decoupled from any caller-provided `RunDir` cleanup.
    cleanup_paths: [PathBuf; 3],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct InvocationLease {
    invocation_id: String,
    /// Full UUID retained for traceability across logs and observation
    /// records. Path components use [`short_invocation_id`] instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    invocation_uuid: Option<String>,
    pid: u32,
    started_at: String,
    port_base: Option<u16>,
    port_max: Option<u16>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    named_leases: Vec<String>,
}

impl InvocationGuard {
    /// Acquire an isolated invocation environment.
    ///
    /// `run_dir` is retained for API compatibility and pipeline context,
    /// but the invocation's state/artifact/tmp directories are placed under
    /// a short, platform-aware root (see [`invocation_runtime_root`]) rather
    /// than nested beneath the run dir. This keeps `HOMEBOY_INVOCATION_*`
    /// paths within the platform `sockaddr_un` budget so downstream
    /// workloads can place UNIX sockets under them without bespoke
    /// path-length defense.
    pub fn acquire(run_dir: &RunDir, requirements: &InvocationRequirements) -> Result<Self> {
        let _ = run_dir; // retained for API compatibility (see doc comment)
        cleanup_stale_child_records()?;

        let uuid = uuid::Uuid::new_v4();
        let short = short_invocation_id();
        // Public id keeps the legacy `inv-` prefix so log scrapers and
        // existing string matchers (rigs, runners, child records) keep
        // working. The path component does not include the prefix.
        let id = format!("inv-{}", short);
        let runtime_root = invocation_runtime_root()?;
        // STATE_DIR is the leaf the workload owns: the invocation root
        // itself. ARTIFACT_DIR and TMP_DIR are siblings with `.a` / `.t`
        // suffixes so they cannot collide with workload-created subdirs
        // under STATE_DIR. Removing the `s/a/t` subdir layer used in the
        // initial PR #2312 reclaims 2 bytes of `sockaddr_un` budget per
        // invocation and — more importantly — gives downstream workloads
        // exclusive ownership of the leaf they bind sockets under, so
        // no extra workload-id segment is needed under STATE_DIR.
        let state_dir = runtime_root.join(&short);
        let artifact_dir = runtime_root.join(format!("{short}.a"));
        let tmp_dir = runtime_root.join(format!("{short}.t"));

        // Enforce the sockaddr_un budget before creating anything on disk
        // so callers fail fast with a clear error instead of much later in
        // a downstream workload's UDS bind. STATE_DIR is the leaf
        // workloads will append socket names to, so its budget is the one
        // that matters most; check all three for completeness.
        for dir in [&state_dir, &artifact_dir, &tmp_dir] {
            enforce_path_budget(dir)?;
        }

        for dir in [&state_dir, &artifact_dir, &tmp_dir] {
            fs::create_dir_all(dir)
                .map_err(|e| errors::invocation_dir_create_error(dir, &runtime_root, e))?;
        }

        let needs_lease =
            requirements.port_range_size.is_some() || !requirements.named_leases.is_empty();
        let mut port_base = None;
        let mut port_max = None;
        let mut lease_id = None;

        if needs_lease {
            let _lock = InvocationIndexLock::acquire()?;
            fs::create_dir_all(invocation_leases_dir()?).map_err(|e| {
                Error::internal_unexpected(format!(
                    "Failed to create invocation lease directory: {}",
                    e
                ))
            })?;
            let live_leases = refresh_lease_index()?;
            validate_named_leases(&id, &requirements.named_leases)?;

            if let Some(size) = requirements.port_range_size {
                let (base, max) = allocate_port_range(size, &live_leases)?;
                port_base = Some(base);
                port_max = Some(max);
            }

            let lease = InvocationLease {
                invocation_id: id.clone(),
                invocation_uuid: Some(uuid.to_string()),
                pid: std::process::id(),
                started_at: chrono::Utc::now().to_rfc3339(),
                port_base,
                port_max,
                named_leases: requirements.named_leases.clone(),
            };
            write_lease(&lease)?;
            lease_id = Some(id.clone());
        }

        let cleanup_paths = [state_dir.clone(), artifact_dir.clone(), tmp_dir.clone()];
        Ok(Self {
            env: InvocationEnv {
                id,
                state_dir,
                artifact_dir,
                tmp_dir,
                port_base,
                port_max,
            },
            lease_id,
            named_leases: requirements.named_leases.clone(),
            cleanup_paths,
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

        crate::core::io::copy_tree(
            &self.env.artifact_dir,
            &target,
            "invocation.artifacts.preserve",
            crate::core::io::EntryPolicy::CopyRegularFilesOnly,
        )?;
        self.preserve_artifact_manifest(&target)?;
        Ok(Some(target))
    }

    fn preserve_artifact_manifest(&self, target: &Path) -> Result<()> {
        let source = &self.env.artifact_dir;
        let manifest = if source
            .join(crate::core::artifact_manifest::ARTIFACT_MANIFEST_FILE)
            .is_file()
        {
            let manifest = crate::core::artifact_manifest::read_manifest_from_root(source)?;
            // Validate against the copied target so the manifest describes the
            // preserved artifact tree, not only the pre-copy invocation directory.
            let entries = manifest.validate_under(target)?;
            crate::core::artifact_manifest::ArtifactManifest::new(
                entries.into_iter().map(|entry| entry.entry).collect(),
            )
        } else {
            crate::core::artifact_manifest::manifest_for_existing_files(target)?
        };
        crate::core::artifact_manifest::write_manifest_to_root(target, &manifest)
    }
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        // Best-effort cleanup of all three sibling invocation directories.
        // Decoupled from any caller-provided `RunDir` cleanup so concurrent
        // invocations do not accumulate stale state under the short
        // platform runtime root.
        for path in &self.cleanup_paths {
            let _ = fs::remove_dir_all(path);
        }

        let Some(id) = &self.lease_id else {
            return;
        };
        let Ok(_lock) = InvocationIndexLock::acquire() else {
            return;
        };
        let Ok(path) = lease_path(id) else {
            return;
        };
        let Ok(Some(lease)) = decode_lease_file(&path) else {
            return;
        };
        if lease.pid == std::process::id() {
            let _ = fs::remove_file(path);
        }
    }
}

fn validate_named_leases(invocation_id: &str, wanted: &[String]) -> Result<()> {
    if wanted.is_empty() {
        return Ok(());
    }
    for lease in refresh_lease_index()? {
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

fn refresh_lease_index() -> Result<Vec<InvocationLease>> {
    let mut live = Vec::new();
    for path in invocation_lease_files()? {
        let Some(lease) = decode_lease_file(&path)? else {
            continue;
        };
        if crate::core::process::pid_is_running(lease.pid) {
            live.push(lease);
        } else {
            remove_stale_invocation_lease(&path)?;
        }
    }
    Ok(live)
}

fn invocation_lease_files() -> Result<Vec<PathBuf>> {
    let dir = invocation_leases_dir()?;
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

fn write_lease(lease: &InvocationLease) -> Result<()> {
    let json = serde_json::to_string_pretty(lease).map_err(|e| {
        Error::internal_unexpected(format!("Failed to serialize invocation lease: {}", e))
    })?;
    fs::write(lease_path(&lease.invocation_id)?, json).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to write invocation lease for '{}': {}",
            lease.invocation_id, e
        ))
    })
}

fn lease_path(invocation_id: &str) -> Result<PathBuf> {
    Ok(invocation_leases_dir()?.join(format!(
        "{}.json",
        paths::sanitize_path_segment(invocation_id)
    )))
}

fn invocation_leases_dir() -> Result<PathBuf> {
    Ok(paths::homeboy()?.join("invocation-leases"))
}

struct InvocationIndexLock {
    path: PathBuf,
}

impl InvocationIndexLock {
    fn acquire() -> Result<Self> {
        let dir = invocation_leases_dir()?;
        fs::create_dir_all(&dir).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to create invocation lease directory: {}",
                e
            ))
        })?;
        let path = dir.join(LOCK_NAME);
        for _ in 0..LOCK_ATTEMPTS {
            match fs::create_dir(&path) {
                Ok(()) => return Ok(Self { path }),
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                    remove_stale_index_lock(&path)?;
                    thread::sleep(LOCK_SLEEP);
                }
                Err(e) => {
                    return Err(Error::internal_unexpected(format!(
                        "Failed to acquire invocation lease lock {}: {}",
                        path.display(),
                        e
                    )))
                }
            }
        }
        Err(Error::internal_unexpected(format!(
            "Timed out acquiring invocation lease lock {}",
            path.display()
        )))
    }
}

impl Drop for InvocationIndexLock {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.path);
    }
}

fn remove_stale_index_lock(path: &Path) -> Result<()> {
    let Ok(metadata) = fs::metadata(path) else {
        return Ok(());
    };
    let Ok(modified) = metadata.modified() else {
        return Ok(());
    };
    if SystemTime::now()
        .duration_since(modified)
        .is_ok_and(|age| age > LOCK_STALE_AFTER)
    {
        fs::remove_dir(path).map_err(|e| {
            Error::internal_unexpected(format!(
                "Failed to remove stale invocation lease lock {}: {}",
                path.display(),
                e
            ))
        })?;
    }
    Ok(())
}

#[cfg(test)]
#[path = "../../../tests/core/engine/invocation_test.rs"]
mod invocation_test;

#[cfg(test)]
mod audit_coverage_tests {
    use super::*;
    use crate::core::engine::run_dir::RunDir;
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
