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
