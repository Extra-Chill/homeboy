//! Public batch-cook fanout command handlers.

use homeboy_engine_primitives::content_hash;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::process::Command;

use homeboy::agents::agent_task_provider::AgentTaskProviderProfileDeclaration;
use homeboy::agents::agent_tasks::batch;
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
use homeboy::core::{config, worktree, Error, Result};

use super::super::CmdResult;
use super::args::{
    AgentTaskFanoutArgs, AgentTaskFanoutBatchStatusArgs, AgentTaskFanoutCommand,
    AgentTaskFanoutCookBatchArgs, AgentTaskFanoutInputArgs, AgentTaskFanoutRunPlanArgs,
    AgentTaskFanoutSubmitArgs, AgentTaskFanoutSubmitBatchArgs,
};
use super::command_json_value;

pub(super) fn fanout(args: AgentTaskFanoutArgs) -> CmdResult<Value> {
    match args.command {
        AgentTaskFanoutCommand::CookBatch(cook_batch_args) => cook_batch(cook_batch_args),
        AgentTaskFanoutCommand::Plan(plan_args) => {
            let plan = load_batch_cook_fanout_plan(&plan_args.input)?;
            Ok((command_json_value(plan)?, 0))
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

/// Runs controller-owned cook-batch coordination while dispatching its typed
/// provider attempts through the caller-selected transport.
pub(crate) fn cook_batch_with_attempt_dispatcher(
    args: AgentTaskFanoutCookBatchArgs,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    cook_batch_inner(args, Some(attempt_dispatcher))
}

fn submit_batch_cook_fanout(args: AgentTaskFanoutSubmitArgs) -> CmdResult<Value> {
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
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
    let report = batch::status(&args.batch_id)?;
    let exit_code = report.batch.state.exit_code();
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
    let batch_record = batch::read_batch_record(&args.batch_id)?;
    let mut portfolio = load_portfolio(&batch_record)?;
    let mut adapter = CookFanoutPortfolioAdapter;
    let dependencies = durable_graph_dependencies(&batch_record)?;
    let report = portfolio.run(&mut adapter, &dependencies)?;
    let exit_code = if report.blocked.is_empty() { 0 } else { 1 };
    Ok((
        serde_json::json!({
            "schema": "homeboy/agent-task-fanout-resume/v2",
            "batch_id": args.batch_id,
            "portfolio": report,
        }),
        exit_code,
    ))
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
    supervisor::write_portfolio(&portfolio)?;
    Ok(status)
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
                        blocker: None,
                        next_action: None,
                    }
                }),
            ))
        }
        Err(error) => Err(error),
    }
}

/// Read the public durable graph projection written by the fanout graph owner.
/// This consumes only its serialized readiness contract so the supervisor can
/// stack on that implementation without importing its internal graph types.
struct DurableGraphDependencies {
    readiness: BTreeMap<String, supervisor::FanoutDependencyReadiness>,
}

impl supervisor::FanoutDependencyResolver for DurableGraphDependencies {
    fn readiness(&self, child_id: &str) -> supervisor::FanoutDependencyReadiness {
        self.readiness
            .get(child_id)
            .cloned()
            .unwrap_or(supervisor::FanoutDependencyReadiness::Ready)
    }
}

