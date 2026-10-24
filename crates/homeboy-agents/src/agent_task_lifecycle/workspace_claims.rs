//! Local controller authority for portable agent-task workspace claims.

use super::runner_continuation::with_runner_continuation;
use super::*;
use homeboy_core::observation::coordination::{
    CoordinationRecord, ExpectedRevision, RecordOutcome, RecordWrite,
};
use homeboy_core::observation::ObservationStore;
use homeboy_core::workspace_claim::{WorkspaceClaimStore, MAX_WORKSPACE_CLAIM_TTL_MS};
use homeboy_core::worktree::{
    authority_set_fingerprint, MissingActiveWorktreeReason, TaskWorktreeRecord,
    TerminalWorkspaceAuthorityObservation, TerminalWorkspaceAuthorityProof,
    WorktreeAuthorityRefusal, TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY,
    TERMINAL_WORKSPACE_AUTHORITY_SCHEMA,
};
use homeboy_runner_contract::{
    WorkspaceClaim, WorkspaceClaimBinding, WorkspaceIdentity, WorkspaceOwnerLease,
};
use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::sync::atomic::{AtomicBool, Ordering};

pub const LOCAL_WORKSPACE_CLAIM_TTL_MS: u64 = MAX_WORKSPACE_CLAIM_TTL_MS;
pub const LOCAL_WORKSPACE_OWNER_LEASE_TTL_MS: u64 = MAX_WORKSPACE_CLAIM_TTL_MS;
const COMMIT_SAFETY_MARGIN_MS: u64 = 1_000;

fn commit_budget_ms(now_ms: u64, claims: impl Iterator<Item = u64>) -> u64 {
    claims
        .map(|expires_at_ms| expires_at_ms.saturating_sub(now_ms))
        .min()
        .unwrap_or(0)
        .saturating_sub(COMMIT_SAFETY_MARGIN_MS)
}

/// Resolve controller-local terminal authority for reconciling one task
/// worktree. Every read is controller-local and bounded; no claim or manifest
/// is mutated.
///
/// A runner can hold a claim or owner lease on a workspace identity only
/// through (a) a dispatch of the owning run, which the controller records as a
/// handoff, runner binding, or submission intent before crossing the runner
/// boundary, or (b) a composite claim acquisition, whose write-ahead intent or
/// cleanup receipt the controller keeps until every component is released. When
/// neither exists the controller-local claim alone fences the workspace: any
/// later dispatch must first register a local owner, which that claim refuses.
/// Otherwise the record needs a remote claim release that bounded inventory
/// does not perform, and only that record is preserved.
pub fn local_terminal_workspace_authority(
    record: &TaskWorktreeRecord,
) -> std::result::Result<Box<TerminalWorkspaceAuthorityProof>, WorktreeAuthorityRefusal> {
    terminal_workspace_authority(record, None)
}

/// [`local_terminal_workspace_authority`] re-checked by the holder of the
/// reconciliation claim on the record's workspace, at mutation time.
///
/// Holding that claim writes its own acquisition intent, and while the claim
/// is live no other acquisition can create an intent or cleanup receipt for the
/// workspace, so the holder's re-check does not treat that intent as an
/// outstanding foreign claim. Every other piece of evidence is re-read.
pub fn local_terminal_workspace_authority_under_claim(
    record: &TaskWorktreeRecord,
    held: &CompositeWorkspaceClaim,
) -> std::result::Result<Box<TerminalWorkspaceAuthorityProof>, WorktreeAuthorityRefusal> {
    terminal_workspace_authority(record, Some(held))
}

fn terminal_workspace_authority(
    record: &TaskWorktreeRecord,
    held: Option<&CompositeWorkspaceClaim>,
) -> std::result::Result<Box<TerminalWorkspaceAuthorityProof>, WorktreeAuthorityRefusal> {
    let unavailable = MissingActiveWorktreeReason::TerminalAuthorityUnavailable;
    let workspace = record
        .effective_workspace_identity()
        .map_err(|error| refuse(unavailable.clone(), &error.message))?;
    let holds_workspace_claim = held.is_some_and(|claim| claim.workspace == workspace);
    if !holds_workspace_claim && composite_claim_receipt_outstanding(&workspace) {
        return Err(refuse(
            MissingActiveWorktreeReason::RemoteClaimReleaseUnsupported,
            "workspace has an unreleased composite claim intent or cleanup receipt",
        ));
    }
    let Some(run_id) = record.run_id.as_deref() else {
        return Err(refuse(
            unavailable,
            "task worktree has no owning run to attest terminality",
        ));
    };
    let lifecycle_store = AgentTaskLifecycleStore::from_current_environment()
        .map_err(|error| refuse(unavailable.clone(), &error.message))?;
    let terminal = match lifecycle_store.read_record(run_id) {
        Ok(controller) => controller_terminal_run(&lifecycle_store, &controller)?,
        Err(_) if matches!(lifecycle_store.record_exists(run_id), Ok(false)) => {
            orphaned_terminal_run(&lifecycle_store, run_id)?
        }
        Err(_) => {
            return Err(refuse(
                unavailable,
                "durable controller run is present but unreadable",
            ))
        }
    };
    let authority_set = vec!["controller".to_string()];
    let proof = TerminalWorkspaceAuthorityProof {
        schema: TERMINAL_WORKSPACE_AUTHORITY_SCHEMA.into(),
        capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.into(),
        capability_version: 1,
        workspace,
        task_worktree_id: record.id.clone(),
        manifest_revision: record.lifecycle_revision,
        run_id: Some(run_id.into()),
        controller_state: format!("{:?}", terminal.state),
        controller_version: terminal.revision,
        accepted_runner_id: None,
        accepted_runner_job_id: None,
        authority_set_fingerprint: authority_set_fingerprint(&authority_set),
        authority_set,
        observations: vec![TerminalWorkspaceAuthorityObservation {
            authority: "controller".into(),
            capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.into(),
            capability_version: 1,
            status: "terminal".into(),
            evidence: terminal.evidence,
            run_id: Some(run_id.into()),
            runner_job_id: None,
        }],
        issued_evidence: vec![format!("controller-run:{run_id}")],
    };
    // A manifest receipt is immutable; one binding other evidence is stale.
    if record
        .terminal_workspace_authority
        .as_ref()
        .is_some_and(|cached| *cached != proof)
    {
        return Err(refuse(
            unavailable,
            "cached terminal authority receipt binds other controller evidence",
        ));
    }
    Ok(Box::new(proof))
}

/// The owning run's terminal state and the durable evidence that attests it.
struct TerminalRun {
    state: AgentTaskRunState,
    revision: u64,
    evidence: String,
}

fn refuse(reason: MissingActiveWorktreeReason, detail: &str) -> WorktreeAuthorityRefusal {
    WorktreeAuthorityRefusal {
        reason,
        detail: detail.to_string(),
    }
}

/// Terminal authority from the owning run's durable lifecycle record.
fn controller_terminal_run(
    lifecycle_store: &AgentTaskLifecycleStore,
    controller: &AgentTaskRunRecord,
) -> std::result::Result<TerminalRun, WorktreeAuthorityRefusal> {
    if controller.lab_handoff.is_some()
        || controller.is_runner_backed()
        || controller
            .metadata
            .get("runner_submission_intent")
            .is_some()
    {
        return Err(refuse(
            MissingActiveWorktreeReason::RemoteClaimReleaseUnsupported,
            "owning run was dispatched to a runner that may hold a workspace claim",
        ));
    }
    if !controller.state().is_terminal() || !controller.run_state_projections_agree() {
        return Err(refuse(
            MissingActiveWorktreeReason::LiveRun,
            "durable controller run is active or has conflicting state projection",
        ));
    }
    if controller
        .workspace_owner_lease
        .as_ref()
        .is_some_and(|lease| {
            validate_local_workspace_owner_in_store(&lifecycle_store.workspace_claim_store(), lease)
                .unwrap_or(true)
        })
    {
        return Err(refuse(
            MissingActiveWorktreeReason::LiveRun,
            "controller still has a live local workspace owner lease",
        ));
    }
    Ok(TerminalRun {
        state: controller.state(),
        revision: controller.workspace_lifecycle_revision,
        evidence: format!(
            "{:?}@{}",
            controller.state(),
            controller.workspace_lifecycle_revision
        ),
    })
}

/// Terminal authority for a run whose lifecycle record no longer exists.
///
/// Task worktrees outlived their runs' lifecycle records before run retention
/// kept the owners of active worktrees (#15844). Such a run is proven terminal
/// only by the aggregate it persisted at completion, and only when its Lab
/// handoff lock is absent: every runner dispatch path takes that lock, so its
/// presence (or an unreadable lock path) means a runner may still hold a claim.
/// A live local owner lease is still refused when the reconciliation claim is
/// acquired.
fn orphaned_terminal_run(
    lifecycle_store: &AgentTaskLifecycleStore,
    run_id: &str,
) -> std::result::Result<TerminalRun, WorktreeAuthorityRefusal> {
    if !matches!(
        lifecycle_store.lab_handoff_lock_path(run_id).try_exists(),
        Ok(false)
    ) {
        return Err(refuse(
            MissingActiveWorktreeReason::RemoteClaimReleaseUnsupported,
            "owning run's lifecycle record is gone and its Lab handoff lock shows it may have reached a runner",
        ));
    }
    let aggregate = lifecycle_store
        .read_aggregate_bounded(run_id)
        .map_err(|_| {
            refuse(
                MissingActiveWorktreeReason::TerminalAuthorityUnavailable,
                "durable controller run is missing and left no terminal aggregate",
            )
        })?;
    let state = super::conversion::run_state_for_aggregate(&aggregate);
    Ok(TerminalRun {
        state,
        revision: 0,
        evidence: format!("{state:?}@aggregate"),
    })
}

/// The complete authority set for a workspace mutation. Each remote component
/// has its own opaque token; callers must validate and release every component.
#[derive(Debug, Clone)]
pub struct CompositeWorkspaceClaim {
    pub workspace: WorkspaceIdentity,
    /// Controller-owned identity for this composition operation. It is not an
    /// authority epoch: each component retains the epoch its own store issued.
    pub generation: u64,
    /// Monotonic controller-side deadline. It starts before any component is
    /// acquired and is intentionally independent of runner wall clocks.
    pub commit_deadline: std::time::Instant,
    pub local: WorkspaceClaim,
    pub local_released: bool,
    pub runners: Vec<CompositeWorkspaceClaimComponent>,
}

