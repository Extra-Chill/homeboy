use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::core::agent_task::AgentTaskOutcome;
use crate::core::observation::ObservationStore;
use crate::core::{git, paths, Error, Result};

pub const CONTROLLER_SCRATCH_SCHEMA: &str = "homeboy/controller-scratch/v1";
const INTERRUPTED_RETENTION: &str = "P1D";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ControllerScratchResource {
    pub path: String,
    pub run_id: String,
    #[serde(default)]
    pub plan_id: String,
    pub task_id: String,
    #[serde(default)]
    pub attempt: u32,
    pub root_bound: String,
    pub owner_pid: u32,
    #[serde(default)]
    pub lifecycle_state: String,
    #[serde(default)]
    pub lease_id: String,
    pub reconstructable: bool,
    pub retention: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_ref: Option<String>,
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalized_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_evidence: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interrupted_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerScratchAllocation {
    pub path: PathBuf,
    pub lease_id: String,
}

/// Allocate and durably register one scheduler-owned temporary root for a
/// provider dispatch attempt. Allocation remains separate from terminal lease
/// handling and retention policy.
pub fn allocate_attempt(
    run_id: &str,
    plan_id: &str,
    task_id: &str,
    attempt: u32,
) -> Result<ControllerScratchAllocation> {
    let root = paths::homeboy_data()?.join("controller-scratch/attempts");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", root.display())),
        )
    })?;
    let root = root.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("canonicalize {}", root.display())),
        )
    })?;
    let lease_id = uuid::Uuid::new_v4().to_string();
    let path = root
        .join(paths::sanitize_path_segment(run_id))
        .join(paths::sanitize_path_segment(plan_id))
        .join(paths::sanitize_path_segment(task_id))
        .join(format!("attempt-{attempt}"))
        .join(&lease_id);
    fs::create_dir_all(&path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", path.display())),
        )
    })?;
    let path = path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("canonicalize {}", path.display())),
        )
    })?;
    if path == root || !path.starts_with(&root) {
        return Err(Error::validation_invalid_argument(
            "controller_scratch.path",
            "allocated scratch path must be contained by its root",
            Some(path.display().to_string()),
            None,
        ));
    }

    let mut index = read_index()?;
    index.resources.push(ControllerScratchResource {
        path: path.display().to_string(),
        run_id: run_id.to_string(),
        plan_id: plan_id.to_string(),
        task_id: task_id.to_string(),
        attempt,
        root_bound: root.display().to_string(),
        owner_pid: std::process::id(),
        lifecycle_state: "active".to_string(),
        lease_id: lease_id.clone(),
        reconstructable: true,
        retention: "P7D".to_string(),
        source_ref: None,
        created_at: chrono::Utc::now().to_rfc3339(),
        finalized_at: None,
        terminal_reason: None,
        terminal_evidence: None,
        interrupted_at: None,
    });
    write_index(&index)?;

    Ok(ControllerScratchAllocation { path, lease_id })
}

/// Releases one scheduler-owned attempt after its candidate evidence has been
/// harvested. The first terminal record is authoritative, making retries and
/// duplicate provider completions safe to replay.
pub fn release_attempt(
    allocation: &ControllerScratchAllocation,
    reason: &str,
    evidence: serde_json::Value,
) -> Result<()> {
    let mut index = read_index()?;
    let Some(resource) = index.resources.iter_mut().find(|resource| {
        resource.lease_id == allocation.lease_id
            && Path::new(&resource.path) == allocation.path.as_path()
    }) else {
        return Err(Error::validation_invalid_argument(
            "controller_scratch.lease_id",
            "allocated scratch lease is not registered",
            Some(allocation.lease_id.clone()),
            None,
        ));
    };
    if resource.finalized_at.is_none() {
        resource.lifecycle_state = "released".to_string();
        resource.finalized_at = Some(chrono::Utc::now().to_rfc3339());
        resource.terminal_reason = Some(reason.to_string());
        resource.terminal_evidence = Some(evidence);
        write_index(&index)?;
    }
    Ok(())
}

