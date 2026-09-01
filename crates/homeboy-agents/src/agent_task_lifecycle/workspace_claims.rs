//! Local controller authority for portable agent-task workspace claims.

use super::runner_continuation::with_runner_continuation;
use super::*;
use homeboy_core::engine::local_files::{remove_file_durably, write_json_file_owner_only};
use homeboy_core::workspace_claim::{
    WorkspaceClaim, WorkspaceClaimBinding, WorkspaceClaimStore, WorkspaceIdentity,
    WorkspaceOwnerLease, MAX_WORKSPACE_CLAIM_TTL_MS,
};
use homeboy_core::worktree::{
    authority_set_fingerprint, TaskWorktreeRecord, TerminalWorkspaceAuthorityObservation,
    TerminalWorkspaceAuthorityProof, TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY,
    TERMINAL_WORKSPACE_AUTHORITY_SCHEMA,
};
use serde::{Deserialize, Serialize};
use std::fs;
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

/// A refusal retains the reason and any earlier observations, allowing callers
/// to present durable evidence without treating an uncertain probe as terminal.
#[derive(Debug, Clone)]
pub enum TerminalWorkspaceAuthorityResolution {
    Proven(Box<TerminalWorkspaceAuthorityProof>),
    Refused {
        reason: String,
        observations: Vec<TerminalWorkspaceAuthorityObservation>,
    },
}

/// Resolve historical authority outside the task-worktree registry lease. The
/// manifest is the request binding; controller state and runner snapshots are
/// evidence, never a wall-clock mutation fence.
pub fn resolve_terminal_workspace_authority(
    record: &TaskWorktreeRecord,
) -> Result<TerminalWorkspaceAuthorityResolution> {
    resolve_terminal_workspace_authority_in_store(
        &AgentTaskLifecycleStore::from_current_environment()?,
        record,
    )
}