fn durable_graph_dependencies(
    batch_record: &homeboy::agents::agent_tasks::AgentTaskBatchRecord,
) -> Result<DurableGraphDependencies> {
    let Some(graph) = batch_record.metadata.get("dependency_graph") else {
        return Ok(DurableGraphDependencies {
            readiness: BTreeMap::new(),
        });
    };
    let invalid = |detail: String| {
        batch_record
            .child_runs
            .iter()
            .map(|child| {
                (
                    child.task_id.clone(),
                    supervisor::FanoutDependencyReadiness::Blocked {
                        detail: detail.clone(),
                        evidence_ref: format!(
                            "homeboy://agent-task/batch/{}/dependency-graph",
                            batch_record.batch_id
                        ),
                    },
                )
            })
            .collect()
    };
    if graph.get("schema").and_then(Value::as_str)
        != Some("homeboy/agent-task-fanout-dependency-graph/v1")
    {
        return Ok(DurableGraphDependencies {
            readiness: invalid("durable fanout dependency graph has an unsupported schema".into()),
        });
    }
    let Some(readiness) = graph.get("readiness").and_then(Value::as_object) else {
        return Ok(DurableGraphDependencies {
            readiness: invalid(
                "durable fanout dependency graph has no readiness projection".into(),
            ),
        });
    };
    let Some(states) = readiness.get("states").and_then(Value::as_object) else {
        return Ok(DurableGraphDependencies {
            readiness: invalid("durable fanout dependency graph has no child states".into()),
        });
    };
    let Some(ready) = readiness.get("ready").and_then(Value::as_array) else {
        return Ok(DurableGraphDependencies {
            readiness: invalid("durable fanout dependency graph has no ready frontier".into()),
        });
    };
    let ready = ready
        .iter()
        .map(Value::as_str)
        .collect::<Option<BTreeSet<_>>>();
    let Some(ready) = ready else {
        return Ok(DurableGraphDependencies {
            readiness: invalid(
                "durable fanout dependency graph has an invalid ready frontier".into(),
            ),
        });
    };
    let blocked_paths = readiness
        .get("blocked_paths")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut projection = BTreeMap::new();
    for child in &batch_record.child_runs {
        let Some(state) = states.get(&child.task_id).and_then(Value::as_str) else {
            return Ok(DurableGraphDependencies {
                readiness: invalid(format!(
                    "durable fanout dependency graph omits child '{}'",
                    child.task_id
                )),
            });
        };
        if ready.contains(child.task_id.as_str()) {
            projection.insert(
                child.task_id.clone(),
                supervisor::FanoutDependencyReadiness::Ready,
            );
            continue;
        }
        let path = blocked_paths
            .get(&child.task_id)
            .and_then(Value::as_array)
            .and_then(|path| path.iter().map(Value::as_str).collect::<Option<Vec<_>>>())
            .map(|path| path.join(" <- "));
        projection.insert(
            child.task_id.clone(),
            supervisor::FanoutDependencyReadiness::Blocked {
                detail: path
                    .unwrap_or_else(|| format!("dependency graph projects child state '{state}'")),
                evidence_ref: format!(
                    "homeboy://agent-task/batch/{}/dependency-graph/children/{}",
                    batch_record.batch_id, child.task_id
                ),
            },
        );
    }
    Ok(DurableGraphDependencies {
        readiness: projection,
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
    let base = declared_base.and_then(|base| {
        let reference = format!("refs/homeboy/fanout/base/{base}");
        let fetch = Command::new("git")
            .args([
                "fetch",
                "--no-tags",
                "origin",
                &format!("refs/heads/{base}:{reference}"),
            ])
            .current_dir(path)
            .output()
            .ok()?;
        if !fetch.status.success() {
            return None;
        }
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

#[cfg(test)]
fn batch_resume_result(
    report: agent_task_service::AgentTaskCookBatchReport,
    exit_code: i32,
    batch_id: &str,
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
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
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
    let mut plan = load_batch_cook_fanout_plan(&args.input)?;
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
        }),
    )?;
    Ok(())
}

fn run_batch_cook_fanout_plan_with_attempt_dispatcher(
    plan: BatchCookFanoutPlan,
    attempt_dispatcher: &CookAttemptDispatcherFactory,
) -> CmdResult<Value> {
    persist_fanout_run_batch_record(&plan)?;
    let result = agent_task_service::run_cook_batch(
        agent_task_service::AgentTaskCookBatchOptions {
            batch_id: plan.fanout_id.clone(),
            cooks: compile_batch_cooks(&plan, |options| {
                options.attempt_dispatcher = Some(attempt_dispatcher(options));
            })?,
            max_concurrency: batch_worker_limit(&plan),
        },
        provider::ExtensionProviderAgentTaskExecutor::discover(),
    )?;
    record_terminal_batch_admission_failures(&plan, &result.value)?;
    Ok(batch_cook_result(&plan, result))
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
    persist_fanout_run_batch_record(&plan)?;
    let result = agent_task_service::run_cook_batch(
        agent_task_service::AgentTaskCookBatchOptions {
            batch_id: plan.fanout_id.clone(),
            cooks: compile_batch_cooks(&plan, |_| {})?,
            max_concurrency: batch_worker_limit(&plan),
        },
        executor,
    )?;
    record_terminal_batch_admission_failures(&plan, &result.value)?;
    Ok(batch_cook_result(&plan, result))
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
    plan.cooks
        .iter()
        .map(|cook| {
            let invocation = cook.to_cook_invocation(plan)?;
            let mut options =
                agent_task_service::compile_cook_attempt(invocation.options, invocation.dispatch)?;
            options.harvest_context = harvest_context.clone();
            configure(&mut options);
            Ok(options)
        })
        .collect()
}

fn batch_worker_limit(plan: &BatchCookFanoutPlan) -> usize {
    plan.cooks
        .len()
        .min(std::thread::available_parallelism().map_or(1, usize::from))
}

fn batch_cook_result(
    plan: &BatchCookFanoutPlan,
    result: agent_task_service::AgentTaskRunResult<agent_task_service::AgentTaskCookBatchReport>,
) -> (Value, i32) {
    let report = result.value;
    let cooks = report
        .cooks
        .iter()
        .zip(&plan.cooks)
        .map(|(cell, cook)| {
            let cell_result = cell
                .result
                .as_ref()
                .map(|result| serde_json::to_value(result).unwrap_or(Value::Null))
                .unwrap_or_else(|| serde_json::json!({ "error": cell.error }));
            serde_json::json!({
                "cook_id": cook.cook_id,
                "run_id": cook.run_id(),
                "worktree": cook.to_worktree,
                "head": cook.head,
                "workspace_materialization": cook.workspace_materialization,
                "exit_code": cell.exit_code,
                "result": cell_result,
            })
        })
        .collect::<Vec<_>>();
    (
        serde_json::json!({
            "schema": AGENT_TASK_BATCH_COOK_FANOUT_RUN_SCHEMA,
            "fanout_id": plan.fanout_id,
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
    // Checked before provider readiness, worktree creation, or any other
    // expensive preflight, so an operator who followed --help does not discover
    // the requirement only after discovery succeeds (#9838).
    if !args.gates.has_deterministic_gate() {
        return Err(Error::validation_invalid_argument(
            "verify",
            "agent-task fanout cook-batch requires at least one deterministic gate: pass --verify or --private-verify",
            None,
            Some(vec![
                "Add a public gate, e.g. --verify 'cargo test --workspace'.".to_string(),
                "Use --private-verify for a gate whose output may contain secrets.".to_string(),
                "A child cook that cannot verify its work cannot promote it, so the gate is not optional.".to_string(),
            ]),
        ));
    }
    apply_provider_profile(&mut args);
    // Resolve the effective backend (explicit --backend or the configured
    // default) and validate it up front (#7717). Otherwise an omitted
    // --backend silently rode a config default like `codebox` all the way to
    // provider execution, where each child cook failed late with a
    // provider-shaped `no extension agent-task provider found for backend`
    // error instead of an early, actionable configuration failure. Making the
    // effective backend explicit here also surfaces it in the preflight and
    // pins every child cook to the same resolved backend.
    resolve_and_validate_effective_backend(&mut args)?;
    let mut plan = build_cook_batch_plan(&args)?;
    let branches = plan
        .cooks
        .iter()
        .map(|cook| {
            let branch = cook.head.clone().expect("generated cooks have heads");
            (branch, cook.to_worktree.clone())
        })
        .collect::<Vec<_>>();
    let worktrees = queue_or_reuse_worktrees(&args, &branches)?;
    bind_materialized_worktree_paths(&mut plan, &worktrees);
    let blocked = worktrees
        .rows
        .iter()
        .filter(|row| {
            !matches!(
                row.status,
                worktree::WorktreeQueueCreateStatus::Created
                    | worktree::WorktreeQueueCreateStatus::Queued
            )
        })
        .count();
    let can_run = !args.dry_run
        && blocked == 0
        && worktrees
            .rows
            .iter()
            .all(|row| matches!(row.status, worktree::WorktreeQueueCreateStatus::Created));
    if can_run {
        bind_materialized_worktrees(&mut plan, &worktrees)?;
        // Compare the exact workspace-bound recipe that provider execution will
        // persist, not the handle-only planning form created before worktree
        // materialization.
        preflight_batch_cook_recipes(&plan, attempt_dispatcher)?;
    } else if args.dry_run {
        preflight_batch_cook_recipes(&plan, attempt_dispatcher)?;
    }
    let run_result = if args.run_plan && can_run {
        let (value, exit_code) = match attempt_dispatcher {
            Some(dispatcher) => {
                run_batch_cook_fanout_plan_with_attempt_dispatcher(plan.clone(), dispatcher)?
            }
            None => run_batch_cook_fanout_plan(plan.clone())?,
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
        "planned"
    } else {
        "ready"
    };
    // A completed batch's aggregate result is authoritative. Worktree blocking
    // remains a pre-execution failure, while child failures retain their durable
    // evidence and produce the same nonzero result at every CLI boundary.
    let exit_code = cook_batch_outer_exit_code(blocked, &run_result);

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
                "deterministic_gates": {
                    "verify": args.gates.verify,
                    "private_verify": args.gates.private_verify,
                }
            },
            "worktrees": worktrees,
            "plan": plan,
            "run_result": run_result,
            "commands": cook_batch_commands(&args),
            "next_actions": cook_batch_next_actions(status, args.run_plan, blocked),
        }),
        exit_code,
    ))
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
        cook.workspace = worktrees
            .rows
            .iter()
            .find(|row| row.handle == cook.to_worktree)
            .and_then(|row| row.path.clone());
    }
}