#[derive(Debug, Clone)]
pub struct CompositeWorkspaceClaimComponent {
    pub runner_id: String,
    pub claim: WorkspaceClaim,
    pub released: bool,
}

/// Evidence for a component which remained live after an acquisition rollback.
#[derive(Debug, Clone)]
pub struct CompositeWorkspaceClaimRollbackFailure {
    pub component: String,
    pub error: Error,
}

/// An acquisition failure preserves its original error and, when rollback was
/// incomplete, the exact claim receipt the caller must retry releasing.
#[derive(Debug, Clone)]
pub struct CompositeWorkspaceClaimAcquisitionFailure {
    pub primary: Error,
    pub rollback_failures: Vec<CompositeWorkspaceClaimRollbackFailure>,
    pub cleanup: Option<CompositeWorkspaceClaim>,
}

pub const PENDING_COMPOSITE_WORKSPACE_CLEANUP_SCHEMA: &str =
    "homeboy/pending-composite-workspace-cleanup/v1";
pub const COMPOSITE_ACQUISITION_INTENT_SCHEMA: &str = "homeboy/composite-acquisition-intent/v1";

/// Coordination namespace of write-ahead composite acquisition intents, one
/// record per workspace digest. The record state is the intent state.
const COMPOSITE_ACQUISITION_NAMESPACE: &str = "composite-workspace-acquisition";
const INTENT_STATE_ACQUIRING: &str = "acquiring";
const INTENT_STATE_ACTIVE: &str = "active";

/// Coordination namespace of composite cleanup receipts, one record per
/// recovery id. A receipt that fails verification moves to `quarantined`.
const COMPOSITE_CLEANUP_NAMESPACE: &str = "composite-workspace-cleanup";
const CLEANUP_STATE_PENDING: &str = "pending";
const CLEANUP_STATE_QUARANTINED: &str = "quarantined";

#[cfg(test)]
static FAIL_NEXT_PENDING_COMPOSITE_CLEANUP_WRITE: AtomicBool = AtomicBool::new(false);

/// A token-free write-ahead marker for one workspace reconciliation. It is
/// retained while the process owns the composite claim so a crash cannot turn
/// an acquired fence into an untracked one.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct CompositeAcquisitionIntent {
    schema: String,
    recovery_id: String,
    workspace: WorkspaceIdentity,
    started_at_ms: u64,
    requested_ttl_ms: u64,
    state: String,
}

/// Owner-only recovery state for a failed composite acquisition or release.
/// This deliberately contains the exact opaque receipts needed to replay an
/// idempotent release; only summaries leave the lifecycle boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCompositeWorkspaceCleanup {
    pub schema: String,
    pub recovery_id: String,
    pub created_at: String,
    pub workspace: WorkspaceIdentity,
    pub generation: u64,
    pub local: WorkspaceClaim,
    pub local_released: bool,
    pub runners: Vec<PendingCompositeWorkspaceCleanupComponent>,
    pub attempt_count: u32,
    pub last_error: PendingCompositeWorkspaceCleanupError,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCompositeWorkspaceCleanupComponent {
    pub runner_id: String,
    pub claim: WorkspaceClaim,
    pub released: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PendingCompositeWorkspaceCleanupError {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct QuarantinedCompositeWorkspaceCleanup {
    recovery_id: String,
    workspace: Option<WorkspaceIdentity>,
    reason: String,
}

#[derive(Debug, Clone)]
pub enum CompositeWorkspaceCleanupStatus {
    Released,
    Pending {
        recovery_id: String,
        attempt_count: u32,
        failures: Vec<String>,
    },
    Quarantined {
        recovery_id: String,
        reason: String,
    },
}

impl CompositeWorkspaceCleanupStatus {
    pub fn recovery_ref(&self) -> Option<&str> {
        match self {
            Self::Released => None,
            Self::Pending { recovery_id, .. } | Self::Quarantined { recovery_id, .. } => {
                Some(recovery_id)
            }
        }
    }

    pub fn public_summary(&self) -> String {
        match self {
            Self::Released => "composite cleanup released".into(),
            Self::Pending {
                recovery_id,
                attempt_count,
                ..
            } => format!(
                "composite cleanup remains pending (recovery_ref={recovery_id}, attempts={attempt_count})"
            ),
            Self::Quarantined { recovery_id, reason } => format!(
                "composite cleanup is quarantined (recovery_ref={recovery_id}): {reason}"
            ),
        }
    }
}

impl From<Error> for CompositeWorkspaceClaimAcquisitionFailure {
    fn from(primary: Error) -> Self {
        Self {
            primary,
            rollback_failures: Vec::new(),
            cleanup: None,
        }
    }
}

struct CompositeWorkspaceClaimAcquisitionGuard {
    workspace: WorkspaceIdentity,
    generation: u64,
    acquired_at: std::time::Instant,
    acquired_at_ms: u64,
    local: WorkspaceClaim,
    runners: Vec<CompositeWorkspaceClaimComponent>,
}

impl CompositeWorkspaceClaimAcquisitionGuard {
    fn new(workspace: WorkspaceIdentity, generation: u64, local: WorkspaceClaim) -> Self {
        Self {
            workspace,
            generation,
            acquired_at: std::time::Instant::now(),
            acquired_at_ms: now_ms(),
            local,
            runners: Vec::new(),
        }
    }

    fn finish(self) -> CompositeWorkspaceClaim {
        let budget_ms = commit_budget_ms(
            self.acquired_at_ms,
            std::iter::once(self.local.expires_at_ms).chain(
                self.runners
                    .iter()
                    .map(|component| component.claim.expires_at_ms),
            ),
        );
        CompositeWorkspaceClaim {
            workspace: self.workspace,
            generation: self.generation,
            commit_deadline: self.acquired_at + std::time::Duration::from_millis(budget_ms),
            local: self.local,
            local_released: false,
            runners: self.runners,
        }
    }

    fn fail(
        mut self,
        mut primary: Error,
        deadline: std::time::Instant,
    ) -> CompositeWorkspaceClaimAcquisitionFailure {
        let mut rollback_failures = Vec::new();
        if let Err(error) = release_local_workspace_claim(&self.local) {
            rollback_failures.push(CompositeWorkspaceClaimRollbackFailure {
                component: "local".into(),
                error,
            });
        } else {
            // A retained receipt must describe only components that still need
            // cleanup, so successful releases are recorded before returning it.
            // `local_released` is populated below on the constructed receipt.
        }
        let local_released = rollback_failures
            .iter()
            .all(|failure| failure.component != "local");
        for component in &mut self.runners {
            if let Err(error) = with_runner_continuation(|provider| {
                provider.release_workspace_claim(&component.runner_id, &component.claim, deadline)
            }) {
                rollback_failures.push(CompositeWorkspaceClaimRollbackFailure {
                    component: component.runner_id.clone(),
                    error,
                });
            } else {
                component.released = true;
            }
        }
        if !rollback_failures.is_empty() {
            primary.retryable = Some(true);
            primary.details["workspace_claim_composite_rollback"] =
                serde_json::json!(rollback_failures
                    .iter()
                    .map(|failure| serde_json::json!({
                        "component": failure.component,
                        "code": failure.error.code.as_str(),
                        "message": failure.error.message,
                    }))
                    .collect::<Vec<_>>());
        }
        CompositeWorkspaceClaimAcquisitionFailure {
            primary,
            cleanup: (!rollback_failures.is_empty()).then_some(CompositeWorkspaceClaim {
                workspace: self.workspace,
                generation: self.generation,
                commit_deadline: self.acquired_at,
                local: self.local,
                local_released,
                runners: self.runners,
            }),
            rollback_failures,
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "failure retains all acquired authority receipts for deterministic rollback"
)]
pub fn acquire_composite_workspace_claim(
    workspace: WorkspaceIdentity,
    generation: u64,
) -> std::result::Result<CompositeWorkspaceClaim, CompositeWorkspaceClaimAcquisitionFailure> {
    acquire_composite_workspace_claim_until(
        workspace,
        generation,
        CompositeClaimAuthorities::ConfiguredRunners,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    )
}

/// Which authorities a composite claim fences.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompositeClaimAuthorities {
    /// Every configured runner authority contributes a component claim.
    ConfiguredRunners,
    /// Controller-local evidence proved no runner can hold this workspace
    /// (see [`local_terminal_workspace_authority`]).
    ControllerOnly,
}

#[expect(
    clippy::result_large_err,
    reason = "failure retains all acquired authority receipts for deterministic rollback"
)]
pub fn acquire_composite_workspace_claim_until(
    workspace: WorkspaceIdentity,
    generation: u64,
    authorities: CompositeClaimAuthorities,
    deadline: std::time::Instant,
) -> std::result::Result<CompositeWorkspaceClaim, CompositeWorkspaceClaimAcquisitionFailure> {
    if std::time::Instant::now() >= deadline {
        return Err(CompositeWorkspaceClaimAcquisitionFailure::from(
            composite_error("workspace claim acquisition deadline exhausted", Vec::new()),
        ));
    }
    if let Some(status) = composite_acquisition_intent_for_workspace(&workspace) {
        let mut failure = CompositeWorkspaceClaimAcquisitionFailure::from(composite_error(
            "workspace has unresolved composite acquisition intent",
            Vec::new(),
        ));
        failure.primary.retryable = Some(true);
        failure.primary.details["workspace_claim_composite_cleanup"] = serde_json::json!({
            "status": status.public_summary(),
            "recovery_ref": status.recovery_ref(),
        });
        return Err(failure);
    }
    if let Some(status) = pending_composite_cleanup_for_workspace(&workspace) {
        let mut failure = CompositeWorkspaceClaimAcquisitionFailure::from(composite_error(
            "workspace has unresolved composite cleanup",
            Vec::new(),
        ));
        failure.primary.retryable = Some(true);
        failure.primary.details["workspace_claim_composite_cleanup"] = serde_json::json!({
            "status": status.public_summary(),
            "recovery_ref": status.recovery_ref(),
        });
        return Err(failure);
    }
    let intent = CompositeAcquisitionIntent {
        schema: COMPOSITE_ACQUISITION_INTENT_SCHEMA.into(),
        recovery_id: uuid::Uuid::new_v4().to_string(),
        workspace: workspace.clone(),
        started_at_ms: now_ms(),
        requested_ttl_ms: LOCAL_WORKSPACE_CLAIM_TTL_MS,
        state: INTENT_STATE_ACQUIRING.into(),
    };
    let intent_revision = match open_composite_store().and_then(|store| {
        write_composite_acquisition_intent(&store, &intent, ExpectedRevision::Absent)
            .map(|revision| (store, revision))
    }) {
        Ok(written) => written,
        Err(error) => return Err(CompositeWorkspaceClaimAcquisitionFailure::from(error)),
    };
    let (store, intent_revision) = intent_revision;
    match acquire_composite_workspace_claim_unrecovered(
        workspace,
        generation,
        authorities,
        deadline,
    ) {
        Ok(claim) => {
            let mut active = intent;
            active.state = INTENT_STATE_ACTIVE.into();
            if let Err(error) = write_composite_acquisition_intent(
                &store,
                &active,
                ExpectedRevision::Revision(intent_revision),
            ) {
                let status =
                    persist_and_retry_composite_workspace_cleanup_until(claim, &error, deadline);
                let mut failure = CompositeWorkspaceClaimAcquisitionFailure::from(error);
                failure.primary.retryable = Some(true);
                failure.primary.details["workspace_claim_composite_cleanup"] = serde_json::json!({
                    "status": status.public_summary(),
                    "recovery_ref": status.recovery_ref(),
                });
                return Err(failure);
            }
            Ok(claim)
        }
        Err(mut failure) => {
            if let Some(cleanup) = failure.cleanup.take() {
                let status = persist_and_retry_composite_workspace_cleanup_until(
                    cleanup,
                    &failure.primary,
                    deadline,
                );
                failure.primary.retryable = Some(true);
                failure.primary.details["workspace_claim_composite_cleanup"] = serde_json::json!({
                    "status": status.public_summary(),
                    "recovery_ref": status.recovery_ref(),
                });
            } else {
                // Every acquired component was released, so the intent is no
                // longer a crash-recovery obligation.
                let _ = remove_composite_acquisition_intent(
                    &store,
                    &intent.workspace,
                    Some(intent_revision),
                );
            }
            Err(failure)
        }
    }
}

