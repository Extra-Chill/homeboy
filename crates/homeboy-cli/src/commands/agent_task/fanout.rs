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

use homeboy::agents::agent_task_provider::AgentTaskProviderProfileDeclaration;
use homeboy::agents::agent_task_scheduler::{
    resolve_batch_concurrency, BatchConcurrencyDecision, BatchConcurrencyInputs,
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
    AgentTaskDispatchCommand, DispatchCoreInputs,
};
use homeboy::agents::agent_tasks::fanout_supervisor as supervisor;
use homeboy::agents::agent_tasks::gate::{
    AgentTaskGateEnvironmentPolicy, AgentTaskGateExecutionPolicy, AgentTaskGateRevealPolicy,
    VerifyGateOptions,
};
use homeboy::agents::agent_tasks::lifecycle as agent_task_lifecycle;
use homeboy::agents::agent_tasks::provider::{self, AgentTaskProviderCatalog};
use homeboy::agents::agent_tasks::scheduler::AgentTaskPlan;
use homeboy::agents::agent_tasks::service::{
    self as agent_task_service, AgentTaskCookServiceOptions,
};
use homeboy::agents::agent_tasks::{
    AGENT_TASK_BATCH_COOK_FANOUT_PLAN_SCHEMA, AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
    AGENT_TASK_BATCH_COOK_FANOUT_SUBMIT_SCHEMA,
};
use homeboy::core::{config, worktree, Error, ErrorCode, Result};

use crate::commands::utils::response::{CommandNextAction, CommandNextActionKind};

use super::super::CmdResult;
use super::args::{
    AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs, AgentTaskFanoutCommand,
    AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs, AgentTaskFanoutRunPlanArgs,
    AgentTaskFanoutSubmitArgs, AgentTaskFanoutSubmitBatchArgs,
};
use super::command_json_value;
use super::gate_contract::{validate_gate_contracts, GateContractValidation};

pub(super) fn fanout(args: AgentTaskFanoutArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskFanoutCommand::CookBatch(cook_batch_args) => cook_batch(*cook_batch_args),
        AgentTaskFanoutCommand::Plan(plan_args) => {
            // A private controller artifact is accepted only from its owned path,
            // then immediately projected before this read-only response renders.
            let plan = load_batch_cook_fanout_plan(&plan_args.input, true)?;
            Ok((command_json_value(public_batch_cook_plan(&plan))?, 0))
        }
        AgentTaskFanoutCommand::Submit(submit_args) => submit_batch_cook_fanout(submit_args),
        AgentTaskFanoutCommand::SubmitBatch(submit_args) => submit_fanout_batch(submit_args),
        AgentTaskFanoutCommand::Status(status_args) => batch_status(status_args),
        AgentTaskFanoutCommand::Resume(resume_args) => batch_resume(resume_args),
        AgentTaskFanoutCommand::Artifacts(status_args) => batch_artifacts(status_args),
        AgentTaskFanoutCommand::RunPlan(run_args) => run_batch_cook_fanout(run_args),
    }
}

type CookAttemptDispatcherFactory = dyn Fn(
        &AgentTaskCookServiceOptions,
    ) -> std::sync::Arc<dyn crate::agents::agent_task_service::AgentTaskCookAttemptDispatcher>
    + Send
    + Sync;

const FANOUT_COORDINATOR_HEARTBEAT_INTERVAL: std::time::Duration =
    std::time::Duration::from_secs(30);

struct CoordinatorHeartbeat {
    stop: std::sync::mpsc::Sender<()>,
    worker: Option<std::thread::JoinHandle<()>>,
    stale_error: std::sync::Arc<std::sync::Mutex<Option<String>>>,
}

impl CoordinatorHeartbeat {
    fn start(batch_id: String, claim_id: String) -> Self {
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
                }
            }
        });
        Self {
            stop,
            worker: Some(worker),
            stale_error,
        }
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
pub(crate) fn cook_batch_with_attempt_dispatcher(
    args: AgentTaskFanoutCookBatchArgs,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    cook_batch_inner(args, Some(attempt_dispatcher))
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

fn submit_fanout_batch(args: AgentTaskFanoutSubmitBatchArgs) -> CmdResult<Value> {
    let plan = load_fanout_agent_task_plan(&args.input)?;
    let record = batch::submit_plan_batch(&plan, args.batch_id.as_deref())?;
    let batch_id = record.batch_id.clone();
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-fanout-batch-submit-result/v1",
            "batch": record,
            "commands": batch_commands(&batch_id),
        }),
        0,
    ))
}

fn batch_status(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    let observations = reconcile_fanout_pr_states(&args.batch_id, false)?;
    let mut report = batch::status(&args.batch_id)?;
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
    let exit_code = report.batch.state.exit_code();
    // `status` is a read-only projection. Durable reconciliation and any child
    // continuation are intentionally limited to the explicit `resume` command.
    let portfolio = reconcile_portfolio(&report.batch)?;
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-fanout-status/v2",
            "batch": report,
            "portfolio": portfolio,
        }),
        exit_code,
    ))
}

/// Resume a durable fanout batch after its synchronous coordinator exited.
/// Idempotently harvests every terminal-but-unfinalized child through its
/// original promotion, deterministic gates, commit, push, and PR finalization
/// contract, reconciling per-child state back into the durable batch record so
/// repeated resume calls converge without duplicate PRs (#9525).
fn batch_resume(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    reconcile_fanout_pr_states(&args.batch_id, true)?;
    let result = agent_task_service::resume_cook_batch(
        &args.batch_id,
        provider::ExtensionProviderAgentTaskExecutor::discover(),
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
        let Ok(record) = agent_task_lifecycle::status(&child.run_id) else {
            continue;
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
        .map(|child| portfolio_observation(&child.task_id, &child.run_id))
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
                        tracker_ref: format!("homeboy://agent-task/run/{}", child.run_id),
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
        portfolio_observation(&child.child_id, &child.run_id)
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
    let record = agent_task_lifecycle::status(&child.run_id)?;
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
    let receipt = force_with_lease_push(path, head)?;
    agent_task_lifecycle::record_cook_force_with_lease_receipt(&child.run_id, receipt.clone())?;
    agent_task_service::recover_cook_pr(&child.run_id, Vec::new(), false)?;
    let mut receipt = receipt;
    receipt["pr_refresh_completed"] = Value::Bool(true);
    agent_task_lifecycle::record_cook_force_with_lease_receipt(&child.run_id, receipt)?;
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
        provider::ExtensionProviderAgentTaskExecutor::discover(),
        crate::commands::infra::route::reconstruct_cook_attempt_dispatcher,
        rerun_completed_gates,
    )?;
    Ok(())
}

fn portfolio_observation(
    child_id: &str,
    run_id: &str,
) -> Result<homeboy::agents::agent_tasks::fanout_supervisor::AgentTaskFanoutPortfolioObservation> {
    use homeboy::agents::agent_tasks::fanout_supervisor as supervisor;
    let record = agent_task_lifecycle::status(run_id).ok();
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
            supervisor::AgentTaskFanoutTrackerState::Unknown,
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
    let task_url = record
        .and_then(|record| record.metadata.pointer("/cook_recipe/source_refs/0"))
        .and_then(Value::as_str);
    let tracker = task_url
        .and_then(|url| IssueRef::parse(url).ok())
        .map(|issue| {
            homeboy::core::git::issue_find(
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
                    .map_or(supervisor::AgentTaskFanoutTrackerState::Unknown, |item| {
                        if item.state.eq_ignore_ascii_case("open") {
                            supervisor::AgentTaskFanoutTrackerState::Open
                        } else {
                            supervisor::AgentTaskFanoutTrackerState::Closed
                        }
                    })
            })
        })
        .transpose()?
        .unwrap_or(supervisor::AgentTaskFanoutTrackerState::Unknown);
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
                "status": format!("homeboy agent-task fanout status {batch_id}"),
                "artifacts": format!("homeboy agent-task fanout artifacts {batch_id}"),
                "resume": format!("homeboy agent-task fanout resume {batch_id}"),
            },
        }),
        exit_code,
    )
}

fn batch_artifacts(args: AgentTaskFanoutBatchStatusArgs) -> CmdResult<Value> {
    Ok((command_json_value(batch::artifacts(&args.batch_id)?)?, 0))
}

