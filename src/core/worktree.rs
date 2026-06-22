use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::core::component::{self, TargetSpec};
use crate::core::error::{Error, Result};
use crate::core::ownership;
use crate::core::{git, paths};

mod types {
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum TaskWorktreeState {
        Active,
        Removed,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum CleanupPolicy {
        RemoveWhenSafe,
        PreserveOnFailure,
    }

    impl CleanupPolicy {
        pub(super) fn default_for_run(run_id: Option<&str>) -> Self {
            if run_id.is_some() {
                Self::PreserveOnFailure
            } else {
                Self::RemoveWhenSafe
            }
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct TaskWorktreeRecord {
        pub id: String,
        pub component_id: String,
        pub source_checkout: String,
        pub worktree_path: String,
        pub branch: String,
        pub base_ref: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub task_url: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub run_id: Option<String>,
        pub cleanup_policy: CleanupPolicy,
        pub created_at: String,
        pub state: TaskWorktreeState,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    pub struct WorktreeSafetyReport {
        pub dirty: bool,
        pub unpushed_commits: u32,
        pub primary_checkout: bool,
        pub path_contained: bool,
        pub worktree_missing: bool,
        pub safe: bool,
        pub reasons: Vec<String>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeCreateOutput {
        pub record: TaskWorktreeRecord,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeListOutput {
        pub worktrees: Vec<TaskWorktreeRecord>,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeStatusOutput {
        pub record: TaskWorktreeRecord,
        pub safety: WorktreeSafetyReport,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeRemoveOutput {
        pub record: TaskWorktreeRecord,
        pub safety: WorktreeSafetyReport,
        pub removed: bool,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeCleanupOutput {
        pub candidates: Vec<WorktreeRemoveOutput>,
    }

    #[derive(Debug, Clone)]
    pub struct WorktreeCreateOptions {
        pub component_id: String,
        pub branch: String,
        pub from: Option<String>,
        pub task_url: Option<String>,
        pub run_id: Option<String>,
        pub cleanup_policy: Option<CleanupPolicy>,
    }

    #[derive(Debug, Clone)]
    pub struct WorktreeRemoveOptions {
        pub id: String,
        pub force: bool,
    }

    #[derive(Debug, Clone)]
    pub struct WorktreeQueueCreateOptions {
        pub repo: String,
        pub branches: Vec<String>,
        pub from: String,
        pub task_url: Option<String>,
        pub task_ref: Option<String>,
        pub dry_run: bool,
        pub retry_after_seconds: u64,
        pub dmc_bin: String,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    pub struct WorktreeQueueCreateOutput {
        pub schema: &'static str,
        pub repo: String,
        pub base_ref: String,
        pub dry_run: bool,
        pub rows: Vec<WorktreeQueueCreateRow>,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    pub struct WorktreeQueueCreateRow {
        pub branch: String,
        pub handle: String,
        pub status: WorktreeQueueCreateStatus,
        pub command: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub retry_after_seconds: Option<u64>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub active_lock_holder: Option<WorktreeQueueLockHolder>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub error: Option<String>,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum WorktreeQueueCreateStatus {
        Queued,
        ActiveLockHolder,
        Created,
        Failed,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    pub struct WorktreeQueueLockHolder {
        pub lock_key: String,
        pub scope: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub path: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub command: Option<String>,
    }
}

pub use types::{
    CleanupPolicy, TaskWorktreeRecord, TaskWorktreeState, WorktreeCleanupOutput,
    WorktreeCreateOptions, WorktreeCreateOutput, WorktreeListOutput, WorktreeQueueCreateOptions,
    WorktreeQueueCreateOutput, WorktreeQueueCreateRow, WorktreeQueueCreateStatus,
    WorktreeQueueLockHolder, WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeSafetyReport,
    WorktreeStatusOutput,
};

pub fn create(options: WorktreeCreateOptions) -> Result<WorktreeCreateOutput> {
    create_with_store(options, &metadata_dir()?)
}

pub fn list() -> Result<WorktreeListOutput> {
    list_with_store(&metadata_dir()?)
}

pub fn status(id: &str) -> Result<WorktreeStatusOutput> {
    status_with_store(id, &metadata_dir()?)
}

pub fn resolve(id: &str) -> Result<TaskWorktreeRecord> {
    read_record(&metadata_dir()?, id)
}

pub fn remove(options: WorktreeRemoveOptions) -> Result<WorktreeRemoveOutput> {
    remove_with_store(options, &metadata_dir()?)
}

pub fn cleanup(force: bool) -> Result<WorktreeCleanupOutput> {
    let store = metadata_dir()?;
    cleanup_with_store(force, &store)
}

fn cleanup_with_store(force: bool, store: &Path) -> Result<WorktreeCleanupOutput> {
    let mut candidates = Vec::new();
    for record in list_with_store(store)?.worktrees {
        if record.state != TaskWorktreeState::Active {
            continue;
        }
        if record.cleanup_policy == CleanupPolicy::PreserveOnFailure {
            continue;
        }
        candidates.push(remove_with_store(
            WorktreeRemoveOptions {
                id: record.id,
                force,
            },
            store,
        )?);
    }
    Ok(WorktreeCleanupOutput { candidates })
}

fn create_with_store(
    options: WorktreeCreateOptions,
    store_dir: &Path,
) -> Result<WorktreeCreateOutput> {
    let target = component::resolve_target(TargetSpec {
        component_id: Some(&options.component_id),
        path_override: None,
        project: None,
        capability: None,
        allow_synthetic: false,
        accept_bare_directory: false,
        ..TargetSpec::default()
    })?;
    let source_checkout = target
        .git_root
        .clone()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "component",
                "Component local_path is not inside a git checkout",
                Some(options.component_id.clone()),
                Some(vec!["Register a git-backed component checkout".to_string()]),
            )
        })?
        .canonicalize()
        .map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(target.source_path.display().to_string()),
            )
        })?;

    let parent = source_checkout.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "source checkout has no parent: {}",
            source_checkout.display()
        ))
    })?;
    let id = format!("{}@{}", target.component_id, branch_slug(&options.branch));
    let worktree_path = parent.join(&id);
    if worktree_path.exists() {
        return Err(Error::validation_invalid_argument(
            "branch",
            "Task worktree path already exists",
            Some(worktree_path.display().to_string()),
            Some(vec![
                "Use a unique branch name or remove the existing task worktree".to_string(),
            ]),
        ));
    }

    let worktree_owner = ownership::owner_for_path_or_ancestor(parent)?;
    let base_ref = options.from.unwrap_or_else(|| "HEAD".to_string());
    git::run_git(
        &source_checkout,
        &[
            "worktree",
            "add",
            "-b",
            &options.branch,
            &worktree_path.to_string_lossy(),
            &base_ref,
        ],
        "git worktree add",
    )?;
    ownership::normalize_created_path(&worktree_path, worktree_owner, true, "git worktree add")?;

    let record = TaskWorktreeRecord {
        id,
        component_id: target.component_id,
        source_checkout: source_checkout.to_string_lossy().to_string(),
        worktree_path: worktree_path.to_string_lossy().to_string(),
        branch: options.branch,
        base_ref,
        task_url: options.task_url,
        run_id: options.run_id.clone(),
        cleanup_policy: options
            .cleanup_policy
            .unwrap_or_else(|| CleanupPolicy::default_for_run(options.run_id.as_deref())),
        created_at: chrono::Utc::now().to_rfc3339(),
        state: TaskWorktreeState::Active,
    };
    write_record(store_dir, &record)?;
    Ok(WorktreeCreateOutput { record })
}

fn list_with_store(store_dir: &Path) -> Result<WorktreeListOutput> {
    let mut worktrees = Vec::new();
    if !store_dir.exists() {
        return Ok(WorktreeListOutput { worktrees });
    }
    for entry in fs::read_dir(store_dir)
        .map_err(|err| Error::internal_io(err.to_string(), Some(store_dir.display().to_string())))?
    {
        let entry = entry.map_err(|err| Error::internal_io(err.to_string(), None))?;
        if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        worktrees.push(read_record_path(&entry.path())?);
    }
    worktrees.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(WorktreeListOutput { worktrees })
}

fn status_with_store(id: &str, store_dir: &Path) -> Result<WorktreeStatusOutput> {
    let record = read_record(store_dir, id)?;
    let safety = safety_report(&record)?;
    Ok(WorktreeStatusOutput { record, safety })
}

fn remove_with_store(
    options: WorktreeRemoveOptions,
    store_dir: &Path,
) -> Result<WorktreeRemoveOutput> {
    let mut record = read_record(store_dir, &options.id)?;
    let safety = safety_report(&record)?;
    if !options.force && !safety.safe {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Task worktree is not safe to remove",
            Some(record.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }
    if safety.primary_checkout || !safety.path_contained {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "Task worktree failed hard removal safety gates",
            Some(record.id.clone()),
            Some(safety.reasons.clone()),
        ));
    }

    if !safety.worktree_missing {
        git::run_git(
            Path::new(&record.source_checkout),
            &["worktree", "remove", &record.worktree_path],
            "git worktree remove",
        )?;
    }
    record.state = TaskWorktreeState::Removed;
    write_record(store_dir, &record)?;
    Ok(WorktreeRemoveOutput {
        record,
        safety,
        removed: true,
    })
}

fn safety_report(record: &TaskWorktreeRecord) -> Result<WorktreeSafetyReport> {
    let source = canonical_existing_path(&record.source_checkout)?;
    let parent = source.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "source checkout has no parent: {}",
            source.display()
        ))
    })?;
    let raw_worktree = Path::new(&record.worktree_path);
    let worktree = match raw_worktree.canonicalize() {
        Ok(path) => path,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            normalize_missing_path(raw_worktree)
        }
        Err(err) => {
            return Err(Error::internal_io(
                err.to_string(),
                Some(record.worktree_path.clone()),
            ))
        }
    };
    let worktree_missing = !raw_worktree.exists();
    let primary_checkout = source == worktree;
    let path_contained = worktree.starts_with(parent) && worktree != source;
    let dirty = !worktree_missing && is_dirty(&worktree)?;
    let unpushed_commits = if worktree_missing {
        0
    } else {
        unpushed_commit_count(&worktree, &record.base_ref)?
    };
    let mut reasons = Vec::new();
    if dirty {
        reasons.push("dirty worktree".to_string());
    }
    if unpushed_commits > 0 {
        reasons.push(format!("{unpushed_commits} unpushed commit(s)"));
    }
    if primary_checkout {
        reasons.push("refuses to remove primary checkout".to_string());
    }
    if !path_contained {
        reasons.push("worktree path is outside the component checkout parent".to_string());
    }
    let safe = reasons.is_empty();
    Ok(WorktreeSafetyReport {
        dirty,
        unpushed_commits,
        primary_checkout,
        path_contained,
        worktree_missing,
        safe,
        reasons,
    })
}