/// The store-rooted counterpart of [`resolve_terminal_workspace_authority`].
///
/// This is a permission gate over destroying a retained workspace, and it used
/// to consult two independently resolved data roots: the durable controller run
/// came from the ambient lifecycle store, the owner lease from a claim store
/// built out of a second `paths::homeboy_data()` read. When those two disagree
/// the lease lookup finds no file, `validate_owner` answers `Ok(false)`, and the
/// gate concludes "no live local workspace owner" for a workspace that is still
/// owned — a fail-open on the exact check that is supposed to fail closed
/// (#7505). One store now roots both reads.
pub fn resolve_terminal_workspace_authority_in_store(
    lifecycle_store: &AgentTaskLifecycleStore,
    record: &TaskWorktreeRecord,
) -> Result<TerminalWorkspaceAuthorityResolution> {
    let workspace = record.effective_workspace_identity()?;
    let authority_set = match configured_terminal_authorities() {
        Ok(authorities) => authorities,
        Err(reason) => {
            return Ok(TerminalWorkspaceAuthorityResolution::Refused {
                reason,
                observations: Vec::new(),
            })
        }
    };
    if let Some(proof) = &record.terminal_workspace_authority {
        if proof.exact_for(record, record.run_id.as_deref())
            && proof.authority_set == authority_set
            && proof.authority_set_fingerprint == authority_set_fingerprint(&authority_set)
        {
            return Ok(TerminalWorkspaceAuthorityResolution::Proven(Box::new(
                proof.clone(),
            )));
        }
        return Ok(TerminalWorkspaceAuthorityResolution::Refused {
            reason:
                "cached terminal authority receipt is malformed or binds another manifest revision"
                    .into(),
            observations: Vec::new(),
        });
    }
    let Some(run_id) = record.run_id.as_deref() else {
        return Ok(TerminalWorkspaceAuthorityResolution::Refused {
            reason: "no-run-id task worktree requires an existing exact terminal authority receipt"
                .into(),
            observations: Vec::new(),
        });
    };
    let controller = match lifecycle_store.read_record(run_id) {
        Ok(record) => record,
        Err(_) => {
            return Ok(TerminalWorkspaceAuthorityResolution::Refused {
                reason: "durable controller run is missing".into(),
                observations: Vec::new(),
            })
        }
    };
    if !controller.state.is_terminal() || !controller.run_state_projections_agree() {
        return Ok(TerminalWorkspaceAuthorityResolution::Refused {
            reason: "durable controller run is active or has conflicting state projection".into(),
            observations: Vec::new(),
        });
    }
    if controller
        .lab_handoff
        .as_ref()
        .is_some_and(|handoff| handoff.state == AgentTaskLabHandoffState::Pending)
    {
        return Ok(TerminalWorkspaceAuthorityResolution::Refused {
            reason: "controller handoff remains pending".into(),
            observations: Vec::new(),
        });
    }
    if controller
        .workspace_owner_lease
        .as_ref()
        .is_some_and(|lease| {
            validate_local_workspace_owner_in_store(&lifecycle_store.workspace_claim_store(), lease)
                .unwrap_or(true)
        })
    {
        return Ok(TerminalWorkspaceAuthorityResolution::Refused {
            reason: "controller still has a live local workspace owner lease".into(),
            observations: Vec::new(),
        });
    }
    let mut observations = vec![TerminalWorkspaceAuthorityObservation {
        authority: "controller".into(),
        capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.into(),
        capability_version: 1,
        status: "terminal".into(),
        evidence: format!(
            "{:?}@{}",
            controller.state, controller.workspace_lifecycle_revision
        ),
        run_id: Some(run_id.into()),
        runner_job_id: None,
    }];
    let accepted = controller
        .lab_handoff
        .as_ref()
        .filter(|handoff| handoff.state == AgentTaskLabHandoffState::Accepted);
    if let Some(handoff) = accepted {
        let Some(job_id) = handoff
            .runner_job_id
            .as_deref()
            .filter(|id| !id.trim().is_empty())
        else {
            return Ok(TerminalWorkspaceAuthorityResolution::Refused {
                reason: "accepted handoff has no exact runner job identity".into(),
                observations,
            });
        };
        if !with_runner_continuation(|provider| provider.supports_terminal_workspace_authority())
            || !with_runner_continuation(|provider| {
                provider
                    .runner_authority(&handoff.runner_id)
                    .is_configured()
            })
            || !with_runner_continuation(|provider| {
                provider.is_runner_connected(&handoff.runner_id)
            })
        {
            return Ok(TerminalWorkspaceAuthorityResolution::Refused {
                reason: "accepted runner is old, missing, or unreachable".into(),
                observations,
            });
        }
        match with_runner_continuation(|provider| {
            provider.reconcile_runner_job(&handoff.runner_id, job_id)
        }) {
            RunnerJobReconciliation::Snapshot(snapshot) if snapshot.job.status.is_terminal() => {
                observations.push(TerminalWorkspaceAuthorityObservation {
                    authority: handoff.runner_id.clone(),
                    capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.into(),
                    capability_version: 1,
                    status: "terminal".into(),
                    evidence: format!("{}:{:?}", job_id, snapshot.job.status),
                    run_id: Some(run_id.into()),
                    runner_job_id: Some(job_id.into()),
                })
            }
            RunnerJobReconciliation::ConfirmedAbsent {
                checked_generations,
            } if checked_generations > 0 => {
                observations.push(TerminalWorkspaceAuthorityObservation {
                    authority: handoff.runner_id.clone(),
                    capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.into(),
                    capability_version: 1,
                    status: "absent_terminal".into(),
                    evidence: format!(
                        "{} absent across {} generations",
                        job_id, checked_generations
                    ),
                    run_id: Some(run_id.into()),
                    runner_job_id: Some(job_id.into()),
                })
            }
            _ => return Ok(TerminalWorkspaceAuthorityResolution::Refused {
                reason:
                    "accepted runner job is active, unknown, or has incomplete terminal evidence"
                        .into(),
                observations,
            }),
        }
    }
    // This protocol has one durable accepted-handoff binding. A configured
    // authority with no such binding cannot truthfully attest terminality, so
    // refuse rather than manufacture an "absent" receipt.
    if authority_set.iter().any(|authority| {
        authority != "controller"
            && !observations
                .iter()
                .any(|observation| observation.authority == *authority)
    }) {
        return Ok(TerminalWorkspaceAuthorityResolution::Refused {
            reason: "configured terminal authority has no accepted exact runner job binding".into(),
            observations,
        });
    }
    Ok(TerminalWorkspaceAuthorityResolution::Proven(Box::new(
        TerminalWorkspaceAuthorityProof {
            schema: TERMINAL_WORKSPACE_AUTHORITY_SCHEMA.into(),
            capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.into(),
            capability_version: 1,
            workspace,
            task_worktree_id: record.id.clone(),
            manifest_revision: record.lifecycle_revision,
            run_id: Some(run_id.into()),
            controller_state: format!("{:?}", controller.state),
            controller_version: controller.workspace_lifecycle_revision,
            accepted_runner_id: accepted.map(|handoff| handoff.runner_id.clone()),
            accepted_runner_job_id: accepted.and_then(|handoff| handoff.runner_job_id.clone()),
            authority_set_fingerprint: authority_set_fingerprint(&authority_set),
            authority_set,
            observations,
            issued_evidence: vec![format!("controller-run:{run_id}")],
        },
    )))
}

