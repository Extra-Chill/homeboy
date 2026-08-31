//! Public batch-cook fanout command handlers.

use homeboy_engine_primitives::content_hash;
use homeboy_engine_primitives::shell::quote_args;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_provider::AgentTaskProviderProfileDeclaration;
use homeboy::agents::agent_task_scheduler::{
    resolve_batch_concurrency, BatchConcurrencyDecision, BatchConcurrencyInputs,
};
use homeboy::agents::agent_task_service::{
    CookAiDisclosure, CookFinalization, CookIdentity, CookProviderTransport, CookRequest,
    CookRetryPolicy, CookWorkspace,
};
use homeboy::agents::agent_task_timeout::{with_current_cook_deadline, CookDeadline};
use homeboy::agents::agent_tasks::batch;
use homeboy::agents::agent_tasks::dependency_actions::{
    execute_resolved_dependency_actions, DependencyAction, DependencyActionExecutor,
    DependencyResolution,
};
use homeboy::agents::agent_tasks::dependency_graph::{
    dependency_graph_readiness, AgentTaskDependencyNode,
};
use homeboy::agents::agent_tasks::dispatch_service::{
    self as dispatch_service, AgentTaskDispatchCommand, DispatchCoreInputs,
};
use homeboy::agents::agent_tasks::fanout_supervisor as supervisor;
use homeboy::agents::agent_tasks::gate::{
    AgentTaskGateEnvironmentPolicy, AgentTaskGateExecutionPolicy, AgentTaskGateRevealPolicy,
    VerifyGateOptions,
};
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::agents::agent_tasks::provider::{self, AgentTaskProviderCatalog};
use homeboy::agents::agent_tasks::scheduler::{AgentTaskPlan, SharedAgentTaskExecutor};
use homeboy::agents::agent_tasks::service::{self as agent_task_service};
use homeboy::agents::agent_tasks::{
    AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA, AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
    AGENT_TASK_BATCH_COOK_FANOUT_SUBMIT_SCHEMA,
};
use homeboy::core::error::{ActionSafety, ExecutableAction};
use homeboy::core::parsed_command_preflight::PlacementDirective;
use homeboy::core::{config, worktree, Error, ErrorCode, Result};
use homeboy_lab_runner_contract::{
    EffectiveExecutionPlacement, ExecutionPlacementFallback, ExecutionPlacementIdentity,
    ExecutionPlacementOverrideAuthorization, ExecutionPlacementRequirement,
};

use crate::cli_surface::Placement;
use crate::commands::utils::response::{CommandNextAction, CommandNextActionKind};

use super::super::CmdResult;
use super::args::{
    AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs, AgentTaskFanoutCommand,
    AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs, AgentTaskFanoutPlanArgs,
    AgentTaskFanoutRunPlanArgs, AgentTaskFanoutSubmitArgs, AgentTaskFanoutSubmitBatchArgs,
    VERIFICATION_PROFILES_EXAMPLE,
};
use super::command_json_value;
use super::default_branch::{resolve_default_branch, DefaultBranchRequest};
use super::gate_contract::{validate_gate_contracts, GateContractValidation};

pub(super) fn fanout(args: AgentTaskFanoutArgs) -> CmdResult<Value> {
    fanout_with_placement(args, Placement::Auto)
}

pub(crate) fn fanout_with_placement(
    args: AgentTaskFanoutArgs,
    placement: Placement,
) -> CmdResult<Value> {
    match args.command {
        AgentTaskFanoutCommand::CookBatch(cook_batch_args) => {
            cook_batch_with_placement(*cook_batch_args, placement)
        }
        AgentTaskFanoutCommand::Plan(plan_args) => {
            // `fanout plan` accepts the same --repo plus issue-URL inputs
            // cook-batch accepts; those route through cook-batch's fully
            // static preview planner so both surfaces validate identically
            // (#13704). A persisted plan input keeps the read-only
            // normalize-and-inspect contract below.
            if !plan_args.issues.is_empty() {
                return cook_batch_with_placement(plan_args.into_cook_batch_preview(), placement);
            }
            let AgentTaskFanoutPlanArgs {
                input,
                fanout_id,
                backend,
                selector,
                model,
                ..
            } = plan_args;
            let load_args = AgentTaskFanoutInputArgs {
                input: input.unwrap_or_default(),
                fanout_id,
                backend,
                selector,
                model,
            };
            // A private controller artifact is accepted only from its owned path,
            // then immediately projected before this read-only response renders.
            let plan = load_batch_cook_fanout_plan(&load_args, true)?;
            Ok((command_json_value(public_batch_cook_plan(&plan))?, 0))
        }
        AgentTaskFanoutCommand::Submit(submit_args) => submit_batch_cook_fanout(submit_args),
        AgentTaskFanoutCommand::SubmitBatch(submit_args) => {
            submit_fanout_batch(submit_args, placement)
        }
        AgentTaskFanoutCommand::Status(status_args) => batch_status(status_args, placement),
        AgentTaskFanoutCommand::Resume(resume_args) => batch_resume(resume_args, placement),
        AgentTaskFanoutCommand::Artifacts(status_args) => batch_artifacts(status_args),
        AgentTaskFanoutCommand::RunPlan(run_args) => run_batch_cook_fanout(run_args, placement),
    }
}

fn invocation_placement_directive(placement: Placement) -> PlacementDirective {
    if let Some(preflight) = homeboy::core::parsed_command_preflight::captured_result() {
        if preflight.placement.requested == placement {
            return preflight.placement;
        }
    }
    let required = if placement == Placement::Lab {
        ExecutionPlacementRequirement::Lab
    } else {
        ExecutionPlacementRequirement::Either
    };
    PlacementDirective {
        requested: placement,
        required,
        selected: if placement == Placement::Lab {
            EffectiveExecutionPlacement::Lab
        } else {
            EffectiveExecutionPlacement::Local
        },
        runner: None,
        fallback: ExecutionPlacementFallback {
            local_allowed: matches!(placement, Placement::Auto | Placement::LabOrLocal),
            reason: None,
        },
        override_authorization: ExecutionPlacementOverrideAuthorization {
            authorized: placement == Placement::Local,
            authority: (placement == Placement::Local)
                .then(|| "operator --placement local".to_string()),
        },
    }
}

fn fanout_placement_preflight(placement: Option<&PlacementDirective>) -> Value {
    let Some(placement) = placement else {
        return Value::Null;
    };
    let admission_deferred = placement.selected == EffectiveExecutionPlacement::Lab;
    serde_json::json!({
        "schema": "homeboy/fanout-placement-preflight/v1",
        "requested": placement.requested,
        "required": placement.required,
        "selected": placement.selected,
        "runner": placement.runner,
        "fallback": placement.fallback,
        "override_authorization": placement.override_authorization,
        "admission": {
            "state": if admission_deferred { "deferred" } else { "confirmed" },
            "revalidate_before_execution": admission_deferred,
            "deferred_to": admission_deferred.then_some("child_attempt_dispatch"),
        },
    })
}

type CookAttemptDispatcherFactory = dyn Fn(
        &CookRequest,
    ) -> std::sync::Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>
    + Send
    + Sync;

const FANOUT_COORDINATOR_HEARTBEAT_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(30);
/// Each static dependency gets an independent budget. Bounded inputs make this
/// a per-phase cost ceiling rather than one workspace-size-dependent deadline.
const DRY_RUN_PHASE_TIMEOUT: Duration = Duration::from_secs(10);
const DRY_RUN_MAX_ISSUES: usize = 128;
const DRY_RUN_MAX_INLINE_JSON_BYTES: usize = 64 * 1024;
const DRY_RUN_MAX_GATE_BYTES: usize = 8 * 1024;
const COMPACT_FANOUT_FAILURE_LIMIT: usize = 3;

#[cfg(test)]
thread_local! {
    static STATIC_WORKTREE_PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// Dry-run is a static preview, not a smaller execution. Keep its budget
/// separate from Cook's execution deadline and report only planning phases so
/// #10019 remains the owner of foreground execution progress.
struct DryRunPlanner {
    phase: &'static str,
    phase_started_at: Instant,
    phase_timeout: Duration,
    replay_command: String,
}

impl DryRunPlanner {
    fn new(args: &AgentTaskFanoutCookBatchArgs, placement: Placement) -> Self {
        Self {
            phase: "initializing",
            phase_started_at: Instant::now(),
            phase_timeout: Duration::from_secs(
                args.dry_run_planner_timeout_seconds
                    .unwrap_or(DRY_RUN_PHASE_TIMEOUT.as_secs()),
            ),
            replay_command: dry_run_replay_command_with_placement(args, placement),
        }
    }

    fn begin(&mut self, phase: &'static str) {
        self.phase = phase;
        self.phase_started_at = Instant::now();
        eprintln!(
            "{{\"event\":\"dry_run_planning_progress\",\"phase\":{}}}",
            serde_json::to_string(phase).expect("phase serializes"),
        );
    }

    fn run<T>(
        &mut self,
        phase: &'static str,
        unresolved_dependency: &'static str,
        operation: impl FnOnce() -> Result<T>,
    ) -> Result<T> {
        self.begin(phase);
        let result = operation();
        self.finish(unresolved_dependency)?;
        result.map_err(|error| self.failure(error, unresolved_dependency))
    }

    /// Static planning workers own cloned inputs and have no mutation authority.
    /// A slow local registry or contract read therefore cannot hold the caller
    /// past this phase's budget; dropping the join handle is safe because the
    /// worker has no durable or repository side effects.
    fn run_bounded<T: Send + 'static>(
        &mut self,
        phase: &'static str,
        unresolved_dependency: &'static str,
        operation: impl FnOnce() -> Result<T> + Send + 'static,
    ) -> Result<T> {
        self.begin(phase);
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        let worker = std::thread::Builder::new()
            .name(format!("homeboy-dry-run-{phase}"))
            .spawn(move || {
                let _ = sender.send(operation());
            })
            .map_err(|error| {
                self.failure(
                    Error::internal_unexpected(format!("start dry-run planner worker: {error}")),
                    unresolved_dependency,
                )
            })?;
        let result = match receiver.recv_timeout(self.phase_timeout) {
            Ok(result) => {
                worker.join().map_err(|_| {
                    self.failure(
                        Error::internal_unexpected("dry-run planner worker panicked"),
                        unresolved_dependency,
                    )
                })?;
                result
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                return Err(
                    self.timeout_error(unresolved_dependency, self.phase_started_at.elapsed())
                )
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                let _ = worker.join();
                return Err(self.failure(
                    Error::internal_unexpected("dry-run planner worker exited without a result"),
                    unresolved_dependency,
                ));
            }
        };
        result.map_err(|error| self.failure(error, unresolved_dependency))
    }

    fn finish(&self, unresolved_dependency: &'static str) -> Result<()> {
        let elapsed = self.phase_started_at.elapsed();
        if elapsed > self.phase_timeout {
            return Err(self.timeout_error(unresolved_dependency, elapsed));
        }
        Ok(())
    }

    fn timeout_error(&self, unresolved_dependency: &'static str, elapsed: Duration) -> Error {
        Error::new(
            ErrorCode::ValidationInvalidArgument,
            "fanout dry-run planner phase deadline exceeded",
            serde_json::json!({
                "reason": "planner_deadline_exceeded",
                "planner_timeout_seconds": self.phase_timeout.as_secs(),
                "phase": self.phase,
                "phase_elapsed_ms": elapsed.as_millis(),
                "unresolved_dependency": unresolved_dependency,
                "replay_command": self.replay_command,
            }),
        )
    }

    fn defer(&self, phase: &'static str, unresolved_dependency: &'static str) -> Error {
        Error::new(
            ErrorCode::ValidationInvalidArgument,
            "fanout dry-run accepts static inputs only",
            serde_json::json!({
                "reason": "static_input_required",
                "phase": phase,
                "unresolved_dependency": unresolved_dependency,
                "replay_command": self.replay_command,
            }),
        )
    }

    fn failure(&self, mut error: Error, unresolved_dependency: &'static str) -> Error {
        error.details["phase"] = Value::String(self.phase.to_string());
        error.details["phase_elapsed_ms"] =
            Value::Number((self.phase_started_at.elapsed().as_millis() as u64).into());
        error.details["unresolved_dependency"] = Value::String(unresolved_dependency.to_string());
        error.details["replay_command"] = Value::String(self.replay_command.clone());
        error
    }
}

struct CoordinatorHeartbeat {
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    stale_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl CoordinatorHeartbeat {
    fn start(batch_id: String, claim_id: String, status_command: String) -> Result<Self> {
        // Claim admission before preflight, then renew it synchronously before
        // any potentially slow gate, workspace, recipe, or provider work.
        batch::heartbeat_fanout_run_batch(&batch_id, &claim_id)?;
        let (stop, receiver) = std::sync::mpsc::channel();
        let stale_error = std::sync::Arc::new(std::sync::Mutex::new(None));
        let worker_error = std::sync::Arc::clone(&stale_error);
        let worker = std::thread::spawn(move || loop {
            match receiver.recv_timeout(FANOUT_COORDINATOR_HEARTBEAT_INTERVAL) {
                Ok(()) | Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    if let Err(error) = batch::heartbeat_fanout_run_batch(&batch_id, &claim_id) {
                        *worker_error.lock().expect("heartbeat error lock") = Some(error.message);
                        break;
                    }
                    if let Ok(record) = batch::read_batch_record(&batch_id) {
                        eprintln!(
                            "{{\"event\":\"coordinator_heartbeat\",\"phase\":{},\"children_total\":{},\"next_action\":{}}}",
                            serde_json::to_string(&record.metadata["coordinator"]["stage"])
                                .expect("coordinator stage serializes"),
                            record.child_runs.len(),
                            serde_json::to_string(&status_command)
                            .expect("status command serializes"),
                        );
                    }
                }
            }
        });
        Ok(Self {
            stop,
            worker: Some(worker),
            stale_error,
        })
    }

    fn finish(mut self) -> Result<()> {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(message) = self
            .stale_error
            .lock()
            .expect("heartbeat error lock")
            .take()
        {
            return Err(Error::validation_invalid_argument(
                "claim_id", message, None, None,
            ));
        }
        Ok(())
    }
}

impl Drop for CoordinatorHeartbeat {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Runs controller-owned cook-batch coordination while dispatching its typed
/// provider attempts through the caller-selected transport.
pub(crate) fn cook_batch_with_attempt_dispatcher_and_placement(
    args: AgentTaskFanoutCookBatchArgs,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
    placement: Placement,
) -> CmdResult<Value> {
    cook_batch_inner(args, Some(attempt_dispatcher), placement)
}

fn submit_batch_cook_fanout(args: AgentTaskFanoutSubmitArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input, false)?;
    if let Some(run_id) = args.run_id {
        plan.rekey(run_id);
    }

    let cooks = plan
        .cooks
        .iter()
        .map(|cook| {
            serde_json::json!({
                "cook_id": cook.cook_id,
                "run_id": cook.run_id(),
                "run_id_semantics": "stable cook id; executed attempts use unique durable run ids",
                "worktree": cook.to_worktree,
                "head": cook.head,
                "workspace_materialization": cook.workspace_materialization,
                "title": cook.title,
                "command": cook_command(&plan, cook),
            })
        })
        .collect::<Vec<_>>();

    Ok((
        serde_json::json!({
            "schema": AGENT_TASK_BATCH_COOK_FANOUT_SUBMIT_SCHEMA,
            "fanout_id": plan.fanout_id,
            "state": "ready",
            "cooks": cooks,
            "next_actions": [
                "run each cook command on its target worktree/branch, or use `agent-task fanout run-plan` to execute the batch cook from this machine"
            ]
        }),
        0,
    ))
}

fn submit_fanout_batch(
    args: AgentTaskFanoutSubmitBatchArgs,
    placement: Placement,
) -> CmdResult<Value> {
    let plan = load_fanout_agent_task_plan(&args.input)?;
    let record = batch::submit_plan_batch(&plan, args.batch_id.as_deref())?;
    let batch_id = record.batch_id.clone();
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-fanout-batch-submit-result/v1",
            "batch": record,
            "commands": batch_commands(&batch_id, placement),
        }),
        0,
    ))
}

fn batch_status(args: AgentTaskFanoutBatchStatusArgs, placement: Placement) -> CmdResult<Value> {
    let mut report = batch::status(&args.batch_id)?;
    // A terminal coordinator failure is the authoritative outcome even when it
    // happened before the first child record existed. Reconciling children first
    // would turn that diagnostic into a misleading "run record not found".
    let admission_pending =
        report.batch.state == batch::AgentTaskBatchState::Admitting && report.admission.absent > 0;
    let observations = if report.admission_blocker.is_some() || admission_pending {
        BTreeMap::new()
    } else {
        reconcile_fanout_pr_states(&args.batch_id, false)?
    };
    if !observations.is_empty() {
        report.dependency_graph = batch::fanout_dependency_graph_with_finalization_statuses(
            &args.batch_id,
            &observations,
        )?;
        if let Some(graph) = &report.dependency_graph {
            let state = batch::fanout_aggregate_state(&report.totals, graph);
            report.batch.state = state;
            report.status = state.outcome_status().to_string();
        }
    }
    // `status` is a read. The operation either returned the requested batch
    // projection (exit 0) or failed and names its cause through `Err`
    // (#13702). The batch's own aggregate state — including `failed` — is
    // subject state: it stays in `data.batch`, never in the transport
    // envelope's success/exit_code.
    // The mutating `resume` command keeps the aggregate exit policy, and
    // durable reconciliation / child continuation stay limited to it.
    let portfolio = if admission_pending {
        load_portfolio(&report.batch)?.status(&BTreeMap::new())
    } else {
        reconcile_portfolio(&report.batch)?
    };
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-fanout-status/v2",
            "batch": report,
            "portfolio": portfolio,
            "commands": batch_commands(&args.batch_id, placement),
        }),
        0,
    ))
}

/// Resume a durable fanout batch after its synchronous coordinator exited.
/// Idempotently harvests every terminal-but-unfinalized child through its
/// original promotion, deterministic gates, commit, push, and PR finalization
/// contract, reconciling per-child state back into the durable batch record so
/// repeated resume calls converge without duplicate PRs (#9525).
fn batch_resume(args: AgentTaskFanoutBatchStatusArgs, placement: Placement) -> CmdResult<Value> {
    reconcile_fanout_pr_states(&args.batch_id, true)?;
    let result = agent_task_service::resume_cook_batch(
        &args.batch_id,
        Arc::new(provider::ExtensionProviderAgentTaskExecutor::discover()),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
    )?;
    let exit_code = result.exit_code;
    let batch = batch::read_batch_record(&args.batch_id)?;
    let portfolio = run_portfolio(&batch)?;
    Ok(batch_resume_result(
        result.value,
        exit_code,
        &args.batch_id,
        Some(portfolio),
        placement,
    ))
}

/// GitHub is the authority for whether a review-ready candidate was accepted.
/// Persist that observation before asking the durable graph for its executable
/// frontier, so a merge releases only its newly-ready descendants.
fn reconcile_fanout_pr_states(batch_id: &str, mutate: bool) -> Result<BTreeMap<String, String>> {
    let batch = batch::read_batch_record(batch_id)?;
    let mut resolutions = Vec::new();
    let mut statuses = BTreeMap::new();
    for child in batch.child_runs {
        let record = if mutate {
            agent_task_lifecycle::reconcile_status(&child.run_id)?
        } else {
            match agent_task_lifecycle::status(&child.run_id) {
                Ok(record) => record,
                // The batch report retains the last durable child state and
                // marks observation freshness separately below. A transient
                // projection lock must not make status itself unavailable.
                Err(error) if error.code == ErrorCode::ObservationStoreBusy => continue,
                Err(error) => return Err(error),
            }
        };
        let Some(mut finalization) = record.metadata.get("cook_finalization").cloned() else {
            continue;
        };
        let pr_ref = finalization
            .get("pr_url")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                finalization
                    .get("pr_number")
                    .and_then(Value::as_u64)
                    .map(|number| number.to_string())
            })
            // Older durable finalization records used a nested PR reference.
            .or_else(|| {
                finalization
                    .get("pr")
                    .and_then(|pr| pr.get("url"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| {
                finalization
                    .get("pr")
                    .and_then(|pr| pr.get("number"))
                    .and_then(Value::as_u64)
                    .map(|number| number.to_string())
            });
        let Some(pr_ref) = pr_ref else { continue };
        let observation = observe_pr_state(&pr_ref)?;
        let status = observation.verdict();
        let mut pr_observation = observation.as_value();
        if let Some(candidate_revision) = finalization
            .pointer("/publication_proof/binding/candidate_sha")
            .and_then(Value::as_str)
        {
            // The PR head is the precise candidate that was reviewed. Retain
            // the comparison with the merge observation so a changed upstream
            // revision always re-runs dependent invalidation/review instead of
            // trusting the prior candidate's gates.
            pr_observation["candidate_revision"] = Value::String(candidate_revision.to_string());
            pr_observation["candidate_revision_matches"] = Value::Bool(
                observation
                    .head_ref_oid
                    .as_deref()
                    .is_none_or(|head| head == candidate_revision),
            );
        }
        statuses.insert(child.task_id.clone(), status.to_string());
        let transition = match status {
            // An approved candidate makes the next stack level reviewable now.
            // Bind it to the exact head observed so a later force-push/new commit
            // is a distinct durable rebase, gate, and review invalidation.
            "review_ready" => observation
                .head_ref_oid
                .clone()
                .zip(observation.head_ref_name.clone()),
            // Once merged, move the dependent from the candidate branch back to
            // the PR's resolved target branch using the merge commit.
            "merged" => observation
                .merge_commit
                .as_ref()
                .map(|commit| commit.oid.clone())
                .zip(observation.base_ref_name.clone()),
            _ => None,
        };
        if let Some((upstream_revision, target_base)) = transition {
            resolutions.push(DependencyResolution {
                child_id: child.task_id.clone(),
                upstream_revision,
                target_base,
            });
        }
        if !mutate
            || (finalization.get("status").and_then(Value::as_str) == Some(status)
                && finalization.get("pr_observation") == Some(&pr_observation))
        {
            continue;
        }
        finalization["status"] = Value::String(status.to_string());
        finalization["pr_observation"] = pr_observation;
        agent_task_lifecycle::record_cook_finalization(&child.run_id, finalization)?;
    }
    if mutate && !resolutions.is_empty() {
        execute_resolved_dependency_actions(
            batch_id,
            &resolutions,
            &mut LocalDependencyActionExecutor,
        )?;
    }
    Ok(statuses)
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FanoutPrObservation {
    state: String,
    #[serde(default)]
    merged_at: Option<String>,
    #[serde(default)]
    review_decision: Option<String>,
    #[serde(default)]
    merge_state_status: Option<String>,
    #[serde(default)]
    merge_commit: Option<FanoutMergeCommit>,
    #[serde(default)]
    base_ref_name: Option<String>,
    #[serde(default)]
    head_ref_oid: Option<String>,
    #[serde(default)]
    head_ref_name: Option<String>,
}

#[derive(Debug, Deserialize)]
struct FanoutMergeCommit {
    oid: String,
}

impl FanoutPrObservation {
    fn verdict(&self) -> &'static str {
        match self.state.as_str() {
            "MERGED" | "CLOSED" if self.merged_at.is_some() => "merged",
            "CLOSED" => "rejected",
            "OPEN" if self.review_decision.as_deref() == Some("CHANGES_REQUESTED") => {
                "revision_requested"
            }
            _ => "review_ready",
        }
    }

    fn as_value(&self) -> Value {
        serde_json::json!({
            "state": self.state,
            "merged_at": self.merged_at,
            "review_decision": self.review_decision,
            "merge_state_status": self.merge_state_status,
            "merge_commit_oid": self.merge_commit.as_ref().map(|commit| &commit.oid),
            "base_ref_name": self.base_ref_name,
            "head_ref_oid": self.head_ref_oid,
            "head_ref_name": self.head_ref_name,
        })
    }
}

fn observe_pr_state(pr: &str) -> Result<FanoutPrObservation> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            pr,
            "--json",
            "state,mergedAt,reviewDecision,mergeStateStatus,mergeCommit,baseRefName,headRefOid,headRefName",
        ])
        .output()
        .map_err(|error| Error::git_command_failed(format!("gh pr view {pr}: {error}")))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "gh pr view {pr}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    serde_json::from_slice(&output.stdout)
        .map_err(|error| Error::internal_json(format!("parse gh pr view {pr}: {error}"), None))
}

struct LocalDependencyActionExecutor;

impl DependencyActionExecutor for LocalDependencyActionExecutor {
    fn side_effect_applied(&mut self, action: &DependencyAction, step: &str) -> Result<bool> {
        match step {
            "fetch" => run_dependency_command(
                &action.worktree,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", action.upstream_revision),
                ],
            )
            .map(|()| true)
            .or_else(|_| Ok(false)),
            "rebase" => Command::new("git")
                .args([
                    "merge-base",
                    "--is-ancestor",
                    &action.upstream_revision,
                    "HEAD",
                ])
                .current_dir(&action.worktree)
                .status()
                .map(|status| status.success())
                .map_err(|error| Error::git_command_failed(format!("git merge-base: {error}"))),
            "push" => {
                let local = dependency_command_output(&action.worktree, &["rev-parse", "HEAD"])?;
                let remote = dependency_command_output(
                    &action.worktree,
                    &[
                        "ls-remote",
                        "--heads",
                        "origin",
                        &format!("refs/heads/{}", action.head),
                    ],
                )?;
                Ok(remote
                    .split_whitespace()
                    .next()
                    .is_some_and(|revision| revision == local))
            }
            "pull_request_base_update" => {
                let Some(pr) = action.pull_request.as_deref() else {
                    return Ok(true);
                };
                let observation = observe_pr_state(pr)?;
                Ok(observation.base_ref_name.as_deref() == Some(&action.target_base))
            }
            // These are durable-local transitions, not GitHub/Git side effects.
            _ => Ok(false),
        }
    }

    fn fetch(&mut self, action: &DependencyAction) -> Result<()> {
        run_dependency_command(
            &action.worktree,
            &["fetch", "--no-tags", "origin", &action.upstream_revision],
        )
    }

    fn rebase(&mut self, action: &DependencyAction) -> Result<()> {
        run_dependency_command(&action.worktree, &["rebase", &action.upstream_revision])
    }

    fn push(&mut self, action: &DependencyAction) -> Result<()> {
        run_dependency_command(
            &action.worktree,
            &[
                "push",
                "--force-with-lease",
                "origin",
                &format!("HEAD:{}", action.head),
            ],
        )
    }

    fn update_pull_request_base(&mut self, action: &DependencyAction) -> Result<()> {
        let Some(pr) = action.pull_request.as_deref() else {
            return Ok(());
        };
        let output = Command::new("gh")
            .args(["pr", "edit", pr, "--base", &action.target_base])
            .current_dir(&action.worktree)
            .output()
            .map_err(|error| Error::git_command_failed(format!("gh pr edit {pr}: {error}")))?;
        if output.status.success() {
            Ok(())
        } else {
            Err(Error::git_command_failed(format!(
                "gh pr edit {pr}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            )))
        }
    }

    fn invalidate_review(&mut self, action: &DependencyAction) -> Result<()> {
        // The Cook lifecycle has already been re-armed by the preceding durable
        // gate-invalidation step. Keep review invalidation as its own receipt.
        let _ = action;
        Ok(())
    }
}

fn run_dependency_command(path: &str, arguments: &[&str]) -> Result<()> {
    dependency_command_output(path, arguments).map(|_| ())
}

fn dependency_command_output(path: &str, arguments: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(arguments)
        .current_dir(path)
        .output()
        .map_err(|error| {
            Error::git_command_failed(format!("git {}: {error}", arguments.join(" ")))
        })?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
    } else {
        Err(Error::git_command_failed(format!(
            "git {}: {}",
            arguments.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

/// Collect current, provider-neutral observations from the durable cook record
/// plus its real git candidate.
fn reconcile_portfolio(
    batch_record: &homeboy::agents::agent_tasks::AgentTaskBatchRecord,
) -> Result<homeboy::agents::agent_tasks::fanout_supervisor::AgentTaskFanoutPortfolioStatus> {
    let mut portfolio = load_portfolio(batch_record)?;
    let observations = batch_record
        .child_runs
        .iter()
        .map(|child| portfolio_observation(&child.task_id, &child.run_id, false))
        .collect::<Result<Vec<_>>>()?;
    let dependencies = durable_graph_dependencies(batch_record)?;
    let status = portfolio.reconcile(observations, &dependencies);
    Ok(status)
}

/// Mutating supervisor entrypoint. Constructing the production adapter here
/// keeps Cook's durable continuation, force-with-lease receipt, and PR recovery
/// on the public `fanout resume` path rather than a status projection.
fn run_portfolio(
    batch_record: &homeboy::agents::agent_tasks::AgentTaskBatchRecord,
) -> Result<supervisor::AgentTaskFanoutPortfolioRunReport> {
    let mut portfolio = load_portfolio(batch_record)?;
    let dependencies = durable_graph_dependencies(batch_record)?;
    portfolio.run(&mut CookFanoutPortfolioAdapter, &dependencies)
}

fn load_portfolio(
    batch_record: &homeboy::agents::agent_tasks::AgentTaskBatchRecord,
) -> Result<supervisor::AgentTaskFanoutPortfolio> {
    match supervisor::read_portfolio(&batch_record.batch_id) {
        Ok(portfolio) => Ok(portfolio),
        Err(_) if !supervisor::portfolio_exists(&batch_record.batch_id)? => {
            Ok(supervisor::AgentTaskFanoutPortfolio::new(
                batch_record.batch_id.clone(),
                batch_record.child_runs.iter().map(|child| {
                    supervisor::AgentTaskFanoutPortfolioChild {
                        child_id: child.task_id.clone(),
                        tracker_ref: agent_task_lifecycle::status(&child.run_id)
                            .ok()
                            .and_then(|record| {
                                declared_tracker_ref(&record.metadata).map(str::to_string)
                            })
                            .unwrap_or_else(|| {
                                format!("homeboy://agent-task/run/{}", child.run_id)
                            }),
                        run_id: child.run_id.clone(),
                        source_sha: None,
                        base_sha: None,
                        head_sha: None,
                        evidence_generation: 0,
                        finding_fingerprints: Default::default(),
                        finding_fingerprint_recency: Default::default(),
                        blocker: None,
                        next_action: None,
                    }
                }),
            ))
        }
        Err(error) => Err(error),
    }
}

/// Consume the graph owner's typed readiness projection without duplicating its
/// topology, state, or downstream action contracts.
struct DurableGraphDependencies {
    batch_id: String,
    readiness: Option<homeboy::agents::agent_tasks::dependency_graph::AgentTaskDependencyReadiness>,
}

impl supervisor::FanoutDependencyResolver for DurableGraphDependencies {
    fn readiness(&self, child_id: &str) -> supervisor::FanoutDependencyReadiness {
        use homeboy::agents::agent_tasks::dependency_graph::AgentTaskDependencyState;

        let Some(readiness) = &self.readiness else {
            return supervisor::FanoutDependencyReadiness::Ready;
        };
        if readiness.states.get(child_id) == Some(&AgentTaskDependencyState::Ready) {
            return supervisor::FanoutDependencyReadiness::Ready;
        }
        let detail = readiness
            .blocked_paths
            .get(child_id)
            .map(|path| path.join(" <- "))
            .unwrap_or_else(|| {
                let state = readiness.states.get(child_id).copied();
                format!("dependency graph projects child state '{state:?}'")
            });
        supervisor::FanoutDependencyReadiness::Blocked {
            detail,
            evidence_ref: format!(
                "homeboy://agent-task/batch/{}/dependency-graph/children/{child_id}",
                self.batch_id
            ),
        }
    }
}

fn durable_graph_dependencies(
    batch_record: &homeboy::agents::agent_tasks::AgentTaskBatchRecord,
) -> Result<DurableGraphDependencies> {
    let Some(graph) = batch_record.metadata.get("dependency_graph") else {
        return Ok(DurableGraphDependencies {
            batch_id: batch_record.batch_id.clone(),
            readiness: None,
        });
    };
    let readiness = graph
        .get("readiness")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(DurableGraphDependencies {
        batch_id: batch_record.batch_id.clone(),
        readiness,
    })
}

/// Production adapter for the durable child action executor. Cook owns the
/// provider, promotion, gate, review, Git, and PR contracts; this adapter only
/// chooses the child-local, idempotent continuation that must run next.
struct CookFanoutPortfolioAdapter;

impl supervisor::FanoutPortfolioAdapter for CookFanoutPortfolioAdapter {
    fn observe(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<supervisor::AgentTaskFanoutPortfolioObservation> {
        portfolio_observation(&child.child_id, &child.run_id, true)
    }

    fn continue_provider(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        resume_fanout_child(child, false)
    }

    fn rebase_candidate(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        resume_fanout_child(child, true)
    }

    fn recreate_candidate(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        // Recreate is intentionally a separate continuation request. The Cook
        // recovery contract selects only its persisted recreation path.
        resume_fanout_child(child, true)
    }

    fn rerun_gates_and_review(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
    ) -> Result<()> {
        resume_fanout_child(child, true)
    }

    fn finalize_or_update_pr(
        &mut self,
        child: &supervisor::AgentTaskFanoutPortfolioChild,
        should_force_with_lease: bool,
    ) -> Result<()> {
        if should_force_with_lease {
            force_with_lease_then_reconcile(child)
        } else {
            resume_fanout_child(child, false)
        }
    }
}

/// Publish the already-gated candidate with a remote compare-and-swap, persist
/// its receipt, then ask Cook's recovery finalizer to refresh the existing PR.
/// This deliberately bypasses Cook's cached-finalization return path.
fn force_with_lease_then_reconcile(
    child: &supervisor::AgentTaskFanoutPortfolioChild,
) -> Result<()> {
    let record = agent_task_lifecycle::reconcile_status(&child.run_id)?;
    let promotion = record.metadata.get("latest_promotion").ok_or_else(|| {
        Error::validation_invalid_argument(
            "latest_promotion",
            "force-with-lease requires the durable promoted candidate",
            Some(child.run_id.clone()),
            None,
        )
    })?;
    let path = promotion
        .pointer("/provenance/worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "force-with-lease requires the promoted candidate worktree",
                Some(child.run_id.clone()),
                None,
            )
        })?;
    let finalization = record.metadata.get("cook_finalization").ok_or_else(|| {
        Error::validation_invalid_argument(
            "cook_finalization",
            "force-with-lease requires a prior Cook finalization with its PR head branch",
            Some(child.run_id.clone()),
            None,
        )
    })?;
    let head = finalization
        .get("head")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "cook_finalization.head",
                "force-with-lease requires the prior finalization head branch",
                Some(child.run_id.clone()),
                None,
            )
        })?;
    // One store for both receipt writes and the status read below: the two
    // receipts describe one force-with-lease and must land in one home.
    let lifecycle_store =
        agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()?;
    let receipt = force_with_lease_push(path, head)?;
    agent_task_lifecycle::record_cook_force_with_lease_receipt_in_store(
        &lifecycle_store,
        &child.run_id,
        receipt.clone(),
    )?;
    agent_task_service::recover_cook_pr(&child.run_id, Vec::new(), false)?;
    let mut receipt = receipt;
    receipt["pr_refresh_completed"] = Value::Bool(true);
    agent_task_lifecycle::record_cook_force_with_lease_receipt_in_store(
        &lifecycle_store,
        &child.run_id,
        receipt,
    )?;
    Ok(())
}

/// Compare-and-swap the already-gated candidate onto its PR branch. Keeping
/// this boundary independent of lifecycle mutation makes the expected remote
/// SHA, command, and post-push observation directly verifiable.
fn force_with_lease_push(path: &str, head: &str) -> Result<Value> {
    let destination = format!("refs/heads/{head}");
    let expected_sha = git_stdout(path, &["ls-remote", "--heads", "origin", &destination])?
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| {
            Error::git_command_failed(format!(
                "cannot force-with-lease a missing remote branch `{destination}`"
            ))
        })?;
    let candidate_sha = git_stdout(path, &["rev-parse", "HEAD"])?;
    let lease = format!("--force-with-lease={destination}:{expected_sha}");
    let refspec = format!("{candidate_sha}:{destination}");
    // A restart may observe the completed push before its receipt was durable.
    // The matching remote ref is sufficient to record that receipt and refresh
    // the PR; issuing a second force-push would widen the interruption window.
    if expected_sha != candidate_sha {
        git_stdout(path, &["push", &lease, "origin", &refspec])?;
    }
    let after_sha = git_stdout(path, &["ls-remote", "--heads", "origin", &destination])?
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| {
            Error::git_command_failed(format!(
                "force-with-lease did not leave remote branch `{destination}` readable"
            ))
        })?;
    if after_sha != candidate_sha {
        return Err(Error::git_command_failed(format!(
            "force-with-lease left `{destination}` at `{after_sha}` instead of candidate `{candidate_sha}`"
        )));
    }
    Ok(serde_json::json!({
        "command": ["git", "push", lease, "origin", refspec],
        "remote": "origin",
        "ref": destination,
        "expected_sha": expected_sha,
        "after_sha": after_sha,
        "reconciled_existing_push": expected_sha == candidate_sha,
        // The receipt is intentionally incomplete until the PR host has been
        // refreshed. A restart then resumes refresh without repeating a push.
        "pr_refresh_completed": false,
    }))
}

fn git_stdout(path: &str, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resume_fanout_child(
    child: &supervisor::AgentTaskFanoutPortfolioChild,
    rerun_completed_gates: bool,
) -> Result<()> {
    agent_task_service::resume_cook(
        &child.run_id,
        Arc::new(provider::ExtensionProviderAgentTaskExecutor::discover()),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
        rerun_completed_gates,
    )?;
    Ok(())
}

fn portfolio_observation(
    child_id: &str,
    run_id: &str,
    reconcile: bool,
) -> Result<homeboy::agents::agent_tasks::fanout_supervisor::AgentTaskFanoutPortfolioObservation> {
    use homeboy::agents::agent_tasks::fanout_supervisor as supervisor;
    let record = if reconcile {
        agent_task_lifecycle::reconcile_status(run_id)
    } else {
        agent_task_lifecycle::status(run_id)
    }
    .ok();
    let provider = match record.as_ref().map(|record| record.state) {
        Some(agent_task_lifecycle::AgentTaskRunState::Running) => {
            supervisor::AgentTaskFanoutProviderState::Running
        }
        Some(
            agent_task_lifecycle::AgentTaskRunState::Succeeded
            | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable,
        ) => supervisor::AgentTaskFanoutProviderState::Succeeded,
        Some(_) => supervisor::AgentTaskFanoutProviderState::Failed,
        None => supervisor::AgentTaskFanoutProviderState::Pending,
    };
    let promotion = record
        .as_ref()
        .and_then(|record| record.metadata.get("latest_promotion"));
    let path = promotion
        .and_then(|value| value.pointer("/provenance/worktree_path"))
        .and_then(Value::as_str);
    let declared_base = promotion
        .and_then(|value| value.pointer("/verified_base/base"))
        .and_then(Value::as_str);
    let (worktree, head_sha, current_base_sha) = path
        .map(|path| git_candidate_state(path, declared_base))
        .unwrap_or((
            supervisor::AgentTaskFanoutWorktreeState::Missing,
            None,
            None,
        ));
    let base_sha = promotion
        .and_then(|value| value.pointer("/verified_base/sha"))
        .and_then(Value::as_str)
        .map(str::to_string);
    let source_sha = promotion
        .and_then(|value| value.pointer("/provenance/source_sha"))
        .or_else(|| promotion.and_then(|value| value.pointer("/source/sha")))
        .and_then(Value::as_str)
        .map(str::to_string);
    let gates = match promotion
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
    {
        Some("applied") => supervisor::AgentTaskFanoutEvidenceState::Current,
        Some("failed") => supervisor::AgentTaskFanoutEvidenceState::Failed,
        _ => supervisor::AgentTaskFanoutEvidenceState::Missing,
    };
    let finalization = record
        .as_ref()
        .and_then(|record| record.metadata.get("cook_finalization"));
    let accepted_evidence = finalization
        .and_then(|value| value.get("status"))
        .and_then(Value::as_str)
        == Some("review_ready");
    let receipt_current = record
        .as_ref()
        .and_then(|record| record.metadata.get("cook_force_with_lease_receipt"))
        .and_then(|receipt| {
            (receipt.get("pr_refresh_completed") == Some(&Value::Bool(true))).then_some(receipt)
        })
        .and_then(|receipt| receipt.get("after_sha"))
        .and_then(Value::as_str)
        .is_some_and(|after_sha| head_sha.as_deref() == Some(after_sha));
    let (tracker, pr, remote_head_sha, findings) = match path {
        Some(path) => github_observation(path, record.as_ref(), finalization)?,
        None => (
            tracker_state_without_observation(record.as_ref()),
            supervisor::AgentTaskFanoutPrState::Unknown,
            None,
            Vec::new(),
        ),
    };
    Ok(supervisor::AgentTaskFanoutPortfolioObservation {
        child_id: child_id.to_string(),
        tracker,
        provider,
        worktree,
        candidate: supervisor::AgentTaskFanoutCandidateState {
            source_sha,
            base_sha,
            head_sha,
            current_base_sha,
            remote_head_sha,
            publication_receipt_current: receipt_current,
            can_rebase: path.is_some(),
            can_recreate: false,
        },
        gates,
        // A durable finalization record is the accepted local evidence. Host
        // review decisions and findings below are independently refreshed.
        acceptance: if accepted_evidence {
            supervisor::AgentTaskFanoutEvidenceState::Current
        } else {
            supervisor::AgentTaskFanoutEvidenceState::Missing
        },
        pr,
        findings,
    })
}

/// Read tracker and PR state through Homeboy's existing GitHub API boundary.
/// Keeping this at the CLI composition layer makes the supervisor itself
/// injectable and product-neutral while avoiding synthetic "open" states.
fn github_observation(
    path: &str,
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
    finalization: Option<&Value>,
) -> Result<(
    supervisor::AgentTaskFanoutTrackerState,
    supervisor::AgentTaskFanoutPrState,
    Option<String>,
    Vec<supervisor::AgentTaskFanoutReviewFinding>,
)> {
    let tracker = match record.and_then(|record| declared_tracker_ref(&record.metadata)) {
        Some(task_url) => match IssueRef::parse(task_url) {
            Ok(issue) => homeboy::core::git::issue_find(
                None,
                homeboy::core::git::IssueFindOptions {
                    state: homeboy::core::git::IssueState::All,
                    limit: 100,
                    path: Some(path.to_string()),
                    ..Default::default()
                },
            )
            .map(|result| {
                result
                    .items
                    .iter()
                    .find(|item| item.number.to_string() == issue.number)
                    .map_or(
                        supervisor::AgentTaskFanoutTrackerState::DeclaredUnobserved,
                        |item| {
                            if item.state.eq_ignore_ascii_case("open") {
                                supervisor::AgentTaskFanoutTrackerState::Open
                            } else {
                                supervisor::AgentTaskFanoutTrackerState::Closed
                            }
                        },
                    )
            }),
            // Tracker identity is generic; this adapter only observes GitHub.
            Err(_) => Ok(supervisor::AgentTaskFanoutTrackerState::DeclaredUnobserved),
        }?,
        None => supervisor::AgentTaskFanoutTrackerState::Unknown,
    };
    let head = finalization
        .and_then(|value| value.get("head"))
        .and_then(Value::as_str);
    let base = finalization
        .and_then(|value| value.get("base"))
        .and_then(Value::as_str);
    let Some(head) = head else {
        return Ok((
            tracker,
            supervisor::AgentTaskFanoutPrState::Missing,
            None,
            Vec::new(),
        ));
    };
    let prs = homeboy::core::git::pr_find(
        None,
        homeboy::core::git::PrFindOptions {
            head: Some(head.to_string()),
            base: base.map(str::to_string),
            state: homeboy::core::git::PrState::All,
            limit: 10,
            path: Some(path.to_string()),
        },
    )?;
    let Some(pr) = prs.items.first() else {
        return Ok((
            tracker,
            supervisor::AgentTaskFanoutPrState::Missing,
            None,
            Vec::new(),
        ));
    };
    let view = homeboy::core::git::pr_view(None, pr.number, Some(path.to_string()))?;
    let findings = matches!(view.review_decision.as_deref(), Some("CHANGES_REQUESTED"))
        .then(|| supervisor::AgentTaskFanoutReviewFinding {
            fingerprint: format!("github-pr-{}-changes-requested", view.number),
            summary: "GitHub review decision is changes requested".to_string(),
        })
        .into_iter()
        .collect();
    let state = if view.merged_at.is_some() {
        supervisor::AgentTaskFanoutPrState::Merged
    } else if view.ci_state.eq_ignore_ascii_case("terminal_green") {
        supervisor::AgentTaskFanoutPrState::OpenChecksPassing
    } else if view.ci_state.eq_ignore_ascii_case("failure") {
        supervisor::AgentTaskFanoutPrState::OpenChecksFailed
    } else {
        supervisor::AgentTaskFanoutPrState::OpenChecksPending
    };
    Ok((tracker, state, view.head_sha, findings))
}

fn declared_tracker_ref(metadata: &Value) -> Option<&str> {
    metadata
        .pointer("/cook_recipe/source_refs/0")
        .and_then(Value::as_str)
        .filter(|reference| reference.starts_with("https://") || reference.starts_with("http://"))
}

fn tracker_state_without_observation(
    record: Option<&agent_task_lifecycle::AgentTaskRunRecord>,
) -> supervisor::AgentTaskFanoutTrackerState {
    if record.is_some_and(|record| declared_tracker_ref(&record.metadata).is_some()) {
        supervisor::AgentTaskFanoutTrackerState::DeclaredUnobserved
    } else {
        supervisor::AgentTaskFanoutTrackerState::Unknown
    }
}

fn git_candidate_state(
    path: &str,
    declared_base: Option<&str>,
) -> (
    homeboy::agents::agent_tasks::fanout_supervisor::AgentTaskFanoutWorktreeState,
    Option<String>,
    Option<String>,
) {
    use homeboy::agents::agent_tasks::fanout_supervisor::AgentTaskFanoutWorktreeState;
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(path)
        .output();
    let Ok(status) = status else {
        return (AgentTaskFanoutWorktreeState::Missing, None, None);
    };
    if !status.status.success() {
        return (AgentTaskFanoutWorktreeState::Missing, None, None);
    }
    let worktree = if status.stdout.is_empty() {
        AgentTaskFanoutWorktreeState::Clean
    } else if String::from_utf8_lossy(&status.stdout)
        .lines()
        .any(|line| line.starts_with("UU") || line.starts_with("AA") || line.starts_with("DD"))
    {
        AgentTaskFanoutWorktreeState::Conflicted
    } else {
        AgentTaskFanoutWorktreeState::Dirty
    };
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(path)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    // Observation must be safe for `fanout status`: use the existing
    // remote-tracking ref rather than fetching and mutating the worktree.
    let base = declared_base.and_then(|base| {
        let reference = format!("refs/remotes/origin/{base}");
        Command::new("git")
            .args(["rev-parse", "--verify", &format!("{reference}^{{commit}}")])
            .current_dir(path)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
    });
    (worktree, head, base)
}

fn batch_resume_result(
    report: agent_task_service::AgentTaskCookBatchReport,
    exit_code: i32,
    batch_id: &str,
    portfolio: Option<supervisor::AgentTaskFanoutPortfolioRunReport>,
    placement: Placement,
) -> (Value, i32) {
    (
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-batch-resume/v1",
            "batch_id": report.batch_id,
            "status": report.status,
            "summary": {
                "total": report.total,
                "queued": report.queued,
                "running": report.running,
                "succeeded": report.succeeded,
                "failed": report.failed,
                "cancelled": report.cancelled,
                "timed_out": report.timed_out,
            },
            "cooks": report.cooks,
            "portfolio": portfolio,
            "commands": {
                "status": fanout_command(placement, "status", batch_id),
                "artifacts": fanout_command(placement, "artifacts", batch_id),
                "resume": fanout_command(placement, "resume", batch_id),
            },
        }),
        exit_code,
    )
}

fn batch_artifacts(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    Ok((command_json_value(batch::artifacts(&args.batch_id)?)?, 0))
}

fn run_batch_cook_fanout(
    args: AgentTaskFanoutRunPlanArgs,
    placement: Placement,
) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input, true)?;
    plan.ensure_placement(invocation_placement_directive(placement))?;
    plan.apply_ai_tool_override(args.ai_tool.as_deref());
    plan.apply_max_concurrency_override(args.max_concurrency.map(|value| value as usize));
    plan.apply_max_duration_override(args.max_duration);
    if let Some(record_run_id) = args.record_run_id {
        plan.rekey(record_run_id);
    }
    admit_batch_provider_routes(&mut plan)?;
    run_batch_cook_fanout_plan_with_placement(plan, placement)
}

/// Keeps durable batch coordination on the controller while routing each
/// independent provider attempt through the selected transport.
pub(crate) fn run_batch_cook_fanout_with_attempt_dispatcher_and_placement(
    args: AgentTaskFanoutRunPlanArgs,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
    placement: Placement,
) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input, true)?;
    plan.ensure_placement(invocation_placement_directive(placement))?;
    plan.apply_ai_tool_override(args.ai_tool.as_deref());
    plan.apply_max_concurrency_override(args.max_concurrency.map(|value| value as usize));
    plan.apply_max_duration_override(args.max_duration);
    if let Some(record_run_id) = args.record_run_id {
        plan.rekey(record_run_id);
    }
    admit_batch_provider_routes(&mut plan)?;
    if plan
        .placement
        .as_ref()
        .is_some_and(|placement| placement.selected == EffectiveExecutionPlacement::Local)
    {
        return run_batch_cook_fanout_plan_with_placement(plan, placement);
    }
    run_batch_cook_fanout_plan_with_attempt_dispatcher_and_placement(
        plan,
        attempt_dispatcher,
        placement,
    )
}

/// Persist the durable batch record before dispatching children so
/// `fanout status <fanout_id>` can resolve every child run (#9397). Without
/// this, run-plan admitted children but never wrote
/// `agent-task-batches/<fanout_id>.json`, so status failed with
/// `No such file or directory`.
fn persist_fanout_run_batch_record(
    plan: &BatchCookFanoutPlan,
    placement: Placement,
) -> Result<bool> {
    let children = plan
        .cooks
        .iter()
        .map(|cook| batch::FanoutRunBatchChild {
            task_id: cook.cook_id.clone(),
            run_id: cook.run_id(),
        })
        .collect::<Vec<_>>();
    let record = batch::persist_fanout_run_batch(
        &plan.fanout_id,
        &plan.fanout_id,
        &children,
        serde_json::json!({
            "source": "fanout-run-plan",
            "durable_child_runs": true,
            "placement": plan.placement,
            "replan_command": secure_batch_plan_execution(&plan.fanout_id, placement),
            "dependency_graph": plan.dependency_graph_metadata()?,
        }),
    )?;
    Ok(record.state != batch::AgentTaskBatchState::Planning)
}

fn claim_fanout_run_batch_coordinator(
    plan: &BatchCookFanoutPlan,
    placement: Placement,
) -> Result<(String, bool)> {
    let retry = persist_fanout_run_batch_record(plan, placement)?;
    if let Some(claim_id) = batch::claim_fanout_run_batch(&plan.fanout_id)? {
        let status_command = fanout_command(placement, "status", &plan.fanout_id);
        eprintln!(
            "{{\"event\":\"coordinator_admission_claimed\",\"phase\":\"admitting\",\"children_total\":{},\"next_action\":{}}}",
            plan.cooks.len(),
            serde_json::to_string(&status_command).expect("status command serializes"),
        );
        return Ok((claim_id, retry));
    }
    Err(Error::validation_invalid_argument(
        "fanout_id",
        "agent-task fanout run-plan is already being coordinated; inspect its durable status",
        Some(plan.fanout_id.clone()),
        None,
    ))
}

fn record_batch_failure(plan: &BatchCookFanoutPlan, claim_id: &str, stage: &str, error: &Error) {
    let recorded = batch::record_fanout_run_batch_failure(
        &plan.fanout_id,
        claim_id,
        stage,
        serde_json::json!({ "message": error.message, "details": error.details }),
    );
    if recorded.is_ok() {
        let placement = plan
            .placement
            .as_ref()
            .map(|placement| placement.requested)
            .unwrap_or(Placement::Auto);
        eprintln!(
            "{{\"event\":\"coordinator_failed\",\"phase\":{},\"children_total\":{},\"next_action\":{}}}",
            serde_json::to_string(stage).expect("stage serializes"),
            plan.cooks.len(),
            serde_json::to_string(&fanout_command(placement, "resume", &plan.fanout_id))
            .expect("resume command serializes"),
        );
    }
}

fn run_batch_cook_fanout_plan_with_attempt_dispatcher_and_placement(
    plan: BatchCookFanoutPlan,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
    placement: Placement,
) -> CmdResult<Value> {
    run_batch_cook_fanout_plan_with_attempt_dispatcher_claim(
        plan,
        attempt_dispatcher,
        None,
        placement,
    )
}

#[cfg(test)]
fn run_batch_cook_fanout_plan_with_attempt_dispatcher(
    plan: BatchCookFanoutPlan,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    run_batch_cook_fanout_plan_with_attempt_dispatcher_and_placement(
        plan,
        attempt_dispatcher,
        Placement::Auto,
    )
}

fn run_batch_cook_fanout_plan_with_attempt_dispatcher_claim(
    plan: BatchCookFanoutPlan,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
    claim: Option<(String, bool)>,
    placement: Placement,
) -> CmdResult<Value> {
    let gate_workspace = batch_plan_gate_workspace(&plan)?;
    let gate_contract_validation = validate_batch_gate_contracts(&plan, gate_workspace.as_deref())?;
    let (claim_id, retry) = match claim {
        Some(claim) => claim,
        None => claim_fanout_run_batch_coordinator(&plan, placement)?,
    };
    let previously_terminal = retry
        .then(|| durable_terminal_worktree_paths(&plan))
        .transpose()?;
    let outcome = (|| {
        let heartbeat = CoordinatorHeartbeat::start(
            plan.fanout_id.clone(),
            claim_id.clone(),
            fanout_command(placement, "status", &plan.fanout_id),
        )?;
        persist_batch_cook_recipes(&plan, |options| {
            record_gate_contract_validation(options, &gate_contract_validation);
            options.provider_transport.attempt_dispatcher = Some(attempt_dispatcher(options));
        })?;
        let ready_plan = plan.ready_plan()?;
        let cooks = compile_batch_cooks(&ready_plan, |options| {
            record_gate_contract_validation(options, &gate_contract_validation);
            options.provider_transport.attempt_dispatcher = Some(attempt_dispatcher(options));
        })?;
        let concurrency = batch_concurrency(&plan, &cooks);
        // Resolved once, here, and bound for the whole batch: every worker thread
        // re-binds this same absolute instant, so the budget covers the batch
        // rather than restarting per child.
        let result = with_current_cook_deadline(plan.cook_deadline(), || {
            batch::start_fanout_run_batch(&plan.fanout_id, &claim_id)?;
            agent_task_service::run_cook_batch_with_control(
                agent_task_service::AgentTaskCookBatchOptions {
                    batch_id: plan.fanout_id.clone(),
                    cooks,
                    max_concurrency: concurrency.limit,
                },
                Arc::new(provider::ExtensionProviderAgentTaskExecutor::discover()),
                cook_batch_coordinator_control(&plan.fanout_id, retry),
            )
        });
        heartbeat.finish()?;
        let result = result?;
        finalize_provider_worktrees(&plan, &result.value, previously_terminal.as_ref())?;
        record_terminal_batch_admission_failures(&plan, &result.value)?;
        notify_batch_wave_complete(&plan.fanout_id, &result.value, result.exit_code);
        let result = batch_cook_result(&plan, result, &concurrency);
        Ok(result)
    })();
    if let Err(error) = &outcome {
        record_batch_failure(&plan, &claim_id, "coordinator", error);
    }
    outcome
}

fn run_batch_cook_fanout_plan_with_placement(
    plan: BatchCookFanoutPlan,
    placement: Placement,
) -> CmdResult<Value> {
    run_batch_cook_fanout_plan_with_executor_claim(
        plan,
        Arc::new(provider::ExtensionProviderAgentTaskExecutor::discover()),
        None,
        placement,
    )
}

fn run_batch_cook_fanout_plan_with_executor_claim(
    plan: BatchCookFanoutPlan,
    executor: SharedAgentTaskExecutor,
    claim: Option<(String, bool)>,
    placement: Placement,
) -> CmdResult<Value> {
    let gate_workspace = batch_plan_gate_workspace(&plan)?;
    let gate_contract_validation = validate_batch_gate_contracts(&plan, gate_workspace.as_deref())?;
    let (claim_id, retry) = match claim {
        Some(claim) => claim,
        None => claim_fanout_run_batch_coordinator(&plan, placement)?,
    };
    let previously_terminal = retry
        .then(|| durable_terminal_worktree_paths(&plan))
        .transpose()?;
    let outcome = (|| {
        let heartbeat = CoordinatorHeartbeat::start(
            plan.fanout_id.clone(),
            claim_id.clone(),
            fanout_command(placement, "status", &plan.fanout_id),
        )?;
        persist_batch_cook_recipes(&plan, |options| {
            record_gate_contract_validation(options, &gate_contract_validation);
        })?;
        let ready_plan = plan.ready_plan()?;
        let cooks = compile_batch_cooks(&ready_plan, |options| {
            record_gate_contract_validation(options, &gate_contract_validation);
        })?;
        let concurrency = batch_concurrency(&plan, &cooks);
        // See the sibling runner: the budget is resolved once and bound for the
        // whole batch so it does not restart per child.
        let result = with_current_cook_deadline(plan.cook_deadline(), || {
            batch::start_fanout_run_batch(&plan.fanout_id, &claim_id)?;
            agent_task_service::run_cook_batch_with_control(
                agent_task_service::AgentTaskCookBatchOptions {
                    batch_id: plan.fanout_id.clone(),
                    cooks,
                    max_concurrency: concurrency.limit,
                },
                executor,
                cook_batch_coordinator_control(&plan.fanout_id, retry),
            )
        });
        heartbeat.finish()?;
        let result = result?;
        finalize_provider_worktrees(&plan, &result.value, previously_terminal.as_ref())?;
        record_terminal_batch_admission_failures(&plan, &result.value)?;
        notify_batch_wave_complete(&plan.fanout_id, &result.value, result.exit_code);
        let result = batch_cook_result(&plan, result, &concurrency);
        Ok(result)
    })();
    if let Err(error) = &outcome {
        record_batch_failure(&plan, &claim_id, "coordinator", error);
    }
    outcome
}

fn cook_batch_coordinator_control(
    fanout_id: &str,
    retry: bool,
) -> agent_task_service::AgentTaskCookBatchControl {
    let mut control = agent_task_service::detached_batch_coordinator_control(fanout_id);
    control.skip_durably_terminal_children |= retry;
    // The batch record is the shared foreground/durable projection for every
    // fanout, not only daemon-owned waves.
    control.publish_child_terminalization = true;
    control
}

fn validate_batch_gate_contracts(
    plan: &BatchCookFanoutPlan,
    workspace: Option<&std::path::Path>,
) -> Result<GateContractValidation> {
    let gates = plan
        .cooks
        .iter()
        .flat_map(|cook| cook.verify.iter().chain(&cook.private_verify).cloned())
        .collect::<BTreeSet<_>>();
    if workspace.is_none()
        && gates.iter().any(|gate| {
            matches!(
                gate.split_whitespace().collect::<Vec<_>>().as_slice(),
                ["homeboy", "lint", ..] | ["homeboy", "test", ..]
            )
        })
    {
        return Err(Error::validation_invalid_argument(
            "gate declaration",
            "fanout cannot diagnose a repository script alias before worktree creation because no authoritative registered component workspace is available; register the repository primary or use `homeboy review lint --path .` / `homeboy review test --path .`.",
            None,
            None,
        ));
    }
    // A set ensures shared and child gates are admitted exactly once per wave.
    validate_gate_contracts(
        gates,
        workspace,
        &crate::cli_runtime::current_augmented_command_contract(),
    )
}

fn record_gate_contract_validation(options: &mut CookRequest, validation: &GateContractValidation) {
    options.identity.initial_plan.metadata["gate_contract_validation"] =
        serde_json::to_value(validation).expect("gate contract validation serializes");
}

fn finalize_provider_worktrees(
    plan: &BatchCookFanoutPlan,
    report: &agent_task_service::AgentTaskCookBatchReport,
    previously_terminal: Option<&BTreeMap<String, String>>,
) -> Result<()> {
    let config = homeboy::core::defaults::load_config();
    for cell in &report.cooks {
        if !cell.lifecycle().terminal {
            continue;
        }
        let Some(cook) = plan
            .cooks
            .iter()
            .find(|cook| cook.run_id() == cell.initial_run_id)
        else {
            continue;
        };
        if previously_terminal.is_some_and(|terminal| terminal.contains_key(&cook.run_id())) {
            continue;
        }
        let disposition = if cell.exit_code == 0 {
            homeboy::core::worktree_provider::WorktreeTerminalDisposition::Succeeded
        } else {
            homeboy::core::worktree_provider::WorktreeTerminalDisposition::Failed
        };
        let finalization = homeboy::core::worktree_provider::finalize_worktree_from_config(
            &cook.to_worktree,
            &homeboy::core::worktree_provider::WorktreeProvisionLifecycle {
                purpose: "agent_task_cook".to_string(),
                owner_run_ref: cook.run_id(),
                cleanup_policy:
                    homeboy::core::worktree_provider::WorktreeCleanupPolicy::RemoveOnSuccess,
            },
            disposition,
            &config,
        )?;
        if matches!(
            finalization,
            homeboy::core::worktree_provider::WorktreeFinalizationLookup::NotFound
        ) && configured_provider_workspace_creation()?
        {
            return Err(
                homeboy::core::worktree_provider::worktree_finalization_not_found_error(
                    &cook.to_worktree,
                    &config,
                ),
            );
        }
    }
    Ok(())
}

/// HOOK — wave-completion notification. Intentionally not implemented here.
///
/// # The gap this marks
///
/// A batch emits no notification of its own. Each child cook notifies
/// independently, so a ten-child wave delivers ten unrelated cook messages and
/// never says "wave done: 7 green, 2 need attention, 1 blocked" — even though
/// `AgentTaskCookBatchReport` has already computed exactly those totals by the
/// time this is called. The one message an operator actually wants is the one
/// that is missing.
///
/// # Why it is a hook and not a call
///
/// The notification emitter (`agent_task_notify.rs` / `notify.rs`) is owned by
/// another change in flight. Reaching into it from here would collide. This
/// marks the exact seam instead.
///
/// # What the emitter needs
///
/// This is called once per coordinator run, after `record_terminal_batch_admission_failures`
/// has converged the durable record and after `batch_cook_result` has built the
/// envelope, so every total is final and durable. It is deliberately
/// infallible-by-signature: a wave that finished must not be failed by a
/// notification transport.
///
/// The envelope passed here already carries everything an emitter needs, and
/// carries it in the shape `fanout status` uses:
///
/// * `fanout_id: &str` — the durable batch id; `agent-task fanout status <id>`
///   is the follow-up command the message should name.
/// * `result["status"]` — the aggregate outcome string
///   (`succeeded` / `partial_failure` / `failed` / `cancelled` / `timed_out`).
/// * `result["summary"]` — `{total, queued, running, succeeded, failed,
///   cancelled, timed_out}`, the "7 green, 2 need attention" numbers.
/// * `result["concurrency"]` — `{limit, source, reason}`, worth including when
///   a resource budget lowered the ceiling, since that explains a slow wave.
/// * `result["cooks"][]` — per child: `cook_id`, `run_id`, `worktree`, `head`,
///   `exit_code`. A child-level digest should project from these rather than
///   from `result["cooks"][].result`, which embeds the full cook report and can
///   quote provider output.
///
/// The suggested signature when the emitter lands:
///
/// ```ignore
/// agent_task_notify::notify_fanout_wave_complete(
///     fanout_id: &str,
///     summary: &Value,   // result["summary"]
///     status: &str,      // result["status"]
/// ) -> Result<()>        // errors logged and dropped here, never propagated
/// ```
///
/// The route is already bound: the coordinator captures the caller's
/// thread-local `notification_route` and re-binds it onto every worker, and the
/// detached launcher passes it explicitly through `notification_route::child_env`,
/// so `notification_route::current()` is correct at this point in both the
/// attached and the detached coordinator.
fn notify_batch_wave_complete(
    fanout_id: &str,
    report: &agent_task_service::AgentTaskCookBatchReport,
    exit_code: i32,
) {
    // `fanout_id` rather than `report.batch_id`: the once-claim must be stable
    // across `fanout resume`, which reuses the fanout id and mints a new batch
    // record. Portfolio counts are `None` here -- `ready`/`blocked`/`merged` are
    // the supervisor's view, and a plain cook batch has no portfolio.
    homeboy::agents::agent_task_notify::batch_terminal(report, fanout_id, None, None, exit_code);
}

/// Recipes are the durable restart boundary. Persist blocked dependents before
/// dispatching any sibling so a later merge can release them through `resume`
/// without reconstructing mutable operator input or re-planning a branch.
fn persist_batch_cook_recipes(
    plan: &BatchCookFanoutPlan,
    configure: impl Fn(&mut CookRequest),
) -> Result<()> {
    for options in compile_batch_cooks(plan, configure)? {
        agent_task_service::persist_initial_recipe(&options)?;
    }
    Ok(())
}

/// A failure before Cook creates its durable child record is nevertheless a
/// terminal outcome for this controller-owned fanout. Persist it on the batch
/// so later status reads do not leave the child running and unavailable.
fn record_terminal_batch_admission_failures(
    plan: &BatchCookFanoutPlan,
    report: &agent_task_service::AgentTaskCookBatchReport,
) -> Result<()> {
    let failed = report
        .cooks
        .iter()
        .filter(|cell| cell.result.is_none() && cell.exit_code != 0)
        .map(|cell| cell.initial_run_id.as_str());
    batch::record_fanout_run_batch_failed_admissions(&plan.fanout_id, failed)
}

fn batch_harvest_context() -> Result<homeboy::agents::agent_task_scheduler::HarvestExecutionContext>
{
    if std::env::var_os("HOMEBOY_RUNNER_HOSTED_EXEC").is_some() {
        homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
    } else {
        Ok(homeboy::agents::agent_task_scheduler::HarvestExecutionContext::default())
    }
}

fn compile_batch_cooks(
    plan: &BatchCookFanoutPlan,
    configure: impl Fn(&mut CookRequest),
) -> Result<Vec<CookRequest>> {
    let harvest_context = batch_harvest_context()?;
    let mut readiness_cache = provider::ProviderRuntimeReadinessCache::default();
    plan.cooks
        .iter()
        .map(|cook| {
            let invocation = cook.to_cook_invocation(plan)?;
            let mut options = agent_task_service::compile_cook_attempt_with_readiness_cache(
                invocation.options,
                invocation.dispatch,
                &mut readiness_cache,
            )?;
            if !cook.repository_identity.is_null() {
                options.identity.initial_plan.metadata["cook_repository_identity"] =
                    cook.repository_identity.clone();
            }
            options.identity.initial_plan.metadata["batch_id"] =
                Value::String(plan.fanout_id.clone());
            if let (Some(workspace), Some(component_id)) = (
                options.workspace.source_worktree_path.as_deref(),
                cook.component_id.as_deref(),
            ) {
                super::run::bind_cook_component_workspace(
                    &mut options.identity.initial_plan,
                    workspace,
                    component_id,
                )?;
            }
            attach_fanout_placement_decision(plan, &mut options)?;
            if let Some(executor) = options
                .identity
                .initial_plan
                .tasks
                .first()
                .map(|task| &task.executor)
            {
                let model = executor.model().map(str::to_string);
                options.ai_disclosure.ai_tool = resolve_ai_tool_disclosure(
                    &options.ai_disclosure.ai_tool,
                    Some(&executor.backend),
                    executor.selector.as_deref(),
                    model.as_deref(),
                );
                options.ai_disclosure.ai_model = model;
            }
            options.harvest_context = harvest_context.clone();
            configure(&mut options);
            enforce_fanout_placement(&options)?;
            Ok(options)
        })
        .collect()
}

fn enforce_fanout_placement(options: &CookRequest) -> Result<()> {
    if options.provider_transport.attempt_dispatcher.is_some() {
        return Ok(());
    }
    let decision = options
        .identity
        .initial_plan
        .metadata
        .get("execution_placement_decision")
        .cloned()
        .and_then(|value| {
            serde_json::from_value::<homeboy_lab_runner_contract::ExecutionPlacementDecision>(value)
                .ok()
        });
    if decision.is_some_and(|decision| decision.required == ExecutionPlacementRequirement::Lab) {
        return Err(Error::validation_invalid_argument(
            "placement",
            "required Lab fanout placement has no child attempt dispatcher; no provider workload executed locally",
            Some("lab".to_string()),
            None,
        ));
    }
    Ok(())
}

fn attach_fanout_placement_decision(
    plan: &BatchCookFanoutPlan,
    options: &mut CookRequest,
) -> Result<()> {
    let Some(directive) = plan.placement.as_ref() else {
        return Ok(());
    };
    let task = options.identity.initial_plan.tasks.first().ok_or_else(|| {
        Error::validation_invalid_argument(
            "placement",
            "fanout child plan has no task identity for placement finalization",
            Some(options.identity.cook_id.clone()),
            None,
        )
    })?;
    let source_path = task.workspace.root.as_deref().map(Path::new);
    let identity = ExecutionPlacementIdentity {
        repository: source_path
            .and_then(Path::file_name)
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| "runner-resident-or-unmaterialized".to_string()),
        workspace: source_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "runner-resident-or-unmaterialized".to_string()),
        task: task.task_id.clone(),
        candidate: source_path.and_then(homeboy::core::git::head_sha),
        base: source_path.and_then(|path| homeboy::core::git::rev_parse(path, "origin/HEAD")),
    };
    options.identity.initial_plan.metadata["execution_placement_decision"] =
        serde_json::to_value(directive.finalize(identity)).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize fanout child placement decision".to_string()),
            )
        })?;
    Ok(())
}

/// Resolve how many children this batch may run at once.
///
/// This used to be `min(children, available_parallelism())`, which is not a
/// limit: an eight-core host started eight concurrent cooks regardless of what
/// those cooks did. A cook whose gate compiles can consume tens of gigabytes of
/// disk, so the core count is the wrong quantity entirely. The ceiling now
/// comes from the operator, the host config, or the plan's resource budget —
/// resolved by the scheduler's own resource module so the batch path and the
/// scheduler apply one rule rather than two.
fn batch_concurrency(
    plan: &BatchCookFanoutPlan,
    cooks: &[CookRequest],
) -> BatchConcurrencyDecision {
    // A batch coordinator commits nothing before it starts, so the budget is
    // read at zero active units. The budget itself is the children's own, taken
    // from the first compiled plan: every child in a batch is compiled from the
    // same host policy.
    let resource_budget = cooks
        .first()
        .map(|cook| cook.identity.initial_plan.options.resource_budget.clone())
        .unwrap_or_default();
    resolve_batch_concurrency(BatchConcurrencyInputs {
        requested: plan.max_concurrency,
        configured: homeboy_core::defaults::load_config()
            .agent_task
            .max_batch_concurrency,
        default_limit: homeboy_core::defaults::DEFAULT_AGENT_TASK_MAX_BATCH_CONCURRENCY,
        resource_budget: &resource_budget,
        active_units: 0,
        child_count: cooks.len(),
    })
}

fn batch_cook_result(
    plan: &BatchCookFanoutPlan,
    result: agent_task_service::AgentTaskRunResult<agent_task_service::AgentTaskCookBatchReport>,
    concurrency: &BatchConcurrencyDecision,
) -> (Value, i32) {
    let placement = plan
        .placement
        .as_ref()
        .map(|placement| placement.requested)
        .unwrap_or(Placement::Auto);
    let report = result.value;
    let cooks = report
        .cooks
        .iter()
        .zip(&plan.cooks)
        .map(|(cell, cook)| {
            // A child with no Cook report is the case that most needs
            // structure: its `error` envelope is the only thing the caller
            // gets, so it is passed through whole rather than rendered.
            let cell_result = cell
                .result
                .as_ref()
                .map(|result| serde_json::to_value(result).unwrap_or(Value::Null))
                .unwrap_or_else(|| serde_json::json!({ "error": cell.error }));
            let lifecycle = cell.lifecycle();
            serde_json::json!({
                "cook_id": cook.cook_id,
                "run_id": cook.run_id(),
                "worktree": cook.to_worktree,
                "head": cook.head,
                "workspace_materialization": cook.workspace_materialization,
                "exit_code": cell.exit_code,
                // The closed-vocabulary classification, so a caller reading
                // this envelope out of a file or an HTTP response can decide
                // completion and retry without a process exit code.
                "lifecycle_status": lifecycle.lifecycle_status,
                "terminal": lifecycle.terminal,
                "retryable": lifecycle.retryable,
                "result": cell_result,
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "schema": AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
            "fanout_id": plan.fanout_id,
            "status": report.status,
            // Reported so an operator can tell a deliberate ceiling from one a
            // resource budget imposed, without re-deriving it from inputs that
            // are no longer on hand.
            "concurrency": {
                "limit": concurrency.limit,
                "source": concurrency.source.as_str(),
                "reason": concurrency.reason,
            },
            "summary": {
                "total": report.total,
                "queued": report.queued,
                "running": report.running,
                "succeeded": report.succeeded,
                "failed": report.failed,
                "cancelled": report.cancelled,
                "timed_out": report.timed_out,
            },
            "cooks": cooks,
            "commands": {
                "status": fanout_command(placement, "status", &plan.fanout_id),
                "artifacts": fanout_command(placement, "artifacts", &plan.fanout_id),
            },
        }),
        result.exit_code,
    )
}

#[cfg(test)]
fn cook_batch(args: AgentTaskFanoutCookBatchArgs) -> CmdResult<Value> {
    cook_batch_with_placement(args, Placement::Auto)
}

fn cook_batch_with_placement(
    args: AgentTaskFanoutCookBatchArgs,
    placement: Placement,
) -> CmdResult<Value> {
    cook_batch_inner(args, None, placement)
}

fn cook_batch_inner(
    mut args: AgentTaskFanoutCookBatchArgs,
    attempt_dispatcher: Option<&CookAttemptDispatcherFactory>,
    placement: Placement,
) -> CmdResult<Value> {
    if args.preview {
        return cook_batch_dry_run(args, placement);
    }
    args.gates.snapshot_file_inputs()?;
    normalize_cook_batch_repo_with_placement(&mut args, placement)?;
    resolve_cook_batch_default_branch(&mut args)?;
    apply_provider_profile(&mut args);
    // Planning and execution share this admission. Only checks that require a
    // materialized workspace or a live runtime remain deferred.
    resolve_and_validate_effective_backend(&mut args)?;
    let mut plan = build_cook_batch_plan(&args)?;
    plan.ensure_placement(invocation_placement_directive(placement))?;
    let replay_args = pin_cook_batch_replay(&args, &plan.fanout_id);
    let plan_ref = batch_plan_reference(&plan)?;
    let plan_has_private_gates = plan
        .cooks
        .iter()
        .any(|cook| !cook.private_verify.is_empty());
    let persisted = args.run_plan && !args.preview;
    let claim = persisted
        .then(|| claim_fanout_run_batch_coordinator(&plan, placement))
        .transpose()?;
    if let Err(error) = validate_batch_cook_gates(&plan, batch_gate_workspace(&args)?) {
        record_batch_preflight_failure(
            claim.as_ref().map(|(claim_id, _)| claim_id.as_str()),
            &plan,
            "gate_preflight",
            &error,
        )?;
        return Err(error);
    }
    let previously_terminal = claim
        .as_ref()
        .is_some_and(|(_, retry)| *retry)
        .then(|| durable_terminal_worktree_paths(&plan))
        .transpose()?;
    let retry = claim.as_ref().is_some_and(|(_, retry)| *retry);
    let (worktrees, worktree_resolution) = match queue_or_reuse_worktrees_with_terminal_paths(
        &args,
        &plan,
        previously_terminal.as_ref(),
        retry,
    ) {
        Ok(resolution) => resolution,
        Err(error) => {
            record_batch_preflight_failure(
                claim.as_ref().map(|(claim_id, _)| claim_id.as_str()),
                &plan,
                "worktree_preflight",
                &error,
            )?;
            return Err(error);
        }
    };
    bind_materialized_worktree_paths(&mut plan, &worktrees);
    let blocked = worktrees
        .rows
        .iter()
        .filter(|row| {
            !matches!(
                row.status,
                worktree::WorktreeQueueCreateStatus::Created
                    | worktree::WorktreeQueueCreateStatus::WouldCreate
            )
        })
        .count();
    if persisted && blocked > 0 {
        batch::record_fanout_run_batch_failure(
            &plan.fanout_id,
            claim
                .as_ref()
                .map(|(claim_id, _)| claim_id.as_str())
                .expect("persisted coordinator claim"),
            "worktree_preflight",
            serde_json::json!({
                "worktrees": worktrees.rows,
                "resolution": worktree_resolution,
            }),
        )?;
    }
    let can_run = !args.preview
        && blocked == 0
        && worktrees
            .rows
            .iter()
            .all(|row| matches!(row.status, worktree::WorktreeQueueCreateStatus::Created));
    let private_artifact_path = if can_run && plan_has_private_gates {
        if let Err(error) = bind_materialized_worktrees(&mut plan, &worktrees) {
            record_batch_preflight_failure(
                claim.as_ref().map(|(claim_id, _)| claim_id.as_str()),
                &plan,
                "worktree_preflight",
                &error,
            )?;
            return Err(error);
        }
        Some(persist_private_batch_plan(&plan)?)
    } else {
        if can_run {
            if let Err(error) = bind_materialized_worktrees(&mut plan, &worktrees) {
                record_batch_preflight_failure(
                    claim.as_ref().map(|(claim_id, _)| claim_id.as_str()),
                    &plan,
                    "worktree_preflight",
                    &error,
                )?;
                return Err(error);
            }
        }
        None
    };
    if can_run {
        // Compare the exact workspace-bound recipe that provider execution will
        // persist, not the handle-only planning form created before worktree
        // materialization.
        if let Err(error) = preflight_batch_cook_recipes(&plan, attempt_dispatcher) {
            record_batch_preflight_failure(
                claim.as_ref().map(|(claim_id, _)| claim_id.as_str()),
                &plan,
                "provider_preflight",
                &error,
            )?;
            return Err(error);
        }
    }
    let run_result = if args.run_plan && can_run {
        let (value, exit_code) = match attempt_dispatcher {
            Some(dispatcher) => run_batch_cook_fanout_plan_with_attempt_dispatcher_claim(
                plan.clone(),
                dispatcher,
                claim.clone(),
                placement,
            )?,
            None => run_batch_cook_fanout_plan_with_executor_claim(
                plan.clone(),
                Arc::new(provider::ExtensionProviderAgentTaskExecutor::discover()),
                claim.clone(),
                placement,
            )?,
        };
        Some(serde_json::json!({ "exit_code": exit_code, "result": value }))
    } else {
        None
    };
    let status = if args.run_plan && run_result.is_some() {
        run_result
            .as_ref()
            .and_then(|value| value["result"]["status"].as_str())
            .unwrap_or("completed")
    } else if blocked > 0 {
        "blocked"
    } else if args.preview {
        "ready"
    } else {
        "ready"
    };
    // A completed batch's aggregate result is authoritative. Worktree blocking
    // remains a pre-execution failure, while child failures retain their durable
    // evidence and produce the same nonzero result at every CLI boundary.
    let exit_code = cook_batch_outer_exit_code(blocked, &run_result);
    let resume_legal = run_result
        .as_ref()
        .is_some_and(|result| batch_resume_is_legal(&result["result"]));
    let (primary_failure, causal_failures) =
        fanout_failure_projection(&worktrees, run_result.as_ref());

    Ok((
        serde_json::json!({
                "schema": "homeboy/agent-task-cook-batch/v1",
                "fanout_id": plan.fanout_id,
                "status": status,
                "dry_run": args.preview,
                "summary": {
                    "issues": plan.cooks.len(),
                     "worktrees_total": worktrees.rows.len(),
                     "worktrees_blocked": blocked,
                     "causal_worktree_failures": causal_failures["total"],
                 },
                "primary_failure": primary_failure,
                "causal_failures": causal_failures,
                "preflight": {
                    "default_branch": args.base_resolution.clone(),
                    "provider_readiness_command": provider_readiness_command(&args),
                    "provider_selection": provider_selection_preflight(&args),
                    "deferred_live_checks": ["provider_runtime_readiness", "workspace_materialization"],
                    "placement": fanout_placement_preflight(plan.placement.as_ref()),
                    "deterministic_gates": effective_batch_cook_gates(&plan)
                },
                "worktrees": worktrees,
                "worktree_resolution": worktree_resolution,
                "plan": public_batch_cook_plan(&plan),
                "run_result": run_result,
        "plan_ref": plan_ref,
        "commands": cook_batch_commands_with_placement(&replay_args, placement, plan_has_private_gates, private_artifact_path.as_deref()),
                // Named run-plan persists before worktree/provider preflight, so
                // status and artifacts remain available when admission is blocked.
                "next_actions": cook_batch_next_actions_with_placement(
                    &replay_args,
                    placement,
                    &plan.fanout_id,
                    status,
                    persisted,
                    resume_legal,
                    &worktrees,
                    plan_has_private_gates,
                    private_artifact_path.as_deref(),
                ),
            }),
        exit_code,
    ))
}

fn blocked_worktree_failure_projection(
    worktrees: &worktree::WorktreeQueueCreateOutput,
) -> (Option<Value>, Value) {
    let causal_rows = worktrees
        .rows
        .iter()
        .enumerate()
        .filter(|(_, row)| {
            matches!(
                row.status,
                worktree::WorktreeQueueCreateStatus::Failed
                    | worktree::WorktreeQueueCreateStatus::ActiveLockHolder
            )
        })
        .collect::<Vec<_>>();
    let total = causal_rows.len();
    let mut failures = causal_rows
        .into_iter()
        .take(COMPACT_FANOUT_FAILURE_LIMIT)
        .map(|(index, row)| {
            let fallback_classification = match row.status {
                worktree::WorktreeQueueCreateStatus::ActiveLockHolder => "active_lock_holder",
                _ => "worktree_creation_failed",
            };
            let reason = row
                .failure
                .as_ref()
                .map(|failure| failure.message.as_str())
                .or(row.error.as_deref())
                .unwrap_or(fallback_classification);
            serde_json::json!({
                "phase": "worktree_preflight",
                "cause_phase": row.failure.as_ref().map(|failure| failure.phase.as_str()),
                "row": index,
                "handle": row.handle,
                "provider_id": row.failure.as_ref().and_then(|failure| failure.provider_id.as_deref()),
                "classification": row.failure.as_ref().map(|failure| failure.classification.as_str()).unwrap_or(fallback_classification),
                "reason": reason,
                "next_action": quote_args(&row.command),
                "evidence_path": format!("worktrees.rows[{index}]"),
            })
        })
        .collect::<Vec<_>>();
    let returned = failures.len();
    let primary = (!failures.is_empty()).then(|| failures.remove(0));
    let summary = serde_json::json!({
        "total": total,
        "returned": returned,
        "omitted": total.saturating_sub(returned),
        "additional_failures": failures,
        "complete_evidence_path": "worktrees.rows",
    });
    (primary, summary)
}

fn fanout_failure_projection(
    worktrees: &worktree::WorktreeQueueCreateOutput,
    run_result: Option<&Value>,
) -> (Option<Value>, Value) {
    let worktree_projection = blocked_worktree_failure_projection(worktrees);
    if worktree_projection.0.is_some() {
        return worktree_projection;
    }
    child_failure_projection(run_result)
}

fn child_failure_projection(run_result: Option<&Value>) -> (Option<Value>, Value) {
    let Some(cooks) = run_result
        .and_then(|value| value.pointer("/result/cooks"))
        .and_then(Value::as_array)
    else {
        return empty_causal_failure_projection();
    };
    let mut groups: Vec<(String, Value, Vec<Value>)> = Vec::new();
    let mut failed_children = 0usize;
    for (index, child) in cooks.iter().enumerate() {
        if child["exit_code"].as_i64().unwrap_or_default() == 0 {
            continue;
        }
        let Some(cause) = child_causal_failure(child) else {
            continue;
        };
        failed_children += 1;
        let key = serde_json::to_string(&serde_json::json!({
            "phase": cause["phase"],
            "classification": cause["classification"],
            "reason": cause["reason"],
            "provider_budget_consumed": cause["provider_budget_consumed"],
            "provider_executions_consumed": cause["provider_executions_consumed"],
        }))
        .expect("bounded child cause serializes");
        let planned_run_id = child["run_id"]
            .as_str()
            .filter(|run_id| !run_id.is_empty())
            .or_else(|| {
                child["initial_run_id"]
                    .as_str()
                    .filter(|run_id| !run_id.is_empty())
            })
            .unwrap_or_default();
        let latest_run_id = child
            .pointer("/result/latest_run_id")
            .and_then(Value::as_str)
            .filter(|run_id| !run_id.is_empty())
            .unwrap_or(planned_run_id);
        let child_ref = serde_json::json!({
            "cook_id": child["cook_id"],
            "run_id": planned_run_id,
            "initial_run_id": child["initial_run_id"],
            "latest_run_id": latest_run_id,
            "evidence_ref": format!("homeboy://agent-task/run/{latest_run_id}/status"),
            "diagnose_command": format!("homeboy agent-task diagnose {latest_run_id} --full"),
            "result_path": format!("run_result.result.cooks[{index}]"),
            "recovery": cause["recovery"],
        });
        if let Some((_, _, child_refs)) = groups.iter_mut().find(|(known, _, _)| known == &key) {
            child_refs.push(child_ref);
        } else {
            groups.push((key, cause, vec![child_ref]));
        }
    }
    if groups.is_empty() {
        return empty_causal_failure_projection();
    }

    let mut projected = groups
        .into_iter()
        .map(|(_, mut cause, child_references)| {
            cause["affected_child_count"] = serde_json::json!(child_references.len());
            cause["child_references"] = Value::Array(child_references);
            cause
        })
        .collect::<Vec<_>>();
    let unique_causes = projected.len();
    let primary = projected.remove(0);
    (
        Some(primary),
        serde_json::json!({
            "total": failed_children,
            "returned": failed_children,
            "omitted": 0,
            "unique_causes": unique_causes,
            "additional_failures": projected,
            "complete_evidence_path": "run_result.result.cooks",
        }),
    )
}

fn child_causal_failure(child: &Value) -> Option<Value> {
    let result = child.get("result");
    let context = result.and_then(|result| result.get("failure_context"));
    let latest_run_id = result
        .and_then(|result| result.get("latest_run_id"))
        .and_then(Value::as_str)
        .filter(|run_id| !run_id.is_empty())
        .or_else(|| {
            child
                .get("run_id")
                .and_then(Value::as_str)
                .filter(|run_id| !run_id.is_empty())
        })
        .or_else(|| {
            child
                .get("initial_run_id")
                .and_then(Value::as_str)
                .filter(|run_id| !run_id.is_empty())
        });
    let durable_diagnostic = context
        .and_then(|context| context.get("diagnostic"))
        .cloned()
        .or_else(|| latest_run_id.and_then(agent_task_service::attempt_primary_failure_diagnostic));
    let diagnostic = durable_diagnostic.as_ref();
    let primary = result.and_then(|result| result.get("primary_failure"));
    let error = child
        .get("error")
        .filter(|value| !value.is_null())
        .or_else(|| {
            result
                .and_then(|result| result.get("error"))
                .filter(|value| !value.is_null())
        });
    let phase = result
        .and_then(|result| result.get("terminal_phase"))
        .or_else(|| context.and_then(|context| context.get("phase")))
        .or_else(|| primary.and_then(|primary| primary.get("phase")))
        .or_else(|| error.and_then(|error| error.pointer("/details/pre_execution_phase")))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let classification = result
        .and_then(|result| result.get("terminal_failure_classification"))
        .or_else(|| diagnostic.and_then(|diagnostic| diagnostic.get("class")))
        .or_else(|| context.and_then(|context| context.get("reason_code")))
        .or_else(|| diagnostic.and_then(|diagnostic| diagnostic.get("code")))
        .or_else(|| error.and_then(|error| error.get("code")))
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    let phase = if classification.starts_with("agent_task.committed_harvest_")
        && matches!(phase, "unknown" | "provider")
    {
        "committed_harvest_preflight"
    } else {
        phase
    };
    let reason = diagnostic
        .and_then(|diagnostic| diagnostic.get("message"))
        .or_else(|| primary.and_then(|primary| primary.get("stderr_excerpt")))
        .or_else(|| error.and_then(|error| error.get("message")))
        .or_else(|| result.and_then(|result| result.get("stop_reason")))
        .and_then(Value::as_str)?;
    let recovery = context
        .and_then(|context| context.get("next_actions"))
        .and_then(Value::as_array)
        .and_then(|actions| actions.first())
        .or_else(|| primary.and_then(|primary| primary.get("next_action")))
        .or_else(|| {
            context
                .and_then(|context| context.get("legal_actions"))
                .and_then(Value::as_array)
                .and_then(|actions| actions.first())
        })
        .cloned();
    let next_action = recovery
        .as_ref()
        .and_then(|recovery| recovery.get("command"))
        .and_then(Value::as_str);

    Some(serde_json::json!({
        "phase": phase,
        "classification": classification,
        "reason": reason,
        "provider_budget_consumed": context.and_then(|context| context.get("provider_budget_consumed")).cloned().unwrap_or(Value::Null),
        "provider_executions_consumed": context.and_then(|context| context.get("provider_executions_consumed")).cloned().unwrap_or(Value::Null),
        "recovery": recovery,
        "next_action": next_action,
    }))
}

fn empty_causal_failure_projection() -> (Option<Value>, Value) {
    (
        None,
        serde_json::json!({
            "total": 0,
            "returned": 0,
            "omitted": 0,
            "additional_failures": [],
            "complete_evidence_path": "worktrees.rows",
        }),
    )
}

/// Static dry-run deliberately stops before any repository, provider, workspace,
/// gate-file, or evidence-file hydration. This makes the planner's wall-clock
/// bound enforceable without spawning an unkillable helper around a synchronous
/// dependency.
fn cook_batch_dry_run(
    mut args: AgentTaskFanoutCookBatchArgs,
    placement: Placement,
) -> CmdResult<Value> {
    let mut planner = DryRunPlanner::new(&args, placement);
    planner.begin("gate_inputs");
    if args.issues.len() > DRY_RUN_MAX_ISSUES {
        return Err(planner.defer("gate_inputs", "bounded issue list"));
    }
    if args
        .issues
        .iter()
        .any(|issue| issue.len() > DRY_RUN_MAX_GATE_BYTES)
        || args.repo.len() > DRY_RUN_MAX_GATE_BYTES
        || args
            .prompt_template
            .as_deref()
            .is_some_and(|template| template.len() > DRY_RUN_MAX_INLINE_JSON_BYTES)
    {
        return Err(planner.defer("gate_inputs", "bounded static identifier input"));
    }
    if args
        .verification_profiles
        .as_deref()
        .is_some_and(|spec| spec.len() > DRY_RUN_MAX_INLINE_JSON_BYTES)
        || args
            .gates
            .verify
            .iter()
            .chain(&args.gates.private_verify)
            .any(|gate| gate.len() > DRY_RUN_MAX_GATE_BYTES)
    {
        return Err(planner.defer("gate_inputs", "bounded inline planning input"));
    }
    if !static_repeatable_inputs_are_bounded(&args) {
        return Err(planner.defer("gate_inputs", "bounded repeatable static planning input"));
    }
    if !args.gates.verify_file.is_empty()
        || !args.gates.private_verify_file.is_empty()
        || !args.provider_evidence_inputs.is_empty()
    {
        return Err(planner.defer("gate_inputs", "file-backed gate or provider evidence input"));
    }
    if args
        .verification_profiles
        .as_deref()
        .is_some_and(|spec| spec.starts_with('@') || spec.trim() == "-")
    {
        return Err(planner.defer("gate_inputs", "file-backed verification profiles"));
    }
    planner.finish("declared gate inputs")?;
    let mut normalized_args = args.clone();
    normalized_args =
        planner.run_bounded("repository", "registered primary repository", move || {
            normalize_static_cook_batch_repo_with_placement(&mut normalized_args, placement)?;
            resolve_cook_batch_default_branch(&mut normalized_args)?;
            Ok(normalized_args)
        })?;
    args = normalized_args;
    // Profile/default resolution is part of the effective child identity. It is
    // local catalog/config projection only; readiness and provider execution
    // remain deferred to the replayed run.
    let mut selected_args = args.clone();
    selected_args = planner.run_bounded(
        "provider_selection",
        "static provider selection",
        move || {
            apply_provider_profile(&mut selected_args);
            resolve_and_validate_effective_backend(&mut selected_args)?;
            Ok(selected_args)
        },
    )?;
    args = selected_args;
    let static_args = args.clone();
    let mut plan = planner.run_bounded(
        "issues_and_gates",
        "supplied issue URLs and gate declarations",
        move || build_static_cook_batch_plan(&static_args),
    )?;
    plan.ensure_placement(invocation_placement_directive(placement))?;
    let replay_args = pin_cook_batch_replay(&args, &plan.fanout_id);
    let plan_ref = batch_plan_reference(&plan)?;
    let workspace_args = args.clone();
    let workspace = planner.run_bounded(
        "gate_workspace",
        "authoritative registered workspace",
        move || batch_gate_workspace(&workspace_args),
    )?;
    let gate_plan = plan.clone();
    planner.run_bounded(
        "gate_contracts",
        "static deterministic gate declarations",
        move || validate_batch_cook_gates(&gate_plan, workspace),
    )?;
    let worktrees = planner.run("worktrees", "static worktree projection", || {
        Ok(static_worktrees_dry_run(&args, &plan))
    })?;
    let plan_has_private_gates =
        planner.run("recipe_declarations", "immutable cook declarations", || {
            Ok(plan
                .cooks
                .iter()
                .any(|cook| !cook.private_verify.is_empty()))
        })?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-cook-batch/v1",
            "fanout_id": plan.fanout_id,
            "status": "ready",
            "dry_run": true,
            "summary": { "issues": plan.cooks.len(), "worktrees_total": worktrees.rows.len(), "worktrees_blocked": 0 },
            "preflight": {
                "default_branch": args.base_resolution.clone(),
                "provider_readiness_command": provider_readiness_command(&args),
                "provider_selection": provider_selection_preflight(&args),
                "deferred_live_checks": ["provider_runtime_readiness", "workspace_materialization"],
                "placement": fanout_placement_preflight(plan.placement.as_ref()),
                "deterministic_gates": effective_batch_cook_gates(&plan),
            },
            "worktrees": worktrees,
            "plan": public_batch_cook_plan(&plan),
            "plan_ref": plan_ref,
            "run_result": Value::Null,
            "commands": cook_batch_commands_with_placement(&replay_args, placement, plan_has_private_gates, None),
            "next_actions": cook_batch_next_actions_with_placement(&replay_args, placement, &plan.fanout_id, "ready", false, false, &worktrees, plan_has_private_gates, None),
        }),
        0,
    ))
}

fn static_repeatable_inputs_are_bounded(args: &AgentTaskFanoutCookBatchArgs) -> bool {
    let gates = &args.gates;
    let count = gates.verify.len()
        + gates.private_verify.len()
        + gates.input_sources.len()
        + gates.gate_toolchains.len()
        + gates.gate_toolchain_specs.len()
        + gates.gate_package_artifacts.len()
        + gates.gate_extension_inputs.len()
        + gates.gate_environment.len()
        + gates.gate_environment_preserve.len()
        + args.secret_env.len();
    if count > DRY_RUN_MAX_ISSUES {
        return false;
    }
    let mut strings = gates
        .verify
        .iter()
        .chain(&gates.private_verify)
        .chain(&gates.gate_toolchains)
        .chain(&args.secret_env);
    if strings.any(|value| value.len() > DRY_RUN_MAX_GATE_BYTES) {
        return false;
    }
    serde_json::to_vec(&(
        &gates.input_sources,
        &gates.gate_toolchain_specs,
        &gates.gate_package_artifacts,
        &gates.gate_extension_inputs,
        &gates.gate_environment,
        &gates.gate_environment_preserve,
    ))
    .is_ok_and(|encoded| encoded.len() <= DRY_RUN_MAX_INLINE_JSON_BYTES)
}

/// Dry-run accepts a registered primary path because it can be normalized from
/// registration and bounded Git metadata without invoking Git or a provider.
fn normalize_static_cook_batch_repo_with_placement(
    args: &mut AgentTaskFanoutCookBatchArgs,
    placement: Placement,
) -> Result<()> {
    let resolution = homeboy::core::component::resolve_registered_primary_identity(&args.repo)?;
    match resolution {
        homeboy::core::component::RegisteredPrimaryPathResolution::Primary(id) => {
            args.repo = id;
            normalize_registered_cook_batch_repo(args)
        }
        resolution => Err(invalid_cook_batch_repo_path(args, resolution, placement)),
    }
}

fn record_batch_preflight_failure(
    claim_id: Option<&str>,
    plan: &BatchCookFanoutPlan,
    stage: &str,
    error: &Error,
) -> Result<()> {
    let Some(claim_id) = claim_id else {
        return Ok(());
    };
    batch::record_fanout_run_batch_failure(
        &plan.fanout_id,
        claim_id,
        stage,
        serde_json::json!({ "message": error.message, "details": error.details }),
    )
}

#[cfg(test)]
fn normalize_cook_batch_repo(args: &mut AgentTaskFanoutCookBatchArgs) -> Result<()> {
    normalize_cook_batch_repo_with_placement(args, Placement::Auto)
}

fn normalize_cook_batch_repo_with_placement(
    args: &mut AgentTaskFanoutCookBatchArgs,
    placement: Placement,
) -> Result<()> {
    let handle_like = args.repo.contains('@');
    let path_like = std::path::Path::new(&args.repo).is_absolute()
        || args.repo.contains(std::path::MAIN_SEPARATOR)
        || std::path::Path::new(&args.repo).exists();

    if handle_like && !path_like {
        let candidates = args
            .repo
            .split_once('@')
            .and_then(|(id, _)| {
                homeboy::core::component::inventory::registered_base()
                    .ok()
                    .and_then(|components| {
                        components
                            .into_iter()
                            .find(|component| component.id == id)
                            .map(|component| vec![component.id])
                    })
            })
            .unwrap_or_default();
        return Err(invalid_cook_batch_repo(args, candidates, placement));
    }

    let resolution = homeboy::core::component::resolve_registered_primary_identity(&args.repo)?;
    match resolution {
        homeboy::core::component::RegisteredPrimaryPathResolution::Primary(id) => {
            args.repo = id;
            normalize_registered_cook_batch_repo(args)
        }
        resolution => Err(invalid_cook_batch_repo_path(args, resolution, placement)),
    }
}

fn normalize_registered_cook_batch_repo(args: &mut AgentTaskFanoutCookBatchArgs) -> Result<()> {
    let (repository, component) =
        super::run::cook_repository_names_for_selection(&args.repo, args.component.as_deref())?;
    args.repo = repository;
    args.component = Some(component);
    Ok(())
}

fn invalid_cook_batch_repo_path(
    args: &AgentTaskFanoutCookBatchArgs,
    resolution: homeboy::core::component::RegisteredPrimaryPathResolution,
    placement: Placement,
) -> Error {
    use homeboy::core::component::{RegisteredPathCandidates, RegisteredPrimaryPathResolution};

    let (classification, candidates) = match resolution {
        RegisteredPrimaryPathResolution::MissingPath => {
            ("missing_path", RegisteredPathCandidates::default())
        }
        RegisteredPrimaryPathResolution::NonGitPath => {
            ("non_git_path", RegisteredPathCandidates::default())
        }
        RegisteredPrimaryPathResolution::UnregisteredRepository(candidates) => {
            ("unregistered_repository", candidates)
        }
        RegisteredPrimaryPathResolution::StaleRegistry(candidates) => {
            ("stale_registry", candidates)
        }
        RegisteredPrimaryPathResolution::AmbiguousNestedComponent(candidates) => {
            ("ambiguous_nested_component", candidates)
        }
        RegisteredPrimaryPathResolution::Primary(_) => unreachable!("primary handled by caller"),
    };
    invalid_cook_batch_repo_with_identity(args, classification, candidates, placement, false)
}

fn resolve_cook_batch_default_branch(args: &mut AgentTaskFanoutCookBatchArgs) -> Result<()> {
    let component_path = super::run::cook_component_path_for_repository_name(
        args.component.as_deref().unwrap_or(&args.repo),
    )?
    .ok_or_else(|| invalid_cook_batch_repo(args, Vec::new(), Placement::Auto))?;
    let resolution = resolve_default_branch(DefaultBranchRequest {
        explicit_base: args.base.as_deref(),
        explicit_from: args.from.as_deref(),
        workspace: None,
        component: Some(&component_path),
        destination: None,
        compatibility_fallback: None,
    })?;
    args.base = Some(resolution.base.clone());
    args.from = Some(resolution.from.clone());
    args.base_resolution = Some(serde_json::to_value(resolution).map_err(|error| {
        Error::internal_unexpected(format!(
            "serialize fanout default-branch resolution: {error}"
        ))
    })?);
    Ok(())
}

fn cook_batch_base(args: &AgentTaskFanoutCookBatchArgs) -> &str {
    args.base
        .as_deref()
        .expect("Cook-batch base is resolved before planning")
}

fn cook_batch_from(args: &AgentTaskFanoutCookBatchArgs) -> &str {
    args.from
        .as_deref()
        .expect("Cook-batch source is resolved before planning")
}

fn invalid_cook_batch_repo(
    args: &AgentTaskFanoutCookBatchArgs,
    candidates: Vec<String>,
    placement: Placement,
) -> Error {
    invalid_cook_batch_repo_with_identity(
        args,
        "invalid_repository_identity",
        homeboy::core::component::RegisteredPathCandidates {
            repositories: Vec::new(),
            components: candidates,
        },
        placement,
        true,
    )
}

fn invalid_cook_batch_repo_with_identity(
    args: &AgentTaskFanoutCookBatchArgs,
    classification: &'static str,
    candidates: homeboy::core::component::RegisteredPathCandidates,
    placement: Placement,
    allow_component_correction: bool,
) -> Error {
    let component_candidates = candidates.components;
    let correction_command = (allow_component_correction
        && component_candidates.len() == 1
        && !has_private_gate_declaration(args))
    .then(|| {
        let mut corrected = args.clone();
        corrected.repo = component_candidates[0].clone();
        quote_args(&cook_batch_argv_with_placement(&corrected, placement))
    });
    let secure_reentry = (allow_component_correction
        && component_candidates.len() == 1
        && has_private_gate_declaration(args)).then(|| {
        format!(
            "re-run the original private Cook-batch invocation with --repo {}; Homeboy will queue, bind, and persist the executable private plan before returning its run-plan command",
            component_candidates[0]
        )
    });
    let message = match classification {
        "missing_path" => "--repo path does not exist",
        "non_git_path" => "--repo path is not inside a Git repository",
        "unregistered_repository" => "--repo Git repository is not a registered Homeboy component primary",
        "stale_registry" => "--repo matches a component whose registered primary path is stale",
        "ambiguous_nested_component" => "--repo is a repository root containing registered nested components, not a registered component primary",
        _ if component_candidates.is_empty() => {
            "--repo must be a registered repo slug or an exact registered primary path"
        }
        _ if component_candidates.len() == 1 => {
            "--repo identifies a related checkout, not a registered primary path"
        }
        _ => "--repo matches multiple registered component identities",
    };
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        message,
        serde_json::json!({
            "provided": args.repo,
            "expected_kind": "registered_repo_slug_or_primary_path",
            "identity_classification": classification,
            "repository_candidates": candidates.repositories,
            "component_candidates": component_candidates.clone(),
            "resolved_candidates": component_candidates,
            "identity_separation_tracker": "https://github.com/Extra-Chill/homeboy/issues/12844",
            "correction_command": correction_command,
            "secure_reentry": secure_reentry,
        }),
    )
}

/// Project the complete typed cook-batch invocation to executable argv.
///
/// Typed command handlers cannot recover which default-valued flags the caller
/// spelled, so this intentionally renders every effective setting. Keeping the
/// projection here makes correction and next-action commands preserve the same
/// recipe rather than each maintaining a partial option list.
#[cfg(test)]
fn cook_batch_argv(args: &AgentTaskFanoutCookBatchArgs) -> Vec<String> {
    cook_batch_argv_with_placement(args, Placement::Auto)
}

fn cook_batch_argv_with_placement(
    args: &AgentTaskFanoutCookBatchArgs,
    placement: Placement,
) -> Vec<String> {
    let mut command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "cook-batch".to_string(),
        "--repo".to_string(),
        args.repo.clone(),
        "--branch-prefix".to_string(),
        args.branch_prefix.clone(),
        "--private-gate-reveal".to_string(),
        clap::ValueEnum::to_possible_value(&args.gates.private_gate_reveal)
            .expect("gate reveal policy has a clap value")
            .get_name()
            .to_string(),
        "--gate-execution-policy".to_string(),
        args.gates.gate_execution_policy.clone(),
        "--gate-timeout-seconds".to_string(),
        args.gates.gate_timeout_seconds.to_string(),
        "--gate-heartbeat-interval-seconds".to_string(),
        args.gates.gate_heartbeat_interval_seconds.to_string(),
        "--gate-no-progress-timeout-seconds".to_string(),
        args.gates.gate_no_progress_timeout_seconds.to_string(),
        "--gate-environment-mode".to_string(),
        args.gates.gate_environment_mode.clone(),
        "--isolate-gate-home".to_string(),
        args.gates.isolate_gate_home.to_string(),
        "--isolate-gate-xdg".to_string(),
        args.gates.isolate_gate_xdg.to_string(),
    ];
    if let Some(component) = &args.component {
        command.extend(["--component".to_string(), component.clone()]);
    }
    if let Some(from) = &args.from {
        command.extend(["--from".to_string(), from.clone()]);
    }
    if let Some(base) = &args.base {
        command.extend(["--base".to_string(), base.clone()]);
    }
    command.splice(1..1, fanout_global_placement_args(placement));
    for (flag, values) in [
        ("--verify", &args.gates.verify),
        ("--verify-file", &args.gates.verify_file),
        ("--private-verify", &args.gates.private_verify),
        ("--private-verify-file", &args.gates.private_verify_file),
        ("--gate-toolchain", &args.gates.gate_toolchains),
        ("--secret-env", &args.secret_env),
    ] {
        for value in values {
            command.extend([flag.to_string(), value.clone()]);
        }
    }
    for value in &args.worktrees {
        command.extend(["--worktree".to_string(), value.clone()]);
    }
    for value in &args.gates.gate_toolchain_specs {
        command.extend([
            "--gate-toolchain-spec".to_string(),
            serde_json::to_string(value).expect("gate toolchain spec serializes"),
        ]);
    }
    for (flag, values) in [
        ("--gate-env", &args.gates.gate_environment),
        ("--gate-env-from", &args.gates.gate_environment_preserve),
    ] {
        for (name, value) in values {
            command.extend([flag.to_string(), format!("{name}={value}")]);
        }
    }
    for value in &args.gates.gate_package_artifacts {
        command.extend([
            "--gate-package-artifact".to_string(),
            serde_json::to_string(value).expect("gate package artifact serializes"),
        ]);
    }
    for value in &args.gates.gate_extension_inputs {
        command.extend([
            "--gate-extension-input".to_string(),
            serde_json::to_string(value).expect("gate extension input serializes"),
        ]);
    }
    for (flag, value) in [
        ("--fanout-id", args.fanout_id.as_ref()),
        ("--prompt-template", args.prompt_template.as_ref()),
        ("--backend", args.backend.as_ref()),
        ("--selector", args.selector.as_ref()),
        ("--model", args.model.as_ref()),
        ("--provider-profile", args.provider_profile.as_ref()),
        ("--provider-config", args.provider_config.as_ref()),
        ("--ai-tool", args.ai_tool.as_ref()),
        (
            "--verification-profiles",
            args.verification_profiles.as_ref(),
        ),
    ] {
        if let Some(value) = value {
            command.extend([flag.to_string(), value.clone()]);
        }
    }
    if args.gates.rerun_completed_gates {
        command.push("--rerun-completed-gates".to_string());
    }
    if args.gates.accept_inherited_failures {
        command.push("--accept-inherited-failures".to_string());
    }
    if let Some(value) = args.max_concurrency {
        command.extend(["--max-concurrency".to_string(), value.to_string()]);
    }
    if let Some(value) = args.max_duration {
        command.extend(["--max-duration".to_string(), value.to_string()]);
    }
    if let Some(value) = args.dry_run_planner_timeout_seconds {
        command.extend([
            "--dry-run-planner-timeout-seconds".to_string(),
            value.to_string(),
        ]);
    }
    if args.preview {
        command.push("--preview".to_string());
    }
    if args.run_plan {
        command.push("--run-plan".to_string());
    }
    command.extend(args.issues.clone());
    command
}

fn bind_materialized_worktrees(
    plan: &mut BatchCookFanoutPlan,
    worktrees: &worktree::WorktreeQueueCreateOutput,
) -> Result<()> {
    for cook in &mut plan.cooks {
        // Generated cook-batch children infer their workspace from the
        // materialized target. A supplied plan's cwd/workspace is explicit
        // provider and promotion source identity, so preserve it unchanged.
        if cook.cwd.is_some() || cook.workspace.is_some() {
            continue;
        }
        let row = worktrees
            .rows
            .iter()
            .find(|row| row.handle == cook.to_worktree)
            .ok_or_else(|| {
                Error::internal_unexpected(format!(
                    "worktree materialization omitted declared cook target '{}'",
                    cook.to_worktree
                ))
            })?;
        let path = row.path.as_deref().ok_or_else(|| {
            Error::internal_unexpected(format!(
                "materialized cook worktree '{}' has no filesystem path",
                cook.to_worktree
            ))
        })?;
        let path = std::fs::canonicalize(path)
            .map_err(|error| Error::internal_io(error.to_string(), Some(path.to_string())))?;
        let path = path.display().to_string();
        cook.cwd = None;
        cook.workspace = Some(path);
    }
    Ok(())
}

fn cook_batch_outer_exit_code(blocked: usize, run_result: &Option<Value>) -> i32 {
    if blocked > 0 {
        1
    } else {
        run_result
            .as_ref()
            .and_then(|value| value["exit_code"].as_i64())
            .unwrap_or(0) as i32
    }
}

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
enum BatchWorktreeResolutionState {
    Blocked,
    StillBlocked,
    Reused,
    ReResolved,
    ReusedTerminal,
    Created,
    Planned,
}

fn batch_worktree_resolution(
    worktrees: &worktree::WorktreeQueueCreateOutput,
    states: &BTreeMap<String, BatchWorktreeResolutionState>,
    retry: bool,
) -> Value {
    let rows = worktrees
        .rows
        .iter()
        .map(|row| {
            serde_json::json!({
                "handle": row.handle,
                "state": states
                    .get(&row.handle)
                    .expect("every worktree row has resolution authority"),
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema": "homeboy/agent-task-cook-batch-worktree-resolution/v1",
        "attempt": if retry { "rerun" } else { "initial" },
        "rows": rows,
    })
}

fn bind_materialized_worktree_paths(
    plan: &mut BatchCookFanoutPlan,
    worktrees: &worktree::WorktreeQueueCreateOutput,
) {
    for cook in &mut plan.cooks {
        if cook.cwd.is_some() || cook.workspace.is_some() {
            continue;
        }
        let row = worktrees
            .rows
            .iter()
            .find(|row| row.handle == cook.to_worktree);
        // A planned path describes the future mutation. Keeping its handle in
        // the emitted plan lets normal execution create that exact destination.
        if matches!(
            row.map(|row| &row.status),
            Some(worktree::WorktreeQueueCreateStatus::Created)
        ) {
            cook.workspace = row.and_then(|row| row.path.clone());
        }
    }
}

#[cfg(test)]
fn queue_or_reuse_worktrees(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
) -> Result<worktree::WorktreeQueueCreateOutput> {
    queue_or_reuse_worktrees_with_terminal_paths(args, plan, None, false)
        .map(|(worktrees, _)| worktrees)
}

fn queue_or_reuse_worktrees_with_terminal_paths(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
    previously_terminal: Option<&BTreeMap<String, String>>,
    retry: bool,
) -> Result<(worktree::WorktreeQueueCreateOutput, Value)> {
    let provider_workspace_creation = configured_provider_workspace_creation()?;
    let provision_repo = cook_batch_provision_repository(&args.repo, provider_workspace_creation)?;
    let queue_create = |cooks: Vec<&BatchCookSpec>, dry_run: bool| {
        worktree::queue_create(worktree::WorktreeQueueCreateOptions {
            repo: provision_repo.clone(),
            requests: cooks.into_iter().map(|cook| worktree::WorktreeQueueCreateRequest {
                branch: cook.head.clone().expect("generated cooks have heads"),
                task_url: cook.task_url.clone(),
                task_ref: cook.task_url.clone(),
                run_id: Some(cook.run_id()),
                provider_lifecycle: provider_workspace_creation.then(|| {
                    homeboy::core::worktree_provider::WorktreeProvisionLifecycle {
                        purpose: "agent_task_cook".to_string(),
                        owner_run_ref: cook.run_id(),
                        cleanup_policy: homeboy::core::worktree_provider::WorktreeCleanupPolicy::RemoveOnSuccess,
                    }
                }),
            }).collect(),
            from: cook_batch_from(args).to_string(),
            dry_run,
            retry_after_seconds: 30,
        })
    };

    if args.preview {
        let worktrees = static_worktrees_dry_run(args, plan);
        let states = worktrees
            .rows
            .iter()
            .map(|row| (row.handle.clone(), BatchWorktreeResolutionState::Planned))
            .collect();
        let resolution = batch_worktree_resolution(&worktrees, &states, retry);
        return Ok((worktrees, resolution));
    }

    let mut reused = Vec::new();
    let mut to_create = Vec::new();
    let mut states = BTreeMap::new();
    let provider_config = provider_workspace_creation.then(homeboy::core::defaults::load_config);
    for cook in &plan.cooks {
        let branch = cook.head.as_ref().expect("generated cooks have heads");
        if let Some(path) = previously_terminal.and_then(|terminal| terminal.get(&cook.run_id())) {
            states.insert(
                cook.to_worktree.clone(),
                BatchWorktreeResolutionState::ReusedTerminal,
            );
            reused.push(worktree::WorktreeQueueCreateRow {
                branch: branch.clone(),
                handle: cook.to_worktree.clone(),
                status: worktree::WorktreeQueueCreateStatus::Created,
                command: vec!["terminal".to_string(), cook.to_worktree.clone()],
                retry_after_seconds: None,
                active_lock_holder: None,
                path: Some(path.clone()),
                error: None,
                failure: None,
            });
            continue;
        }
        if cook.adopted_worktree {
            let workspace = homeboy::core::worktree_provider::resolve_configured_worktree_mutation_target_from_config(
                &cook.to_worktree,
                &homeboy::core::defaults::load_config(),
                homeboy::core::worktree_provider::WorktreeMutationContext::default(),
            )?;
            if workspace.branch.as_deref() != Some(branch) {
                return Err(Error::validation_invalid_argument(
                    "worktree",
                    "explicit worktree branch does not match its Cook child",
                    Some(cook.to_worktree.clone()),
                    None,
                ));
            }
            if workspace.task_url.as_deref() != cook.task_url.as_deref() {
                return Err(Error::validation_invalid_argument(
                    "worktree",
                    "explicit worktree tracker does not match its Cook child",
                    Some(cook.to_worktree.clone()),
                    None,
                ));
            }
            if cook
                .protected_branches
                .iter()
                .any(|protected| protected == branch)
            {
                return Err(Error::validation_invalid_argument(
                    "worktree",
                    "explicit worktree branch is protected",
                    Some(cook.to_worktree.clone()),
                    None,
                ));
            }
            let path = workspace.path.clone();
            homeboy::core::worktree_provider::validate_worktree_root(&path, &cook.to_worktree)?;
            let base = homeboy::core::git::run_git(
                &path,
                &[
                    "rev-parse",
                    "--verify",
                    &format!("{}^{{commit}}", cook_batch_from(args)),
                ],
                "resolve explicit worktree base",
            )?;
            let head = homeboy::core::git::run_git(
                &path,
                &["rev-parse", "--verify", "HEAD^{commit}"],
                "resolve explicit worktree HEAD",
            )?;
            if !homeboy::core::git::is_ancestor(
                &path.display().to_string(),
                base.trim(),
                head.trim(),
            )
            .unwrap_or(false)
            {
                return Err(Error::validation_invalid_argument(
                    "worktree",
                    "explicit worktree does not descend from the immutable Cook base",
                    Some(cook.to_worktree.clone()),
                    None,
                ));
            }
            reused.push(worktree::WorktreeQueueCreateRow {
                branch: branch.clone(),
                handle: cook.to_worktree.clone(),
                status: worktree::WorktreeQueueCreateStatus::Created,
                command: vec!["adopted".to_string(), cook.to_worktree.clone()],
                retry_after_seconds: None,
                active_lock_holder: None,
                path: Some(workspace.path.display().to_string()),
                error: None,
                failure: None,
            });
            states.insert(
                cook.to_worktree.clone(),
                BatchWorktreeResolutionState::Reused,
            );
            continue;
        }
        if let Some(config) = &provider_config {
            match homeboy::core::worktree_provider::resolve_configured_worktree_mutation_target_from_config(
                    &cook.to_worktree,
                    config,
                    homeboy::core::worktree_provider::WorktreeMutationContext::default(),
                ) {
                Ok(workspace) => {
                    if workspace.branch.as_deref() != Some(branch) {
                        return Err(Error::validation_invalid_argument(
                            "worktree",
                            "resolved provider worktree branch does not match its Cook child",
                            Some(cook.to_worktree.clone()),
                            None,
                        ));
                    }
                    if workspace.task_url.as_deref().is_some_and(|task_url| {
                        cook.task_url.as_deref().is_none_or(|expected| {
                            homeboy::core::worktree_provider::normalize_worktree_task_url(task_url)
                                != homeboy::core::worktree_provider::normalize_worktree_task_url(expected)
                        })
                    }) {
                        return Err(Error::validation_invalid_argument(
                            "worktree",
                            "resolved provider worktree tracker does not match its Cook child",
                            Some(cook.to_worktree.clone()),
                            None,
                        ));
                    }
                    reused.push(worktree::WorktreeQueueCreateRow {
                        branch: branch.clone(),
                        handle: cook.to_worktree.clone(),
                        status: worktree::WorktreeQueueCreateStatus::Created,
                        command: Vec::new(),
                        retry_after_seconds: None,
                        active_lock_holder: None,
                        path: Some(workspace.path.display().to_string()),
                        error: None,
                        failure: None,
                    });
                    states.insert(
                        cook.to_worktree.clone(),
                        if retry {
                            BatchWorktreeResolutionState::ReResolved
                        } else {
                            BatchWorktreeResolutionState::Reused
                        },
                    );
                    continue;
                }
                Err(error) if error.details["worktree_provider_lookup"] == "not_found" => {}
                Err(error) => return Err(error),
            }
        }
        match (!provider_workspace_creation)
            .then(|| active_registered_worktree_path(&cook.to_worktree))
        {
            Some(Some(path)) => {
                reused.push(worktree::WorktreeQueueCreateRow {
                    branch: branch.clone(),
                    handle: cook.to_worktree.clone(),
                    status: worktree::WorktreeQueueCreateStatus::Created,
                    command: worktree_create_command(args, branch),
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(path),
                    error: None,
                    failure: None,
                });
                states.insert(
                    cook.to_worktree.clone(),
                    BatchWorktreeResolutionState::Reused,
                );
            }
            _ => to_create.push(cook),
        }
    }

    let created = queue_create(to_create, false)?;
    let mut rows = Vec::new();
    for cook in &plan.cooks {
        let branch = cook.head.as_ref().expect("generated cooks have heads");
        if let Some(row) = reused.iter().find(|row| row.handle == cook.to_worktree) {
            rows.push(row.clone());
        } else if let Some(row) = created.rows.iter().find(|row| row.branch == *branch) {
            rows.push(row.clone());
        }
    }

    let worktrees = with_workspace_owner_repair_commands(
        args,
        plan,
        worktree::WorktreeQueueCreateOutput {
            schema: "homeboy/worktree-queue-create/v1",
            repo: provision_repo,
            base_ref: cook_batch_from(args).to_string(),
            dry_run: false,
            rows,
        },
    )?;
    for row in &worktrees.rows {
        states
            .entry(row.handle.clone())
            .or_insert(match row.status {
                worktree::WorktreeQueueCreateStatus::Created => {
                    BatchWorktreeResolutionState::Created
                }
                worktree::WorktreeQueueCreateStatus::WouldCreate => {
                    BatchWorktreeResolutionState::Planned
                }
                worktree::WorktreeQueueCreateStatus::Failed
                | worktree::WorktreeQueueCreateStatus::Queued
                | worktree::WorktreeQueueCreateStatus::ActiveLockHolder => {
                    if retry {
                        BatchWorktreeResolutionState::StillBlocked
                    } else {
                        BatchWorktreeResolutionState::Blocked
                    }
                }
            });
    }
    let resolution = batch_worktree_resolution(&worktrees, &states, retry);
    Ok((worktrees, resolution))
}

fn durable_terminal_worktree_paths(plan: &BatchCookFanoutPlan) -> Result<BTreeMap<String, String>> {
    let mut terminal = BTreeMap::new();
    for cook in &plan.cooks {
        let Ok(record) = agent_task_lifecycle::reconcile_status(&cook.run_id()) else {
            continue;
        };
        if !record.state.is_terminal() {
            continue;
        }
        let recipe = agent_task_service::load_recipe(&cook.run_id())?;
        let path = recipe
            .attempts
            .last()
            .and_then(|attempt| attempt.plan.tasks.first())
            .and_then(|task| task.workspace.root.clone())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "worktree",
                    "terminal Cook recipe does not preserve its immutable workspace",
                    Some(cook.to_worktree.clone()),
                    None,
                )
            })?;
        terminal.insert(cook.run_id(), path);
    }
    Ok(terminal)
}

/// A preview must not contact a worktree provider, inspect existing worktrees,
/// or resolve Git refs. Those operations can hydrate remote state or block on a
/// provider. The command is the replayable execution boundary; paths are left
/// unknown until that boundary materializes the worktree.
fn static_worktrees_dry_run(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
) -> worktree::WorktreeQueueCreateOutput {
    #[cfg(test)]
    STATIC_WORKTREE_PROJECTIONS.with(|count| count.set(count.get() + 1));
    worktree::WorktreeQueueCreateOutput {
        schema: "homeboy/worktree-queue-create/v1",
        repo: args.repo.clone(),
        base_ref: cook_batch_from(args).to_string(),
        dry_run: true,
        rows: plan
            .cooks
            .iter()
            .map(|cook| worktree::WorktreeQueueCreateRow {
                branch: cook.head.clone().expect("generated cooks have heads"),
                handle: cook.to_worktree.clone(),
                status: worktree::WorktreeQueueCreateStatus::WouldCreate,
                command: worktree_create_command(args, cook.head.as_deref().expect("head exists")),
                retry_after_seconds: None,
                active_lock_holder: None,
                path: None,
                error: None,
                failure: None,
            })
            .collect(),
    }
}

fn with_workspace_owner_repair_commands(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
    mut worktrees: worktree::WorktreeQueueCreateOutput,
) -> Result<worktree::WorktreeQueueCreateOutput> {
    if !configured_provider_workspace_creation()? {
        return Ok(worktrees);
    }

    let config = homeboy::core::defaults::load_config();
    let provision_repo = cook_batch_provision_repository(&args.repo, true)?;
    for row in &mut worktrees.rows {
        // An empty command on a created row is explicit evidence that current
        // provider authority resolved the exact destination. Do not replace it
        // with a creation command that was neither needed nor run.
        if row.status == worktree::WorktreeQueueCreateStatus::Created
            && (row.command.is_empty()
                || matches!(
                    row.command.first().map(String::as_str),
                    Some("terminal" | "adopted")
                ))
        {
            continue;
        }
        let Some(cook) = plan
            .cooks
            .iter()
            .find(|cook| cook.to_worktree == row.handle)
        else {
            continue;
        };
        let intent = homeboy::core::worktree_provider::WorktreeProvisionIntent {
            handle: row.handle.clone(),
            repo: provision_repo.clone(),
            base: cook_batch_from(args).to_string(),
            head: row.branch.clone(),
            task_url: Some(
                cook.task_url
                    .clone()
                    .expect("generated cooks have task URLs"),
            ),
        };
        let lifecycle = homeboy::core::worktree_provider::WorktreeProvisionLifecycle {
            purpose: "agent_task_cook".to_string(),
            owner_run_ref: cook.run_id(),
            cleanup_policy:
                homeboy::core::worktree_provider::WorktreeCleanupPolicy::RemoveOnSuccess,
        };
        row.command =
            homeboy::core::worktree_provider::configured_worktree_lifecycle_ensure_argv_from_config(
                &intent, &lifecycle, &config,
            )?;
    }
    Ok(worktrees)
}

fn cook_batch_provision_repository(
    repo: &str,
    provider_workspace_creation: bool,
) -> Result<String> {
    if !provider_workspace_creation {
        return Ok(repo.to_string());
    }
    Ok(homeboy::core::component::registered_by_id(repo)?
        .and_then(|component| component.remote_url)
        .map(|remote| super::run::normalize_repository_name(&remote))
        .filter(|repository| !repository.is_empty())
        .unwrap_or_else(|| repo.to_string()))
}

fn configured_provider_workspace_creation() -> Result<bool> {
    let config = homeboy::core::defaults::load_config();
    Ok(config
        .worktree_providers
        .values()
        .any(|provider| provider.enabled && provider.apply_enabled))
}

fn active_registered_worktree_path(handle: &str) -> Option<String> {
    if let Ok(workspace) =
        homeboy::core::worktree_provider::observe_worktree_provider_workspace(handle)
    {
        return (!workspace.safety.missing).then_some(workspace.ownership.path);
    }
    homeboy::core::worktree_provider::resolve_native_worktree_mutation_target(
        handle,
        homeboy::core::worktree_provider::WorktreeMutationContext::default(),
    )
    .ok()
    .flatten()
    .map(|target| target.path.display().to_string())
}

fn preflight_batch_cook_recipes(
    plan: &BatchCookFanoutPlan,
    attempt_dispatcher: Option<&CookAttemptDispatcherFactory>,
) -> Result<()> {
    // Planning and dry-run callers may only have a managed worktree handle.
    // Validate immutable recipe inputs without resolving that handle as a live
    // workspace; execution validates the materialized workspace separately.
    let mut readiness_cache = provider::ProviderRuntimeReadinessCache::default();
    for cook in &plan.cooks {
        let invocation = cook.to_cook_invocation(plan)?;
        // Preflight must construct the same initial plan that Cook persists.
        // Comparing the uncompiled invocation made existing recipes appear to
        // drift whenever their workspace-derived plan had already been stored.
        let mut options = agent_task_service::compile_cook_attempt_with_readiness_cache(
            invocation.options,
            invocation.dispatch,
            &mut readiness_cache,
        )?;
        options.harvest_context = batch_harvest_context()?;
        if let Some(dispatcher) = attempt_dispatcher {
            options.provider_transport.attempt_dispatcher = Some(dispatcher(&options));
        }
        agent_task_service::validate_initial_recipe_compatibility(&options)?;
    }
    Ok(())
}

fn load_fanout_agent_task_plan(
    args: &AgentTaskFanoutInputArgs,
) -> Result<homeboy::agents::agent_tasks::scheduler::AgentTaskPlan> {
    agent_task_service::read_plan(&args.input)
}

fn load_batch_cook_fanout_plan(
    args: &AgentTaskFanoutInputArgs,
    allow_private_execution_input: bool,
) -> Result<BatchCookFanoutPlan> {
    // Both private-plan reads below answer the same security question — is the
    // supplied path THE controller-owned artifact? — so they have to agree on
    // where that artifact lives. Resolving the data root twice let the
    // pre-read permission validation and the path authorization consult two
    // different answers: a plan whose parent missed the first check could still
    // satisfy the second, and the owner-only permission gate would be skipped.
    let data_root = homeboy::core::paths::homeboy_data()?;
    let raw = if let Some(path) = args.input.strip_prefix('@') {
        let path = PathBuf::from(path);
        if path.parent() == Some(private_batch_plan_dir_in_roots(&data_root).as_path()) {
            validate_private_plan_path_before_read(&path)?;
        }
        read_batch_plan_input(path)?
    } else {
        config::read_json_spec_to_string(&args.input)?
    };
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task fanout batch-cook input".to_string()),
            None,
        )
    })?;
    let value = if value["schema"] == "homeboy/agent-task-private-batch-cook-plan/v1" {
        if !allow_private_execution_input {
            return Err(Error::validation_invalid_argument(
                "input",
                "private batch plans are execution-only; use fanout run-plan with the returned command",
                None,
                None,
            ));
        }
        let fanout_id = value["fanout_id"].as_str().ok_or_else(|| {
            Error::validation_invalid_argument(
                "input",
                "private batch plan is missing its fanout id",
                None,
                None,
            )
        })?;
        let expected_path = private_batch_plan_path_in_roots(&data_root, fanout_id);
        let supplied_path = args
            .input
            .strip_prefix('@')
            .map(PathBuf::from)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "input",
                    "private batch plans must be read from their controller-owned artifact path",
                    None,
                    None,
                )
            })?;
        if supplied_path != expected_path {
            return Err(Error::validation_invalid_argument(
                "input",
                "private batch plan path is not the controller-owned artifact path",
                None,
                None,
            ));
        }
        let metadata = fs::symlink_metadata(&expected_path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(expected_path.display().to_string()))
        })?;
        if !metadata.file_type().is_file() {
            return Err(Error::validation_invalid_argument(
                "input",
                "private batch plan artifact must be a regular controller-owned file",
                None,
                None,
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return Err(Error::validation_invalid_argument(
                    "input",
                    "private batch plan artifact permissions are not owner-only",
                    None,
                    None,
                ));
            }
        }
        let plan = value["plan"].clone();
        let expected = value["sha256"].as_str().ok_or_else(|| {
            Error::validation_invalid_argument(
                "input",
                "private batch plan is missing its digest",
                None,
                None,
            )
        })?;
        let actual = private_plan_digest(&plan)?;
        if expected != actual {
            return Err(Error::validation_invalid_argument(
                "input",
                "private batch plan checksum mismatch; artifact may be corrupted",
                None,
                None,
            ));
        }
        plan
    } else {
        value
    };
    BatchCookFanoutPlan::from_value(value, args)
}

const MAX_BATCH_PLAN_BYTES: u64 = 4 * 1024 * 1024;

/// Read every `@path` batch input through a non-following descriptor. This keeps
/// private-envelope bytes out of generic input diagnostics and lets the private
/// path validate the same file it reads.
fn read_batch_plan_input(path: PathBuf) -> Result<String> {
    let before = fs::symlink_metadata(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if !before.file_type().is_file() || before.len() > MAX_BATCH_PLAN_BYTES {
        return Err(Error::validation_invalid_argument(
            "input",
            "batch plan input must be a bounded regular file",
            Some(path.display().to_string()),
            None,
        ));
    }
    let mut options = fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let file = options
        .open(&path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let opened = file
        .metadata()
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    #[cfg(unix)]
    let same_identity = {
        use std::os::unix::fs::MetadataExt;
        before.dev() == opened.dev() && before.ino() == opened.ino()
    };
    #[cfg(not(unix))]
    // Descriptor validation is portable; device/inode identity pinning is an
    // additional Unix guarantee where the standard library exposes it.
    let same_identity = true;
    if !opened.is_file() || !same_identity || opened.len() > MAX_BATCH_PLAN_BYTES {
        return Err(Error::validation_invalid_argument(
            "input",
            "batch plan input changed or is unsafe",
            Some(path.display().to_string()),
            None,
        ));
    }
    let mut bytes = Vec::with_capacity(opened.len() as usize);
    file.take(MAX_BATCH_PLAN_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if bytes.len() as u64 > MAX_BATCH_PLAN_BYTES {
        return Err(Error::validation_invalid_argument(
            "input",
            "batch plan input exceeds the byte limit",
            Some(path.display().to_string()),
            None,
        ));
    }
    String::from_utf8(bytes).map_err(|_| {
        Error::validation_invalid_argument(
            "input",
            "batch plan input must be UTF-8",
            Some(path.display().to_string()),
            None,
        )
    })
}

fn private_plan_digest(plan: &Value) -> Result<String> {
    use sha2::{Digest, Sha256};
    let bytes = serde_json::to_vec(plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize private batch plan".to_string()),
        )
    })?;
    Ok(format!("sha256:{:x}", Sha256::digest(bytes)))
}

fn batch_plan_reference(plan: &BatchCookFanoutPlan) -> Result<Value> {
    let plan = serde_json::to_value(plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize batch cook plan reference".to_string()),
        )
    })?;
    Ok(serde_json::json!({
        "fanout_id": plan["fanout_id"],
        "sha256": private_plan_digest(&plan)?,
    }))
}

fn private_batch_plan_path(fanout_id: &str) -> Result<PathBuf> {
    Ok(private_batch_plan_path_in_roots(
        &homeboy::core::paths::homeboy_data()?,
        fanout_id,
    ))
}

fn private_batch_plan_path_in_roots(data_root: &Path, fanout_id: &str) -> PathBuf {
    private_batch_plan_dir_in_roots(data_root).join(format!(
        "{}.json",
        homeboy::core::paths::sanitize_path_segment(fanout_id)
    ))
}

fn private_batch_plan_dir_in_roots(data_root: &Path) -> PathBuf {
    data_root.join("agent-task").join("private-batch-plans")
}

fn validate_private_plan_path_before_read(path: &PathBuf) -> Result<()> {
    let metadata = fs::symlink_metadata(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if !metadata.file_type().is_file() {
        return Err(Error::validation_invalid_argument(
            "input",
            "private batch plan artifact must be a regular file",
            Some(path.display().to_string()),
            None,
        ));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(Error::validation_invalid_argument(
                "input",
                "private batch plan artifact permissions are not owner-only",
                Some(path.display().to_string()),
                None,
            ));
        }
    }
    Ok(())
}

/// Retained beside durable Cook recipes until the owning Homeboy data directory
/// is cleaned. The envelope binds the replay input to its exact snapshotted plan.
fn persist_private_batch_plan(plan: &BatchCookFanoutPlan) -> Result<PathBuf> {
    let path = private_batch_plan_path(&plan.fanout_id)?;
    let parent = path.parent().expect("private plan parent");
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(error.to_string(), Some(parent.display().to_string()))
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(parent, fs::Permissions::from_mode(0o700)).map_err(|error| {
            Error::internal_io(error.to_string(), Some(parent.display().to_string()))
        })?;
    }
    let plan_value = serde_json::to_value(plan).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize private batch plan".to_string()),
        )
    })?;
    let envelope = serde_json::json!({
        "schema": "homeboy/agent-task-private-batch-cook-plan/v1",
        "fanout_id": plan.fanout_id,
        "sha256": private_plan_digest(&plan_value)?,
        "plan": plan_value,
    });
    let temporary = path.with_extension(format!("{}.tmp", uuid::Uuid::new_v4()));
    let mut file = create_private_plan_temp(&temporary)?;
    file.write_all(&serde_json::to_vec(&envelope).expect("private envelope serializes"))
        .and_then(|_| file.sync_all())
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(temporary.display().to_string()))
        })?;
    fs::rename(&temporary, &path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    Ok(path)
}

fn create_private_plan_temp(path: &std::path::Path) -> Result<fs::File> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        // Mode is supplied to the atomic O_CREAT call, so no permissive file
        // exists between creation and the first private byte write.
        options.mode(0o600);
    }
    options
        .open(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookFanoutPlan {
    #[serde(default = "batch_cook_fanout_plan_schema")]
    schema: String,
    fanout_id: String,
    cooks: Vec<BatchCookSpec>,
    /// Operator ceiling on how many children run at once, carried on the plan
    /// so a plan built by `cook-batch` keeps its limit when it is persisted and
    /// later executed by `run-plan`. `None` defers to host config.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_concurrency: Option<usize>,
    /// Wall-clock budget for the whole batch, in seconds.
    ///
    /// Stored as a duration rather than an absolute instant so a plan
    /// persisted today and executed tomorrow gets the budget it asked for
    /// instead of one that expired while it sat on disk. It is resolved to an
    /// absolute deadline once, when the batch starts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_duration_seconds: Option<u64>,
    /// Resolved routing authority captured before planning. Child identities are
    /// bound later, but requested/effective/fallback policy must survive replay.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    placement: Option<PlacementDirective>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    metadata: Value,
}

impl BatchCookFanoutPlan {
    fn ensure_placement(&mut self, placement: PlacementDirective) -> Result<()> {
        if let Some(planned) = self.placement.as_ref() {
            let explicit_runner_changed = placement.runner.as_ref().is_some_and(|runner| {
                runner.source == homeboy_lab_runner_contract::RunnerSelectionSource::Explicit
                    && planned.runner.as_ref() != Some(runner)
            });
            if planned.requested != placement.requested || explicit_runner_changed {
                return Err(Error::validation_invalid_argument(
                    "placement",
                    "run-plan placement conflicts with the durable fanout placement policy",
                    Some(format!("requested {:?}", placement.requested)),
                    Some(vec![
                        "Replay the plan with its original global placement and runner arguments."
                            .to_string(),
                    ]),
                ));
            }
            return Ok(());
        }
        self.placement = Some(placement);
        Ok(())
    }

    fn from_value(value: Value, args: &AgentTaskFanoutInputArgs) -> Result<Self> {
        reject_generic_fanout_inputs(&value)?;
        let mut plan: BatchCookFanoutPlan = serde_json::from_value(value).map_err(|error| {
            Error::validation_invalid_argument(
                "input",
                error.to_string(),
                None,
                Some(vec![
                    "Expected homeboy/agent-task-batch-cook-fanout-plan/v1 with a non-empty cooks array.".to_string(),
                ]),
            )
        })?;
        if let Some(fanout_id) = &args.fanout_id {
            plan.rekey(fanout_id.clone());
        }
        if plan.schema != AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA {
            return Err(invalid_fanout(
                "agent-task fanout requires homeboy/agent-task-batch-cook-fanout-plan/v1",
            ));
        }
        if plan.fanout_id.trim().is_empty() {
            return Err(invalid_fanout(
                "fanout_id is required for batch cook fanout",
            ));
        }
        if plan.cooks.is_empty() {
            return Err(invalid_fanout(
                "batch cook fanout requires at least one cook",
            ));
        }
        for cook in &mut plan.cooks {
            cook.apply_defaults(args)?;
            validate_batch_cook_repository_component(cook)?;
        }
        plan.resolve_dependencies()?;
        Ok(plan)
    }

    fn rekey(&mut self, fanout_id: String) {
        let previous_prefix = format!("{}-", self.fanout_id);
        let mut ids = BTreeMap::new();
        for cook in &mut self.cooks {
            let cell_id = cook
                .cook_id
                .strip_prefix(&previous_prefix)
                .unwrap_or(&cook.cook_id);
            let previous = cook.cook_id.clone();
            cook.cook_id = format!("{fanout_id}-{cell_id}");
            ids.insert(previous, cook.cook_id.clone());
        }
        for cook in &mut self.cooks {
            for dependency in &mut cook.depends_on {
                if let Some(rekeyed) = ids.get(dependency) {
                    *dependency = rekeyed.clone();
                }
            }
        }
        self.fanout_id = fanout_id;
    }

    fn apply_ai_tool_override(&mut self, ai_tool: Option<&str>) {
        let Some(ai_tool) = ai_tool else { return };
        for cook in &mut self.cooks {
            cook.ai_tool = ai_tool.to_string();
        }
    }

    /// An execution-time `--max-concurrency` replaces the ceiling the plan was
    /// built with. Absent, the persisted plan value stands.
    fn apply_max_concurrency_override(&mut self, max_concurrency: Option<usize>) {
        if max_concurrency.is_some() {
            self.max_concurrency = max_concurrency;
        }
    }

    /// An execution-time `--max-duration` replaces the budget the plan was
    /// built with. Absent, the persisted plan value stands.
    fn apply_max_duration_override(&mut self, max_duration_seconds: Option<u64>) {
        if max_duration_seconds.is_some() {
            self.max_duration_seconds = max_duration_seconds;
        }
    }

    /// Resolve this batch's wall-clock budget to an absolute deadline, now.
    fn cook_deadline(&self) -> Option<CookDeadline> {
        self.max_duration_seconds
            .map(CookDeadline::from_duration_seconds)
    }

    fn dependency_nodes(&self) -> Vec<AgentTaskDependencyNode> {
        self.cooks
            .iter()
            .map(|cook| AgentTaskDependencyNode {
                id: cook.cook_id.clone(),
                tracker_url: cook.task_url.clone(),
                repository: cook.repo.clone(),
                worktree: cook.workspace.clone().or_else(|| cook.cwd.clone()),
                head: cook.head.clone(),
                depends_on: cook.depends_on.clone(),
            })
            .collect()
    }

    fn resolve_dependencies(&mut self) -> Result<()> {
        let (edges, _) = dependency_graph_readiness(&self.dependency_nodes(), &BTreeMap::new())?;
        for edge in edges {
            let parent = self
                .cooks
                .iter()
                .find(|cook| cook.cook_id == edge.upstream_id)
                .expect("validated graph parent");
            let branch = parent.head.clone().ok_or_else(|| {
                invalid_fanout(&format!(
                    "upstream child '{}' must declare head for dependent '{}'",
                    parent.cook_id, edge.downstream_id
                ))
            })?;
            let child = self
                .cooks
                .iter_mut()
                .find(|cook| cook.cook_id == edge.downstream_id)
                .expect("validated graph child");
            if child.depends_on.len() > 1 {
                return Err(invalid_fanout(&format!(
                    "child '{}' has multiple upstream candidates; declare one stack base per child",
                    child.cook_id
                )));
            }
            child.base = branch;
        }
        Ok(())
    }

    fn ready_plan(&self) -> Result<Self> {
        let (_, readiness) =
            dependency_graph_readiness(&self.dependency_nodes(), &BTreeMap::new())?;
        let ready = readiness.ready.into_iter().collect::<HashSet<_>>();
        Ok(Self {
            cooks: self
                .cooks
                .iter()
                .filter(|cook| ready.contains(&cook.cook_id))
                .cloned()
                .collect(),
            ..self.clone()
        })
    }

    fn dependency_graph_metadata(&self) -> Result<Value> {
        let nodes = self.dependency_nodes();
        let (edges, readiness) = dependency_graph_readiness(&nodes, &BTreeMap::new())?;
        Ok(serde_json::json!({
            "schema": "homeboy/agent-task-fanout-dependency-graph/v1",
            "nodes": nodes,
            "edges": edges,
            "readiness": readiness,
        }))
    }
}

fn validate_batch_cook_repository_component(cook: &mut BatchCookSpec) -> Result<()> {
    let identity_present = !cook.repository_identity.is_null();
    let (repo, component) = match (cook.repo.as_deref(), cook.component_id.as_deref()) {
        (Some(repo), Some(component)) if identity_present => (repo, component),
        (Some(_), None) if !identity_present => return Ok(()),
        (None, None) if !identity_present => return Ok(()),
        _ => {
            return Err(invalid_fanout(&format!(
                "cook `{}` has an incomplete repository/component identity",
                cook.cook_id
            )))
        }
    };
    let (repository, component_id, identity) =
        super::run::cook_repository_identity_for_selection(repo, Some(component))?;
    if repository != repo || component_id != component {
        return Err(invalid_fanout(&format!(
            "cook `{}` component `{component}` does not belong to repository `{repo}`",
            cook.cook_id
        )));
    }
    for field in [
        "repository_name",
        "component_id",
        "component_cwd",
        "remote_identity",
    ] {
        if cook.repository_identity.get(field) != identity.get(field) {
            return Err(invalid_fanout(&format!(
                "cook `{}` repository identity does not match component `{component}`",
                cook.cook_id
            )));
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookSpec {
    cook_id: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    depends_on: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    prompt: Option<String>,
    #[serde(default)]
    tasks: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    workspace_materialization: Vec<BatchCookWorkspaceMaterialization>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    repo: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    component_id: Option<String>,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    repository_identity: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    task_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    backend: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    selector: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    model: Option<String>,
    #[serde(default)]
    secret_env: Vec<String>,
    // Absent means "unspecified", so the dispatch-plan layer can resolve the
    // budget against the configured provider rotation (#11082). An explicit
    // value in a batch plan still wins.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    same_provider_retries: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_rotations: Option<u32>,
    #[serde(default = "one_usize")]
    concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_config: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    provider_evidence_inputs: Vec<super::args::AgentTaskProviderEvidenceInput>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_context: Option<String>,
    to_worktree: String,
    #[serde(default)]
    adopted_worktree: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_command: Option<String>,
    #[serde(default)]
    verify: Vec<String>,
    #[serde(default)]
    private_verify: Vec<String>,
    #[serde(default)]
    input_sources: Vec<homeboy::agents::agent_tasks::gate::AgentTaskGateInputSource>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    verification_profile: Option<String>,
    /// Typed declared verification. Legacy shell gates remain separately
    /// persisted for concrete records that predate this contract.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    test_execution_plan: Option<homeboy_engine_primitives::test_execution::TestExecutionPlan>,
    #[serde(default = "default_private_gate_reveal")]
    private_gate_reveal: AgentTaskGateRevealPolicy,
    #[serde(default)]
    execution_policy: AgentTaskGateExecutionPolicy,
    #[serde(default = "default_gate_timeout_seconds")]
    gate_timeout_seconds: u64,
    #[serde(default = "default_gate_heartbeat_interval_seconds")]
    gate_heartbeat_interval_seconds: u64,
    #[serde(default = "default_gate_no_progress_timeout_seconds")]
    gate_no_progress_timeout_seconds: u64,
    #[serde(default)]
    rerun_completed_gates: bool,
    #[serde(default)]
    gate_environment: AgentTaskGateEnvironmentPolicy,
    #[serde(default)]
    gate_toolchains: Vec<homeboy::agents::agent_tasks::gate::AgentTaskGateToolchainRequirement>,
    #[serde(default)]
    gate_package_artifacts:
        Vec<homeboy::agents::agent_tasks::gate::AgentTaskGatePackageArtifactRequirement>,
    #[serde(default = "default_max_attempts")]
    max_attempts: u32,
    #[serde(default)]
    no_finalize: bool,
    #[serde(default)]
    draft_pr: bool,
    #[serde(default = "default_base")]
    base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    head: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    commit_message: Option<String>,
    #[serde(default)]
    protected_branches: Vec<String>,
    #[serde(default = "default_ai_tool")]
    ai_tool: String,
    #[serde(default = "default_ai_used_for")]
    ai_used_for: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookWorkspaceMaterialization {
    field: String,
    controller_path: String,
    runner_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    branch: Option<String>,
    #[serde(default, rename = "ref", skip_serializing_if = "Option::is_none")]
    ref_name: Option<String>,
    sync_status: String,
}

#[derive(Debug, Clone)]
struct BatchCookInvocation {
    dispatch: AgentTaskDispatchCommand,
    options: CookRequest,
}

impl BatchCookSpec {
    fn apply_defaults(&mut self, args: &AgentTaskFanoutInputArgs) -> Result<()> {
        if self.cook_id.trim().is_empty() {
            return Err(invalid_fanout("each cook requires a non-empty cook_id"));
        }
        if self.to_worktree.trim().is_empty() {
            return Err(invalid_fanout("each cook requires to_worktree"));
        }
        if self.prompt.is_none() && self.tasks.is_empty() {
            return Err(invalid_fanout("each cook requires prompt or tasks"));
        }
        if self.backend.is_none() {
            self.backend = args.backend.clone();
        }
        if self.selector.is_none() {
            self.selector = args.selector.clone();
        }
        if self.model.is_none() {
            self.model = args.model.clone();
        }
        if self.protected_branches.is_empty() {
            self.protected_branches = super::review::default_protected_branches();
        }
        Ok(())
    }

    fn run_id(&self) -> String {
        format!("cook-{}", self.cook_id)
    }

    fn to_cook_invocation(&self, plan: &BatchCookFanoutPlan) -> Result<BatchCookInvocation> {
        if self.verify.is_empty()
            && self.private_verify.is_empty()
            && self.test_execution_plan.is_none()
        {
            return Err(invalid_fanout(
                "each fanout cook requires verify or private_verify so PR finalization has deterministic gates",
            ));
        }
        let mut prompt = self.prompt.clone();
        let workspace_root = self.workspace.as_deref().or(self.cwd.as_deref());
        let mut provider_config = self.provider_config.clone();
        if workspace_root.is_some() {
            let admitted_evidence =
                super::run::admit_provider_evidence_inputs(&self.provider_evidence_inputs)?;
            let evidence = super::run::project_admitted_provider_evidence_inputs(
                &self.provider_evidence_inputs,
                &admitted_evidence,
            )?;
            let projected_paths = super::run::projected_provider_evidence_paths(&evidence);
            super::run::rewrite_provider_evidence_prompt(
                &mut prompt,
                &self.provider_evidence_inputs,
                &admitted_evidence,
                &evidence,
                &projected_paths,
            )?;
            if !evidence.is_empty() {
                let raw = provider_config.as_deref().unwrap_or("{}");
                let mut config: Value = homeboy::core::config::read_json_spec_to_string(raw)
                    .ok()
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or_else(|| serde_json::json!({}));
                if !config.is_object() {
                    config = serde_json::json!({});
                }
                config["evidence_inputs"] = serde_json::Value::Array(evidence);
                provider_config = Some(config.to_string());
            }
        }
        let dispatch = AgentTaskDispatchCommand {
            prompt,
            prompt_is_literal: false,
            tasks: self.tasks.clone(),
            cwd: self.cwd.clone(),
            workspace: self
                .workspace
                .clone()
                .or_else(|| self.cwd.is_none().then(|| self.to_worktree.clone())),
            repo: self.repo.clone(),
            component: self.component_id.clone(),
            task_url: self.task_url.clone(),
            backend: self.backend.clone(),
            selector: self.selector.clone(),
            model: self.model.clone(),
            required_capabilities: Vec::new(),
            secret_env: self.secret_env.clone(),
            concurrency: self.concurrency,
            run_id: Some(agent_task_lifecycle::cook_attempt_run_id(&self.run_id(), 1)),
            task_id: None,
            core: DispatchCoreInputs {
                tasks_json: None,
                provider_config,
                client_context: Some(merged_client_context(plan, self)),
                attempts: self.attempts,
                same_provider_retries: self.same_provider_retries,
                provider_rotations: self.provider_rotations,
                queue_only: false,
                timeout_ms: None,
                resolved_provider_policy: None,
                deny_command: Vec::new(),
                allow_command: Vec::new(),
                command_policy_reason: None,
            },
        };
        let title = self
            .title
            .clone()
            .unwrap_or_else(|| default_cook_title(self));
        let commit_message = self
            .commit_message
            .clone()
            .unwrap_or_else(|| default_cook_commit_message(self));
        let source_worktree_path =
            agent_task_service::source_worktree_path(self.cwd.clone(), self.workspace.clone());
        let task_base_sha = source_worktree_path
            .as_deref()
            .and_then(|path| {
                std::process::Command::new("git")
                    .args(["rev-parse", "HEAD"])
                    .current_dir(path)
                    .output()
                    .ok()
            })
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
        Ok(BatchCookInvocation {
            dispatch,
            options: CookRequest {
                identity: CookIdentity {
                    cook_id: self.run_id(),
                    initial_run_id: self.run_id(),
                    initial_plan: AgentTaskPlan::new(self.run_id(), Vec::new()),
                },
                workspace: CookWorkspace {
                    to_worktree: self.to_worktree.clone(),
                    source_worktree_path,
                    task_base_sha,
                    source_refs: self
                        .task_url
                        .clone()
                        .into_iter()
                        .chain(std::iter::once(cook_recipe_source_identity(plan, self)?))
                        .collect(),
                },
                provider_transport: CookProviderTransport {
                    provider_command: self.provider_command.clone(),
                    provider_invocation: None,
                    attempt_dispatcher: None,
                },
                gates: VerifyGateOptions {
                    verify: self.verify.clone(),
                    private_verify: self.private_verify.clone(),
                    test_execution_plan: self.test_execution_plan.clone(),
                    input_sources: self.input_sources.clone(),
                    private_gate_reveal: self.private_gate_reveal,
                    execution_policy: self.execution_policy,
                    gate_timeout_seconds: self.gate_timeout_seconds,
                    gate_heartbeat_interval_seconds: self.gate_heartbeat_interval_seconds,
                    gate_no_progress_timeout_seconds: self.gate_no_progress_timeout_seconds,
                    rerun_completed_gates: self.rerun_completed_gates,
                    accept_inherited_failures: false,
                    gate_environment: self.gate_environment.clone(),
                    gate_toolchains: self.gate_toolchains.clone(),
                    gate_package_artifacts: self.gate_package_artifacts.clone(),
                    gate_diagnostic_sidecars: Vec::new(),
                    hydrate_dependencies: true,
                },
                retry_policy: CookRetryPolicy {
                    max_attempts: self.max_attempts,
                },
                finalization: CookFinalization {
                    no_finalize: self.no_finalize,
                    draft_pr: self.draft_pr,
                    base: self.base.clone(),
                    head: self.head.clone(),
                    title,
                    commit_message,
                    protected_branches: self.protected_branches.clone(),
                },
                ai_disclosure: CookAiDisclosure {
                    ai_tool: resolve_ai_tool_disclosure(
                        &self.ai_tool,
                        self.backend.as_deref(),
                        self.selector.as_deref(),
                        self.model.as_deref(),
                    ),
                    // Explicit/config/rotation model selection only. Disclosure text
                    // is presentation, not provenance, so it is never reverse-parsed
                    // into a model — omitted stays omitted (#9789).
                    ai_model: self.model.clone(),
                    ai_used_for: self.ai_used_for.clone(),
                },
                harvest_context:
                    crate::agents::agent_task_scheduler::HarvestExecutionContext::default(),
            },
        })
    }
}

fn cook_recipe_source_identity(plan: &BatchCookFanoutPlan, cook: &BatchCookSpec) -> Result<String> {
    let encoded = serde_json::to_vec(&serde_json::json!({
        "schema": "homeboy/agent-task-fanout-cook-source/v1",
        "fanout_id": plan.fanout_id,
        "cook": cook,
    }))
    .map_err(|error| Error::internal_json(error.to_string(), None))?;
    Ok(format!(
        "homeboy://agent-task/fanout-source/{}",
        content_hash::sha256_hex(&encoded)
    ))
}

fn default_cook_title(cook: &BatchCookSpec) -> String {
    let target = cook
        .repo
        .as_deref()
        .or(cook.task_url.as_deref())
        .unwrap_or("agent task");
    format!("Cook {target}")
}

fn default_cook_commit_message(cook: &BatchCookSpec) -> String {
    let target = cook.repo.as_deref().unwrap_or("agent task");
    format!("fix: cook {target}")
}

fn merged_client_context(plan: &BatchCookFanoutPlan, cook: &BatchCookSpec) -> String {
    let mut context = serde_json::from_str::<Value>(cook.client_context.as_deref().unwrap_or("{}"))
        .unwrap_or(Value::Null);
    if !context.is_object() {
        context = serde_json::json!({ "base": context });
    }
    if let Some(object) = context.as_object_mut() {
        object.insert(
            "fanout".to_string(),
            serde_json::json!({
                "id": plan.fanout_id,
                "semantics": "batch_cook",
                "cook_id": cook.cook_id,
                "to_worktree": cook.to_worktree,
                "head": cook.head,
                "workspace_materialization": cook.workspace_materialization,
            }),
        );
    }
    context.to_string()
}

fn cook_command(plan: &BatchCookFanoutPlan, _cook: &BatchCookSpec) -> Vec<String> {
    vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "run-plan".to_string(),
        "--input".to_string(),
        "<batch-cook-plan.json>".to_string(),
        "--record-run-id".to_string(),
        plan.fanout_id.clone(),
    ]
}

/// Return the canonical durable identity for a cook-batch invocation.
///
/// Routing observers must use this rather than deriving an approximation from
/// issue arguments: `build_cook_batch_plan` includes all effective cook inputs
/// in generated identities.
pub(crate) fn cook_batch_fanout_id(args: &AgentTaskFanoutCookBatchArgs) -> Result<String> {
    Ok(build_cook_batch_plan(args)?.fanout_id)
}

fn build_cook_batch_plan(args: &AgentTaskFanoutCookBatchArgs) -> Result<BatchCookFanoutPlan> {
    super::run::validate_provider_evidence_inputs(
        &args.provider_evidence_inputs,
        args.prompt_template.as_deref(),
    )?;
    let profiles = load_verification_profiles(args.verification_profiles.as_deref())?;
    build_cook_batch_plan_with_profiles(args, profiles)
}

/// Parse only inline declarations for static dry-run. Unlike the normal loader,
/// this deliberately cannot open stdin or an `@file` reference.
fn build_static_cook_batch_plan(
    args: &AgentTaskFanoutCookBatchArgs,
) -> Result<BatchCookFanoutPlan> {
    // This is the same prompt/evidence authority live Cook applies before it
    // constructs child plans. Static planning must reject an input that replay
    // would reject before worktree mutation.
    super::run::validate_provider_evidence_inputs(
        &args.provider_evidence_inputs,
        args.prompt_template.as_deref(),
    )?;
    let profiles = match args.verification_profiles.as_deref() {
        Some(spec) => parse_verification_profiles(spec)?,
        None => VerificationProfiles {
            profiles: BTreeMap::new(),
            assignments: Vec::new(),
        },
    };
    build_cook_batch_plan_with_profiles(args, profiles)
}

fn build_cook_batch_plan_with_profiles(
    args: &AgentTaskFanoutCookBatchArgs,
    profiles: VerificationProfiles,
) -> Result<BatchCookFanoutPlan> {
    let (repository, component_id, repository_identity) =
        super::run::cook_repository_identity_for_selection(&args.repo, args.component.as_deref())?;
    // Provider-owned workspaces name handles by canonical repository, so
    // canonicalize the identity-resolved repository rather than the raw `--repo`
    // selector. Component selection has already been validated above.
    let worktree_repo =
        cook_batch_provision_repository(&repository, configured_provider_workspace_creation()?)?;
    let bindings = parse_explicit_worktree_bindings(&args.worktrees)?;
    if !bindings.is_empty()
        && bindings
            .keys()
            .any(|issue| !args.issues.iter().any(|declared| declared == issue))
    {
        return Err(invalid_fanout(
            "--worktree binding names an issue outside this Cook-batch",
        ));
    }
    let mut seen = HashSet::new();
    let mut cooks = Vec::with_capacity(args.issues.len());
    for issue_url in &args.issues {
        let issue = IssueRef::parse(issue_url)?;
        if !seen.insert(issue.key.clone()) {
            return Err(invalid_fanout(
                "duplicate issue URLs are not allowed in one cook-batch",
            ));
        }
        let branch = format!(
            "{}/issue-{}-{}",
            trim_slashes(&args.branch_prefix),
            issue.number,
            slugify(&issue.repo)
        );
        let worktree = format!("{}@{}", worktree_repo, slugify(&branch));
        let explicit_worktree = bindings.get(issue_url).cloned();
        let prompt = render_prompt(
            args.prompt_template.as_deref(),
            &issue,
            &repository,
            &branch,
            &worktree,
        );
        let task_selector = format!("issue-{}", issue.number);
        let (verify, private_verify, verification_profile, test_execution_plan) = profiles
            .resolve(
                issue_url,
                &issue.key,
                &task_selector,
                &args.gates.verify,
                &args.gates.private_verify,
            )?;
        let input_sources =
            sources_for_executed_gates(&args.gates.input_sources, &verify, &private_verify);
        cooks.push(BatchCookSpec {
            cook_id: format!("issue-{}", issue.number),
            depends_on: Vec::new(),
            prompt: Some(prompt),
            tasks: Vec::new(),
            cwd: None,
            workspace: None,
            workspace_materialization: Vec::new(),
            repo: Some(repository.clone()),
            component_id: Some(component_id.clone()),
            repository_identity: repository_identity.clone(),
            task_url: Some(issue_url.clone()),
            backend: args.backend.clone(),
            selector: args.selector.clone(),
            model: args.model.clone(),
            secret_env: args.secret_env.clone(),
            attempts: None,
            same_provider_retries: None,
            provider_rotations: None,
            concurrency: 1,
            provider_config: args.provider_config.clone(),
            provider_evidence_inputs: args.provider_evidence_inputs.clone(),
            client_context: Some(
                serde_json::json!({
                    "issue_url": issue_url,
                    "issue_ref": issue.key,
                    "operator_workflow": "agent-task fanout cook-batch"
                })
                .to_string(),
            ),
            to_worktree: explicit_worktree.clone().unwrap_or(worktree),
            adopted_worktree: explicit_worktree.is_some(),
            provider_command: None,
            verify,
            private_verify,
            input_sources,
            verification_profile,
            test_execution_plan,
            private_gate_reveal: args.gates.private_gate_reveal,
            execution_policy: VerifyGateOptions::from(args.gates.clone()).execution_policy,
            gate_timeout_seconds: args.gates.gate_timeout_seconds,
            gate_heartbeat_interval_seconds: args.gates.gate_heartbeat_interval_seconds,
            gate_no_progress_timeout_seconds: args.gates.gate_no_progress_timeout_seconds,
            rerun_completed_gates: args.gates.rerun_completed_gates,
            gate_environment: VerifyGateOptions::from(args.gates.clone()).gate_environment,
            gate_toolchains: VerifyGateOptions::from(args.gates.clone()).gate_toolchains,
            gate_package_artifacts: VerifyGateOptions::from(args.gates.clone())
                .gate_package_artifacts,
            max_attempts: default_max_attempts(),
            no_finalize: false,
            draft_pr: false,
            base: cook_batch_base(args).to_string(),
            head: Some(branch),
            title: Some(format!("Fix {}", issue.key)),
            commit_message: Some(format!("fix: address {}", issue.key)),
            protected_branches: super::review::default_protected_branches(),
            ai_tool: args.ai_tool.clone().unwrap_or_else(default_ai_tool),
            ai_used_for: default_ai_used_for(),
        });
    }
    if bindings.len() != args.issues.len() && !bindings.is_empty() {
        return Err(invalid_fanout(
            "--worktree bindings must cover every Cook-batch issue exactly once",
        ));
    }
    profiles.validate_assignments(&cooks)?;
    let first = cooks
        .first()
        .map(|cook| cook.cook_id.clone())
        .unwrap_or_else(|| "empty".to_string());
    let fanout_id = args.fanout_id.clone().unwrap_or_else(|| {
        let encoded = serde_json::to_vec(&cooks).expect("Cook specs serialize");
        let digest = content_hash::sha256_hex(&encoded);
        format!(
            "cook-batch-{}-{}-{}-{}",
            repository,
            first,
            cooks.len(),
            &digest[..12]
        )
    });
    // Durable cook recipes are keyed by cook_id. Scope each generated child to
    // its fanout generation so the same issue can run in a later batch.
    for cook in &mut cooks {
        cook.cook_id = format!("{fanout_id}-{}", cook.cook_id);
    }
    Ok(BatchCookFanoutPlan {
        schema: batch_cook_fanout_plan_schema(),
        fanout_id,
        cooks,
        max_concurrency: args.max_concurrency.map(|value| value as usize),
        max_duration_seconds: args.max_duration,
        placement: None,
        metadata: serde_json::json!({
            "source": "agent-task fanout cook-batch",
            "issue_count": args.issues.len(),
            "repo": repository,
            "component": component_id,
            "repository_identity": repository_identity,
            "base": args.base,
            "from": args.from,
            "default_branch_resolution": args.base_resolution,
        }),
    })
}

fn parse_explicit_worktree_bindings(values: &[String]) -> Result<BTreeMap<String, String>> {
    let mut bindings = BTreeMap::new();
    for value in values {
        let (issue, handle) = value.split_once('=').ok_or_else(|| {
            Error::validation_invalid_argument(
                "worktree",
                "--worktree must use ISSUE_URL=HANDLE",
                Some(value.clone()),
                None,
            )
        })?;
        IssueRef::parse(issue)?;
        if handle.trim().is_empty()
            || bindings
                .insert(issue.to_string(), handle.to_string())
                .is_some()
        {
            return Err(Error::validation_invalid_argument(
                "worktree",
                "each --worktree issue binding must be unique and name a handle",
                Some(value.clone()),
                None,
            ));
        }
    }
    Ok(bindings)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationProfiles {
    #[serde(default)]
    profiles: BTreeMap<String, VerificationProfile>,
    #[serde(default)]
    assignments: Vec<VerificationProfileAssignment>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationProfile {
    plan: homeboy_engine_primitives::test_execution::TestExecutionPlan,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct VerificationProfileAssignment {
    selector: String,
    profile: String,
}

impl VerificationProfiles {
    fn validate_assignments(&self, cooks: &[BatchCookSpec]) -> Result<()> {
        for assignment in &self.assignments {
            let matched = cooks.iter().any(|cook| {
                cook.task_url.as_deref() == Some(assignment.selector.as_str())
                    || cook
                        .task_url
                        .as_deref()
                        .and_then(|url| IssueRef::parse(url).ok())
                        .is_some_and(|issue| issue.key == assignment.selector)
                    || cook.cook_id == assignment.selector
            });
            if !matched {
                return Err(Error::validation_invalid_argument(
                    "verification-profiles.assignments.selector",
                    "selector_unmatched: verification profile selector did not match any batch child",
                    Some(assignment.selector.clone()),
                    Some(
                        cooks
                            .iter()
                            .flat_map(verification_profile_selectors)
                            .collect(),
                    ),
                ));
            }
        }
        Ok(())
    }

    fn resolve(
        &self,
        issue_url: &str,
        issue_key: &str,
        task_selector: &str,
        shared_verify: &[String],
        shared_private_verify: &[String],
    ) -> Result<(
        Vec<String>,
        Vec<String>,
        Option<String>,
        Option<homeboy_engine_primitives::test_execution::TestExecutionPlan>,
    )> {
        let matches = self
            .assignments
            .iter()
            .filter(|assignment| {
                assignment.selector == issue_url
                    || assignment.selector == issue_key
                    || assignment.selector == task_selector
            })
            .collect::<Vec<_>>();
        if matches.len() > 1 {
            return Err(Error::validation_invalid_argument(
                "verification-profiles.assignments",
                "selector_ambiguous: more than one verification profile matches a child",
                Some(task_selector.to_string()),
                Some(
                    matches
                        .into_iter()
                        .map(|entry| entry.profile.clone())
                        .collect(),
                ),
            ));
        }
        let Some(assignment) = matches.into_iter().next() else {
            return Ok((
                shared_verify.to_vec(),
                shared_private_verify.to_vec(),
                None,
                None,
            ));
        };
        let profile = self.profiles.get(&assignment.profile).ok_or_else(|| {
            Error::validation_invalid_argument(
                "verification-profiles.assignments",
                "profile_unknown: assignment references an undeclared verification profile",
                Some(assignment.profile.clone()),
                Some(self.profiles.keys().cloned().collect()),
            )
        })?;
        profile.plan.declared_command().map_err(|message| {
            Error::invalid_argument("verification-profiles.profiles.plan", message)
        })?;
        Ok((
            shared_verify.to_vec(),
            shared_private_verify.to_vec(),
            Some(assignment.profile.clone()),
            Some(profile.plan.clone()),
        ))
    }
}

fn sources_for_executed_gates(
    sources: &[homeboy::agents::agent_tasks::gate::AgentTaskGateInputSource],
    verify: &[String],
    private_verify: &[String],
) -> Vec<homeboy::agents::agent_tasks::gate::AgentTaskGateInputSource> {
    use homeboy::agents::agent_tasks::gate::AgentTaskGateVisibility;
    use sha2::{Digest, Sha256};

    let mut required = BTreeMap::<(String, String), usize>::new();
    for (visibility, commands) in [
        (AgentTaskGateVisibility::Visible, verify),
        (AgentTaskGateVisibility::Private, private_verify),
    ] {
        for command in commands {
            let digest = format!("sha256:{:x}", Sha256::digest(command.as_bytes()));
            *required
                .entry((format!("{visibility:?}"), digest))
                .or_default() += 1;
        }
    }
    sources
        .iter()
        .filter(|source| {
            let key = (format!("{:?}", source.visibility), source.sha256.clone());
            let Some(remaining) = required.get_mut(&key) else {
                return false;
            };
            if *remaining == 0 {
                return false;
            }
            *remaining -= 1;
            true
        })
        .cloned()
        .collect()
}

fn load_verification_profiles(spec: Option<&str>) -> Result<VerificationProfiles> {
    let Some(spec) = spec else {
        return Ok(VerificationProfiles {
            profiles: BTreeMap::new(),
            assignments: Vec::new(),
        });
    };
    let raw = config::read_json_spec_to_string(spec)?;
    parse_verification_profiles(&raw)
}

fn parse_verification_profiles(raw: &str) -> Result<VerificationProfiles> {
    let value: Value = serde_json::from_str(raw).map_err(|error| {
        Error::validation_invalid_argument(
            "verification-profiles",
            format!("schema_invalid at $: invalid JSON verification profile declaration: {error}"),
            None,
            Some(vec![format!(
                "Use this shape: {VERIFICATION_PROFILES_EXAMPLE}"
            )]),
        )
    })?;
    let profiles: VerificationProfiles =
        serde_path_to_error::deserialize(value).map_err(|error| {
            let message = error.inner().to_string();
            let mut path = error.path().to_string();
            if path == "." {
                path.clear();
            }
            if let Some(field) = message
                .strip_prefix("unknown field `")
                .and_then(|rest| rest.split_once('`').map(|(field, _)| field))
            {
                if !path.is_empty() {
                    path.push('.');
                }
                path.push_str(field);
            }
            let path = if path.is_empty() {
                "$".to_string()
            } else {
                format!("$.{path}")
            };
            Error::validation_invalid_argument(
                "verification-profiles",
                format!("schema_invalid at {path}: {message}"),
                None,
                Some(vec![format!(
                    "Use this shape: {VERIFICATION_PROFILES_EXAMPLE}"
                )]),
            )
        })?;
    for assignment in &profiles.assignments {
        if assignment.selector.trim().is_empty() {
            return Err(Error::invalid_argument(
                "verification-profiles.assignments.selector",
                "selector_empty: each profile assignment requires a selector",
            ));
        }
    }
    Ok(profiles)
}

fn validate_batch_cook_gates(
    plan: &BatchCookFanoutPlan,
    workspace: Option<std::path::PathBuf>,
) -> Result<()> {
    for cook in &plan.cooks {
        if cook.verify.is_empty()
            && cook.private_verify.is_empty()
            && cook.test_execution_plan.is_none()
        {
            return Err(Error::validation_invalid_argument(
                "verification-profiles",
                "gate_missing: every cook-batch child requires verify or private_verify before worktree creation",
                Some(cook.cook_id.clone()),
                Some(verification_profile_selectors(cook)),
            ));
        }
    }
    validate_batch_gate_contracts(plan, workspace.as_deref())?;
    Ok(())
}

fn batch_gate_workspace(args: &AgentTaskFanoutCookBatchArgs) -> Result<Option<std::path::PathBuf>> {
    let selector = args.component.as_deref().unwrap_or(&args.repo);
    let Some(path) = super::run::cook_component_path_for_repository_name(selector)? else {
        return Ok(None);
    };
    Ok(path.is_dir().then_some(path))
}

fn batch_plan_gate_workspace(plan: &BatchCookFanoutPlan) -> Result<Option<std::path::PathBuf>> {
    let repositories = plan
        .cooks
        .iter()
        .filter_map(|cook| cook.repo.as_deref())
        .collect::<BTreeSet<_>>();
    if repositories.len() != 1 {
        return Ok(None);
    }
    let repository = repositories.into_iter().next().expect("one repository");
    let component_id = plan
        .cooks
        .iter()
        .filter_map(|cook| cook.component_id.as_deref())
        .collect::<BTreeSet<_>>();
    let component_id = match component_id.len() {
        0 => repository,
        1 => component_id.into_iter().next().expect("one component"),
        _ => return Ok(None),
    };
    let Some(path) = super::run::cook_component_path_for_repository_name(component_id)? else {
        return Ok(None);
    };
    Ok(path.is_dir().then_some(path))
}

fn effective_batch_cook_gates(plan: &BatchCookFanoutPlan) -> Vec<Value> {
    plan.cooks
        .iter()
        .map(|cook| {
            serde_json::json!({
                "cook_id": cook.cook_id,
                "task_url": cook.task_url,
                "selectors": verification_profile_selectors(cook),
                "profile": cook.verification_profile,
                "test_execution_plan": cook.test_execution_plan,
                "verify": cook.verify,
                "private_verify": cook.private_verify.iter().map(|_| "[private]").collect::<Vec<_>>(),
                "input_sources": cook.input_sources,
            })
        })
        .collect()
}

fn verification_profile_selectors(cook: &BatchCookSpec) -> Vec<String> {
    let Some(issue_url) = cook.task_url.as_deref() else {
        return vec![cook.cook_id.clone()];
    };
    let Ok(issue) = IssueRef::parse(issue_url) else {
        return vec![issue_url.to_string(), cook.cook_id.clone()];
    };
    vec![
        issue_url.to_string(),
        issue.key,
        format!("issue-{}", issue.number),
    ]
}

/// Batch plans are durable controller state and retain private commands for the
/// existing private-inline gate replay contract. CLI output is a public
/// projection and must never disclose those command bytes.
fn public_batch_cook_plan(plan: &BatchCookFanoutPlan) -> BatchCookFanoutPlan {
    let mut public = plan.clone();
    for cook in &mut public.cooks {
        cook.private_verify = vec!["[private]".to_string(); cook.private_verify.len()];
    }
    public
}

fn apply_provider_profile(args: &mut AgentTaskFanoutCookBatchArgs) {
    let Some(profile) = selected_provider_profile(args.provider_profile.as_deref()) else {
        return;
    };
    if args.backend.is_none() {
        args.backend = profile.backend;
    }
    if args.selector.is_none() {
        args.selector = profile.selector;
    }
    if args.model.is_none() {
        args.model = profile.model;
    }
    if args.provider_config.is_none() {
        args.provider_config = profile.provider_config.map(|value| value.to_string());
    }
}

/// Admit the provider route using the same catalog-backed contract for static
/// planning and live execution. Runtime probes remain a separate live check.
fn resolve_and_validate_effective_backend(args: &mut AgentTaskFanoutCookBatchArgs) -> Result<()> {
    let catalog = AgentTaskProviderCatalog::discover();
    resolve_and_validate_effective_backend_with_catalog(args, &catalog)
}

fn resolve_and_validate_effective_backend_with_catalog(
    args: &mut AgentTaskFanoutCookBatchArgs,
    catalog: &AgentTaskProviderCatalog,
) -> Result<()> {
    resolve_and_validate_effective_backend_with_catalog_and_default(
        args,
        catalog,
        provider::default_backend_for_component,
    )
}

fn resolve_and_validate_effective_backend_with_catalog_and_default(
    args: &mut AgentTaskFanoutCookBatchArgs,
    catalog: &AgentTaskProviderCatalog,
    default_backend: impl FnOnce(Option<&str>) -> Result<Option<String>>,
) -> Result<()> {
    let command = fanout_provider_dispatch_command(
        Some(args.repo.clone()),
        args.component.clone(),
        args.backend.clone(),
        args.selector.clone(),
        args.model.clone(),
        args.secret_env.clone(),
        args.provider_config.clone(),
    );
    let request = dispatch_service::resolve_dispatch_request_with_default_and_catalog(
        command,
        default_backend,
        catalog,
    )
    .map_err(with_provider_admission_remediation)?;
    validate_provider_route(&request, catalog).map_err(with_provider_admission_remediation)?;

    args.backend = Some(request.backend);
    args.selector = request.selector;
    args.model = request.model;
    Ok(())
}

fn with_provider_admission_remediation(error: Error) -> Error {
    error.with_action(ExecutableAction::new(
        "inspect-agent-task-provider-readiness",
        "Inspect dispatchable agent-task providers",
        "homeboy",
        ["agent-task", "providers", "--validate-readiness"],
        ActionSafety::ReadOnly,
    ))
}

fn fanout_provider_dispatch_command(
    repo: Option<String>,
    component: Option<String>,
    backend: Option<String>,
    selector: Option<String>,
    model: Option<String>,
    secret_env: Vec<String>,
    provider_config: Option<String>,
) -> AgentTaskDispatchCommand {
    AgentTaskDispatchCommand {
        prompt: Some("Validate fanout provider admission declarations.".to_string()),
        prompt_is_literal: true,
        repo,
        component,
        backend,
        selector,
        model,
        secret_env,
        core: DispatchCoreInputs {
            provider_config,
            ..DispatchCoreInputs::default()
        },
        ..AgentTaskDispatchCommand::default()
    }
}

fn validate_provider_route(
    request: &dispatch_service::AgentTaskDispatchRequest,
    catalog: &AgentTaskProviderCatalog,
) -> Result<()> {
    match provider::resolve_provider_for_backend(
        catalog.providers(),
        &request.backend,
        request.selector.as_deref(),
    ) {
        provider::ProviderResolution::Resolved(_) => {}
        provider::ProviderResolution::SelectorMismatch { available_ids, .. } => {
            return Err(Error::validation_invalid_argument(
                "selector",
                format!(
                    "--selector does not select a provider for backend `{}`",
                    request.backend
                ),
                request.selector.clone(),
                Some(
                    available_ids
                        .iter()
                        .map(|id| {
                            format!("Pass --selector {id} with --backend {}.", request.backend)
                        })
                        .collect(),
                ),
            ));
        }
        provider::ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => {
            return Err(Error::validation_invalid_argument(
                "selector",
                format!(
                    "backend alias `{}` matches multiple providers",
                    request.backend
                ),
                None,
                Some(
                    candidate_ids
                        .iter()
                        .map(|id| format!("Pass --selector {id}."))
                        .collect(),
                ),
            ));
        }
        provider::ProviderResolution::NotFound => {
            let available = catalog.backends();
            return Err(Error::validation_invalid_argument(
                "backend",
                format!(
                    "agent-task fanout backend `{}` has no installed provider",
                    request.backend
                ),
                Some(request.backend.clone()),
                Some(if available.is_empty() {
                    vec![
                        "Run `homeboy agent-task providers` to diagnose provider discovery."
                            .to_string(),
                    ]
                } else {
                    available
                        .iter()
                        .map(|candidate| format!("Pass --backend {candidate}."))
                        .collect()
                }),
            ));
        }
    }
    dispatch_service::preflight_dispatch_provider_admission(request, catalog)
}

fn admit_batch_provider_routes(plan: &mut BatchCookFanoutPlan) -> Result<()> {
    let catalog = AgentTaskProviderCatalog::discover();
    admit_batch_provider_routes_with_catalog(plan, &catalog)
}

fn admit_batch_provider_routes_with_catalog(
    plan: &mut BatchCookFanoutPlan,
    catalog: &AgentTaskProviderCatalog,
) -> Result<()> {
    for cook in &mut plan.cooks {
        let command = fanout_provider_dispatch_command(
            cook.repo.clone(),
            cook.component_id.clone(),
            cook.backend.clone(),
            cook.selector.clone(),
            cook.model.clone(),
            cook.secret_env.clone(),
            cook.provider_config.clone(),
        );
        let request = dispatch_service::resolve_dispatch_request_with_default_and_catalog(
            command,
            provider::default_backend_for_component,
            catalog,
        )
        .map_err(with_provider_admission_remediation)?;
        validate_provider_route(&request, catalog).map_err(with_provider_admission_remediation)?;
        cook.backend = Some(request.backend);
        cook.selector = request.selector;
        cook.model = request.model;
    }
    Ok(())
}

fn selected_provider_profile(name: Option<&str>) -> Option<AgentTaskProviderProfileDeclaration> {
    let name = name?.trim();
    AgentTaskProviderCatalog::discover()
        .providers()
        .iter()
        .flat_map(|provider| provider.cli.profiles.iter())
        .find(|profile| profile.name == name)
        .cloned()
}

/// Planning and execution report the same provider selection.
///
/// Deferring profile resolution while planning meant a dry run could report a
/// selection that execution then rejected, which is the one thing a dry run
/// exists to rule out. There is one answer now, and planning gives it.
fn provider_selection_preflight(args: &AgentTaskFanoutCookBatchArgs) -> Value {
    let warnings = provider_selection_warnings(args);
    serde_json::json!({
        "profile": args.provider_profile,
        "executor": {
            "backend": args.backend,
            "selector": args.selector,
        },
        "model": args.model,
        "provider_config": args.provider_config.as_ref().map(|_| "provided"),
        "warnings": warnings,
    })
}

fn provider_selection_warnings(args: &AgentTaskFanoutCookBatchArgs) -> Vec<String> {
    match args.provider_profile.as_deref() {
        Some(name) if selected_provider_profile(Some(name)).is_none() => vec![format!(
            "provider profile '{name}' is not declared by installed executor providers; run `homeboy agent-task providers` to inspect available profiles"
        )],
        _ => Vec::new(),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct IssueRef {
    url: String,
    owner: String,
    repo: String,
    number: String,
    key: String,
}

impl IssueRef {
    fn parse(url: &str) -> Result<Self> {
        let trimmed = url.trim();
        let marker = "/issues/";
        let Some((prefix, number_part)) = trimmed.split_once(marker) else {
            return Err(invalid_fanout(
                "cook-batch issue inputs must be GitHub issue URLs",
            ));
        };
        let number = number_part
            .split(['/', '?', '#'])
            .next()
            .unwrap_or_default();
        if number.is_empty() || !number.chars().all(|c| c.is_ascii_digit()) {
            return Err(invalid_fanout(
                "GitHub issue URL is missing a numeric issue number",
            ));
        }
        let mut segments = prefix.trim_end_matches('/').rsplit('/');
        let repo = segments.next().unwrap_or_default();
        let owner = segments.next().unwrap_or_default();
        if owner.is_empty() || repo.is_empty() {
            return Err(invalid_fanout(
                "GitHub issue URL must include owner and repo",
            ));
        }
        let key = format!("{owner}/{repo}#{number}");
        Ok(Self {
            url: trimmed.to_string(),
            owner: owner.to_string(),
            repo: repo.to_string(),
            number: number.to_string(),
            key,
        })
    }
}

fn render_prompt(
    template: Option<&str>,
    issue: &IssueRef,
    repo: &str,
    branch: &str,
    worktree: &str,
) -> String {
    let template = template.unwrap_or(
        "Fix {issue_url}. Inspect the issue, implement the smallest correct change in {repo}, run the requested verification gates, and report the changed files plus verification results. Homeboy deterministic finalization is enabled: Homeboy will commit, push the prepared branch, create or update the PR, add AI disclosure, and finalize reviewer-ready evidence after gates pass. Do not inspect credentials, configure git identity, commit, push, or create or update the PR yourself.",
    );
    template
        .replace("{issue_url}", &issue.url)
        .replace("{issue_ref}", &issue.key)
        .replace("{repo}", repo)
        .replace("{branch}", branch)
        .replace("{worktree}", worktree)
}

fn provider_readiness_command(args: &AgentTaskFanoutCookBatchArgs) -> Vec<String> {
    let mut command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "providers".to_string(),
    ];
    if let Some(backend) = &args.backend {
        command.push(format!("--backend={backend}"));
    }
    if let Some(selector) = &args.selector {
        command.push(format!("--selector={selector}"));
    }
    for secret in &args.secret_env {
        command.push(format!("--secret-env={secret}"));
    }
    command.push("--validate-readiness".to_string());
    command
}

fn worktree_create_command(args: &AgentTaskFanoutCookBatchArgs, branch: &str) -> Vec<String> {
    vec![
        "worktree".to_string(),
        "create".to_string(),
        args.repo.clone(),
        "--branch".to_string(),
        branch.to_string(),
        "--from".to_string(),
        cook_batch_from(args).to_string(),
    ]
}

fn cook_batch_plan_command(args: &AgentTaskFanoutCookBatchArgs, placement: Placement) -> String {
    let mut planned = args.clone();
    planned.preview = true;
    planned.run_plan = false;
    quote_args(&cook_batch_argv_with_placement(&planned, placement))
}

fn pin_cook_batch_replay(
    args: &AgentTaskFanoutCookBatchArgs,
    fanout_id: &str,
) -> AgentTaskFanoutCookBatchArgs {
    let mut pinned = args.clone();
    pinned.fanout_id = Some(fanout_id.to_string());
    pinned
}

/// Error envelopes must be safe to persist or render. A private gate can only
/// be replayed from the original local invocation, never by echoing its bytes.
#[cfg(test)]
fn dry_run_replay_command(args: &AgentTaskFanoutCookBatchArgs) -> String {
    dry_run_replay_command_with_placement(args, Placement::Auto)
}

fn dry_run_replay_command_with_placement(
    args: &AgentTaskFanoutCookBatchArgs,
    placement: Placement,
) -> String {
    if !args.gates.private_verify.is_empty()
        || !args.gates.private_verify_file.is_empty()
        || args
            .verification_profiles
            .as_deref()
            .is_some_and(|spec| spec.starts_with('@') || spec.trim() == "-")
    {
        return "[redacted: re-run the original local private Cook-batch invocation]".to_string();
    }
    cook_batch_plan_command(args, placement)
}

/// Typed plans are always visible declarations. Private programs remain the
/// explicit shell-only escape hatch handled above.
fn has_private_gate_declaration(args: &AgentTaskFanoutCookBatchArgs) -> bool {
    if !args.gates.private_verify.is_empty() || !args.gates.private_verify_file.is_empty() {
        return true;
    }
    false
}

fn secure_batch_plan_execution(fanout_id: &str, placement: Placement) -> String {
    let path = private_batch_plan_path(fanout_id)
        .expect("Homeboy data directory is required for private batch plans");
    private_artifact_run_command_with_placement(&path, placement)
}

fn private_artifact_run_command_with_placement(
    path: &std::path::Path,
    placement: Placement,
) -> String {
    let mut command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "run-plan".to_string(),
        "--input".to_string(),
        format!("@{}", path.display()),
    ];
    command.splice(1..1, fanout_global_placement_args(placement));
    quote_args(&command)
}

fn fanout_command(placement: Placement, command: &str, fanout_id: &str) -> String {
    let mut argv = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        command.to_string(),
        fanout_id.to_string(),
    ];
    argv.splice(1..1, fanout_global_placement_args(placement));
    quote_args(&argv)
}

fn run_next_command(placement: Placement, fanout_id: &str) -> String {
    let mut argv = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-next".to_string(),
        "--fanout".to_string(),
        fanout_id.to_string(),
    ];
    argv.splice(1..1, fanout_global_placement_args(placement));
    quote_args(&argv)
}

fn fanout_global_placement_args(placement: Placement) -> Vec<String> {
    let mut args = Vec::new();
    if placement != Placement::Auto {
        args.extend([
            "--placement".to_string(),
            clap::ValueEnum::to_possible_value(&placement)
                .expect("placement has a clap value")
                .get_name()
                .to_string(),
        ]);
    }
    if let Some(runner_id) = homeboy::core::parsed_command_preflight::captured_result()
        .filter(|preflight| preflight.placement.requested == placement)
        .and_then(|preflight| preflight.placement.runner)
        .filter(|runner| {
            runner.source == homeboy_lab_runner_contract::RunnerSelectionSource::Explicit
        })
        .map(|runner| runner.runner_id)
    {
        args.extend(["--runner".to_string(), runner_id]);
    }
    args
}

#[cfg(test)]
fn cook_batch_run_command(args: &AgentTaskFanoutCookBatchArgs) -> String {
    cook_batch_run_command_with_placement(args, Placement::Auto)
}

fn cook_batch_run_command_with_placement(
    args: &AgentTaskFanoutCookBatchArgs,
    placement: Placement,
) -> String {
    let mut runnable = args.clone();
    runnable.preview = false;
    runnable.run_plan = true;
    quote_args(&cook_batch_argv_with_placement(&runnable, placement))
}

/// The command map for a cook-batch envelope.
///
/// `status` and `retry` used to live here as English sentences — "inspect each
/// cook result under plan.cooks and use agent-task status <run-id>" — in a map
/// whose every other entry was an executable command line. They are gone:
/// [`cook_batch_next_actions`] now emits both as typed, executable
/// [`CommandNextAction`]s bound to the ids this run actually produced, which is
/// what the prose was gesturing at.
///
/// `resume_from_plan` stays prose because it genuinely is not one command: it
/// requires the caller to write `.plan` to a file first. Naming a command that
/// cannot be executed as-is would be worse than saying so.
#[cfg(test)]
fn cook_batch_commands(
    args: &AgentTaskFanoutCookBatchArgs,
    has_private_gates: bool,
    private_artifact_path: Option<&std::path::Path>,
) -> Value {
    cook_batch_commands_with_placement(
        args,
        Placement::Auto,
        has_private_gates,
        private_artifact_path,
    )
}

fn cook_batch_commands_with_placement(
    args: &AgentTaskFanoutCookBatchArgs,
    placement: Placement,
    has_private_gates: bool,
    private_artifact_path: Option<&std::path::Path>,
) -> Value {
    if has_private_gates {
        return serde_json::json!({
            "plan": "[redacted: private verification gates cannot be rendered in a public rerun command]",
            "run": private_artifact_path.map_or_else(
                || "[unavailable: private plan is not persisted until concrete worktrees are bound; re-run the original local invocation after remediation]".to_string(),
                |path| private_artifact_run_command_with_placement(path, placement),
            ),
            "resume_from_plan": "[unavailable until Homeboy binds and persists the private plan]",
        });
    }
    let resume_from_plan = format!(
        "save .plan to JSON and run {}",
        private_artifact_run_command_with_placement(Path::new("batch-cook-plan.json"), placement,)
    );
    serde_json::json!({
        "plan": cook_batch_plan_command(args, placement),
        "run": cook_batch_run_command_with_placement(args, placement),
        "resume_from_plan": resume_from_plan,
    })
}

/// Executable next actions for a cook-batch envelope.
///
/// # Why these are typed
///
/// The sibling `agent-task status` command emits [`CommandNextAction`]s with
/// [`CommandNextActionKind`]s and real command lines. This one emitted
/// sentences addressed to a human — "repair worktree queue blockers reported
/// under worktrees.rows" — which an orchestrator cannot execute, cannot
/// classify, and cannot even reliably parse an id out of.
///
/// # Why the branches differ
///
/// `homeboy agent-task fanout status|artifacts|resume <fanout_id>` all read a
/// durable batch record. A named run-plan creates it before preflight, so a
/// blocked durable batch exposes status and artifacts alongside repair steps.
#[cfg(test)]
fn cook_batch_next_actions(
    args: &AgentTaskFanoutCookBatchArgs,
    fanout_id: &str,
    status: &str,
    executed: bool,
    resume_legal: bool,
    worktrees: &worktree::WorktreeQueueCreateOutput,
    has_private_gates: bool,
    private_artifact_path: Option<&std::path::Path>,
) -> Vec<CommandNextAction> {
    cook_batch_next_actions_with_placement(
        args,
        Placement::Auto,
        fanout_id,
        status,
        executed,
        resume_legal,
        worktrees,
        has_private_gates,
        private_artifact_path,
    )
}

fn cook_batch_next_actions_with_placement(
    args: &AgentTaskFanoutCookBatchArgs,
    placement: Placement,
    fanout_id: &str,
    status: &str,
    executed: bool,
    resume_legal: bool,
    worktrees: &worktree::WorktreeQueueCreateOutput,
    has_private_gates: bool,
    private_artifact_path: Option<&std::path::Path>,
) -> Vec<CommandNextAction> {
    let blocked_rows = worktrees
        .rows
        .iter()
        .filter(|row| {
            !matches!(
                row.status,
                worktree::WorktreeQueueCreateStatus::Created
                    | worktree::WorktreeQueueCreateStatus::WouldCreate
            )
        })
        .collect::<Vec<_>>();

    if !blocked_rows.is_empty() {
        // Every blocked row already carries the exact command that would
        // create it. Emitting that is the difference between "repair the
        // blockers" and something an orchestrator can run.
        let mut actions = blocked_rows
            .iter()
            .map(|row| {
                CommandNextAction::new(
                    format!("create blocked worktree {}", row.handle),
                    quote_args(&row.command),
                )
                .with_kind(CommandNextActionKind::Repair)
            })
            .collect::<Vec<_>>();
        // Created worktrees are recorded, so the same command is idempotent
        // over the ones that already succeeded.
        let command = if has_private_gates {
            private_artifact_path.map_or_else(
                || "[unavailable: resolve worktree blockers, then re-run the original local private Cook-batch invocation]".to_string(),
                |path| private_artifact_run_command_with_placement(path, placement),
            )
        } else {
            cook_batch_run_command_with_placement(args, placement)
        };
        actions.push(
            CommandNextAction::new("rerun this cook-batch once the worktrees exist", command)
                .with_kind(CommandNextActionKind::Repair),
        );
        if executed {
            actions.push(
                CommandNextAction::new(
                    "show persisted batch status",
                    fanout_command(placement, "status", fanout_id),
                )
                .with_kind(CommandNextActionKind::Show),
            );
            actions.push(
                CommandNextAction::new(
                    "list persisted batch artifacts",
                    fanout_command(placement, "artifacts", fanout_id),
                )
                .with_kind(CommandNextActionKind::Artifacts),
            );
        }
        return actions;
    }

    if executed {
        let mut actions = vec![
            CommandNextAction::new(
                "show batch status",
                fanout_command(placement, "status", fanout_id),
            )
            .with_kind(CommandNextActionKind::Show),
            CommandNextAction::new(
                "list batch artifacts",
                fanout_command(placement, "artifacts", fanout_id),
            )
            .with_kind(CommandNextActionKind::Artifacts),
        ];
        // Resume idempotently harvests children that stopped short of
        // finalization. A batch that already succeeded has nothing to harvest,
        // so offering it there would be noise rather than an action.
        if status != "succeeded" && resume_legal {
            actions.push(
                CommandNextAction::new(
                    "resume children that stopped short of finalization",
                    fanout_command(placement, "resume", fanout_id),
                )
                .with_kind(CommandNextActionKind::Repair),
            );
        }
        return actions;
    }

    if has_private_gates {
        return vec![CommandNextAction::new(
            "private gates require bound trusted plan persistence",
            private_artifact_path.map_or_else(
                || "[unavailable: re-run the original local private Cook-batch invocation after worktree binding]".to_string(),
                |path| private_artifact_run_command_with_placement(path, placement),
            ),
        )
        .with_kind(CommandNextActionKind::Repair)];
    }
    vec![
        CommandNextAction::new(
            "re-plan this cook-batch",
            cook_batch_plan_command(args, placement),
        )
        .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new(
            "execute this cook-batch",
            cook_batch_run_command_with_placement(args, placement),
        )
        .with_kind(CommandNextActionKind::Repair),
    ]
}

/// A batch resume is legal only when every incomplete child explicitly permits
/// recovery. The batch aggregate alone cannot establish that: a recipe-only
/// child can be terminal while having no lifecycle record to resume.
fn batch_resume_is_legal(result: &Value) -> bool {
    let Some(cooks) = result.get("cooks").and_then(Value::as_array) else {
        return false;
    };
    let incomplete = cooks
        .iter()
        .filter(|cook| {
            let status = cook
                .get("status")
                .and_then(Value::as_str)
                .unwrap_or_default();
            !homeboy::core::run_lifecycle_status::RunLifecycleStatus::from(
                &homeboy::core::cook_status::CookStatus::from_status(status),
            )
            .is_success()
        })
        .collect::<Vec<_>>();
    !incomplete.is_empty()
        && incomplete.iter().all(|cook| {
            cook.pointer("/result/failure_context/recovery_legal") == Some(&Value::Bool(true))
        })
}

fn trim_slashes(value: &str) -> String {
    value.trim_matches('/').to_string()
}

fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut last_dash = false;
    for ch in value.chars() {
        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            last_dash = false;
        } else if !last_dash {
            slug.push('-');
            last_dash = true;
        }
    }
    slug.trim_matches('-').to_string()
}

fn reject_generic_fanout_inputs(value: &Value) -> Result<()> {
    let schema = value.get("schema").and_then(Value::as_str);
    if matches!(
        schema,
        Some("homeboy/agent-task-plan/v1" | "homeboy/agent-task-fanout-plan/v1")
    ) || value.is_array()
        || value.get("tasks").is_some()
        || value.get("packets").is_some()
    {
        return Err(invalid_fanout(
            "agent-task fanout now accepts only batch cook plans with independent cooks; generic task fanout belongs behind internal scheduler code",
        ));
    }
    Ok(())
}

fn invalid_fanout(message: &str) -> Error {
    Error::validation_invalid_argument("input", message.to_string(), None, None)
}

fn batch_cook_fanout_plan_schema() -> String {
    AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA.to_string()
}

fn one_usize() -> usize {
    1
}

fn default_max_attempts() -> u32 {
    3
}

fn default_base() -> String {
    "main".to_string()
}

fn default_private_gate_reveal() -> AgentTaskGateRevealPolicy {
    AgentTaskGateRevealPolicy::SummaryOnly
}

fn default_gate_timeout_seconds() -> u64 {
    30 * 60
}

fn default_gate_heartbeat_interval_seconds() -> u64 {
    5
}

fn default_gate_no_progress_timeout_seconds() -> u64 {
    5 * 60
}

fn default_ai_tool() -> String {
    GENERIC_AI_DISCLOSURE.to_string()
}

/// Generic fallback disclosure used when no provider supplies one. Also the
/// sentinel that marks `ai_tool` as "not explicitly overridden by the operator",
/// so a `--model` selection can derive a concrete disclosure instead.
const GENERIC_AI_DISCLOSURE: &str = "AI-assisted";

/// Resolve the effective `ai_tool` disclosure for a cook.
///
/// When the operator explicitly supplied `--ai-tool`, that value is preserved.
/// Otherwise (`ai_tool` is empty or the generic default) the disclosure is
/// derived from the effective backend/selector/model via the provider catalog —
/// the single typed disclosure source — so a `--model` override produces a
/// correct AI-assistance statement rather than a stale hard-coded default.
/// (#8404)
pub(super) fn resolve_ai_tool_disclosure(
    ai_tool: &str,
    backend: Option<&str>,
    selector: Option<&str>,
    model: Option<&str>,
) -> String {
    let is_operator_override =
        !ai_tool.trim().is_empty() && ai_tool.trim() != GENERIC_AI_DISCLOSURE;
    if is_operator_override {
        return ai_tool.to_string();
    }

    let backend = match backend {
        Some(backend) if !backend.trim().is_empty() => backend.to_string(),
        _ => match provider::default_backend().ok().flatten() {
            Some(backend) => backend,
            None => return ai_tool.to_string(),
        },
    };

    AgentTaskProviderCatalog::discover()
        .ai_disclosure_for(&backend, selector, model)
        .unwrap_or_else(|| ai_tool.to_string())
}

/// Legacy AI-usage disclosure default. The reviewer-facing "Used for" text is
/// now authored by the agent's `review_form.used_for` and enforced by the cook
/// loop's review-form gate, so this no longer feeds the PR body. Defaults empty
/// (no canned platitude); retained only for recipe back-compatibility.
fn default_ai_used_for() -> String {
    String::new()
}

fn batch_commands(batch_id: &str, placement: Placement) -> Value {
    serde_json::json!({
        "status": fanout_command(placement, "status", batch_id),
        "artifacts": fanout_command(placement, "artifacts", batch_id),
        "run_next": run_next_command(placement, batch_id)
    })
}

#[cfg(test)]
mod tests {

    /// Tests are the entry point for their own unit of work, so the store
    /// resolves once here (#7505).
    fn test_lifecycle_store() -> homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore {
        homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("lifecycle store")
    }
    use super::*;
    use crate::cli_surface::{Cli, Commands, Placement};
    use crate::commands::agent_task::{AgentTaskCommand, AgentTaskFanoutCommand};
    use crate::test_support::{env_lock, with_isolated_home};
    use clap::{CommandFactory, Parser};
    use serde_json::json;
    use sha2::{Digest, Sha256};

    #[test]
    fn cook_batch_help_documents_the_complete_verification_profile_contract() {
        let mut command = Cli::command()
            .find_subcommand("agent-task")
            .expect("agent-task command")
            .find_subcommand("fanout")
            .expect("fanout command")
            .find_subcommand("cook-batch")
            .expect("cook-batch command")
            .clone();
        let help = command.render_long_help().to_string();

        for expected in [
            "--verification-profiles <JSON>",
            "Complete example:",
            "https://github.com/owner/repo/issues/123",
            "issue-number",
            "typed `plan`",
            "suite_timeout_seconds",
            "\"homeboy\",\"review\",\"test\"",
        ] {
            assert!(help.contains(expected), "missing {expected}:\n{help}");
        }
    }

    #[test]
    fn verification_profile_parser_rejects_unknown_fields_with_paths_and_example() {
        for (declaration, expected_path) in [
            (
                r#"{"profiles":{},"assignments":[],"per_issue":{}}"#,
                "$.per_issue",
            ),
            (
                r#"{"profiles":{"rust":{"plan":{"adapter":"homeboy_review_test","command":["homeboy","review","test"],"suite_timeout_seconds":30,"unexpected":true}}},"assignments":[]}"#,
                "$.profiles.rust.plan.unexpected",
            ),
            (
                r#"{"profiles":{"rust":{"plan":{"adapter":"homeboy_review_test","command":["homeboy","review","test"],"suite_timeout_seconds":30}}},"assignments":[{"selector":"issue-1","profile":"rust","append":true}]}"#,
                "$.assignments[0].append",
            ),
        ] {
            let error = parse_verification_profiles(declaration)
                .expect_err("unknown verification profile field");
            assert_eq!(error.details["field"], "verification-profiles");
            assert!(
                error.details["problem"]
                    .as_str()
                    .expect("typed problem")
                    .contains(expected_path),
                "{error}"
            );
            assert!(
                error.details["tried"][0]
                    .as_str()
                    .expect("corrected example")
                    .contains(VERIFICATION_PROFILES_EXAMPLE),
                "{error}"
            );
        }
    }

    #[test]
    fn verification_profile_rejects_noncanonical_review_test_argv() {
        let mut args = cook_batch_args();
        args.verification_profiles = Some(
            r#"{"profiles":{"invalid":{"plan":{"adapter":"homeboy_review_test","command":["cargo","test"],"suite_timeout_seconds":30}}},"assignments":[{"selector":"issue-6453","profile":"invalid"}]}"#.to_string(),
        );

        let error =
            build_cook_batch_plan(&args).expect_err("adapter argv is validated before planning");
        assert_eq!(
            error.details["field"],
            "verification-profiles.profiles.plan"
        );
        assert!(error.message.contains("homeboy review test"), "{error}");
    }

    fn source(
        command: &str,
        visibility: homeboy::agents::agent_tasks::gate::AgentTaskGateVisibility,
    ) -> homeboy::agents::agent_tasks::gate::AgentTaskGateInputSource {
        homeboy::agents::agent_tasks::gate::AgentTaskGateInputSource {
            visibility,
            source_kind: "file".to_string(),
            path: (visibility
                == homeboy::agents::agent_tasks::gate::AgentTaskGateVisibility::Visible)
                .then(|| "gate.sh".to_string()),
            sha256: format!("sha256:{:x}", Sha256::digest(command.as_bytes())),
            size_bytes: command.len() as u64,
            redaction_policy: AgentTaskGateRevealPolicy::SummaryOnly,
        }
    }

    #[test]
    fn public_fanout_projection_redacts_private_gate_text() {
        let mut plan = test_batch_plan();
        plan.cooks[0].private_verify = vec!["printf private-token".to_string()];
        let output =
            serde_json::to_value(public_batch_cook_plan(&plan)).expect("serialize public plan");
        assert!(!output.to_string().contains("private-token"));
        let gates = effective_batch_cook_gates(&plan);
        assert!(!serde_json::to_string(&gates)
            .unwrap()
            .contains("private-token"));
    }

    #[test]
    fn profile_gate_provenance_tracks_only_executed_shared_gates() {
        use homeboy::agents::agent_tasks::gate::AgentTaskGateVisibility;
        let shared_public = "shared public".to_string();
        let shared_private = "shared private".to_string();
        let profile_public = "profile public".to_string();
        let sources = vec![
            source(&shared_public, AgentTaskGateVisibility::Visible),
            source(&shared_private, AgentTaskGateVisibility::Private),
        ];
        let selected = sources_for_executed_gates(&sources, &[profile_public], &[]);
        assert!(
            selected.is_empty(),
            "replace profile omits shared provenance"
        );
        let selected = sources_for_executed_gates(&sources, &[shared_public], &[shared_private]);
        assert_eq!(
            selected, sources,
            "append profile keeps executed shared provenance"
        );
    }

    #[test]
    fn raw_private_gates_still_use_redacted_public_paths() {
        let sentinel = "PRIVATE_PROFILE_SENTINEL";
        let mut args = cook_batch_args();
        args.gates.private_verify = vec![sentinel.to_string()];
        assert!(has_private_gate_declaration(&args));
        let commands = cook_batch_commands(&args, true, None);
        assert!(!commands.to_string().contains(sentinel));
        assert!(commands["run"].as_str().unwrap().contains("unavailable"));
    }

    #[test]
    fn private_profile_sentinel_is_redacted_across_repo_error_dry_run_and_bound_plan() {
        let sentinel = "PRIVATE_PROFILE_ALL_STATES_SENTINEL";
        with_isolated_home(|home| {
            let mut invalid = cook_batch_args();
            invalid.repo = "homeboy@bad".to_string();
            invalid.gates.private_verify = vec![sentinel.to_string()];
            let error = normalize_cook_batch_repo(&mut invalid).expect_err("invalid repo");
            assert!(!format!("{} {:?}", error.message, error.details).contains(sentinel));

            let mut dry = cook_batch_args();
            dry.gates.private_verify = vec![sentinel.to_string()];
            dry.preview = true;
            let plan = build_cook_batch_plan(&dry).expect("profile plan");
            let public = serde_json::to_string(&public_batch_cook_plan(&plan)).unwrap();
            assert!(!public.contains(sentinel));
            let commands = cook_batch_commands(&dry, true, None);
            assert!(!commands.to_string().contains(sentinel));
            assert!(!commands["run"].as_str().unwrap().contains("run-plan"));

            let primary = home.path().join("primary");
            fs::create_dir(&primary).expect("primary");
            init_git_primary(&primary);
            write_component_registration(home.path(), "homeboy", &primary);
        });
        with_materialized_cook_batch_worktrees(|| {
            let mut executable = cook_batch_args();
            executable.gates.private_verify = vec![sentinel.to_string()];
            executable.preview = false;
            executable.run_plan = false;
            let resolved = build_cook_batch_plan(&executable).expect("resolve executable profile");
            assert!(resolved
                .cooks
                .iter()
                .any(|cook| cook.private_verify.iter().any(|gate| gate == sentinel)));
            let (public, _) = cook_batch(executable).expect("bound private profile plan");
            assert!(!public.to_string().contains(sentinel));
            assert!(public["commands"]["run"]
                .as_str()
                .expect("private run command")
                .contains("run-plan"));
            let path = private_batch_plan_path(public["fanout_id"].as_str().unwrap()).unwrap();
            assert!(path.exists());
            let loaded = load_batch_cook_fanout_plan(
                &AgentTaskFanoutInputArgs {
                    input: format!("@{}", path.display()),
                    fanout_id: None,
                    backend: None,
                    selector: None,
                    model: None,
                },
                true,
            )
            .expect("trusted bound plan");
            assert!(loaded
                .cooks
                .iter()
                .any(|cook| cook.private_verify.iter().any(|gate| gate == sentinel)));
        });
    }

    #[test]
    fn public_cook_batch_commands_and_actions_never_embed_private_gate_text() {
        let sentinel = "PRIVATE_GATE_SENTINEL_12230";
        let mut args = cook_batch_args();
        args.gates.private_verify = vec![sentinel.to_string()];
        let commands = cook_batch_commands(&args, true, None);
        let worktrees = worktree::WorktreeQueueCreateOutput {
            schema: "test",
            repo: "homeboy".to_string(),
            base_ref: "main".to_string(),
            dry_run: true,
            rows: Vec::new(),
        };
        let actions = cook_batch_next_actions(
            &args,
            "fanout-private",
            "ready",
            false,
            false,
            &worktrees,
            true,
            None,
        );
        assert!(!commands.to_string().contains(sentinel));
        assert!(!serde_json::to_string(&actions).unwrap().contains(sentinel));
        assert!(commands["run"].as_str().unwrap().contains("unavailable"));
    }

    #[test]
    fn private_dry_run_and_blocked_projections_never_advertise_unpersisted_artifacts() {
        with_isolated_home(|_| {
            let sentinel = "PRIVATE_UNPERSISTED_SENTINEL";
            let mut args = cook_batch_args();
            args.gates.private_verify = vec![sentinel.to_string()];
            args.preview = true;
            let artifact = private_batch_plan_path("issue-wave").expect("artifact path");
            assert!(!artifact.exists(), "dry-run begins without an artifact");
            let commands = cook_batch_commands(&args, true, None);
            assert!(!commands.to_string().contains(sentinel));
            assert!(!commands["run"].as_str().unwrap().contains("run-plan"));
            assert!(
                !artifact.exists(),
                "projection must not persist dry-run state"
            );

            let worktrees = worktree_output(vec![worktree_row(
                "homeboy@fix-a",
                worktree::WorktreeQueueCreateStatus::Failed,
            )]);
            let actions = cook_batch_next_actions(
                &args,
                "issue-wave",
                "blocked",
                false,
                false,
                &worktrees,
                true,
                None,
            );
            let rendered = serde_json::to_string(&actions).unwrap();
            assert!(!rendered.contains(sentinel));
            assert!(!rendered.contains("run-plan"));
            assert!(
                !artifact.exists(),
                "blocked projection must not persist state"
            );
        });
    }

    #[test]
    fn private_batch_plan_command_loads_snapshot_and_rejects_tampering() {
        with_isolated_home(|_| {
            let sentinel = "PRIVATE_GATE_SENTINEL_12230";
            let mut plan = test_batch_plan();
            plan.fanout_id = "private-plan-test".to_string();
            plan.cooks[0].private_verify = vec![sentinel.to_string()];
            let path = persist_private_batch_plan(&plan).expect("persist private plan");
            assert!(path.ends_with("agent-task/private-batch-plans/private-plan-test.json"));
            #[cfg(unix)]
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            let args = AgentTaskFanoutInputArgs {
                input: format!("@{}", path.display()),
                fanout_id: None,
                backend: None,
                selector: None,
                model: None,
            };
            let loaded = load_batch_cook_fanout_plan(&args, true).expect("load private plan");
            assert_eq!(loaded.cooks[0].private_verify, vec![sentinel]);
            let command = secure_batch_plan_execution(&plan.fanout_id, Placement::Auto);
            assert!(command.contains(&path.display().to_string()));
            assert!(!command.contains(sentinel));

            let mut envelope: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            envelope["plan"]["cooks"][0]["private_verify"][0] = json!("tampered");
            fs::write(&path, serde_json::to_vec(&envelope).unwrap()).expect("tamper private plan");
            let error = load_batch_cook_fanout_plan(&args, true).expect_err("tampered plan fails");
            assert!(!format!("{} {:?}", error.message, error.details).contains(sentinel));
            fs::remove_file(&path).expect("remove private plan");
            assert!(load_batch_cook_fanout_plan(&args, true).is_err());
        });
    }

    #[test]
    fn private_batch_plan_path_uses_canonical_configured_data_root() {
        with_isolated_home(|home| {
            let canonical = homeboy::core::paths::homeboy_data().expect("canonical data root");
            assert_ne!(canonical, home.path());
            assert_eq!(
                private_batch_plan_path("path test").expect("private path"),
                canonical
                    .join("agent-task/private-batch-plans")
                    .join("path_test.json")
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn private_plan_temp_and_parent_are_owner_only_before_write() {
        with_isolated_home(|_| {
            let parent = private_batch_plan_dir_in_roots(
                &homeboy::core::paths::homeboy_data().expect("data root"),
            );
            fs::create_dir_all(&parent).expect("create parent");
            fs::set_permissions(&parent, fs::Permissions::from_mode(0o755))
                .expect("make parent permissive for repair test");
            let mut plan = test_batch_plan();
            plan.fanout_id = "mode-boundary".to_string();
            let path = persist_private_batch_plan(&plan).expect("persist plan");
            assert_eq!(
                fs::metadata(&parent).unwrap().permissions().mode() & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );

            let temporary = path.with_extension("prewrite.tmp");
            let file = create_private_plan_temp(&temporary).expect("create private temp");
            assert_eq!(file.metadata().unwrap().permissions().mode() & 0o777, 0o600);
            drop(file);
            fs::remove_file(temporary).expect("remove temp");
        });
    }

    #[test]
    fn private_plan_read_only_projection_redacts_and_execution_retains_gate() {
        with_isolated_home(|_| {
            let sentinel = "PRIVATE_GATE_SENTINEL_12230";
            let mut plan = test_batch_plan();
            plan.fanout_id = "private-plan-projection".to_string();
            plan.cooks[0].private_verify = vec![sentinel.to_string()];
            let path = persist_private_batch_plan(&plan).expect("persist private plan");
            let args = AgentTaskFanoutInputArgs {
                input: format!("@{}", path.display()),
                fanout_id: None,
                backend: None,
                selector: None,
                model: None,
            };
            let execution =
                load_batch_cook_fanout_plan(&args, true).expect("trusted execution load");
            assert_eq!(execution.cooks[0].private_verify, vec![sentinel]);
            let public = serde_json::to_string(&public_batch_cook_plan(&execution)).unwrap();
            assert!(!public.contains(sentinel));
        });
    }

    #[test]
    fn private_envelope_outside_controller_path_is_rejected() {
        with_isolated_home(|_| {
            let mut plan = test_batch_plan();
            plan.fanout_id = "private-path-check".to_string();
            plan.cooks[0].private_verify = vec!["secret".to_string()];
            let path = persist_private_batch_plan(&plan).expect("persist private plan");
            let escaped = path.with_file_name("escaped.json");
            fs::copy(&path, &escaped).expect("copy envelope outside owned name");
            let args = AgentTaskFanoutInputArgs {
                input: format!("@{}", escaped.display()),
                fanout_id: None,
                backend: None,
                selector: None,
                model: None,
            };
            assert!(load_batch_cook_fanout_plan(&args, true).is_err());
        });
    }
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;

    #[derive(Debug)]
    struct FailingFanoutDispatcher;

    impl crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher for FailingFanoutDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(json!({ "kind": "failing-fanout-dispatcher" }))
        }

        fn dispatch_attempt(
            &self,
            _plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
            _run_id: &str,
            _derived_cook_baseline: Option<
                &homeboy::agents::agent_task_service::DerivedCookBaselineCapability,
            >,
        ) -> Result<()> {
            Err(Error::internal_unexpected("fixture dispatcher failed"))
        }
    }

    fn test_concurrency_decision() -> BatchConcurrencyDecision {
        BatchConcurrencyDecision {
            limit: 2,
            source: homeboy::agents::agent_task_scheduler::BatchConcurrencySource::ChildCount,
            reason: "batch has 2 children".to_string(),
        }
    }

    fn test_batch_plan() -> BatchCookFanoutPlan {
        BatchCookFanoutPlan::from_value(
            json!({
                "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                "fanout_id": "test-batch",
                "cooks": [
                    {"cook_id": "first", "prompt": "first", "cwd": env!("CARGO_MANIFEST_DIR"), "to_worktree": "homeboy@first", "verify": ["true"]},
                    {"cook_id": "second", "prompt": "second", "cwd": env!("CARGO_MANIFEST_DIR"), "to_worktree": "homeboy@second", "verify": ["true"]}
                ]
            }),
            &args(),
        )
        .expect("test batch plan")
    }

    #[derive(Debug)]
    struct LabRecipeDispatcher;

    impl homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher for LabRecipeDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(json!({
                "kind": "lab",
                "runner_id": "test-lab",
                "execution_placement_decision": {},
                "allow_local_fallback": true,
                "allow_dirty_lab_workspace": false,
                "skip_deps_hydration": false,
                "detach_after_handoff": false,
                "source_path": null,
                "job_overrides": {
                    "env": {},
                    "secret_env_names": [],
                    "workspace_root": null,
                },
            }))
        }

        fn dispatch_attempt(
            &self,
            _plan: homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
            _run_id: &str,
            _derived_cook_baseline: Option<
                &homeboy::agents::agent_task_service::DerivedCookBaselineCapability,
            >,
        ) -> Result<()> {
            Ok(())
        }
    }

    #[cfg(unix)]
    fn provider_finalization_fixture(
        dirty: bool,
    ) -> (tempfile::TempDir, PathBuf, BatchCookFanoutPlan) {
        let fixture = tempfile::tempdir().expect("provider fixture");
        let workspace = fixture.path().join("workspace");
        let records = fixture.path().join("records");
        let script = fixture.path().join("provider");
        std::fs::create_dir(&workspace).expect("create provider workspace");
        let output = Command::new("git")
            .args(["init", "-q", "-b", "fixture"])
            .current_dir(&workspace)
            .output()
            .expect("initialize provider workspace");
        assert!(output.status.success(), "initialize provider workspace");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  printf '{{\"worktrees\":[{{\"handle\":\"%s\",\"path\":\"{}\",\"branch\":\"fixture\",\"safety\":{{\"dirty\":{},\"unpushed\":false,\"primary\":false}}}}]}}\\n' \"$2\"\nelse\n  printf '%s|%s|%s|%s|%s|%s\\n' \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\" >> '{}'\nfi\n",
                workspace.display(),
                dirty,
                records.display(),
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&script)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("make provider executable");
        let mut config = homeboy::core::defaults::load_config();
        config.worktree_providers.insert(
            "fixture".to_string(),
            homeboy::core::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: homeboy::core::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![
                        script.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    ensure: Some(vec![script.display().to_string(), "ensure".to_string()]),
                    ..Default::default()
                },
                list_result_mapping: Some(
                    homeboy::core::defaults::WorktreeProviderListResultMapping {
                        items: "$.worktrees".to_string(),
                        handle: "$.handle".to_string(),
                        path: "$.path".to_string(),
                        branch: "$.branch".to_string(),
                        dirty: "$.safety.dirty".to_string(),
                        unpushed: "$.safety.unpushed".to_string(),
                        primary: "$.safety.primary".to_string(),
                        task_url: None,
                    },
                ),
            },
        );
        config.settings.insert(
            homeboy::core::worktree_providers::WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY.to_string(),
            json!({ "fixture": { "finalize": [script.display().to_string(), "finalize", "{handle}", "{purpose}", "{owner_run_ref}", "{cleanup_policy}", "{disposition}", "{idempotency_key}"] } }),
        );
        homeboy::core::defaults::save_config(&config).expect("save provider config");
        let mut plan = test_batch_plan();
        for cook in &mut plan.cooks {
            cook.to_worktree = format!("homeboy@{}", cook.cook_id);
        }
        (fixture, records, plan)
    }

    #[cfg(unix)]
    #[test]
    fn dispatcher_fanout_terminalization_finalizes_success_and_failure_provider_records() {
        with_isolated_home(|_| {
            let (_fixture, records, plan) = provider_finalization_fixture(false);
            let report = agent_task_service::AgentTaskCookBatchReport {
                schema: "homeboy/agent-task-cook-batch/v1",
                batch_id: plan.fanout_id.clone(),
                status: "partial_failure".to_string(),
                total: 2,
                queued: 0,
                running: 0,
                succeeded: 1,
                failed: 1,
                cancelled: 0,
                timed_out: 0,
                cooks: plan
                    .cooks
                    .iter()
                    .enumerate()
                    .map(
                        |(index, cook)| agent_task_service::AgentTaskCookBatchCellReport {
                            cook_id: cook.cook_id.clone(),
                            initial_run_id: cook.run_id(),
                            status: if index == 0 { "succeeded" } else { "failed" }.to_string(),
                            exit_code: index as i32,
                            result: None,
                            error: None,
                        },
                    )
                    .collect(),
            };

            finalize_provider_worktrees(&plan, &report, None)
                .expect("dispatcher fanout terminalization");

            let records = std::fs::read_to_string(records).expect("provider finalization records");
            assert!(records.contains(&format!(
                "homeboy@{}|agent_task_cook|{}|remove_on_success|succeeded|finalize:{}",
                plan.cooks[0].cook_id,
                plan.cooks[0].run_id(),
                plan.cooks[0].run_id(),
            )));
            assert!(records.contains(&format!(
                "homeboy@{}|agent_task_cook|{}|remove_on_success|failed|finalize:{}",
                plan.cooks[1].cook_id,
                plan.cooks[1].run_id(),
                plan.cooks[1].run_id(),
            )));
        });
    }

    #[cfg(unix)]
    #[test]
    fn dispatcher_fanout_finalizes_a_terminal_provider_owned_dirty_candidate() {
        with_isolated_home(|_| {
            let (_fixture, records, mut plan) = provider_finalization_fixture(true);
            plan.cooks.truncate(1);
            let cook = &plan.cooks[0];
            let report = agent_task_service::AgentTaskCookBatchReport {
                schema: "homeboy/agent-task-cook-batch/v1",
                batch_id: plan.fanout_id.clone(),
                status: "succeeded".to_string(),
                total: 1,
                queued: 0,
                running: 0,
                succeeded: 1,
                failed: 0,
                cancelled: 0,
                timed_out: 0,
                cooks: vec![agent_task_service::AgentTaskCookBatchCellReport {
                    cook_id: cook.cook_id.clone(),
                    initial_run_id: cook.run_id(),
                    status: "succeeded".to_string(),
                    exit_code: 0,
                    result: None,
                    error: None,
                }],
            };

            finalize_provider_worktrees(&plan, &report, None)
                .expect("terminal owner may finalize its dirty candidate");

            let records = std::fs::read_to_string(records).expect("provider finalization record");
            assert!(records.contains(&format!(
                "homeboy@{}|agent_task_cook|{}|remove_on_success|succeeded|finalize:{}",
                cook.cook_id,
                cook.run_id(),
                cook.run_id(),
            )));
        });
    }

    #[cfg(unix)]
    #[test]
    fn dispatcher_fanout_execution_finalizes_failed_provider_lifecycle() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let (_fixture, records, mut plan) = provider_finalization_fixture(false);
            plan.cooks.truncate(1);
            let dispatcher: &CookAttemptDispatcherFactory =
                &|_| std::sync::Arc::new(FailingFanoutDispatcher);

            let (_, exit_code) =
                run_batch_cook_fanout_plan_with_attempt_dispatcher(plan.clone(), dispatcher)
                    .expect("dispatcher fanout result");

            assert_ne!(exit_code, 0);
            let records = std::fs::read_to_string(records).expect("provider finalization record");
            assert!(records.contains(&format!(
                "homeboy@{}|agent_task_cook|{}|remove_on_success|failed|finalize:{}",
                plan.cooks[0].cook_id,
                plan.cooks[0].run_id(),
                plan.cooks[0].run_id(),
            )));
        });
    }

    #[test]
    fn dispatcher_fanout_finalizes_native_failure_as_preserved() {
        with_isolated_home(|home| {
            let source = home.path().join("Developer/fixture");
            std::fs::create_dir_all(&source).expect("source checkout");
            for args in [
                vec!["init", "--quiet", "-b", "main"],
                vec!["config", "user.email", "test@example.com"],
                vec!["config", "user.name", "Homeboy Test"],
            ] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("git runs")
                    .success());
            }
            std::fs::write(source.join("homeboy.json"), r#"{"id":"fixture"}"#)
                .expect("component manifest");
            assert!(Command::new("git")
                .args(["add", "."])
                .current_dir(&source)
                .status()
                .expect("git add")
                .success());
            assert!(Command::new("git")
                .args(["commit", "--quiet", "-m", "base"])
                .current_dir(&source)
                .status()
                .expect("git commit")
                .success());
            init_git_primary(&source);
            write_component_registration(home.path(), "fixture", &source);

            let mut plan = test_batch_plan();
            plan.cooks.truncate(1);
            plan.cooks[0].to_worktree = "fixture@native-finalization".to_string();
            homeboy::core::worktree::create(homeboy::core::worktree::WorktreeCreateOptions {
                component_id: "fixture".to_string(),
                branch: "native-finalization".to_string(),
                from: Some("main".to_string()),
                task_url: plan.cooks[0].task_url.clone(),
                run_id: Some(plan.cooks[0].run_id()),
                cleanup_policy: Some(homeboy::core::worktree::CleanupPolicy::RemoveWhenSafe),
                require_handoff_freshness: false,
            })
            .expect("native destination");
            let report = agent_task_service::AgentTaskCookBatchReport {
                schema: "homeboy/agent-task-cook-batch/v1",
                batch_id: plan.fanout_id.clone(),
                status: "failed".to_string(),
                total: 1,
                queued: 0,
                running: 0,
                succeeded: 0,
                failed: 1,
                cancelled: 0,
                timed_out: 0,
                cooks: vec![agent_task_service::AgentTaskCookBatchCellReport {
                    cook_id: plan.cooks[0].cook_id.clone(),
                    initial_run_id: plan.cooks[0].run_id(),
                    status: "failed".to_string(),
                    exit_code: 1,
                    result: None,
                    error: None,
                }],
            };

            finalize_provider_worktrees(&plan, &report, None)
                .expect("native terminal finalization");
            let record = homeboy::core::worktree::resolve(&plan.cooks[0].to_worktree)
                .expect("native record");
            assert_eq!(
                record.cleanup_policy,
                homeboy::core::worktree::CleanupPolicy::PreserveOnFailure
            );
            assert_eq!(record.terminal_disposition.as_deref(), Some("failed"));
        });
    }

    struct EnvRestore(Vec<(&'static str, Option<std::ffi::OsString>)>);

    impl EnvRestore {
        fn set(values: &[(&'static str, Option<&str>)]) -> Self {
            let prior = values
                .iter()
                .map(|(name, value)| {
                    let previous = std::env::var_os(name);
                    match value {
                        Some(value) => std::env::set_var(name, value),
                        None => std::env::remove_var(name),
                    }
                    (*name, previous)
                })
                .collect();
            Self(prior)
        }
    }

    impl Drop for EnvRestore {
        fn drop(&mut self) {
            for (name, value) in self.0.drain(..) {
                match value {
                    Some(value) => std::env::set_var(name, value),
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    fn command(path: &Path, program: &str, args: &[&str]) -> String {
        let output = Command::new(program)
            .args(args)
            .current_dir(path)
            .output()
            .unwrap_or_else(|error| panic!("run {program} {args:?}: {error}"));
        assert!(
            output.status.success(),
            "{program} {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn write_fake_gh(root: &Path) -> (PathBuf, PathBuf) {
        let bin = root.join("bin");
        std::fs::create_dir(&bin).expect("fake gh bin");
        let log = root.join("gh.log");
        let script = bin.join("gh");
        std::fs::write(
            &script,
            r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOMEBOY_FAKE_GH_LOG"
if [ "$1 $2" = "pr view" ]; then
  case "$3" in
    upstream) printf '%s\n' "$HOMEBOY_FAKE_UPSTREAM_PR" ;;
    dependent) printf '%s\n' "$HOMEBOY_FAKE_DEPENDENT_PR" ;;
    *) exit 2 ;;
  esac
elif [ "$1 $2" = "pr edit" ] && [ "${HOMEBOY_FAKE_GH_FAIL_EDIT:-}" = "1" ]; then
  exit 1
fi
"#,
        )
        .expect("fake gh script");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755))
                .expect("make fake gh executable");
        }
        (bin, log)
    }

    fn dependency_repo(root: &Path, conflict: bool) -> (PathBuf, String, String) {
        let remote = root.join("remote.git");
        let checkout = root.join("checkout");
        command(
            root,
            "git",
            &["init", "--bare", remote.to_str().expect("remote path")],
        );
        command(
            root,
            "git",
            &[
                "clone",
                remote.to_str().expect("remote path"),
                checkout.to_str().expect("checkout path"),
            ],
        );
        command(
            &checkout,
            "git",
            &["config", "user.email", "test@example.test"],
        );
        command(&checkout, "git", &["config", "user.name", "Fanout Test"]);
        std::fs::write(checkout.join("shared.txt"), "base\n").expect("base file");
        command(&checkout, "git", &["add", "."]);
        command(&checkout, "git", &["commit", "-m", "base"]);
        command(&checkout, "git", &["branch", "-M", "main"]);
        command(&checkout, "git", &["push", "origin", "main"]);

        command(&checkout, "git", &["checkout", "-b", "foundation"]);
        if conflict {
            std::fs::write(checkout.join("shared.txt"), "foundation\n").expect("foundation file");
        } else {
            std::fs::write(checkout.join("foundation.txt"), "foundation\n")
                .expect("foundation file");
        }
        command(&checkout, "git", &["add", "."]);
        command(&checkout, "git", &["commit", "-m", "foundation"]);
        command(&checkout, "git", &["push", "origin", "foundation"]);

        command(&checkout, "git", &["checkout", "-b", "dependent"]);
        if conflict {
            std::fs::write(checkout.join("shared.txt"), "dependent\n").expect("dependent file");
        } else {
            std::fs::write(checkout.join("dependent.txt"), "dependent\n").expect("dependent file");
        }
        command(&checkout, "git", &["add", "."]);
        command(&checkout, "git", &["commit", "-m", "dependent"]);
        command(&checkout, "git", &["push", "origin", "dependent"]);

        command(&checkout, "git", &["checkout", "main"]);
        command(
            &checkout,
            "git",
            &["merge", "--no-ff", "foundation", "-m", "merge foundation"],
        );
        if conflict {
            std::fs::write(checkout.join("shared.txt"), "main\n").expect("main conflict file");
            command(&checkout, "git", &["add", "."]);
            command(&checkout, "git", &["commit", "-m", "main conflict"]);
        }
        let merged = command(&checkout, "git", &["rev-parse", "HEAD"]);
        command(&checkout, "git", &["push", "origin", "main"]);
        command(&checkout, "git", &["checkout", "dependent"]);
        let remote_dependent = command(&checkout, "git", &["rev-parse", "origin/dependent"]);
        (checkout, merged, remote_dependent)
    }

    fn seed_dependency_batch(batch_id: &str, checkout: &Path) {
        let run =
            homeboy::agents::agent_tasks::scheduler::AgentTaskPlan::new("fanout-e2e", Vec::new());
        for (run_id, pr) in [
            ("upstream-run", Some("upstream")),
            ("dependent-run", Some("dependent")),
            ("sibling-run", None),
        ] {
            agent_task_lifecycle::submit_plan(&run, Some(run_id)).expect("durable child run");
            if let Some(pr) = pr {
                agent_task_lifecycle::record_cook_finalization(
                    run_id,
                    json!({ "status": "review_ready", "pr_url": pr }),
                )
                .expect("child PR finalization");
                if run_id == "dependent-run" {
                    agent_task_lifecycle::record_promotion(
                        run_id,
                        json!({ "status": "succeeded" }),
                    )
                    .expect("dependent promotion");
                }
            }
        }
        batch::persist_fanout_run_batch(
            batch_id,
            batch_id,
            &[
                batch::FanoutRunBatchChild { task_id: "upstream".into(), run_id: "upstream-run".into() },
                batch::FanoutRunBatchChild { task_id: "sibling".into(), run_id: "sibling-run".into() },
                batch::FanoutRunBatchChild { task_id: "dependent".into(), run_id: "dependent-run".into() },
            ],
            json!({ "dependency_graph": { "nodes": [
                {"id":"upstream","repository":"repo","worktree":checkout,"head":"foundation","depends_on":[]},
                {"id":"sibling","repository":"repo","worktree":checkout,"head":"sibling","depends_on":[]},
                {"id":"dependent","repository":"repo","worktree":checkout,"head":"dependent","depends_on":["upstream"]}
            ]}}),
        )
        .expect("durable fanout batch");
    }

    fn dependency_receipt(batch_id: &str, key: &str) -> Option<serde_json::Value> {
        let batch = batch::read_batch_record(batch_id).expect("batch record");
        batch.metadata["dependency_action_receipts"][key]
            .as_object()
            .map(|_| batch.metadata["dependency_action_receipts"][key].clone())
    }

    #[test]
    fn fanout_resume_rebases_pushes_updates_pr_and_rearms_downstream_after_interruption() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("fanout tempdir");
            let (checkout, merged, _) = dependency_repo(temp.path(), false);
            let (bin, gh_log) = write_fake_gh(temp.path());
            let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
            let _env = EnvRestore::set(&[
                ("PATH", Some(&path)),
                (
                    "HOMEBOY_FAKE_GH_LOG",
                    Some(gh_log.to_str().expect("gh log path")),
                ),
                ("HOMEBOY_FAKE_GH_FAIL_EDIT", Some("1")),
                (
                    "HOMEBOY_FAKE_UPSTREAM_PR",
                    Some(&format!(
                        r#"{{"state":"MERGED","mergedAt":"2026-01-01T00:00:00Z","mergeCommit":{{"oid":"{merged}"}},"baseRefName":"main"}}"#
                    )),
                ),
                (
                    "HOMEBOY_FAKE_DEPENDENT_PR",
                    Some(
                        r#"{"state":"OPEN","mergedAt":null,"reviewDecision":"APPROVED","mergeCommit":null,"baseRefName":"foundation"}"#,
                    ),
                ),
            ]);
            seed_dependency_batch("fanout-e2e", &checkout);

            // The first resume reaches the real force-with-lease push, then the
            // fake PR adapter interrupts the process at the next durable step.
            reconcile_fanout_pr_states("fanout-e2e", true)
                .expect("push is journaled before PR edit");
            let receipt =
                dependency_receipt("fanout-e2e", &format!("upstream:dependent:{merged}:main"))
                    .expect("receipt exists");
            assert_eq!(receipt["steps"]["push"]["status"], "completed");
            assert_eq!(receipt["blocked_step"], "pull_request_base_update");
            let pushed = command(&checkout, "git", &["rev-parse", "origin/dependent"]);
            assert_eq!(
                command(
                    &checkout,
                    "git",
                    &["merge-base", "--is-ancestor", &merged, &pushed]
                ),
                ""
            );

            std::env::remove_var("HOMEBOY_FAKE_GH_FAIL_EDIT");
            reconcile_fanout_pr_states("fanout-e2e", true).expect("idempotent resume");
            let receipt =
                dependency_receipt("fanout-e2e", &format!("upstream:dependent:{merged}:main"))
                    .expect("receipt exists");
            assert_eq!(receipt["status"], "completed");
            assert_eq!(receipt["steps"]["gates_invalidate"]["status"], "completed");
            assert_eq!(receipt["steps"]["review_invalidate"]["status"], "completed");
            let dependent =
                agent_task_lifecycle::reconcile_status("dependent-run").expect("dependent record");
            assert!(dependent.metadata.get("cook_finalization").is_none());
            assert_eq!(
                dependent.metadata["cook_recovery_source_checkpoint"]["phase"],
                "verification_pending"
            );
            let gh_calls = std::fs::read_to_string(gh_log).expect("gh calls");
            assert_eq!(gh_calls.matches("pr edit dependent --base main").count(), 2);
        });
    }

    #[test]
    fn fanout_resume_rejection_and_rebase_conflict_block_without_later_mutations() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("fanout tempdir");
            let (checkout, merged, before) = dependency_repo(temp.path(), true);
            let (bin, gh_log) = write_fake_gh(temp.path());
            let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
            let _env = EnvRestore::set(&[
                ("PATH", Some(&path)),
                (
                    "HOMEBOY_FAKE_GH_LOG",
                    Some(gh_log.to_str().expect("gh log path")),
                ),
                (
                    "HOMEBOY_FAKE_UPSTREAM_PR",
                    Some(
                        r#"{"state":"CLOSED","mergedAt":null,"mergeCommit":null,"baseRefName":"main"}"#,
                    ),
                ),
                (
                    "HOMEBOY_FAKE_DEPENDENT_PR",
                    Some(
                        r#"{"state":"OPEN","mergedAt":null,"mergeCommit":null,"baseRefName":"foundation"}"#,
                    ),
                ),
            ]);
            seed_dependency_batch("fanout-blocked", &checkout);

            reconcile_fanout_pr_states("fanout-blocked", true).expect("rejection observation");
            assert!(dependency_receipt("fanout-blocked", "upstream:dependent:missing").is_none());
            assert_eq!(
                command(&checkout, "git", &["rev-parse", "origin/dependent"]),
                before
            );

            std::env::set_var(
                "HOMEBOY_FAKE_UPSTREAM_PR",
                format!(
                    r#"{{"state":"MERGED","mergedAt":"2026-01-01T00:00:00Z","mergeCommit":{{"oid":"{merged}"}},"baseRefName":"main"}}"#
                ),
            );
            reconcile_fanout_pr_states("fanout-blocked", true)
                .expect("conflict becomes durable block");
            let receipt = dependency_receipt(
                "fanout-blocked",
                &format!("upstream:dependent:{merged}:main"),
            )
            .expect("conflict receipt");
            assert_eq!(receipt["blocked_step"], "rebase");
            assert_eq!(receipt["steps"]["push"], serde_json::Value::Null);
            assert_eq!(
                command(&checkout, "git", &["rev-parse", "origin/dependent"]),
                before
            );
            assert!(!std::fs::read_to_string(gh_log)
                .expect("gh calls")
                .contains("pr edit dependent"));
        });
    }

    #[test]
    fn fanout_stack_rebases_review_ready_heads_then_moves_to_target_after_merge() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("fanout tempdir");
            let (checkout, _, _) = dependency_repo(temp.path(), false);
            let (bin, gh_log) = write_fake_gh(temp.path());
            let path = format!("{}:{}", bin.display(), std::env::var("PATH").expect("PATH"));
            let _env = EnvRestore::set(&[
                ("PATH", Some(&path)),
                (
                    "HOMEBOY_FAKE_GH_LOG",
                    Some(gh_log.to_str().expect("gh log path")),
                ),
                (
                    "HOMEBOY_FAKE_DEPENDENT_PR",
                    Some(
                        r#"{"state":"OPEN","mergedAt":null,"reviewDecision":"APPROVED","mergeCommit":null,"baseRefName":"foundation"}"#,
                    ),
                ),
            ]);
            seed_dependency_batch("fanout-stack", &checkout);

            // The first accepted candidate releases the dependent against its
            // branch, and the receipt binds the exact upstream head used.
            let first = command(&checkout, "git", &["rev-parse", "origin/foundation"]);
            std::env::set_var(
                "HOMEBOY_FAKE_UPSTREAM_PR",
                format!(
                    r#"{{"state":"OPEN","mergedAt":null,"reviewDecision":"APPROVED","mergeCommit":null,"baseRefName":"main","headRefName":"foundation","headRefOid":"{first}"}}"#
                ),
            );
            reconcile_fanout_pr_states("fanout-stack", true).expect("first stack transition");
            let first_receipt = dependency_receipt(
                "fanout-stack",
                &format!("upstream:dependent:{first}:foundation"),
            )
            .expect("first candidate receipt");
            assert_eq!(first_receipt["action"]["upstream_revision"], first);
            assert_eq!(first_receipt["action"]["target_base"], "foundation");

            // A new upstream head invalidates the dependent's prior review and
            // produces a second rebase/reverification receipt rather than
            // trusting the review of the old candidate.
            command(&checkout, "git", &["checkout", "foundation"]);
            std::fs::write(checkout.join("foundation-update.txt"), "updated\n").expect("update");
            command(&checkout, "git", &["add", "."]);
            command(&checkout, "git", &["commit", "-m", "update foundation"]);
            command(&checkout, "git", &["push", "origin", "foundation"]);
            let second = command(&checkout, "git", &["rev-parse", "HEAD"]);
            std::env::set_var(
                "HOMEBOY_FAKE_UPSTREAM_PR",
                format!(
                    r#"{{"state":"OPEN","mergedAt":null,"reviewDecision":"APPROVED","mergeCommit":null,"baseRefName":"main","headRefName":"foundation","headRefOid":"{second}"}}"#
                ),
            );
            reconcile_fanout_pr_states("fanout-stack", true).expect("updated stack transition");
            let second_receipt = dependency_receipt(
                "fanout-stack",
                &format!("upstream:dependent:{second}:foundation"),
            )
            .expect("updated candidate receipt");
            assert_eq!(second_receipt["status"], "completed");
            assert_eq!(
                second_receipt["steps"]["gates_invalidate"]["status"],
                "completed"
            );
            assert_eq!(
                second_receipt["steps"]["review_invalidate"]["status"],
                "completed"
            );

            command(&checkout, "git", &["checkout", "main"]);
            command(
                &checkout,
                "git",
                &[
                    "merge",
                    "--no-ff",
                    "foundation",
                    "-m",
                    "merge updated foundation",
                ],
            );
            let merged = command(&checkout, "git", &["rev-parse", "HEAD"]);
            command(&checkout, "git", &["push", "origin", "main"]);
            std::env::set_var(
                "HOMEBOY_FAKE_UPSTREAM_PR",
                format!(
                    r#"{{"state":"MERGED","mergedAt":"2026-01-01T00:00:00Z","mergeCommit":{{"oid":"{merged}"}},"baseRefName":"main","headRefName":"foundation","headRefOid":"{second}"}}"#
                ),
            );
            reconcile_fanout_pr_states("fanout-stack", true).expect("merge transition");
            let merge_receipt =
                dependency_receipt("fanout-stack", &format!("upstream:dependent:{merged}:main"))
                    .expect("merge receipt");
            assert_eq!(merge_receipt["action"]["target_base"], "main");
            assert_eq!(merge_receipt["steps"]["rebase"]["status"], "completed");
            assert!(std::fs::read_to_string(gh_log)
                .expect("gh calls")
                .contains("pr edit dependent --base main"));
        });
    }

    #[test]
    fn invalid_dependency_graph_does_not_mutate_a_real_repository() {
        with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("fanout tempdir");
            let (checkout, _, before) = dependency_repo(temp.path(), false);
            let error = BatchCookFanoutPlan::from_value(
                json!({
                    "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                    "fanout_id": "invalid",
                    "cooks": [
                        {"cook_id":"upstream","repo":"repo","prompt":"upstream","workspace":checkout,"to_worktree":"upstream","head":"upstream","verify":["true"]},
                        {"cook_id":"dependent","repo":"repo","depends_on":["missing"],"prompt":"dependent","workspace":checkout,"to_worktree":"dependent","head":"dependent","verify":["true"]}
                    ]
                }),
                &args(),
            )
            .expect_err("missing edge must fail validation");
            assert!(error.message.contains("missing"));
            assert_eq!(
                command(&checkout, "git", &["rev-parse", "origin/dependent"]),
                before
            );
            assert_eq!(command(&checkout, "git", &["status", "--porcelain"]), "");
        });
    }

    #[test]
    fn resolve_ai_tool_disclosure_preserves_an_explicit_operator_value() {
        // An operator-supplied disclosure is preserved verbatim regardless of
        // the selected model. (#8404)
        let resolved = resolve_ai_tool_disclosure(
            "Custom Tool (v2)",
            Some("opencode"),
            None,
            Some("openai/gpt-5.6-terra"),
        );
        assert_eq!(resolved, "Custom Tool (v2)");
    }

    #[test]
    fn pr_observation_distinguishes_merge_rejection_and_revision() {
        let observation = |state: &str, merged_at: Option<&str>, review_decision: Option<&str>| {
            FanoutPrObservation {
                state: state.to_string(),
                merged_at: merged_at.map(str::to_string),
                review_decision: review_decision.map(str::to_string),
                merge_state_status: None,
                merge_commit: None,
                base_ref_name: None,
                head_ref_oid: None,
                head_ref_name: None,
            }
        };

        assert_eq!(
            observation("CLOSED", Some("2026-07-30T00:00:00Z"), None).verdict(),
            "merged"
        );
        assert_eq!(observation("CLOSED", None, None).verdict(), "rejected");
        assert_eq!(
            observation("OPEN", None, Some("CHANGES_REQUESTED")).verdict(),
            "revision_requested"
        );
    }

    #[test]
    fn resolve_ai_tool_disclosure_keeps_generic_default_for_an_unknown_backend() {
        // With no explicit override and a backend the catalog cannot resolve,
        // the generic default is preserved (nothing to derive from). Using an
        // explicit unknown backend keeps this deterministic regardless of the
        // providers installed in the test environment.
        let resolved = resolve_ai_tool_disclosure(
            GENERIC_AI_DISCLOSURE,
            Some("no-such-backend-xyz"),
            None,
            Some("openai/gpt-5.6-terra"),
        );
        assert_eq!(resolved, GENERIC_AI_DISCLOSURE);
    }

    #[test]
    fn compile_batch_cooks_delivers_controller_context_to_every_cell() {
        // HomeGuard already holds env_lock; a second env_lock() around
        // with_isolated_home deadlocks. EnvRestore composes with that guard.
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let _env = EnvRestore::set(&[
                ("HOMEBOY_RUNNER_HOSTED_EXEC", None),
                ("HOMEBOY_SOURCE_SNAPSHOT_JSON", None),
                ("HOMEBOY_LAB_OFFLOAD_JSON", None),
            ]);
            let plan = test_batch_plan();
            let cooks = compile_batch_cooks(&plan, |_| {}).expect("compile batch cooks");

            assert_eq!(cooks.len(), 2);
            assert!(cooks
                .iter()
                .all(|cook| cook.identity.initial_plan.tasks.len() == 1));
            assert!(cooks
                .iter()
                .all(|cook| format!("{:?}", cook.harvest_context)
                    == "HarvestExecutionContext { source_snapshot: None, lab_offload: None }"));
        });
    }

    fn args() -> AgentTaskFanoutInputArgs {
        AgentTaskFanoutInputArgs {
            input: "inline".to_string(),
            fanout_id: Some("fanout/refactor".to_string()),
            backend: Some("test".to_string()),
            selector: Some("fixture".to_string()),
            model: None,
        }
    }

    #[test]
    fn batch_cook_plan_requires_independent_cooks_with_worktrees() {
        with_isolated_home(|_| {
            let plan = BatchCookFanoutPlan::from_value(
                json!({
                    "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                    "fanout_id": "fanout/original",
                    "cooks": [
                        {
                            "cook_id": "5929-docs",
                            "prompt": "fix docs",
                            "repo": "homeboy",
                            "cwd": "/runner/workspaces/homeboy@5929-docs",
                            "workspace_materialization": [{
                                "field": "cwd",
                                "controller_path": "/Users/user/Developer/homeboy@5929-docs",
                                "runner_path": "/runner/workspaces/homeboy@5929-docs",
                                "branch": "fix/5929-docs",
                                "ref": "fix/5929-docs",
                                "sync_status": "materialized"
                            }],
                            "to_worktree": "homeboy@fix-5929-docs",
                            "head": "fix/5929-docs",
                            "verify": ["homeboy review test homeboy"]
                        },
                        {
                            "cook_id": "5929-cli",
                            "prompt": "fix cli",
                            "repo": "homeboy",
                            "to_worktree": "homeboy@fix-5929-cli",
                            "head": "fix/5929-cli",
                            "verify": ["homeboy review test homeboy"]
                        }
                    ]
                }),
                &args(),
            )
            .expect("batch cook fanout plan");

            assert_eq!(plan.fanout_id, "fanout/refactor");
            assert_eq!(plan.cooks.len(), 2);
            assert_eq!(plan.cooks[0].backend.as_deref(), Some("test"));
            assert_eq!(plan.cooks[0].selector.as_deref(), Some("fixture"));
            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(
                invocation.options.workspace.to_worktree,
                "homeboy@fix-5929-docs"
            );
            assert_eq!(
                invocation.options.finalization.head.as_deref(),
                Some("fix/5929-docs")
            );
            assert_eq!(
                invocation.dispatch.cwd.as_deref(),
                Some("/runner/workspaces/homeboy@5929-docs")
            );
            assert_eq!(invocation.dispatch.workspace, None);
            let run_id = invocation
                .dispatch
                .run_id
                .as_deref()
                .expect("attempt run id");
            assert!(run_id.starts_with("cook-fanout_refactor-5929-docs-attempt-1-"));
            assert_eq!(
                run_id.len(),
                "cook-fanout_refactor-5929-docs-attempt-1-".len() + 8
            );
            assert_eq!(
                invocation.options.gates.verify,
                vec!["homeboy review test homeboy"]
            );
            assert!(invocation
                .dispatch
                .core
                .client_context
                .as_deref()
                .expect("client context")
                .contains("batch_cook"));
            assert!(invocation
                .dispatch
                .core
                .client_context
                .as_deref()
                .expect("client context")
                .contains("/Users/user/Developer/homeboy@5929-docs"));
        });
    }

    #[test]
    fn batch_plan_persists_a_two_level_stack_and_schedules_ready_siblings() {
        let plan = BatchCookFanoutPlan::from_value(
            json!({
                "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                "fanout_id": "stack",
                "cooks": [
                    {"cook_id": "foundation", "task_url": "https://github.com/Extra-Chill/homeboy/issues/1", "repo": "homeboy", "prompt": "foundation", "to_worktree": "homeboy@foundation", "head": "fix/foundation", "verify": ["true"]},
                    {"cook_id": "sibling", "task_url": "https://github.com/Extra-Chill/homeboy/issues/2", "repo": "homeboy", "prompt": "sibling", "to_worktree": "homeboy@sibling", "head": "fix/sibling", "verify": ["true"]},
                    {"cook_id": "dependent", "task_url": "https://github.com/Extra-Chill/homeboy/issues/3", "repo": "homeboy", "depends_on": ["https://github.com/Extra-Chill/homeboy/issues/1"], "prompt": "dependent", "to_worktree": "homeboy@dependent", "head": "fix/dependent", "verify": ["true"]}
                ]
            }),
            &args(),
        )
        .expect("stack plan");
        assert_eq!(plan.cooks[2].base, "fix/foundation");
        assert_eq!(
            plan.ready_plan()
                .expect("ready frontier")
                .cooks
                .iter()
                .map(|cook| cook.cook_id.as_str())
                .collect::<Vec<_>>(),
            vec!["fanout/refactor-foundation", "fanout/refactor-sibling"]
        );
        let graph = plan.dependency_graph_metadata().expect("durable graph");
        assert_eq!(
            graph["readiness"]["states"]["fanout/refactor-dependent"],
            "blocked_by_dependency"
        );
    }

    #[test]
    fn batch_plan_rejects_missing_and_cross_repository_dependencies_before_mutation() {
        let missing = BatchCookFanoutPlan::from_value(
            json!({"schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA, "fanout_id": "missing", "cooks": [{"cook_id": "child", "depends_on": ["absent"], "prompt": "x", "to_worktree": "homeboy@child", "verify": ["true"]}]}),
            &args(),
        )
        .expect_err("missing dependency");
        assert!(missing.message.contains("missing"));
        let cross_repo = BatchCookFanoutPlan::from_value(
            json!({"schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA, "fanout_id": "cross", "cooks": [{"cook_id": "a", "repo": "one", "prompt": "x", "to_worktree": "one@a", "head": "fix/a", "verify": ["true"]}, {"cook_id": "b", "repo": "two", "depends_on": ["a"], "prompt": "x", "to_worktree": "two@b", "verify": ["true"]}]}),
            &args(),
        )
        .expect_err("cross repository dependency");
        assert!(cross_repo.message.contains("cross-repository"));
    }

    #[test]
    fn generic_fanout_inputs_are_rejected_from_public_contract() {
        let error = BatchCookFanoutPlan::from_value(
            json!({
                "schema": "homeboy/agent-task-fanout-plan/v1",
                "fanout_id": "generic",
                "plane": "workflow",
                "tasks": []
            }),
            &args(),
        )
        .expect_err("generic fanout rejected");

        assert!(error
            .to_string()
            .contains("accepts only batch cook plans with independent cooks"));
    }

    fn cook_batch_args() -> AgentTaskFanoutCookBatchArgs {
        AgentTaskFanoutCookBatchArgs {
            issues: vec![
                "https://github.com/Extra-Chill/homeboy/issues/6453".to_string(),
                "https://github.com/Extra-Chill/homeboy/issues/6454".to_string(),
            ],
            repo: "homeboy".to_string(),
            component: None,
            from: Some("origin/main".to_string()),
            base: Some("main".to_string()),
            base_resolution: None,
            branch_prefix: "fix".to_string(),
            fanout_id: Some("issue-wave".to_string()),
            worktrees: Vec::new(),
            prompt_template: None,
            backend: Some("sandbox".to_string()),
            selector: Some("sample.executor-provider".to_string()),
            model: Some("gpt-5.5".to_string()),
            provider_profile: None,
            // No secret is declared: planning and execution share one admission,
            // so a credential the fixture cannot supply would be checked for real.
            secret_env: Vec::new(),
            provider_config: Some(r#"{"runtime":"opencode"}"#.to_string()),
            provider_evidence_inputs: Vec::new(),
            ai_tool: None,
            gates: super::super::args::VerifyGateArgs {
                accept_inherited_failures: false,
                gate_package_artifacts: Vec::new(),
                gate_extension_inputs: Vec::new(),
                verify: vec!["cargo test --lib".to_string()],
                verify_file: Vec::new(),
                private_verify: Vec::new(),
                private_verify_file: Vec::new(),
                input_sources: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                gate_execution_policy: "ordered-fail-fast".to_string(),
                gate_timeout_seconds: 30 * 60,
                gate_heartbeat_interval_seconds: 5,
                gate_no_progress_timeout_seconds: 5 * 60,
                rerun_completed_gates: false,
                gate_environment_mode: "inherit".to_string(),
                gate_environment: Vec::new(),
                gate_environment_preserve: Vec::new(),
                gate_toolchains: Vec::new(),
                gate_toolchain_specs: Vec::new(),
                isolate_gate_home: true,
                isolate_gate_xdg: true,
                gate_shared_cargo_target: false,
                no_gate_shared_cargo_target: false,
            },
            verification_profiles: None,
            max_concurrency: None,
            max_duration: None,
            preview: true,
            dry_run_planner_timeout_seconds: None,
            run_plan: false,
        }
    }

    #[test]
    fn cook_batch_preserves_canonical_repository_and_nested_component_identity() {
        with_isolated_home(|home| {
            let repository = home.path().join("blocks-engine");
            std::fs::create_dir_all(&repository).expect("repository");
            homeboy::core::test_support::run_git_fixture_command(&repository, &["init", "-q"]);
            let component = repository.join("php-transformer");
            std::fs::create_dir_all(&component).expect("nested component");
            let registrations = home.path().join(".config/homeboy/components");
            std::fs::create_dir_all(&registrations).expect("component registrations");
            std::fs::write(
                registrations.join("php-transformer.json"),
                serde_json::json!({
                    "local_path": component,
                    "remote_url": "https://github.com/Automattic/blocks-engine.git",
                    "aliases": ["blocks-engine"]
                })
                .to_string(),
            )
            .expect("component registration");
            let second_component = repository.join("block-parser");
            std::fs::create_dir_all(&second_component).expect("second nested component");
            std::fs::write(
                registrations.join("block-parser.json"),
                serde_json::json!({
                    "local_path": second_component,
                    "remote_url": "https://github.com/Automattic/blocks-engine.git"
                })
                .to_string(),
            )
            .expect("second component registration");

            let mut batch_args = cook_batch_args();
            batch_args.repo = "php-transformer".to_string();
            normalize_cook_batch_repo(&mut batch_args).expect("normalize component to owning repo");
            assert_eq!(batch_args.repo, "blocks-engine");
            assert_eq!(batch_args.component.as_deref(), Some("php-transformer"));
            assert_eq!(
                super::super::run::cook_component_path_for_repository_name(
                    batch_args.component.as_deref().expect("component")
                )
                .expect("resolve component path"),
                Some(component)
            );
            assert_eq!(
                batch_gate_workspace(&batch_args).expect("gate workspace"),
                Some(repository.join("php-transformer"))
            );

            let plan = build_cook_batch_plan(&batch_args).expect("build typed fanout plan");
            assert_eq!(plan.metadata["repo"], "blocks-engine");
            assert_eq!(plan.metadata["component"], "php-transformer");
            assert!(plan
                .cooks
                .iter()
                .all(|cook| cook.repo.as_deref() == Some("blocks-engine")));
            assert!(plan
                .cooks
                .iter()
                .all(|cook| cook.component_id.as_deref() == Some("php-transformer")));
            assert!(plan
                .cooks
                .iter()
                .all(|cook| cook.to_worktree.starts_with("blocks-engine@")));

            let replayed = BatchCookFanoutPlan::from_value(
                serde_json::to_value(&plan).expect("serialize fanout plan"),
                &args(),
            )
            .expect("replay fanout plan");
            let invocation = replayed.cooks[0]
                .to_cook_invocation(&replayed)
                .expect("compile replay invocation");
            assert_eq!(invocation.dispatch.repo.as_deref(), Some("blocks-engine"));
            assert_eq!(
                invocation.dispatch.component.as_deref(),
                Some("php-transformer")
            );
            let replay = cook_batch_argv(&batch_args);
            assert!(replay
                .windows(2)
                .any(|args| args == ["--repo", "blocks-engine"]));
            assert!(replay
                .windows(2)
                .any(|args| args == ["--component", "php-transformer"]));

            let mut tampered = serde_json::to_value(&plan).expect("serialize tampered plan");
            tampered["cooks"][0]["component_id"] = Value::String("block-parser".to_string());
            let error = BatchCookFanoutPlan::from_value(tampered, &args())
                .expect_err("mismatched repository identity must fail closed");
            assert!(error.message.contains("repository identity does not match"));

            let mut partial = serde_json::to_value(&plan).expect("serialize partial plan");
            partial["cooks"][0]["repository_identity"] = Value::Null;
            let error = BatchCookFanoutPlan::from_value(partial, &args())
                .expect_err("partial component identity must fail closed");
            assert!(error
                .message
                .contains("incomplete repository/component identity"));
        });
    }

    #[test]
    fn rendered_cook_batch_commands_preserve_explicit_placement_and_replay_identity() {
        let args = cook_batch_args();
        for (placement, name) in [
            (Placement::Local, "local"),
            (Placement::Lab, "lab"),
            (Placement::LabOrLocal, "lab-or-local"),
        ] {
            let commands = cook_batch_commands_with_placement(&args, placement, false, None);
            let actions = cook_batch_next_actions_with_placement(
                &args,
                placement,
                "issue-wave",
                "ready",
                false,
                false,
                &worktree_output(Vec::new()),
                false,
                None,
            );
            let rendered = [
                commands["plan"].as_str().expect("plan command"),
                commands["run"].as_str().expect("run command"),
                actions[0].command.as_str(),
                actions[1].command.as_str(),
            ];

            for (index, command) in rendered.into_iter().enumerate() {
                let cli = Cli::try_parse_from(shlex::split(command).expect("shell command parses"))
                    .expect("rendered command parses as CLI");
                assert_eq!(cli.placement, placement, "{name}: {command}");
                let Commands::AgentTask(agent_task) = cli.command else {
                    panic!("{name}: agent-task command")
                };
                let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
                    panic!("{name}: fanout command")
                };
                let AgentTaskFanoutCommand::CookBatch(replayed) = fanout.command else {
                    panic!("{name}: cook-batch command")
                };
                assert_eq!(replayed.fanout_id.as_deref(), Some("issue-wave"));
                assert_eq!(replayed.run_plan, index == 1 || index == 3);
            }
        }
    }

    fn placement_fixture(
        requested: Placement,
        selected: EffectiveExecutionPlacement,
    ) -> PlacementDirective {
        let runner = (selected == EffectiveExecutionPlacement::Lab).then(|| {
            homeboy_lab_runner_contract::ExecutionPlacementRunnerSelection {
                runner_id: "test-lab".to_string(),
                source: homeboy_lab_runner_contract::RunnerSelectionSource::Policy,
            }
        });
        PlacementDirective {
            requested,
            required: if requested == Placement::Lab {
                ExecutionPlacementRequirement::Lab
            } else {
                ExecutionPlacementRequirement::Either
            },
            selected,
            runner,
            fallback: ExecutionPlacementFallback {
                local_allowed: requested == Placement::Auto,
                reason: None,
            },
            override_authorization: ExecutionPlacementOverrideAuthorization {
                authorized: requested == Placement::Local,
                authority: (requested == Placement::Local)
                    .then(|| "operator --placement local".to_string()),
            },
        }
    }

    fn materialize_test_child(options: &mut CookRequest) {
        options.identity.initial_plan.tasks = vec![serde_json::from_value(serde_json::json!({
            "task_id": options.identity.cook_id,
            "executor": { "backend": "test" },
            "instructions": "test fanout placement",
            "workspace": { "root": env!("CARGO_MANIFEST_DIR") },
        }))
        .expect("materialized test task")];
        options.identity.initial_plan.rebuild_homeboy_plan();
    }

    #[test]
    fn placement_round_trips_through_plan_replay_recipe_lifecycle_and_status() {
        with_isolated_home(|_| {
            for (requested, selected, name, authority) in [
                (
                    Placement::Local,
                    EffectiveExecutionPlacement::Local,
                    "local",
                    "operator_overridable",
                ),
                (
                    Placement::Lab,
                    EffectiveExecutionPlacement::Lab,
                    "lab",
                    "policy_pinned",
                ),
                (
                    Placement::Auto,
                    EffectiveExecutionPlacement::Lab,
                    "automatic",
                    "policy_pinned",
                ),
            ] {
                let mut plan = test_batch_plan();
                plan.cooks.truncate(1);
                plan.rekey(format!("placement-round-trip-{name}"));
                plan.ensure_placement(placement_fixture(requested, selected))
                    .expect("bind plan placement");
                let encoded = serde_json::to_value(&plan).expect("serialize placement plan");
                let decoded: BatchCookFanoutPlan =
                    serde_json::from_value(encoded).expect("deserialize placement plan");
                assert_eq!(decoded.placement, plan.placement, "{name}: plan policy");
                let preflight = fanout_placement_preflight(decoded.placement.as_ref());
                assert_eq!(preflight["requested"], serde_json::json!(requested));
                assert_eq!(preflight["selected"], serde_json::json!(selected));
                assert_eq!(
                    preflight["admission"]["state"],
                    if selected == EffectiveExecutionPlacement::Lab {
                        "deferred"
                    } else {
                        "confirmed"
                    }
                );

                let replay = cook_batch_run_command_with_placement(&cook_batch_args(), requested);
                let cli = Cli::try_parse_from(shlex::split(&replay).expect("split replay"))
                    .expect("parse replay");
                assert_eq!(cli.placement, requested, "{name}: replay policy");

                let mut options = decoded.cooks[0]
                    .to_cook_invocation(&decoded)
                    .expect("compile child invocation")
                    .options;
                materialize_test_child(&mut options);
                attach_fanout_placement_decision(&decoded, &mut options)
                    .expect("bind child placement");
                if selected == EffectiveExecutionPlacement::Lab {
                    options.provider_transport.attempt_dispatcher =
                        Some(Arc::new(LabRecipeDispatcher));
                }
                enforce_fanout_placement(&options).expect("admit child placement");
                let decision: homeboy_lab_runner_contract::ExecutionPlacementDecision =
                    serde_json::from_value(
                        options.identity.initial_plan.metadata["execution_placement_decision"]
                            .clone(),
                    )
                    .expect("canonical child decision");
                assert_eq!(decision.requested, requested, "{name}: requested");
                assert_eq!(decision.selected, selected, "{name}: selected");

                agent_task_service::persist_initial_recipe(&options)
                    .expect("persist placement-bound recipe");
                let recipe = agent_task_service::load_recipe(&options.identity.cook_id)
                    .expect("load placement-bound recipe");
                assert_eq!(
                    recipe.attempts[0].plan.metadata["execution_placement_decision"]["decision_id"],
                    decision.decision_id,
                    "{name}: recipe decision"
                );

                let run_id = format!("placement-round-trip-{name}-run");
                agent_task_lifecycle::submit_plan(&options.identity.initial_plan, Some(&run_id))
                    .expect("submit durable child");
                let outcome = decision
                    .outcome(
                        selected,
                        (selected == EffectiveExecutionPlacement::Lab)
                            .then(|| "test-lab".to_string()),
                    )
                    .expect("verified placement outcome");
                agent_task_lifecycle::record_execution_placement_outcome(&run_id, outcome)
                    .expect("record placement outcome");
                batch::persist_fanout_run_batch(
                    &decoded.fanout_id,
                    &decoded.fanout_id,
                    &[batch::FanoutRunBatchChild {
                        task_id: decoded.cooks[0].cook_id.clone(),
                        run_id: run_id.clone(),
                    }],
                    serde_json::json!({ "placement": decoded.placement }),
                )
                .expect("persist placement batch");
                let status = batch::status(&decoded.fanout_id).expect("fanout placement status");
                let placement = status.batch.child_runs[0]
                    .placement
                    .as_ref()
                    .expect("child placement projection");
                assert_eq!(placement.requested, requested, "{name}: status requested");
                assert_eq!(placement.selected, selected, "{name}: status selected");
                assert_eq!(
                    placement.effective,
                    Some(selected),
                    "{name}: status effective"
                );
                assert_eq!(placement.authority, authority, "{name}: status authority");
                assert_eq!(placement.decision_id, decision.decision_id);
                assert_eq!(placement.outcome_decision_id, Some(decision.decision_id));

                let claim_id = batch::claim_fanout_run_batch(&decoded.fanout_id)
                    .expect("claim placement batch")
                    .expect("placement batch claim id");
                batch::record_fanout_run_batch_failure(
                    &decoded.fanout_id,
                    &claim_id,
                    "test",
                    serde_json::json!({ "message": "terminal coordinator fixture" }),
                )
                .expect("record terminal coordinator fixture");
                assert!(batch::status(&decoded.fanout_id)
                    .expect("failed fanout placement status")
                    .batch
                    .child_runs[0]
                    .placement
                    .is_some());
            }
        });
    }

    #[test]
    fn explicit_lab_plan_fails_closed_without_a_child_dispatcher() {
        let mut plan = test_batch_plan();
        plan.ensure_placement(placement_fixture(
            Placement::Lab,
            EffectiveExecutionPlacement::Lab,
        ))
        .expect("bind plan placement");
        let replay_error = plan
            .ensure_placement(placement_fixture(
                Placement::Auto,
                EffectiveExecutionPlacement::Local,
            ))
            .expect_err("omitting explicit Lab on replay must fail closed");
        assert!(replay_error.message.contains("conflicts"));
        let mut options = plan.cooks[0]
            .to_cook_invocation(&plan)
            .expect("compile child invocation")
            .options;
        materialize_test_child(&mut options);
        attach_fanout_placement_decision(&plan, &mut options).expect("bind child placement");
        let error = enforce_fanout_placement(&options)
            .expect_err("required Lab must not compile onto the local executor");
        assert!(error.message.contains("no child attempt dispatcher"));
    }

    #[test]
    fn correction_command_preserves_explicit_placement() {
        let mut args = cook_batch_args();
        args.repo = "homeboy@invalid".to_string();
        let error = invalid_cook_batch_repo(&args, vec!["homeboy".to_string()], Placement::Lab);
        let command = error.details["correction_command"]
            .as_str()
            .expect("correction command");
        let cli = Cli::try_parse_from(shlex::split(command).expect("shell command parses"))
            .expect("correction command parses as CLI");
        assert_eq!(cli.placement, Placement::Lab);
    }

    #[test]
    fn private_replay_command_preserves_explicit_placement() {
        let command = private_artifact_run_command_with_placement(
            Path::new("/tmp/private-batch-plan.json"),
            Placement::Lab,
        );
        let cli = Cli::try_parse_from(shlex::split(&command).expect("shell command parses"))
            .expect("private replay command parses as CLI");
        assert_eq!(cli.placement, Placement::Lab);
    }

    #[test]
    fn persisted_replays_and_durable_actions_preserve_explicit_placement() {
        with_isolated_home(|_| {
            for (placement, name) in [
                (Placement::Local, "local"),
                (Placement::Lab, "lab"),
                (Placement::LabOrLocal, "lab-or-local"),
            ] {
                let mut plan = test_batch_plan();
                plan.fanout_id = format!("placement-{name}");
                persist_fanout_run_batch_record(&plan, placement).expect("persist batch record");
                let record = batch::read_batch_record(&plan.fanout_id).expect("read batch record");
                let replan = record.metadata["replan_command"]
                    .as_str()
                    .expect("persisted replan command");
                let actions = cook_batch_next_actions_with_placement(
                    &cook_batch_args(),
                    placement,
                    &plan.fanout_id,
                    "partial_failure",
                    true,
                    true,
                    &worktree_output(Vec::new()),
                    false,
                    None,
                );
                let batch_commands = batch_commands(&plan.fanout_id, placement);
                let commands = vec![
                    replan,
                    actions[0].command.as_str(),
                    actions[1].command.as_str(),
                    actions[2].command.as_str(),
                    batch_commands["status"].as_str().expect("status command"),
                    batch_commands["artifacts"]
                        .as_str()
                        .expect("artifacts command"),
                    batch_commands["run_next"]
                        .as_str()
                        .expect("run-next command"),
                ];
                for command in commands {
                    let cli =
                        Cli::try_parse_from(shlex::split(command).expect("shell command parses"))
                            .expect("rendered command parses as CLI");
                    assert_eq!(cli.placement, placement, "{name}: {command}");
                }
            }
        });
    }

    /// Install the executor providers the fanout fixtures dispatch against.
    ///
    /// Provider selection resolves a `--backend`/`--selector` pair against the
    /// installed catalog, which core discovers from standalone agent-runtime
    /// manifests under the config root. A hermetic home has none, so every case
    /// that dispatches fails with "is not dispatchable: the requested
    /// backend/selector route did not resolve" before reaching what it asserts.
    ///
    /// Both fixture argument builders are covered, and their selectors are
    /// distinct provider ids, so one home can serve both without colliding.
    fn install_fanout_agent_task_providers(home: &Path) {
        for (provider_id, backend) in [("sample.executor-provider", "sandbox"), ("fixture", "test")]
        {
            let runtime_id = format!("{backend}-runtime");
            let runtime_dir = home
                .join(".config/homeboy/agent-runtimes")
                .join(&runtime_id);
            std::fs::create_dir_all(&runtime_dir).expect("agent runtime directory");
            std::fs::write(
                runtime_dir.join(format!("{runtime_id}.json")),
                serde_json::json!({
                    "schema": "homeboy/agent-runtime-manifest/v1",
                    "id": runtime_id,
                    "runtime_path": runtime_dir,
                    "agent_task_executors": [{
                        "schema": "homeboy/agent-task-executor-provider/v1",
                        "id": provider_id,
                        "backend": backend,
                        "invocation": { "argv": ["node", "{{runtime_path}}/runner.cjs"] },
                        "request_schema": "homeboy/agent-task-request/v1",
                        "outcome_schema": "homeboy/agent-task-outcome/v1"
                    }]
                })
                .to_string(),
            )
            .expect("agent runtime manifest");
        }
        // The catalog is discovered once per process and cached, and that cache
        // is not keyed by config root. Without an explicit refresh the first
        // hermetic home to look wins for the whole binary, so a fixture that
        // installs a provider still sees the empty catalog of whichever test
        // ran first. Re-discover against the home that just changed.
        homeboy::agents::agent_tasks::provider::AgentTaskProviderCatalog::refresh();
    }

    fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
        write_component_registration_with_identity(home, id, local_path, None);
    }

    fn write_component_registration_with_identity(
        home: &Path,
        id: &str,
        local_path: &Path,
        remote_url: Option<&str>,
    ) {
        let components = home.join(".config/homeboy/components");
        std::fs::create_dir_all(&components).expect("components directory");
        std::fs::write(
            components.join(format!("{id}.json")),
            serde_json::json!({
                "local_path": local_path,
                "remote_url": remote_url,
            })
            .to_string(),
        )
        .expect("component registration");
    }

    /// Make a registered primary answer default-branch resolution.
    ///
    /// `cook_batch_args` plans from `origin/main`, and repository planning now
    /// resolves that against the registered primary before anything else runs.
    /// A bare directory has no branch and no remote, so every case built on one
    /// fails in repository resolution before reaching the behavior it asserts.
    /// The remote ref is published locally because a fixture must not need a
    /// network to resolve its own source ref.
    ///
    /// This is deliberately not folded into `write_component_registration`: the
    /// large-registry case registers hundreds of unrelated components while
    /// measuring elapsed time, and only the planning target needs a repository.
    fn init_git_primary(path: &Path) {
        std::fs::create_dir_all(path).expect("primary fixture directory");
        for args in [
            ["init", "-b", "main"].as_slice(),
            ["config", "user.email", "test@example.com"].as_slice(),
            ["config", "user.name", "Homeboy Test"].as_slice(),
            ["commit", "--allow-empty", "-m", "initial"].as_slice(),
            ["update-ref", "refs/remotes/origin/main", "HEAD"].as_slice(),
        ] {
            assert!(
                Command::new("git")
                    .args(args)
                    .current_dir(path)
                    .status()
                    .expect("initialize primary fixture")
                    .success(),
                "git {args:?} failed"
            );
        }
    }

    #[test]
    fn fanout_dry_run_bounds_gate_workspace_lookup_in_a_large_registry() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let target = home.path().join("target");
            std::fs::create_dir(&target).expect("target workspace");
            std::fs::write(target.join("homeboy.json"), r#"{"id":"fixture"}"#)
                .expect("target manifest");
            init_git_primary(&target);
            write_component_registration(home.path(), "fixture", &target);

            for index in 0..300 {
                let id = format!("unrelated-{index}");
                let workspace = home.path().join(&id);
                std::fs::create_dir(&workspace).expect("unrelated workspace");
                std::fs::write(
                    workspace.join("homeboy.json"),
                    serde_json::json!({ "id": id }).to_string(),
                )
                .expect("unrelated manifest");
                write_component_registration(home.path(), &id, &workspace);
            }

            let mut args = cook_batch_args();
            args.repo = "fixture".to_string();
            args.issues = (1..=4)
                .map(|issue| format!("https://github.com/Extra-Chill/homeboy/issues/{issue}"))
                .collect();
            let started = Instant::now();
            let (value, exit_code) = cook_batch(args).expect("bounded four-child dry run");
            let rows = value["worktrees"]["rows"]
                .as_array()
                .expect("worktree rows");
            let planned_handles = value["plan"]["cooks"]
                .as_array()
                .expect("planned cooks")
                .iter()
                .map(|cook| cook["to_worktree"].as_str().expect("planned handle"))
                .collect::<BTreeSet<_>>();
            let projected_handles = rows
                .iter()
                .map(|row| row["handle"].as_str().expect("projected handle"))
                .collect::<BTreeSet<_>>();

            assert_eq!(exit_code, 0, "{value}");
            assert!(
                started.elapsed() < DRY_RUN_PHASE_TIMEOUT,
                "targeted registry lookup exceeded the normal planner deadline"
            );
            assert_eq!(rows.len(), 4);
            assert_eq!(projected_handles, planned_handles);
        });
    }

    #[test]
    fn cook_batch_repo_normalization_accepts_slugs_and_registered_primary_paths() {
        with_isolated_home(|home| {
            let primary = home.path().join("primary");
            std::fs::create_dir(&primary).expect("primary directory");
            init_git_primary(&primary);
            write_component_registration(home.path(), "fixture", &primary);

            let mut slug = cook_batch_args();
            slug.repo = "fixture".to_string();
            normalize_cook_batch_repo(&mut slug).expect("slug remains accepted");
            assert_eq!(slug.repo, "fixture");

            let mut path = cook_batch_args();
            path.repo = primary.to_string_lossy().to_string();
            let bin = home.path().join("bin");
            std::fs::create_dir(&bin).expect("fake Git bin");
            let fake_git = bin.join("git");
            let git_invoked = home.path().join("git-invoked");
            std::fs::write(
                &fake_git,
                format!("#!/bin/sh\n: > '{}'\nsleep 5\n", git_invoked.display()),
            )
            .expect("sleeping Git");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755))
                    .expect("executable fake Git");
            }
            let previous_path = std::env::var_os("PATH");
            let mut search_path = std::ffi::OsString::from(bin.as_os_str());
            search_path.push(":");
            search_path.push(previous_path.unwrap_or_default());
            let _path = homeboy::core::test_support::EnvVarGuard::set("PATH", search_path);

            let started = Instant::now();
            normalize_cook_batch_repo(&mut path).expect("primary path resolves");
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "exact primary identity must resolve before a planner deadline"
            );
            assert_eq!(path.repo, "fixture");
            assert!(!git_invoked.exists(), "identity normalization invoked Git");
        });
    }

    #[test]
    fn cook_batch_repo_normalization_rejects_unknown_slug_with_immediate_candidates() {
        with_isolated_home(|home| {
            for index in (0..64).rev() {
                let id = format!("candidate-{index:02}");
                let primary = home.path().join(&id);
                write_component_registration(home.path(), &id, &primary);
            }
            let mut args = cook_batch_args();
            args.repo = "unknown-repository".to_string();

            let started = Instant::now();
            let error = normalize_cook_batch_repo(&mut args).expect_err("unknown slug");

            assert!(started.elapsed() < Duration::from_secs(2));
            assert_eq!(
                error.details["identity_classification"],
                "unregistered_repository"
            );
            let candidates = error.details["component_candidates"]
                .as_array()
                .expect("component candidates");
            assert_eq!(candidates.len(), 64);
            assert_eq!(candidates[0], "candidate-00");
            assert_eq!(candidates[63], "candidate-63");
        });
    }

    #[cfg(unix)]
    #[test]
    fn fanout_keeps_component_identity_but_provisions_its_repository() {
        use std::os::unix::fs::PermissionsExt;

        with_isolated_home(|home| {
            let primary = home.path().join("blocks-engine");
            let component = primary.join("php-transformer");
            std::fs::create_dir_all(&component).expect("nested component directory");
            init_git_primary(&primary);
            write_component_registration_with_identity(
                home.path(),
                "php-transformer",
                &component,
                Some("https://github.com/example/blocks-engine.git"),
            );

            let mut args = cook_batch_args();
            args.repo = "php-transformer".to_string();
            normalize_cook_batch_repo(&mut args).expect("component identity resolves");
            assert_eq!(args.repo, "blocks-engine");
            assert_eq!(args.component.as_deref(), Some("php-transformer"));
            assert_eq!(
                cook_batch_provision_repository(&args.repo, true).expect("provider repository"),
                "blocks-engine"
            );

            let provider = home.path().join("dmc-worktree-provider");
            std::fs::write(&provider, "#!/bin/sh\nexit 1\n").expect("provider fixture");
            let mut permissions = std::fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).expect("provider executable");
            let mut config = homeboy::core::defaults::HomeboyConfig::default();
            config.worktree_providers.insert(
                "dmc".to_string(),
                homeboy::core::defaults::WorktreeProviderConfig {
                    enabled: true,
                    kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                    apply_enabled: true,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: homeboy::core::defaults::WorktreeProviderCommands {
                        resolve: Some(vec![
                            provider.display().to_string(),
                            "resolve".to_string(),
                            "{handle}".to_string(),
                        ]),
                        resolve_not_found_exit_codes: vec![1],
                        ensure: Some(vec![
                            provider.display().to_string(),
                            "ensure".to_string(),
                            "{repo}".to_string(),
                            "{handle}".to_string(),
                            "{base}".to_string(),
                            "{head}".to_string(),
                            "{task_url}".to_string(),
                            "{idempotency_key}".to_string(),
                            "{purpose}".to_string(),
                            "{owner_run_ref}".to_string(),
                            "{cleanup_policy}".to_string(),
                        ]),
                        ..Default::default()
                    },
                    list_result_mapping: Some(
                        homeboy::core::defaults::WorktreeProviderListResultMapping {
                            items: "$.worktrees".to_string(),
                            handle: "$.handle".to_string(),
                            path: "$.path".to_string(),
                            branch: "$.branch".to_string(),
                            dirty: "$.safety.dirty".to_string(),
                            unpushed: "$.safety.unpushed".to_string(),
                            primary: "$.safety.primary".to_string(),
                            task_url: None,
                        },
                    ),
                },
            );
            homeboy::core::defaults::save_config(&config).expect("save provider config");

            let plan = build_cook_batch_plan(&args).expect("fanout plan");
            assert!(plan.cooks.iter().all(|cook| {
                cook.repo.as_deref() == Some("blocks-engine")
                    && cook.component_id.as_deref() == Some("php-transformer")
                    && cook.to_worktree.starts_with("blocks-engine@")
            }));
            args.preview = false;
            let worktrees = queue_or_reuse_worktrees(&args, &plan).expect("provider worktrees");
            assert_eq!(worktrees.repo, "blocks-engine");
            assert!(worktrees
                .rows
                .iter()
                .zip(&plan.cooks)
                .all(|(row, cook)| { row.handle == cook.to_worktree }));
            assert!(worktrees.rows.iter().all(|row| {
                row.command.iter().any(|arg| arg == "blocks-engine")
                    && !row.command.iter().any(|arg| arg == "php-transformer")
            }));
        });
    }

    #[test]
    fn cook_batch_repo_normalization_rejects_handles_and_unknown_paths_with_corrections() {
        with_isolated_home(|home| {
            let private_sentinel = "PRIVATE_GATE_SENTINEL_INVALID_REPO";
            let primary = home.path().join("primary");
            std::fs::create_dir(&primary).expect("primary directory");
            init_git_primary(&primary);
            write_component_registration(home.path(), "fixture", &primary);

            let mut handle = cook_batch_args();
            handle.repo = "fixture@fix-11984".to_string();
            handle.from = Some("origin/release".to_string());
            handle.base = Some("release".to_string());
            handle.branch_prefix = "repair".to_string();
            handle.fanout_id = Some("faithful-correction".to_string());
            handle.backend = Some("fixture-backend".to_string());
            handle.selector = Some("fixture-selector".to_string());
            handle.model = Some("fixture-model".to_string());
            handle.provider_profile = Some("fixture-profile".to_string());
            handle.secret_env = vec!["FIXTURE_TOKEN".to_string()];
            handle.gates.verify = vec!["cargo check --all".to_string()];
            handle.gates.private_verify = vec![private_sentinel.to_string()];
            handle.gates.gate_execution_policy = "continue-all".to_string();
            handle.gates.gate_timeout_seconds = 41;
            handle.gates.gate_heartbeat_interval_seconds = 9;
            handle.gates.gate_no_progress_timeout_seconds = 17;
            handle.gates.rerun_completed_gates = true;
            handle.gates.gate_environment_mode = "replace".to_string();
            handle.gates.gate_environment = vec![("FEATURE".to_string(), "enabled".to_string())];
            handle.gates.gate_toolchains = vec!["fixture-tool".to_string()];
            handle.gates.gate_toolchain_specs = vec![
                homeboy::agents::agent_tasks::gate::AgentTaskGateToolchainRequirement {
                    command: "custom-tool".to_string(),
                    probe_arguments: vec!["probe".to_string(), "--json".to_string()],
                },
            ];
            handle.max_concurrency = Some(3);
            handle.max_duration = Some(120);
            let error = normalize_cook_batch_repo(&mut handle).expect_err("handle is not a repo");
            assert_eq!(error.details["provided"], "fixture@fix-11984");
            assert_eq!(
                error.details["expected_kind"],
                "registered_repo_slug_or_primary_path"
            );
            assert_eq!(error.details["resolved_candidates"], json!(["fixture"]));
            assert!(error.details["correction_command"].is_null());
            let reentry = error.details["secure_reentry"]
                .as_str()
                .expect("private reentry instruction");
            assert!(reentry.contains("--repo fixture"));
            assert!(!reentry.contains(private_sentinel));
            let mut public_handle = handle.clone();
            public_handle.gates.private_verify.clear();
            let mut corrected = public_handle.clone();
            corrected.repo = "fixture".to_string();
            let expected_command = quote_args(&cook_batch_argv(&corrected));
            let error = normalize_cook_batch_repo(&mut public_handle)
                .expect_err("public fixture handle remains invalid");
            assert_eq!(
                error.details["correction_command"], expected_command,
                "the correction changes only --repo"
            );

            let cli = Cli::try_parse_from(cook_batch_argv(&corrected))
                .expect("correction command remains a valid invocation");
            let Commands::AgentTask(agent_task) = cli.command else {
                panic!("agent-task command");
            };
            let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
                panic!("fanout command");
            };
            let AgentTaskFanoutCommand::CookBatch(replayed) = fanout.command else {
                panic!("cook-batch command");
            };
            assert_eq!(replayed.repo, "fixture");
            assert_eq!(replayed.from, corrected.from);
            assert_eq!(replayed.base, corrected.base);
            assert_eq!(replayed.branch_prefix, corrected.branch_prefix);
            assert_eq!(replayed.fanout_id, corrected.fanout_id);
            assert_eq!(replayed.backend, corrected.backend);
            assert_eq!(replayed.selector, corrected.selector);
            assert_eq!(replayed.model, corrected.model);
            assert_eq!(replayed.provider_profile, corrected.provider_profile);
            assert_eq!(replayed.secret_env, corrected.secret_env);
            assert_eq!(replayed.gates.verify, corrected.gates.verify);
            assert_eq!(
                replayed.gates.private_verify,
                corrected.gates.private_verify
            );
            assert_eq!(
                replayed.gates.gate_execution_policy,
                corrected.gates.gate_execution_policy
            );
            assert_eq!(
                replayed.gates.gate_timeout_seconds,
                corrected.gates.gate_timeout_seconds
            );
            assert_eq!(
                replayed.gates.gate_environment,
                corrected.gates.gate_environment
            );
            assert_eq!(
                replayed.gates.gate_toolchains,
                corrected.gates.gate_toolchains
            );
            assert_eq!(
                replayed.gates.gate_toolchain_specs,
                corrected.gates.gate_toolchain_specs
            );
            assert_eq!(replayed.max_concurrency, corrected.max_concurrency);
            assert_eq!(replayed.max_duration, corrected.max_duration);

            let mut unknown = cook_batch_args();
            unknown.repo = home.path().join("unknown").to_string_lossy().to_string();
            let error =
                normalize_cook_batch_repo(&mut unknown).expect_err("unknown path is rejected");
            assert_eq!(error.details["provided"], unknown.repo);
            assert_eq!(error.details["identity_classification"], "missing_path");
            assert_eq!(error.details["resolved_candidates"], json!([]));
            assert!(error.details["correction_command"].is_null());

            let non_git_path = home.path().join("non-git");
            std::fs::create_dir(&non_git_path).expect("non-Git path");
            let mut non_git = cook_batch_args();
            non_git.repo = non_git_path.display().to_string();
            let error = normalize_cook_batch_repo(&mut non_git).expect_err("non-Git path");
            assert_eq!(error.details["identity_classification"], "non_git_path");

            let unregistered_path = home.path().join("unregistered-repository");
            std::fs::create_dir(&unregistered_path).expect("unregistered repository");
            git(&unregistered_path, &["init"]);
            git(
                &unregistered_path,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://example.test/acme/unregistered-repository.git",
                ],
            );
            let mut unregistered = cook_batch_args();
            unregistered.repo = unregistered_path.display().to_string();
            let error = normalize_cook_batch_repo(&mut unregistered)
                .expect_err("unregistered repository is rejected");
            assert_eq!(
                error.details["identity_classification"],
                "unregistered_repository"
            );
            assert_eq!(
                error.details["repository_candidates"],
                json!(["unregistered-repository"])
            );

            let stale_path = unregistered_path.join("packages/stale-component");
            std::fs::write(
                home.path()
                    .join(".config/homeboy/components/stale-component.json"),
                serde_json::json!({ "local_path": stale_path }).to_string(),
            )
            .expect("stale component registration");
            let error = normalize_cook_batch_repo(&mut unregistered)
                .expect_err("stale registration is rejected");
            assert_eq!(error.details["identity_classification"], "stale_registry");
            assert_eq!(
                error.details["component_candidates"],
                json!(["stale-component"])
            );
        });
    }

    #[test]
    fn fanout_repository_identity_fails_before_any_configured_planner_deadline() {
        with_isolated_home(|home| {
            let repository = home.path().join("blocks-engine");
            let component = repository.join("packages/php-transformer");
            std::fs::create_dir_all(&component).expect("nested component");
            git(&repository, &["init"]);
            git(
                &repository,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/Extra-Chill/blocks-engine.git",
                ],
            );
            write_component_registration(home.path(), "php-transformer", &component);
            let blocking_registration =
                home.path().join(".config/homeboy/components/blocking.json");
            assert!(Command::new("mkfifo")
                .arg(&blocking_registration)
                .status()
                .expect("create blocking registration")
                .success());

            let bin = home.path().join("bin");
            std::fs::create_dir(&bin).expect("fake Git bin");
            let fake_git = bin.join("git");
            let git_invoked = home.path().join("git-invoked");
            std::fs::write(
                &fake_git,
                format!("#!/bin/sh\n: > '{}'\nsleep 5\n", git_invoked.display()),
            )
            .expect("sleeping Git");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&fake_git, std::fs::Permissions::from_mode(0o755))
                    .expect("executable fake Git");
            }
            let previous_path = std::env::var_os("PATH");
            let mut path = std::ffi::OsString::from(bin.as_os_str());
            path.push(":");
            path.push(previous_path.unwrap_or_default());
            let _path = homeboy::core::test_support::EnvVarGuard::set("PATH", path);

            for (repo, timeout) in [
                (repository.display().to_string(), 1),
                ("~/blocks-engine".to_string(), 60),
            ] {
                let mut args = cook_batch_args();
                args.repo = repo;
                args.dry_run_planner_timeout_seconds = Some(timeout);
                let started = Instant::now();
                let error = cook_batch(args).expect_err("unregistered repository root");
                assert!(
                    started.elapsed() < Duration::from_secs(2),
                    "{timeout}-second planner setting delayed static identity failure: {error}"
                );
                assert_eq!(
                    error.details["identity_classification"],
                    "ambiguous_nested_component"
                );
                assert_eq!(
                    error.details["repository_candidates"],
                    json!(["blocks-engine"])
                );
                assert_eq!(
                    error.details["component_candidates"],
                    json!(["php-transformer"])
                );
                assert!(error.details["correction_command"].is_null());
                assert_eq!(
                    error.details["identity_separation_tracker"],
                    "https://github.com/Extra-Chill/homeboy/issues/12844"
                );
                assert!(error.details["reason"].is_null());
            }
            assert!(
                !git_invoked.exists(),
                "static identity resolution must not invoke Git"
            );
        });
    }

    fn with_materialized_cook_batch_worktrees(test: impl FnOnce()) {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let mut config = homeboy::core::defaults::load_config();
            config.agent_task.default_backend = Some("sandbox".to_string());
            config.worktree_providers.clear();
            config.settings.remove(
                homeboy::core::worktree_providers::WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY,
            );
            homeboy::core::defaults::save_config(&config)
                .expect("configure fixture default backend");
            // `--repo homeboy` only resolves against a registered primary, so the
            // fixture has to own that registration as well as the worktrees it
            // adopts. Without it every cook-batch case fails in repository
            // resolution before reaching the behavior under test.
            let primary = home.path().join("cook-batch-primary");
            std::fs::create_dir_all(&primary).expect("create primary fixture");
            for args in [
                ["init", "-b", "main"].as_slice(),
                ["config", "user.email", "test@example.com"].as_slice(),
                ["config", "user.name", "Homeboy Test"].as_slice(),
                ["commit", "--allow-empty", "-m", "initial"].as_slice(),
                // The planned source ref has to resolve without a network, so the
                // fixture publishes `origin/main` locally rather than fetching it.
                ["update-ref", "refs/remotes/origin/main", "HEAD"].as_slice(),
            ] {
                assert!(
                    Command::new("git")
                        .args(args)
                        .current_dir(&primary)
                        .status()
                        .expect("initialize primary fixture")
                        .success(),
                    "git {args:?} failed"
                );
            }
            init_git_primary(&primary);
            write_component_registration(home.path(), "homeboy", &primary);
            let worktrees = tempfile::tempdir().expect("managed worktree fixtures");
            for (handle, name) in [
                ("homeboy@fix-issue-6453-homeboy", "issue-6453"),
                ("homeboy@fix-issue-6454-homeboy", "issue-6454"),
            ] {
                let path = worktrees.path().join(name);
                std::fs::create_dir(&path).expect("create managed worktree fixture");
                for args in [
                    ["init", "-b", "main"].as_slice(),
                    ["config", "user.email", "test@example.com"].as_slice(),
                    ["config", "user.name", "Homeboy Test"].as_slice(),
                    ["commit", "--allow-empty", "-m", "initial"].as_slice(),
                ] {
                    assert!(
                        Command::new("git")
                            .args(args)
                            .current_dir(&path)
                            .status()
                            .expect("initialize managed worktree fixture")
                            .success(),
                        "git {args:?} failed"
                    );
                }
                worktree::adopt(worktree::WorktreeAdoptOptions {
                    handle: handle.to_string(),
                    path: path.display().to_string(),
                    kind: Some("test-fixture".to_string()),
                    provenance: None,
                })
                .expect("register managed worktree fixture");
            }
            test();
        });
    }

    #[cfg(unix)]
    #[test]
    fn fanout_uses_an_ensure_resolve_provider_without_a_finalizer_for_creation_binding_and_repair()
    {
        use std::os::unix::fs::PermissionsExt;

        with_isolated_home(|_| {
            let fixture = tempfile::tempdir().expect("provider fixture");
            let workspace_root = fixture.path().join("worktrees");
            let ensured = fixture.path().join("ensured");
            let provider = fixture.path().join("provider");
            std::fs::create_dir(&workspace_root).expect("provider worktree directory");
            std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncase \"$1\" in\nresolve)\n  path='{}/'$2\n  if [ -d \"$path\" ]; then\n    branch=$(git -C \"$path\" branch --show-current)\n    printf '{{\"worktrees\":[{{\"handle\":\"%s\",\"path\":\"%s\",\"branch\":\"%s\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}\\n' \"$2\" \"$path\" \"$branch\"\n  else\n    printf '{{\"status\":\"error\",\"error\":{{\"code\":\"worktree_not_found\"}}}}\\n'\n  fi\n  ;;\nensure)\n  path='{}/'$2\n  git init --quiet -b \"$5\" \"$path\"\n  printf '%s|%s|%s|%s\\n' \"$2\" \"$8\" \"$9\" \"${{10}}\" >> '{}'\n  ;;\nesac\n",
                    workspace_root.display(),
                    workspace_root.display(),
                    ensured.display(),
                ),
            )
            .expect("write provider");
            let mut permissions = std::fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).expect("make provider executable");

            let mut config = homeboy::core::defaults::HomeboyConfig::default();
            config.worktree_providers.insert(
                "fixture".to_string(),
                homeboy::core::defaults::WorktreeProviderConfig {
                    enabled: true,
                    kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                    apply_enabled: true,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: homeboy::core::defaults::WorktreeProviderCommands {
                        resolve: Some(vec![
                            provider.display().to_string(),
                            "resolve".to_string(),
                            "{handle}".to_string(),
                        ]),
                        resolve_not_found_exit_codes: vec![1],
                        ensure: Some(vec![
                            provider.display().to_string(),
                            "ensure".to_string(),
                            "{handle}".to_string(),
                            "{repo}".to_string(),
                            "{base}".to_string(),
                            "{head}".to_string(),
                            "{task_url}".to_string(),
                            "{idempotency_key}".to_string(),
                            "{purpose}".to_string(),
                            "{owner_run_ref}".to_string(),
                            "{cleanup_policy}".to_string(),
                        ]),
                        ..Default::default()
                    },
                    list_result_mapping: Some(
                        homeboy::core::defaults::WorktreeProviderListResultMapping {
                            items: "$.worktrees".to_string(),
                            handle: "$.handle".to_string(),
                            path: "$.path".to_string(),
                            branch: "$.branch".to_string(),
                            dirty: "$.safety.dirty".to_string(),
                            unpushed: "$.safety.unpushed".to_string(),
                            primary: "$.safety.primary".to_string(),
                            task_url: None,
                        },
                    ),
                },
            );
            homeboy::core::defaults::save_config(&config).expect("save provider config");

            let mut args = cook_batch_args();
            args.issues = (6453..=6458)
                .map(|number| format!("https://github.com/Extra-Chill/homeboy/issues/{number}"))
                .collect();
            args.preview = false;
            let mut plan = build_cook_batch_plan(&args).expect("fanout plan");
            let (worktrees, resolution) =
                queue_or_reuse_worktrees_with_terminal_paths(&args, &plan, None, false)
                    .expect("provider worktree queue");

            assert!(worktrees.rows.iter().all(|row| {
                row.status == worktree::WorktreeQueueCreateStatus::Created
                    && row.path.as_deref().is_some_and(|path| {
                        path.starts_with(workspace_root.to_string_lossy().as_ref())
                    })
            }));
            assert!(resolution["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["state"] == "created"));
            let ensured = std::fs::read_to_string(&ensured).expect("provider ensure records");
            assert_eq!(ensured.lines().count(), 6, "{ensured}");
            assert!(ensured.contains("agent_task_cook"));
            assert!(plan
                .cooks
                .iter()
                .all(|cook| ensured.contains(&cook.to_worktree)));
            assert!(worktrees.rows.iter().all(|row| {
                row.command.first() == Some(&provider.display().to_string())
                    && !row
                        .command
                        .windows(3)
                        .any(|argv| argv == ["homeboy", "worktree", "create"])
            }));

            bind_materialized_worktree_paths(&mut plan, &worktrees);
            assert_eq!(plan.cooks.len(), 6);
            assert!(plan.cooks.iter().all(|cook| cook.workspace.is_some()));

            let mut blocked = worktrees.clone();
            blocked.rows[0].status = worktree::WorktreeQueueCreateStatus::Failed;
            let actions = cook_batch_next_actions(
                &args,
                &plan.fanout_id,
                "blocked",
                true,
                false,
                &blocked,
                false,
                None,
            );
            let repair_commands = action_commands(&actions);
            assert!(repair_commands
                .iter()
                .any(|command| command.contains(&provider.display().to_string())));
            assert!(!repair_commands
                .iter()
                .any(|command| command.contains("homeboy worktree create")));
        });
    }

    #[cfg(unix)]
    #[test]
    fn provider_fanout_retry_re_resolves_out_of_band_worktrees_without_ensuring_again() {
        use std::os::unix::fs::PermissionsExt;

        with_isolated_home(|_| {
            let fixture = tempfile::tempdir().expect("provider fixture");
            let workspace_root = fixture.path().join("worktrees");
            let ensure_calls = fixture.path().join("ensure-calls");
            let provider = fixture.path().join("provider");
            std::fs::create_dir(&workspace_root).expect("provider worktree directory");
            std::fs::write(
                &provider,
                format!(
                    "#!/bin/sh\ncase \"$1\" in\nresolve)\n  path='{}/'$2\n  if [ -d \"$path\" ]; then\n    branch=$(git -C \"$path\" branch --show-current)\n    printf '{{\"worktrees\":[{{\"handle\":\"%s\",\"path\":\"%s\",\"branch\":\"%s\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}\\n' \"$2\" \"$path\" \"$branch\"\n  else\n    exit 1\n  fi\n  ;;\nensure)\n  printf '%s\\n' \"$2\" >> '{}'\n  exit 1\n  ;;\nesac\n",
                    workspace_root.display(),
                    ensure_calls.display(),
                ),
            )
            .expect("write provider");
            let mut permissions = std::fs::metadata(&provider)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&provider, permissions).expect("make provider executable");

            let mut config = homeboy::core::defaults::HomeboyConfig::default();
            config.worktree_providers.insert(
                "fixture".to_string(),
                homeboy::core::defaults::WorktreeProviderConfig {
                    enabled: true,
                    kind: homeboy::core::defaults::WorktreeProviderKind::Command,
                    apply_enabled: true,
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    commands: homeboy::core::defaults::WorktreeProviderCommands {
                        resolve: Some(vec![
                            provider.display().to_string(),
                            "resolve".to_string(),
                            "{handle}".to_string(),
                        ]),
                        resolve_not_found_exit_codes: vec![1],
                        ensure: Some(vec![
                            provider.display().to_string(),
                            "ensure".to_string(),
                            "{handle}".to_string(),
                            "{repo}".to_string(),
                            "{base}".to_string(),
                            "{head}".to_string(),
                            "{task_url}".to_string(),
                            "{idempotency_key}".to_string(),
                            "{purpose}".to_string(),
                            "{owner_run_ref}".to_string(),
                            "{cleanup_policy}".to_string(),
                        ]),
                        ..Default::default()
                    },
                    list_result_mapping: Some(
                        homeboy::core::defaults::WorktreeProviderListResultMapping {
                            items: "$.worktrees".to_string(),
                            handle: "$.handle".to_string(),
                            path: "$.path".to_string(),
                            branch: "$.branch".to_string(),
                            dirty: "$.safety.dirty".to_string(),
                            unpushed: "$.safety.unpushed".to_string(),
                            primary: "$.safety.primary".to_string(),
                            task_url: None,
                        },
                    ),
                },
            );
            homeboy::core::defaults::save_config(&config).expect("save provider config");

            let mut args = cook_batch_args();
            args.preview = false;
            let plan = build_cook_batch_plan(&args).expect("immutable fanout plan");
            let original_plan = plan.clone();
            let (first_claim, retry) =
                claim_fanout_run_batch_coordinator(&plan, Placement::Auto).expect("first claim");
            assert!(!retry);
            let (blocked, blocked_resolution) =
                queue_or_reuse_worktrees_with_terminal_paths(&args, &plan, None, false)
                    .expect("blocked first attempt");
            assert_eq!(
                blocked.rows[0].status,
                worktree::WorktreeQueueCreateStatus::Failed
            );
            assert!(blocked.rows[1..]
                .iter()
                .all(|row| { row.status == worktree::WorktreeQueueCreateStatus::Queued }));
            batch::record_fanout_run_batch_failure(
                &plan.fanout_id,
                &first_claim,
                "worktree_preflight",
                json!({
                    "worktrees": blocked.rows,
                    "resolution": blocked_resolution,
                }),
            )
            .expect("record blocked preflight");
            assert_eq!(
                batch::read_batch_record(&plan.fanout_id).unwrap().metadata["terminal_failure"]
                    ["failure"]["worktrees"],
                json!(blocked.rows),
                "the first blocked observation is durable"
            );
            assert_eq!(
                batch::read_batch_record(&plan.fanout_id).unwrap().metadata["terminal_failure"]
                    ["failure"]["resolution"]["rows"][0]["state"],
                "blocked"
            );

            let (still_blocked_claim, retry) =
                claim_fanout_run_batch_coordinator(&plan, Placement::Auto)
                    .expect("reclaim repaired batch");
            assert!(retry);
            assert!(
                cook_batch_coordinator_control(&plan.fanout_id, retry)
                    .skip_durably_terminal_children
            );
            let (still_blocked, still_blocked_resolution) =
                queue_or_reuse_worktrees_with_terminal_paths(&args, &plan, None, true)
                    .expect("unchanged provider stays blocked");
            assert_eq!(
                still_blocked.rows[0].status,
                worktree::WorktreeQueueCreateStatus::Failed
            );
            assert!(still_blocked.rows[1..]
                .iter()
                .all(|row| { row.status == worktree::WorktreeQueueCreateStatus::Queued }));
            batch::record_fanout_run_batch_failure(
                &plan.fanout_id,
                &still_blocked_claim,
                "worktree_preflight",
                json!({
                    "worktrees": still_blocked.rows,
                    "resolution": still_blocked_resolution,
                }),
            )
            .expect("refresh unchanged blocked preflight");
            assert_eq!(
                batch::read_batch_record(&plan.fanout_id).unwrap().metadata["terminal_failure"]
                    ["failure"]["worktrees"],
                json!(still_blocked.rows),
                "retry status reflects the current provider observation"
            );
            assert_eq!(
                batch::read_batch_record(&plan.fanout_id).unwrap().metadata["terminal_failure"]
                    ["failure"]["resolution"]["rows"][0]["state"],
                "still_blocked"
            );
            assert_eq!(
                std::fs::read_to_string(&ensure_calls)
                    .expect("one ensure call per blocked attempt"),
                format!("{0}\n{0}\n", plan.cooks[0].to_worktree,)
            );

            let mismatched = &plan.cooks[0];
            let mismatched_path = workspace_root.join(&mismatched.to_worktree);
            std::fs::create_dir(&mismatched_path).expect("materialize mismatched destination");
            assert!(Command::new("git")
                .args(["init", "--quiet", "-b", "wrong-branch"])
                .current_dir(&mismatched_path)
                .status()
                .expect("initialize mismatched destination")
                .success());
            let mismatch = queue_or_reuse_worktrees(&args, &plan)
                .expect_err("immutable branch mismatch must be rejected");
            assert!(mismatch.message.contains("branch does not match"));
            std::fs::remove_dir_all(&mismatched_path).expect("remove mismatched destination");

            for cook in &plan.cooks {
                let path = workspace_root.join(&cook.to_worktree);
                std::fs::create_dir(&path).expect("materialize provider destination");
                assert!(Command::new("git")
                    .args(["init", "--quiet", "-b", cook.head.as_deref().expect("head")])
                    .current_dir(&path)
                    .status()
                    .expect("initialize provider destination")
                    .success());
            }

            let (_, retry) = claim_fanout_run_batch_coordinator(&plan, Placement::Auto)
                .expect("claim recovered rerun");
            assert!(retry);
            let (recovered, recovered_resolution) =
                queue_or_reuse_worktrees_with_terminal_paths(&args, &plan, None, true)
                    .expect("re-resolve recovered destinations");

            assert_eq!(plan, original_plan, "resolution cannot mutate Cook intent");
            assert!(recovered.rows.iter().all(|row| {
                row.status == worktree::WorktreeQueueCreateStatus::Created
                    && row.path.as_deref().is_some_and(|path| {
                        path.starts_with(workspace_root.to_string_lossy().as_ref())
                    })
                    && row.error.is_none()
            }));
            assert!(recovered_resolution["rows"]
                .as_array()
                .unwrap()
                .iter()
                .all(|row| row["state"] == "re_resolved"));

            let terminal_cook = &plan.cooks[0];
            let terminal_path = workspace_root.join(&terminal_cook.to_worktree);
            std::fs::remove_dir_all(&terminal_path).expect("simulate finalized terminal worktree");
            let terminal_paths =
                BTreeMap::from([(terminal_cook.run_id(), terminal_path.display().to_string())]);
            let (terminal_replay, terminal_resolution) =
                queue_or_reuse_worktrees_with_terminal_paths(
                    &args,
                    &plan,
                    Some(&terminal_paths),
                    true,
                )
                .expect("terminal child does not reprovision its finalized worktree");
            assert_eq!(
                terminal_replay.rows[0].path.as_deref(),
                Some(terminal_path.to_string_lossy().as_ref())
            );
            assert_eq!(terminal_resolution["rows"][0]["state"], "reused_terminal");
            assert_eq!(
                std::fs::read_to_string(&ensure_calls).expect("ensure call remains recorded"),
                format!("{0}\n{0}\n", plan.cooks[0].to_worktree),
                "exact recovered destinations must not trigger duplicate creation"
            );
            assert!(cook_batch_next_actions(
                &args,
                &plan.fanout_id,
                "ready",
                true,
                false,
                &recovered,
                false,
                None,
            )
            .iter()
            .all(|action| !action.label.starts_with("create blocked worktree")));
        });
    }

    #[test]
    fn cook_batch_builds_batch_cook_plan_from_issue_urls() {
        with_isolated_home(|_| {
            let args = cook_batch_args();
            let mut plan = build_cook_batch_plan(&args).expect("cook batch plan");

            assert_eq!(plan.fanout_id, "issue-wave");
            assert_eq!(plan.cooks.len(), 2);
            assert_eq!(plan.cooks[0].cook_id, "issue-wave-issue-6453");
            assert_eq!(plan.cooks[0].to_worktree, "homeboy@fix-issue-6453-homeboy");
            assert_eq!(
                plan.cooks[0].head.as_deref(),
                Some("fix/issue-6453-homeboy")
            );
            assert_eq!(
                plan.cooks[0].title.as_deref(),
                Some("Fix Extra-Chill/homeboy#6453")
            );
            assert!(plan.cooks[0]
                .prompt
                .as_deref()
                .expect("prompt")
                .contains("https://github.com/Extra-Chill/homeboy/issues/6453"));
            let prompt = plan.cooks[0].prompt.as_deref().expect("prompt");
            assert!(prompt.contains("Homeboy will commit, push the prepared branch"));
            assert!(prompt.contains("create or update the PR"));
            assert!(prompt.contains("add AI disclosure"));
            assert!(
                prompt.contains("Do not inspect credentials, configure git identity, commit, push")
            );
            crate::commands::agent_task::run::validate_provider_evidence_inputs(&[], Some(prompt))
                .expect("generated default prompt must pass evidence-path validation");
            assert_eq!(plan.cooks[0].verify, vec!["cargo test --lib"]);
            assert_eq!(plan.cooks[0].backend.as_deref(), Some("sandbox"));

            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(
                invocation.dispatch.workspace.as_deref(),
                Some("homeboy@fix-issue-6453-homeboy")
            );

            let workspace = tempfile::tempdir().expect("task worktree");
            bind_materialized_worktree_paths(
                &mut plan,
                &worktree::WorktreeQueueCreateOutput {
                    schema: "homeboy/worktree-queue-create/v1",
                    repo: "homeboy".to_string(),
                    base_ref: "origin/main".to_string(),
                    dry_run: false,
                    rows: vec![worktree::WorktreeQueueCreateRow {
                        branch: "fix/issue-6453-homeboy".to_string(),
                        handle: "homeboy@fix-issue-6453-homeboy".to_string(),
                        status: worktree::WorktreeQueueCreateStatus::Created,
                        command: Vec::new(),
                        retry_after_seconds: None,
                        active_lock_holder: None,
                        path: Some(workspace.path().display().to_string()),
                        error: None,
                        failure: None,
                    }],
                },
            );
            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("materialized cook invocation");
            assert_eq!(
                invocation.dispatch.workspace.as_deref(),
                Some(workspace.path().to_string_lossy().as_ref())
            );
            assert_eq!(
                invocation.options.workspace.source_worktree_path.as_deref(),
                Some(workspace.path())
            );
        });
    }

    #[test]
    fn cook_batch_adopts_explicit_three_child_worktree_bindings_idempotently() {
        with_isolated_home(|_| {
            let mut args = cook_batch_args();
            args.issues
                .push("https://github.com/Extra-Chill/homeboy/issues/6455".to_string());
            args.worktrees = vec![
                "https://github.com/Extra-Chill/homeboy/issues/6453=provider@prepared-6453"
                    .to_string(),
                "https://github.com/Extra-Chill/homeboy/issues/6454=provider@prepared-6454"
                    .to_string(),
                "https://github.com/Extra-Chill/homeboy/issues/6455=provider@prepared-6455"
                    .to_string(),
            ];
            let first = build_cook_batch_plan(&args).expect("plan explicit adoptions");
            let replay = build_cook_batch_plan(&args).expect("replay explicit adoptions");
            assert_eq!(
                first, replay,
                "replay must retain child and worktree identities"
            );
            assert_eq!(first.cooks.len(), 3);
            assert!(first.cooks.iter().all(|cook| cook.adopted_worktree));
            assert_eq!(
                first
                    .cooks
                    .iter()
                    .map(|cook| cook.to_worktree.as_str())
                    .collect::<Vec<_>>(),
                vec![
                    "provider@prepared-6453",
                    "provider@prepared-6454",
                    "provider@prepared-6455"
                ]
            );
        });
    }

    #[test]
    fn cook_batch_rejects_partial_duplicate_and_unrelated_worktree_bindings() {
        with_isolated_home(|_| {
            let mut partial = cook_batch_args();
            partial.worktrees = vec![
                "https://github.com/Extra-Chill/homeboy/issues/6453=provider@prepared-6453"
                    .to_string(),
            ];
            assert!(build_cook_batch_plan(&partial).is_err());

            let mut duplicate = cook_batch_args();
            duplicate.worktrees = vec![
                "https://github.com/Extra-Chill/homeboy/issues/6453=provider@prepared-6453"
                    .to_string(),
                "https://github.com/Extra-Chill/homeboy/issues/6453=provider@other-6453"
                    .to_string(),
            ];
            assert!(build_cook_batch_plan(&duplicate).is_err());

            let mut unrelated = cook_batch_args();
            unrelated.worktrees = vec![
                "https://github.com/Extra-Chill/homeboy/issues/6453=provider@prepared-6453"
                    .to_string(),
                "https://github.com/Extra-Chill/homeboy/issues/9999=provider@prepared-9999"
                    .to_string(),
            ];
            assert!(build_cook_batch_plan(&unrelated).is_err());
        });
    }

    #[test]
    fn private_batch_artifact_is_persisted_only_after_workspace_binding() {
        with_materialized_cook_batch_worktrees(|| {
            let sentinel = "PRIVATE_GATE_BOUND_PLAN_SENTINEL";
            let mut args = cook_batch_args();
            args.gates.private_verify = vec![sentinel.to_string()];
            args.preview = false;
            args.run_plan = false;

            let (public, exit_code) = cook_batch(args).expect("prepare private batch");
            assert_eq!(exit_code, 0);
            assert!(!public.to_string().contains(sentinel));
            let fanout_id = public["fanout_id"].as_str().expect("fanout id");
            let path = private_batch_plan_path(fanout_id).expect("private artifact path");
            let loaded = load_batch_cook_fanout_plan(
                &AgentTaskFanoutInputArgs {
                    input: format!("@{}", path.display()),
                    fanout_id: None,
                    backend: None,
                    selector: None,
                    model: None,
                },
                true,
            )
            .expect("load bound private artifact");
            assert_eq!(loaded.cooks[0].private_verify, vec![sentinel]);
            assert!(loaded.cooks.iter().all(|cook| cook.workspace.is_some()));
        });
    }

    #[test]
    fn cook_batch_projects_a_declared_verification_plan_to_cook_and_lab() {
        with_materialized_cook_batch_worktrees(|| {
            let mut batch_args = cook_batch_args();
            batch_args
                .issues
                .push("https://github.com/Extra-Chill/homeboy/issues/6455".to_string());
            batch_args.gates.verify.clear();
            batch_args.verification_profiles = Some(
                serde_json::json!({
                    "profiles": {
                        "review": { "plan": {
                            "adapter": "homeboy_review_test",
                            "command": ["homeboy", "review", "test", "homeboy"],
                            "suite_timeout_seconds": 123,
                        }}
                    },
                    "assignments": [
                        { "selector": "issue-6454", "profile": "review" }
                    ]
                })
                .to_string(),
            );

            let plan = build_cook_batch_plan(&batch_args).expect("mixed verification plan");
            assert_eq!(
                plan.cooks[1].verification_profile.as_deref(),
                Some("review")
            );
            let declared = plan.cooks[1]
                .test_execution_plan
                .as_ref()
                .expect("declared plan");
            assert_eq!(declared.suite_timeout().as_secs(), 123);

            let round_trip = BatchCookFanoutPlan::from_value(
                serde_json::to_value(&plan).expect("serialize plan"),
                &args(),
            )
            .expect("deserialize plan");
            assert_eq!(round_trip.cooks.len(), plan.cooks.len());
            for (reloaded, original) in round_trip.cooks.iter().zip(&plan.cooks) {
                assert_eq!(reloaded.verification_profile, original.verification_profile);
                assert_eq!(reloaded.verify, original.verify);
                assert_eq!(reloaded.private_verify, original.private_verify);
                assert_eq!(reloaded.test_execution_plan, original.test_execution_plan);
            }
            assert_eq!(
                round_trip.cooks[1]
                    .to_cook_invocation(&round_trip)
                    .expect("Lab handoff invocation")
                    .options
                    .gates
                    .test_execution_plan,
                plan.cooks[1].test_execution_plan
            );
            assert_eq!(
                round_trip.cooks[1]
                    .to_cook_invocation(&round_trip)
                    .expect("Cook projection")
                    .options
                    .gates
                    .gate_timeout_seconds,
                batch_args.gates.gate_timeout_seconds
            );
        });
    }

    #[test]
    fn cook_batch_rejects_an_unmatched_verification_profile_selector() {
        let mut args = cook_batch_args();
        args.verification_profiles = Some(
            r#"{"profiles":{"node":{"plan":{"adapter":"homeboy_review_test","command":["homeboy","review","test"],"suite_timeout_seconds":30}}},"assignments":[{"selector":"issue-9999","profile":"node"}]}"#.to_string(),
        );

        let error = build_cook_batch_plan(&args).expect_err("unmatched profile selector");
        assert_eq!(
            error.details["field"],
            "verification-profiles.assignments.selector"
        );
        assert!(error.details["problem"]
            .as_str()
            .expect("typed problem")
            .starts_with("selector_unmatched:"));
    }

    #[test]
    fn cook_batch_requires_a_gate_for_every_resolved_child() {
        let mut args = cook_batch_args();
        args.gates.verify.clear();
        args.verification_profiles = Some(
            r#"{"profiles":{"rust":{"plan":{"adapter":"homeboy_review_test","command":["homeboy","review","test"],"suite_timeout_seconds":30}}},"assignments":[{"selector":"issue-6453","profile":"rust"}]}"#.to_string(),
        );

        let plan = build_cook_batch_plan(&args).expect("plan before worktree creation");
        let error = validate_batch_cook_gates(&plan, None).expect_err("every child needs a gate");
        assert_eq!(error.details["problem"], "gate_missing: every cook-batch child requires verify or private_verify before worktree creation");
        assert!(error.details["id"]
            .as_str()
            .expect("uncovered child id")
            .ends_with("issue-6454"));
        assert_eq!(
            error.details["tried"],
            json!([
                "https://github.com/Extra-Chill/homeboy/issues/6454",
                "Extra-Chill/homeboy#6454",
                "issue-6454"
            ])
        );
    }

    #[test]
    fn cook_batch_rejects_repository_script_alias_before_worktree_queueing() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let source = home.path().join("fixture-primary");
            std::fs::create_dir_all(&source).expect("primary directory");
            std::fs::write(
                source.join("homeboy.json"),
                r#"{"scripts":{"lint":["check"]}}"#,
            )
            .expect("component manifest");
            init_git_primary(&source);
            write_component_registration(home.path(), "fixture", &source);

            let mut args = cook_batch_args();
            args.repo = "fixture".to_string();
            args.gates.verify = vec!["homeboy lint fixture --path .".to_string()];
            args.preview = false;
            let error =
                cook_batch(args).expect_err("script alias must reject before queuing worktrees");
            assert!(error.message.contains("repository script identity"));
            assert!(error.message.contains("homeboy review lint --path ."));
            assert!(!home.path().join("fixture@fix-issue-6453-fixture").exists());
        });
    }

    #[test]
    fn fanout_alias_without_a_component_root_reports_the_admission_limit() {
        let mut args = cook_batch_args();
        args.gates.verify = vec!["homeboy lint fixture --path .".to_string()];
        let plan = build_cook_batch_plan(&args).expect("plan");
        let error = validate_batch_gate_contracts(&plan, None)
            .expect_err("alias needs an authoritative workspace");
        assert!(error
            .message
            .contains("no authoritative registered component workspace"));
    }

    #[test]
    fn fanout_ai_tool_override_is_typed_persisted_and_applied_to_every_child() {
        with_materialized_cook_batch_worktrees(|| {
            let mut cook_args = cook_batch_args();
            cook_args.ai_tool = Some("OpenAI GPT-5.6 Sol via OpenCode".to_string());
            let plan =
                build_cook_batch_plan(&cook_args).expect("build plan with disclosure override");
            assert!(plan
                .cooks
                .iter()
                .all(|cook| cook.ai_tool == "OpenAI GPT-5.6 Sol via OpenCode"));

            let serialized = serde_json::to_value(&plan).expect("serialize plan");
            let mut loaded =
                BatchCookFanoutPlan::from_value(serialized, &args()).expect("load persisted plan");
            loaded.apply_ai_tool_override(Some("OpenAI GPT-5.6 Terra via OpenCode"));
            persist_batch_cook_recipes(&loaded, |_| {}).expect("persist child recipes");
            for cook in &loaded.cooks {
                let invocation = cook.to_cook_invocation(&loaded).expect("cook invocation");
                assert_eq!(
                    invocation.options.ai_disclosure.ai_tool,
                    "OpenAI GPT-5.6 Terra via OpenCode"
                );
                let recipe = agent_task_service::load_recipe(&cook.run_id())
                    .expect("load persisted child recipe");
                assert_eq!(
                    recipe.finalization["ai_tool"],
                    "OpenAI GPT-5.6 Terra via OpenCode"
                );
            }
        });
    }

    #[test]
    fn fanout_plan_keeps_child_disclosures_without_an_override() {
        let plan = BatchCookFanoutPlan::from_value(
            json!({
                "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                "fanout_id": "mixed-disclosures",
                "cooks": [
                    {"cook_id":"sol","prompt":"sol","cwd":env!("CARGO_MANIFEST_DIR"),"to_worktree":"homeboy@sol","verify":["true"],"ai_tool":"OpenAI GPT-5.6 Sol via OpenCode"},
                    {"cook_id":"terra","prompt":"terra","cwd":env!("CARGO_MANIFEST_DIR"),"to_worktree":"homeboy@terra","verify":["true"],"ai_tool":"OpenAI GPT-5.6 Terra via OpenCode"}
                ]
            }),
            &args(),
        )
        .expect("load mixed-provider plan");
        assert_eq!(
            plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("sol invocation")
                .options
                .ai_disclosure
                .ai_tool,
            "OpenAI GPT-5.6 Sol via OpenCode"
        );
        assert_eq!(
            plan.cooks[1]
                .to_cook_invocation(&plan)
                .expect("terra invocation")
                .options
                .ai_disclosure
                .ai_tool,
            "OpenAI GPT-5.6 Terra via OpenCode"
        );
    }

    #[test]
    fn cook_batch_run_plan_binds_inferred_worktree_for_dispatch_and_promotion() {
        // `cook-batch --run-plan` generates children without --cwd/--workspace.
        // Once its declared worktree is materialized, the same canonical root
        // must drive provider dispatch and promotion before execution starts.
        with_materialized_cook_batch_worktrees(|| {
            let mut plan = build_cook_batch_plan(&cook_batch_args()).expect("generated cook plan");
            let root = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).expect("canonical root");
            let rows = plan
                .cooks
                .iter()
                .map(|cook| worktree::WorktreeQueueCreateRow {
                    branch: cook.head.clone().expect("generated head"),
                    handle: cook.to_worktree.clone(),
                    status: worktree::WorktreeQueueCreateStatus::Created,
                    command: Vec::new(),
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(root.display().to_string()),
                    error: None,
                    failure: None,
                })
                .collect();
            bind_materialized_worktrees(
                &mut plan,
                &worktree::WorktreeQueueCreateOutput {
                    schema: "homeboy/worktree-queue-create/v1",
                    repo: "homeboy".to_string(),
                    base_ref: "origin/main".to_string(),
                    dry_run: false,
                    rows,
                },
            )
            .expect("bind materialized worktrees");

            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("workspace-bound cook invocation");
            assert_eq!(invocation.dispatch.workspace.as_deref(), root.to_str());
            assert_eq!(
                invocation.options.workspace.source_worktree_path.as_deref(),
                Some(root.as_path())
            );

            let compiled = compile_batch_cooks(&plan, |_| {}).expect("compile before provider");
            assert_eq!(
                compiled[0].identity.initial_plan.tasks[0]
                    .workspace
                    .root
                    .as_deref(),
                root.to_str()
            );
        });
    }

    #[test]
    fn cook_batch_binding_preserves_explicit_workspace_and_cwd_precedence() {
        let mut plan = build_cook_batch_plan(&cook_batch_args()).expect("generated cook plan");
        let explicit_cwd = std::fs::canonicalize(env!("CARGO_MANIFEST_DIR"))
            .expect("canonical explicit cwd")
            .display()
            .to_string();
        plan.cooks[0].cwd = Some(explicit_cwd.clone());
        plan.cooks[0].workspace = Some("/explicit/multi-repo-workspace".to_string());
        let materialized_root =
            std::fs::canonicalize(env!("CARGO_MANIFEST_DIR")).expect("canonical materialized root");
        let rows = plan
            .cooks
            .iter()
            .map(|cook| worktree::WorktreeQueueCreateRow {
                branch: cook.head.clone().expect("generated head"),
                handle: cook.to_worktree.clone(),
                status: worktree::WorktreeQueueCreateStatus::Created,
                command: Vec::new(),
                retry_after_seconds: None,
                active_lock_holder: None,
                path: Some(materialized_root.display().to_string()),
                error: None,
                failure: None,
            })
            .collect();
        bind_materialized_worktrees(
            &mut plan,
            &worktree::WorktreeQueueCreateOutput {
                schema: "homeboy/worktree-queue-create/v1",
                repo: "homeboy".to_string(),
                base_ref: "origin/main".to_string(),
                dry_run: false,
                rows,
            },
        )
        .expect("bind materialized worktrees");

        let invocation = plan.cooks[0]
            .to_cook_invocation(&plan)
            .expect("explicit workspace cook invocation");
        assert_eq!(
            invocation.dispatch.cwd.as_deref(),
            Some(explicit_cwd.as_str())
        );
        assert_eq!(
            invocation.dispatch.workspace.as_deref(),
            Some("/explicit/multi-repo-workspace")
        );
        assert_eq!(
            invocation.options.workspace.source_worktree_path.as_deref(),
            Some(std::path::Path::new(&explicit_cwd))
        );
    }

    #[test]
    fn cook_invocation_omits_model_when_only_disclosure_names_one() {
        // #9789: an ai_tool disclosure like `OpenCode (gpt-5.5)` must not be
        // reverse-parsed into a model. With no explicit/config/rotation model,
        // the execution request's ai_model stays None.
        with_isolated_home(|_| {
            let plan = BatchCookFanoutPlan::from_value(
                json!({
                    "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                    "fanout_id": "disclosure-only",
                    "cooks": [{
                        "cook_id": "no-model",
                        "prompt": "do the thing",
                        "cwd": env!("CARGO_MANIFEST_DIR"),
                        "to_worktree": "homeboy@no-model",
                        "verify": ["true"],
                        "ai_tool": "OpenCode (gpt-5.5)"
                    }]
                }),
                &args(),
            )
            .expect("plan");
            // Guard the premise: no model selection anywhere, only a disclosure.
            assert_eq!(plan.cooks[0].model, None);
            assert!(plan.cooks[0].ai_tool.contains("gpt-5.5"));

            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(
                invocation.options.ai_disclosure.ai_model, None,
                "disclosure text must not populate ai_model"
            );
        });
    }

    #[test]
    fn cook_invocation_preserves_explicit_model_selection() {
        // Explicit/config/rotation model selection must still populate the
        // execution request even when a disclosure is present.
        with_isolated_home(|_| {
            let plan = BatchCookFanoutPlan::from_value(
                json!({
                    "schema": AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA,
                    "fanout_id": "explicit-model",
                    "cooks": [{
                        "cook_id": "with-model",
                        "prompt": "do the thing",
                        "cwd": env!("CARGO_MANIFEST_DIR"),
                        "to_worktree": "homeboy@with-model",
                        "verify": ["true"],
                        "model": "openai/gpt-5.6-terra",
                        "ai_tool": "OpenCode (stale-disclosure-model)"
                    }]
                }),
                &args(),
            )
            .expect("plan");

            let invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            assert_eq!(
                invocation.options.ai_disclosure.ai_model.as_deref(),
                Some("openai/gpt-5.6-terra"),
                "explicit model selection must reach the execution request"
            );
        });
    }

    #[test]
    fn cook_batch_scopes_durable_children_to_the_fanout_generation() {
        let first = build_cook_batch_plan(&cook_batch_args()).expect("first cook batch plan");
        let mut second_args = cook_batch_args();
        second_args.fanout_id = Some("issue-wave-v2".to_string());
        second_args.branch_prefix = "fix-v2".to_string();
        let second = build_cook_batch_plan(&second_args).expect("second cook batch plan");

        assert_eq!(first.cooks[0].task_url, second.cooks[0].task_url);
        assert_eq!(
            first.cooks[0].client_context, second.cooks[0].client_context,
            "issue identity remains task metadata"
        );
        assert_ne!(first.cooks[0].cook_id, second.cooks[0].cook_id);
        assert_ne!(first.cooks[0].run_id(), second.cooks[0].run_id());
        assert_ne!(first.cooks[0].to_worktree, second.cooks[0].to_worktree);
        assert_eq!(second.cooks[0].cook_id, "issue-wave-v2-issue-6453");
    }

    #[test]
    fn implicit_fanout_identity_tracks_effective_cook_inputs() {
        with_materialized_cook_batch_worktrees(|| {
            let mut args = cook_batch_args();
            args.fanout_id = None;
            let first = build_cook_batch_plan(&args).expect("first implicit plan");
            let replay = build_cook_batch_plan(&args).expect("exact replay");
            assert_eq!(first.fanout_id, replay.fanout_id);
            assert_eq!(first.cooks[0].cook_id, replay.cooks[0].cook_id);

            args.branch_prefix = "fix-v2".to_string();
            let changed = build_cook_batch_plan(&args).expect("changed plan");
            assert_ne!(first.fanout_id, changed.fanout_id);
            assert_ne!(first.cooks[0].cook_id, changed.cooks[0].cook_id);
        });
    }

    #[test]
    fn canonical_cook_batch_identity_matches_the_plan_and_its_child_lineage() {
        with_materialized_cook_batch_worktrees(|| {
            let mut args = cook_batch_args();
            args.fanout_id = None;

            let fanout_id = cook_batch_fanout_id(&args).expect("canonical fanout identity");
            let plan = build_cook_batch_plan(&args).expect("canonical fanout plan");

            assert_eq!(fanout_id, plan.fanout_id);
            for cook in plan.cooks {
                assert!(cook.cook_id.starts_with(&format!("{fanout_id}-")));
                assert!(cook.run_id().starts_with(&format!("cook-{fanout_id}-")));
            }
        });
    }

    #[test]
    fn fanout_overrides_rekey_scoped_and_legacy_children_once() {
        let mut plan = test_batch_plan();
        assert_eq!(plan.cooks[0].cook_id, "fanout/refactor-first");

        plan.rekey("replacement".to_string());
        assert_eq!(plan.fanout_id, "replacement");
        assert_eq!(plan.cooks[0].cook_id, "replacement-first");
        assert_eq!(plan.cooks[1].cook_id, "replacement-second");

        let mut legacy = BatchCookFanoutPlan {
            schema: batch_cook_fanout_plan_schema(),
            fanout_id: "legacy".to_string(),
            max_concurrency: None,
            max_duration_seconds: None,
            placement: None,
            cooks: vec![BatchCookSpec {
                cook_id: "issue-6453".to_string(),
                ..plan.cooks[0].clone()
            }],
            metadata: Value::Null,
        };
        legacy.rekey("fresh".to_string());
        assert_eq!(legacy.cooks[0].cook_id, "fresh-issue-6453");
    }

    #[test]
    fn recipe_preflight_accepts_safe_pre_execution_recipe_corrections() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let plan = test_batch_plan();
            let compiled = compile_batch_cooks(&plan, |_| {}).expect("compile batch cooks");
            let mut invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            invocation.options.harvest_context = batch_harvest_context().expect("harvest context");
            invocation.options.identity.initial_plan = compiled[0].identity.initial_plan.clone();
            agent_task_service::persist_initial_recipe(&invocation.options)
                .expect("persist initial recipe");

            if let Err(error) = preflight_batch_cook_recipes(&plan, None) {
                panic!("exact replay is incompatible: {error:?}");
            }

            let mut changed = plan;
            changed.cooks[0].title = Some("changed title".to_string());
            preflight_batch_cook_recipes(&changed, None)
                .expect("pre-execution finalization metadata can be corrected safely");
        });
    }

    #[test]
    fn local_selected_fanout_recipes_remain_local_through_preflight() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let plan = test_batch_plan();

            persist_batch_cook_recipes(&plan, |_| {}).expect("persist local child recipes");
            preflight_batch_cook_recipes(&plan, None).expect("preflight local recipes");

            for cook in &plan.cooks {
                let recipe = agent_task_service::load_recipe(&cook.run_id()).expect("local recipe");
                assert_eq!(
                    recipe.promotion_transport["attempt_dispatch"]["kind"],
                    "local"
                );
                assert!(
                    crate::commands::infra::route::reconstruct_cook_attempt_dispatcher(
                        &recipe.promotion_transport["attempt_dispatch"],
                    )
                    .expect("reconstruct local dispatcher")
                    .is_none()
                );
            }
        });
    }

    #[test]
    fn lab_or_local_fanout_recipes_preserve_the_dispatcher_used_for_execution() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let plan = test_batch_plan();
            let dispatcher = |_: &CookRequest| {
                std::sync::Arc::new(LabRecipeDispatcher)
                    as std::sync::Arc<
                        dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher,
                    >
            };

            persist_batch_cook_recipes(&plan, |options| {
                options.provider_transport.attempt_dispatcher = Some(dispatcher(options));
            })
            .expect("persist Lab child recipes");
            preflight_batch_cook_recipes(&plan, Some(&dispatcher))
                .expect("preflight Lab recipes with their dispatcher");
            let compiled = compile_batch_cooks(&plan, |_| {}).expect("compile child options");

            for (cook, options) in plan.cooks.iter().zip(&compiled) {
                let recipe = agent_task_service::load_recipe(&cook.run_id()).expect("Lab recipe");
                assert_eq!(
                    recipe.promotion_transport["attempt_dispatch"]["kind"],
                    "lab"
                );
                assert_eq!(
                    recipe.promotion_transport["attempt_dispatch"]["allow_local_fallback"],
                    true
                );
                assert!(
                    agent_task_service::reconstruct_options_with_dispatcher(&recipe, None).is_err()
                );
                assert!(agent_task_service::reconstruct_options_with_dispatcher(
                    &recipe,
                    Some(dispatcher(options)),
                )
                .is_ok());
            }
        });
    }

    #[test]
    fn executing_batch_fails_early_for_a_backend_without_a_provider() {
        // #7717: an executing batch whose backend cannot be served by any
        // installed provider must fail early with an actionable configuration
        // error, not ride the backend all the way to a late provider-shaped
        // child failure. The test environment installs no providers, so any
        // backend is unresolved — which is exactly the "no installed provider"
        // path.
        let mut args = cook_batch_args();
        args.backend = Some("codebox-nonexistent".to_string());
        args.selector = None;
        args.preview = false;
        args.run_plan = true;

        let catalog = AgentTaskProviderCatalog::default();
        let error = resolve_and_validate_effective_backend_with_catalog(&mut args, &catalog)
            .expect_err("an executing batch with an unresolved backend must fail early");
        assert_eq!(error.details["field"], "backend");
        assert!(
            error.message.contains("codebox-nonexistent")
                && error.message.contains("no installed provider"),
            "error must name the backend and the missing provider: {}",
            error.message
        );
        assert!(error.details["tried"]
            .as_array()
            .is_some_and(|tried| !tried.is_empty()));
    }

    #[test]
    fn loaded_run_plan_admits_every_child_before_coordination() {
        let mut plan = test_batch_plan();
        for cook in &mut plan.cooks {
            cook.backend = Some("unavailable-loaded-plan-backend".to_string());
            cook.selector = None;
            cook.secret_env.clear();
        }

        let error = admit_batch_provider_routes_with_catalog(
            &mut plan,
            &AgentTaskProviderCatalog::default(),
        )
        .expect_err("a loaded plan must be admitted before coordinator effects");

        assert_eq!(error.details["field"], "backend");
        assert_eq!(error.details["id"], "unavailable-loaded-plan-backend");
        assert_eq!(error.details["_homeboy_actions"][0]["safety"], "read_only");
    }

    #[test]
    fn executing_batch_names_selector_mismatch_for_an_installed_backend() {
        let mut args = cook_batch_args();
        args.backend = Some("opencode".to_string());
        args.selector = Some("dmc".to_string());
        args.preview = false;
        args.run_plan = true;
        let catalog = AgentTaskProviderCatalog {
            providers: vec![serde_json::from_value(serde_json::json!({
                "id": "opencode.agent-task-executor",
                "backend": "opencode",
                "extension_id": "opencode.extension",
                "runtime_id": "opencode-runtime",
            }))
            .expect("provider fixture")],
            ..AgentTaskProviderCatalog::default()
        };

        let error = resolve_and_validate_effective_backend_with_catalog(&mut args, &catalog)
            .expect_err("an unknown selector must fail early");

        assert_eq!(error.details["field"], "selector");
        assert_eq!(error.details["id"], "dmc");
        assert!(error.details["tried"][0]
            .as_str()
            .is_some_and(|hint| hint.contains("opencode.agent-task-executor")));
    }

    #[test]
    fn dry_run_and_live_provider_admission_reject_the_same_unavailable_backend() {
        let catalog = AgentTaskProviderCatalog::default();
        let mut dry_run = cook_batch_args();
        dry_run.backend = Some("sandbox".to_string());
        dry_run.preview = true;
        dry_run.run_plan = false;
        let mut live = dry_run.clone();
        live.preview = false;
        live.run_plan = true;

        let dry_error = resolve_and_validate_effective_backend_with_catalog(&mut dry_run, &catalog)
            .expect_err("dry-run must reject an unavailable backend");
        let live_error = resolve_and_validate_effective_backend_with_catalog(&mut live, &catalog)
            .expect_err("live admission must reject an unavailable backend");

        assert_eq!(dry_error.code, live_error.code);
        assert_eq!(dry_error.message, live_error.message);
        assert_eq!(dry_error.details, live_error.details);
        assert_eq!(dry_error.details["field"], "backend");
        assert!(dry_error.details["tried"].is_array());
    }

    #[test]
    fn dry_run_and_live_provider_admission_reject_the_same_unsupported_model() {
        let catalog = AgentTaskProviderCatalog {
            providers: vec![serde_json::from_value(serde_json::json!({
                "id": "opencode.agent-task-executor",
                "backend": "opencode",
                "cli": {
                    "profiles": [{ "name": "sol", "model": "openai/gpt-5.6-sol" }]
                }
            }))
            .expect("provider fixture")],
            ..AgentTaskProviderCatalog::default()
        };
        let mut dry_run = cook_batch_args();
        dry_run.backend = Some("opencode".to_string());
        dry_run.selector = None;
        dry_run.model = Some("openai/unsupported".to_string());
        dry_run.secret_env.clear();
        dry_run.preview = true;
        dry_run.run_plan = false;
        let mut live = dry_run.clone();
        live.preview = false;
        live.run_plan = true;

        let dry_error = resolve_and_validate_effective_backend_with_catalog(&mut dry_run, &catalog)
            .expect_err("dry-run must reject an unsupported model");
        let live_error = resolve_and_validate_effective_backend_with_catalog(&mut live, &catalog)
            .expect_err("live admission must reject an unsupported model");

        assert_eq!(dry_error.code, live_error.code);
        assert_eq!(dry_error.message, live_error.message);
        assert_eq!(dry_error.details, live_error.details);
        assert_eq!(dry_error.details["field"], "model");
        assert!(dry_error.details["tried"][0]
            .as_str()
            .is_some_and(|hint| hint.contains("openai/gpt-5.6-sol")));
    }

    #[test]
    fn dry_run_and_live_provider_admission_reject_the_same_missing_declared_credential() {
        with_isolated_home(|_| {
            let catalog = AgentTaskProviderCatalog {
                providers: vec![serde_json::from_value(serde_json::json!({
                    "id": "opencode.agent-task-executor",
                    "backend": "opencode"
                }))
                .expect("provider fixture")],
                ..AgentTaskProviderCatalog::default()
            };
            let mut dry_run = cook_batch_args();
            dry_run.backend = Some("opencode".to_string());
            dry_run.selector = None;
            dry_run.model = None;
            dry_run.secret_env = vec!["HOMEBOY_TEST_FANOUT_MISSING_CREDENTIAL_13589".to_string()];
            dry_run.preview = true;
            dry_run.run_plan = false;
            let mut live = dry_run.clone();
            live.preview = false;
            live.run_plan = true;

            let dry_error =
                resolve_and_validate_effective_backend_with_catalog(&mut dry_run, &catalog)
                    .expect_err("dry-run must reject a missing declared credential");
            let live_error =
                resolve_and_validate_effective_backend_with_catalog(&mut live, &catalog)
                    .expect_err("live admission must reject a missing declared credential");

            assert_eq!(dry_error.code, live_error.code);
            assert_eq!(dry_error.message, live_error.message);
            assert_eq!(dry_error.details, live_error.details);
            assert_eq!(dry_error.details["field"], "secret_env");
        });
    }

    #[test]
    fn dry_run_and_live_provider_admission_reject_the_same_missing_readiness_invocation() {
        let catalog = AgentTaskProviderCatalog {
            providers: vec![serde_json::from_value(serde_json::json!({
                "id": "opencode.agent-task-executor",
                "backend": "opencode",
                "capabilities": ["cli_runtime", "provider_owned_auth"],
                "cli": {
                    "profiles": [{ "name": "terra", "model": "openai/gpt-5.6-terra" }]
                }
            }))
            .expect("provider fixture")],
            ..AgentTaskProviderCatalog::default()
        };
        let mut dry_run = cook_batch_args();
        dry_run.backend = Some("opencode".to_string());
        dry_run.selector = None;
        dry_run.model = Some("openai/gpt-5.6-terra".to_string());
        dry_run.secret_env.clear();
        dry_run.preview = true;
        dry_run.run_plan = false;
        let mut live = dry_run.clone();
        live.preview = false;
        live.run_plan = true;

        let dry_error = resolve_and_validate_effective_backend_with_catalog(&mut dry_run, &catalog)
            .expect_err("preview must reject a provider-owned auth contract without readiness");
        let live_error = resolve_and_validate_effective_backend_with_catalog(&mut live, &catalog)
            .expect_err("run-plan admission must reject the same provider contract");

        assert_eq!(dry_error.code, live_error.code);
        assert_eq!(dry_error.message, live_error.message);
        assert_eq!(dry_error.details, live_error.details);
        assert_eq!(dry_error.details["field"], "provider_dispatchability");
        assert!(
            dry_error.message.contains("missing_readiness_invocation")
                || dry_error.message.contains("readiness invocation")
        );
    }

    #[test]
    fn dry_run_and_live_provider_admission_share_typed_missing_default_remediation() {
        with_isolated_home(|_| {
            let catalog = AgentTaskProviderCatalog {
                providers: vec![serde_json::from_value(serde_json::json!({
                    "id": "opencode.agent-task-executor",
                    "backend": "opencode",
                    "extension_id": "opencode.extension",
                    "runtime_id": "opencode-runtime",
                }))
                .expect("provider fixture")],
                ..AgentTaskProviderCatalog::default()
            };
            let mut dry_run = cook_batch_args();
            dry_run.repo = "unregistered-provider-parity".to_string();
            dry_run.backend = None;
            dry_run.preview = true;
            dry_run.run_plan = false;
            let mut live = dry_run.clone();
            live.preview = false;
            live.run_plan = true;

            let dry_error =
                resolve_and_validate_effective_backend_with_catalog(&mut dry_run, &catalog)
                    .expect_err("dry-run must require backend selection");
            let live_error =
                resolve_and_validate_effective_backend_with_catalog(&mut live, &catalog)
                    .expect_err("live admission must require backend selection");

            assert_eq!(dry_error.code, live_error.code);
            assert_eq!(dry_error.message, live_error.message);
            assert_eq!(dry_error.details, live_error.details);
            assert_eq!(dry_error.details["field"], "backend");
            assert_eq!(dry_error.details["selection_required"], true);
            assert_eq!(
                dry_error.details["_homeboy_actions"][0]["program"],
                "homeboy"
            );
            assert_eq!(
                dry_error.details["_homeboy_actions"][0]["args"],
                serde_json::json!(["agent-task", "providers", "--validate-readiness"])
            );
            assert!(dry_error.details["tried"]
                .as_array()
                .is_some_and(|tried| tried
                    .iter()
                    .any(|hint| hint.as_str().is_some_and(|hint| hint.contains("--backend")))));
        });
    }

    #[test]
    fn fanout_resolves_implicit_backend_from_exact_component_policy() {
        let mut args = cook_batch_args();
        args.repo = "blocks-engine".to_string();
        args.component = Some("php-transformer".to_string());
        args.backend = None;
        args.selector = None;
        args.secret_env.clear();
        args.preview = true;
        args.run_plan = false;

        let catalog = AgentTaskProviderCatalog {
            providers: vec![serde_json::from_value(serde_json::json!({
                "id": "component-policy.agent-task-executor",
                "backend": "component-policy"
            }))
            .expect("provider fixture")],
            ..AgentTaskProviderCatalog::default()
        };
        resolve_and_validate_effective_backend_with_catalog_and_default(
            &mut args,
            &catalog,
            |component| {
                assert_eq!(component, Some("php-transformer"));
                Ok(Some("component-policy".to_string()))
            },
        )
        .expect("resolve component-scoped backend");

        assert_eq!(args.backend.as_deref(), Some("component-policy"));
    }

    #[test]
    fn dry_run_and_live_plan_reject_the_same_undeclared_prompt_path() {
        let mut args = cook_batch_args();
        args.prompt_template = Some("Read /private/results.json for {issue_ref}.".to_string());

        let dry_run = build_static_cook_batch_plan(&args)
            .expect_err("dry-run must validate undeclared prompt paths");
        let live = build_cook_batch_plan(&args)
            .expect_err("live planning must reject the same prompt path");

        assert_eq!(dry_run.code, live.code);
        assert_eq!(dry_run.message, live.message);
        assert_eq!(dry_run.details, live.details);
    }

    #[test]
    fn dry_run_replay_pins_fanout_child_and_worktree_owners() {
        with_materialized_cook_batch_worktrees(|| {
            let mut args = cook_batch_args();
            args.fanout_id = None;

            let dry_plan = build_static_cook_batch_plan(&args).expect("dry-run plan");
            let replay = pin_cook_batch_replay(&args, &dry_plan.fanout_id);
            let live_plan = build_cook_batch_plan(&replay).expect("replayed live plan");

            assert_eq!(live_plan.fanout_id, dry_plan.fanout_id);
            assert_eq!(
                live_plan
                    .cooks
                    .iter()
                    .map(|cook| (&cook.cook_id, cook.run_id(), &cook.to_worktree, &cook.head))
                    .collect::<Vec<_>>(),
                dry_plan
                    .cooks
                    .iter()
                    .map(|cook| (&cook.cook_id, cook.run_id(), &cook.to_worktree, &cook.head))
                    .collect::<Vec<_>>(),
                "replay must retain each child cook, worktree owner, and branch identity"
            );
            assert!(cook_batch_run_command(&replay)
                .contains(&format!("--fanout-id {}", dry_plan.fanout_id)));
            assert_eq!(
                batch_plan_reference(&dry_plan).expect("plan reference"),
                batch_plan_reference(&live_plan).expect("plan reference"),
                "the replayed plan must retain its immutable input digest"
            );
        });
    }

    #[test]
    fn cook_batch_preserves_explicit_prompt_template_for_manual_pr_modes() {
        let mut args = cook_batch_args();
        args.prompt_template = Some(
            "Fix {issue_ref} on {branch}; push manually and open the pull request.".to_string(),
        );

        let plan = build_cook_batch_plan(&args).expect("cook batch plan");

        assert_eq!(
            plan.cooks[0].prompt.as_deref(),
            Some(
                "Fix Extra-Chill/homeboy#6453 on fix/issue-6453-homeboy; push manually and open the pull request."
            )
        );
    }

    #[test]
    fn cook_batch_dry_run_returns_status_and_resume_commands() {
        with_materialized_cook_batch_worktrees(|| {
            let args = cook_batch_args();
            let (value, exit_code) = cook_batch(args).expect("cook batch dry run");

            assert_eq!(exit_code, 0, "{value}");
            assert_eq!(value["schema"], "homeboy/agent-task-cook-batch/v1");
            assert_eq!(value["status"], "ready");
            assert_eq!(value["summary"]["issues"], 2);
            assert_eq!(
                value["preflight"]["provider_selection"]["executor"]["backend"],
                "sandbox"
            );
            assert_eq!(value["preflight"]["provider_selection"]["model"], "gpt-5.5");
            assert_eq!(
                value["preflight"]["provider_selection"]["provider_config"],
                "provided"
            );
            assert_eq!(value["worktrees"]["dry_run"], true);
            assert_eq!(value["worktrees"]["rows"][0]["status"], "would_create");
            assert_eq!(value["plan_ref"]["fanout_id"], value["fanout_id"]);
            assert!(value["plan_ref"]["sha256"]
                .as_str()
                .expect("plan digest")
                .starts_with("sha256:"));
            assert!(value["commands"]["run"]
                .as_str()
                .expect("pinned run command")
                .contains("--fanout-id issue-wave"));
            assert!(value["commands"]["resume_from_plan"]
                .as_str()
                .expect("resume command")
                .contains("fanout run-plan"));
            // The prose entries are gone: a typed next action covers both.
            assert!(
                value["commands"]["status"].is_null(),
                "prose `status` sentence must not survive beside typed actions"
            );
            assert!(
                value["commands"]["retry"].is_null(),
                "prose `retry` sentence must not survive beside typed actions"
            );
            let actions = value["next_actions"]
                .as_array()
                .expect("typed next actions")
                .clone();
            assert!(!actions.is_empty());
            for action in &actions {
                assert!(
                    action["command"]
                        .as_str()
                        .expect("every next action names a command")
                        .starts_with("homeboy "),
                    "next action must be executable, got {action}"
                );
                assert!(action["kind"].is_string(), "next action must carry a kind");
            }
        });
    }

    #[test]
    fn dry_run_projects_static_worktrees_once_for_output_and_next_actions() {
        with_materialized_cook_batch_worktrees(|| {
            STATIC_WORKTREE_PROJECTIONS.with(|count| count.set(0));

            let (value, exit_code) = cook_batch(cook_batch_args()).expect("cook batch dry run");

            assert_eq!(exit_code, 0, "{value}");
            assert_eq!(
                STATIC_WORKTREE_PROJECTIONS.with(|count| count.get()),
                1,
                "next actions must reuse the response worktree projection"
            );
        });
    }

    #[test]
    fn dry_run_plans_absent_worktrees_without_creating_them() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let parent = home.path().join("Developer");
            let source = parent.join("fanout-dry-run-fixture");
            std::fs::create_dir_all(&source).expect("source directory");
            std::fs::write(
                source.join("homeboy.json"),
                r#"{"id":"fanout-dry-run-fixture"}"#,
            )
            .expect("component manifest");
            for args in [
                ["init", "-b", "main"].as_slice(),
                ["config", "user.email", "test@example.com"].as_slice(),
                ["config", "user.name", "Homeboy Test"].as_slice(),
                ["commit", "--allow-empty", "-m", "initial"].as_slice(),
            ] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .unwrap()
                    .success());
            }
            init_git_primary(&source);
            write_component_registration(home.path(), "fanout-dry-run-fixture", &source);

            let mut args = cook_batch_args();
            args.repo = "fanout-dry-run-fixture".to_string();
            args.from = Some("HEAD".to_string());
            args.gates.verify = vec!["shared-check".to_string()];
            args.verification_profiles = Some(
                serde_json::json!({
                    "profiles": {
                        "append": { "plan": { "adapter": "homeboy_review_test", "command": ["homeboy", "review", "test"], "suite_timeout_seconds": 30 } },
                        "replace": { "plan": { "adapter": "homeboy_review_test", "command": ["homeboy", "review", "test", "fixture"], "suite_timeout_seconds": 30 } }
                    },
                    "assignments": [
                        { "selector": "Extra-Chill/homeboy#6453", "profile": "append" },
                        { "selector": "https://github.com/Extra-Chill/homeboy/issues/6454", "profile": "replace" }
                    ]
                })
                .to_string(),
            );
            let (value, exit_code) = cook_batch(args).expect("dry-run plan");

            assert_eq!(exit_code, 0);
            assert_eq!(value["status"], "ready");
            assert_eq!(
                value["worktrees"]["rows"].as_array().expect("rows").len(),
                2,
                "the deterministic two-child preview retains every child"
            );
            for row in value["worktrees"]["rows"].as_array().expect("rows") {
                assert_eq!(row["status"], "would_create");
                assert!(
                    row["path"].is_null(),
                    "static planning does not probe paths"
                );
            }
            assert!(value["plan"]["cooks"]
                .as_array()
                .expect("cooks")
                .iter()
                .all(|cook| cook["workspace"].is_null()));
            let gates = value["preflight"]["deterministic_gates"]
                .as_array()
                .expect("resolved gate preview");
            assert_eq!(gates[0]["profile"], "append");
            assert_eq!(gates[0]["verify"], json!(["shared-check"]));
            assert_eq!(gates[0]["test_execution_plan"]["suite_timeout_seconds"], 30);
            assert_eq!(
                gates[0]["selectors"],
                json!([
                    "https://github.com/Extra-Chill/homeboy/issues/6453",
                    "Extra-Chill/homeboy#6453",
                    "issue-6453"
                ])
            );
            assert_eq!(gates[1]["profile"], "replace");
            assert_eq!(gates[1]["verify"], json!(["shared-check"]));
            assert_eq!(
                gates[1]["test_execution_plan"]["command"],
                json!(["homeboy", "review", "test", "fixture"])
            );
            assert!(!home
                .path()
                .join(".local/share/homeboy/agent-task-recipes")
                .exists());
            assert!(
                !home
                    .path()
                    .join(".local/share/homeboy/agent-task-batches")
                    .exists(),
                "dry-run must not create a durable batch"
            );
        });
    }

    #[test]
    fn dry_run_planner_enforces_the_slow_worktree_phase_budget() {
        let args = cook_batch_args();
        let mut planner = DryRunPlanner::new(&args, Placement::Auto);
        planner.phase_timeout = Duration::from_millis(20);

        let started = Instant::now();
        let error = planner
            .run_bounded("worktrees", "static worktree projection", || {
                std::thread::sleep(Duration::from_secs(2));
                Ok(())
            })
            .expect_err("slow static worktree phase must be bounded");

        assert!(
            started.elapsed() < Duration::from_secs(1),
            "planner must return at its phase budget: {error}"
        );
        assert_eq!(error.details["reason"], "planner_deadline_exceeded");
        assert_eq!(error.details["phase"], "worktrees");
        assert!(error.details["phase_elapsed_ms"].as_u64().unwrap() >= 20);
        assert_eq!(
            error.details["unresolved_dependency"],
            "static worktree projection"
        );
        assert!(error.details["replay_command"]
            .as_str()
            .expect("replay command")
            .contains("--preview"));
    }

    #[test]
    fn dry_run_planner_attributes_a_slow_workspace_lookup_to_its_exact_phase() {
        let args = cook_batch_args();
        let mut planner = DryRunPlanner::new(&args, Placement::Auto);
        planner.phase_timeout = Duration::from_millis(20);

        let error = planner
            .run_bounded(
                "gate_workspace",
                "authoritative registered workspace",
                || {
                    std::thread::sleep(Duration::from_secs(2));
                    Ok(())
                },
            )
            .expect_err("slow workspace lookup must be bounded");

        assert_eq!(error.details["reason"], "planner_deadline_exceeded");
        assert_eq!(error.details["phase"], "gate_workspace");
        assert!(error.details["phase_elapsed_ms"].as_u64().unwrap() >= 20);
        assert_eq!(
            error.details["unresolved_dependency"],
            "authoritative registered workspace"
        );
    }

    #[test]
    fn dry_run_planner_timeout_is_configurable_and_replayable() {
        let mut args = cook_batch_args();
        args.dry_run_planner_timeout_seconds = Some(42);
        let planner = DryRunPlanner::new(&args, Placement::Auto);

        assert_eq!(planner.phase_timeout, Duration::from_secs(42));
        assert!(dry_run_replay_command(&args).contains("--dry-run-planner-timeout-seconds 42"));
    }

    #[test]
    fn dry_run_rejects_a_blocking_gate_file_without_opening_it() {
        with_isolated_home(|home| {
            let fifo = home.path().join("blocking-gate-input");
            assert!(Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .expect("create blocking fixture")
                .success());
            let mut args = cook_batch_args();
            args.gates.verify_file.push(fifo.display().to_string());

            let started = Instant::now();
            let error = cook_batch(args).expect_err("static dry-run rejects file-backed gates");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "dry-run must return before opening the FIFO: {error}"
            );
            assert_eq!(error.details["reason"], "static_input_required");
            assert_eq!(
                error.details["unresolved_dependency"],
                "file-backed gate or provider evidence input"
            );
            assert!(error.details["replay_command"]
                .as_str()
                .expect("replay command")
                .contains("--preview"));
            assert!(
                !home
                    .path()
                    .join(".local/share/homeboy/agent-task-recipes")
                    .exists(),
                "blocking planning input must not create recipes"
            );
        });
    }

    #[test]
    fn dry_run_bounds_large_issue_lists_before_plan_construction() {
        with_isolated_home(|home| {
            let mut args = cook_batch_args();
            args.issues = (0..=DRY_RUN_MAX_ISSUES)
                .map(|number| format!("https://github.com/Extra-Chill/homeboy/issues/{number}"))
                .collect();

            let started = Instant::now();
            let error = cook_batch(args).expect_err("oversized static issue list");
            assert!(
                started.elapsed() < Duration::from_secs(1),
                "input cap must bound planning before child construction: {error}"
            );
            assert_eq!(error.details["reason"], "static_input_required");
            assert_eq!(error.details["unresolved_dependency"], "bounded issue list");
            assert!(!home
                .path()
                .join(".local/share/homeboy/agent-task-recipes")
                .exists());
        });
    }

    #[test]
    fn dry_run_normalizes_registered_primary_and_validates_static_gate_aliases() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let primary = home.path().join("primary");
            std::fs::create_dir_all(&primary).expect("primary directory");
            git(&primary, &["init", "-b", "main"]);
            git(
                &primary,
                &[
                    "-c",
                    "user.name=Homeboy Test",
                    "-c",
                    "user.email=test@example.com",
                    "commit",
                    "--allow-empty",
                    "-m",
                    "initial",
                ],
            );
            git(
                &primary,
                &["update-ref", "refs/remotes/origin/main", "HEAD"],
            );
            std::fs::write(
                primary.join("homeboy.json"),
                r#"{"scripts":{"lint":["check"],"test":["check"]}}"#,
            )
            .expect("component manifest");
            init_git_primary(&primary);
            write_component_registration(home.path(), "fixture", &primary);

            for gate in [
                "homeboy lint fixture --path .",
                "homeboy test fixture --path .",
            ] {
                let mut args = cook_batch_args();
                args.repo = primary.display().to_string();
                args.gates.verify = vec![gate.to_string()];
                let error = cook_batch(args).expect_err("static gate alias is rejected");
                assert!(
                    error.message.contains("repository script identity"),
                    "{error}"
                );
                assert!(
                    error.details["replay_command"]
                        .as_str()
                        .expect("public replay command")
                        .contains("--repo"),
                    "primary-path input remains replayable"
                );
            }
            assert!(!home
                .path()
                .join(".local/share/homeboy/agent-task-recipes")
                .exists());
        });
    }

    #[test]
    fn public_inline_profile_keeps_an_executable_replay_command() {
        let mut args = cook_batch_args();
        args.verification_profiles = Some(
            r#"{"profiles":{"public":{"plan":{"adapter":"homeboy_review_test","command":["homeboy","review","test"],"suite_timeout_seconds":30}}},"assignments":[]}"#.to_string(),
        );
        assert!(dry_run_replay_command(&args).starts_with("homeboy "));

        let sentinel = "PRIVATE_PROFILE_REPLAY_SENTINEL";
        args.gates.private_verify = vec![sentinel.to_string()];
        assert!(!dry_run_replay_command(&args).contains(sentinel));
    }

    #[test]
    fn dry_run_reuses_existing_worktrees_and_plans_missing_children() {
        with_isolated_home(|home| {
            install_fanout_agent_task_providers(home.path());
            let parent = home.path().join("Developer");
            let source = parent.join("fanout-mixed-fixture");
            std::fs::create_dir_all(&source).expect("source directory");
            std::fs::write(
                source.join("homeboy.json"),
                r#"{"id":"fanout-mixed-fixture"}"#,
            )
            .expect("component manifest");
            for args in [
                ["init", "-b", "main"].as_slice(),
                ["config", "user.email", "test@example.com"].as_slice(),
                ["config", "user.name", "Homeboy Test"].as_slice(),
                ["add", "."].as_slice(),
                ["commit", "-m", "initial"].as_slice(),
            ] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .unwrap()
                    .success());
            }
            init_git_primary(&source);
            write_component_registration(home.path(), "fanout-mixed-fixture", &source);
            worktree::queue_create(worktree::WorktreeQueueCreateOptions {
                repo: "fanout-mixed-fixture".to_string(),
                requests: vec![worktree::WorktreeQueueCreateRequest {
                    branch: "fix/issue-6453-homeboy".to_string(),
                    task_url: None,
                    task_ref: None,
                    run_id: None,
                    provider_lifecycle: None,
                }],
                from: "HEAD".to_string(),
                dry_run: false,
                retry_after_seconds: 30,
            })
            .expect("create existing child worktree");

            let mut args = cook_batch_args();
            args.repo = "fanout-mixed-fixture".to_string();
            args.from = Some("HEAD".to_string());
            let (value, exit_code) = cook_batch(args).expect("mixed dry-run plan");

            assert_eq!(exit_code, 0, "{value}");
            assert_eq!(value["worktrees"]["rows"][0]["status"], "would_create");
            assert_eq!(value["worktrees"]["rows"][1]["status"], "would_create");
        });
    }

    fn worktree_row(
        handle: &str,
        status: worktree::WorktreeQueueCreateStatus,
    ) -> worktree::WorktreeQueueCreateRow {
        worktree::WorktreeQueueCreateRow {
            branch: format!("fix/{handle}"),
            handle: handle.to_string(),
            status,
            command: vec![
                "homeboy".to_string(),
                "worktree".to_string(),
                "create".to_string(),
                "homeboy".to_string(),
                "--branch".to_string(),
                format!("fix/{handle}"),
                "--from".to_string(),
                "origin/main".to_string(),
            ],
            retry_after_seconds: None,
            active_lock_holder: None,
            path: None,
            error: None,
            failure: None,
        }
    }

    fn worktree_output(
        rows: Vec<worktree::WorktreeQueueCreateRow>,
    ) -> worktree::WorktreeQueueCreateOutput {
        worktree::WorktreeQueueCreateOutput {
            schema: "homeboy/worktree-queue-create/v1",
            repo: "homeboy".to_string(),
            base_ref: "origin/main".to_string(),
            dry_run: false,
            rows,
        }
    }

    fn action_commands(actions: &[CommandNextAction]) -> Vec<String> {
        actions
            .iter()
            .map(|action| action.command.clone())
            .collect()
    }

    /// Every emitted action must be a command, not an instruction. This is the
    /// property the prose versions failed.
    fn assert_every_action_is_executable(actions: &[CommandNextAction]) {
        for action in actions {
            assert!(
                action.command.starts_with("homeboy "),
                "next action `{}` is not an executable command",
                action.command
            );
            assert!(
                action.kind.is_some(),
                "next action `{}` carries no kind",
                action.command
            );
        }
    }

    /// A blocked row already carries the exact command that creates it. The
    /// old text — "repair worktree queue blockers reported under
    /// worktrees.rows" — asked a human to go find that command by hand.
    #[test]
    fn blocked_worktrees_emit_the_exact_command_that_unblocks_each_row() {
        let worktrees = worktree_output(vec![
            worktree_row(
                "homeboy@fix-a",
                worktree::WorktreeQueueCreateStatus::Created,
            ),
            worktree_row("homeboy@fix-b", worktree::WorktreeQueueCreateStatus::Failed),
        ]);

        let actions = cook_batch_next_actions(
            &cook_batch_args(),
            "issue-wave",
            "blocked",
            true,
            false,
            &worktrees,
            false,
            None,
        );

        assert_every_action_is_executable(&actions);
        let commands = action_commands(&actions);
        assert!(
            commands.iter().any(|command| command
                == "homeboy worktree create homeboy --branch fix/homeboy@fix-b --from origin/main"),
            "the blocked row's own command must be offered: {commands:?}"
        );
        assert!(
            !commands
                .iter()
                .any(|command| command.contains("fix/homeboy@fix-a")),
            "a created row is not a blocker: {commands:?}"
        );
        assert!(
            commands
                .iter()
                .any(|command| command.contains("--run-plan")),
            "the rerun command must be named: {commands:?}"
        );
        assert!(commands
            .iter()
            .any(|command| command == "homeboy agent-task fanout status issue-wave"));
        assert!(commands
            .iter()
            .any(|command| command == "homeboy agent-task fanout artifacts issue-wave"));
    }

    #[test]
    fn blocked_worktree_projection_orders_distinct_causes_and_bounds_rows() {
        let mut rows = (0..4)
            .map(|index| {
                worktree_row(
                    &format!("homeboy@fix-{index}"),
                    worktree::WorktreeQueueCreateStatus::Failed,
                )
            })
            .collect::<Vec<_>>();
        for (index, row) in rows.iter_mut().enumerate() {
            row.error = Some(format!("cause {index}"));
            row.failure = Some(worktree::WorktreeQueueCreateFailure {
                code: "validation.invalid_argument".to_string(),
                classification: format!("cause_{index}"),
                phase: if index % 2 == 0 {
                    "worktree_provider_ensure"
                } else {
                    "worktree_provider_resolve"
                }
                .to_string(),
                message: format!("cause {index}"),
                provider_id: Some(format!("provider-{index}")),
                details: serde_json::json!({"complete": index}),
            });
        }

        let (primary, projection) = blocked_worktree_failure_projection(&worktree_output(rows));
        let primary = primary.expect("primary failure");

        assert_eq!(projection["total"], 4);
        assert_eq!(projection["returned"], COMPACT_FANOUT_FAILURE_LIMIT);
        assert_eq!(projection["omitted"], 1);
        assert_eq!(projection["complete_evidence_path"], "worktrees.rows");
        assert_eq!(primary["row"], 0);
        assert_eq!(primary["provider_id"], "provider-0");
        assert_eq!(
            projection["additional_failures"][0]["classification"],
            "cause_1"
        );
        assert_eq!(
            projection["additional_failures"][0]["cause_phase"],
            "worktree_provider_resolve"
        );
        assert_eq!(
            primary["next_action"],
            "homeboy worktree create homeboy --branch fix/homeboy@fix-0 --from origin/main"
        );
    }

    #[test]
    fn identical_child_pre_provider_failures_form_one_complete_primary_cause() {
        let cooks = (1..=3)
            .map(|index| {
                serde_json::json!({
                    "cook_id": format!("cook-{index}"),
                    "initial_run_id": format!("cook-{index}-attempt-1"),
                    "status": "failed",
                    "exit_code": 1,
                    "result": {
                        "latest_run_id": format!("cook-{index}-replacement"),
                        "terminal_phase": "committed_harvest_preflight",
                        "terminal_failure_classification": "agent_task.committed_harvest_dirty_workspace",
                        "failure_context": {
                            "phase": "committed_harvest_preflight",
                            "reason_code": "agent_task.committed_harvest_dirty_workspace",
                            "diagnostic": {
                                "class": "agent_task.committed_harvest_dirty_workspace",
                                "message": "refusing committed-change harvest from a workspace with pre-existing uncommitted changes"
                            },
                            "provider_budget_consumed": false,
                            "provider_executions_consumed": 0,
                            "recovery_legal": true,
                            "next_actions": [{
                                "action": "repair",
                                "command": format!("homeboy agent-task cook-continue cook-{index}")
                            }]
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        let run_result = serde_json::json!({
            "result": {
                "status": "failed",
                "cooks": cooks,
            }
        });

        let (primary, causal) = fanout_failure_projection(
            &worktree_output(vec![worktree_row(
                "homeboy@fix-a",
                worktree::WorktreeQueueCreateStatus::Created,
            )]),
            Some(&run_result),
        );
        let primary = primary.expect("shared primary failure");

        assert_eq!(
            primary["classification"],
            "agent_task.committed_harvest_dirty_workspace"
        );
        assert_eq!(primary["phase"], "committed_harvest_preflight");
        assert_eq!(primary["provider_budget_consumed"], false);
        assert_eq!(primary["provider_executions_consumed"], 0);
        assert_eq!(primary["affected_child_count"], 3);
        assert_eq!(primary["child_references"].as_array().unwrap().len(), 3);
        assert_eq!(
            primary["child_references"][0]["evidence_ref"],
            "homeboy://agent-task/run/cook-1-replacement/status"
        );
        assert_eq!(
            primary["child_references"][2]["diagnose_command"],
            "homeboy agent-task diagnose cook-3-replacement --full"
        );
        assert_eq!(
            primary["recovery"]["command"],
            "homeboy agent-task cook-continue cook-1"
        );
        assert_eq!(causal["total"], 3);
        assert_eq!(causal["returned"], 3);
        assert_eq!(causal["omitted"], 0);
        assert_eq!(causal["unique_causes"], 1);
        assert_eq!(causal["additional_failures"], serde_json::json!([]));
        assert_eq!(causal["complete_evidence_path"], "run_result.result.cooks");
    }

    #[test]
    fn identical_child_failures_group_across_distinct_stop_reasons_and_wire_run_ids() {
        let cooks = (1..=3)
            .map(|index| {
                serde_json::json!({
                    "cook_id": format!("cook-{index}"),
                    "run_id": format!("cook-{index}-attempt-1"),
                    "status": "failed",
                    "exit_code": 1,
                    "result": {
                        "latest_run_id": format!("cook-{index}-replacement"),
                        "stop_reason": format!("agent-task run cook-{index}-replacement ended in state Failed"),
                        "failure_context": {
                            "phase": "provider",
                            "reason_code": "failed",
                            "diagnostic": {
                                "class": "agent_task.committed_harvest_dirty_workspace",
                                "message": "refusing committed-change harvest from a workspace with pre-existing uncommitted changes"
                            },
                            "provider_budget_consumed": false,
                            "provider_executions_consumed": 0,
                            "next_actions": [{
                                "action": "diagnose",
                                "command": format!("homeboy agent-task diagnose cook-{index}-replacement")
                            }]
                        }
                    }
                })
            })
            .collect::<Vec<_>>();
        let run_result = serde_json::json!({ "result": { "status": "failed", "cooks": cooks } });

        let (primary, causal) = fanout_failure_projection(
            &worktree_output(vec![worktree_row(
                "homeboy@fix-a",
                worktree::WorktreeQueueCreateStatus::Created,
            )]),
            Some(&run_result),
        );
        let primary = primary.expect("shared primary failure");

        assert_eq!(primary["phase"], "committed_harvest_preflight");
        assert_eq!(
            primary["classification"],
            "agent_task.committed_harvest_dirty_workspace"
        );
        assert_eq!(primary["affected_child_count"], 3);
        assert_eq!(causal["unique_causes"], 1);
        assert_eq!(primary["child_references"][1]["run_id"], "cook-2-attempt-1");
    }

    #[test]
    fn blocked_worktrees_remain_the_primary_cause_when_children_also_failed() {
        let mut row = worktree_row("homeboy@fix-a", worktree::WorktreeQueueCreateStatus::Failed);
        row.error = Some("worktree create failed".to_string());
        let run_result = serde_json::json!({
            "result": {
                "status": "failed",
                "cooks": [{
                    "cook_id": "cook-1",
                    "run_id": "cook-1",
                    "exit_code": 1,
                    "result": {
                        "latest_run_id": "cook-1",
                        "terminal_phase": "committed_harvest_preflight",
                        "terminal_failure_classification": "agent_task.committed_harvest_dirty_workspace",
                        "failure_context": {
                            "phase": "committed_harvest_preflight",
                            "reason_code": "agent_task.committed_harvest_dirty_workspace",
                            "diagnostic": {
                                "class": "agent_task.committed_harvest_dirty_workspace",
                                "message": "refusing committed-change harvest from a workspace with pre-existing uncommitted changes"
                            },
                            "provider_budget_consumed": false,
                            "provider_executions_consumed": 0,
                            "next_actions": [{"action": "repair", "command": "homeboy agent-task cook-continue cook-1"}]
                        }
                    }
                }]
            }
        });

        let (primary, causal) =
            fanout_failure_projection(&worktree_output(vec![row]), Some(&run_result));
        let primary = primary.expect("worktree primary failure");

        assert_eq!(primary["phase"], "worktree_preflight");
        assert_eq!(causal["complete_evidence_path"], "worktrees.rows");
    }

    #[test]
    fn blocked_workspace_owner_action_round_trips_complete_argv() {
        let owner_argv = vec![
            "workspace-owner".to_string(),
            "worktree".to_string(),
            "add".to_string(),
            "blocks-engine".to_string(),
            "fix/issue-855-blocks-engine".to_string(),
            "--from=origin/trunk".to_string(),
            "--task-url=https://example.test/issues/855".to_string(),
        ];
        let mut row = worktree_row(
            "blocks-engine@fix-issue-855-blocks-engine",
            worktree::WorktreeQueueCreateStatus::Failed,
        );
        row.command = owner_argv.clone();
        let actions = cook_batch_next_actions(
            &cook_batch_args(),
            "issue-wave",
            "blocked",
            false,
            false,
            &worktree_output(vec![row]),
            false,
            None,
        );
        let command = &actions[0].command;

        assert_eq!(command, &quote_args(&owner_argv));
        assert_eq!(
            homeboy_engine_primitives::shell::normalize_args(&vec![command.clone()]),
            owner_argv,
            "the rendered repair command must round-trip to the registered owner's argv"
        );
        assert!(!command.starts_with("homeboy homeboy "));
    }

    /// Named run-plans persist before preflight, while dry plans have no durable
    /// batch record to inspect.
    #[test]
    fn batch_record_commands_are_only_offered_once_the_batch_has_run() {
        let worktrees = worktree_output(vec![worktree_row(
            "homeboy@fix-a",
            worktree::WorktreeQueueCreateStatus::Created,
        )]);

        let planned = cook_batch_next_actions(
            &cook_batch_args(),
            "issue-wave",
            "planned",
            false,
            false,
            &worktrees,
            false,
            None,
        );
        assert_every_action_is_executable(&planned);
        assert!(
            !action_commands(&planned)
                .iter()
                .any(|command| command.contains("fanout status")
                    || command.contains("fanout resume")
                    || command.contains("fanout artifacts")),
            "a plan that never ran has no batch record to inspect"
        );

        let executed = cook_batch_next_actions(
            &cook_batch_args(),
            "issue-wave",
            "partial_failure",
            true,
            true,
            &worktrees,
            false,
            None,
        );
        assert_every_action_is_executable(&executed);
        let commands = action_commands(&executed);
        assert!(commands
            .iter()
            .any(|command| command == "homeboy agent-task fanout status issue-wave"));
        assert!(commands
            .iter()
            .any(|command| command == "homeboy agent-task fanout artifacts issue-wave"));
        assert!(
            commands
                .iter()
                .any(|command| command == "homeboy agent-task fanout resume issue-wave"),
            "an incomplete batch must offer resume: {commands:?}"
        );
    }

    /// Resume harvests children that stopped short of finalization. A batch
    /// that already succeeded has none, so offering it would be noise.
    #[test]
    fn a_succeeded_batch_is_not_offered_a_resume() {
        let worktrees = worktree_output(vec![worktree_row(
            "homeboy@fix-a",
            worktree::WorktreeQueueCreateStatus::Created,
        )]);

        let actions = cook_batch_next_actions(
            &cook_batch_args(),
            "issue-wave",
            "succeeded",
            true,
            false,
            &worktrees,
            false,
            None,
        );

        assert_every_action_is_executable(&actions);
        assert!(!action_commands(&actions)
            .iter()
            .any(|command| command.contains("fanout resume")));
    }

    #[test]
    fn batch_recovery_projection_requires_legal_recovery_for_every_incomplete_child() {
        let mut result = json!({
            "cooks": [
                {
                    "status": "review_ready",
                    "result": {}
                },
                {
                    "status": "failed",
                    "result": {
                        "failure_context": {
                            "recovery_legal": true,
                            "recovery_reason": "durable recipe and lifecycle record permit recovery"
                        }
                    }
                }
            ]
        });
        assert!(batch_resume_is_legal(&result));

        result["cooks"][1]["result"]["failure_context"]["recovery_legal"] = json!(false);
        assert!(!batch_resume_is_legal(&result));

        result["cooks"][1]["result"]["failure_context"]["recovery_legal"] = json!(true);
        assert!(batch_resume_is_legal(&result));
    }

    #[test]
    fn batch_cook_cli_envelope_preserves_partial_failure_and_nonzero_exit() {
        let plan = test_batch_plan();
        let result = agent_task_service::AgentTaskRunResult {
            exit_code: 1,
            value: agent_task_service::AgentTaskCookBatchReport {
                schema: "homeboy/agent-task-cook-batch/v1",
                batch_id: plan.fanout_id.clone(),
                status: "partial_failure".to_string(),
                total: 2,
                queued: 0,
                running: 0,
                succeeded: 1,
                failed: 1,
                cancelled: 0,
                timed_out: 0,
                cooks: vec![
                    agent_task_service::AgentTaskCookBatchCellReport {
                        cook_id: "first".to_string(),
                        initial_run_id: "first-run".to_string(),
                        status: "review_ready".to_string(),
                        exit_code: 0,
                        result: None,
                        error: None,
                    },
                    agent_task_service::AgentTaskCookBatchCellReport {
                        cook_id: "second".to_string(),
                        initial_run_id: "second-run".to_string(),
                        status: "failed".to_string(),
                        exit_code: 1,
                        result: None,
                        error: Some(agent_task_service::AgentTaskCookCellError::declared(
                            "agent_task.controller_admission_denied",
                            "controller admission failed",
                            false,
                        )),
                    },
                ],
            },
        };
        let mut all_failed_report = result.value.clone();
        all_failed_report.status = "failed".to_string();
        all_failed_report.succeeded = 0;
        all_failed_report.failed = 2;
        for cell in &mut all_failed_report.cooks {
            cell.exit_code = 1;
        }
        let mut active_failed_report = result.value.clone();
        active_failed_report.status = "running".to_string();
        active_failed_report.queued = 0;
        active_failed_report.running = 1;
        active_failed_report.succeeded = 0;
        active_failed_report.failed = 1;
        active_failed_report.cooks[0].status = "in_flight".to_string();
        let (data, exit_code) = batch_cook_result(&plan, result, &test_concurrency_decision());
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &Ok(data),
            exit_code,
            "agent-task fanout cook-batch",
            None,
        );

        assert_eq!(exit_code, 1);
        assert!(!envelope.success);
        assert_eq!(envelope.exit_code, 1);
        assert_eq!(envelope.status, "partial_failure");
        assert_eq!(
            envelope.data.expect("batch data")["status"],
            "partial_failure"
        );

        let (data, exit_code) = batch_cook_result(
            &plan,
            agent_task_service::AgentTaskRunResult {
                exit_code: 1,
                value: all_failed_report,
            },
            &test_concurrency_decision(),
        );
        let envelope = crate::commands::utils::response::cli_response_for_json_result_for_command(
            &Ok(data),
            exit_code,
            "agent-task fanout cook-batch",
            None,
        );
        assert_eq!(exit_code, 1);
        assert!(!envelope.success);
        assert_eq!(envelope.status, "failed");

        let (immediate, exit_code) = batch_cook_result(
            &plan,
            agent_task_service::AgentTaskRunResult {
                exit_code: 0,
                value: active_failed_report.clone(),
            },
            &test_concurrency_decision(),
        );
        let (resumed, resume_exit_code) =
            batch_resume_result(active_failed_report, 0, "test-batch", None, Placement::Auto);
        for (name, value, code) in [
            ("immediate", immediate, exit_code),
            ("resume", resumed, resume_exit_code),
        ] {
            assert_eq!(value["status"], "running", "{name}");
            assert_eq!(code, 0, "{name}");
            let summary = &value["summary"];
            assert_eq!(
                summary["queued"].as_u64().unwrap_or_default()
                    + summary["running"].as_u64().unwrap_or_default()
                    + summary["succeeded"].as_u64().unwrap_or_default()
                    + summary["failed"].as_u64().unwrap_or_default()
                    + summary["cancelled"].as_u64().unwrap_or_default()
                    + summary["timed_out"].as_u64().unwrap_or_default(),
                summary["total"].as_u64().unwrap_or_default(),
                "{name}"
            );
        }
    }

    #[test]
    fn cook_batch_outer_exit_code_uses_completed_child_outcome() {
        assert_eq!(cook_batch_outer_exit_code(0, &None), 0);
        assert_eq!(
            cook_batch_outer_exit_code(0, &Some(json!({ "exit_code": 1 }))),
            1
        );
        assert_eq!(
            cook_batch_outer_exit_code(1, &Some(json!({ "exit_code": 0 }))),
            1
        );
    }

    #[test]
    fn durable_batch_status_envelope_preserves_canonical_terminal_state() {
        for state in [
            batch::AgentTaskBatchState::Queued,
            batch::AgentTaskBatchState::Running,
            batch::AgentTaskBatchState::Succeeded,
            batch::AgentTaskBatchState::PartialFailure,
            batch::AgentTaskBatchState::Failed,
            batch::AgentTaskBatchState::Cancelled,
            batch::AgentTaskBatchState::TimedOut,
        ] {
            // `fanout status` is a read: it exits 0 and reports `succeeded`
            // whenever the projection returned, regardless of the batch's own
            // terminal state (#13702). The subject state stays in `data`.
            let envelope =
                crate::commands::utils::response::cli_response_for_json_result_for_identity(
                    &Ok(json!({
                        "schema": "homeboy/agent-task-fanout-status/v2",
                        "batch": { "status": state.outcome_status(), "batch": { "state": state.outcome_status() } }
                    })),
                    0,
                    &crate::commands::utils::response::CommandIdentity::with_operation(
                        "agent-task",
                        "fanout status",
                    ),
                    None,
                );

            assert!(envelope.success, "{state:?}");
            assert_eq!(envelope.exit_code, 0, "{state:?}");
            assert_eq!(envelope.status, "succeeded", "{state:?}");
            assert_eq!(
                envelope.data.expect("durable status")["batch"]["batch"]["state"],
                state.outcome_status(),
                "{state:?}"
            );
        }
    }

    /// #13702 direction 2 regression: reading a FAILED batch is a successful
    /// read. The documented recovery path (`next_actions` prints exactly this
    /// command) must survive `set -e`.
    #[test]
    fn batch_status_read_of_a_failed_batch_is_a_successful_operation() {
        with_isolated_home(|_| {
            let batch_id = "read-of-failed-batch";
            batch::persist_fanout_run_batch(
                batch_id,
                batch_id,
                &[batch::FanoutRunBatchChild {
                    task_id: "child".to_string(),
                    run_id: "missing-child-record".to_string(),
                }],
                json!({}),
            )
            .expect("persist fanout batch");
            let claim_id = batch::claim_fanout_run_batch(batch_id)
                .expect("claim batch")
                .expect("coordinator claim");
            batch::record_fanout_run_batch_failure(
                batch_id,
                &claim_id,
                "worktree_preflight",
                json!({ "message": "fixture failure before first child" }),
            )
            .expect("persist coordinator failure");

            let (value, exit_code) = batch_status(
                AgentTaskFanoutBatchStatusArgs {
                    batch_id: batch_id.to_string(),
                },
                Placement::Auto,
            )
            .expect("reading a failed batch must still return its projection");

            assert_eq!(exit_code, 0);
            assert_eq!(value["batch"]["status"], "failed");
            assert_eq!(value["batch"]["batch"]["state"], "failed");
            assert_eq!(
                value["batch"]["admission_blocker"]["stage"],
                "worktree_preflight"
            );
        });
    }

    #[test]
    fn public_fanout_status_returns_stale_durable_state_during_observation_contention() {
        with_isolated_home(|_| {
            let batch_id = "contended-public-status";
            let run_id = "contended-public-status-child";
            agent_task_lifecycle::record_detached_cook_handoff_parent_in_store(
                &test_lifecycle_store(),
                run_id,
            )
            .expect("persist child lifecycle record");
            batch::persist_fanout_run_batch(
                batch_id,
                batch_id,
                &[batch::FanoutRunBatchChild {
                    task_id: run_id.to_string(),
                    run_id: run_id.to_string(),
                }],
                json!({ "replan_command": "homeboy agent-task fanout run-plan --input @plan.json" }),
            )
            .expect("persist fanout batch");

            let database =
                homeboy::core::observation::store::database_path().expect("observation path");
            let connection =
                rusqlite::Connection::open(database).expect("open contention connection");
            connection
                // WAL permits concurrent readers. Use rollback-journal locking so
                // the public command must exercise the bounded busy fallback.
                .execute_batch("PRAGMA journal_mode=DELETE; BEGIN EXCLUSIVE")
                .expect("hold observation write lock");

            let (value, exit_code) = batch_status(
                AgentTaskFanoutBatchStatusArgs {
                    batch_id: batch_id.to_string(),
                },
                Placement::Auto,
            )
            .expect("public status keeps the durable batch readable");

            assert_eq!(exit_code, 0);
            assert_eq!(value["batch"]["observation_fresh"], false);
            assert_eq!(value["batch"]["batch"]["state"], "queued");
        });
    }

    #[test]
    fn coordinator_heartbeat_starts_before_slow_preflight() {
        with_isolated_home(|_| {
            let batch_id = "heartbeat-before-preflight";
            batch::persist_fanout_run_batch(
                batch_id,
                batch_id,
                &[batch::FanoutRunBatchChild {
                    task_id: "child".to_string(),
                    run_id: "child-run".to_string(),
                }],
                json!({ "replan_command": "homeboy agent-task fanout run-plan --input @plan.json" }),
            )
            .expect("persist batch");
            let claim_id = batch::claim_fanout_run_batch(batch_id)
                .expect("claim")
                .expect("claim id");
            let before = batch::read_batch_record(batch_id).expect("batch").metadata["coordinator"]
                ["admission_deadline_at"]
                .clone();

            std::thread::sleep(std::time::Duration::from_millis(2));
            CoordinatorHeartbeat::start(
                batch_id.to_string(),
                claim_id,
                format!("homeboy agent-task fanout status {batch_id}"),
            )
            .expect("start heartbeat before preflight")
            .finish()
            .expect("finish heartbeat");

            assert_ne!(
                batch::read_batch_record(batch_id)
                    .expect("renewed batch")
                    .metadata["coordinator"]["admission_deadline_at"],
                before
            );
        });
    }

    #[test]
    fn public_status_reports_admitting_before_the_first_child_exists() {
        with_isolated_home(|_| {
            let batch_id = "status-during-admission";
            batch::persist_fanout_run_batch(
                batch_id,
                batch_id,
                &[batch::FanoutRunBatchChild {
                    task_id: "child".to_string(),
                    run_id: "synthesized-child-run".to_string(),
                }],
                json!({}),
            )
            .expect("persist batch before child admission");
            batch::claim_fanout_run_batch(batch_id)
                .expect("claim batch")
                .expect("coordinator claim");

            let (value, exit_code) = batch_status(
                AgentTaskFanoutBatchStatusArgs {
                    batch_id: batch_id.to_string(),
                },
                Placement::Lab,
            )
            .expect("admission window remains a readable fanout status");

            assert_eq!(exit_code, 0);
            assert_eq!(value["batch"]["status"], "admitting");
            assert_eq!(value["batch"]["batch"]["state"], "admitting");
            assert_eq!(value["batch"]["admission"]["admitted"], 0);
            assert_eq!(value["batch"]["admission"]["absent"], 1);
            assert!(value["batch"]["unavailable_child_runs"]
                .as_array()
                .is_none_or(Vec::is_empty));
        });
    }

    #[test]
    fn detached_lab_transport_retry_replaces_the_canonical_fanout_child() {
        with_isolated_home(|_| {
            let mut plan = test_batch_plan();
            plan.cooks.truncate(1);
            plan.rekey("detached-lab-transport-retry".to_string());
            let cook = &plan.cooks[0];
            let task_id = cook.cook_id.clone();
            let mut options = cook
                .to_cook_invocation(&plan)
                .expect("compile child invocation")
                .options;
            materialize_test_child(&mut options);
            options.identity.initial_plan.metadata["batch_id"] = json!(plan.fanout_id);
            options.provider_transport.attempt_dispatcher = Some(Arc::new(LabRecipeDispatcher));
            agent_task_service::persist_initial_recipe(&options)
                .expect("persist detached Lab recipe");
            let initial_run_id = options.identity.initial_run_id.clone();
            batch::persist_fanout_run_batch(
                &plan.fanout_id,
                &plan.fanout_id,
                &[batch::FanoutRunBatchChild {
                    task_id,
                    run_id: initial_run_id.clone(),
                }],
                json!({}),
            )
            .expect("persist fanout roster");

            let retry_run_id = format!("{initial_run_id}-transport-retry");
            let recipe_store = agent_task_service::CookRecipeStore::from_current_data_root()
                .expect("recipe store");
            let recipe = recipe_store
                .record_recipe_attempt_replacement(
                    &options.identity.cook_id,
                    &initial_run_id,
                    &retry_run_id,
                )
                .expect("record transport replacement");
            assert_eq!(
                recipe.promotion_transport["attempt_dispatch"]["kind"],
                "lab"
            );
            agent_task_lifecycle::submit_plan(
                &recipe.attempts.last().expect("replacement attempt").plan,
                Some(&retry_run_id),
            )
            .expect("materialize replacement child");

            let (value, exit_code) = batch_status(
                AgentTaskFanoutBatchStatusArgs {
                    batch_id: plan.fanout_id.clone(),
                },
                Placement::Lab,
            )
            .expect("original fanout id resolves replacement child");

            assert_eq!(exit_code, 0);
            assert_eq!(
                value["batch"]["batch"]["child_runs"][0]["run_id"],
                retry_run_id
            );
            assert_eq!(value["batch"]["admission"]["admitted"], 1);
            assert!(value["batch"]["unavailable_child_runs"]
                .as_array()
                .is_none_or(Vec::is_empty));
        });
    }

    #[test]
    fn cook_batch_does_not_warn_for_specific_backend_names_in_core() {
        with_materialized_cook_batch_worktrees(|| {
            let mut args = cook_batch_args();
            // Planning and execution share admission, so this has to name an
            // installed backend. `sandbox` is specific; the assertion is that
            // a concrete name is not itself a warning.
            args.backend = Some("sandbox".to_string());
            args.provider_config = None;

            let (value, exit_code) = cook_batch(args).expect("cook batch dry run");

            assert_eq!(exit_code, 0, "{value}");
            assert!(value["preflight"]["provider_selection"]["warnings"]
                .as_array()
                .expect("warnings")
                .is_empty());
        });
    }

    #[test]
    fn cook_batch_cli_parses_multiple_issues_and_gates() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "cook-batch",
            "--repo",
            "homeboy",
            "--provider-profile",
            "opencode-codex-gpt55",
            "--verify",
            "cargo test --lib",
            "--ai-tool",
            "OpenAI GPT-5.6 Sol via OpenCode",
            "https://github.com/Extra-Chill/homeboy/issues/6453",
            "https://github.com/Extra-Chill/homeboy/issues/6454",
        ])
        .expect("cook-batch parses");

        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
            panic!("fanout command");
        };
        let AgentTaskFanoutCommand::CookBatch(args) = fanout.command else {
            panic!("cook-batch command");
        };
        assert_eq!(args.issues.len(), 2);
        assert_eq!(args.gates.verify, vec!["cargo test --lib"]);
        assert_eq!(
            args.ai_tool.as_deref(),
            Some("OpenAI GPT-5.6 Sol via OpenCode")
        );
        assert_eq!(args.from, None);
        assert_eq!(args.base, None);
        assert_eq!(
            args.provider_profile,
            Some("opencode-codex-gpt55".to_string())
        );
    }

    #[test]
    fn run_plan_cli_parses_ai_tool_override() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--ai-tool",
            "OpenAI GPT-5.6 Sol via OpenCode",
        ])
        .expect("run-plan parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
            panic!("fanout command");
        };
        let AgentTaskFanoutCommand::RunPlan(args) = fanout.command else {
            panic!("run-plan command");
        };
        assert_eq!(
            args.ai_tool.as_deref(),
            Some("OpenAI GPT-5.6 Sol via OpenCode")
        );
    }

    #[test]
    fn run_plan_cli_parses_max_concurrency() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--max-concurrency",
            "2",
        ])
        .expect("run-plan parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
            panic!("fanout command");
        };
        let AgentTaskFanoutCommand::RunPlan(args) = fanout.command else {
            panic!("run-plan command");
        };
        assert_eq!(args.max_concurrency, Some(2));
    }

    /// Zero workers would deadlock a batch that has work to do, so the flag
    /// refuses it at parse time rather than silently clamping later.
    #[test]
    fn max_concurrency_rejects_zero() {
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--max-concurrency",
            "0",
        ])
        .is_err());
    }

    /// A ceiling declared at plan time has to survive persistence, or a batch
    /// planned with `--max-concurrency 1` fans out unbounded when `run-plan`
    /// executes it later.
    #[test]
    fn a_persisted_plan_carries_its_concurrency_ceiling() {
        let mut plan = test_batch_plan();
        assert_eq!(plan.max_concurrency, None);
        plan.max_concurrency = Some(1);

        let encoded = serde_json::to_value(&plan).expect("plan serializes");
        assert_eq!(encoded["max_concurrency"], 1);

        let reloaded = BatchCookFanoutPlan::from_value(encoded, &args()).expect("plan reloads");
        assert_eq!(reloaded.max_concurrency, Some(1));
    }

    #[test]
    fn an_execution_time_flag_overrides_the_persisted_ceiling() {
        let mut plan = test_batch_plan();
        plan.max_concurrency = Some(4);

        plan.apply_max_concurrency_override(None);
        assert_eq!(plan.max_concurrency, Some(4), "absent flag must not clear");

        plan.apply_max_concurrency_override(Some(1));
        assert_eq!(plan.max_concurrency, Some(1));
    }

    #[test]
    fn run_plan_cli_parses_max_duration() {
        let cli = Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--max-duration",
            "5400",
        ])
        .expect("run-plan parses");
        let Commands::AgentTask(agent_task) = cli.command else {
            panic!("agent-task command");
        };
        let AgentTaskCommand::Fanout(fanout) = agent_task.command else {
            panic!("fanout command");
        };
        let AgentTaskFanoutCommand::RunPlan(args) = fanout.command else {
            panic!("run-plan command");
        };
        assert_eq!(args.max_duration, Some(5400));
    }

    /// A zero-second budget would expire before the first attempt could start,
    /// so it is refused at parse time rather than silently killing the batch.
    #[test]
    fn max_duration_rejects_zero() {
        assert!(Cli::try_parse_from([
            "homeboy",
            "agent-task",
            "fanout",
            "run-plan",
            "--input",
            "@plan.json",
            "--max-duration",
            "0",
        ])
        .is_err());
    }

    /// Absent the flag, a batch must stay unbudgeted — the pre-existing
    /// behaviour for every caller that never asked for a deadline.
    #[test]
    fn a_plan_without_a_budget_resolves_no_deadline() {
        let plan = test_batch_plan();
        assert_eq!(plan.max_duration_seconds, None);
        assert!(plan.cook_deadline().is_none());
    }

    /// The budget is persisted as a duration and resolved to an absolute
    /// instant only at execution, so a plan that sat on disk still gets the
    /// budget it asked for instead of one that expired while it waited.
    #[test]
    fn a_budget_is_persisted_as_a_duration_and_resolved_at_run_time() {
        let mut plan = test_batch_plan();
        plan.max_duration_seconds = Some(3_600);

        let encoded = serde_json::to_value(&plan).expect("plan serializes");
        assert_eq!(encoded["max_duration_seconds"], 3_600);

        let reloaded = BatchCookFanoutPlan::from_value(encoded, &args()).expect("plan reloads");
        assert_eq!(reloaded.max_duration_seconds, Some(3_600));

        let deadline = reloaded.cook_deadline().expect("a resolved deadline");
        assert!(
            !deadline.is_expired(),
            "a fresh budget must not start spent"
        );
        assert!(deadline.remaining_ms() > 0);
    }

    #[test]
    fn an_execution_time_flag_overrides_the_persisted_budget() {
        let mut plan = test_batch_plan();
        plan.max_duration_seconds = Some(7_200);

        plan.apply_max_duration_override(None);
        assert_eq!(
            plan.max_duration_seconds,
            Some(7_200),
            "absent flag must not clear the persisted budget"
        );

        plan.apply_max_duration_override(Some(60));
        assert_eq!(plan.max_duration_seconds, Some(60));
    }

    /// The operator-facing half of the fix: the limit and why it was chosen
    /// have to be readable off the result.
    #[test]
    fn the_batch_result_reports_the_effective_limit_and_its_source() {
        let plan = test_batch_plan();
        let report = agent_task_service::AgentTaskCookBatchReport {
            schema: "homeboy/agent-task-cook-batch/v1",
            batch_id: "test-batch".to_string(),
            status: "succeeded".to_string(),
            total: 2,
            queued: 0,
            running: 0,
            succeeded: 2,
            failed: 0,
            cancelled: 0,
            timed_out: 0,
            cooks: Vec::new(),
        };
        let (data, _exit_code) = batch_cook_result(
            &plan,
            agent_task_service::AgentTaskRunResult {
                exit_code: 0,
                value: report,
            },
            &BatchConcurrencyDecision {
                limit: 1,
                source: homeboy::agents::agent_task_scheduler::BatchConcurrencySource::Flag,
                reason: "--max-concurrency 1 requested by the caller".to_string(),
            },
        );
        assert_eq!(data["concurrency"]["limit"], 1);
        assert_eq!(data["concurrency"]["source"], "flag");
        assert_eq!(
            data["concurrency"]["reason"],
            "--max-concurrency 1 requested by the caller"
        );
    }

    #[test]
    fn force_with_lease_uses_the_observed_sha_and_converges_after_push_before_receipt() {
        let root = tempfile::tempdir().expect("temporary Git fixture");
        let remote = root.path().join("remote.git");
        let worktree = root.path().join("worktree");
        git(root.path(), &["init", "--bare", remote.to_str().unwrap()]);
        git(
            root.path(),
            &[
                "clone",
                remote.to_str().unwrap(),
                worktree.to_str().unwrap(),
            ],
        );
        git(&worktree, &["config", "user.name", "Homeboy test"]);
        git(&worktree, &["config", "user.email", "homeboy@example.test"]);
        std::fs::write(worktree.join("candidate.txt"), "base\n").expect("write base");
        git(&worktree, &["add", "."]);
        git(&worktree, &["commit", "-m", "base"]);
        git(&worktree, &["branch", "-M", "main"]);
        git(&worktree, &["push", "origin", "main"]);
        git(&worktree, &["checkout", "-b", "fanout/child"]);
        std::fs::write(worktree.join("candidate.txt"), "old candidate\n").expect("write old");
        git(&worktree, &["commit", "-am", "old candidate"]);
        git(&worktree, &["push", "origin", "fanout/child"]);
        let expected_sha = git(&worktree, &["rev-parse", "HEAD"]);

        std::fs::write(worktree.join("candidate.txt"), "new candidate\n").expect("write new");
        git(&worktree, &["commit", "-am", "new candidate"]);
        let candidate_sha = git(&worktree, &["rev-parse", "HEAD"]);

        let receipt = force_with_lease_push(worktree.to_str().unwrap(), "fanout/child")
            .expect("force-with-lease push");
        assert_eq!(receipt["expected_sha"], expected_sha);
        assert_eq!(receipt["after_sha"], candidate_sha);
        assert_eq!(
            receipt["command"][2],
            format!("--force-with-lease=refs/heads/fanout/child:{expected_sha}")
        );
        assert_eq!(receipt["pr_refresh_completed"], false);

        // This models a restart after Git accepted the push but before Homeboy
        // persisted its receipt. The second pass observes the candidate already
        // published and records convergence instead of issuing another push.
        let restarted = force_with_lease_push(worktree.to_str().unwrap(), "fanout/child")
            .expect("reconcile completed push");
        assert_eq!(restarted["expected_sha"], candidate_sha);
        assert_eq!(restarted["after_sha"], candidate_sha);
        assert_eq!(restarted["reconciled_existing_push"], true);
        assert_eq!(
            git(
                &worktree,
                &["ls-remote", "--heads", "origin", "refs/heads/fanout/child"]
            )
            .split_whitespace()
            .next(),
            Some(candidate_sha.as_str())
        );
    }

    #[test]
    fn github_observation_refreshes_a_fake_review_ready_pr() {
        let _env = env_lock();
        let root = tempfile::tempdir().expect("temporary GitHub fixture");
        let worktree = root.path().join("worktree");
        git(root.path(), &["init", worktree.to_str().unwrap()]);
        git(
            &worktree,
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/acme/fanout.git",
            ],
        );
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).expect("fake gh bin");
        let fake_gh = bin.join("gh");
        std::fs::write(
            &fake_gh,
            r#"#!/bin/sh
case "$1 $2" in
  "--version "|"auth status") exit 0 ;;
  "pr list") printf '%s\n' '[{"number":42,"title":"Fanout child","url":"https://github.com/acme/fanout/pull/42","state":"OPEN","baseRefName":"main","headRefName":"fanout/child"}]' ;;
  "pr view") printf '%s\n' '{"author":{"login":"homeboy"},"baseRefName":"main","headRefName":"fanout/child","headRefOid":"candidate-sha","title":"Fanout child","url":"https://github.com/acme/fanout/pull/42","state":"OPEN","isDraft":false,"mergedAt":null,"reviewDecision":"APPROVED","mergeStateStatus":"CLEAN","statusCheckRollup":[{"conclusion":"SUCCESS","status":"COMPLETED"}]}' ;;
  *) exit 1 ;;
esac
"#,
        )
        .expect("write fake gh");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&fake_gh, std::fs::Permissions::from_mode(0o755))
                .expect("make fake gh executable");
        }
        let previous_path = std::env::var_os("PATH");
        let mut path = std::ffi::OsString::from(bin.as_os_str());
        path.push(":");
        path.push(previous_path.clone().unwrap_or_default());
        std::env::set_var("PATH", path);

        let observed = github_observation(
            worktree.to_str().unwrap(),
            None,
            Some(&json!({ "head": "fanout/child", "base": "main" })),
        );
        match previous_path {
            Some(path) => std::env::set_var("PATH", path),
            None => std::env::remove_var("PATH"),
        }
        let (tracker, pr, remote_head, findings) = observed.expect("fake GitHub observation");
        assert_eq!(tracker, supervisor::AgentTaskFanoutTrackerState::Unknown);
        assert_eq!(pr, supervisor::AgentTaskFanoutPrState::OpenChecksPassing);
        assert_eq!(remote_head.as_deref(), Some("candidate-sha"));
        assert!(findings.is_empty());
    }

    #[test]
    fn declared_recipe_tracker_reference_is_projected_without_a_github_observation() {
        let metadata = json!({
            "cook_recipe": {
                "source_refs": ["https://tracker.example.test/issues/42"]
            }
        });

        assert_eq!(
            declared_tracker_ref(&metadata),
            Some("https://tracker.example.test/issues/42")
        );
        assert_eq!(
            declared_tracker_ref(&json!({
                "cook_recipe": { "source_refs": ["fanout:recipe"] }
            })),
            None
        );
    }

    fn git(path: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run Git fixture command");
        assert!(
            output.status.success(),
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}