fn is_dirty(path: &Path) -> Result<bool> {
    Ok(
        !git::run_git(path, &["status", "--porcelain=v1"], "git status")?
            .trim()
            .is_empty(),
    )
}

fn unpushed_commit_count(path: &Path, base_ref: &str) -> Result<u32> {
    let upstream = git::run_git(path, &["rev-parse", "--abbrev-ref", "@{u}"], "git upstream");
    let range = if let Ok(upstream) = upstream {
        let upstream = upstream.trim();
        if upstream.is_empty() {
            format!("{base_ref}..HEAD")
        } else {
            format!("{upstream}..HEAD")
        }
    } else {
        format!("{base_ref}..HEAD")
    };
    let count = git::run_git(path, &["rev-list", "--count", &range], "git rev-list")?;
    Ok(count.trim().parse::<u32>().unwrap_or(0))
}

fn canonical_existing_path(path: &str) -> Result<PathBuf> {
    Path::new(path)
        .canonicalize()
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.to_string())))
}

fn normalize_missing_path(path: &Path) -> PathBuf {
    let Some(parent) = path.parent() else {
        return path.to_path_buf();
    };
    let Some(file_name) = path.file_name() else {
        return path.to_path_buf();
    };
    parent
        .canonicalize()
        .map(|parent| parent.join(file_name))
        .unwrap_or_else(|_| path.to_path_buf())
}