#[derive(Debug, Default, Serialize, Deserialize)]
struct ControllerScratchIndex {
    #[serde(default = "schema")]
    schema: String,
    #[serde(default)]
    resources: Vec<ControllerScratchResource>,
}

#[derive(Debug, Serialize)]
pub struct ControllerScratchCleanupOutput {
    pub command: &'static str,
    pub mode: &'static str,
    pub candidate_count: usize,
    pub applied_count: usize,
    pub skipped_count: usize,
    pub estimated_bytes: u64,
    pub reclaimed_bytes: u64,
    pub candidates: Vec<ControllerScratchCandidate>,
    pub skipped: Vec<ControllerScratchSkipped>,
    pub remaining_candidate_count: usize,
    pub remaining_candidate_bytes: u64,
    pub has_more: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_command: Option<String>,
    pub drain_command: String,
}

#[derive(Debug, Serialize)]
pub struct ControllerScratchCandidate {
    pub path: String,
    pub run_id: String,
    pub task_id: String,
    pub size_bytes: u64,
    pub owner_pid: u32,
    pub lease_id: String,
    pub reason: String,
    pub lifecycle_state: String,
    pub source_ref: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ControllerScratchSkipped {
    pub path: String,
    pub run_id: Option<String>,
    pub owner_pid: Option<u32>,
    pub lifecycle_state: Option<String>,
    pub reason: String,
}

#[derive(Debug, Clone, Copy)]
pub struct ControllerScratchCleanupOptions {
    pub apply: bool,
    pub limit: usize,
}

/// Registers provider-created controller scratch returned in an outcome's
/// `metadata.controller_scratch` object or array. Providers own materializing
/// the path; Homeboy owns its durable lifecycle and cleanup policy.
pub fn register_outcome_resources(run_id: &str, outcomes: &[AgentTaskOutcome]) -> Result<()> {
    let mut index = read_index()?;
    let mut changed = false;
    for outcome in outcomes {
        let values = outcome
            .metadata
            .get("controller_scratch")
            .map(|value| {
                value
                    .as_array()
                    .cloned()
                    .unwrap_or_else(|| vec![value.clone()])
            })
            .unwrap_or_default();
        for value in values {
            let Some(path) = value.get("path").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let path = PathBuf::from(path).canonicalize().map_err(|error| {
                Error::validation_invalid_argument(
                    "controller_scratch.path",
                    error.to_string(),
                    Some(path.to_string()),
                    None,
                )
            })?;
            let root = value
                .get("root_bound")
                .and_then(serde_json::Value::as_str)
                .map(PathBuf::from)
                .unwrap_or_else(|| path.parent().unwrap_or(&path).to_path_buf())
                .canonicalize()
                .map_err(|error| {
                    Error::validation_invalid_argument(
                        "controller_scratch.root_bound",
                        error.to_string(),
                        None,
                        None,
                    )
                })?;
            if path == root || !path.starts_with(&root) {
                return Err(Error::validation_invalid_argument(
                    "controller_scratch.path",
                    "scratch path must be contained by its declared root",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            let resource = ControllerScratchResource {
                path: path.display().to_string(),
                run_id: run_id.to_string(),
                plan_id: String::new(),
                task_id: outcome.task_id.clone(),
                attempt: 0,
                root_bound: root.display().to_string(),
                owner_pid: std::process::id(),
                lifecycle_state: "provider_registered".to_string(),
                lease_id: String::new(),
                reconstructable: value
                    .get("reconstructable")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                retention: value
                    .get("retention")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("P7D")
                    .to_string(),
                source_ref: value
                    .get("source_ref")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string),
                created_at: chrono::Utc::now().to_rfc3339(),
                finalized_at: None,
                terminal_reason: None,
                terminal_evidence: None,
                interrupted_at: None,
            };
            index
                .resources
                .retain(|existing| existing.path != resource.path);
            index.resources.push(resource);
            changed = true;
        }
    }
    if changed {
        write_index(&index)?;
    }
    Ok(())
}

/// Marks all resources owned by a terminal run as finalized, including failed
/// and cancelled exits, so retention starts regardless of task outcome.
pub fn finalize_run(run_id: &str) -> Result<()> {
    let mut index = read_index()?;
    let now = chrono::Utc::now().to_rfc3339();
    let mut changed = false;
    for resource in &mut index.resources {
        if resource.run_id == run_id {
            if resource.finalized_at.is_none() {
                resource.finalized_at = Some(now.clone());
                changed = true;
            }
        }
    }
    if changed {
        write_index(&index)?;
    }
    Ok(())
}

pub fn cleanup(options: ControllerScratchCleanupOptions) -> Result<ControllerScratchCleanupOutput> {
    let mut index = read_index()?;
    let mut candidates = Vec::new();
    let mut skipped = legacy_unknown_paths(&std::env::temp_dir())?;
    let mut applied_count = 0;
    let mut reclaimed_bytes = 0;
    let now = chrono::Utc::now();
    let mut eligible = Vec::new();
    let mut reconciled = false;
    for resource in &mut index.resources {
        let path = PathBuf::from(&resource.path);
        if !path.exists() {
            continue;
        }
        let lifecycle_state = resource.lifecycle_state.clone();
        let interrupted_at = resource.interrupted_at.clone();
        let reason = cleanup_block_reason(resource, &path, now)?;
        reconciled |= resource.lifecycle_state != lifecycle_state
            || resource.interrupted_at != interrupted_at;
        if let Some(reason) = reason {
            skipped.push(ControllerScratchSkipped {
                path: resource.path.clone(),
                run_id: Some(resource.run_id.clone()),
                owner_pid: Some(resource.owner_pid),
                lifecycle_state: Some(resource.lifecycle_state.clone()),
                reason,
            });
            continue;
        }
        let size_bytes = path_size(&path)?;
        eligible.push(ControllerScratchCandidate {
            path: resource.path.clone(),
            run_id: resource.run_id.clone(),
            task_id: resource.task_id.clone(),
            size_bytes,
            owner_pid: resource.owner_pid,
            lease_id: resource.lease_id.clone(),
            reason: resource.terminal_reason.clone().unwrap_or_default(),
            lifecycle_state: resource.lifecycle_state.clone(),
            source_ref: resource.source_ref.clone(),
        });
    }
    if reconciled {
        write_index(&index)?;
    }

    let candidate_count = eligible.len();
    let estimated_bytes = eligible.iter().map(|candidate| candidate.size_bytes).sum();
    let remaining: Vec<_> = eligible.iter().skip(options.limit).collect();
    let remaining_candidate_count = remaining.len();
    let remaining_candidate_bytes = remaining.iter().map(|candidate| candidate.size_bytes).sum();
    let has_more = remaining_candidate_count > 0;
    for candidate in eligible.into_iter().take(options.limit) {
        if options.apply {
            match remove_candidate(&candidate, now)? {
                Some(bytes) => {
                    applied_count += 1;
                    reclaimed_bytes += bytes;
                }
                None => skipped.push(ControllerScratchSkipped {
                    path: candidate.path.clone(),
                    run_id: Some(candidate.run_id.clone()),
                    owner_pid: Some(candidate.owner_pid),
                    lifecycle_state: Some(candidate.lifecycle_state.clone()),
                    reason: "resource changed or disappeared before deletion".to_string(),
                }),
            }
        }
        candidates.push(candidate);
    }
    Ok(ControllerScratchCleanupOutput {
        command: "cleanup.controller-scratch",
        mode: if options.apply { "apply" } else { "dry_run" },
        candidate_count,
        applied_count,
        skipped_count: skipped.len(),
        estimated_bytes,
        reclaimed_bytes,
        candidates,
        skipped,
        remaining_candidate_count,
        remaining_candidate_bytes,
        has_more,
        next_command: has_more.then(|| cleanup_command(options)),
        drain_command: cleanup_command(ControllerScratchCleanupOptions {
            apply: true,
            limit: options.limit.saturating_mul(10).max(1),
        }),
    })
}

fn cleanup_block_reason(
    resource: &mut ControllerScratchResource,
    path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<String>> {
    if !resource.reconstructable {
        return Ok(Some(
            "resource is not explicitly reconstructable".to_string(),
        ));
    }
    let root = PathBuf::from(&resource.root_bound).canonicalize().ok();
    let canonical = path.canonicalize().ok();
    if root.is_none()
        || canonical.is_none()
        || !canonical
            .as_ref()
            .is_some_and(|path| path.starts_with(root.as_ref().unwrap()))
    {
        return Ok(Some(
            "resource escaped its registered root bound".to_string(),
        ));
    }
    if crate::core::process::pid_is_running(resource.owner_pid) {
        return Ok(Some("owner process is still running".to_string()));
    }
    if resource.lifecycle_state == "active" {
        resource.lifecycle_state = "interrupted".to_string();
        resource.finalized_at = Some(now.to_rfc3339());
        resource.interrupted_at = Some(now.to_rfc3339());
        resource.terminal_reason = Some("owner_process_dead".to_string());
        resource.terminal_evidence = Some(serde_json::json!({
            "owner_pid": resource.owner_pid,
            "stale_retention": INTERRUPTED_RETENTION,
        }));
        return Ok(Some(
            "active lease owner is proven dead; interrupted retention has started".to_string(),
        ));
    }
    let terminal = ObservationStore::open_initialized()?
        .get_run(&resource.run_id)?
        .map(|run| run.status != "running")
        .unwrap_or(true);
    if !terminal {
        return Ok(Some("owning run is still active".to_string()));
    }
    if resource.finalized_at.is_none() {
        return Ok(Some(
            "resource has not been finalized by its owning run".to_string(),
        ));
    }
    let retention = if resource.lifecycle_state == "interrupted" {
        INTERRUPTED_RETENTION
    } else {
        &resource.retention
    };
    if !retention_expired(resource.finalized_at.as_deref(), retention, path, now) {
        return Ok(Some("retention has not expired".to_string()));
    }
    if git_dirty_or_unpushed(path) {
        return Ok(Some("git checkout has dirty or unpushed state".to_string()));
    }
    Ok(None)
}

fn retention_expired(
    finalized_at: Option<&str>,
    retention: &str,
    path: &Path,
    now: chrono::DateTime<chrono::Utc>,
) -> bool {
    let seconds = retention
        .strip_suffix('s')
        .and_then(|value| value.parse::<i64>().ok())
        .or_else(|| {
            retention
                .strip_prefix('P')
                .and_then(|value| value.strip_suffix('D'))
                .and_then(|value| value.parse::<i64>().ok())
                .map(|days| days * 86400)
        })
        .unwrap_or(i64::MAX);
    let reference = finalized_at
        .and_then(|value| chrono::DateTime::parse_from_rfc3339(value).ok())
        .map(|value| value.with_timezone(&chrono::Utc))
        .or_else(|| {
            fs::metadata(path)
                .ok()?
                .modified()
                .ok()
                .map(chrono::DateTime::<chrono::Utc>::from)
        });
    reference.is_some_and(|time| now.signed_duration_since(time).num_seconds() >= seconds)
}

fn remove_candidate(
    candidate: &ControllerScratchCandidate,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<u64>> {
    let mut index = read_index()?;
    let Some(resource) = index.resources.iter_mut().find(|resource| {
        resource.lease_id == candidate.lease_id
            && !resource.lease_id.is_empty()
            && resource.path == candidate.path
            && resource.owner_pid == candidate.owner_pid
    }) else {
        return Ok(None);
    };
    let path = PathBuf::from(&resource.path);
    if !path.exists() || cleanup_block_reason(resource, &path, now)?.is_some() {
        write_index(&index)?;
        return Ok(None);
    }
    let bytes = path_size(&path)?;
    fs::remove_dir_all(&path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("remove {}", path.display())),
        )
    })?;
    write_index(&index)?;
    Ok(Some(bytes))
}

fn cleanup_command(options: ControllerScratchCleanupOptions) -> String {
    let mut command = format!(
        "homeboy cleanup --include controller-scratch --limit {}",
        options.limit
    );
    if options.apply {
        command.push_str(" --apply");
    }
    command
}

fn git_dirty_or_unpushed(path: &Path) -> bool {
    let status = git::run_git(path, &["status", "--porcelain=v1"], "git status");
    let Ok(status) = status else {
        return true;
    };
    if !status.trim().is_empty() {
        return true;
    }
    git::run_git(
        path,
        &["rev-list", "--count", "@{upstream}..HEAD"],
        "git rev-list upstream",
    )
    .map(|count| count.trim() != "0")
    .unwrap_or(true)
}

fn legacy_unknown_paths(root: &Path) -> Result<Vec<ControllerScratchSkipped>> {
    let index = read_index()?;
    let known: std::collections::HashSet<_> = index
        .resources
        .iter()
        .map(|resource| resource.path.as_str())
        .collect();
    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(_) => return Ok(Vec::new()),
    };
    let mut unknown = Vec::new();
    for entry in entries.flatten() {
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if path.is_dir()
            && (name.starts_with("homeboy-") || name.starts_with("opencode-"))
            && !known.contains(path.to_string_lossy().as_ref())
        {
            unknown.push(ControllerScratchSkipped {
                path: path.display().to_string(),
                run_id: None,
                owner_pid: None,
                lifecycle_state: None,
                reason: "legacy scratch path has no Homeboy ownership metadata; report-only"
                    .to_string(),
            });
        }
    }
    Ok(unknown)
}

fn path_size(path: &Path) -> Result<u64> {
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("stat {}", path.display())))
    })?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Ok(metadata.len());
    }
    fs::read_dir(path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
        })?
        .try_fold(metadata.len(), |total, entry| {
            Ok(total
                + path_size(
                    &entry
                        .map_err(|error| Error::internal_io(error.to_string(), None))?
                        .path(),
                )?)
        })
}