#[expect(
    clippy::result_large_err,
    reason = "failure retains all acquired authority receipts for deterministic rollback"
)]
fn acquire_composite_workspace_claim_unrecovered(
    workspace: WorkspaceIdentity,
    generation: u64,
    authorities: CompositeClaimAuthorities,
    deadline: std::time::Instant,
) -> std::result::Result<CompositeWorkspaceClaim, CompositeWorkspaceClaimAcquisitionFailure> {
    workspace.verify()?;
    let local = acquire_local_workspace_claim(workspace.clone(), generation)?;
    let mut guard =
        CompositeWorkspaceClaimAcquisitionGuard::new(workspace.clone(), generation, local);
    let runner_ids = match authorities {
        CompositeClaimAuthorities::ControllerOnly => Vec::new(),
        CompositeClaimAuthorities::ConfiguredRunners => {
            match with_runner_continuation(|provider| provider.workspace_claim_runner_ids()) {
                Ok(runner_ids) => runner_ids,
                Err(error) => return Err(guard.fail(error, deadline)),
            }
        }
    };
    let mut unique_runner_ids = std::collections::BTreeSet::new();
    for runner_id in &runner_ids {
        if runner_id.trim().is_empty() || runner_id != runner_id.trim() || runner_id == "controller"
        {
            return Err(guard.fail(
                Error::validation_invalid_argument(
                    "workspace_claim_runner_ids",
                    "configured workspace claim runner id is malformed",
                    Some(runner_id.clone()),
                    None,
                ),
                deadline,
            ));
        }
        if !unique_runner_ids.insert(runner_id) {
            return Err(guard.fail(
                Error::validation_invalid_argument(
                    "workspace_claim_runner_ids",
                    "configured workspace claim runner ids are not unique",
                    Some(runner_id.clone()),
                    None,
                ),
                deadline,
            ));
        }
    }
    if !runner_ids.is_empty()
        && !with_runner_continuation(|provider| provider.supports_workspace_claims())
    {
        return Err(guard.fail(
            composite_error(
                "workspace claim provider does not support configured remote authorities",
                Vec::new(),
            ),
            deadline,
        ));
    }
    for runner_id in runner_ids {
        let acquired = match with_runner_continuation(|provider| {
            provider.acquire_workspace_claim(&runner_id, workspace.clone(), generation, deadline)
        }) {
            Ok(claim) => claim,
            Err(error) => return Err(guard.fail(error, deadline)),
        };
        match acquired {
            claim if claim.workspace == workspace => {
                guard.runners.push(CompositeWorkspaceClaimComponent {
                    runner_id,
                    claim,
                    released: false,
                });
            }
            _ => {
                return Err(guard.fail(
                    Error::validation_invalid_argument(
                        "workspace_claim",
                        "runner returned a workspace claim for another workspace",
                        Some(runner_id),
                        None,
                    ),
                    deadline,
                ));
            }
        }
    }
    Ok(guard.finish())
}

/// This is deliberately offline: remote validation belongs before acquiring
/// the registry lease. The retained components are checked by exact shape,
/// token, identity and the monotonic acquisition budget only.
pub fn composite_workspace_claim_ready_to_commit(claim: &CompositeWorkspaceClaim) -> bool {
    if std::time::Instant::now() >= claim.commit_deadline
        || claim.local.workspace != claim.workspace
        || claim.local.verify_shape(0).is_err()
    {
        return false;
    }
    claim.runners.iter().all(|component| {
        !component.runner_id.trim().is_empty()
            && component.claim.workspace == claim.workspace
            && component.claim.verify_shape(0).is_ok()
    })
}

pub fn validate_composite_workspace_claim(claim: &CompositeWorkspaceClaim) -> Result<bool> {
    validate_composite_workspace_claim_until(
        claim,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    )
}

pub fn validate_composite_workspace_claim_until(
    claim: &CompositeWorkspaceClaim,
    deadline: std::time::Instant,
) -> Result<bool> {
    claim.workspace.verify()?;
    if claim.local.workspace != claim.workspace || !validate_local_workspace_claim(&claim.local)? {
        return Ok(false);
    }
    for component in &claim.runners {
        if component.runner_id.trim().is_empty()
            || component.claim.workspace != claim.workspace
            || !with_runner_continuation(|provider| {
                provider.validate_workspace_claim(&component.runner_id, &component.claim, deadline)
            })?
        {
            return Ok(false);
        }
    }
    Ok(true)
}

pub enum CompositeWorkspaceClaimRelease {
    Released,
    Partial { failures: Vec<String> },
}

pub fn release_composite_workspace_claim(
    claim: &mut CompositeWorkspaceClaim,
) -> Result<CompositeWorkspaceClaimRelease> {
    release_composite_workspace_claim_until(
        claim,
        std::time::Instant::now() + std::time::Duration::from_secs(30),
    )
}

pub fn release_composite_workspace_claim_until(
    claim: &mut CompositeWorkspaceClaim,
    deadline: std::time::Instant,
) -> Result<CompositeWorkspaceClaimRelease> {
    let mut failures = Vec::new();
    // Release the primary inventory authority first; retries skip evidence of a
    // successful release and fan out only to unresolved remote components.
    if !claim.local_released {
        match release_local_workspace_claim(&claim.local) {
            Ok(()) => claim.local_released = true,
            Err(error) => failures.push(format!("local: {}", error.message)),
        }
    }
    for component in &mut claim.runners {
        if component.released {
            continue;
        }
        if let Err(error) = with_runner_continuation(|provider| {
            provider.release_workspace_claim(&component.runner_id, &component.claim, deadline)
        }) {
            failures.push(format!("{}: {}", component.runner_id, error.message));
        } else {
            component.released = true;
        }
    }
    if failures.is_empty() {
        remove_composite_acquisition_intent(&open_composite_store()?, &claim.workspace, None)?;
        Ok(CompositeWorkspaceClaimRelease::Released)
    } else {
        Ok(CompositeWorkspaceClaimRelease::Partial { failures })
    }
}

/// Persist an exact composite receipt before retrying its idempotent release.
/// The returned status is safe to surface to users: opaque tokens remain only
/// in the owner-only receipt.
pub fn persist_and_retry_composite_workspace_cleanup(
    claim: CompositeWorkspaceClaim,
    error: &Error,
) -> CompositeWorkspaceCleanupStatus {
    let pending = PendingCompositeWorkspaceCleanup {
        schema: PENDING_COMPOSITE_WORKSPACE_CLEANUP_SCHEMA.into(),
        recovery_id: uuid::Uuid::new_v4().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        workspace: claim.workspace,
        generation: claim.generation,
        local: claim.local,
        local_released: claim.local_released,
        runners: claim
            .runners
            .into_iter()
            .map(|component| PendingCompositeWorkspaceCleanupComponent {
                runner_id: component.runner_id,
                claim: component.claim,
                released: component.released,
            })
            .collect(),
        attempt_count: 0,
        last_error: PendingCompositeWorkspaceCleanupError {
            code: error.code.as_str().into(),
            message: error.message.clone(),
        },
    };
    let recovery_id = pending.recovery_id.clone();
    let store = match open_composite_store().and_then(|store| {
        write_pending_composite_cleanup(&store, &pending, ExpectedRevision::Absent)?;
        Ok(store)
    }) {
        Ok(store) => store,
        Err(error) => {
            return CompositeWorkspaceCleanupStatus::Quarantined {
                recovery_id,
                reason: format!("could not persist cleanup receipt: {}", error.message),
            }
        }
    };
    // The exact receipt is durable now; removing the token-free intent cannot
    // reopen acquisition because the receipt itself remains identity-scoped.
    if let Err(error) = remove_composite_acquisition_intent(&store, &pending.workspace, None) {
        return CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id,
            reason: format!(
                "could not finalize cleanup receipt replacement: {}",
                error.message
            ),
        };
    }
    retry_pending_composite_workspace_cleanup(&pending.recovery_id).unwrap_or_else(|error| {
        CompositeWorkspaceCleanupStatus::Pending {
            recovery_id: pending.recovery_id,
            attempt_count: pending.attempt_count,
            failures: vec![error.message],
        }
    })
}