fn metadata_dir() -> Result<PathBuf> {
    let observation_db = paths::observation_db()?;
    let data_root = observation_db.parent().ok_or_else(|| {
        Error::internal_unexpected(format!(
            "observation database path `{}` has no parent directory",
            observation_db.display()
        ))
    })?;

    Ok(data_root.join("task-worktrees"))
}

fn record_path(store_dir: &Path, id: &str) -> PathBuf {
    store_dir.join(format!("{}.json", paths::sanitize_path_segment(id)))
}

fn write_record(store_dir: &Path, record: &TaskWorktreeRecord) -> Result<()> {
    let store_owner = ownership::owner_for_path_or_ancestor(store_dir)?;
    fs::create_dir_all(store_dir).map_err(|err| {
        Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
    })?;
    let json = serde_json::to_string_pretty(record)
        .map_err(|err| Error::internal_json(err.to_string(), Some(record.id.clone())))?;
    let path = record_path(store_dir, &record.id);
    fs::write(&path, format!("{json}\n"))
        .map_err(|err| Error::internal_io(err.to_string(), Some(record.id.clone())))?;
    ownership::normalize_created_path(store_dir, store_owner, false, "write worktree metadata")?;
    ownership::normalize_created_path(&path, store_owner, false, "write worktree metadata")?;
    Ok(())
}