fn index_path() -> Result<PathBuf> {
    Ok(paths::homeboy_data()?.join("controller-scratch/resources.json"))
}
fn read_index() -> Result<ControllerScratchIndex> {
    let path = index_path()?;
    match fs::read_to_string(&path) {
        Ok(raw) => serde_json::from_str(&raw).map_err(|error| {
            Error::internal_json(error.to_string(), Some(path.display().to_string()))
        }),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(ControllerScratchIndex {
            schema: schema(),
            resources: Vec::new(),
        }),
        Err(error) => Err(Error::internal_io(
            error.to_string(),
            Some(path.display().to_string()),
        )),
    }
}
fn write_index(index: &ControllerScratchIndex) -> Result<()> {
    let path = index_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let raw = serde_json::to_vec_pretty(index)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    fs::write(path, raw).map_err(|error| Error::internal_io(error.to_string(), None))
}
fn schema() -> String {
    CONTROLLER_SCRATCH_SCHEMA.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn resource(path: &Path, root: &Path) -> ControllerScratchResource {
        ControllerScratchResource {
            path: path.display().to_string(),
            run_id: "missing-terminal-run".to_string(),
            plan_id: String::new(),
            task_id: "task-1".to_string(),
            attempt: 0,
            root_bound: root.display().to_string(),
            owner_pid: u32::MAX,
            lifecycle_state: "active".to_string(),
            lease_id: "test-lease".to_string(),
            reconstructable: true,
            retention: "0s".to_string(),
            source_ref: None,
            created_at: chrono::Utc::now().to_rfc3339(),
            finalized_at: Some(chrono::Utc::now().to_rfc3339()),
            terminal_reason: None,
            terminal_evidence: None,
            interrupted_at: None,
        }
    }

    #[test]
    fn active_owner_protects_expired_reconstructable_resource() {
        let root = tempfile::tempdir().expect("root");
        let scratch = root.path().join("scratch");
        fs::create_dir(&scratch).expect("scratch");
        let mut resource = resource(&scratch, root.path());
        resource.owner_pid = std::process::id();

        assert_eq!(
            cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now()).expect("check"),
            Some("owner process is still running".to_string())
        );
    }

    #[test]
    fn dead_active_owner_transitions_to_interrupted_and_waits_stale_retention() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![resource(&scratch, root.path())],
            })
            .expect("index");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
            })
            .expect("reconcile");
            assert_eq!(output.candidate_count, 0);
            assert_eq!(
                output
                    .skipped
                    .iter()
                    .find(|skipped| skipped.path == scratch.display().to_string())
                    .expect("scratch skip")
                    .reason,
                "active lease owner is proven dead; interrupted retention has started"
            );
            let resource = read_index()
                .expect("index")
                .resources
                .into_iter()
                .next()
                .expect("resource");
            assert_eq!(resource.lifecycle_state, "interrupted");
            assert_eq!(
                resource.terminal_reason.as_deref(),
                Some("owner_process_dead")
            );
            assert_eq!(
                resource.interrupted_at.as_deref(),
                resource.finalized_at.as_deref()
            );
            let interrupted_at = resource.finalized_at.as_deref().expect("finalized");
            let interrupted_at = chrono::DateTime::parse_from_rfc3339(interrupted_at)
                .expect("timestamp")
                .with_timezone(&chrono::Utc);
            assert!(!retention_expired(
                resource.finalized_at.as_deref(),
                INTERRUPTED_RETENTION,
                &scratch,
                interrupted_at
            ));
            assert!(retention_expired(
                resource.finalized_at.as_deref(),
                INTERRUPTED_RETENTION,
                &scratch,
                interrupted_at + chrono::Duration::days(1)
            ));
        });
    }

    #[test]
    fn allocation_is_unique_contained_and_durably_indexed() {
        crate::test_support::with_isolated_home(|_| {
            let first = allocate_attempt("run-1", "plan-1", "task-1", 1).expect("first");
            let second = allocate_attempt("run-1", "plan-1", "task-1", 2).expect("second");
            let index = read_index().expect("index");

            assert_ne!(first.path, second.path);
            assert!(first.path.is_dir());
            assert!(second.path.is_dir());
            let resource = index
                .resources
                .iter()
                .find(|resource| resource.lease_id == first.lease_id)
                .expect("first resource");
            assert!(Path::new(&resource.path).starts_with(&resource.root_bound));
            assert_eq!(resource.run_id, "run-1");
            assert_eq!(resource.plan_id, "plan-1");
            assert_eq!(resource.task_id, "task-1");
            assert_eq!(resource.attempt, 1);
            assert_eq!(resource.lifecycle_state, "active");
            assert_eq!(resource.owner_pid, std::process::id());
            assert!(resource.reconstructable);
            assert!(!resource.created_at.is_empty());
        });
    }

    #[test]
    fn release_is_idempotent_and_preserves_first_terminal_evidence() {
        crate::test_support::with_isolated_home(|_| {
            let allocation = allocate_attempt("run-1", "plan-1", "task-1", 1).expect("allocate");
            release_attempt(
                &allocation,
                "provider_failure",
                serde_json::json!({ "artifact": "first" }),
            )
            .expect("first release");
            release_attempt(
                &allocation,
                "cancelled",
                serde_json::json!({ "artifact": "second" }),
            )
            .expect("replayed release");

            let resource = read_index()
                .expect("index")
                .resources
                .into_iter()
                .find(|resource| resource.lease_id == allocation.lease_id)
                .expect("resource");
            assert_eq!(resource.lifecycle_state, "released");
            assert_eq!(
                resource.terminal_reason.as_deref(),
                Some("provider_failure")
            );
            assert_eq!(
                resource.terminal_evidence,
                Some(serde_json::json!({ "artifact": "first" }))
            );
            assert!(resource.finalized_at.is_some());
        });
    }

    #[test]
    fn unknown_legacy_path_is_report_only() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let path = root.path().join("homeboy-legacy");
            fs::create_dir(&path).expect("legacy");

            let skipped = legacy_unknown_paths(root.path()).expect("inventory");
            assert_eq!(skipped.len(), 1);
            assert_eq!(skipped[0].path, path.display().to_string());
            assert_eq!(skipped[0].run_id, None);
            assert_eq!(skipped[0].owner_pid, None);
            assert_eq!(
                skipped[0].reason,
                "legacy scratch path has no Homeboy ownership metadata; report-only"
            );
            assert!(path.exists());
        });
    }

    #[test]
    fn released_resource_waits_for_its_configured_retention() {
        let root = tempfile::tempdir().expect("root");
        let scratch = root.path().join("scratch");
        fs::create_dir(&scratch).expect("scratch");
        let mut resource = resource(&scratch, root.path());
        resource.lifecycle_state = "released".to_string();
        resource.retention = "P7D".to_string();

        assert_eq!(
            cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now()).expect("check"),
            Some("retention has not expired".to_string())
        );
    }

    #[test]
    fn dirty_or_unpushed_checkout_is_preserved() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            run_git(&scratch, &["init", "-b", "main"]);
            fs::write(scratch.join("untracked.txt"), "dirty").expect("dirty file");
            let mut resource = resource(&scratch, root.path());
            resource.lifecycle_state = "released".to_string();

            assert_eq!(
                cleanup_block_reason(&mut resource, &scratch, chrono::Utc::now()).expect("check"),
                Some("git checkout has dirty or unpushed state".to_string())
            );
            assert!(scratch.exists());
        });
    }

    #[test]
    fn terminal_reconstructable_resource_is_inventoried_and_removed_with_byte_accounting() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            let remote = root.path().join("remote.git");
            run_git(
                root.path(),
                &["init", "--bare", remote.to_str().expect("remote path")],
            );
            fs::create_dir(&scratch).expect("scratch");
            fs::write(scratch.join("generated.txt"), "generated bytes").expect("content");
            run_git(&scratch, &["init", "-b", "main"]);
            run_git(&scratch, &["config", "user.email", "homeboy@example.test"]);
            run_git(&scratch, &["config", "user.name", "Homeboy Test"]);
            run_git(&scratch, &["add", "."]);
            run_git(&scratch, &["commit", "-m", "initial"]);
            run_git(
                &scratch,
                &[
                    "remote",
                    "add",
                    "origin",
                    remote.to_str().expect("remote path"),
                ],
            );
            run_git(&scratch, &["push", "-u", "origin", "main"]);
            let mut resource = resource(&scratch, root.path());
            resource.lifecycle_state = "released".to_string();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![resource],
            })
            .expect("resource index");

            let inventory = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
            })
            .expect("inventory");
            assert_eq!(inventory.candidate_count, 1);
            assert!(inventory.estimated_bytes > 0);
            assert_eq!(inventory.reclaimed_bytes, 0);
            assert_eq!(inventory.candidates[0].owner_pid, u32::MAX);
            assert_eq!(inventory.candidates[0].lifecycle_state, "released");
            assert!(inventory.candidates[0].reason.is_empty());

            let applied = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: 1,
            })
            .expect("apply");
            assert_eq!(applied.applied_count, 1);
            assert_eq!(applied.reclaimed_bytes, inventory.estimated_bytes);
            assert!(!scratch.exists());
            let retained = read_index()
                .expect("index")
                .resources
                .into_iter()
                .find(|resource| resource.lease_id == "test-lease")
                .expect("retained lifecycle evidence");
            assert_eq!(retained.lifecycle_state, "released");
            assert_eq!(retained.path, scratch.display().to_string());

            let repeated = cleanup(ControllerScratchCleanupOptions {
                apply: true,
                limit: 1,
            })
            .expect("repeated apply");
            assert_eq!(repeated.applied_count, 0);
            assert_eq!(repeated.candidate_count, 0);
        });
    }

    #[test]
    fn apply_revalidates_a_resource_that_becomes_live() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let scratch = root.path().join("scratch");
            fs::create_dir(&scratch).expect("scratch");
            let mut stored = resource(&scratch, root.path());
            stored.lifecycle_state = "released".to_string();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![stored],
            })
            .expect("index");

            let candidate = ControllerScratchCandidate {
                path: scratch.display().to_string(),
                run_id: "missing-terminal-run".to_string(),
                task_id: "task-1".to_string(),
                size_bytes: 0,
                owner_pid: u32::MAX,
                lease_id: "test-lease".to_string(),
                reason: String::new(),
                lifecycle_state: "released".to_string(),
                source_ref: None,
            };
            let index = read_index().expect("index");
            let resource = &mut index.resources.into_iter().next().expect("resource");
            resource.owner_pid = std::process::id();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![resource.clone()],
            })
            .expect("live index");

            assert_eq!(
                remove_candidate(&candidate, chrono::Utc::now()).expect("remove"),
                None
            );
            assert!(scratch.exists());
        });
    }

    #[test]
    fn bounded_cleanup_reports_continuation() {
        crate::test_support::with_isolated_home(|_| {
            let root = tempfile::tempdir().expect("root");
            let remote = root.path().join("remote.git");
            run_git(
                root.path(),
                &["init", "--bare", remote.to_str().expect("remote")],
            );
            let first = clean_checkout(root.path(), &remote, "first");
            let second = root.path().join("second");
            run_git(
                root.path(),
                &[
                    "clone",
                    "-b",
                    "main",
                    remote.to_str().expect("remote"),
                    second.to_str().expect("second"),
                ],
            );
            let mut first_resource = resource(&first, root.path());
            first_resource.lifecycle_state = "released".to_string();
            let mut second_resource = resource(&second, root.path());
            second_resource.lease_id = "second-lease".to_string();
            second_resource.lifecycle_state = "released".to_string();
            write_index(&ControllerScratchIndex {
                schema: schema(),
                resources: vec![first_resource, second_resource],
            })
            .expect("index");

            let output = cleanup(ControllerScratchCleanupOptions {
                apply: false,
                limit: 1,
            })
            .expect("preview");
            assert_eq!(output.candidate_count, 2);
            assert_eq!(output.candidates.len(), 1);
            assert_eq!(output.remaining_candidate_count, 1);
            assert!(output.remaining_candidate_bytes > 0);
            assert!(output.has_more);
            assert_eq!(
                output.next_command.as_deref(),
                Some("homeboy cleanup --include controller-scratch --limit 1")
            );
            assert_eq!(
                output.drain_command,
                "homeboy cleanup --include controller-scratch --limit 10 --apply"
            );
        });
    }

    fn clean_checkout(root: &Path, remote: &Path, name: &str) -> PathBuf {
        let scratch = root.join(name);
        fs::create_dir(&scratch).expect("scratch");
        fs::write(scratch.join("generated.txt"), name).expect("content");
        run_git(&scratch, &["init", "-b", "main"]);
        run_git(&scratch, &["config", "user.email", "homeboy@example.test"]);
        run_git(&scratch, &["config", "user.name", "Homeboy Test"]);
        run_git(&scratch, &["add", "."]);
        run_git(&scratch, &["commit", "-m", "initial"]);
        run_git(
            &scratch,
            &["remote", "add", "origin", remote.to_str().expect("remote")],
        );
        run_git(&scratch, &["push", "-u", "origin", "main"]);
        scratch
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let status = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .status()
            .expect("run git");
        assert!(status.success(), "git {args:?}");
    }
}