/// Persist a cleanup receipt within a caller-owned operation budget. It never
/// starts a remote release: an expired apply page leaves the receipt for the
/// normal recovery worker rather than opening a fresh release window.
pub fn persist_and_retry_composite_workspace_cleanup_until(
    claim: CompositeWorkspaceClaim,
    error: &Error,
    _deadline: std::time::Instant,
) -> CompositeWorkspaceCleanupStatus {
    let pending = PendingCompositeWorkspaceCleanup {
        schema: PENDING_COMPOSITE_WORKSPACE_CLEANUP_SCHEMA.into(),
        recovery_id: uuid::Uuid::new_v4().to_string(),
        created_at: chrono::Utc::now().to_rfc3339(),
        workspace: claim.workspace,
        generation: claim.generation,
        local: claim.local,
        local_released: claim.local_released,
        runners: claim
            .runners
            .into_iter()
            .map(|component| PendingCompositeWorkspaceCleanupComponent {
                runner_id: component.runner_id,
                claim: component.claim,
                released: component.released,
            })
            .collect(),
        attempt_count: 0,
        last_error: PendingCompositeWorkspaceCleanupError {
            code: error.code.as_str().into(),
            message: error.message.clone(),
        },
    };
    let recovery_id = pending.recovery_id.clone();
    match open_composite_store().and_then(|store| {
        write_pending_composite_cleanup(&store, &pending, ExpectedRevision::Absent)
    }) {
        Ok(_) => CompositeWorkspaceCleanupStatus::Pending {
            recovery_id,
            attempt_count: 0,
            failures: vec!["reconciliation deadline exhausted before release".into()],
        },
        Err(error) => CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id,
            reason: format!("could not persist cleanup receipt: {}", error.message),
        },
    }
}

/// Explicit lifecycle recovery API. Every pending receipt is replayed at most
/// `limit` times; completed receipts are removed atomically after all component
/// releases have reported success.
pub fn retry_pending_composite_workspace_cleanups(
    limit: usize,
) -> Result<Vec<CompositeWorkspaceCleanupStatus>> {
    let entries = read_pending_composite_cleanups(&open_composite_store()?)?;
    entries
        .into_iter()
        .take(limit)
        .map(|(_, pending)| retry_pending_composite_workspace_cleanup(&pending.recovery_id))
        .collect()
}

/// Replay one receipt. Its record revision is the concurrency guard: a retry
/// that loses the compare-and-set to another retry reports the stored state
/// instead of overwriting it, and component releases are idempotent.
fn retry_pending_composite_workspace_cleanup(
    recovery_id: &str,
) -> Result<CompositeWorkspaceCleanupStatus> {
    let store = open_composite_store()?;
    let Some((revision, mut pending)) = read_pending_composite_cleanups(&store)?
        .into_iter()
        .find(|(_, pending)| pending.recovery_id == recovery_id)
    else {
        return Ok(CompositeWorkspaceCleanupStatus::Released);
    };
    let mut claim = CompositeWorkspaceClaim {
        workspace: pending.workspace.clone(),
        generation: pending.generation,
        // A cleanup receipt is never committed, so its old commit deadline is
        // irrelevant to an idempotent release.
        commit_deadline: std::time::Instant::now(),
        local: pending.local.clone(),
        local_released: pending.local_released,
        runners: pending
            .runners
            .iter()
            .map(|component| CompositeWorkspaceClaimComponent {
                runner_id: component.runner_id.clone(),
                claim: component.claim.clone(),
                released: component.released,
            })
            .collect(),
    };
    match release_composite_workspace_claim(&mut claim)? {
        CompositeWorkspaceClaimRelease::Released => {
            match store.delete_coordination_record(
                COMPOSITE_CLEANUP_NAMESPACE,
                &pending.recovery_id,
                revision,
            )? {
                RecordOutcome::Applied(_) | RecordOutcome::Conflict { current: None } => {
                    Ok(CompositeWorkspaceCleanupStatus::Released)
                }
                RecordOutcome::Conflict { current: Some(_) } => Err(composite_error(
                    "pending composite cleanup receipt changed while it was retried",
                    Vec::new(),
                )),
            }
        }
        CompositeWorkspaceClaimRelease::Partial { failures } => {
            pending.local_released = claim.local_released;
            pending.runners = claim
                .runners
                .into_iter()
                .map(|component| PendingCompositeWorkspaceCleanupComponent {
                    runner_id: component.runner_id,
                    claim: component.claim,
                    released: component.released,
                })
                .collect();
            pending.attempt_count = pending.attempt_count.saturating_add(1);
            pending.last_error = PendingCompositeWorkspaceCleanupError {
                code: "workspace_claim_composite".into(),
                message: failures.join("; "),
            };
            write_pending_composite_cleanup(
                &store,
                &pending,
                ExpectedRevision::Revision(revision),
            )?;
            Ok(CompositeWorkspaceCleanupStatus::Pending {
                recovery_id: pending.recovery_id,
                attempt_count: pending.attempt_count,
                failures,
            })
        }
    }
}

fn pending_composite_cleanup_for_workspace(
    workspace: &WorkspaceIdentity,
) -> Option<CompositeWorkspaceCleanupStatus> {
    let unavailable = |error: Error| {
        Some(CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id: "unknown".into(),
            reason: error.message,
        })
    };
    let store = match open_composite_store() {
        Ok(store) => store,
        Err(error) => return unavailable(error),
    };
    match read_pending_composite_cleanups(&store) {
        Ok(entries) => entries
            .into_iter()
            .find_map(|(_, pending)| {
                (pending.workspace == *workspace).then_some(
                    CompositeWorkspaceCleanupStatus::Pending {
                        recovery_id: pending.recovery_id,
                        attempt_count: pending.attempt_count,
                        failures: vec![pending.last_error.message],
                    },
                )
            })
            .or_else(|| quarantined_composite_cleanup_for_workspace(&store, workspace)),
        Err(error) => unavailable(error),
    }
}

/// Composite intents and cleanup receipts live in the ambient installation's
/// observation store, as coordination records (#15947).
fn open_composite_store() -> Result<ObservationStore> {
    ObservationStore::open_initialized_for_lifecycle()
}

/// The workspace digest keying its composite acquisition intent record.
fn composite_acquisition_intent_key(workspace: &WorkspaceIdentity) -> String {
    homeboy_engine_primitives::content_hash::nul_separated_digest([
        workspace.schema.as_str(),
        workspace.kind.as_str(),
        workspace.locator.as_str(),
    ])
}

fn verify_composite_acquisition_intent(intent: &CompositeAcquisitionIntent) -> Result<()> {
    if intent.schema != COMPOSITE_ACQUISITION_INTENT_SCHEMA
        || intent.recovery_id.trim().is_empty()
        || intent.requested_ttl_ms == 0
        || intent.requested_ttl_ms > MAX_WORKSPACE_CLAIM_TTL_MS
        || !matches!(
            intent.state.as_str(),
            INTENT_STATE_ACQUIRING | INTENT_STATE_ACTIVE
        )
    {
        return Err(composite_error(
            "composite acquisition intent is malformed",
            Vec::new(),
        ));
    }
    intent.workspace.verify()
}

/// Write the intent when its record is still at `expected`, returning the new
/// revision. Creating with [`ExpectedRevision::Absent`] is what makes two
/// concurrent acquisitions of one workspace fail closed on the second.
fn write_composite_acquisition_intent(
    store: &ObservationStore,
    intent: &CompositeAcquisitionIntent,
    expected: ExpectedRevision,
) -> Result<i64> {
    verify_composite_acquisition_intent(intent)?;
    put_composite_record(
        store,
        COMPOSITE_ACQUISITION_NAMESPACE,
        &composite_acquisition_intent_key(&intent.workspace),
        expected,
        &intent.state,
        &to_payload(intent)?,
        "workspace has a concurrent composite acquisition intent",
    )
}

/// Remove the intent. With `revision` set only that exact intent is removed,
/// so a fresh intent written meanwhile survives.
fn remove_composite_acquisition_intent(
    store: &ObservationStore,
    workspace: &WorkspaceIdentity,
    revision: Option<i64>,
) -> Result<()> {
    let key = composite_acquisition_intent_key(workspace);
    loop {
        let Some(current) = store.coordination_record(COMPOSITE_ACQUISITION_NAMESPACE, &key)?
        else {
            return Ok(());
        };
        if revision.is_some_and(|revision| revision != current.revision) {
            return Err(composite_error(
                "composite acquisition intent changed before it was cleared",
                Vec::new(),
            ));
        }
        if let RecordOutcome::Applied(_) = store.delete_coordination_record(
            COMPOSITE_ACQUISITION_NAMESPACE,
            &key,
            current.revision,
        )? {
            return Ok(());
        }
    }
}

/// Read-only: whether any composite claim intent or cleanup receipt may still
/// cover `workspace`. An unreadable receipt cannot prove absence.
fn composite_claim_receipt_outstanding(workspace: &WorkspaceIdentity) -> bool {
    let Ok(store) = open_composite_store() else {
        return true;
    };
    // An intent past the maximum claim lifetime no longer proves a live claim;
    // acquisition clears it only after a complete authority inventory, so
    // preflight leaves that decision to acquisition instead of blocking it.
    let intent_present = store
        .coordination_record(
            COMPOSITE_ACQUISITION_NAMESPACE,
            &composite_acquisition_intent_key(workspace),
        )
        .map(|record| {
            record.is_some_and(|record| {
                serde_json::from_value::<CompositeAcquisitionIntent>(record.payload)
                    .ok()
                    .is_none_or(|intent| {
                        now_ms()
                            < intent
                                .started_at_ms
                                .saturating_add(MAX_WORKSPACE_CLAIM_TTL_MS)
                    })
            })
        })
        .unwrap_or(true);
    let receipt_present = || {
        let Ok(records) = store.coordination_records(COMPOSITE_CLEANUP_NAMESPACE) else {
            return true;
        };
        records.into_iter().any(|record| {
            record
                .payload
                .get("workspace")
                .cloned()
                .and_then(|value| serde_json::from_value::<WorkspaceIdentity>(value).ok())
                .is_none_or(|recorded| recorded == *workspace)
        })
    };
    intent_present || receipt_present()
}

