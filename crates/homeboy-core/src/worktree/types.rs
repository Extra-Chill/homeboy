use crate::workspace_claim::{WorkspaceClaim, WorkspaceIdentity};
use crate::Result;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Canonical, portable identity for a managed task worktree. The registry
/// handle and registered component identify the physical allocation; paths and
/// logical agent-task IDs intentionally do not participate.
pub fn task_worktree_workspace_identity(
    component_id: &str,
    handle: &str,
) -> Result<WorkspaceIdentity> {
    WorkspaceIdentity::new("task-worktree", format!("{component_id}/{handle}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TaskWorktreeState {
    Active,
    Removed,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeLeaseActivity {
    Live,
    Stale,
    Stopped,
}

/// Deterministic local view of the write authority protecting a managed checkout.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeOwnershipProbe {
    pub handle: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub holder: Option<String>,
    pub lifecycle_state: String,
    pub activity: WorktreeLeaseActivity,
    pub heartbeat_fresh: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lease_expires_at_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub live_holders: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CleanupPolicy {
    RemoveWhenSafe,
    PreserveOnFailure,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BranchCleanupIntent {
    #[default]
    DeleteWhenMerged,
    Preserve,
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
    /// Stored for new manifests. Older records deterministically derive the
    /// same value from their stable registry identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_identity: Option<WorkspaceIdentity>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub cleanup_policy: CleanupPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_disposition: Option<String>,
    #[serde(default)]
    pub branch_cleanup_intent: BranchCleanupIntent,
    pub created_at: String,
    pub state: TaskWorktreeState,
    #[serde(default)]
    pub lifecycle_revision: u64,
    /// Immutable terminal evidence is retained on the manifest so reconciliation
    /// remains possible after the controller compacts its run record.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_workspace_authority: Option<TerminalWorkspaceAuthorityProof>,
}

pub const TERMINAL_WORKSPACE_AUTHORITY_SCHEMA: &str = "homeboy/terminal-workspace-authority/v1";
pub const TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY: &str = "terminal-workspace-authority";

/// Versioned, portable evidence that every authority which could own a task
/// workspace observed a terminal outcome. This is evidence, not a time fence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalWorkspaceAuthorityProof {
    pub schema: String,
    pub capability: String,
    pub capability_version: u32,
    pub workspace: WorkspaceIdentity,
    pub task_worktree_id: String,
    pub manifest_revision: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    pub controller_state: String,
    pub controller_version: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_runner_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepted_runner_job_id: Option<String>,
    pub authority_set: Vec<String>,
    /// Deterministic fingerprint of `authority_set`, retained so a replay can
    /// reject configuration drift without consulting historical receipts.
    pub authority_set_fingerprint: String,
    pub observations: Vec<TerminalWorkspaceAuthorityObservation>,
    pub issued_evidence: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TerminalWorkspaceAuthorityObservation {
    pub authority: String,
    pub capability: String,
    pub capability_version: u32,
    pub status: String,
    pub evidence: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runner_job_id: Option<String>,
}

impl TerminalWorkspaceAuthorityProof {
    pub fn exact_for(&self, record: &TaskWorktreeRecord, expected_run_id: Option<&str>) -> bool {
        let Ok(workspace) = record.effective_workspace_identity() else {
            return false;
        };
        let mut authorities = self.authority_set.clone();
        authorities.sort();
        authorities.dedup();
        let controller_terminal = matches!(
            self.controller_state.as_str(),
            "Succeeded"
                | "CandidateRecoverable"
                | "PartialRecoverable"
                | "PartialFailure"
                | "Failed"
                | "Cancelled"
        );
        let accepted_binding_matches =
            self.accepted_runner_id.is_some() == self.accepted_runner_job_id.is_some();
        let observation_bindings_match = self.observations.iter().all(|observation| {
            observation.run_id.as_deref() == expected_run_id
                && (observation.authority == "controller"
                    || observation.runner_job_id == self.accepted_runner_job_id)
        });
        let controller_observation_matches = self.observations.iter().any(|observation| {
            observation.authority == "controller"
                && observation.status == "terminal"
                && observation.runner_job_id.is_none()
        });
        let accepted_observation_matches =
            match (&self.accepted_runner_id, &self.accepted_runner_job_id) {
                (Some(runner_id), Some(job_id)) => self.observations.iter().any(|observation| {
                    observation.authority == *runner_id
                        && observation.runner_job_id.as_deref() == Some(job_id)
                }),
                (None, None) => true,
                _ => false,
            };
        self.schema == TERMINAL_WORKSPACE_AUTHORITY_SCHEMA
            && self.capability == TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY
            && self.capability_version == 1
            && self.workspace == workspace
            && self.task_worktree_id == record.id
            && self.manifest_revision == record.lifecycle_revision
            && self.run_id.as_deref() == expected_run_id
            && self
                .issued_evidence
                .iter()
                .any(|evidence| match expected_run_id {
                    Some(run_id) => evidence == &format!("controller-run:{run_id}"),
                    None => evidence == "controller-no-run-id",
                })
            && controller_terminal
            && accepted_binding_matches
            && authorities == self.authority_set
            && authorities
                .first()
                .is_some_and(|authority| authority == "controller")
            && self.authority_set_fingerprint == authority_set_fingerprint(&authorities)
            && self.observations.len() == authorities.len()
            && self.observations.iter().all(|observation| {
                observation.capability == TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY
                    && observation.capability_version == 1
                    && !observation.authority.trim().is_empty()
                    && matches!(observation.status.as_str(), "terminal" | "absent_terminal")
            })
            && self
                .observations
                .iter()
                .map(|observation| &observation.authority)
                .collect::<std::collections::BTreeSet<_>>()
                == authorities.iter().collect()
            && observation_bindings_match
            && controller_observation_matches
            && accepted_observation_matches
    }
}

pub fn authority_set_fingerprint(authorities: &[String]) -> String {
    // Terminated, not separated: the authority set is variable-length, so a
    // trailing separator after every element is what keeps it unambiguous.
    homeboy_engine_primitives::content_hash::nul_terminated_digest(authorities)
}

impl TaskWorktreeRecord {
    pub fn effective_workspace_identity(&self) -> Result<WorkspaceIdentity> {
        let derived = task_worktree_workspace_identity(&self.component_id, &self.id)?;
        if let Some(identity) = &self.workspace_identity {
            identity.verify()?;
            if identity != &derived {
                return Err(crate::error::Error::validation_invalid_argument(
                    "workspace_identity",
                    "task worktree manifest identity conflicts with its stable handle and component",
                    Some(self.id.clone()),
                    None,
                ));
            }
        }
        Ok(derived)
    }
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
#[allow(clippy::large_enum_variant)] // Registry records are returned by value.
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

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeCreateAction {
    Created,
    Existing,
    Restored,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeCreateEvidence {
    pub task_worktree_id: String,
    pub component_id: String,
    pub source_checkout: String,
    pub worktree_path: String,
    pub branch: String,
    pub workspace_identity: WorkspaceIdentity,
    pub git_registration: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCreateReconciliation {
    pub action: WorktreeCreateAction,
    pub previous: WorktreeCreateEvidence,
    pub current: WorktreeCreateEvidence,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeCreateOutput {
    pub record: TaskWorktreeRecord,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<WorktreeCreateReconciliation>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeAdoptOutput {
    pub record: AdoptedWorkspaceRecord,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeListOutput {
    pub worktrees: Vec<TaskWorktreeRecord>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeInventoryOutput {
    pub schema: &'static str,
    pub authorization: WorktreeInventoryAuthorization,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub apply_refusal: Option<WorktreeInventoryApplyRefusal>,
    /// The task-worktree page starts strictly after this sorted record ID.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
    pub limit: usize,
    pub total: usize,
    pub truncated: bool,
    /// Cross-tab counts describe `records`, never the uninspected registry remainder.
    pub cross_tab_scope: &'static str,
    pub cross_tab: WorktreeInventoryCrossTab,
    pub records: Vec<WorktreeInventoryRecord>,
    pub adopted: WorktreeAdoptedInventoryPage,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeInventoryAuthorization {
    Preview,
    ExplicitApply,
    ApplyRefused,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeInventoryApplyRefusal {
    pub code: &'static str,
    pub mutated_records: usize,
    pub mutation_provenance: &'static str,
    pub required_primitive: &'static str,
    pub message: &'static str,
}

#[derive(Debug, Clone, Serialize, Default, PartialEq, Eq)]
pub struct WorktreeInventoryCrossTab {
    pub active_path_present: usize,
    pub active_path_missing: usize,
    pub removed_path_present: usize,
    pub removed_path_missing: usize,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeInventoryRecord {
    pub record: TaskWorktreeRecord,
    pub path_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub missing_active: Option<MissingActiveWorktree>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconciliation: Option<WorktreeReconciliationResult>,
}

pub trait WorktreeReconciliationAuthority {
    /// Acquires an owner-issued admission fence before the registry write lease.
    fn acquire(&self, record: &TaskWorktreeRecord) -> Result<WorktreeLivenessAuthority>;

    /// Revalidates the opaque fence at the owner. This is intentionally outside
    /// the task-worktree registry lease because it can be a network operation.
    fn validate(&self, _record: &TaskWorktreeRecord, _claim: &WorkspaceClaim) -> Result<bool> {
        Ok(false)
    }

    /// Performs only controller-local checks while the registry write lease is
    /// held. Implementations must not contact runner authorities here.
    fn ready_to_commit(&self, _claim: &WorkspaceClaim) -> bool {
        false
    }

    fn requires_terminal_workspace_authority_proof(&self) -> bool {
        false
    }

    /// Releases the owner-issued fence after the conditional registry mutation.
    fn release(&self, _claim: &WorkspaceClaim) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeLivenessAuthority {
    Terminal {
        claim: WorkspaceClaim,
        provenance: String,
    },
    Live {
        provenance: String,
    },
    Incomplete {
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeReconciliationResult {
    pub action: WorktreeReconciliationAction,
    pub provenance: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeReconciliationAction {
    Reconciled,
    Preserved,
    Refused,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct AdoptedWorkspaceInventoryRecord {
    pub record: AdoptedWorkspaceRecord,
    pub path_exists: bool,
    pub reason: MissingActiveWorktreeReason,
    pub continuation: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeAdoptedInventoryPage {
    pub cursor: Option<String>,
    pub next_cursor: Option<String>,
    pub total: usize,
    pub truncated: bool,
    pub records: Vec<AdoptedWorkspaceInventoryRecord>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct MissingActiveWorktree {
    pub reason: MissingActiveWorktreeReason,
    pub local_evidence: WorktreeInventoryLocalEvidence,
    pub continuation: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeInventoryLocalEvidence {
    pub source_checkout_exists: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_dirty: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unpushed_branch_commits: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MissingActiveWorktreeReason {
    PreserveOnFailure,
    SourceCheckoutUnavailable,
    SourceDirty,
    UnpushedBranch,
    BranchEvidenceUnavailable,
    RequiresAuthoritativeLiveness,
    LiveRun,
    AdoptedWorkspace,
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeInventoryOptions {
    pub limit: usize,
    pub cursor: Option<String>,
    pub adopted_cursor: Option<String>,
    pub apply: bool,
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
    /// Records represented by `candidates`, which cleanup can act on directly.
    pub candidates: usize,
    pub removed: usize,
    pub skipped: usize,
    /// Missing active worktrees that require inventory reconciliation authority
    /// before cleanup can make progress.
    pub reconciliation_blockers: usize,
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
    pub requests: Vec<WorktreeQueueCreateRequest>,
    pub from: String,
    pub dry_run: bool,
    pub retry_after_seconds: u64,
}

#[derive(Debug, Clone)]
pub struct WorktreeQueueCreateRequest {
    pub branch: String,
    pub task_url: Option<String>,
    pub task_ref: Option<String>,
    pub run_id: Option<String>,
    pub provider_lifecycle: Option<crate::worktree_provider::WorktreeProvisionLifecycle>,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<WorktreeQueueCreateFailure>,
}

/// Lossless structured cause for a queue row that failed before creation.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeQueueCreateFailure {
    pub code: String,
    pub classification: String,
    pub phase: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_id: Option<String>,
    pub details: Value,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeQueueCreateStatus {
    Queued,
    WouldCreate,
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