fn configured_terminal_authorities() -> std::result::Result<Vec<String>, String> {
    let runner_ids = with_runner_continuation(|provider| {
        let ids = provider.terminal_workspace_authority_runner_ids()?;
        if !ids.is_empty() && !provider.supports_terminal_workspace_authority() {
            return Err(Error::validation_invalid_argument(
                "terminal_workspace_authority",
                "configured runner does not support terminal workspace authority",
                None,
                None,
            ));
        }
        for runner_id in &ids {
            if runner_id.trim().is_empty()
                || runner_id == "controller"
                || !provider.runner_authority(runner_id).is_configured()
                || !provider.is_runner_connected(runner_id)
            {
                return Err(Error::validation_invalid_argument(
                    "terminal_workspace_authority",
                    "configured terminal workspace authority is missing or unreachable",
                    Some(runner_id.clone()),
                    None,
                ));
            }
        }
        Ok(ids)
    })
    .map_err(|error| error.message)?;
    let mut authorities = vec!["controller".to_string()];
    authorities.extend(runner_ids);
    authorities.sort();
    if authorities.windows(2).any(|pair| pair[0] == pair[1]) {
        return Err("configured terminal workspace authorities are not unique".into());
    }
    Ok(authorities)
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

    fn fail(mut self, mut primary: Error) -> CompositeWorkspaceClaimAcquisitionFailure {
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
                provider.release_workspace_claim(&component.runner_id, &component.claim)
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
    // Recovery is bounded and does not let an unrelated unreachable authority
    // prevent a distinct workspace from being reconciled.
    let _ = retry_pending_composite_workspace_cleanups(32);
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
        state: "acquiring".into(),
    };
    if let Err(error) =
        homeboy_core::config::with_config_lock(|| write_composite_acquisition_intent(&intent))
    {
        return Err(CompositeWorkspaceClaimAcquisitionFailure::from(error));
    }
    match acquire_composite_workspace_claim_unrecovered(workspace, generation) {
        Ok(claim) => {
            let mut active = intent;
            active.state = "active".into();
            if let Err(error) = homeboy_core::config::with_config_lock(|| {
                write_composite_acquisition_intent(&active)
            }) {
                let status = persist_and_retry_composite_workspace_cleanup(claim, &error);
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
                let status =
                    persist_and_retry_composite_workspace_cleanup(cleanup, &failure.primary);
                failure.primary.retryable = Some(true);
                failure.primary.details["workspace_claim_composite_cleanup"] = serde_json::json!({
                    "status": status.public_summary(),
                    "recovery_ref": status.recovery_ref(),
                });
            } else {
                // Every acquired component was released, so the intent is no
                // longer a crash-recovery obligation.
                let _ = homeboy_core::config::with_config_lock(|| {
                    remove_composite_acquisition_intent(&intent.workspace)
                });
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
) -> std::result::Result<CompositeWorkspaceClaim, CompositeWorkspaceClaimAcquisitionFailure> {
    workspace.verify()?;
    let local = acquire_local_workspace_claim(workspace.clone(), generation)?;
    let mut guard =
        CompositeWorkspaceClaimAcquisitionGuard::new(workspace.clone(), generation, local);
    let runner_ids =
        match with_runner_continuation(|provider| provider.workspace_claim_runner_ids()) {
            Ok(runner_ids) => runner_ids,
            Err(error) => return Err(guard.fail(error)),
        };
    let mut unique_runner_ids = std::collections::BTreeSet::new();
    for runner_id in &runner_ids {
        if runner_id.trim().is_empty() || runner_id != runner_id.trim() || runner_id == "controller"
        {
            return Err(guard.fail(Error::validation_invalid_argument(
                "workspace_claim_runner_ids",
                "configured workspace claim runner id is malformed",
                Some(runner_id.clone()),
                None,
            )));
        }
        if !unique_runner_ids.insert(runner_id) {
            return Err(guard.fail(Error::validation_invalid_argument(
                "workspace_claim_runner_ids",
                "configured workspace claim runner ids are not unique",
                Some(runner_id.clone()),
                None,
            )));
        }
    }
    if !runner_ids.is_empty()
        && !with_runner_continuation(|provider| provider.supports_workspace_claims())
    {
        return Err(guard.fail(composite_error(
            "workspace claim provider does not support configured remote authorities",
            Vec::new(),
        )));
    }
    for runner_id in runner_ids {
        let acquired = match with_runner_continuation(|provider| {
            provider.acquire_workspace_claim(&runner_id, workspace.clone(), generation)
        }) {
            Ok(claim) => claim,
            Err(error) => return Err(guard.fail(error)),
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
                return Err(guard.fail(Error::validation_invalid_argument(
                    "workspace_claim",
                    "runner returned a workspace claim for another workspace",
                    Some(runner_id),
                    None,
                )));
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
    claim.workspace.verify()?;
    if claim.local.workspace != claim.workspace || !validate_local_workspace_claim(&claim.local)? {
        return Ok(false);
    }
    for component in &claim.runners {
        if component.runner_id.trim().is_empty()
            || component.claim.workspace != claim.workspace
            || !with_runner_continuation(|provider| {
                provider.validate_workspace_claim(&component.runner_id, &component.claim)
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
            provider.release_workspace_claim(&component.runner_id, &component.claim)
        }) {
            failures.push(format!("{}: {}", component.runner_id, error.message));
        } else {
            component.released = true;
        }
    }
    if failures.is_empty() {
        homeboy_core::config::with_config_lock(|| {
            remove_composite_acquisition_intent(&claim.workspace)
        })?;
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
    if let Err(error) =
        homeboy_core::config::with_config_lock(|| write_pending_composite_cleanup(&pending))
    {
        return CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id,
            reason: format!("could not persist cleanup receipt: {}", error.message),
        };
    }
    // The exact receipt is durable now; removing the token-free intent cannot
    // reopen acquisition because the receipt itself remains identity-scoped.
    if let Err(error) = homeboy_core::config::with_config_lock(|| {
        remove_composite_acquisition_intent(&pending.workspace)
    }) {
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

/// Explicit lifecycle recovery API. Every pending receipt is replayed at most
/// `limit` times; completed receipts are removed atomically after all component
/// releases have reported success.
pub fn retry_pending_composite_workspace_cleanups(
    limit: usize,
) -> Result<Vec<CompositeWorkspaceCleanupStatus>> {
    let entries = read_pending_composite_cleanups()?;
    entries
        .into_iter()
        .take(limit)
        .map(|(_, pending)| retry_pending_composite_workspace_cleanup(&pending.recovery_id))
        .collect()
}

fn retry_pending_composite_workspace_cleanup(
    recovery_id: &str,
) -> Result<CompositeWorkspaceCleanupStatus> {
    homeboy_core::config::with_config_lock(|| {
        let Some((path, mut pending)) = read_pending_composite_cleanups()?
            .into_iter()
            .find(|(_, pending)| pending.recovery_id == recovery_id)
        else {
            return Ok(CompositeWorkspaceCleanupStatus::Released);
        };
        let mut claim = CompositeWorkspaceClaim {
            workspace: pending.workspace.clone(),
            generation: pending.generation,
            // A cleanup receipt is never committed, so its old commit deadline
            // is irrelevant to an idempotent release.
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
                remove_file_durably(&path, "pending composite cleanup")?;
                Ok(CompositeWorkspaceCleanupStatus::Released)
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
                write_pending_composite_cleanup(&pending)?;
                Ok(CompositeWorkspaceCleanupStatus::Pending {
                    recovery_id: pending.recovery_id,
                    attempt_count: pending.attempt_count,
                    failures,
                })
            }
        }
    })
}

fn pending_composite_cleanup_for_workspace(
    workspace: &WorkspaceIdentity,
) -> Option<CompositeWorkspaceCleanupStatus> {
    match read_pending_composite_cleanups() {
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
            .or_else(|| quarantined_composite_cleanup_for_workspace(workspace)),
        Err(error) => Some(CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id: "unknown".into(),
            reason: error.message,
        }),
    }
}

fn pending_composite_cleanup_dir() -> Result<std::path::PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task-composite-workspace-cleanups"))
}

fn composite_acquisition_intent_dir() -> Result<std::path::PathBuf> {
    Ok(paths::homeboy_data()?.join("agent-task-composite-acquisition-intents"))
}

fn composite_acquisition_intent_path(workspace: &WorkspaceIdentity) -> Result<std::path::PathBuf> {
    // This digest is the on-disk filename, so its bytes are a compatibility
    // surface -- see the pinned assertions in `content_hash`.
    let digest = homeboy_engine_primitives::content_hash::nul_separated_digest([
        workspace.schema.as_str(),
        workspace.kind.as_str(),
        workspace.locator.as_str(),
    ]);
    Ok(composite_acquisition_intent_dir()?.join(format!("{digest}.json")))
}

fn write_composite_acquisition_intent(intent: &CompositeAcquisitionIntent) -> Result<()> {
    if intent.schema != COMPOSITE_ACQUISITION_INTENT_SCHEMA
        || intent.recovery_id.trim().is_empty()
        || intent.requested_ttl_ms == 0
        || intent.requested_ttl_ms > MAX_WORKSPACE_CLAIM_TTL_MS
        || !matches!(intent.state.as_str(), "acquiring" | "active")
    {
        return Err(composite_error(
            "composite acquisition intent is malformed",
            Vec::new(),
        ));
    }
    intent.workspace.verify()?;
    write_json_file_owner_only(
        &composite_acquisition_intent_path(&intent.workspace)?,
        intent,
    )
}

fn remove_composite_acquisition_intent(workspace: &WorkspaceIdentity) -> Result<()> {
    let path = composite_acquisition_intent_path(workspace)?;
    remove_file_durably(&path, &path.display().to_string())
}

fn composite_acquisition_intent_for_workspace(
    workspace: &WorkspaceIdentity,
) -> Option<CompositeWorkspaceCleanupStatus> {
    let path = composite_acquisition_intent_path(workspace).ok()?;
    if !path.exists() {
        return None;
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return Some(CompositeWorkspaceCleanupStatus::Quarantined {
                recovery_id: "unknown".into(),
                reason: format!("could not read composite acquisition intent: {error}"),
            })
        }
    };
    let intent: CompositeAcquisitionIntent = match serde_json::from_slice(&bytes) {
        Ok(intent) => intent,
        Err(_) => {
            return Some(CompositeWorkspaceCleanupStatus::Quarantined {
                recovery_id: "unknown".into(),
                reason: "composite acquisition intent is malformed".into(),
            })
        }
    };
    if intent.schema != COMPOSITE_ACQUISITION_INTENT_SCHEMA
        || intent.workspace != *workspace
        || intent.recovery_id.trim().is_empty()
        || intent.requested_ttl_ms == 0
        || intent.requested_ttl_ms > MAX_WORKSPACE_CLAIM_TTL_MS
        || !matches!(intent.state.as_str(), "acquiring" | "active")
    {
        return Some(CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id: intent.recovery_id,
            reason: "composite acquisition intent is malformed".into(),
        });
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
    let clear = store()
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
        Ok(true) if remove_composite_acquisition_intent(workspace).is_ok() => None,
        Ok(_) => Some(CompositeWorkspaceCleanupStatus::Pending {
            recovery_id: intent.recovery_id,
            attempt_count: 0,
            failures: vec![
                "complete authority inventory still reports a live claim or owner".into(),
            ],
        }),
        Err(error) => Some(CompositeWorkspaceCleanupStatus::Quarantined {
            recovery_id: intent.recovery_id,
            reason: format!(
                "complete authority inventory is unavailable: {}",
                error.message
            ),
        }),
    }
}

fn pending_composite_cleanup_path(recovery_id: &str) -> Result<std::path::PathBuf> {
    Ok(pending_composite_cleanup_dir()?.join(format!("{recovery_id}.json")))
}

fn write_pending_composite_cleanup(pending: &PendingCompositeWorkspaceCleanup) -> Result<()> {
    #[cfg(test)]
    if FAIL_NEXT_PENDING_COMPOSITE_CLEANUP_WRITE.swap(false, Ordering::SeqCst) {
        return Err(Error::internal_io(
            "injected pending composite cleanup receipt write failure",
            None,
        ));
    }
    verify_pending_composite_cleanup(pending)?;
    let path = pending_composite_cleanup_path(&pending.recovery_id)?;
    write_json_file_owner_only(&path, pending)
}

fn read_pending_composite_cleanups(
) -> Result<Vec<(std::path::PathBuf, PendingCompositeWorkspaceCleanup)>> {
    let directory = pending_composite_cleanup_dir()?;
    if !directory.exists() {
        return Ok(Vec::new());
    }
    let mut entries = Vec::new();
    for entry in fs::read_dir(&directory).map_err(|error| {
        Error::internal_io(error.to_string(), Some(directory.display().to_string()))
    })? {
        let path = entry
            .map_err(|error| Error::internal_io(error.to_string(), None))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("json")
            || path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".quarantine.json"))
        {
            continue;
        }
        let bytes = fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        match serde_json::from_slice::<PendingCompositeWorkspaceCleanup>(&bytes)
            .map_err(|error| {
                Error::internal_json(error.to_string(), Some(path.display().to_string()))
            })
            .and_then(|pending| {
                verify_pending_composite_cleanup(&pending)?;
                Ok(pending)
            }) {
            Ok(pending) => entries.push((path, pending)),
            Err(error) => quarantine_pending_composite_cleanup(&path, &bytes, error.message)?,
        }
    }
    Ok(entries)
}