fn composite_acquisition_intent_for_workspace(
    workspace: &WorkspaceIdentity,
) -> Option<CompositeWorkspaceCleanupStatus> {
    let quarantined = |recovery_id: String, reason: String| {
        Some(CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id,
            reason,
        })
    };
    let store = match open_composite_store() {
        Ok(store) => store,
        Err(error) => {
            return quarantined(
                "unknown".into(),
                format!("could not open composite acquisition intents: {error}"),
            )
        }
    };
    let record = match store.coordination_record(
        COMPOSITE_ACQUISITION_NAMESPACE,
        &composite_acquisition_intent_key(workspace),
    ) {
        Ok(Some(record)) => record,
        Ok(None) => return None,
        Err(error) => {
            return quarantined(
                "unknown".into(),
                format!("could not read composite acquisition intent: {error}"),
            )
        }
    };
    let Ok(intent) = serde_json::from_value::<CompositeAcquisitionIntent>(record.payload) else {
        return quarantined(
            "unknown".into(),
            "composite acquisition intent is malformed".into(),
        );
    };
    if verify_composite_acquisition_intent(&intent).is_err()
        || intent.workspace != *workspace
        || intent.state != record.state
    {
        return quarantined(
            intent.recovery_id,
            "composite acquisition intent is malformed".into(),
        );
    }
    let conservative_expiry = intent
        .started_at_ms
        .saturating_add(MAX_WORKSPACE_CLAIM_TTL_MS);
    if now_ms() < conservative_expiry {
        return Some(CompositeWorkspaceCleanupStatus::Pending {
            recovery_id: intent.recovery_id,
            attempt_count: 0,
            failures: vec![
                "composite acquisition intent is within the maximum component TTL".into(),
            ],
        });
    }
    // A token-free marker may be cleared only with a complete local and remote
    // authority inventory. Any unavailable inventory remains a quarantine.
    let clear = claim_store()
        .and_then(|store| {
            store
                .has_live_authority(workspace, now_ms())
                .map(|live| !live)
        })
        .and_then(|local_clear| {
            if !local_clear {
                return Ok(false);
            }
            with_runner_continuation(|provider| {
                provider
                    .workspace_claim_runner_ids()
                    .and_then(|runner_ids| {
                        if !runner_ids.is_empty() && !provider.supports_workspace_claims() {
                            return Ok(false);
                        }
                        runner_ids.into_iter().try_fold(true, |clear, runner_id| {
                            provider
                                .workspace_claim_authority_is_clear(&runner_id, workspace)
                                .map(|remote_clear| clear && remote_clear)
                        })
                    })
            })
        });
    match clear {
        Ok(true)
            if remove_composite_acquisition_intent(&store, workspace, Some(record.revision))
                .is_ok() =>
        {
            None
        }
        Ok(_) => Some(CompositeWorkspaceCleanupStatus::Pending {
            recovery_id: intent.recovery_id,
            attempt_count: 0,
            failures: vec![
                "complete authority inventory still reports a live claim or owner".into(),
            ],
        }),
        Err(error) => quarantined(
            intent.recovery_id,
            format!(
                "complete authority inventory is unavailable: {}",
                error.message
            ),
        ),
    }
}

/// Write a pending cleanup receipt when its record is still at `expected`.
fn write_pending_composite_cleanup(
    store: &ObservationStore,
    pending: &PendingCompositeWorkspaceCleanup,
    expected: ExpectedRevision,
) -> Result<i64> {
    #[cfg(test)]
    if FAIL_NEXT_PENDING_COMPOSITE_CLEANUP_WRITE.swap(false, Ordering::SeqCst) {
        return Err(Error::internal_io(
            "injected pending composite cleanup receipt write failure",
            None,
        ));
    }
    verify_pending_composite_cleanup(pending)?;
    put_composite_record(
        store,
        COMPOSITE_CLEANUP_NAMESPACE,
        &pending.recovery_id,
        expected,
        CLEANUP_STATE_PENDING,
        &to_payload(pending)?,
        "pending composite cleanup receipt changed concurrently",
    )
}

/// Every verified pending receipt with its record revision. A receipt that
/// fails verification is quarantined in place: its record moves to the
/// `quarantined` state, keeping only the identity it blocks and the reason.
fn read_pending_composite_cleanups(
    store: &ObservationStore,
) -> Result<Vec<(i64, PendingCompositeWorkspaceCleanup)>> {
    let mut entries = Vec::new();
    for record in store.coordination_records(COMPOSITE_CLEANUP_NAMESPACE)? {
        if record.state != CLEANUP_STATE_PENDING {
            continue;
        }
        match serde_json::from_value::<PendingCompositeWorkspaceCleanup>(record.payload.clone())
            .map_err(|error| Error::internal_json(error.to_string(), Some(record.key.clone())))
            .and_then(|pending| {
                verify_pending_composite_cleanup(&pending)?;
                (pending.recovery_id == record.key)
                    .then_some(pending)
                    .ok_or_else(|| {
                        composite_error(
                            "pending composite cleanup receipt is malformed",
                            Vec::new(),
                        )
                    })
            }) {
            Ok(pending) => entries.push((record.revision, pending)),
            Err(error) => quarantine_pending_composite_cleanup(store, &record, error.message)?,
        }
    }
    Ok(entries)
}

fn quarantine_pending_composite_cleanup(
    store: &ObservationStore,
    record: &CoordinationRecord,
    reason: String,
) -> Result<()> {
    let workspace = record
        .payload
        .get("workspace")
        .cloned()
        .and_then(|value| serde_json::from_value(value).ok());
    let quarantine = QuarantinedCompositeWorkspaceCleanup {
        recovery_id: record.key.clone(),
        workspace,
        reason,
    };
    put_composite_record(
        store,
        COMPOSITE_CLEANUP_NAMESPACE,
        &record.key,
        ExpectedRevision::Revision(record.revision),
        CLEANUP_STATE_QUARANTINED,
        &to_payload(&quarantine)?,
        "pending composite cleanup receipt changed while it was quarantined",
    )
    .map(|_| ())
}

fn quarantined_composite_cleanup_for_workspace(
    store: &ObservationStore,
    workspace: &WorkspaceIdentity,
) -> Option<CompositeWorkspaceCleanupStatus> {
    store
        .coordination_records(COMPOSITE_CLEANUP_NAMESPACE)
        .ok()?
        .into_iter()
        .filter(|record| record.state == CLEANUP_STATE_QUARANTINED)
        .find_map(|record| {
            let quarantine: QuarantinedCompositeWorkspaceCleanup =
                serde_json::from_value(record.payload).ok()?;
            (quarantine.workspace.as_ref() == Some(workspace)).then_some(
                CompositeWorkspaceCleanupStatus::Quarantined {
                    recovery_id: quarantine.recovery_id,
                    reason: quarantine.reason,
                },
            )
        })
}

fn put_composite_record(
    store: &ObservationStore,
    namespace: &'static str,
    key: &str,
    expected: ExpectedRevision,
    state: &str,
    payload: &serde_json::Value,
    conflict: &str,
) -> Result<i64> {
    match store.put_coordination_record(
        namespace,
        key,
        expected,
        RecordWrite {
            state,
            payload,
            expires_at_ms: None,
        },
    )? {
        RecordOutcome::Applied(Some(record)) => Ok(record.revision),
        RecordOutcome::Applied(None) | RecordOutcome::Conflict { .. } => {
            Err(composite_error(conflict, Vec::new()))
        }
    }
}

fn to_payload(value: &impl Serialize) -> Result<serde_json::Value> {
    serde_json::to_value(value).map_err(|error| Error::internal_json(error.to_string(), None))
}

fn verify_pending_composite_cleanup(pending: &PendingCompositeWorkspaceCleanup) -> Result<()> {
    pending.workspace.verify()?;
    if pending.schema != PENDING_COMPOSITE_WORKSPACE_CLEANUP_SCHEMA
        || pending.recovery_id.trim().is_empty()
        || pending.generation == 0
        || pending.local.workspace != pending.workspace
    {
        return Err(composite_error(
            "pending composite cleanup receipt is malformed",
            Vec::new(),
        ));
    }
    pending.local.verify_shape(0)?;
    for component in &pending.runners {
        if component.runner_id.trim().is_empty() || component.claim.workspace != pending.workspace {
            return Err(composite_error(
                "pending composite cleanup component is malformed",
                Vec::new(),
            ));
        }
        component.claim.verify_shape(0)?;
    }
    Ok(())
}

fn composite_error(message: &str, failures: Vec<String>) -> Error {
    Error::validation_invalid_argument(
        "workspace_claim_composite",
        message,
        None,
        (!failures.is_empty()).then_some(failures),
    )
}

fn claim_store() -> Result<WorkspaceClaimStore> {
    Ok(workspace_claim_store_at(paths::homeboy_data()?))
}

/// Construct the controller-local claim authority below an explicit lifecycle
/// data root. Lifecycle commits use this rather than resolving ambient paths.
pub(crate) fn workspace_claim_store_at(data_root: std::path::PathBuf) -> WorkspaceClaimStore {
    WorkspaceClaimStore::in_data_root(&data_root)
}

/// Return the durable controller binding for a workspace-owning run. Callers
/// serialize this unchanged across the direct daemon boundary.
pub fn workspace_claim_binding(run_id: &str) -> Result<Option<WorkspaceClaimBinding>> {
    let record = store::read_record(run_id)?;
    require_record_workspace_owner(&record)?;
    Ok(record
        .workspace_identity
        .zip(record.workspace_claim)
        .map(|(workspace, claim)| WorkspaceClaimBinding {
            workspace,
            lifecycle_revision: record.workspace_lifecycle_revision,
            claim: Some(claim),
        }))
}

/// Legacy generic runner runs have a durable run id but no agent-task record.
/// They remain unbound; an existing workspace-owning record still fails closed.
pub fn workspace_claim_binding_if_present(run_id: &str) -> Result<Option<WorkspaceClaimBinding>> {
    match store::read_record(run_id) {
        Ok(_) => workspace_claim_binding(run_id),
        Err(_) => Ok(None),
    }
}