fn queue_or_reuse_worktrees(
    args: &AgentTaskFanoutCookBatchArgs,
    branches: &[(String, String)],
) -> Result<worktree::WorktreeQueueCreateOutput> {
    let queue_create = |create_branches: Vec<String>, dry_run: bool| {
        worktree::queue_create(worktree::WorktreeQueueCreateOptions {
            repo: args.repo.clone(),
            branches: create_branches,
            from: args.from.clone(),
            task_url: None,
            task_ref: None,
            dry_run,
            retry_after_seconds: 30,
        })
    };

    if args.dry_run {
        return queue_create(
            branches.iter().map(|(branch, _)| branch.clone()).collect(),
            true,
        );
    }

    let mut reused = Vec::new();
    let mut to_create = Vec::new();
    for (branch, handle) in branches {
        match worktree::status(handle) {
            Ok(status)
                if status.record.state == worktree::TaskWorktreeState::Active
                    && !status.safety.worktree_missing =>
            {
                reused.push(worktree::WorktreeQueueCreateRow {
                    branch: branch.clone(),
                    handle: handle.clone(),
                    status: worktree::WorktreeQueueCreateStatus::Created,
                    command: worktree_create_command(args, branch),
                    retry_after_seconds: None,
                    active_lock_holder: None,
                    path: Some(status.record.worktree_path),
                    error: None,
                });
            }
            _ => to_create.push(branch.clone()),
        }
    }

    let created = queue_create(to_create, false)?;
    let mut rows = Vec::new();
    for (branch, handle) in branches {
        if let Some(row) = reused.iter().find(|row| row.handle == *handle) {
            rows.push(row.clone());
        } else if let Some(row) = created.rows.iter().find(|row| row.branch == *branch) {
            rows.push(row.clone());
        }
    }

    Ok(worktree::WorktreeQueueCreateOutput {
        schema: "homeboy/worktree-queue-create/v1",
        repo: args.repo.clone(),
        base_ref: args.from.clone(),
        dry_run: false,
        rows,
    })
}