fn quarantine_pending_composite_cleanup(
    path: &std::path::Path,
    bytes: &[u8],
    reason: String,
) -> Result<()> {
    let recovery_id = path
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("unknown")
        .to_string();
    let workspace = serde_json::from_slice::<serde_json::Value>(bytes)
        .ok()
        .and_then(|value| value.get("workspace").cloned())
        .and_then(|value| serde_json::from_value(value).ok());
    let quarantine = QuarantinedCompositeWorkspaceCleanup {
        recovery_id: recovery_id.clone(),
        workspace,
        reason,
    };
    let quarantine_path = path.with_file_name(format!("{recovery_id}.quarantine.json"));
    write_json_file_owner_only(&quarantine_path, &quarantine)?;
    remove_file_durably(path, &path.display().to_string())
}

fn quarantined_composite_cleanup_for_workspace(
    workspace: &WorkspaceIdentity,
) -> Option<CompositeWorkspaceCleanupStatus> {
    let directory = pending_composite_cleanup_dir().ok()?;
    let entries = fs::read_dir(directory).ok()?;
    entries.filter_map(|entry| entry.ok()).find_map(|entry| {
        let path = entry.path();
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".quarantine.json"))
            .then_some(())?;
        let quarantine: QuarantinedCompositeWorkspaceCleanup =
            serde_json::from_slice(&fs::read(path).ok()?).ok()?;
        (quarantine.workspace.as_ref() == Some(workspace)).then_some(
            CompositeWorkspaceCleanupStatus::Quarantined {
                recovery_id: quarantine.recovery_id,
                reason: quarantine.reason,
            },
        )
    })
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