fn read_record(store_dir: &Path, id: &str) -> Result<TaskWorktreeRecord> {
    read_record_path(&record_path(store_dir, id))
}

fn read_record_path(path: &Path) -> Result<TaskWorktreeRecord> {
    let raw = fs::read_to_string(path)
        .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|err| Error::internal_json(err.to_string(), Some(path.display().to_string())))
}

fn branch_slug(branch: &str) -> String {
    branch
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

pub fn queue_create(options: WorktreeQueueCreateOptions) -> Result<WorktreeQueueCreateOutput> {
    let mut rows = Vec::new();
    let total = options.branches.len();
    for (index, branch) in options.branches.iter().enumerate() {
        let command = dmc_add_command(&options, branch);
        let handle = dmc_worktree_handle(&options.repo, branch);

        if options.dry_run {
            rows.push(queue_row(
                branch,
                handle,
                command,
                WorktreeQueueCreateStatus::Queued,
            ));
            continue;
        }

        if let Some(holder) = active_lock_holder(&options.dmc_bin, &options.repo)? {
            let mut row = queue_row(
                branch,
                handle,
                command,
                WorktreeQueueCreateStatus::ActiveLockHolder,
            );
            row.retry_after_seconds = Some(options.retry_after_seconds);
            row.active_lock_holder = Some(holder);
            rows.push(row);
            for queued_branch in options.branches.iter().take(total).skip(index + 1) {
                rows.push(queue_row(
                    queued_branch,
                    dmc_worktree_handle(&options.repo, queued_branch),
                    dmc_add_command(&options, queued_branch),
                    WorktreeQueueCreateStatus::Queued,
                ));
            }
            break;
        }

        let output = Command::new(&options.dmc_bin)
            .args(dmc_add_args(&options, branch))
            .output()
            .map_err(|err| Error::internal_io(err.to_string(), Some(command.join(" "))))?;
        if output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let mut row = queue_row(branch, handle, command, WorktreeQueueCreateStatus::Created);
            row.path = parse_prefixed_line(&stdout, "Path:");
            rows.push(row);
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let stdout = String::from_utf8_lossy(&output.stdout);
            let status_error = format_command_error(&stdout, &stderr);
            let mut row = if let Some(holder) = active_lock_holder(&options.dmc_bin, &options.repo)?
            {
                let mut row = queue_row(
                    branch,
                    handle,
                    command,
                    WorktreeQueueCreateStatus::ActiveLockHolder,
                );
                row.retry_after_seconds = Some(options.retry_after_seconds);
                row.active_lock_holder = Some(holder);
                row
            } else {
                queue_row(branch, handle, command, WorktreeQueueCreateStatus::Failed)
            };
            row.error = Some(status_error);
            rows.push(row);
            for queued_branch in options.branches.iter().take(total).skip(index + 1) {
                rows.push(queue_row(
                    queued_branch,
                    dmc_worktree_handle(&options.repo, queued_branch),
                    dmc_add_command(&options, queued_branch),
                    WorktreeQueueCreateStatus::Queued,
                ));
            }
            break;
        }
    }

    Ok(WorktreeQueueCreateOutput {
        schema: "homeboy/worktree-queue-create/v1",
        repo: options.repo,
        base_ref: options.from,
        dry_run: options.dry_run,
        rows,
    })
}