/// Direct daemons issue their own durable lease in their authority store. The
/// controller supplies only the stable workspace and owner identities.
pub fn workspace_owner_registration_if_present(
    run_id: &str,
) -> Result<Option<(WorkspaceIdentity, String)>> {
    match store::read_record(run_id) {
        Ok(record) => {
            require_record_workspace_owner(&record)?;
            Ok(record
                .workspace_identity
                .map(|workspace| (workspace, record.run_id)))
        }
        Err(_) => Ok(None),
    }
}

fn now_ms() -> u64 {
    chrono::Utc::now().timestamp_millis().max(0) as u64
}

// `local_workspace_authority_inventory` and its
// `local_workspace_authority_inventory_in_store` sibling lived here. The pair
// was dead end to end: the ambient half had no callers, and the rooted half had
// exactly one — the ambient half. Inventory was local-controller evidence only,
// and runner authority composition is already delegated to
// `RunnerContinuationProvider` in a later layer, so nothing was left to read it.
// Deleting the ambient half removed a resolution point that existed for nobody;
// the rooted half went with it rather than being left behind as a rooted body
// no caller reaches (#7505).

pub fn acquire_local_workspace_claim(
    workspace: WorkspaceIdentity,
    _lifecycle_revision: u64,
) -> Result<WorkspaceClaim> {
    claim_store()?.acquire(workspace, LOCAL_WORKSPACE_CLAIM_TTL_MS, now_ms())
}

pub fn validate_local_workspace_claim(claim: &WorkspaceClaim) -> Result<bool> {
    claim_store()?.validate(claim, now_ms())
}

pub fn release_local_workspace_claim(claim: &WorkspaceClaim) -> Result<()> {
    claim_store()?.release(claim, now_ms())
}

pub(crate) fn register_local_workspace_owner_in_store(
    store: &WorkspaceClaimStore,
    workspace: WorkspaceIdentity,
    run_id: &str,
) -> Result<WorkspaceOwnerLease> {
    store.register_owner(
        workspace,
        run_id,
        LOCAL_WORKSPACE_OWNER_LEASE_TTL_MS,
        now_ms(),
    )
}

pub(crate) fn validate_local_workspace_owner_in_store(
    store: &WorkspaceClaimStore,
    lease: &WorkspaceOwnerLease,
) -> Result<bool> {
    store.validate_owner(lease, now_ms())
}

pub(crate) fn release_local_workspace_owner_in_store(
    store: &WorkspaceClaimStore,
    lease: &WorkspaceOwnerLease,
) -> Result<()> {
    store.release_owner(lease, now_ms())
}

pub(crate) fn renew_record_workspace_owner_in_store(
    store: &WorkspaceClaimStore,
    record: &mut AgentTaskRunRecord,
) -> Result<()> {
    if record.state().is_terminal() {
        return Ok(());
    }
    let Some(lease) = record.workspace_owner_lease.as_ref() else {
        return Ok(());
    };
    let renewed = store.renew_owner(lease, LOCAL_WORKSPACE_OWNER_LEASE_TTL_MS, now_ms())?;
    record.workspace_lifecycle_revision = renewed.lifecycle_revision;
    record.workspace_owner_lease = Some(renewed);
    Ok(())
}

pub(crate) fn require_record_workspace_owner(record: &AgentTaskRunRecord) -> Result<()> {
    require_record_workspace_owner_in_store(&claim_store()?, record)
}

pub(crate) fn require_record_workspace_owner_in_store(
    store: &WorkspaceClaimStore,
    record: &AgentTaskRunRecord,
) -> Result<()> {
    let Some(identity) = record.workspace_identity.as_ref() else {
        return Ok(());
    };
    identity.verify()?;
    let lease = record.workspace_owner_lease.as_ref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_owner_lease",
            "workspace-owning agent-task record has no durable owner lease",
            Some(record.run_id.clone()),
            None,
        )
    })?;
    if lease.workspace != *identity
        || lease.owner_id != record.run_id
        || lease.lifecycle_revision != record.workspace_lifecycle_revision
        || !validate_local_workspace_owner_in_store(store, lease)?
    {
        return Err(Error::validation_invalid_argument(
            "workspace_owner_lease",
            "workspace owner lease is stale, malformed, or no longer owned by this controller",
            Some(record.run_id.clone()),
            None,
        ));
    }
    Ok(())
}

pub(crate) fn release_terminal_record_workspace_owner_in_store(
    store: &WorkspaceClaimStore,
    record: &AgentTaskRunRecord,
) -> Result<()> {
    if record.state().is_terminal() {
        if let Some(lease) = record.workspace_owner_lease.as_ref() {
            store.release_owner(lease, now_ms())?;
        }
    }
    Ok(())
}

