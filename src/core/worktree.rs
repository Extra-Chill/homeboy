use std::fs;
use std::path::{Path, PathBuf};

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

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum BranchCleanupIntent {
        DeleteWhenMerged,
        Preserve,
    }

    impl Default for BranchCleanupIntent {
        fn default() -> Self {
            Self::DeleteWhenMerged
        }
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    #[serde(rename_all = "snake_case")]
    pub enum BranchCleanupStatus {
        Merged,
        Unmerged,
        Missing,
        Preserved,
        Unknown,
        Deleted,
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
        #[serde(default)]
        pub branch_cleanup_intent: BranchCleanupIntent,
        pub created_at: String,
        pub state: TaskWorktreeState,
    }

    #[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
    pub struct AdoptedWorkspaceRecord {
        pub handle: String,
        pub path: String,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub kind: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub provenance: Option<serde_json::Value>,
        pub created_at: String,
        pub state: TaskWorktreeState,
    }

    #[derive(Debug, Clone, PartialEq, Eq)]
    pub enum WorkspaceRefRecord {
        Task(TaskWorktreeRecord),
        Adopted(AdoptedWorkspaceRecord),
    }

    impl WorkspaceRefRecord {
        pub fn handle(&self) -> &str {
            match self {
                WorkspaceRefRecord::Task(record) => &record.id,
                WorkspaceRefRecord::Adopted(record) => &record.handle,
            }
        }

        pub fn path(&self) -> &str {
            match self {
                WorkspaceRefRecord::Task(record) => &record.worktree_path,
                WorkspaceRefRecord::Adopted(record) => &record.path,
            }
        }

        pub fn state(&self) -> &TaskWorktreeState {
            match self {
                WorkspaceRefRecord::Task(record) => &record.state,
                WorkspaceRefRecord::Adopted(record) => &record.state,
            }
        }

        pub fn source_kind(&self) -> &'static str {
            match self {
                WorkspaceRefRecord::Task(_) => "task_worktree",
                WorkspaceRefRecord::Adopted(_) => "adopted_workspace",
            }
        }

        pub fn provenance(&self) -> Option<&serde_json::Value> {
            match self {
                WorkspaceRefRecord::Task(_) => None,
                WorkspaceRefRecord::Adopted(record) => record.provenance.as_ref(),
            }
        }
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
    pub struct WorktreeAdoptOutput {
        pub record: AdoptedWorkspaceRecord,
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
        pub branch_cleanup: WorktreeBranchCleanupReport,
        pub removed: bool,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeCleanupOutput {
        pub dry_run: bool,
        pub counts: WorktreeCleanupCounts,
        pub candidates: Vec<WorktreeCleanupCandidate>,
        pub removed: Vec<WorktreeRemoveOutput>,
        pub skipped: Vec<WorktreeCleanupSkipped>,
    }

    #[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
    pub struct WorktreeCleanupCounts {
        pub candidates: usize,
        pub removed: usize,
        pub skipped: usize,
        pub branch_delete_candidates: usize,
        pub branches_deleted: usize,
        pub unmerged_branches: usize,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeCleanupCandidate {
        pub record: TaskWorktreeRecord,
        pub safety: WorktreeSafetyReport,
        pub branch_cleanup: WorktreeBranchCleanupReport,
    }

    #[derive(Debug, Clone, Serialize, PartialEq, Eq)]
    pub struct WorktreeBranchCleanupReport {
        pub branch: String,
        pub base_ref: String,
        pub intent: BranchCleanupIntent,
        pub status: BranchCleanupStatus,
        pub safe_delete: bool,
        pub deleted: bool,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub reason: Option<String>,
        pub cleanup_command: String,
    }

    #[derive(Debug, Clone, Serialize)]
    pub struct WorktreeCleanupSkipped {
        pub record: TaskWorktreeRecord,
        #[serde(skip_serializing_if = "Option::is_none")]
        pub safety: Option<WorktreeSafetyReport>,
        pub reasons: Vec<String>,
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
    pub struct WorktreeAdoptOptions {
        pub handle: String,
        pub path: String,
        pub kind: Option<String>,
        pub provenance: Option<serde_json::Value>,
    }

    #[derive(Debug, Clone)]
    pub struct WorktreeRemoveOptions {
        pub id: String,
        pub force: bool,
        pub cleanup_branch: bool,
        pub allow_unmerged_branch: bool,
    }

    #[derive(Debug, Clone)]
    pub struct WorktreeCleanupOptions {
        pub force: bool,
        pub dry_run: bool,
        pub cleanup_branches: bool,
        pub allow_unmerged_branches: bool,
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
    AdoptedWorkspaceRecord, BranchCleanupIntent, BranchCleanupStatus, CleanupPolicy,
    TaskWorktreeRecord, TaskWorktreeState, WorkspaceRefRecord, WorktreeAdoptOptions,
    WorktreeAdoptOutput, WorktreeBranchCleanupReport, WorktreeCleanupCandidate,
    WorktreeCleanupCounts, WorktreeCleanupOptions, WorktreeCleanupOutput, WorktreeCleanupSkipped,
    WorktreeCreateOptions, WorktreeCreateOutput, WorktreeListOutput, WorktreeQueueCreateOptions,
    WorktreeQueueCreateOutput, WorktreeQueueCreateRow, WorktreeQueueCreateStatus,
    WorktreeQueueLockHolder, WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeSafetyReport,
    WorktreeStatusOutput,
};

pub fn create(options: WorktreeCreateOptions) -> Result<WorktreeCreateOutput> {
    create_with_store(options, &metadata_dir()?)
}

pub fn adopt(options: WorktreeAdoptOptions) -> Result<WorktreeAdoptOutput> {
    adopt_with_store(options, &adopted_metadata_dir()?)
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

pub fn resolve_workspace_ref(handle: &str) -> Result<WorkspaceRefRecord> {
    if let Ok(record) = read_record(&metadata_dir()?, handle) {
        return Ok(WorkspaceRefRecord::Task(record));
    }
    read_adopted_record(&adopted_metadata_dir()?, handle).map(WorkspaceRefRecord::Adopted)
}

pub fn remove(options: WorktreeRemoveOptions) -> Result<WorktreeRemoveOutput> {
    remove_with_store(options, &metadata_dir()?)
}

pub fn cleanup(options: WorktreeCleanupOptions) -> Result<WorktreeCleanupOutput> {
    let store = metadata_dir()?;
    cleanup_with_store(options, &store)
}

mod store_ops {
    use super::*;

    pub(super) fn adopt_with_store(
        options: WorktreeAdoptOptions,
        store_dir: &Path,
    ) -> Result<WorktreeAdoptOutput> {
        if options.handle.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "handle",
                "Adopted workspace handle must not be empty",
                Some(options.handle),
                None,
            ));
        }
        let path = PathBuf::from(&options.path).canonicalize().map_err(|err| {
            Error::validation_invalid_argument(
                "path",
                "Adopted workspace path must exist on the controller",
                Some(format!("{} ({err})", options.path)),
                Some(vec![
                    "Pass an existing local checkout or workspace path.".to_string()
                ]),
            )
        })?;
        if !path.is_dir() {
            return Err(Error::validation_invalid_argument(
                "path",
                "Adopted workspace path must be a directory",
                Some(path.display().to_string()),
                None,
            ));
        }
        let record = AdoptedWorkspaceRecord {
            handle: options.handle,
            path: path.display().to_string(),
            kind: options.kind,
            provenance: options.provenance,
            created_at: chrono::Utc::now().to_rfc3339(),
            state: TaskWorktreeState::Active,
        };
        write_adopted_record(store_dir, &record)?;
        Ok(WorktreeAdoptOutput { record })
    }

    pub(super) fn cleanup_with_store(
        options: WorktreeCleanupOptions,
        store: &Path,
    ) -> Result<WorktreeCleanupOutput> {
        let mut candidates = Vec::new();
        let mut removed = Vec::new();
        let mut skipped = Vec::new();
        for record in list_with_store(store)?.worktrees {
            if record.state != TaskWorktreeState::Active {
                continue;
            }
            if record.cleanup_policy == CleanupPolicy::PreserveOnFailure {
                continue;
            }
            let safety = match safety_report(&record) {
                Ok(safety) => safety,
                Err(error) => {
                    skipped.push(WorktreeCleanupSkipped {
                        record,
                        safety: None,
                        reasons: vec![error.message],
                    });
                    continue;
                }
            };
            let branch_cleanup = branch_cleanup_report(&record)
                .unwrap_or_else(|error| branch_cleanup_unknown(&record, error.message));
            let skip_reasons = cleanup_skip_reasons(&safety, options.force);
            if !skip_reasons.is_empty() {
                skipped.push(WorktreeCleanupSkipped {
                    record,
                    safety: Some(safety),
                    reasons: skip_reasons,
                });
                continue;
            }

            candidates.push(WorktreeCleanupCandidate {
                record: record.clone(),
                safety: safety.clone(),
                branch_cleanup: branch_cleanup.clone(),
            });

            if !options.dry_run {
                removed.push(remove_with_store(
                    WorktreeRemoveOptions {
                        id: record.id,
                        force: options.force,
                        cleanup_branch: options.cleanup_branches,
                        allow_unmerged_branch: options.allow_unmerged_branches,
                    },
                    store,
                )?);
            }
        }
        let branch_delete_candidates = candidates
            .iter()
            .filter(|candidate| candidate.branch_cleanup.safe_delete)
            .count();
        let unmerged_branches = candidates
            .iter()
            .filter(|candidate| candidate.branch_cleanup.status == BranchCleanupStatus::Unmerged)
            .count();
        let branches_deleted = removed
            .iter()
            .filter(|output| output.branch_cleanup.deleted)
            .count();
        let counts = WorktreeCleanupCounts {
            candidates: candidates.len() + skipped.len(),
            removed: removed.len(),
            skipped: skipped.len(),
            branch_delete_candidates,
            branches_deleted,
            unmerged_branches,
        };
        Ok(WorktreeCleanupOutput {
            dry_run: options.dry_run,
            counts,
            candidates,
            removed,
            skipped,
        })
    }

    fn cleanup_skip_reasons(safety: &WorktreeSafetyReport, force: bool) -> Vec<String> {
        let mut reasons = Vec::new();
        if safety.primary_checkout {
            reasons.push("refuses to remove primary checkout".to_string());
        }
        if !safety.path_contained {
            reasons.push("worktree path is outside the component checkout parent".to_string());
        }
        if !force {
            if safety.dirty {
                reasons.push("dirty worktree".to_string());
            }
            if safety.unpushed_commits > 0 {
                reasons.push(format!("{} unpushed commit(s)", safety.unpushed_commits));
            }
        }
        reasons
    }

    pub(super) fn create_with_store(
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
        let source_checkout = source_checkout_for_worktree(&target)?;

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
        ownership::normalize_created_path(
            &worktree_path,
            worktree_owner,
            true,
            "git worktree add",
        )?;

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
            branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
            created_at: chrono::Utc::now().to_rfc3339(),
            state: TaskWorktreeState::Active,
        };
        write_record(store_dir, &record)?;
        Ok(WorktreeCreateOutput { record })
    }

    pub(super) fn list_with_store(store_dir: &Path) -> Result<WorktreeListOutput> {
        let mut worktrees = Vec::new();
        if !store_dir.exists() {
            return Ok(WorktreeListOutput { worktrees });
        }
        for entry in fs::read_dir(store_dir).map_err(|err| {
            Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
        })? {
            let entry = entry.map_err(|err| Error::internal_io(err.to_string(), None))?;
            if entry.path().extension().and_then(|ext| ext.to_str()) != Some("json") {
                continue;
            }
            worktrees.push(read_record_path(&entry.path())?);
        }
        worktrees.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(WorktreeListOutput { worktrees })
    }

    pub(super) fn status_with_store(id: &str, store_dir: &Path) -> Result<WorktreeStatusOutput> {
        let mut record = read_record(store_dir, id)?;
        repair_record_source_checkout_if_needed(&mut record, store_dir)?;
        let safety = safety_report(&record)?;
        Ok(WorktreeStatusOutput { record, safety })
    }

    pub(super) fn remove_with_store(
        options: WorktreeRemoveOptions,
        store_dir: &Path,
    ) -> Result<WorktreeRemoveOutput> {
        let mut record = read_record(store_dir, &options.id)?;
        repair_record_source_checkout_if_needed(&mut record, store_dir)?;
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
            let mut args = vec!["worktree", "remove"];
            if options.force {
                args.push("--force");
            }
            args.push(&record.worktree_path);
            git::run_git(
                Path::new(&record.source_checkout),
                &args,
                "git worktree remove",
            )?;
        }
        let mut branch_cleanup = branch_cleanup_report(&record)
            .unwrap_or_else(|error| branch_cleanup_unknown(&record, error.message));
        if options.cleanup_branch {
            branch_cleanup =
                apply_branch_cleanup(&record, branch_cleanup, options.allow_unmerged_branch)?;
        }
        record.state = TaskWorktreeState::Removed;
        write_record(store_dir, &record)?;
        Ok(WorktreeRemoveOutput {
            record,
            safety,
            branch_cleanup,
            removed: true,
        })
    }

    pub(super) fn branch_cleanup_report(
        record: &TaskWorktreeRecord,
    ) -> Result<WorktreeBranchCleanupReport> {
        let cleanup_command = format!(
            "homeboy worktree remove {} --cleanup-branch",
            shell_arg(&record.id)
        );
        if record.branch_cleanup_intent == BranchCleanupIntent::Preserve {
            return Ok(WorktreeBranchCleanupReport {
                branch: record.branch.clone(),
                base_ref: record.base_ref.clone(),
                intent: record.branch_cleanup_intent.clone(),
                status: BranchCleanupStatus::Preserved,
                safe_delete: false,
                deleted: false,
                reason: Some("branch cleanup intent preserves this branch".to_string()),
                cleanup_command,
            });
        }
        let source = resolved_source_checkout(record)?;
        let branch = record.branch.as_str();
        let exists = git::run_git(
            &source,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}"),
            ],
            "git show-ref branch",
        )
        .is_ok();
        if !exists {
            return Ok(WorktreeBranchCleanupReport {
                branch: record.branch.clone(),
                base_ref: record.base_ref.clone(),
                intent: record.branch_cleanup_intent.clone(),
                status: BranchCleanupStatus::Missing,
                safe_delete: false,
                deleted: false,
                reason: Some("local branch is already missing".to_string()),
                cleanup_command,
            });
        }
        let base_ref = branch_cleanup_base_ref(record);
        let merged = git::run_git(
            &source,
            &["merge-base", "--is-ancestor", branch, &base_ref],
            "git merge-base branch cleanup",
        )
        .is_ok();
        Ok(WorktreeBranchCleanupReport {
            branch: record.branch.clone(),
            base_ref,
            intent: record.branch_cleanup_intent.clone(),
            status: if merged {
                BranchCleanupStatus::Merged
            } else {
                BranchCleanupStatus::Unmerged
            },
            safe_delete: merged,
            deleted: false,
            reason: if merged {
                Some("branch is merged into the cleanup base ref".to_string())
            } else {
                Some("branch is not merged into the cleanup base ref".to_string())
            },
            cleanup_command,
        })
    }

    fn apply_branch_cleanup(
        record: &TaskWorktreeRecord,
        mut report: WorktreeBranchCleanupReport,
        allow_unmerged_branch: bool,
    ) -> Result<WorktreeBranchCleanupReport> {
        if report.status == BranchCleanupStatus::Missing || report.deleted {
            return Ok(report);
        }
        if !report.safe_delete && !allow_unmerged_branch {
            return Ok(report);
        }
        let source = resolved_source_checkout(record)?;
        let delete_flag = if report.safe_delete { "-d" } else { "-D" };
        git::run_git(
            &source,
            &["branch", delete_flag, &record.branch],
            "git branch delete task worktree branch",
        )?;
        report.deleted = true;
        report.status = BranchCleanupStatus::Deleted;
        report.reason = Some(if report.safe_delete {
            "merged branch deleted".to_string()
        } else {
            "unmerged branch deleted by explicit allow flag".to_string()
        });
        Ok(report)
    }

    fn branch_cleanup_unknown(
        record: &TaskWorktreeRecord,
        reason: String,
    ) -> WorktreeBranchCleanupReport {
        WorktreeBranchCleanupReport {
            branch: record.branch.clone(),
            base_ref: record.base_ref.clone(),
            intent: record.branch_cleanup_intent.clone(),
            status: BranchCleanupStatus::Unknown,
            safe_delete: false,
            deleted: false,
            reason: Some(reason),
            cleanup_command: format!(
                "homeboy worktree remove {} --cleanup-branch",
                shell_arg(&record.id)
            ),
        }
    }

    fn branch_cleanup_base_ref(record: &TaskWorktreeRecord) -> String {
        let trimmed = record.base_ref.trim();
        if trimmed.is_empty() || trimmed == "HEAD" {
            return "HEAD".to_string();
        }
        trimmed
            .strip_prefix("origin/")
            .unwrap_or(trimmed)
            .to_string()
    }

    fn shell_arg(value: &str) -> String {
        if value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | '@' | ':'))
        {
            value.to_string()
        } else {
            format!("'{}'", value.replace('\'', "'\\''"))
        }
    }

    pub(super) fn safety_report(record: &TaskWorktreeRecord) -> Result<WorktreeSafetyReport> {
        let source = resolved_source_checkout(record)?;
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

    pub(super) fn is_dirty(path: &Path) -> Result<bool> {
        Ok(
            !git::run_git(path, &["status", "--porcelain=v1"], "git status")?
                .trim()
                .is_empty(),
        )
    }

    pub(super) fn unpushed_commit_count(path: &Path, base_ref: &str) -> Result<u32> {
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

    pub(super) fn canonical_existing_path(path: &str) -> Result<PathBuf> {
        Path::new(path)
            .canonicalize()
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.to_string())))
    }

    fn repair_record_source_checkout_if_needed(
        record: &mut TaskWorktreeRecord,
        store_dir: &Path,
    ) -> Result<()> {
        if Path::new(&record.source_checkout).exists() {
            return Ok(());
        }

        let source = recovered_component_source_checkout(record)?;
        let repaired = source.to_string_lossy().to_string();
        if record.source_checkout != repaired {
            record.source_checkout = repaired;
            write_record(store_dir, record)?;
        }
        Ok(())
    }

    fn resolved_source_checkout(record: &TaskWorktreeRecord) -> Result<PathBuf> {
        if Path::new(&record.source_checkout).exists() {
            return canonical_existing_path(&record.source_checkout);
        }

        recovered_component_source_checkout(record)
    }

    fn recovered_component_source_checkout(record: &TaskWorktreeRecord) -> Result<PathBuf> {
        let target = component::resolve_target(TargetSpec {
            component_id: Some(&record.component_id),
            path_override: None,
            project: None,
            capability: None,
            allow_synthetic: false,
            accept_bare_directory: false,
            ..TargetSpec::default()
        })
        .map_err(|error| missing_source_checkout_error(record, Some(error.message)))?;
        let source = super::queue_ops::source_checkout_for_worktree(&target)
            .map_err(|error| missing_source_checkout_error(record, Some(error.message)))?;
        let worktree = Path::new(&record.worktree_path)
            .canonicalize()
            .unwrap_or_else(|_| normalize_missing_path(Path::new(&record.worktree_path)));

        if source == worktree {
            return Err(missing_source_checkout_error(
                record,
                Some("resolved component checkout is the task worktree itself".to_string()),
            ));
        }

        Ok(source)
    }

    fn missing_source_checkout_error(
        record: &TaskWorktreeRecord,
        recovery_error: Option<String>,
    ) -> Error {
        let mut tried = vec![format!(
            "recorded source_checkout: {}",
            record.source_checkout
        )];
        if let Some(recovery_error) = recovery_error {
            tried.push(format!(
                "component checkout resolution for '{}': {recovery_error}",
                record.component_id
            ));
        } else {
            tried.push(format!(
                "component checkout resolution for '{}'",
                record.component_id
            ));
        }

        Error::validation_invalid_argument(
            "source_checkout",
            "Task worktree source checkout is missing and Homeboy could not safely recover a component checkout",
            Some(record.id.clone()),
            Some(tried),
        )
        .with_hint(format!(
            "Restore the source checkout path or update component '{}' to an existing git checkout, then retry.",
            record.component_id
        ))
        .with_hint(format!(
            "If the task worktree is intentionally gone, remove or repair the metadata record for '{}'.",
            record.id
        ))
    }

    pub(super) fn normalize_missing_path(path: &Path) -> PathBuf {
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

    pub(super) fn metadata_dir() -> Result<PathBuf> {
        let observation_db = paths::observation_db()?;
        let data_root = observation_db.parent().ok_or_else(|| {
            Error::internal_unexpected(format!(
                "observation database path `{}` has no parent directory",
                observation_db.display()
            ))
        })?;

        Ok(data_root.join("task-worktrees"))
    }

    pub(super) fn adopted_metadata_dir() -> Result<PathBuf> {
        let observation_db = paths::observation_db()?;
        let data_root = observation_db.parent().ok_or_else(|| {
            Error::internal_unexpected(format!(
                "observation database path `{}` has no parent directory",
                observation_db.display()
            ))
        })?;

        Ok(data_root.join("adopted-workspaces"))
    }

    pub(super) fn record_path(store_dir: &Path, id: &str) -> PathBuf {
        store_dir.join(format!("{}.json", paths::sanitize_path_segment(id)))
    }

    pub(super) fn write_record(store_dir: &Path, record: &TaskWorktreeRecord) -> Result<()> {
        let store_owner = ownership::owner_for_path_or_ancestor(store_dir)?;
        fs::create_dir_all(store_dir).map_err(|err| {
            Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
        })?;
        let json = serde_json::to_string_pretty(record)
            .map_err(|err| Error::internal_json(err.to_string(), Some(record.id.clone())))?;
        let path = record_path(store_dir, &record.id);
        fs::write(&path, format!("{json}\n"))
            .map_err(|err| Error::internal_io(err.to_string(), Some(record.id.clone())))?;
        ownership::normalize_created_path(
            store_dir,
            store_owner,
            false,
            "write worktree metadata",
        )?;
        ownership::normalize_created_path(&path, store_owner, false, "write worktree metadata")?;
        Ok(())
    }

    pub(super) fn write_adopted_record(
        store_dir: &Path,
        record: &AdoptedWorkspaceRecord,
    ) -> Result<()> {
        let store_owner = ownership::owner_for_path_or_ancestor(store_dir)?;
        fs::create_dir_all(store_dir).map_err(|err| {
            Error::internal_io(err.to_string(), Some(store_dir.display().to_string()))
        })?;
        let json = serde_json::to_string_pretty(record)
            .map_err(|err| Error::internal_json(err.to_string(), Some(record.handle.clone())))?;
        let path = record_path(store_dir, &record.handle);
        fs::write(&path, format!("{json}\n"))
            .map_err(|err| Error::internal_io(err.to_string(), Some(record.handle.clone())))?;
        ownership::normalize_created_path(
            store_dir,
            store_owner,
            false,
            "write adopted workspace metadata",
        )?;
        ownership::normalize_created_path(
            &path,
            store_owner,
            false,
            "write adopted workspace metadata",
        )?;
        Ok(())
    }

    pub(super) fn read_record(store_dir: &Path, id: &str) -> Result<TaskWorktreeRecord> {
        read_record_path(&record_path(store_dir, id))
    }

    pub(super) fn read_record_path(path: &Path) -> Result<TaskWorktreeRecord> {
        let raw = fs::read_to_string(path)
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        serde_json::from_str(&raw)
            .map_err(|err| Error::internal_json(err.to_string(), Some(path.display().to_string())))
    }

    pub(super) fn read_adopted_record(
        store_dir: &Path,
        handle: &str,
    ) -> Result<AdoptedWorkspaceRecord> {
        let path = record_path(store_dir, handle);
        let raw = fs::read_to_string(&path)
            .map_err(|err| Error::internal_io(err.to_string(), Some(path.display().to_string())))?;
        serde_json::from_str(&raw)
            .map_err(|err| Error::internal_json(err.to_string(), Some(path.display().to_string())))
    }

    pub(super) fn branch_slug(branch: &str) -> String {
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
}
use store_ops::*;

pub fn queue_create(options: WorktreeQueueCreateOptions) -> Result<WorktreeQueueCreateOutput> {
    let mut rows = Vec::new();
    let total = options.branches.len();
    for (index, branch) in options.branches.iter().enumerate() {
        let command = worktree_create_command(&options, branch);
        let handle = worktree_handle(&options.repo, branch);

        if options.dry_run {
            rows.push(queue_row(
                branch,
                handle,
                command,
                WorktreeQueueCreateStatus::Queued,
            ));
            continue;
        }

        match create(WorktreeCreateOptions {
            component_id: options.repo.clone(),
            branch: branch.clone(),
            from: Some(options.from.clone()),
            task_url: options.task_url.clone(),
            run_id: None,
            cleanup_policy: None,
        }) {
            Ok(created) => {
                let mut row =
                    queue_row(branch, handle, command, WorktreeQueueCreateStatus::Created);
                row.path = Some(created.record.worktree_path);
                rows.push(row);
            }
            Err(error) => {
                let mut row = queue_row(branch, handle, command, WorktreeQueueCreateStatus::Failed);
                row.error = Some(error.message);
                rows.push(row);
                for queued_branch in options.branches.iter().take(total).skip(index + 1) {
                    rows.push(queue_row(
                        queued_branch,
                        worktree_handle(&options.repo, queued_branch),
                        worktree_create_command(&options, queued_branch),
                        WorktreeQueueCreateStatus::Queued,
                    ));
                }
                break;
            }
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

mod queue_ops {
    use super::*;

    pub(super) fn source_checkout_for_worktree(
        target: &component::ResolvedTarget,
    ) -> Result<PathBuf> {
        if let Some(git_root) = &target.git_root {
            return git_root.canonicalize().map_err(|err| {
                Error::internal_io(
                    err.to_string(),
                    Some(target.source_path.display().to_string()),
                )
            });
        }

        if let Some(checkout) = lab_runner_workspace_checkout(&target.component_id)? {
            return Ok(checkout);
        }

        Err(Error::validation_invalid_argument(
            "component",
            "Component local_path is not inside a git checkout",
            Some(target.component_id.clone()),
            Some(vec!["Register a git-backed component checkout".to_string()]),
        ))
    }

    fn lab_runner_workspace_checkout(component_id: &str) -> Result<Option<PathBuf>> {
        let cwd = std::env::current_dir()
            .map_err(|err| Error::internal_io(err.to_string(), Some("current_dir".to_string())))?;
        let Some(runner_root) = runner_workspace_root_from_lab_snapshot(&cwd) else {
            return Ok(None);
        };
        let candidate = runner_root.join(component_id);
        let Some(git_root) = component::resolution::detect_git_root(&candidate) else {
            return Ok(None);
        };
        if git_root
            != candidate
                .canonicalize()
                .unwrap_or_else(|_| candidate.clone())
        {
            return Ok(None);
        }
        let Some(discovered) = component::discover_from_portable(&git_root) else {
            return Ok(None);
        };
        if discovered.id != component_id {
            return Ok(None);
        }
        git_root.canonicalize().map(Some).map_err(|err| {
            Error::internal_io(err.to_string(), Some(candidate.display().to_string()))
        })
    }

    fn runner_workspace_root_from_lab_snapshot(cwd: &Path) -> Option<PathBuf> {
        for ancestor in cwd.ancestors() {
            if ancestor.file_name().and_then(|name| name.to_str()) == Some("_lab_workspaces") {
                return ancestor.parent().map(Path::to_path_buf);
            }
        }
        None
    }

    pub(super) fn queue_row(
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

    pub(super) fn worktree_create_command(
        options: &WorktreeQueueCreateOptions,
        branch: &str,
    ) -> Vec<String> {
        let mut args = vec![
            "homeboy".to_string(),
            "worktree".to_string(),
            "create".to_string(),
            options.repo.clone(),
            "--branch".to_string(),
            branch.to_string(),
            "--from".to_string(),
            options.from.clone(),
        ];
        if let Some(task_url) = &options.task_url {
            args.push("--task-url".to_string());
            args.push(task_url.clone());
        }
        args
    }

    pub(super) fn worktree_handle(repo: &str, branch: &str) -> String {
        format!("{}@{}", repo, branch_slug(branch))
    }
}
use queue_ops::*;

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
            branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
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

    fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
        let dir = home.join(".config/homeboy/components");
        fs::create_dir_all(&dir).expect("components dir");
        fs::write(
            dir.join(format!("{id}.json")),
            serde_json::json!({
                "local_path": local_path,
                "remote_path": format!("wp-content/plugins/{id}")
            })
            .to_string(),
        )
        .expect("component registration");
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

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: false,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        assert_eq!(output.counts.candidates, 1);
        assert_eq!(output.counts.removed, 1);
        assert_eq!(output.counts.skipped, 0);
        assert_eq!(output.removed.len(), 1);
        assert!(output.removed[0].removed);
        assert!(output.removed[0].safety.worktree_missing);
        assert_eq!(updated.state, TaskWorktreeState::Removed);
    }

    #[test]
    fn cleanup_deletes_merged_task_branch_when_requested() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        run_git(source.path(), &["branch", "task"]);
        let worktree = sibling_worktree_path(source.path(), "merged-branch-cleanup");
        let store = dir.path().join("store");
        let record = fixture_record(source.path(), &worktree);
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: false,
                dry_run: false,
                cleanup_branches: true,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();

        assert_eq!(output.counts.branch_delete_candidates, 1);
        assert_eq!(output.counts.branches_deleted, 1);
        assert_eq!(
            output.removed[0].branch_cleanup.status,
            BranchCleanupStatus::Deleted
        );
        assert!(std::process::Command::new("git")
            .args(["show-ref", "--verify", "--quiet", "refs/heads/task"])
            .current_dir(source.path())
            .status()
            .unwrap()
            .code()
            .is_some_and(|code| code != 0));
    }

    #[test]
    fn cleanup_reports_unmerged_task_branch_without_deleting_by_default() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        run_git(source.path(), &["checkout", "-q", "-b", "task"]);
        fs::write(source.path().join("task.txt"), "task\n").unwrap();
        run_git(source.path(), &["add", "."]);
        run_git(source.path(), &["commit", "-q", "-m", "task"]);
        run_git(source.path(), &["checkout", "-q", "-"]);
        let worktree = sibling_worktree_path(source.path(), "unmerged-branch-cleanup");
        let store = dir.path().join("store");
        let record = fixture_record(source.path(), &worktree);
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: false,
                dry_run: false,
                cleanup_branches: true,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();

        assert_eq!(output.counts.branch_delete_candidates, 0);
        assert_eq!(output.counts.branches_deleted, 0);
        assert_eq!(output.counts.unmerged_branches, 1);
        assert_eq!(
            output.removed[0].branch_cleanup.status,
            BranchCleanupStatus::Unmerged
        );
        run_git(
            source.path(),
            &["show-ref", "--verify", "--quiet", "refs/heads/task"],
        );
    }

    #[test]
    fn status_repairs_missing_source_checkout_from_component_checkout() {
        use crate::test_support::with_isolated_home;

        with_isolated_home(|home| {
            let dir = tempfile::tempdir().unwrap();
            let source = git_repo();
            let missing_source = sibling_worktree_path(source.path(), "removed-source");
            let worktree = sibling_worktree_path(source.path(), "status-repair");
            let store = dir.path().join("store");
            write_component_registration(home.path(), "fixture", source.path());
            let record = fixture_record(&missing_source, &worktree);
            write_record(&store, &record).unwrap();

            let output = status_with_store(&record.id, &store).unwrap();
            let updated = read_record(&store, &record.id).unwrap();

            assert_eq!(
                PathBuf::from(&output.record.source_checkout),
                source.path().canonicalize().unwrap()
            );
            assert_eq!(updated.source_checkout, output.record.source_checkout);
            assert!(output.safety.worktree_missing);
            assert!(output.safety.safe);
        });
    }

    #[test]
    fn status_reports_missing_source_checkout_as_validation_diagnostic() {
        use crate::test_support::with_isolated_home;

        with_isolated_home(|_| {
            let dir = tempfile::tempdir().unwrap();
            let missing_source = dir.path().join("removed-source");
            let worktree = dir.path().join("fixture@task");
            let store = dir.path().join("store");
            let record = fixture_record(&missing_source, &worktree);
            write_record(&store, &record).unwrap();

            let err = status_with_store(&record.id, &store).unwrap_err();

            assert_eq!(
                err.code,
                crate::core::error::ErrorCode::ValidationInvalidArgument
            );
            assert_eq!(
                err.details.get("field").and_then(|field| field.as_str()),
                Some("source_checkout")
            );
            assert!(err
                .to_string()
                .contains("Task worktree source checkout is missing"));
        });
    }

    #[test]
    fn cleanup_skips_unrepairable_missing_source_and_continues() {
        use crate::test_support::with_isolated_home;

        with_isolated_home(|_| {
            let dir = tempfile::tempdir().unwrap();
            let source = git_repo();
            let store = dir.path().join("store");
            let mut unrepairable = fixture_record(
                &dir.path().join("removed-source"),
                &dir.path().join("unrepairable@task"),
            );
            unrepairable.id = "unrepairable@task".to_string();
            unrepairable.component_id = "unrepairable".to_string();
            write_record(&store, &unrepairable).unwrap();
            let removable_worktree = sibling_worktree_path(source.path(), "cleanup-continues");
            let mut removable = fixture_record(source.path(), &removable_worktree);
            removable.id = "fixture@cleanup-continues".to_string();
            write_record(&store, &removable).unwrap();

            let output = cleanup_with_store(
                WorktreeCleanupOptions {
                    force: false,
                    dry_run: false,
                    cleanup_branches: false,
                    allow_unmerged_branches: false,
                },
                &store,
            )
            .unwrap();
            let skipped = read_record(&store, &unrepairable.id).unwrap();
            let removed = read_record(&store, &removable.id).unwrap();

            assert_eq!(output.counts.candidates, 2);
            assert_eq!(output.counts.removed, 1);
            assert_eq!(output.counts.skipped, 1);
            assert_eq!(output.removed[0].record.id, removable.id);
            assert_eq!(output.skipped[0].record.id, unrepairable.id);
            assert_eq!(skipped.state, TaskWorktreeState::Active);
            assert_eq!(removed.state, TaskWorktreeState::Removed);
        });
    }

    #[test]
    fn cleanup_skips_dirty_worktree_without_force() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "dirty-cleanup-refused");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "dirty-cleanup-refused",
                &worktree.to_string_lossy(),
            ],
        );
        fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();
        let store = dir.path().join("store");
        let mut dirty_record = fixture_record(source.path(), &worktree);
        dirty_record.id = "fixture@dirty".to_string();
        let mut safe_record = fixture_record(
            source.path(),
            &sibling_worktree_path(source.path(), "missing-after-dirty"),
        );
        safe_record.id = "fixture@missing".to_string();
        write_record(&store, &dirty_record).unwrap();
        write_record(&store, &safe_record).unwrap();

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: false,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();
        let updated = read_record(&store, &dirty_record.id).unwrap();

        assert_eq!(output.counts.candidates, 2);
        assert_eq!(output.counts.removed, 1);
        assert_eq!(output.counts.skipped, 1);
        assert_eq!(output.skipped[0].record.id, dirty_record.id);
        assert!(output.skipped[0]
            .reasons
            .iter()
            .any(|reason| reason == "dirty worktree"));
        assert_eq!(output.removed[0].record.id, safe_record.id);
        assert_eq!(updated.state, TaskWorktreeState::Active);
        assert!(worktree.exists());
    }

    #[test]
    fn cleanup_force_still_skips_primary_checkout_hard_gate() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let store = dir.path().join("store");
        let record = fixture_record(source.path(), source.path());
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: true,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        assert_eq!(output.counts.candidates, 1);
        assert_eq!(output.counts.removed, 0);
        assert_eq!(output.counts.skipped, 1);
        assert!(output.skipped[0]
            .reasons
            .iter()
            .any(|reason| reason == "refuses to remove primary checkout"));
        assert_eq!(updated.state, TaskWorktreeState::Active);
        assert!(source.path().exists());
    }

    #[test]
    fn cleanup_dry_run_reports_safe_candidate_without_removing() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "dry-run-cleanup");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "dry-run-cleanup",
                &worktree.to_string_lossy(),
            ],
        );
        let store = dir.path().join("store");
        let record = fixture_record(source.path(), &worktree);
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: false,
                dry_run: true,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        assert!(output.dry_run);
        assert_eq!(output.counts.candidates, 1);
        assert_eq!(output.counts.removed, 0);
        assert_eq!(output.counts.skipped, 0);
        assert_eq!(output.candidates[0].record.id, record.id);
        assert!(!output.candidates[0].safety.worktree_missing);
        assert_eq!(updated.state, TaskWorktreeState::Active);
        assert!(worktree.exists());
    }

    #[test]
    fn cleanup_force_removes_dirty_worktree_after_homeboy_gates_pass() {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let worktree = sibling_worktree_path(source.path(), "dirty-cleanup-forced");
        run_git(
            source.path(),
            &[
                "worktree",
                "add",
                "-b",
                "dirty-cleanup-forced",
                &worktree.to_string_lossy(),
            ],
        );
        fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();
        let store = dir.path().join("store");
        let record = fixture_record(source.path(), &worktree);
        write_record(&store, &record).unwrap();

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: true,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        assert_eq!(output.counts.candidates, 1);
        assert_eq!(output.counts.removed, 1);
        assert_eq!(output.counts.skipped, 0);
        assert!(output.removed[0].removed);
        assert!(output.removed[0].safety.dirty);
        assert_eq!(updated.state, TaskWorktreeState::Removed);
        assert!(!worktree.exists());
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
        }
    }

    #[test]
    fn queue_create_dry_run_returns_queued_rows_with_exact_homeboy_commands() {
        let output = queue_create(queue_options()).unwrap();

        assert_eq!(output.schema, "homeboy/worktree-queue-create/v1");
        assert_eq!(output.rows.len(), 2);
        assert_eq!(output.rows[0].status, WorktreeQueueCreateStatus::Queued);
        assert_eq!(output.rows[0].handle, "homeboy@cook-one");
        assert_eq!(
            output.rows[0].command,
            vec![
                "homeboy",
                "worktree",
                "create",
                "homeboy",
                "--branch",
                "cook/one",
                "--from",
                "origin/main",
                "--task-url",
                "https://github.com/Extra-Chill/homeboy/issues/5786",
            ]
        );
    }

    #[test]
    fn queue_create_records_successful_homeboy_worktree() {
        use crate::test_support::with_isolated_home;

        with_isolated_home(|home| {
            let parent = home.path().join("Developer");
            let source = parent.join("queue-fixture");
            let worktree_path = parent.join("queue-fixture@cook-one");
            if parent.exists() {
                fs::remove_dir_all(&parent).unwrap();
            }
            fs::create_dir_all(&parent).unwrap();
            fs::create_dir_all(&source).unwrap();
            run_git(&source, &["init", "-q"]);
            run_git(&source, &["config", "user.email", "homeboy@example.com"]);
            run_git(&source, &["config", "user.name", "Homeboy Test"]);
            fs::write(source.join("README.md"), "initial\n").unwrap();
            fs::write(source.join("homeboy.json"), r#"{"id":"queue-fixture"}"#).unwrap();
            run_git(&source, &["add", "."]);
            run_git(&source, &["commit", "-q", "-m", "initial"]);
            write_component_registration(home.path(), "queue-fixture", &source);

            let output = queue_create(WorktreeQueueCreateOptions {
                repo: "queue-fixture".to_string(),
                branches: vec!["cook/one".to_string()],
                from: "HEAD".to_string(),
                task_url: Some("https://github.com/Extra-Chill/homeboy/issues/5924".to_string()),
                task_ref: None,
                dry_run: false,
                retry_after_seconds: 30,
            })
            .unwrap();

            assert_eq!(
                output.rows[0].status,
                WorktreeQueueCreateStatus::Created,
                "queue row failed: {:?}",
                output.rows[0].error
            );
            assert_eq!(output.rows[0].handle, "queue-fixture@cook-one");
            assert!(output.rows[0].path.is_some());
            let record = resolve("queue-fixture@cook-one").expect("queued worktree record");
            assert!(Path::new(&record.worktree_path).exists());
            assert_eq!(
                PathBuf::from(&record.worktree_path).canonicalize().unwrap(),
                worktree_path.canonicalize().unwrap()
            );
            assert_eq!(record.branch, "cook/one");
            assert_eq!(record.base_ref, "HEAD");
            assert_eq!(
                record.task_url.as_deref(),
                Some("https://github.com/Extra-Chill/homeboy/issues/5924")
            );
        });
    }

    #[test]
    fn queue_create_uses_runner_checkout_when_lab_snapshot_is_not_git_backed() {
        use crate::test_support::with_isolated_home;

        with_isolated_home(|home| {
            let runner_root = home.path().join("Developer");
            let source = runner_root.join("lab-fixture");
            let snapshot = runner_root.join("_lab_workspaces/job-123");
            fs::create_dir_all(&source).unwrap();
            fs::create_dir_all(&snapshot).unwrap();
            run_git(&source, &["init", "-q"]);
            run_git(&source, &["config", "user.email", "homeboy@example.com"]);
            run_git(&source, &["config", "user.name", "Homeboy Test"]);
            fs::write(source.join("README.md"), "initial\n").unwrap();
            fs::write(source.join("homeboy.json"), r#"{"id":"lab-fixture"}"#).unwrap();
            fs::write(snapshot.join("homeboy.json"), r#"{"id":"lab-fixture"}"#).unwrap();
            run_git(&source, &["add", "."]);
            run_git(&source, &["commit", "-q", "-m", "initial"]);
            write_component_registration(home.path(), "lab-fixture", &snapshot);
            let _cwd = CurrentDirGuard::set(&snapshot);

            let output = queue_create(WorktreeQueueCreateOptions {
                repo: "lab-fixture".to_string(),
                branches: vec!["cook/lab".to_string()],
                from: "HEAD".to_string(),
                task_url: None,
                task_ref: None,
                dry_run: false,
                retry_after_seconds: 30,
            })
            .unwrap();

            assert_eq!(
                output.rows[0].status,
                WorktreeQueueCreateStatus::Created,
                "queue row failed: {:?}",
                output.rows[0].error
            );
            let record = resolve("lab-fixture@cook-lab").expect("queued worktree record");
            assert_eq!(
                PathBuf::from(record.source_checkout),
                source.canonicalize().unwrap()
            );
            assert!(runner_root.join("lab-fixture@cook-lab").exists());
        });
    }

    struct CurrentDirGuard {
        prior: PathBuf,
    }

    impl CurrentDirGuard {
        fn set(path: &Path) -> Self {
            let prior = std::env::current_dir().unwrap();
            std::env::set_current_dir(path).unwrap();
            Self { prior }
        }
    }

    impl Drop for CurrentDirGuard {
        fn drop(&mut self) {
            std::env::set_current_dir(&self.prior).unwrap();
        }
    }
}