fn run_batch_cook_fanout(args: AgentTaskFanoutRunPlanArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input, true)?;
    plan.apply_ai_tool_override(args.ai_tool.as_deref());
    plan.apply_max_concurrency_override(args.max_concurrency.map(|value| value as usize));
    plan.apply_max_duration_override(args.max_duration);
    if let Some(record_run_id) = args.record_run_id {
        plan.rekey(record_run_id);
    }
    run_batch_cook_fanout_plan(plan)
}

/// Keeps durable batch coordination on the controller while routing each
/// independent provider attempt through the selected transport.
pub(crate) fn run_batch_cook_fanout_with_attempt_dispatcher(
    args: AgentTaskFanoutRunPlanArgs,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input, true)?;
    plan.apply_ai_tool_override(args.ai_tool.as_deref());
    plan.apply_max_concurrency_override(args.max_concurrency.map(|value| value as usize));
    plan.apply_max_duration_override(args.max_duration);
    if let Some(record_run_id) = args.record_run_id {
        plan.rekey(record_run_id);
    }
    run_batch_cook_fanout_plan_with_attempt_dispatcher(plan, attempt_dispatcher)
}

/// Persist the durable batch record before dispatching children so
/// `fanout status <fanout_id>` can resolve every child run (#9397). Without
/// this, run-plan admitted children but never wrote
/// `agent-task-batches/<fanout_id>.json`, so status failed with
/// `No such file or directory`.
fn persist_fanout_run_batch_record(plan: &BatchCookFanoutPlan) -> Result<()> {
    let children = plan
        .cooks
        .iter()
        .map(|cook| batch::FanoutRunBatchChild {
            task_id: cook.cook_id.clone(),
            run_id: cook.run_id(),
        })
        .collect::<Vec<_>>();
    batch::persist_fanout_run_batch(
        &plan.fanout_id,
        &plan.fanout_id,
        &children,
        serde_json::json!({
            "source": "fanout-run-plan",
            "durable_child_runs": true,
            "dependency_graph": plan.dependency_graph_metadata()?,
        }),
    )?;
    Ok(())
}

fn claim_fanout_run_batch_coordinator(plan: &BatchCookFanoutPlan) -> Result<String> {
    persist_fanout_run_batch_record(plan)?;
    if let Some(claim_id) = batch::claim_fanout_run_batch(&plan.fanout_id)? {
        return Ok(claim_id);
    }
    Err(Error::validation_invalid_argument(
        "fanout_id",
        "agent-task fanout run-plan is already being coordinated; inspect its durable status",
        Some(plan.fanout_id.clone()),
        None,
    ))
}

fn record_batch_failure(plan: &BatchCookFanoutPlan, claim_id: &str, stage: &str, error: &Error) {
    let _ = batch::record_fanout_run_batch_failure(
        &plan.fanout_id,
        claim_id,
        stage,
        serde_json::json!({ "message": error.message, "details": error.details }),
    );
}

fn run_batch_cook_fanout_plan_with_attempt_dispatcher(
    plan: BatchCookFanoutPlan,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    run_batch_cook_fanout_plan_with_attempt_dispatcher_claim(plan, attempt_dispatcher, None)
}

fn run_batch_cook_fanout_plan_with_attempt_dispatcher_claim(
    plan: BatchCookFanoutPlan,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
    claim_id: Option<String>,
) -> CmdResult<Value> {
    let gate_workspace = batch_plan_gate_workspace(&plan)?;
    let gate_contract_validation = validate_batch_gate_contracts(&plan, gate_workspace.as_deref())?;
    let claim_id = match claim_id {
        Some(claim_id) => claim_id,
        None => claim_fanout_run_batch_coordinator(&plan)?,
    };
    let outcome = (|| {
        persist_batch_cook_recipes(&plan, |options| {
            record_gate_contract_validation(options, &gate_contract_validation);
            options.attempt_dispatcher = Some(attempt_dispatcher(options));
        })?;
        let ready_plan = plan.ready_plan()?;
        let cooks = compile_batch_cooks(&ready_plan, |options| {
            record_gate_contract_validation(options, &gate_contract_validation);
            options.attempt_dispatcher = Some(attempt_dispatcher(options));
        })?;
        let concurrency = batch_concurrency(&plan, &cooks);
        // Resolved once, here, and bound for the whole batch: every worker thread
        // re-binds this same absolute instant, so the budget covers the batch
        // rather than restarting per child.
        let heartbeat = CoordinatorHeartbeat::start(plan.fanout_id.clone(), claim_id.clone());
        let result = with_current_cook_deadline(plan.cook_deadline(), || {
            batch::heartbeat_fanout_run_batch(&plan.fanout_id, &claim_id)?;
            agent_task_service::run_cook_batch_with_control(
                agent_task_service::AgentTaskCookBatchOptions {
                    batch_id: plan.fanout_id.clone(),
                    cooks,
                    max_concurrency: concurrency.limit,
                },
                provider::ExtensionProviderAgentTaskExecutor::discover(),
                agent_task_service::detached_batch_coordinator_control(&plan.fanout_id),
            )
        });
        heartbeat.finish()?;
        let result = result?;
        finalize_provider_worktrees(&plan, &result.value)?;
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

fn run_batch_cook_fanout_plan(plan: BatchCookFanoutPlan) -> CmdResult<Value> {
    run_batch_cook_fanout_plan_with_executor(
        plan,
        provider::ExtensionProviderAgentTaskExecutor::discover(),
    )
}

fn run_batch_cook_fanout_plan_with_executor<E>(
    plan: BatchCookFanoutPlan,
    executor: E,
) -> CmdResult<Value>
where
    E: homeboy::agents::agent_tasks::scheduler::AgentTaskExecutorAdapter + Clone + Send,
{
    run_batch_cook_fanout_plan_with_executor_claim(plan, executor, None)
}

fn run_batch_cook_fanout_plan_with_executor_claim<E>(
    plan: BatchCookFanoutPlan,
    executor: E,
    claim_id: Option<String>,
) -> CmdResult<Value>
where
    E: homeboy::agents::agent_tasks::scheduler::AgentTaskExecutorAdapter + Clone + Send,
{
    let gate_workspace = batch_plan_gate_workspace(&plan)?;
    let gate_contract_validation = validate_batch_gate_contracts(&plan, gate_workspace.as_deref())?;
    let claim_id = match claim_id {
        Some(claim_id) => claim_id,
        None => claim_fanout_run_batch_coordinator(&plan)?,
    };
    let outcome = (|| {
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
        let heartbeat = CoordinatorHeartbeat::start(plan.fanout_id.clone(), claim_id.clone());
        let result = with_current_cook_deadline(plan.cook_deadline(), || {
            batch::heartbeat_fanout_run_batch(&plan.fanout_id, &claim_id)?;
            agent_task_service::run_cook_batch_with_control(
                agent_task_service::AgentTaskCookBatchOptions {
                    batch_id: plan.fanout_id.clone(),
                    cooks,
                    max_concurrency: concurrency.limit,
                },
                executor,
                agent_task_service::detached_batch_coordinator_control(&plan.fanout_id),
            )
        });
        heartbeat.finish()?;
        let result = result?;
        finalize_provider_worktrees(&plan, &result.value)?;
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

fn record_gate_contract_validation(
    options: &mut AgentTaskCookServiceOptions,
    validation: &GateContractValidation,
) {
    options.initial_plan.metadata["gate_contract_validation"] =
        serde_json::to_value(validation).expect("gate contract validation serializes");
}

fn finalize_provider_worktrees(
    plan: &BatchCookFanoutPlan,
    report: &agent_task_service::AgentTaskCookBatchReport,
) -> Result<()> {
    if !configured_provider_lifecycle()? {
        return Ok(());
    }
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
        let resolution =
            homeboy::core::worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
                &cook.to_worktree,
                &config,
                None,
            )?;
        let disposition = if cell.exit_code == 0 {
            homeboy::core::worktree_providers::WorktreeProviderTerminalDisposition::Succeeded
        } else {
            homeboy::core::worktree_providers::WorktreeProviderTerminalDisposition::Failed
        };
        homeboy::core::worktree_providers::finalize_apply_enabled_worktree_provider_from_config(
            &resolution,
            &homeboy::core::worktree_providers::WorktreeProviderLifecycleIntent {
                purpose: "agent_task_cook".to_string(),
                owner_run_ref: cook.run_id(),
                cleanup_policy: homeboy::core::worktree_providers::WorktreeProviderCleanupPolicy::RemoveOnSuccess,
            },
            disposition,
            &config,
        )?;
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
    configure: impl Fn(&mut AgentTaskCookServiceOptions),
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
    configure: impl Fn(&mut AgentTaskCookServiceOptions),
) -> Result<Vec<AgentTaskCookServiceOptions>> {
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
            options.harvest_context = harvest_context.clone();
            configure(&mut options);
            Ok(options)
        })
        .collect()
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
    cooks: &[AgentTaskCookServiceOptions],
) -> BatchConcurrencyDecision {
    // A batch coordinator commits nothing before it starts, so the budget is
    // read at zero active units. The budget itself is the children's own, taken
    // from the first compiled plan: every child in a batch is compiled from the
    // same host policy.
    let resource_budget = cooks
        .first()
        .map(|cook| cook.initial_plan.options.resource_budget.clone())
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
                "status": format!("homeboy agent-task fanout status {}", plan.fanout_id),
                "artifacts": format!("homeboy agent-task fanout artifacts {}", plan.fanout_id),
            },
        }),
        result.exit_code,
    )
}