fn queue_row(
    branch: &str,
    handle: String,
    command: Vec<String>,
    status: WorktreeQueueCreateStatus,
) -> WorktreeQueueCreateRow {
    WorktreeQueueCreateRow {
        branch: branch.to_string(),
        handle,
        status,
        command,
        retry_after_seconds: None,
        active_lock_holder: None,
        path: None,
        error: None,
    }
}

fn dmc_add_command(options: &WorktreeQueueCreateOptions, branch: &str) -> Vec<String> {
    let mut command = vec![options.dmc_bin.clone()];
    command.extend(dmc_add_args(options, branch));
    command
}

fn dmc_add_args(options: &WorktreeQueueCreateOptions, branch: &str) -> Vec<String> {
    let mut args = vec![
        "wp".to_string(),
        "datamachine-code".to_string(),
        "workspace".to_string(),
        "worktree".to_string(),
        "add".to_string(),
        options.repo.clone(),
        branch.to_string(),
        format!("--from={}", options.from),
    ];
    if let Some(task_url) = &options.task_url {
        args.push(format!("--task-url={task_url}"));
    }
    if let Some(task_ref) = &options.task_ref {
        args.push(format!("--task-ref={task_ref}"));
    }
    args
}

fn dmc_worktree_handle(repo: &str, branch: &str) -> String {
    format!("{}@{}", repo, branch_slug(branch))
}

fn active_lock_holder(dmc_bin: &str, repo: &str) -> Result<Option<WorktreeQueueLockHolder>> {
    let output = Command::new(dmc_bin)
        .args([
            "wp",
            "datamachine-code",
            "workspace",
            "worktree",
            "locks",
            "--format=json",
        ])
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some("DMC lock status".to_string())))?;
    if !output.status.success() {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        Error::internal_json(err.to_string(), Some("DMC lock status".to_string()))
    })?;
    Ok(active_lock_holder_from_status(&value, repo))
}