fn store() -> Result<WorkspaceClaimStore> {
    Ok(workspace_claim_store_at(paths::homeboy_data()?))
}

/// Construct the controller-local claim authority below an explicit lifecycle
/// data root. Lifecycle commits use this rather than resolving ambient paths.
pub(crate) fn workspace_claim_store_at(data_root: std::path::PathBuf) -> WorkspaceClaimStore {
    WorkspaceClaimStore::new(
        data_root.join(homeboy_core::workspace_claim::LOCAL_WORKSPACE_CLAIMS_DIR),
    )
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
    store()?.acquire(workspace, LOCAL_WORKSPACE_CLAIM_TTL_MS, now_ms())
}

pub fn validate_local_workspace_claim(claim: &WorkspaceClaim) -> Result<bool> {
    store()?.validate(claim, now_ms())
}

pub fn release_local_workspace_claim(claim: &WorkspaceClaim) -> Result<()> {
    store()?.release(claim, now_ms())
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
    if record.state.is_terminal() {
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
    require_record_workspace_owner_in_store(&store()?, record)
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
    if record.state.is_terminal() {
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
    use homeboy_core::workspace_claim::{WorkspaceClaimProtocol, WORKSPACE_CLAIM_SCHEMA};
    use std::collections::BTreeSet;
    use std::sync::{Arc, Mutex};

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

        fn release_workspace_claim(&self, runner_id: &str, _claim: &WorkspaceClaim) -> Result<()> {
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

        fn submit_reverse_broker_job(
            &self,
            _runner_id: &str,
            _request: homeboy_core::api_jobs::RemoteRunnerJobRequest,
        ) -> Result<homeboy_core::api_jobs::Job> {
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
            let path = pending_composite_cleanup_path(&recovery_id).expect("receipt path");
            fs::create_dir_all(path.parent().expect("receipt directory"))
                .expect("receipt directory");
            fs::write(&path, serde_json::to_vec(&pending).expect("receipt json"))
                .expect("corrupt receipt");

            let blocked = acquire_composite_workspace_claim(workspace.clone(), 2)
                .expect_err("malformed receipt must fail closed");
            assert!(
                blocked.primary.details["workspace_claim_composite_cleanup"]["status"]
                    .as_str()
                    .expect("status")
                    .contains(&recovery_id)
            );
            assert!(path
                .with_file_name(format!("{recovery_id}.quarantine.json"))
                .exists());

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
            write_composite_acquisition_intent(&intent).expect("write token-free intent");
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
            write_composite_acquisition_intent(&intent).expect("age intent beyond ttl");
            let live = acquire_local_workspace_claim(workspace.clone(), 1).expect("live authority");
            assert!(matches!(
                composite_acquisition_intent_for_workspace(&workspace),
                Some(CompositeWorkspaceCleanupStatus::Pending { .. })
            ));
            release_local_workspace_claim(&live).expect("release live authority");
            assert!(composite_acquisition_intent_for_workspace(&workspace).is_none());

            let mut claim = acquire_composite_workspace_claim(workspace.clone(), 1)
                .expect("clear intent permits acquisition");
            assert!(composite_acquisition_intent_path(&workspace)
                .expect("intent path")
                .exists());
            assert!(matches!(
                release_composite_workspace_claim(&mut claim).expect("release claim"),
                CompositeWorkspaceClaimRelease::Released
            ));
            assert!(!composite_acquisition_intent_path(&workspace)
                .expect("intent path")
                .exists());
        });
    }

    #[test]
    fn malformed_write_ahead_intent_quarantines_its_identity() {
        with_isolated_home(|_| {
            let workspace = workspace();
            let path = composite_acquisition_intent_path(&workspace).expect("intent path");
            fs::create_dir_all(path.parent().expect("intent directory")).expect("intent directory");
            fs::write(&path, b"not json").expect("malformed intent");
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
            assert!(composite_acquisition_intent_path(&workspace())
                .expect("intent path")
                .exists());
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