pub(crate) fn identity_for_plan(plan: &AgentTaskPlan) -> Result<Option<WorkspaceIdentity>> {
    // Task-worktree identity is assigned by the workspace resolver before the
    // plan is submitted. Other modes have no portable physical allocation here.
    let _ = plan;
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::test_support::with_isolated_home;
    use homeboy_runner_contract::{WorkspaceClaimProtocol, WORKSPACE_CLAIM_SCHEMA};
    use std::collections::BTreeSet;
    use std::fs;
    use std::sync::{Arc, Mutex};

    fn composite_store() -> ObservationStore {
        open_composite_store().expect("observation store")
    }

    fn intent_record(workspace: &WorkspaceIdentity) -> Option<CoordinationRecord> {
        composite_store()
            .coordination_record(
                COMPOSITE_ACQUISITION_NAMESPACE,
                &composite_acquisition_intent_key(workspace),
            )
            .expect("read intent record")
    }

    /// Replace whatever intent is stored for the workspace.
    fn overwrite_intent(intent: &CompositeAcquisitionIntent) {
        let expected = intent_record(&intent.workspace)
            .map_or(ExpectedRevision::Absent, |record| {
                ExpectedRevision::Revision(record.revision)
            });
        write_composite_acquisition_intent(&composite_store(), intent, expected)
            .expect("write intent");
    }

    struct ClaimProvider {
        runner_ids: Result<Vec<String>>,
        fail_acquire_for: Option<String>,
        fail_release_once_for: Mutex<BTreeSet<String>>,
        fail_release_always_for: Arc<Mutex<BTreeSet<String>>>,
        live: Arc<Mutex<BTreeSet<String>>>,
        released: Arc<Mutex<Vec<String>>>,
    }

    impl ClaimProvider {
        fn claim(workspace: WorkspaceIdentity, runner_id: &str) -> WorkspaceClaim {
            WorkspaceClaim {
                schema: WORKSPACE_CLAIM_SCHEMA.into(),
                protocol: WorkspaceClaimProtocol::current(),
                workspace,
                lifecycle_revision: 1,
                token: format!("{runner_id}-claim"),
                expires_at_ms: now_ms() + 60_000,
            }
        }
    }

    impl RunnerContinuationProvider for ClaimProvider {
        fn supports_workspace_claims(&self) -> bool {
            true
        }

        fn workspace_claim_runner_ids(&self) -> Result<Vec<String>> {
            self.runner_ids.clone()
        }

        fn acquire_workspace_claim(
            &self,
            runner_id: &str,
            workspace: WorkspaceIdentity,
            _lifecycle_revision: u64,
            _deadline: std::time::Instant,
        ) -> Result<WorkspaceClaim> {
            if self.fail_acquire_for.as_deref() == Some(runner_id)
                && workspace.locator == "workspace-claim-rollback"
            {
                return Err(Error::internal_unexpected(format!(
                    "injected acquisition failure for {runner_id}"
                )));
            }
            self.live
                .lock()
                .expect("live claims")
                .insert(runner_id.into());
            Ok(Self::claim(workspace, runner_id))
        }

        fn release_workspace_claim(
            &self,
            runner_id: &str,
            _claim: &WorkspaceClaim,
            _deadline: std::time::Instant,
        ) -> Result<()> {
            self.released
                .lock()
                .expect("release log")
                .push(runner_id.into());
            if self
                .fail_release_once_for
                .lock()
                .expect("release faults")
                .remove(runner_id)
            {
                return Err(Error::internal_unexpected(format!(
                    "injected release failure for {runner_id}"
                )));
            }
            if self
                .fail_release_always_for
                .lock()
                .expect("release faults")
                .contains(runner_id)
            {
                return Err(Error::internal_unexpected(format!(
                    "injected persistent release failure for {runner_id}"
                )));
            }
            self.live.lock().expect("live claims").remove(runner_id);
            Ok(())
        }

        fn runner_job_log_snapshot(
            &self,
            _runner_id: &str,
            _job_id: &str,
        ) -> Result<homeboy_core::api_jobs::RunnerJobLogSnapshot> {
            Err(Error::internal_unexpected(
                "not used by workspace claim tests",
            ))
        }

        fn is_runner_connected(&self, _runner_id: &str) -> bool {
            true
        }

        fn runner_authority(&self, _runner_id: &str) -> RunnerAuthority {
            RunnerAuthority::Configured
        }

        fn run_continuation_exec(
            &self,
            _runner_id: &str,
            _cwd: &str,
            _command: &[String],
            _run_id: &str,
        ) -> Result<i32> {
            Err(Error::internal_unexpected(
                "not used by workspace claim tests",
            ))
        }
    }

    fn workspace() -> WorkspaceIdentity {
        WorkspaceIdentity::new("test", "workspace-claim-rollback").expect("workspace")
    }

    fn install_claim_provider(provider: ClaimProvider) -> RunnerContinuationTestGuard {
        super::super::tests::ensure_runner_continuation_provider_reset_hook();
        RunnerContinuationTestGuard::install(Box::new(provider))
    }

    fn provider(runner_ids: Result<Vec<String>>) -> (ClaimProvider, Arc<Mutex<BTreeSet<String>>>) {
        let live = Arc::new(Mutex::new(BTreeSet::new()));
        (
            ClaimProvider {
                runner_ids,
                fail_acquire_for: None,
                fail_release_once_for: Mutex::new(BTreeSet::new()),
                fail_release_always_for: Arc::new(Mutex::new(BTreeSet::new())),
                live: live.clone(),
                released: Arc::new(Mutex::new(Vec::new())),
            },
            live,
        )
    }

    #[test]
    fn commit_budget_uses_shortest_authority_expiry() {
        // The explicit `now_ms` is a fake monotonic-clock injection seam: no
        // wall clock or sleep is needed to exercise expiry while waiting.
        assert_eq!(commit_budget_ms(10_000, [310_000, 10_500].into_iter()), 0);
        assert_eq!(
            commit_budget_ms(10_000, [310_000, 12_500].into_iter()),
            1_500
        );
    }

    fn task_worktree(id: &str, run_id: Option<&str>) -> TaskWorktreeRecord {
        TaskWorktreeRecord {
            id: format!("fixture@{id}"),
            component_id: "fixture".into(),
            source_checkout: "/tmp/source".into(),
            worktree_path: format!("/tmp/missing-{id}"),
            branch: id.into(),
            base_ref: "HEAD".into(),
            workspace_identity: None,
            task_url: None,
            run_id: run_id.map(str::to_string),
            cleanup_policy: homeboy_core::worktree::CleanupPolicy::RemoveWhenSafe,
            terminal_disposition: Some("succeeded".into()),
            branch_cleanup_intent: homeboy_core::worktree::BranchCleanupIntent::DeleteWhenMerged,
            created_at: "2026-01-01T00:00:00Z".into(),
            state: homeboy_core::worktree::TaskWorktreeState::Active,
            lifecycle_revision: 1,
            terminal_workspace_authority: None,
        }
    }

    fn controller_run(
        store: &AgentTaskLifecycleStore,
        run_id: &str,
        state: AgentTaskRunState,
        metadata: serde_json::Value,
    ) -> AgentTaskRunRecord {
        use homeboy_core::run_lifecycle_record::RunLifecycleRecord;
        let record = {
            let mut record = AgentTaskRunRecord::new_queued(
                run_id.to_string(),
                "plan".to_string(),
                store.controller_plan_path(run_id).display().to_string(),
                "2026-08-13T00:00:00Z".to_string(),
            );
            record.lifecycle = RunLifecycleRecord::with_execution_state(state.into());
            record.metadata = metadata;
            record.set_state_for_test(state);
            record
        };
        store.write_record(&record).expect("controller run");
        record
    }

    #[test]
    fn local_terminal_authority_is_per_record_under_a_remote_claim_authority() {
        with_isolated_home(|_| {
            // A Lab-style remote claim authority is configured on this controller.
            let (provider, live) = provider(Ok(vec!["runner-a".into()]));
            let _provider = install_claim_provider(provider);
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let succeeded = AgentTaskRunState::Succeeded;
            controller_run(&store, "local-run", succeeded, serde_json::json!({}));
            let mut handed_off =
                controller_run(&store, "handed-off-run", succeeded, serde_json::json!({}));
            handed_off.lab_handoff = Some(
                AgentTaskLabHandoff::pending("runner-a", now_timestamp(), now_timestamp())
                    .accepted("job-a", now_timestamp()),
            );
            store.write_record(&handed_off).expect("handed-off run");
            controller_run(
                &store,
                "legacy-runner-run",
                succeeded,
                serde_json::json!({ "runner_id": "runner-a" }),
            );
            controller_run(
                &store,
                "submitted-run",
                succeeded,
                serde_json::json!({ "runner_submission_intent": { "state": "preparing" } }),
            );
            controller_run(
                &store,
                "running-run",
                AgentTaskRunState::Running,
                serde_json::json!({}),
            );

            let local = task_worktree("local", Some("local-run"));
            let proof = local_terminal_workspace_authority(&local).expect("locally provable");
            assert_eq!(proof.authority_set, vec!["controller".to_string()]);
            assert!(proof.exact_for(&local, Some("local-run")));

            let reason = |id: &str, run_id: Option<&str>| {
                local_terminal_workspace_authority(&task_worktree(id, run_id))
                    .expect_err("record must be preserved")
                    .reason
            };
            let remote = MissingActiveWorktreeReason::RemoteClaimReleaseUnsupported;
            assert_eq!(reason("handed-off", Some("handed-off-run")), remote);
            assert_eq!(reason("legacy", Some("legacy-runner-run")), remote);
            assert_eq!(reason("submitted", Some("submitted-run")), remote);
            assert_eq!(
                reason("running", Some("running-run")),
                MissingActiveWorktreeReason::LiveRun
            );
            let unavailable = MissingActiveWorktreeReason::TerminalAuthorityUnavailable;
            assert_eq!(reason("no-run", None), unavailable);
            assert_eq!(reason("missing-run", Some("absent-run")), unavailable);

            // A controller-only claim never contacts the configured runner.
            let mut claim = acquire_composite_workspace_claim_until(
                proof.workspace.clone(),
                1,
                CompositeClaimAuthorities::ControllerOnly,
                std::time::Instant::now() + std::time::Duration::from_secs(5),
            )
            .expect("controller-only claim");
            assert!(claim.runners.is_empty());
            assert!(live.lock().expect("live claims").is_empty());
            // While its acquisition intent is outstanding the record is preserved,
            // except for the claim holder's own mutation-time re-check.
            assert_eq!(
                local_terminal_workspace_authority(&local)
                    .expect_err("intent covers the workspace")
                    .reason,
                remote
            );
            assert!(local_terminal_workspace_authority_under_claim(&local, &claim).is_ok());
            let mut foreign = claim.clone();
            foreign.workspace = task_worktree("other", None)
                .effective_workspace_identity()
                .expect("other workspace");
            assert_eq!(
                local_terminal_workspace_authority_under_claim(&local, &foreign)
                    .expect_err("a claim on another workspace grants nothing")
                    .reason,
                remote
            );
            assert!(matches!(
                release_composite_workspace_claim(&mut claim).expect("release"),
                CompositeWorkspaceClaimRelease::Released
            ));
            assert!(local_terminal_workspace_authority(&local).is_ok());
        });
    }

    #[test]
    fn orphaned_run_is_terminal_only_by_its_aggregate_and_never_after_a_lab_handoff() {
        use crate::agent_task::{AgentTaskOutcome, AgentTaskOutcomeStatus};
        use crate::agent_task_scheduler::{AgentTaskAggregate, AgentTaskAggregateTotals};
        with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_current_environment().expect("store");
            let aggregate = |status| AgentTaskAggregate {
                schema: crate::agent_task::AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
                plan_id: "plan".to_string(),
                totals: AgentTaskAggregateTotals::default(),
                outcomes: vec![AgentTaskOutcome {
                    status,
                    ..Default::default()
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            };
            // Each run left an aggregate but has no lifecycle record.
            store
                .write_aggregate("orphan-run", &aggregate(AgentTaskOutcomeStatus::Failed))
                .expect("orphan aggregate");
            store
                .write_aggregate(
                    "handed-off-orphan",
                    &aggregate(AgentTaskOutcomeStatus::Succeeded),
                )
                .expect("handed-off aggregate");
            fs::write(store.lab_handoff_lock_path("handed-off-orphan"), b"").expect("lock");
            assert!(!store.record_exists("orphan-run").expect("probe"));

            let orphan = task_worktree("orphan", Some("orphan-run"));
            let proof = local_terminal_workspace_authority(&orphan).expect("aggregate proves it");
            assert_eq!(proof.controller_state, "Failed");
            assert_eq!(proof.observations[0].evidence, "Failed@aggregate");
            assert!(proof.exact_for(&orphan, Some("orphan-run")));

            let reason = |id: &str, run_id: &str| {
                local_terminal_workspace_authority(&task_worktree(id, Some(run_id)))
                    .expect_err("record must be preserved")
                    .reason
            };
            assert_eq!(
                reason("handed-off", "handed-off-orphan"),
                MissingActiveWorktreeReason::RemoteClaimReleaseUnsupported
            );
            assert_eq!(
                reason("no-proof", "run-without-aggregate"),
                MissingActiveWorktreeReason::TerminalAuthorityUnavailable
            );

            // A live lifecycle record stays the authority over its aggregate.
            controller_run(
                &store,
                "orphan-run",
                AgentTaskRunState::Running,
                serde_json::json!({}),
            );
            assert_eq!(
                reason("orphan", "orphan-run"),
                MissingActiveWorktreeReason::LiveRun
            );
        });
    }

    #[test]
    fn enumeration_failure_after_local_acquisition_releases_the_local_claim() {
        with_isolated_home(|_| {
            let (provider, live) = provider(Err(Error::internal_unexpected(
                "injected enumeration failure",
            )));
            let _provider = install_claim_provider(provider);
            let workspace = workspace();

            let failure = acquire_composite_workspace_claim(workspace.clone(), 1)
                .expect_err("enumeration must fail");

            assert_eq!(failure.primary.message, "injected enumeration failure");
            assert!(failure.rollback_failures.is_empty());
            assert!(failure.cleanup.is_none());
            assert!(live.lock().expect("live claims").is_empty());
            let claim = acquire_local_workspace_claim(workspace, 1).expect("local claim released");
            release_local_workspace_claim(&claim).expect("release verification claim");
        });
    }

    #[test]
    fn malformed_and_duplicate_runner_ids_release_the_local_claim() {
        for runner_ids in [vec![" ".into()], vec!["runner-a".into(), "runner-a".into()]] {
            with_isolated_home(|_| {
                let (provider, live) = provider(Ok(runner_ids));
                let _provider = install_claim_provider(provider);
                let failure = acquire_composite_workspace_claim(workspace(), 1)
                    .expect_err("invalid runner IDs must fail");
                assert!(failure.primary.message.contains("runner id"));
                assert!(failure.cleanup.is_none());
                assert!(live.lock().expect("live claims").is_empty());
                let claim =
                    acquire_local_workspace_claim(workspace(), 1).expect("local claim released");
                release_local_workspace_claim(&claim).expect("release verification claim");
            });
        }
    }

    #[test]
    fn later_remote_failure_releases_local_and_prior_remote_claims() {
        with_isolated_home(|_| {
            let (mut provider, live) = provider(Ok(vec!["runner-a".into(), "runner-b".into()]));
            provider.fail_acquire_for = Some("runner-b".into());
            let _provider = install_claim_provider(provider);

            let failure = acquire_composite_workspace_claim(workspace(), 1)
                .expect_err("second remote acquisition must fail");

            assert_eq!(
                failure.primary.message,
                "injected acquisition failure for runner-b"
            );
            assert!(failure.rollback_failures.is_empty());
            assert!(failure.cleanup.is_none());
            assert!(live.lock().expect("live claims").is_empty());
            let claim =
                acquire_local_workspace_claim(workspace(), 1).expect("local claim released");
            release_local_workspace_claim(&claim).expect("release verification claim");
        });
    }

    #[test]
    fn failed_remote_rollback_persists_and_retries_a_durable_cleanup_receipt() {
        with_isolated_home(|_| {
            let (mut provider, live) = provider(Ok(vec!["runner-a".into(), "runner-b".into()]));
            provider.fail_acquire_for = Some("runner-b".into());
            let faults = provider.fail_release_always_for.clone();
            faults.lock().expect("faults").insert("runner-a".into());
            let _provider = install_claim_provider(provider);

            let failure = acquire_composite_workspace_claim(workspace(), 1)
                .expect_err("second remote acquisition must fail");

            assert_eq!(
                failure.primary.message,
                "injected acquisition failure for runner-b"
            );
            assert_eq!(failure.rollback_failures.len(), 1);
            assert_eq!(failure.rollback_failures[0].component, "runner-a");
            assert_eq!(
                failure.primary.details["workspace_claim_composite_rollback"][0]["message"],
                "injected persistent release failure for runner-a"
            );
            assert!(
                failure.cleanup.is_none(),
                "receipt is lifecycle-owned after persistence"
            );
            let recovery_ref = failure.primary.details["workspace_claim_composite_cleanup"]
                ["recovery_ref"]
                .as_str()
                .expect("redacted recovery reference");
            assert!(!recovery_ref.contains("runner-a-claim"));
            assert!(matches!(
                pending_composite_cleanup_for_workspace(&workspace()),
                Some(CompositeWorkspaceCleanupStatus::Pending { .. })
            ));
            let blocked = acquire_composite_workspace_claim(workspace(), 2)
                .expect_err("same workspace must fail closed while cleanup is pending");
            assert_eq!(blocked.primary.retryable, Some(true));
            faults.lock().expect("faults").clear();
            let retry = retry_pending_composite_workspace_cleanups(1).expect("restart retry");
            assert!(matches!(
                retry.as_slice(),
                [CompositeWorkspaceCleanupStatus::Released]
            ));
            let mut unrelated = acquire_composite_workspace_claim(
                WorkspaceIdentity::new("test", "unrelated-workspace-claim").expect("identity"),
                1,
            )
            .expect("unrelated workspace can proceed");
            assert!(matches!(
                release_composite_workspace_claim(&mut unrelated).expect("unrelated release"),
                CompositeWorkspaceClaimRelease::Released
            ));
            // A fresh lifecycle call can replay the persisted receipt without
            // retaining any process-local composite token map.
            assert!(retry_pending_composite_workspace_cleanups(1)
                .expect("idempotent retry")
                .is_empty());
            assert!(live.lock().expect("live claims").is_empty());
        });
    }

    #[test]
    fn malformed_pending_receipt_is_quarantined_and_blocks_only_its_identity() {
        with_isolated_home(|_| {
            let workspace = workspace();
            let local = acquire_local_workspace_claim(workspace.clone(), 1).expect("local claim");
            let recovery_id = "malformed-receipt".to_string();
            let mut pending = PendingCompositeWorkspaceCleanup {
                schema: PENDING_COMPOSITE_WORKSPACE_CLEANUP_SCHEMA.into(),
                recovery_id: recovery_id.clone(),
                created_at: chrono::Utc::now().to_rfc3339(),
                workspace: workspace.clone(),
                generation: 1,
                local: local.clone(),
                local_released: false,
                runners: Vec::new(),
                attempt_count: 0,
                last_error: PendingCompositeWorkspaceCleanupError {
                    code: "test".into(),
                    message: "test fixture".into(),
                },
            };
            pending.local.token.clear();
            put_composite_record(
                &composite_store(),
                COMPOSITE_CLEANUP_NAMESPACE,
                &recovery_id,
                ExpectedRevision::Absent,
                CLEANUP_STATE_PENDING,
                &to_payload(&pending).expect("receipt json"),
                "fixture",
            )
            .expect("corrupt receipt");

            let blocked = acquire_composite_workspace_claim(workspace.clone(), 2)
                .expect_err("malformed receipt must fail closed");
            assert!(
                blocked.primary.details["workspace_claim_composite_cleanup"]["status"]
                    .as_str()
                    .expect("status")
                    .contains(&recovery_id)
            );
            let quarantined = composite_store()
                .coordination_record(COMPOSITE_CLEANUP_NAMESPACE, &recovery_id)
                .expect("read receipt")
                .expect("quarantined receipt is retained");
            assert_eq!(quarantined.state, CLEANUP_STATE_QUARANTINED);
            assert!(!quarantined.payload.to_string().contains(&local.token));
            // The quarantine survives a reopen and still blocks its identity
            // without blocking an unrelated one.
            assert!(matches!(
                pending_composite_cleanup_for_workspace(&workspace),
                Some(CompositeWorkspaceCleanupStatus::Quarantined { .. })
            ));
            assert!(pending_composite_cleanup_for_workspace(
                &WorkspaceIdentity::new("test", "unrelated-quarantine").expect("identity")
            )
            .is_none());

            release_local_workspace_claim(&local).expect("fixture release");
        });
    }

    #[test]
    fn write_ahead_intent_blocks_only_its_identity_until_authority_is_proven_clear() {
        with_isolated_home(|_| {
            let (provider, _) = provider(Ok(Vec::new()));
            let _provider = install_claim_provider(provider);
            let workspace = workspace();
            let mut intent = CompositeAcquisitionIntent {
                schema: COMPOSITE_ACQUISITION_INTENT_SCHEMA.into(),
                recovery_id: "intent-recovery".into(),
                workspace: workspace.clone(),
                started_at_ms: now_ms(),
                requested_ttl_ms: LOCAL_WORKSPACE_CLAIM_TTL_MS,
                state: "acquiring".into(),
            };
            overwrite_intent(&intent);
            // A second acquisition intent for the same identity loses the
            // write-ahead compare-and-set.
            assert!(write_composite_acquisition_intent(
                &composite_store(),
                &intent,
                ExpectedRevision::Absent
            )
            .is_err());
            assert!(composite_claim_receipt_outstanding(&workspace));
            assert!(matches!(
                composite_acquisition_intent_for_workspace(&workspace),
                Some(CompositeWorkspaceCleanupStatus::Pending { .. })
            ));
            let mut unrelated = acquire_composite_workspace_claim(
                WorkspaceIdentity::new("test", "unrelated-intent").expect("identity"),
                1,
            )
            .expect("unrelated identity remains available");
            assert!(matches!(
                release_composite_workspace_claim(&mut unrelated).expect("release unrelated"),
                CompositeWorkspaceClaimRelease::Released
            ));

            intent.started_at_ms = now_ms().saturating_sub(MAX_WORKSPACE_CLAIM_TTL_MS + 1);
            overwrite_intent(&intent);
            // Preflight defers an expired intent to acquisition's full inventory.
            assert!(!composite_claim_receipt_outstanding(&workspace));
            let live = acquire_local_workspace_claim(workspace.clone(), 1).expect("live authority");
            assert!(matches!(
                composite_acquisition_intent_for_workspace(&workspace),
                Some(CompositeWorkspaceCleanupStatus::Pending { .. })
            ));
            release_local_workspace_claim(&live).expect("release live authority");
            assert!(composite_acquisition_intent_for_workspace(&workspace).is_none());

            let mut claim = acquire_composite_workspace_claim(workspace.clone(), 1)
                .expect("clear intent permits acquisition");
            assert_eq!(
                intent_record(&workspace).expect("active intent").state,
                INTENT_STATE_ACTIVE
            );
            assert!(matches!(
                release_composite_workspace_claim(&mut claim).expect("release claim"),
                CompositeWorkspaceClaimRelease::Released
            ));
            assert!(intent_record(&workspace).is_none());
        });
    }

    #[test]
    fn malformed_write_ahead_intent_quarantines_its_identity() {
        with_isolated_home(|_| {
            let workspace = workspace();
            put_composite_record(
                &composite_store(),
                COMPOSITE_ACQUISITION_NAMESPACE,
                &composite_acquisition_intent_key(&workspace),
                ExpectedRevision::Absent,
                INTENT_STATE_ACQUIRING,
                &serde_json::json!("not an intent"),
                "fixture",
            )
            .expect("malformed intent");
            assert!(matches!(
                composite_acquisition_intent_for_workspace(&workspace),
                Some(CompositeWorkspaceCleanupStatus::Quarantined { .. })
            ));
        });
    }

    #[test]
    fn failed_pending_receipt_write_preserves_the_write_ahead_intent() {
        with_isolated_home(|_| {
            let (mut provider, _) = provider(Ok(vec!["runner-a".into(), "runner-b".into()]));
            provider.fail_acquire_for = Some("runner-b".into());
            provider
                .fail_release_always_for
                .lock()
                .expect("faults")
                .insert("runner-a".into());
            let faults = provider.fail_release_always_for.clone();
            let _provider = install_claim_provider(provider);
            FAIL_NEXT_PENDING_COMPOSITE_CLEANUP_WRITE.store(true, Ordering::SeqCst);

            let failure = acquire_composite_workspace_claim(workspace(), 1)
                .expect_err("incomplete rollback must fail");
            assert!(
                failure.primary.details["workspace_claim_composite_cleanup"]["status"]
                    .as_str()
                    .expect("public status")
                    .contains("quarantined")
            );
            assert!(intent_record(&workspace()).is_some());
            assert!(acquire_composite_workspace_claim(workspace(), 2).is_err());

            let mut unrelated = acquire_composite_workspace_claim(
                WorkspaceIdentity::new("test", "unrelated-write-failure").expect("identity"),
                1,
            )
            .expect("unrelated identity remains available");
            // The injected runner failure is global to this test provider; the
            // unrelated acquisition above proves identity isolation.
            faults.lock().expect("faults").clear();
            assert!(matches!(
                release_composite_workspace_claim(&mut unrelated).expect("release unrelated"),
                CompositeWorkspaceClaimRelease::Released
            ));
        });
    }
}