fn cook_batch(args: AgentTaskFanoutCookBatchArgs) -> CmdResult<Value> {
    cook_batch_inner(args, None)
}

fn cook_batch_inner(
    mut args: AgentTaskFanoutCookBatchArgs,
    attempt_dispatcher: Option<&CookAttemptDispatcherFactory>,
) -> CmdResult<Value> {
    args.gates.snapshot_file_inputs()?;
    normalize_cook_batch_repo(&mut args)?;
    apply_provider_profile(&mut args);
    // Resolve the effective backend (explicit --backend or the configured
    // default) and validate it up front (#7717). Otherwise an omitted
    // --backend silently rode a configured default all the way to
    // provider execution, where each child cook failed late with a
    // provider-shaped `no extension agent-task provider found for backend`
    // error instead of an early, actionable configuration failure. Making the
    // effective backend explicit here also surfaces it in the preflight and
    // pins every child cook to the same resolved backend.
    resolve_and_validate_effective_backend(&mut args)?;
    let mut plan = build_cook_batch_plan(&args)?;
    let plan_has_private_gates = plan
        .cooks
        .iter()
        .any(|cook| !cook.private_verify.is_empty());
    let persisted = args.run_plan && !args.dry_run;
    let claim_id = persisted
        .then(|| claim_fanout_run_batch_coordinator(&plan))
        .transpose()?;
    if let Err(error) = validate_batch_cook_gates(&plan, batch_gate_workspace(&args)?) {
        record_batch_preflight_failure(claim_id.as_deref(), &plan, "gate_preflight", &error)?;
        return Err(error);
    }
    let worktrees = match queue_or_reuse_worktrees(&args, &plan) {
        Ok(worktrees) => worktrees,
        Err(error) => {
            record_batch_preflight_failure(
                claim_id.as_deref(),
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
            claim_id.as_deref().expect("persisted coordinator claim"),
            "worktree_preflight",
            serde_json::json!({ "worktrees": worktrees.rows }),
        )?;
    }
    let can_run = !args.dry_run
        && blocked == 0
        && worktrees
            .rows
            .iter()
            .all(|row| matches!(row.status, worktree::WorktreeQueueCreateStatus::Created));
    let private_artifact_path = if can_run && plan_has_private_gates {
        if let Err(error) = bind_materialized_worktrees(&mut plan, &worktrees) {
            record_batch_preflight_failure(
                claim_id.as_deref(),
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
                    claim_id.as_deref(),
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
                claim_id.as_deref(),
                &plan,
                "provider_preflight",
                &error,
            )?;
            return Err(error);
        }
    } else if args.dry_run {
        preflight_batch_cook_recipe_declarations(&plan)?;
    }
    let run_result = if args.run_plan && can_run {
        let (value, exit_code) = match attempt_dispatcher {
            Some(dispatcher) => run_batch_cook_fanout_plan_with_attempt_dispatcher_claim(
                plan.clone(),
                dispatcher,
                claim_id.clone(),
            )?,
            None => run_batch_cook_fanout_plan_with_executor_claim(
                plan.clone(),
                provider::ExtensionProviderAgentTaskExecutor::discover(),
                claim_id.clone(),
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
    } else if args.dry_run {
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

    Ok((
        serde_json::json!({
                "schema": "homeboy/agent-task-cook-batch/v1",
                "fanout_id": plan.fanout_id,
                "status": status,
                "dry_run": args.dry_run,
                "summary": {
                    "issues": plan.cooks.len(),
                    "worktrees_total": worktrees.rows.len(),
                    "worktrees_blocked": blocked,
                },
                "preflight": {
                    "provider_readiness_command": provider_readiness_command(&args),
                    "provider_selection": provider_selection_preflight(&args),
                    "deterministic_gates": effective_batch_cook_gates(&plan)
                },
                "worktrees": worktrees,
                "plan": public_batch_cook_plan(&plan),
                "run_result": run_result,
        "commands": cook_batch_commands(&args, plan_has_private_gates, private_artifact_path.as_deref()),
                // Named run-plan persists before worktree/provider preflight, so
                // status and artifacts remain available when admission is blocked.
                "next_actions": cook_batch_next_actions(
                    &args,
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

fn normalize_cook_batch_repo(args: &mut AgentTaskFanoutCookBatchArgs) -> Result<()> {
    let path_like = std::path::Path::new(&args.repo).is_absolute()
        || args.repo.contains(std::path::MAIN_SEPARATOR)
        || std::path::Path::new(&args.repo).exists();
    let handle_like = args.repo.contains('@');
    if !path_like && !handle_like {
        return Ok(());
    }

    if handle_like && !path_like {
        let candidates = args
            .repo
            .split_once('@')
            .and_then(|(id, _)| {
                homeboy::core::component::registered()
                    .ok()
                    .and_then(|components| {
                        components
                            .into_iter()
                            .find(|component| component.id == id)
                            .map(|component| vec![component.id])
                    })
            })
            .unwrap_or_default();
        return Err(invalid_cook_batch_repo(args, candidates));
    }

    match homeboy::core::component::resolve_registered_primary_path(&args.repo)? {
        homeboy::core::component::RegisteredPrimaryPathResolution::Primary(id) => {
            args.repo = id;
            Ok(())
        }
        homeboy::core::component::RegisteredPrimaryPathResolution::Related(candidates) => {
            Err(invalid_cook_batch_repo(args, candidates))
        }
        homeboy::core::component::RegisteredPrimaryPathResolution::Unknown => {
            Err(invalid_cook_batch_repo(args, Vec::new()))
        }
    }
}

fn invalid_cook_batch_repo(args: &AgentTaskFanoutCookBatchArgs, candidates: Vec<String>) -> Error {
    let correction_command =
        (candidates.len() == 1 && !has_private_gate_declaration(args)).then(|| {
            let mut corrected = args.clone();
            corrected.repo = candidates[0].clone();
            quote_args(&cook_batch_argv(&corrected))
        });
    let secure_reentry = (candidates.len() == 1 && has_private_gate_declaration(args)).then(|| {
        format!(
            "re-run the original private Cook-batch invocation with --repo {}; Homeboy will queue, bind, and persist the executable private plan before returning its run-plan command",
            candidates[0]
        )
    });
    let message = if candidates.is_empty() {
        "--repo must be a registered repo slug or an exact registered primary path"
    } else if candidates.len() == 1 {
        "--repo identifies a related checkout, not a registered primary path"
    } else {
        "--repo matches multiple registered component identities"
    };
    Error::new(
        ErrorCode::ValidationInvalidArgument,
        message,
        serde_json::json!({
            "provided": args.repo,
            "expected_kind": "registered_repo_slug_or_primary_path",
            "resolved_candidates": candidates,
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
fn cook_batch_argv(args: &AgentTaskFanoutCookBatchArgs) -> Vec<String> {
    let mut command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "cook-batch".to_string(),
        "--repo".to_string(),
        args.repo.clone(),
        "--from".to_string(),
        args.from.clone(),
        "--base".to_string(),
        args.base.clone(),
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
    if args.dry_run {
        command.push("--dry-run".to_string());
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

fn queue_or_reuse_worktrees(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
) -> Result<worktree::WorktreeQueueCreateOutput> {
    let provider_lifecycle = configured_provider_lifecycle()?;
    let queue_create = |cooks: Vec<&BatchCookSpec>, dry_run: bool| {
        worktree::queue_create(worktree::WorktreeQueueCreateOptions {
            repo: args.repo.clone(),
            requests: cooks.into_iter().map(|cook| worktree::WorktreeQueueCreateRequest {
                branch: cook.head.clone().expect("generated cooks have heads"),
                task_url: cook.task_url.clone(),
                task_ref: cook.task_url.clone(),
                run_id: Some(cook.run_id()),
                provider_lifecycle: provider_lifecycle.then(|| {
                    homeboy::core::worktree_providers::WorktreeProviderLifecycleIntent {
                        purpose: "agent_task_cook".to_string(),
                        owner_run_ref: cook.run_id(),
                        cleanup_policy: homeboy::core::worktree_providers::WorktreeProviderCleanupPolicy::RemoveOnSuccess,
                    }
                }),
            }).collect(),
            from: args.from.clone(),
            dry_run,
            retry_after_seconds: 30,
        })
    };

    if args.dry_run {
        if configured_provider_workspace_creation()? {
            return with_workspace_owner_repair_commands(
                args,
                plan,
                plan_provider_worktrees_dry_run(args, plan)?,
            );
        }
        return with_workspace_owner_repair_commands(
            args,
            plan,
            queue_or_reuse_worktrees_dry_run(args, plan, queue_create)?,
        );
    }

    let mut reused = Vec::new();
    let mut to_create = Vec::new();
    for cook in &plan.cooks {
        let branch = cook.head.as_ref().expect("generated cooks have heads");
        match (!provider_lifecycle).then(|| active_registered_worktree_path(&cook.to_worktree)) {
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
                });
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

    with_workspace_owner_repair_commands(
        args,
        plan,
        worktree::WorktreeQueueCreateOutput {
            schema: "homeboy/worktree-queue-create/v1",
            repo: args.repo.clone(),
            base_ref: args.from.clone(),
            dry_run: false,
            rows,
        },
    )
}

fn with_workspace_owner_repair_commands(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
    mut worktrees: worktree::WorktreeQueueCreateOutput,
) -> Result<worktree::WorktreeQueueCreateOutput> {
    if !configured_provider_lifecycle()? {
        return Ok(worktrees);
    }

    let config = homeboy::core::defaults::load_config();
    for row in &mut worktrees.rows {
        let Some(cook) = plan
            .cooks
            .iter()
            .find(|cook| cook.to_worktree == row.handle)
        else {
            continue;
        };
        let intent = homeboy::core::worktree_providers::WorktreeProviderCreateIntent {
            handle: row.handle.clone(),
            repo: args.repo.clone(),
            base: args.from.clone(),
            head: row.branch.clone(),
            task_url: cook
                .task_url
                .clone()
                .expect("generated cooks have task URLs"),
        };
        let lifecycle = homeboy::core::worktree_providers::WorktreeProviderLifecycleIntent {
            purpose: "agent_task_cook".to_string(),
            owner_run_ref: cook.run_id(),
            cleanup_policy:
                homeboy::core::worktree_providers::WorktreeProviderCleanupPolicy::RemoveOnSuccess,
        };
        row.command =
            homeboy::core::worktree_providers::worktree_provider_lifecycle_ensure_argv_from_config(
                &intent, &lifecycle, &config,
            )?;
    }
    Ok(worktrees)
}

/// Provider-managed destinations have provider-owned path policy. Dry-run
/// therefore asks the provider's optional read-only plan command rather than
/// deriving native sibling paths or invoking ensure.
fn plan_provider_worktrees_dry_run(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
) -> Result<worktree::WorktreeQueueCreateOutput> {
    use homeboy::core::worktree_providers::{
        plan_apply_enabled_worktree_provider_with_lifecycle_from_config,
        WorktreeProviderCreateIntent, WorktreeProviderCreatePlan,
    };

    let config = homeboy::core::defaults::load_config();
    let mut rows = Vec::new();
    for cook in &plan.cooks {
        let head = cook.head.as_ref().expect("generated cooks have heads");
        let task_url = cook
            .task_url
            .as_ref()
            .expect("generated cooks have task URLs");
        let intent = WorktreeProviderCreateIntent {
            handle: cook.to_worktree.clone(),
            repo: args.repo.clone(),
            base: args.from.clone(),
            head: head.clone(),
            task_url: task_url.clone(),
        };
        let command = worktree_create_command(args, head);
        match plan_apply_enabled_worktree_provider_with_lifecycle_from_config(&intent, &config) {
            Ok(WorktreeProviderCreatePlan::Existing(resolution)) => {
                rows.push(worktree::WorktreeQueueCreateRow {
                    branch: head.clone(),
                    handle: resolution.worktree.handle,
                    status: worktree::WorktreeQueueCreateStatus::Created,
                    command,
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(resolution.worktree.path),
                    error: None,
                });
            }
            Ok(WorktreeProviderCreatePlan::WouldCreate(resolution)) => {
                rows.push(worktree::WorktreeQueueCreateRow {
                    branch: head.clone(),
                    handle: resolution.worktree.handle,
                    status: worktree::WorktreeQueueCreateStatus::WouldCreate,
                    command,
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(resolution.worktree.path),
                    error: None,
                });
            }
            Err(error) => {
                let mut row = worktree::WorktreeQueueCreateRow {
                    branch: head.clone(),
                    handle: cook.to_worktree.clone(),
                    status: worktree::WorktreeQueueCreateStatus::Failed,
                    command,
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: None,
                    error: Some(error.message),
                };
                row.error = Some(
                    serde_json::json!({
                        "message": row.error,
                        "details": error.details,
                    })
                    .to_string(),
                );
                rows.push(row);
            }
        }
    }
    Ok(worktree::WorktreeQueueCreateOutput {
        schema: "homeboy/worktree-queue-create/v1",
        repo: args.repo.clone(),
        base_ref: args.from.clone(),
        dry_run: true,
        rows,
    })
}

fn configured_provider_workspace_creation() -> Result<bool> {
    configured_provider_lifecycle()
}

/// Dry-run observes existing managed worktrees but only plans creation for
/// missing handles. This preserves the exact native creation path while never
/// asking dispatch to resolve a workspace that does not exist yet.
fn queue_or_reuse_worktrees_dry_run(
    args: &AgentTaskFanoutCookBatchArgs,
    plan: &BatchCookFanoutPlan,
    queue_create: impl Fn(Vec<&BatchCookSpec>, bool) -> Result<worktree::WorktreeQueueCreateOutput>,
) -> Result<worktree::WorktreeQueueCreateOutput> {
    let mut reused = Vec::new();
    let mut to_create = Vec::new();
    for cook in &plan.cooks {
        let branch = cook.head.as_ref().expect("generated cooks have heads");
        match active_registered_worktree_path(&cook.to_worktree) {
            Some(path) => {
                reused.push(worktree::WorktreeQueueCreateRow {
                    branch: branch.clone(),
                    handle: cook.to_worktree.clone(),
                    status: worktree::WorktreeQueueCreateStatus::Created,
                    command: worktree_create_command(args, branch),
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(path),
                    error: None,
                });
            }
            _ => to_create.push(cook),
        }
    }
    let planned = queue_create(to_create, true)?;
    let rows = plan
        .cooks
        .iter()
        .filter_map(|cook| {
            let branch = cook.head.as_ref().expect("generated cooks have heads");
            reused
                .iter()
                .find(|row| row.handle == cook.to_worktree)
                .cloned()
                .or_else(|| {
                    planned
                        .rows
                        .iter()
                        .find(|row| row.branch == *branch)
                        .cloned()
                })
        })
        .collect();
    Ok(worktree::WorktreeQueueCreateOutput {
        schema: "homeboy/worktree-queue-create/v1",
        repo: args.repo.clone(),
        base_ref: args.from.clone(),
        dry_run: true,
        rows,
    })
}

fn active_registered_worktree_path(handle: &str) -> Option<String> {
    if let Ok(status) = worktree::status(handle) {
        return (status.record.state == worktree::TaskWorktreeState::Active
            && !status.safety.worktree_missing)
            .then_some(status.record.worktree_path);
    }
    match worktree::resolve_workspace_ref_if_present(handle)
        .ok()
        .flatten()?
    {
        worktree::WorkspaceRefRecord::Adopted(record)
            if record.state == worktree::TaskWorktreeState::Active
                && std::path::Path::new(&record.path).is_dir() =>
        {
            Some(record.path)
        }
        _ => None,
    }
}

fn configured_provider_lifecycle() -> Result<bool> {
    let config = homeboy::core::defaults::load_config();
    for (id, provider) in &config.worktree_providers {
        if provider.enabled
            && provider.apply_enabled
            && provider.commands.ensure.is_some()
            && homeboy::core::worktree_providers::worktree_provider_lifecycle_finalizer_argv_from_config(id, &config)?.is_some()
        {
            return Ok(true);
        }
    }
    Ok(false)
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
            options.attempt_dispatcher = Some(dispatcher(&options));
        }
        agent_task_service::validate_initial_recipe_compatibility(&options)?;
    }
    Ok(())
}

/// A dry-run validates the immutable recipe declaration, not a future
/// workspace. Dispatch compilation resolves workspace handles by design, so it
/// runs only after execution has materialized the declared destination.
fn preflight_batch_cook_recipe_declarations(plan: &BatchCookFanoutPlan) -> Result<()> {
    for cook in &plan.cooks {
        cook.to_cook_invocation(plan)?;
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
    let raw = if let Some(path) = args.input.strip_prefix('@') {
        let path = PathBuf::from(path);
        if path.parent() == Some(private_batch_plan_dir()?.as_path()) {
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
        let expected_path = private_batch_plan_path(fanout_id)?;
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

fn private_batch_plan_path(fanout_id: &str) -> Result<PathBuf> {
    Ok(private_batch_plan_dir()?.join(format!(
        "{}.json",
        homeboy::core::paths::sanitize_path_segment(fanout_id)
    )))
}

fn private_batch_plan_dir() -> Result<PathBuf> {
    Ok(homeboy::core::paths::homeboy_data()?
        .join("agent-task")
        .join("private-batch-plans"))
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
    #[serde(default, skip_serializing_if = "Value::is_null")]
    metadata: Value,
}

impl BatchCookFanoutPlan {
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
    options: AgentTaskCookServiceOptions,
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
        if self.verify.is_empty() && self.private_verify.is_empty() {
            return Err(invalid_fanout(
                "each fanout cook requires verify or private_verify so PR finalization has deterministic gates",
            ));
        }
        let mut prompt = self.prompt.clone();
        let workspace_root = self.workspace.as_deref().or(self.cwd.as_deref());
        let mut provider_config = self.provider_config.clone();
        if let Some(workspace) = workspace_root {
            let evidence = super::run::project_provider_evidence_inputs(
                &self.provider_evidence_inputs,
                Path::new(workspace),
                None,
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
            super::run::rewrite_provider_evidence_prompt(
                &mut prompt,
                &self.provider_evidence_inputs,
                Some(workspace),
            );
        }
        let dispatch = AgentTaskDispatchCommand {
            prompt,
            tasks: self.tasks.clone(),
            cwd: self.cwd.clone(),
            workspace: self
                .workspace
                .clone()
                .or_else(|| self.cwd.is_none().then(|| self.to_worktree.clone())),
            repo: self.repo.clone(),
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
            options: AgentTaskCookServiceOptions {
                cook_id: self.run_id(),
                initial_run_id: self.run_id(),
                initial_plan: AgentTaskPlan::new(self.run_id(), Vec::new()),
                to_worktree: self.to_worktree.clone(),
                source_worktree_path,
                provider_command: self.provider_command.clone(),
                provider_invocation: None,
                gates: VerifyGateOptions {
                    verify: self.verify.clone(),
                    private_verify: self.private_verify.clone(),
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
                max_attempts: self.max_attempts,
                no_finalize: self.no_finalize,
                draft_pr: self.draft_pr,
                base: self.base.clone(),
                task_base_sha,
                head: self.head.clone(),
                title,
                commit_message,
                source_refs: self
                    .task_url
                    .clone()
                    .into_iter()
                    .chain(std::iter::once(cook_recipe_source_identity(plan, self)?))
                    .collect(),
                protected_branches: self.protected_branches.clone(),
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
                attempt_dispatcher: None,
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
        let worktree = format!("{}@{}", args.repo, slugify(&branch));
        let prompt = render_prompt(
            args.prompt_template.as_deref(),
            &issue,
            &args.repo,
            &branch,
            &worktree,
        );
        let task_selector = format!("issue-{}", issue.number);
        let (verify, private_verify, verification_profile) = profiles.resolve(
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
            repo: Some(args.repo.clone()),
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
            to_worktree: worktree,
            provider_command: None,
            verify,
            private_verify,
            input_sources,
            verification_profile,
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
            base: args.base.clone(),
            head: Some(branch),
            title: Some(format!("Fix {}", issue.key)),
            commit_message: Some(format!("fix: address {}", issue.key)),
            protected_branches: super::review::default_protected_branches(),
            ai_tool: args.ai_tool.clone().unwrap_or_else(default_ai_tool),
            ai_used_for: default_ai_used_for(),
        });
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
            args.repo,
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
        metadata: serde_json::json!({
            "source": "agent-task fanout cook-batch",
            "issue_count": args.issues.len(),
            "repo": args.repo,
            "base": args.base,
            "from": args.from,
        }),
    })
}

#[derive(Debug, Deserialize)]
struct VerificationProfiles {
    #[serde(default)]
    profiles: BTreeMap<String, VerificationProfile>,
    #[serde(default)]
    assignments: Vec<VerificationProfileAssignment>,
}

#[derive(Debug, Deserialize)]
struct VerificationProfile {
    #[serde(default)]
    verify: Vec<String>,
    #[serde(default)]
    private_verify: Vec<String>,
    #[serde(default)]
    mode: VerificationProfileMode,
}

#[derive(Debug, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
enum VerificationProfileMode {
    #[default]
    Append,
    Replace,
}

#[derive(Debug, Deserialize)]
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
                    Some(cooks.iter().map(|cook| cook.cook_id.clone()).collect()),
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
    ) -> Result<(Vec<String>, Vec<String>, Option<String>)> {
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
            return Ok((shared_verify.to_vec(), shared_private_verify.to_vec(), None));
        };
        let profile = self.profiles.get(&assignment.profile).ok_or_else(|| {
            Error::validation_invalid_argument(
                "verification-profiles.assignments",
                "profile_unknown: assignment references an undeclared verification profile",
                Some(assignment.profile.clone()),
                Some(self.profiles.keys().cloned().collect()),
            )
        })?;
        let (mut verify, mut private_verify) = match profile.mode {
            VerificationProfileMode::Append => {
                (shared_verify.to_vec(), shared_private_verify.to_vec())
            }
            VerificationProfileMode::Replace => (Vec::new(), Vec::new()),
        };
        verify.extend(profile.verify.clone());
        private_verify.extend(profile.private_verify.clone());
        Ok((verify, private_verify, Some(assignment.profile.clone())))
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
    let profiles: VerificationProfiles = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_argument(
            "verification-profiles",
            format!("invalid JSON verification profile declaration: {error}"),
            None,
            None,
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
        if cook.verify.is_empty() && cook.private_verify.is_empty() {
            return Err(Error::validation_invalid_argument(
                "verification-profiles",
                "gate_missing: every cook-batch child requires verify or private_verify before worktree creation",
                Some(cook.cook_id.clone()),
                Some(vec!["Pass shared --verify/--private-verify gates, or assign a non-empty profile to this child.".to_string()]),
            ));
        }
    }
    validate_batch_gate_contracts(plan, workspace.as_deref())?;
    Ok(())
}

fn batch_gate_workspace(args: &AgentTaskFanoutCookBatchArgs) -> Result<Option<std::path::PathBuf>> {
    let component = homeboy::core::component::registered()?
        .into_iter()
        .find(|component| component.id == args.repo);
    let Some(component) = component else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(component.local_path);
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
    let component = homeboy::core::component::registered()?
        .into_iter()
        .find(|component| component.id == repository);
    let Some(component) = component else {
        return Ok(None);
    };
    let path = std::path::PathBuf::from(component.local_path);
    Ok(path.is_dir().then_some(path))
}

fn effective_batch_cook_gates(plan: &BatchCookFanoutPlan) -> Vec<Value> {
    plan.cooks
        .iter()
        .map(|cook| {
            serde_json::json!({
                "cook_id": cook.cook_id,
                "task_url": cook.task_url,
                "profile": cook.verification_profile,
                "verify": cook.verify,
                "private_verify": cook.private_verify.iter().map(|_| "[private]").collect::<Vec<_>>(),
                "input_sources": cook.input_sources,
            })
        })
        .collect()
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

/// Resolve the effective execution backend for the batch and fail early when it
/// has no installed provider (#7717).
///
/// When `--backend` is omitted the effective backend comes from the configured
/// `agent_task.default_backend`. We pin it onto `args.backend` so it is visible
/// in the preflight and carried identically to every child cook, then confirm a
/// provider can serve it — turning a late, provider-shaped child failure into an
/// early configuration error listing the backends that are actually installed.
fn resolve_and_validate_effective_backend(args: &mut AgentTaskFanoutCookBatchArgs) -> Result<()> {
    let effective = match args.backend.as_deref() {
        Some(backend) if !backend.trim().is_empty() => backend.trim().to_string(),
        _ => match provider::default_backend().map_err(|error| {
            invalid_fanout(&format!("could not resolve default backend: {error}"))
        })? {
            Some(backend) => backend,
            // No explicit backend and no configured default: leave resolution to
            // the existing per-cook/component defaulting and its own diagnostics.
            None => return Ok(()),
        },
    };

    // Validate against installed providers only when the batch will actually
    // execute. Dry-run/planning legitimately runs where no provider is installed
    // (e.g. CI compiling the plan), so a hard provider check there would reject
    // valid planning. Execution is where an unresolved backend fails late, so
    // that is exactly where we fail early instead.
    let will_execute = args.run_plan && !args.dry_run;
    if will_execute {
        let catalog = AgentTaskProviderCatalog::discover();
        let selector = args.selector.as_deref();
        if !matches!(
            provider::resolve_provider_for_backend(catalog.providers(), &effective, selector),
            provider::ProviderResolution::Resolved(_)
        ) {
            let mut available: Vec<String> = catalog
                .providers()
                .iter()
                .map(|p| p.backend.clone())
                .collect();
            available.sort();
            available.dedup();
            let available_hint = if available.is_empty() {
                "no agent-task provider backends are installed; install a provider extension (e.g. opencode)".to_string()
            } else {
                format!("installed backends: {}", available.join(", "))
            };
            let source = if args.backend.is_some() {
                "requested via --backend"
            } else {
                "resolved from agent_task.default_backend"
            };
            return Err(invalid_fanout(&format!(
                "agent-task fanout backend '{effective}' ({source}) has no installed provider. \
                 Pass --backend <installed> explicitly, or set agent_task.default_backend to an installed backend. {available_hint}"
            )));
        }
    }

    // Pin the resolved backend so the preflight and every child cook use it,
    // making an otherwise-implicit default visible and consistent.
    args.backend = Some(effective);
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
        "Fix {issue_url}. Inspect the issue, implement the smallest correct change in {repo}, run the requested verification gates, and report the changed files plus verification results. Homeboy deterministic finalization is enabled: Homeboy will commit, push {branch}, open/update the PR, add AI disclosure, and finalize reviewer-ready evidence after gates pass. Do not inspect credentials, configure git identity, commit, push, or open/update the PR yourself.",
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
        args.from.clone(),
    ]
}

fn cook_batch_plan_command(args: &AgentTaskFanoutCookBatchArgs) -> String {
    let mut planned = args.clone();
    planned.dry_run = true;
    planned.run_plan = false;
    quote_args(&cook_batch_argv(&planned))
}

fn has_private_gates(args: &AgentTaskFanoutCookBatchArgs) -> bool {
    has_private_gate_declaration(args)
}

/// Before Cook resolves issue assignments, conservatively treat any declared
/// private profile as private so invalid-input recovery cannot echo its JSON.
fn has_private_gate_declaration(args: &AgentTaskFanoutCookBatchArgs) -> bool {
    if !args.gates.private_verify.is_empty() || !args.gates.private_verify_file.is_empty() {
        return true;
    }
    load_verification_profiles(args.verification_profiles.as_deref())
        .map(|profiles| {
            profiles
                .profiles
                .values()
                .any(|profile| !profile.private_verify.is_empty())
        })
        .unwrap_or(true)
}

fn secure_batch_plan_execution(fanout_id: &str) -> String {
    let path = private_batch_plan_path(fanout_id)
        .expect("Homeboy data directory is required for private batch plans");
    private_artifact_run_command(&path)
}

fn private_artifact_run_command(path: &std::path::Path) -> String {
    quote_args(&[
        "homeboy".to_string(),
        "agent-task".to_string(),
        "fanout".to_string(),
        "run-plan".to_string(),
        "--input".to_string(),
        format!("@{}", path.display()),
    ])
}

fn cook_batch_run_command(args: &AgentTaskFanoutCookBatchArgs) -> String {
    let mut runnable = args.clone();
    runnable.dry_run = false;
    runnable.run_plan = true;
    quote_args(&cook_batch_argv(&runnable))
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
fn cook_batch_commands(
    args: &AgentTaskFanoutCookBatchArgs,
    has_private_gates: bool,
    private_artifact_path: Option<&std::path::Path>,
) -> Value {
    if has_private_gates {
        return serde_json::json!({
            "plan": "[redacted: private verification gates cannot be rendered in a public rerun command]",
            "run": private_artifact_path.map_or_else(
                || "[unavailable: private plan is not persisted until concrete worktrees are bound; re-run the original local invocation after remediation]".to_string(),
                |path| private_artifact_run_command(path),
            ),
            "resume_from_plan": "[unavailable until Homeboy binds and persists the private plan]",
        });
    }
    serde_json::json!({
        "plan": cook_batch_plan_command(args),
        "run": cook_batch_run_command(args),
        "resume_from_plan": "save .plan to JSON and run homeboy agent-task fanout run-plan --input @batch-cook-plan.json",
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
                private_artifact_run_command,
            )
        } else {
            cook_batch_run_command(args)
        };
        actions.push(
            CommandNextAction::new("rerun this cook-batch once the worktrees exist", command)
                .with_kind(CommandNextActionKind::Repair),
        );
        if executed {
            actions.push(
                CommandNextAction::new(
                    "show persisted batch status",
                    format!("homeboy agent-task fanout status {fanout_id}"),
                )
                .with_kind(CommandNextActionKind::Show),
            );
            actions.push(
                CommandNextAction::new(
                    "list persisted batch artifacts",
                    format!("homeboy agent-task fanout artifacts {fanout_id}"),
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
                format!("homeboy agent-task fanout status {fanout_id}"),
            )
            .with_kind(CommandNextActionKind::Show),
            CommandNextAction::new(
                "list batch artifacts",
                format!("homeboy agent-task fanout artifacts {fanout_id}"),
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
                    format!("homeboy agent-task fanout resume {fanout_id}"),
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
                private_artifact_run_command,
            ),
        )
        .with_kind(CommandNextActionKind::Repair)];
    }
    vec![
        CommandNextAction::new("re-plan this cook-batch", cook_batch_plan_command(args))
            .with_kind(CommandNextActionKind::Show),
        CommandNextAction::new("execute this cook-batch", cook_batch_run_command(args))
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

fn batch_commands(batch_id: &str) -> Value {
    serde_json::json!({
        "status": format!("homeboy agent-task fanout status {batch_id}"),
        "artifacts": format!("homeboy agent-task fanout artifacts {batch_id}"),
        "run_next": "homeboy agent-task run-next"
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cli_surface::{Cli, Commands};
    use crate::commands::agent_task::{AgentTaskCommand, AgentTaskFanoutCommand};
    use crate::test_support::{env_lock, with_isolated_home};
    use clap::Parser;
    use serde_json::json;
    use sha2::{Digest, Sha256};

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
    fn private_profile_declarations_use_redacted_public_paths_for_append_and_replace() {
        let sentinel = "PRIVATE_PROFILE_SENTINEL";
        for mode in ["append", "replace"] {
            let mut args = cook_batch_args();
            args.verification_profiles = Some(format!(
                r#"{{"profiles":{{"private":{{"private_verify":["{sentinel}"],"mode":"{mode}"}}}},"assignments":[{{"selector":"issue-6453","profile":"private"}}]}}"#
            ));
            assert!(has_private_gate_declaration(&args));
            let plan = build_cook_batch_plan(&args).expect("resolve profile");
            assert!(plan
                .cooks
                .iter()
                .any(|cook| cook.private_verify.iter().any(|gate| gate == sentinel)));
            let commands = cook_batch_commands(&args, true, None);
            assert!(!commands.to_string().contains(sentinel));
            assert!(commands["run"].as_str().unwrap().contains("unavailable"));
        }
    }

    #[test]
    fn private_profile_sentinel_is_redacted_across_repo_error_dry_run_and_bound_plan() {
        let sentinel = "PRIVATE_PROFILE_ALL_STATES_SENTINEL";
        let profiles = format!(
            r#"{{"profiles":{{"private":{{"private_verify":["{sentinel}"],"mode":"append"}}}},"assignments":[{{"selector":"issue-6453","profile":"private"}}]}}"#
        );
        with_isolated_home(|home| {
            let mut invalid = cook_batch_args();
            invalid.repo = "homeboy@bad".to_string();
            invalid.verification_profiles = Some(profiles.clone());
            let error = normalize_cook_batch_repo(&mut invalid).expect_err("invalid repo");
            assert!(!format!("{} {:?}", error.message, error.details).contains(sentinel));

            let mut dry = cook_batch_args();
            dry.verification_profiles = Some(profiles.clone());
            dry.dry_run = true;
            let plan = build_cook_batch_plan(&dry).expect("profile plan");
            let public = serde_json::to_string(&public_batch_cook_plan(&plan)).unwrap();
            assert!(!public.contains(sentinel));
            let commands = cook_batch_commands(&dry, true, None);
            assert!(!commands.to_string().contains(sentinel));
            assert!(!commands["run"].as_str().unwrap().contains("run-plan"));

            let primary = home.path().join("primary");
            fs::create_dir(&primary).expect("primary");
            write_component_registration(home.path(), "homeboy", &primary);
        });
        with_materialized_cook_batch_worktrees(|| {
            let mut executable = cook_batch_args();
            executable.verification_profiles = Some(profiles);
            executable.dry_run = false;
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
            args.dry_run = true;
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
            let command = secure_batch_plan_execution(&plan.fanout_id);
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
            let parent = private_batch_plan_dir().expect("private plan dir");
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
    fn provider_finalization_fixture() -> (tempfile::TempDir, PathBuf, BatchCookFanoutPlan) {
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
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  printf '{{\"worktrees\":[{{\"handle\":\"%s\",\"path\":\"{}\",\"branch\":\"fixture\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}\\n' \"$2\"\nelse\n  printf '%s|%s|%s|%s|%s|%s\\n' \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\" >> '{}'\nfi\n",
                workspace.display(),
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
            let (_fixture, records, plan) = provider_finalization_fixture();
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

            finalize_provider_worktrees(&plan, &report).expect("dispatcher fanout terminalization");

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
    fn dispatcher_fanout_execution_finalizes_failed_provider_lifecycle() {
        with_isolated_home(|_| {
            let (_fixture, records, mut plan) = provider_finalization_fixture();
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
                agent_task_lifecycle::status("dependent-run").expect("dependent record");
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
        let _env_lock = env_lock();
        let _env = EnvRestore::set(&[
            ("HOMEBOY_RUNNER_HOSTED_EXEC", None),
            ("HOMEBOY_SOURCE_SNAPSHOT_JSON", None),
            ("HOMEBOY_LAB_OFFLOAD_JSON", None),
        ]);
        let plan = test_batch_plan();
        let cooks = compile_batch_cooks(&plan, |_| {}).expect("compile batch cooks");

        assert_eq!(cooks.len(), 2);
        assert!(cooks.iter().all(|cook| cook.initial_plan.tasks.len() == 1));
        assert!(cooks
            .iter()
            .all(|cook| format!("{:?}", cook.harvest_context)
                == "HarvestExecutionContext { source_snapshot: None, lab_offload: None }"));
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
            assert_eq!(invocation.options.to_worktree, "homeboy@fix-5929-docs");
            assert_eq!(invocation.options.head.as_deref(), Some("fix/5929-docs"));
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
            from: "origin/main".to_string(),
            base: "main".to_string(),
            branch_prefix: "fix".to_string(),
            fanout_id: Some("issue-wave".to_string()),
            prompt_template: None,
            backend: Some("sandbox".to_string()),
            selector: Some("sample.executor-provider".to_string()),
            model: Some("gpt-5.5".to_string()),
            provider_profile: None,
            secret_env: vec!["AI_PROVIDER_OPENAI_CODEX_TOKEN".to_string()],
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
            dry_run: true,
            run_plan: false,
        }
    }

    fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
        let components = home.join(".config/homeboy/components");
        std::fs::create_dir_all(&components).expect("components directory");
        std::fs::write(
            components.join(format!("{id}.json")),
            serde_json::json!({ "local_path": local_path }).to_string(),
        )
        .expect("component registration");
    }

    #[test]
    fn cook_batch_repo_normalization_accepts_slugs_and_registered_primary_paths() {
        with_isolated_home(|home| {
            let primary = home.path().join("primary");
            std::fs::create_dir(&primary).expect("primary directory");
            write_component_registration(home.path(), "fixture", &primary);

            let mut slug = cook_batch_args();
            slug.repo = "fixture".to_string();
            normalize_cook_batch_repo(&mut slug).expect("slug remains accepted");
            assert_eq!(slug.repo, "fixture");

            let mut path = cook_batch_args();
            path.repo = primary.to_string_lossy().to_string();
            normalize_cook_batch_repo(&mut path).expect("primary path resolves");
            assert_eq!(path.repo, "fixture");
        });
    }

    #[test]
    fn cook_batch_repo_normalization_rejects_handles_and_unknown_paths_with_corrections() {
        with_isolated_home(|home| {
            let private_sentinel = "PRIVATE_GATE_SENTINEL_INVALID_REPO";
            let primary = home.path().join("primary");
            std::fs::create_dir(&primary).expect("primary directory");
            write_component_registration(home.path(), "fixture", &primary);

            let mut handle = cook_batch_args();
            handle.repo = "fixture@fix-11984".to_string();
            handle.from = "origin/release".to_string();
            handle.base = "release".to_string();
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
            assert_eq!(error.details["resolved_candidates"], json!([]));
            assert!(error.details["correction_command"].is_null());
        });
    }

    fn with_materialized_cook_batch_worktrees(test: impl FnOnce()) {
        with_isolated_home(|_| {
            let mut config = homeboy::core::defaults::load_config();
            config.agent_task.default_backend = Some("sandbox".to_string());
            config.worktree_providers.clear();
            config.settings.remove(
                homeboy::core::worktree_providers::WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY,
            );
            homeboy::core::defaults::save_config(&config)
                .expect("configure fixture default backend");
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
            assert!(prompt.contains("Homeboy will commit, push fix/issue-6453-homeboy"));
            assert!(prompt.contains("open/update the PR"));
            assert!(prompt.contains("add AI disclosure"));
            assert!(
                prompt.contains("Do not inspect credentials, configure git identity, commit, push")
            );
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
                invocation.options.source_worktree_path.as_deref(),
                Some(workspace.path())
            );
        });
    }

    #[test]
    fn private_batch_artifact_is_persisted_only_after_workspace_binding() {
        with_materialized_cook_batch_worktrees(|| {
            let sentinel = "PRIVATE_GATE_BOUND_PLAN_SENTINEL";
            let mut args = cook_batch_args();
            args.gates.private_verify = vec![sentinel.to_string()];
            args.dry_run = false;
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
    fn cook_batch_resolves_mixed_verification_profiles_and_round_trips_commands() {
        let mut batch_args = cook_batch_args();
        batch_args
            .issues
            .push("https://github.com/Extra-Chill/homeboy/issues/6455".to_string());
        batch_args.gates.verify = vec!["shared gate --strict='exact bytes'".to_string()];
        batch_args.verification_profiles = Some(
            serde_json::json!({
                "profiles": {
                    "php": { "verify": ["composer audit --format=json"], "mode": "append" },
                    "node": { "verify": ["npm audit --omit=dev"], "mode": "replace" },
                    "rust": { "verify": ["cargo fmt --check", "cargo test -p homeboy-cli"] }
                },
                "assignments": [
                    { "selector": "Extra-Chill/homeboy#6453", "profile": "php" },
                    { "selector": "issue-6454", "profile": "node" },
                    { "selector": "https://github.com/Extra-Chill/homeboy/issues/6455", "profile": "rust" }
                ]
            })
            .to_string(),
        );

        let plan = build_cook_batch_plan(&batch_args).expect("mixed verification plan");
        assert_eq!(plan.cooks[0].verification_profile.as_deref(), Some("php"));
        assert_eq!(
            plan.cooks[0].verify,
            vec![
                "shared gate --strict='exact bytes'",
                "composer audit --format=json"
            ]
        );
        assert_eq!(plan.cooks[1].verification_profile.as_deref(), Some("node"));
        assert_eq!(plan.cooks[1].verify, vec!["npm audit --omit=dev"]);
        assert_eq!(plan.cooks[2].verification_profile.as_deref(), Some("rust"));
        assert_eq!(
            plan.cooks[2].verify,
            vec![
                "shared gate --strict='exact bytes'",
                "cargo fmt --check",
                "cargo test -p homeboy-cli"
            ]
        );

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
        }
        assert_eq!(
            round_trip.cooks[2]
                .to_cook_invocation(&round_trip)
                .expect("Lab handoff invocation")
                .options
                .gates
                .verify,
            plan.cooks[2].verify
        );
    }

    #[test]
    fn cook_batch_rejects_an_unmatched_verification_profile_selector() {
        let mut args = cook_batch_args();
        args.verification_profiles = Some(
            r#"{"profiles":{"node":{"verify":["npm audit"]}},"assignments":[{"selector":"issue-9999","profile":"node"}]}"#.to_string(),
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
            r#"{"profiles":{"rust":{"verify":["cargo test"]}},"assignments":[{"selector":"issue-6453","profile":"rust"}]}"#.to_string(),
        );

        let plan = build_cook_batch_plan(&args).expect("plan before worktree creation");
        let error = validate_batch_cook_gates(&plan, None).expect_err("every child needs a gate");
        assert_eq!(error.details["problem"], "gate_missing: every cook-batch child requires verify or private_verify before worktree creation");
    }

    #[test]
    fn cook_batch_rejects_repository_script_alias_before_worktree_queueing() {
        with_isolated_home(|home| {
            let source = home.path().join("fixture-primary");
            std::fs::create_dir_all(&source).expect("primary directory");
            std::fs::write(
                source.join("homeboy.json"),
                r#"{"scripts":{"lint":["check"]}}"#,
            )
            .expect("component manifest");
            write_component_registration(home.path(), "fixture", &source);

            let mut args = cook_batch_args();
            args.repo = "fixture".to_string();
            args.gates.verify = vec!["homeboy lint fixture --path .".to_string()];
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
                    invocation.options.ai_tool,
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
                .ai_tool,
            "OpenAI GPT-5.6 Sol via OpenCode"
        );
        assert_eq!(
            plan.cooks[1]
                .to_cook_invocation(&plan)
                .expect("terra invocation")
                .options
                .ai_tool,
            "OpenAI GPT-5.6 Terra via OpenCode"
        );
    }

    #[test]
    fn cook_batch_run_plan_binds_inferred_worktree_for_dispatch_and_promotion() {
        // `cook-batch --run-plan` generates children without --cwd/--workspace.
        // Once its declared worktree is materialized, the same canonical root
        // must drive provider dispatch and promotion before execution starts.
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
            invocation.options.source_worktree_path.as_deref(),
            Some(root.as_path())
        );

        let compiled = compile_batch_cooks(&plan, |_| {}).expect("compile before provider");
        assert_eq!(
            compiled[0].initial_plan.tasks[0].workspace.root.as_deref(),
            root.to_str()
        );
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
            invocation.options.source_worktree_path.as_deref(),
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
                invocation.options.ai_model, None,
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
                invocation.options.ai_model.as_deref(),
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
    }

    #[test]
    fn canonical_cook_batch_identity_matches_the_plan_and_its_child_lineage() {
        let mut args = cook_batch_args();
        args.fanout_id = None;

        let fanout_id = cook_batch_fanout_id(&args).expect("canonical fanout identity");
        let plan = build_cook_batch_plan(&args).expect("canonical fanout plan");

        assert_eq!(fanout_id, plan.fanout_id);
        for cook in plan.cooks {
            assert!(cook.cook_id.starts_with(&format!("{fanout_id}-")));
            assert!(cook.run_id().starts_with(&format!("cook-{fanout_id}-")));
        }
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
        with_isolated_home(|_| {
            let plan = test_batch_plan();
            let compiled = compile_batch_cooks(&plan, |_| {}).expect("compile batch cooks");
            let mut invocation = plan.cooks[0]
                .to_cook_invocation(&plan)
                .expect("cook invocation");
            invocation.options.harvest_context = batch_harvest_context().expect("harvest context");
            invocation.options.initial_plan = compiled[0].initial_plan.clone();
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
        with_isolated_home(|_| {
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
        with_isolated_home(|_| {
            let plan = test_batch_plan();
            let dispatcher = |_: &AgentTaskCookServiceOptions| {
                std::sync::Arc::new(LabRecipeDispatcher)
                    as std::sync::Arc<
                        dyn homeboy::agents::agent_task_service::AgentTaskCookAttemptDispatcher,
                    >
            };

            persist_batch_cook_recipes(&plan, |options| {
                options.attempt_dispatcher = Some(dispatcher(options));
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
        args.dry_run = false;
        args.run_plan = true;

        let error = resolve_and_validate_effective_backend(&mut args)
            .expect_err("an executing batch with an unresolved backend must fail early");
        assert_eq!(error.details["field"], "input");
        assert!(
            error.message.contains("codebox-nonexistent")
                && error.message.contains("no installed provider"),
            "error must name the backend and the missing provider: {}",
            error.message
        );
        assert!(
            error.message.contains("--backend"),
            "error must be actionable: {}",
            error.message
        );
    }

    #[test]
    fn dry_run_batch_pins_the_backend_without_requiring_a_provider() {
        // Dry-run/planning must not require an installed provider — it only
        // builds the plan. The effective backend is still pinned so it is
        // visible and carried consistently.
        let mut args = cook_batch_args();
        args.backend = Some("sandbox".to_string());
        args.dry_run = true;
        args.run_plan = false;

        resolve_and_validate_effective_backend(&mut args)
            .expect("dry-run planning must not require an installed provider");
        assert_eq!(args.backend.as_deref(), Some("sandbox"));
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
            assert_eq!(value["worktrees"]["rows"][0]["status"], "created");
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
    fn dry_run_plans_absent_worktrees_without_creating_them() {
        with_isolated_home(|home| {
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
            write_component_registration(home.path(), "fanout-dry-run-fixture", &source);

            let mut args = cook_batch_args();
            args.repo = "fanout-dry-run-fixture".to_string();
            args.from = "HEAD".to_string();
            let (value, exit_code) = cook_batch(args).expect("dry-run plan");

            assert_eq!(exit_code, 0);
            assert_eq!(value["status"], "ready");
            for row in value["worktrees"]["rows"].as_array().expect("rows") {
                assert_eq!(row["status"], "would_create");
                let path = row["path"].as_str().expect("planned path");
                assert!(!std::path::Path::new(path).exists());
            }
            assert!(value["plan"]["cooks"]
                .as_array()
                .expect("cooks")
                .iter()
                .all(|cook| cook["workspace"].is_null()));
            assert!(!home
                .path()
                .join(".local/share/homeboy/agent-task-recipes")
                .exists());
        });
    }

    #[test]
    fn dry_run_reuses_existing_worktrees_and_plans_missing_children() {
        with_isolated_home(|home| {
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
            args.from = "HEAD".to_string();
            let (value, exit_code) = cook_batch(args).expect("mixed dry-run plan");

            assert_eq!(exit_code, 0, "{value}");
            assert_eq!(value["worktrees"]["rows"][0]["status"], "created");
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
            batch_resume_result(active_failed_report, 0, "test-batch", None);
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
            let exit_code = state.exit_code();
            let envelope =
                crate::commands::utils::response::cli_response_for_json_result_for_command(
                    &Ok(json!({
                        "status": state.outcome_status(),
                        "batch": { "state": state.outcome_status() }
                    })),
                    exit_code,
                    "agent-task fanout status",
                    None,
                );

            assert_eq!(envelope.success, exit_code == 0, "{state:?}");
            assert_eq!(envelope.exit_code, exit_code, "{state:?}");
            assert_eq!(envelope.status, state.outcome_status(), "{state:?}");
            assert_eq!(
                envelope.data.expect("durable status")["batch"]["state"],
                state.outcome_status(),
                "{state:?}"
            );
        }
    }

    #[test]
    fn cook_batch_unknown_provider_profile_warns_without_core_defaults() {
        with_materialized_cook_batch_worktrees(|| {
            let mut args = cook_batch_args();
            args.backend = None;
            args.model = None;
            args.provider_profile = Some("example-profile".to_string());

            let (value, exit_code) = cook_batch(args).expect("cook batch dry run");

            assert_eq!(exit_code, 0, "{value}");
            assert_eq!(
                value["preflight"]["provider_selection"]["profile"],
                "example-profile"
            );
            assert!(value["preflight"]["provider_selection"]["warnings"][0]
                .as_str()
                .expect("warning")
                .contains("not declared"));
        });
    }

    #[test]
    fn cook_batch_does_not_warn_for_specific_backend_names_in_core() {
        with_materialized_cook_batch_worktrees(|| {
            let mut args = cook_batch_args();
            args.backend = Some("example".to_string());
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
        assert_eq!(args.from, "origin/main");
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
