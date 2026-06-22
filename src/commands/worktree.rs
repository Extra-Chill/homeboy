use std::process::Command;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;
use serde_json::Value;

use homeboy::core::cleanup::{
    self as artifact_cleanup, ArtifactCleanupOptions, ArtifactCleanupOutput,
};
use homeboy::core::worktree::{
    self, CleanupPolicy, WorktreeCleanupOutput, WorktreeCreateOptions, WorktreeCreateOutput,
    WorktreeListOutput, WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeStatusOutput,
};

use super::CmdResult;

#[derive(Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeCommand,
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Create a task worktree from a registered component checkout
    Create {
        /// Component ID to use as the source checkout
        component_id: String,
        /// Branch to create in the task worktree
        #[arg(long)]
        branch: String,
        /// Base ref for the new worktree branch
        #[arg(long = "from")]
        from: Option<String>,
        /// Task or issue URL associated with this worktree
        #[arg(long)]
        task_url: Option<String>,
        /// Agent-task run ID associated with this worktree
        #[arg(long)]
        run_id: Option<String>,
        /// Cleanup policy for lifecycle cleanup
        #[arg(long, value_enum)]
        cleanup_policy: Option<CliCleanupPolicy>,
    },
    /// Create multiple DMC worktrees one-at-a-time with lock-aware queue status JSON
    QueueCreate {
        /// DMC workspace repo handle, e.g. homeboy
        repo: String,
        /// Branch to create. Repeat for fanout batches.
        #[arg(long = "branch", value_name = "BRANCH", required = true)]
        branches: Vec<String>,
        /// Base ref for each worktree branch
        #[arg(long = "from", default_value = "origin/main")]
        from: String,
        /// Task or issue URL associated with these worktrees
        #[arg(long)]
        task_url: Option<String>,
        /// Short task reference recorded by DMC, e.g. Extra-Chill/homeboy#5786
        #[arg(long)]
        task_ref: Option<String>,
        /// Print the queue plan/status without creating worktrees
        #[arg(long)]
        dry_run: bool,
        /// Suggested orchestrator wait when DMC reports an active lock but no retry-after value
        #[arg(long, default_value_t = 60)]
        retry_after_seconds: u64,
        /// Executable used for DMC calls. Defaults to `studio`.
        #[arg(long, default_value = "studio")]
        dmc_bin: String,
    },
    /// List persisted task worktrees
    List,
    /// Inspect one task worktree and its safety gates
    Status {
        /// Task worktree ID, e.g. component@branch-slug
        id: String,
    },
    /// Remove one task worktree after safety checks
    Remove {
        /// Task worktree ID, e.g. component@branch-slug
        id: String,
        /// Allow dirty/unpushed worktree removal; hard gates still apply
        #[arg(long)]
        force: bool,
    },
    /// Remove cleanup-eligible task worktrees after safety checks
    Cleanup {
        /// Allow dirty/unpushed worktree removal; hard gates still apply
        #[arg(long)]
        force: bool,
        /// Skip the automatic rebuildable artifact cleanup pass.
        #[arg(long)]
        skip_artifact_cleanup: bool,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum CliCleanupPolicy {
    RemoveWhenSafe,
    PreserveOnFailure,
}

impl From<CliCleanupPolicy> for CleanupPolicy {
    fn from(value: CliCleanupPolicy) -> Self {
        match value {
            CliCleanupPolicy::RemoveWhenSafe => CleanupPolicy::RemoveWhenSafe,
            CliCleanupPolicy::PreserveOnFailure => CleanupPolicy::PreserveOnFailure,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorktreeOutput {
    Create(WorktreeCreateOutput),
    QueueCreate(WorktreeQueueCreateOutput),
    List(WorktreeListOutput),
    Status(WorktreeStatusOutput),
    Remove(WorktreeRemoveOutput),
    Cleanup(WorktreeCleanupCommandOutput),
}

#[derive(Serialize)]
pub struct WorktreeCleanupCommandOutput {
    pub worktrees: WorktreeCleanupOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_cleanup: Option<ArtifactCleanupOutput>,
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

pub fn run(args: WorktreeArgs, _global: &super::GlobalArgs) -> CmdResult<WorktreeOutput> {
    let output = match args.command {
        WorktreeCommand::Create {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy,
        } => WorktreeOutput::Create(worktree::create(WorktreeCreateOptions {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy: cleanup_policy.map(Into::into),
        })?),
        WorktreeCommand::QueueCreate {
            repo,
            branches,
            from,
            task_url,
            task_ref,
            dry_run,
            retry_after_seconds,
            dmc_bin,
        } => WorktreeOutput::QueueCreate(queue_create(WorktreeQueueCreateOptions {
            repo,
            branches,
            from,
            task_url,
            task_ref,
            dry_run,
            retry_after_seconds,
            dmc_bin,
        })?),
        WorktreeCommand::List => WorktreeOutput::List(worktree::list()?),
        WorktreeCommand::Status { id } => WorktreeOutput::Status(worktree::status(&id)?),
        WorktreeCommand::Remove { id, force } => {
            WorktreeOutput::Remove(worktree::remove(WorktreeRemoveOptions { id, force })?)
        }
        WorktreeCommand::Cleanup {
            force,
            skip_artifact_cleanup,
        } => {
            let worktrees = worktree::cleanup(force)?;
            let artifact_cleanup = if skip_artifact_cleanup {
                None
            } else {
                Some(artifact_cleanup::cleanup_artifacts(
                    ArtifactCleanupOptions {
                        path: None,
                        apply: true,
                        self_artifacts: true,
                        temp_roots: Vec::new(),
                        merged_only: false,
                    },
                )?)
            };
            WorktreeOutput::Cleanup(WorktreeCleanupCommandOutput {
                worktrees,
                artifact_cleanup,
            })
        }
    };
    Ok((output, 0))
}

struct WorktreeQueueCreateOptions {
    repo: String,
    branches: Vec<String>,
    from: String,
    task_url: Option<String>,
    task_ref: Option<String>,
    dry_run: bool,
    retry_after_seconds: u64,
    dmc_bin: String,
}

fn queue_create(
    options: WorktreeQueueCreateOptions,
) -> homeboy::core::Result<WorktreeQueueCreateOutput> {
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
            .map_err(|err| {
                homeboy::core::Error::internal_io(err.to_string(), Some(command.join(" ")))
            })?;
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

fn active_lock_holder(
    dmc_bin: &str,
    repo: &str,
) -> homeboy::core::Result<Option<WorktreeQueueLockHolder>> {
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
        .map_err(|err| {
            homeboy::core::Error::internal_io(err.to_string(), Some("DMC lock status".to_string()))
        })?;
    if !output.status.success() {
        return Ok(None);
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|err| {
        homeboy::core::Error::internal_json(err.to_string(), Some("DMC lock status".to_string()))
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
    use serde_json::json;

    fn options() -> WorktreeQueueCreateOptions {
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
        let output = queue_create(options()).unwrap();

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
        let status = json!({
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
        let status = json!({
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