fn preflight_batch_cook_recipes(
    plan: &BatchCookFanoutPlan,
    attempt_dispatcher: Option<&CookAttemptDispatcherFactory>,
) -> Result<()> {
    // Planning and dry-run callers may only have a managed worktree handle.
    // Validate immutable recipe inputs without resolving that handle as a live
    // workspace; execution validates the materialized workspace separately.
    for cook in &plan.cooks {
        let mut invocation = cook.to_cook_invocation(plan)?;
        invocation.options.harvest_context = batch_harvest_context()?;
        if let Some(dispatcher) = attempt_dispatcher {
            invocation.options.attempt_dispatcher = Some(dispatcher(&invocation.options));
        }
        agent_task_service::validate_initial_recipe_compatibility(&invocation.options)?;
    }
    Ok(())
}

fn load_fanout_agent_task_plan(
    args: &AgentTaskFanoutInputArgs,
) -> Result<homeboy::agents::agent_tasks::scheduler::AgentTaskPlan> {
    agent_task_service::read_plan(&args.input)
}

fn load_batch_cook_fanout_plan(args: &AgentTaskFanoutInputArgs) -> Result<BatchCookFanoutPlan> {
    let raw = config::read_json_spec_to_string(&args.input)?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| {
        Error::validation_invalid_json(
            error,
            Some("agent-task fanout batch-cook input".to_string()),
            Some(raw.clone()),
        )
    })?;
    BatchCookFanoutPlan::from_value(value, args)
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookFanoutPlan {
    #[serde(default = "batch_cook_fanout_plan_schema")]
    schema: String,
    fanout_id: String,
    cooks: Vec<BatchCookSpec>,
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
        Ok(plan)
    }

    fn rekey(&mut self, fanout_id: String) {
        let previous_prefix = format!("{}-", self.fanout_id);
        for cook in &mut self.cooks {
            let cell_id = cook
                .cook_id
                .strip_prefix(&previous_prefix)
                .unwrap_or(&cook.cook_id);
            cook.cook_id = format!("{fanout_id}-{cell_id}");
        }
        self.fanout_id = fanout_id;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
struct BatchCookSpec {
    cook_id: String,
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
    #[serde(default = "one")]
    attempts: u32,
    #[serde(default)]
    same_provider_retries: u32,
    #[serde(default)]
    provider_rotations: u32,
    #[serde(default = "one_usize")]
    concurrency: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_config: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_context: Option<String>,
    to_worktree: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    provider_command: Option<String>,
    #[serde(default)]
    verify: Vec<String>,
    #[serde(default)]
    private_verify: Vec<String>,
    #[serde(default = "default_private_gate_reveal")]
    private_gate_reveal: AgentTaskGateRevealPolicy,
    #[serde(default)]
    execution_policy: AgentTaskGateExecutionPolicy,
    #[serde(default = "default_gate_timeout_seconds")]
    gate_timeout_seconds: u64,
    #[serde(default = "default_gate_heartbeat_interval_seconds")]
    gate_heartbeat_interval_seconds: u64,
    #[serde(default)]
    rerun_completed_gates: bool,
    #[serde(default)]
    gate_environment: AgentTaskGateEnvironmentPolicy,
    #[serde(default)]
    gate_toolchains: Vec<homeboy::agents::agent_tasks::gate::AgentTaskGateToolchainRequirement>,
    #[serde(default = "default_max_attempts")]
    max_attempts: u32,
    #[serde(default)]
    no_finalize: bool,
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
        let dispatch = AgentTaskDispatchCommand {
            prompt: self.prompt.clone(),
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
                provider_config: self.provider_config.clone(),
                client_context: Some(merged_client_context(plan, self)),
                attempts: self.attempts,
                same_provider_retries: self.same_provider_retries,
                provider_rotations: self.provider_rotations,
                queue_only: false,
                timeout_ms: None,
                resolved_provider_policy: None,
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
                    private_gate_reveal: self.private_gate_reveal,
                    execution_policy: self.execution_policy,
                    gate_timeout_seconds: self.gate_timeout_seconds,
                    gate_heartbeat_interval_seconds: self.gate_heartbeat_interval_seconds,
                    rerun_completed_gates: self.rerun_completed_gates,
                    gate_environment: self.gate_environment.clone(),
                    gate_toolchains: self.gate_toolchains.clone(),
                    gate_diagnostic_sidecars: Vec::new(),
                    hydrate_dependencies: true,
                },
                max_attempts: self.max_attempts,
                no_finalize: self.no_finalize,
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

fn build_cook_batch_plan(args: &AgentTaskFanoutCookBatchArgs) -> Result<BatchCookFanoutPlan> {
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
        cooks.push(BatchCookSpec {
            cook_id: format!("issue-{}", issue.number),
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
            attempts: 1,
            same_provider_retries: 0,
            provider_rotations: 0,
            concurrency: 1,
            provider_config: args.provider_config.clone(),
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
            verify: args.gates.verify.clone(),
            private_verify: args.gates.private_verify.clone(),
            private_gate_reveal: args.gates.private_gate_reveal,
            execution_policy: VerifyGateOptions::from(args.gates.clone()).execution_policy,
            gate_timeout_seconds: args.gates.gate_timeout_seconds,
            gate_heartbeat_interval_seconds: args.gates.gate_heartbeat_interval_seconds,
            rerun_completed_gates: args.gates.rerun_completed_gates,
            gate_environment: VerifyGateOptions::from(args.gates.clone()).gate_environment,
            gate_toolchains: VerifyGateOptions::from(args.gates.clone()).gate_toolchains,
            max_attempts: default_max_attempts(),
            no_finalize: false,
            base: args.base.clone(),
            head: Some(branch),
            title: Some(format!("Fix {}", issue.key)),
            commit_message: Some(format!("fix: address {}", issue.key)),
            protected_branches: super::review::default_protected_branches(),
            ai_tool: default_ai_tool(),
            ai_used_for: default_ai_used_for(),
        });
    }
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
        metadata: serde_json::json!({
            "source": "agent-task fanout cook-batch",
            "issue_count": args.issues.len(),
            "repo": args.repo,
            "base": args.base,
            "from": args.from,
        }),
    })
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
            .split(|c| matches!(c, '/' | '?' | '#'))
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