fn active_lock_holder_from_status(value: &Value, repo: &str) -> Option<WorktreeQueueLockHolder> {
    let lock_key = format!("worktree-{repo}");
    for section in ["database", "filesystem"] {
        let Some(section_value) = value.get(section) else {
            continue;
        };
        let active_keys = section_value
            .get("active_keys")
            .and_then(Value::as_array)
            .map(|keys| keys.iter().filter_map(Value::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        let Some(locks) = section_value.get("locks").and_then(Value::as_array) else {
            continue;
        };
        for lock in locks {
            let key = lock
                .get("lock_key")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let scope = lock
                .get("scope")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let state = lock
                .get("state")
                .and_then(Value::as_str)
                .unwrap_or_default();
            let active = state == "active"
                || active_keys.iter().any(|active_key| {
                    *active_key == key || *active_key == scope || *active_key == lock_key
                });
            if (key == lock_key || scope == repo) && active {
                return Some(WorktreeQueueLockHolder {
                    lock_key: key.to_string(),
                    scope: scope.to_string(),
                    path: lock.get("path").and_then(Value::as_str).map(str::to_string),
                    command: lock
                        .get("command")
                        .and_then(Value::as_str)
                        .map(str::to_string),
                });
            }
        }
    }
    None
}

fn parse_prefixed_line(output: &str, prefix: &str) -> Option<String> {
    output.lines().find_map(|line| {
        line.trim()
            .strip_prefix(prefix)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn format_command_error(stdout: &str, stderr: &str) -> String {
    let message = [stderr.trim(), stdout.trim()]
        .into_iter()
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>()
        .join("\n");
    if message.is_empty() {
        "DMC worktree add failed without output".to_string()
    } else {
        message
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run_git(dir: &Path, args: &[&str]) {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?} failed: stdout={} stderr={}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn fixture_record(source: &Path, worktree: &Path) -> TaskWorktreeRecord {
        TaskWorktreeRecord {
            id: "fixture@task".to_string(),
            component_id: "fixture".to_string(),
            source_checkout: source.to_string_lossy().to_string(),
            worktree_path: worktree.to_string_lossy().to_string(),
            branch: "task".to_string(),
            base_ref: "HEAD".to_string(),
            task_url: Some("https://example.com/task".to_string()),
            run_id: None,
            cleanup_policy: CleanupPolicy::RemoveWhenSafe,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            state: TaskWorktreeState::Active,
        }
    }

    fn git_repo() -> tempfile::TempDir {
        let temp = tempfile::tempdir().unwrap();
        run_git(temp.path(), &["init", "-q"]);
        run_git(
            temp.path(),
            &["config", "user.email", "homeboy@example.com"],
        );
        run_git(temp.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(temp.path().join("README.md"), "initial\n").unwrap();
        run_git(temp.path(), &["add", "."]);
        run_git(temp.path(), &["commit", "-q", "-m", "initial"]);
        temp
    }

    #[test]
    fn metadata_round_trips_and_lists() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("source");
        let worktree = dir.path().join("source@task");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&worktree).unwrap();
        let store = dir.path().join("store");
        let record = fixture_record(&source, &worktree);

        write_record(&store, &record).unwrap();
        let listed = list_with_store(&store).unwrap();

        assert_eq!(listed.worktrees, vec![record]);
    }

    #[test]
    fn safety_report_blocks_dirty_worktree() {
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "dirty");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "dirty-task",
                &worktree.to_string_lossy(),
            ],
        );
        fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();

        let report = safety_report(&fixture_record(source.path(), &worktree)).unwrap();

        assert!(report.dirty);
        assert!(!report.safe);
        assert!(report
            .reasons
            .iter()
            .any(|reason| reason == "dirty worktree"));
    }

    #[test]
    fn safety_report_blocks_primary_checkout() {
        let source = git_repo();

        let report = safety_report(&fixture_record(source.path(), source.path())).unwrap();

        assert!(report.primary_checkout);
        assert!(!report.path_contained);
        assert!(!report.worktree_missing);
        assert!(!report.safe);
    }

    #[test]
    fn safety_report_allows_missing_contained_worktree() {
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "missing");

        let report = safety_report(&fixture_record(source.path(), &worktree)).unwrap();

        assert!(report.worktree_missing);
        assert!(report.path_contained);
        assert!(!report.primary_checkout);
        assert!(!report.dirty);
        assert_eq!(report.unpushed_commits, 0);
        assert!(report.safe);
    }

    #[test]
    fn cleanup_marks_missing_worktree_record_removed() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "missing-cleanup");
        let store = dir.path().join("store");
        let record = fixture_record(source.path(), &worktree);
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(false, &store).unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        assert_eq!(output.candidates.len(), 1);
        assert!(output.candidates[0].removed);
        assert!(output.candidates[0].safety.worktree_missing);
        assert_eq!(updated.state, TaskWorktreeState::Removed);
    }

    #[test]
    fn safety_report_blocks_unpushed_commits() {
        let remote = tempfile::tempdir().unwrap();
        run_git(remote.path(), &["init", "--bare", "-q"]);
        let source = tempfile::tempdir().unwrap();
        run_git(
            source.path(),
            &["clone", &remote.path().to_string_lossy(), "."],
        );
        run_git(
            source.path(),
            &["config", "user.email", "homeboy@example.com"],
        );
        run_git(source.path(), &["config", "user.name", "Homeboy Test"]);
        fs::write(source.path().join("README.md"), "initial\n").unwrap();
        run_git(source.path(), &["add", "."]);
        run_git(source.path(), &["commit", "-q", "-m", "initial"]);
        run_git(source.path(), &["push", "-u", "origin", "HEAD:main"]);

        let worktree = sibling_worktree_path(source.path(), "unpushed");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "unpushed-task",
                &worktree.to_string_lossy(),
                "HEAD",
            ],
        );
        fs::write(worktree.join("change.txt"), "change\n").unwrap();
        run_git(&worktree, &["add", "."]);
        run_git(&worktree, &["commit", "-q", "-m", "change"]);

        let mut record = fixture_record(source.path(), &worktree);
        record.base_ref = "origin/main".to_string();
        let report = safety_report(&record).unwrap();

        assert_eq!(report.unpushed_commits, 1);
        assert!(!report.safe);
    }

    fn sibling_worktree_path(source: &Path, suffix: &str) -> PathBuf {
        let name = source
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("source");
        source.with_file_name(format!("{name}-{suffix}-worktree"))
    }

    fn queue_options() -> WorktreeQueueCreateOptions {
        WorktreeQueueCreateOptions {
            repo: "homeboy".to_string(),
            branches: vec!["cook/one".to_string(), "cook/two".to_string()],
            from: "origin/main".to_string(),
            task_url: Some("https://github.com/Extra-Chill/homeboy/issues/5786".to_string()),
            task_ref: Some("Extra-Chill/homeboy#5786".to_string()),
            dry_run: true,
            retry_after_seconds: 30,
            dmc_bin: "studio".to_string(),
        }
    }

    #[test]
    fn queue_create_dry_run_returns_queued_rows_with_exact_dmc_commands() {
        let output = queue_create(queue_options()).unwrap();

        assert_eq!(output.schema, "homeboy/worktree-queue-create/v1");
        assert_eq!(output.rows.len(), 2);
        assert_eq!(output.rows[0].status, WorktreeQueueCreateStatus::Queued);
        assert_eq!(output.rows[0].handle, "homeboy@cook-one");
        assert_eq!(
            output.rows[0].command,
            vec![
                "studio",
                "wp",
                "datamachine-code",
                "workspace",
                "worktree",
                "add",
                "homeboy",
                "cook/one",
                "--from=origin/main",
                "--task-url=https://github.com/Extra-Chill/homeboy/issues/5786",
                "--task-ref=Extra-Chill/homeboy#5786",
            ]
        );
    }

    #[test]
    fn active_lock_status_distinguishes_holder_for_repo() {
        let status = serde_json::json!({
            "database": { "locks": [{
                "lock_key": "worktree-homeboy",
                "scope": "homeboy",
                "state": "active",
                "path": "/tmp/worktree-homeboy.lock",
                "command": "wp datamachine-code workspace worktree add homeboy cook/one"
            }]},
            "filesystem": { "locks": [] }
        });

        let holder = active_lock_holder_from_status(&status, "homeboy").unwrap();

        assert_eq!(holder.lock_key, "worktree-homeboy");
        assert_eq!(holder.scope, "homeboy");
        assert_eq!(holder.path.as_deref(), Some("/tmp/worktree-homeboy.lock"));
        assert_eq!(
            holder.command.as_deref(),
            Some("wp datamachine-code workspace worktree add homeboy cook/one")
        );
    }

    #[test]
    fn active_lock_status_checks_filesystem_when_database_section_is_absent() {
        let status = serde_json::json!({
            "filesystem": {
                "active_keys": ["worktree-homeboy"],
                "locks": [{
                    "lock_key": "worktree-homeboy",
                    "scope": "homeboy",
                    "path": "/tmp/worktree-homeboy.lock"
                }]
            }
        });

        let holder = active_lock_holder_from_status(&status, "homeboy").unwrap();

        assert_eq!(holder.lock_key, "worktree-homeboy");
        assert_eq!(holder.scope, "homeboy");
        assert_eq!(holder.path.as_deref(), Some("/tmp/worktree-homeboy.lock"));
    }
}