fn cook_batch_commands(args: &AgentTaskFanoutCookBatchArgs) -> Value {
    let issues = args.issues.join(" ");
    serde_json::json!({
        "plan": format!("homeboy agent-task fanout cook-batch --repo {} --dry-run {}", args.repo, issues),
        "run": format!("homeboy agent-task fanout cook-batch --repo {} --run-plan {}", args.repo, issues),
        "status": "inspect each cook result under plan.cooks and use agent-task status <run-id>",
        "retry": "rerun this cook-batch after fixing provider/worktree blockers, or rerun the blocked issue URL only",
        "resume_from_plan": "save .plan to JSON and run homeboy agent-task fanout run-plan --input @batch-cook-plan.json",
    })
}

fn cook_batch_next_actions(status: &str, run_plan: bool, blocked: usize) -> Vec<String> {
    if blocked > 0 {
        return vec![
            "repair worktree queue blockers reported under worktrees.rows".to_string(),
            "rerun the same cook-batch command; created worktrees are recorded and blocked rows carry retry commands".to_string(),
        ];
    }
    if run_plan {
        return vec![format!(
            "batch execution {status}; inspect run_result.result.cooks for PR/finalization outcomes"
        )];
    }
    vec![
        "review plan.cooks and provider_readiness_command before execution".to_string(),
        "rerun with --run-plan or save plan to JSON and run homeboy agent-task fanout run-plan --input @batch-cook-plan.json".to_string(),
    ]
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

fn one() -> u32 {
    1
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

fn default_ai_tool() -> String {
    AgentTaskProviderCatalog::discover()
        .providers()
        .iter()
        .find_map(|provider| provider.cli.default_ai_disclosure.clone())
        .or_else(|| {
            AgentTaskProviderCatalog::discover()
                .providers()
                .iter()
                .flat_map(|provider| provider.cli.profiles.iter())
                .find_map(|profile| profile.ai_disclosure.clone())
        })
        .unwrap_or_else(|| GENERIC_AI_DISCLOSURE.to_string())
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
            gates: super::super::args::VerifyGateArgs {
                verify: vec!["cargo test --lib".to_string()],
                private_verify: Vec::new(),
                private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                gate_execution_policy: "ordered-fail-fast".to_string(),
                gate_timeout_seconds: 30 * 60,
                gate_heartbeat_interval_seconds: 5,
                rerun_completed_gates: false,
                gate_environment_mode: "inherit".to_string(),
                gate_environment: Vec::new(),
                gate_environment_preserve: Vec::new(),
                gate_toolchains: Vec::new(),
                isolate_gate_home: true,
                isolate_gate_xdg: true,
            },
            dry_run: true,
            run_plan: false,
        }
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
    fn recipe_preflight_accepts_exact_replay_and_rejects_changed_inputs() {
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
            let error = preflight_batch_cook_recipes(&changed, None)
                .expect_err("changed execution inputs conflict");
            assert!(error.message.contains("different execution inputs"));
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
        let (value, exit_code) = cook_batch(cook_batch_args()).expect("cook batch dry run");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-cook-batch/v1");
        assert_eq!(value["status"], "planned");
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
        assert_eq!(value["worktrees"]["rows"][0]["status"], "queued");
        assert!(value["commands"]["resume_from_plan"]
            .as_str()
            .expect("resume command")
            .contains("fanout run-plan"));
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
                        error: Some("controller admission failed".to_string()),
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
        let (data, exit_code) = batch_cook_result(&plan, result);
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
        );
        let (resumed, resume_exit_code) =
            batch_resume_result(active_failed_report, 0, "test-batch");
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
        let mut args = cook_batch_args();
        args.backend = None;
        args.model = None;
        args.provider_profile = Some("example-profile".to_string());

        let (value, exit_code) = cook_batch(args).expect("cook batch dry run");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["preflight"]["provider_selection"]["profile"],
            "example-profile"
        );
        assert!(value["preflight"]["provider_selection"]["warnings"][0]
            .as_str()
            .expect("warning")
            .contains("not declared"));
    }

    #[test]
    fn cook_batch_does_not_warn_for_specific_backend_names_in_core() {
        let mut args = cook_batch_args();
        args.backend = Some("example".to_string());
        args.provider_config = None;

        let (value, exit_code) = cook_batch(args).expect("cook batch dry run");

        assert_eq!(exit_code, 0);
        assert!(value["preflight"]["provider_selection"]["warnings"]
            .as_array()
            .expect("warnings")
            .is_empty());
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
        assert_eq!(args.from, "origin/main");
        assert_eq!(
            args.provider_profile,
            Some("opencode-codex-gpt55".to_string())
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
