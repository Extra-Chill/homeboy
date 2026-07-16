//! Agent-task cook orchestration: the deterministic provider → promote → loop
//! → finalize attempt cycle plus its report/options types and promotion-source
//! resolution. Pure move out of the former `agent_task_service.rs` god-file.

use serde_json::Value;
use sha2::Digest;
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use crate::agent_task::AgentTaskExecutor;
use crate::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::agent_task_finalization::{
    finalize_pr_with_backend, AgentTaskPrEvidence, AgentTaskPrFinalizationBackend,
    AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails, AgentTaskPrSourceRelationship,
    AgentTaskPrVerification, RealAgentTaskPrFinalizationBackend,
};
use crate::agent_task_gate::VerifyGateOptions;
use crate::agent_task_lifecycle;
use crate::agent_task_promotion::{
    normalize_promotion_patch, promote, AgentTaskPromotionOptions, AgentTaskPromotionReport,
    AgentTaskPromotionStatus,
};
use crate::agent_task_review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewTestStep,
};
use crate::agent_task_scheduler::{
    AgentTaskExecutionBudget, AgentTaskExecutorAdapter, AgentTaskPlan, AgentTaskState,
};
use crate::command_invocation::CommandInvocation;
use crate::{config, Error, Result};

use super::execution::run_loaded_plan_with_derived_cook_baseline;
use super::AgentTaskRunResult;

/// Executes one provider attempt while cook retains ownership of promotion,
/// gates, retries, and finalization.
pub trait AgentTaskCookAttemptDispatcher: Send + Sync + std::fmt::Debug {
    /// Durable, generic transport descriptor used to reconstruct this
    /// dispatcher in a fresh controller process.
    fn durable_recipe(&self) -> Result<Value>;

    /// Establish transport readiness before the cook pins its runtime
    /// generation. A reconnect can promote the runner runtime, which must not
    /// wait on the cook that needs that reconnect.
    fn prepare_for_cook(&self) -> Result<()> {
        Ok(())
    }

    /// `derived_cook_baseline` is process-local authority for a gate-fix retry.
    /// Implementations must not serialize it into the provider request.
    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()>;
}

#[derive(Debug, Clone)]
pub struct AgentTaskCookServiceOptions {
    pub cook_id: String,
    pub initial_run_id: String,
    /// Controller-compiled first attempt. The cook service owns dispatching it
    /// through the same local-or-Lab transport used by gate-feedback retries.
    pub initial_plan: AgentTaskPlan,
    pub to_worktree: String,
    pub source_worktree_path: Option<PathBuf>,
    pub provider_command: Option<String>,
    pub provider_invocation: Option<CommandInvocation>,
    /// Shared deterministic verification gate fields, factored out of the
    /// per-field duplication that previously spanned the loop/promote types.
    pub gates: VerifyGateOptions,
    pub max_attempts: u32,
    pub no_finalize: bool,
    pub base: String,
    pub task_base_sha: Option<String>,
    pub head: Option<String>,
    pub title: String,
    pub commit_message: String,
    pub source_refs: Vec<String>,
    pub protected_branches: Vec<String>,
    pub ai_tool: String,
    pub ai_model: Option<String>,
    pub ai_used_for: String,
    /// The route-selected provider transport. `None` executes locally.
    pub attempt_dispatcher: Option<Arc<dyn AgentTaskCookAttemptDispatcher>>,
    pub harvest_context: crate::agent_task_scheduler::HarvestExecutionContext,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookReport {
    pub schema: &'static str,
    pub cook_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latest_run_id: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub history_run_ids: Vec<String>,
    pub status: String,
    pub attempts: Vec<AgentTaskCookAttemptReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub finalization: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentTaskCookAttemptReport {
    pub attempt: u32,
    pub run_id: String,
    pub run_state: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub aggregate_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub promotion: Option<AgentTaskPromotionReport>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub feedback: Option<AgentTaskCookLoopReport>,
}

pub fn run_cook<E>(
    options: AgentTaskCookServiceOptions,
    executor: E,
) -> Result<AgentTaskRunResult<AgentTaskCookReport>>
where
    E: AgentTaskExecutorAdapter + Clone,
{
    // The durable reconstruction boundary must exist before an external provider
    // can accept the first attempt.
    super::persist_initial_recipe(&options)?;
    let max_attempts = options.max_attempts.max(1);
    let mut attempts = Vec::new();
    let mut run_id = options.initial_run_id.clone();
    let mut next_plan = Some(options.initial_plan.clone());
    let cook_id = options.cook_id.clone();
    let mut budget_limit = None;
    let mut observed_budget_used = ExecutionBudgetUsage::default();
    let mut remediation_category_usage = ExecutionBudgetUsage::default();

    for attempt in 1..=max_attempts {
        let plan = match next_plan.take() {
            Some(plan) => plan,
            None => agent_task_lifecycle::load_plan(&run_id)?,
        };
        let needs_execution = agent_task_lifecycle::status(&run_id)
            .map(|record| {
                !matches!(
                    record.state,
                    agent_task_lifecycle::AgentTaskRunState::Succeeded
                        | agent_task_lifecycle::AgentTaskRunState::PartialFailure
                        | agent_task_lifecycle::AgentTaskRunState::Failed
                        | agent_task_lifecycle::AgentTaskRunState::Cancelled
                )
            })
            .unwrap_or(true);
        if needs_execution {
            let execution = if let Some(dispatcher) = &options.attempt_dispatcher {
                dispatcher.dispatch_attempt(plan.clone(), &run_id, None)
            } else {
                run_loaded_plan_with_derived_cook_baseline(
                    plan.clone(),
                    Some(&run_id),
                    executor.clone(),
                    None,
                    Some(cook_attempt_harvest_context(&options.harvest_context)),
                )
                .map(|_| ())
            };
            if let Err(error) = execution {
                let record = agent_task_lifecycle::status(&run_id).ok();
                if record.is_some() {
                    agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
                }
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: record
                        .as_ref()
                        .map(|record| format!("{:?}", record.state))
                        .unwrap_or_else(|| "DispatchFailed".to_string()),
                    aggregate_path: record.and_then(|record| record.aggregate_path),
                    promotion: None,
                    feedback: None,
                });
                return Ok(cook_report(
                    cook_id,
                    "provider_failure",
                    attempts,
                    None,
                    Some(error.to_string()),
                    1,
                ));
            }
        }
        agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
        let record = agent_task_lifecycle::status(&run_id)?;
        if record.state == agent_task_lifecycle::AgentTaskRunState::Running
            && record.runner_job_id().is_some()
        {
            // A detached runner handoff has durably accepted the provider
            // child. Its daemon owns timeout and provider rotation; this
            // controller returns without attempting to read a future aggregate.
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(cook_report(
                cook_id,
                "in_flight",
                attempts,
                None,
                Some("provider attempt accepted by the runner daemon".to_string()),
                0,
            ));
        }
        let plan = agent_task_lifecycle::load_plan_for_execution(&run_id)?;
        budget_limit.get_or_insert_with(|| plan.options.execution_budget.clone());
        let aggregate = agent_task_lifecycle::read_aggregate(&run_id)?;
        observed_budget_used.add(execution_budget_usage(&aggregate));
        let mut budget_used = observed_budget_used;
        budget_used.same_provider_retries = budget_used
            .same_provider_retries
            .saturating_add(remediation_category_usage.same_provider_retries);
        budget_used.provider_rotations = budget_used
            .provider_rotations
            .saturating_add(remediation_category_usage.provider_rotations);
        let Some(source_request) = plan.tasks.first().cloned() else {
            return Ok(cook_report(
                cook_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task cook requires a plan with one source task".to_string()),
                1,
            ));
        };
        if plan.tasks.len() != 1 {
            return Ok(cook_report(
                cook_id,
                "policy_failure",
                attempts,
                None,
                Some("agent-task cook currently supports one task per cook attempt".to_string()),
                1,
            ));
        }

        if !matches!(
            record.state,
            agent_task_lifecycle::AgentTaskRunState::Succeeded
                | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
        ) {
            attempts.push(AgentTaskCookAttemptReport {
                attempt,
                run_id: run_id.clone(),
                run_state: format!("{:?}", record.state),
                aggregate_path: record.aggregate_path,
                promotion: None,
                feedback: None,
            });
            return Ok(cook_report(
                cook_id,
                "provider_failure",
                attempts,
                None,
                Some(format!(
                    "agent-task run {run_id} ended in state {:?}",
                    record.state
                )),
                1,
            ));
        }

        let promotion = match promote_or_load_attempt(&options, &run_id) {
            Ok(report) => report,
            Err(error) => {
                attempts.push(AgentTaskCookAttemptReport {
                    attempt,
                    run_id: run_id.clone(),
                    run_state: format!("{:?}", record.state),
                    aggregate_path: record.aggregate_path,
                    promotion: None,
                    feedback: None,
                });
                return Ok(cook_report(
                    cook_id,
                    "policy_failure",
                    attempts,
                    None,
                    Some(error.to_string()),
                    1,
                ));
            }
        };

        let feedback = evaluate_cook_loop(AgentTaskCookLoopOptions {
            source_request,
            promotion_report: promotion.clone(),
            attempt,
            max_attempts,
            source_run_id: Some(run_id.clone()),
            current_diff: String::new(),
            metadata: Value::Null,
        });
        let feedback_status = feedback.status;
        let follow_up_request = feedback.follow_up_request.clone();
        attempts.push(AgentTaskCookAttemptReport {
            attempt,
            run_id: run_id.clone(),
            run_state: format!("{:?}", record.state),
            aggregate_path: record.aggregate_path,
            promotion: Some(promotion.clone()),
            feedback: Some(feedback.clone()),
        });

        match feedback_status {
            AgentTaskCookLoopStatus::GreenCompleted => {
                if options.no_finalize {
                    return Ok(cook_report(
                        cook_id,
                        "green_no_finalize",
                        attempts,
                        None,
                        Some(
                            "deterministic gates completed green; --no-finalize skipped commit, push, and PR finalization"
                                .to_string(),
                        ),
                        0,
                    ));
                }
                let finalization = finalize_cook_pr(&options, &run_id, &promotion)?;
                let final_status = finalization["status"]
                    .as_str()
                    .unwrap_or("unknown")
                    .to_string();
                let exit_code = if final_status == "review_ready" { 0 } else { 1 };
                let stop_reason = (final_status == "no_changes").then(|| {
                    "cook completed provider execution and gates, but finalization found no changed files; task likely still requires review or retry".to_string()
                });
                return Ok(cook_report(
                    cook_id,
                    &final_status,
                    attempts,
                    Some(finalization),
                    stop_reason,
                    exit_code,
                ));
            }
            AgentTaskCookLoopStatus::NoChanges => {
                return Ok(cook_report(
                    cook_id,
                    "no_changes",
                    attempts,
                    None,
                    Some(
                        "cook completed provider execution but produced no changed files; task likely still requires review or retry"
                            .to_string(),
                    ),
                    1,
                ));
            }
            AgentTaskCookLoopStatus::RetryRequested => {
                let Some(mut follow_up_request) = follow_up_request else {
                    return Ok(cook_report(
                        cook_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cook feedback requested retry without a follow-up request".to_string(),
                        ),
                        1,
                    ));
                };
                let next_run_id = agent_task_lifecycle::cook_attempt_run_id(&cook_id, attempt + 1);
                let Some(remaining_budget) = budget_limit
                    .as_ref()
                    .and_then(|budget| budget_remaining(budget, budget_used))
                else {
                    return Ok(cook_report(
                        cook_id,
                        "execution_budget_exhausted",
                        attempts,
                        None,
                        Some("provider execution stopped because max_provider_executions was exhausted".to_string()),
                        1,
                    ));
                };
                let Some(same_provider) =
                    terminal_executor_matches(&aggregate, &follow_up_request.executor)
                else {
                    return Ok(cook_report(
                        cook_id,
                        "policy_failure",
                        attempts,
                        None,
                        Some(
                            "cannot classify Cook remediation without terminal executor identity"
                                .to_string(),
                        ),
                        1,
                    ));
                };
                let reservation = match reserve_remediation_budget(&remaining_budget, same_provider)
                {
                    Ok(reservation) => reservation,
                    Err(exhausted_budget) => {
                        return Ok(cook_report(
                            cook_id,
                            "execution_budget_exhausted",
                            attempts,
                            None,
                            Some(format!("provider execution stopped because {exhausted_budget} was exhausted")),
                            1,
                        ));
                    }
                };
                // This is durable evidence, not the process-local baseline
                // capability. A restarted controller re-materializes and
                // verifies that capability from the exact promoted artifact.
                follow_up_request.inputs["cook_loop"]["artifact_provenance"] = serde_json::json!({
                    "source_run_id": run_id,
                    "source_task_id": promotion.source.task_id,
                    "source_patch_artifact_sha256": promotion.patch_artifact.sha256,
                });
                let mut follow_up_plan = AgentTaskPlan::new(
                    format!("{cook_id}-cook-attempt-{}", attempt + 1),
                    vec![follow_up_request],
                );
                follow_up_plan.options = plan.options.clone();
                follow_up_plan.options.execution_budget = AgentTaskExecutionBudget::new(1, 0, 0);
                follow_up_plan.options.retry.max_attempts = 1;
                super::record_recipe_attempt(&cook_id, attempt + 1, &next_run_id, &follow_up_plan)?;
                // A restarted controller may find this exact retry already
                // accepted or terminal. Its durable recipe is the dispatch
                // boundary; never send the provider a second copy.
                if attempt_needs_execution(&next_run_id) {
                    let baseline = match materialize_follow_up_baseline(&promotion, &run_id) {
                        Ok(baseline) => baseline,
                        Err(error) => {
                            return Ok(cook_report(
                                cook_id,
                                "policy_failure",
                                attempts,
                                None,
                                Some(error.to_string()),
                                1,
                            ));
                        }
                    };
                    let mut follow_up_plan = follow_up_plan;
                    follow_up_plan.tasks[0].workspace.root =
                        Some(baseline.path.display().to_string());
                    // Requests retain artifact facts for review, never authorization.
                    follow_up_plan.tasks[0].inputs["cook_loop"]["artifact_provenance"] =
                        baseline.artifact_provenance();
                    if let Some(dispatcher) = &options.attempt_dispatcher {
                        dispatcher.dispatch_attempt(
                            follow_up_plan,
                            &next_run_id,
                            Some(baseline.capability()),
                        )?;
                    } else {
                        run_loaded_plan_with_derived_cook_baseline(
                            follow_up_plan,
                            Some(&next_run_id),
                            executor.clone(),
                            Some(baseline.capability()),
                            Some(cook_attempt_harvest_context(&options.harvest_context)),
                        )?;
                    }
                }
                remediation_category_usage.add(reservation);
                run_id = next_run_id;
            }
            AgentTaskCookLoopStatus::RetriesExhausted => {
                return Ok(cook_report(
                    cook_id,
                    "retries_exhausted",
                    attempts,
                    None,
                    Some(
                        "deterministic gates stayed red after the configured attempt budget"
                            .to_string(),
                    ),
                    1,
                ));
            }
        }
    }

    Ok(cook_report(
        cook_id,
        "retries_exhausted",
        attempts,
        None,
        Some("cook attempt budget exhausted".to_string()),
        1,
    ))
}

/// A cook-owned detached checkout turns the already-promoted dirty candidate
/// into a clean commit before the scheduler creates its normal attempt checkout.
struct CookFollowUpBaseline {
    source_root: PathBuf,
    path: PathBuf,
    capability: DerivedCookBaselineCapability,
}

fn cook_attempt_harvest_context(
    harvest_context: &crate::agent_task_scheduler::HarvestExecutionContext,
) -> crate::agent_task_scheduler::HarvestExecutionContext {
    harvest_context.clone()
}

/// Process-local authority for one materialized cook retry baseline. It is not
/// serializable and never enters a request, environment, or durable record.
pub struct DerivedCookBaselineCapability {
    canonical_path: PathBuf,
    commit: String,
    tree: String,
    artifact_sha256: String,
    source_run_id: String,
    source_task_id: String,
    parent_snapshot: Option<Value>,
}

impl DerivedCookBaselineCapability {
    pub fn canonical_path(&self) -> &std::path::Path {
        &self.canonical_path
    }

    pub(crate) fn commit(&self) -> &str {
        &self.commit
    }

    pub(crate) fn tree(&self) -> &str {
        &self.tree
    }

    pub(crate) fn source_task_id(&self) -> &str {
        &self.source_task_id
    }

    pub(crate) fn parent_snapshot(&self) -> Option<&Value> {
        self.parent_snapshot.as_ref()
    }

    pub(crate) fn artifact_provenance(&self) -> Value {
        serde_json::json!({
            "source_run_id": self.source_run_id,
            "source_task_id": self.source_task_id,
            "source_patch_artifact_sha256": self.artifact_sha256,
        })
    }

    /// Evidence derived from the controller-validated capability. It is not
    /// authorization for remote workspace or snapshot verification.
    pub fn verified_baseline_provenance(&self) -> Value {
        serde_json::json!({
            "source_run_id": self.source_run_id,
            "source_task_id": self.source_task_id,
            "promoted_patch_artifact_sha256": self.artifact_sha256,
            "baseline_commit": self.commit,
            "baseline_tree": self.tree,
            "parent_snapshot_identity": self.parent_snapshot.as_ref().and_then(|snapshot| {
                snapshot
                    .get("workspace_snapshot_identity")
                    .cloned()
                    .or_else(|| snapshot.get("identity").cloned())
            }),
        })
    }
}

impl CookFollowUpBaseline {
    fn capability(&self) -> &DerivedCookBaselineCapability {
        &self.capability
    }

    fn artifact_provenance(&self) -> Value {
        self.capability.artifact_provenance()
    }
}

#[cfg(any(test, feature = "test-support"))]
pub fn test_derived_cook_baseline_capability(
    path: PathBuf,
    commit: String,
    tree: String,
    task_id: &str,
    parent_snapshot: Option<Value>,
) -> DerivedCookBaselineCapability {
    DerivedCookBaselineCapability {
        canonical_path: path
            .canonicalize()
            .expect("test baseline path canonicalizes"),
        commit,
        tree,
        artifact_sha256: "test-artifact-sha256".to_string(),
        source_run_id: "test-source-run".to_string(),
        source_task_id: task_id.to_string(),
        parent_snapshot,
    }
}

impl Drop for CookFollowUpBaseline {
    fn drop(&mut self) {
        let _ = Command::new("git")
            .args(["worktree", "remove", "--force"])
            .arg(&self.path)
            .current_dir(&self.source_root)
            .status();
        let _ = Command::new("git")
            .args(["worktree", "prune"])
            .current_dir(&self.source_root)
            .status();
    }
}

fn materialize_follow_up_baseline(
    promotion: &AgentTaskPromotionReport,
    source_run_id: &str,
) -> Result<CookFollowUpBaseline> {
    let source_root = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "gate-failed promotion did not report its managed target workspace",
                None,
                None,
            )
        })?;
    let expected_head = promotion.target.head.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "promotion.target.head",
            "gate-failed promotion did not record the immutable target HEAD",
            None,
            None,
        )
    })?;
    if git_output(&source_root, &["rev-parse", "HEAD"])? != expected_head {
        return Err(Error::validation_invalid_argument(
            "promotion.target.head",
            "promotion target HEAD changed after the gate-failed promotion; refusing cook retry baseline",
            None,
            None,
        ));
    }
    let parent_snapshot = std::env::var(crate::observation::SOURCE_SNAPSHOT_METADATA_ENV)
        .ok()
        .map(|raw| serde_json::from_str::<Value>(&raw))
        .transpose()
        .map_err(|error| {
            Error::validation_invalid_argument("source_snapshot", error.to_string(), None, None)
        })?;
    let artifact_bytes = std::fs::read(&promotion.patch_artifact.path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read promoted patch artifact".to_string()),
        )
    })?;
    let artifact_sha256 = format!("{:x}", sha2::Sha256::digest(&artifact_bytes));
    if let Some(expected) = promotion.patch_artifact.sha256.as_deref() {
        if expected != artifact_sha256 {
            return Err(Error::validation_invalid_argument(
                "promotion.patch_artifact.sha256",
                "promoted artifact bytes no longer match durable sha256",
                None,
                None,
            ));
        }
    }
    let artifact = std::str::from_utf8(&artifact_bytes).map_err(|error| {
        Error::validation_invalid_argument(
            "promotion.patch_artifact",
            format!("patch bytes are not UTF-8: {error}"),
            None,
            None,
        )
    })?;
    let normalized = normalize_promotion_patch(artifact, &promotion.to_worktree)?;
    let index = tempfile::NamedTempFile::new().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create cook baseline Git index".to_string()),
        )
    })?;
    let index_path = index.path().display().to_string();
    git_output_with_env(
        &source_root,
        &["read-tree", expected_head],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    git_output_with_env(
        &source_root,
        &["add", "--all"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let target_tree = git_output_with_env(
        &source_root,
        &["write-tree"],
        &[("GIT_INDEX_FILE", &index_path)],
    )?;
    let parent = std::env::temp_dir().join("homeboy-cook-follow-up-baselines");
    std::fs::create_dir_all(&parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create cook baseline directory".to_string()),
        )
    })?;
    let path = parent.join(format!("baseline-{}", uuid::Uuid::new_v4()));
    let path_string = path.display().to_string();
    git_output(
        &source_root,
        &["worktree", "add", "--detach", &path_string, expected_head],
    )?;
    let baseline = CookFollowUpBaseline {
        source_root,
        path: path.clone(),
        // The capability is completed only after the committed baseline's
        // identity has been verified below.
        capability: DerivedCookBaselineCapability {
            canonical_path: path,
            commit: String::new(),
            tree: String::new(),
            artifact_sha256,
            source_run_id: source_run_id.to_string(),
            source_task_id: promotion.source.task_id.clone(),
            parent_snapshot,
        },
    };
    let patch_path = baseline.path.join(".homeboy-cook-baseline.patch");
    std::fs::write(&patch_path, normalized.content.as_bytes()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write cook baseline patch".to_string()),
        )
    })?;
    git_output(
        &baseline.path,
        &[
            "apply",
            "--whitespace=nowarn",
            &patch_path.display().to_string(),
        ],
    )?;
    std::fs::remove_file(&patch_path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("remove cook baseline patch".to_string()),
        )
    })?;
    git_output(&baseline.path, &["add", "--all"])?;
    git_output(
        &baseline.path,
        &[
            "-c",
            "user.name=Homeboy",
            "-c",
            "user.email=homeboy@localhost",
            "commit",
            "--no-verify",
            "-m",
            "homeboy: cook promoted baseline",
        ],
    )?;
    let commit = git_output(&baseline.path, &["rev-parse", "HEAD"])?;
    let tree = git_output(&baseline.path, &["rev-parse", "HEAD^{tree}"])?;
    if tree != target_tree {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "promotion target contains extra, missing, or unrelated changes; refusing cook retry baseline",
            None,
            None,
        ));
    }
    let mut baseline = baseline;
    baseline.capability.canonical_path = baseline.path.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("canonicalize cook retry baseline".to_string()),
        )
    })?;
    baseline.capability.commit = commit;
    baseline.capability.tree = tree;
    Ok(baseline)
}

fn git_output(cwd: &std::path::Path, args: &[&str]) -> Result<String> {
    git_output_with_env(cwd, args, &[])
}

fn git_output_with_env(
    cwd: &std::path::Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("git {}", args.join(" "))))
        })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "promotion",
            format!(
                "git {} failed: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct ExecutionBudgetUsage {
    pub(crate) executions: u32,
    pub(crate) same_provider_retries: u32,
    pub(crate) provider_rotations: u32,
}

impl ExecutionBudgetUsage {
    fn add(&mut self, other: Self) {
        self.executions = self.executions.saturating_add(other.executions);
        self.same_provider_retries = self
            .same_provider_retries
            .saturating_add(other.same_provider_retries);
        self.provider_rotations = self
            .provider_rotations
            .saturating_add(other.provider_rotations);
    }
}

pub(crate) fn execution_budget_usage(
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
) -> ExecutionBudgetUsage {
    let executions = aggregate
        .events
        .iter()
        .filter(|event| event.state == AgentTaskState::Running)
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let same_provider_retries = aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.diagnostics)
        .filter(|diagnostic| diagnostic.class == "agent_task.retry_attempt")
        .count()
        .try_into()
        .unwrap_or(u32::MAX);
    let provider_rotations = aggregate
        .outcomes
        .iter()
        .filter_map(provider_rotation_attempts)
        .map(|attempts| attempts.len().saturating_sub(1) as u32)
        .fold(0, u32::saturating_add);
    ExecutionBudgetUsage {
        executions,
        same_provider_retries,
        provider_rotations,
    }
}

pub(crate) fn budget_remaining(
    budget: &AgentTaskExecutionBudget,
    usage: ExecutionBudgetUsage,
) -> Option<AgentTaskExecutionBudget> {
    let max_provider_executions = budget
        .max_provider_executions
        .saturating_sub(usage.executions);
    (max_provider_executions > 0).then(|| {
        AgentTaskExecutionBudget::new(
            max_provider_executions,
            budget
                .max_same_provider_retries
                .saturating_sub(usage.same_provider_retries),
            budget
                .max_provider_rotations
                .saturating_sub(usage.provider_rotations),
        )
    })
}

pub(crate) fn reserve_remediation_budget(
    budget: &AgentTaskExecutionBudget,
    same_provider: bool,
) -> std::result::Result<ExecutionBudgetUsage, &'static str> {
    if budget.max_provider_executions == 0 {
        return Err("max_provider_executions");
    }
    if same_provider {
        if budget.max_same_provider_retries == 0 {
            return Err("max_same_provider_retries");
        }
        return Ok(ExecutionBudgetUsage {
            same_provider_retries: 1,
            ..Default::default()
        });
    }
    if budget.max_provider_rotations == 0 {
        return Err("max_provider_rotations");
    }
    Ok(ExecutionBudgetUsage {
        provider_rotations: 1,
        ..Default::default()
    })
}

fn terminal_executor_matches(
    aggregate: &crate::agent_task_scheduler::AgentTaskAggregate,
    follow_up: &AgentTaskExecutor,
) -> Option<bool> {
    let outcome = aggregate.outcomes.last()?;
    let terminal = terminal_executor_identity(outcome)?;
    Some(
        terminal.backend == follow_up.backend
            && terminal.selector == follow_up.selector
            && terminal.model.as_deref() == follow_up.model(),
    )
}

fn provider_rotation_attempts(
    outcome: &crate::agent_task::AgentTaskOutcome,
) -> Option<Vec<crate::agent_task_scheduler::AgentTaskProviderRotationAttempt>> {
    serde_json::from_value(
        outcome
            .metadata
            .pointer("/provider_rotation/attempts")?
            .clone(),
    )
    .ok()
}

struct TerminalExecutorIdentity {
    backend: String,
    selector: Option<String>,
    model: Option<String>,
}

fn terminal_executor_identity(
    outcome: &crate::agent_task::AgentTaskOutcome,
) -> Option<TerminalExecutorIdentity> {
    if let Some(attempt) =
        provider_rotation_attempts(outcome).and_then(|attempts| attempts.last().cloned())
    {
        return Some(TerminalExecutorIdentity {
            backend: attempt.backend,
            selector: attempt.selector,
            model: attempt.model,
        });
    }
    let executor = outcome.metadata.get("executor")?;
    Some(TerminalExecutorIdentity {
        backend: executor.get("backend")?.as_str()?.to_string(),
        selector: executor
            .get("selector")
            .and_then(Value::as_str)
            .map(str::to_string),
        model: executor
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_string),
    })
}

pub fn source_worktree_path(cwd: Option<String>, workspace: Option<String>) -> Option<PathBuf> {
    cwd.or_else(|| {
        workspace.and_then(|workspace| {
            let path = PathBuf::from(&workspace);
            path.exists().then_some(workspace)
        })
    })
    .map(PathBuf::from)
}

pub fn ai_model_from_tool(ai_tool: &str) -> Option<String> {
    let start = ai_tool.find('(')?;
    let end = ai_tool[start + 1..].find(')')? + start + 1;
    let model = ai_tool[start + 1..end].trim();
    (!model.is_empty()).then(|| model.to_string())
}

pub fn promotion_source(spec: &str) -> Result<(String, Option<PathBuf>)> {
    if spec != "-" {
        let path = PathBuf::from(spec.strip_prefix('@').unwrap_or(spec));
        if path.is_file() {
            let raw = std::fs::read_to_string(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!(
                        "read agent-task promotion source {}",
                        path.display()
                    )),
                )
            })?;
            return Ok((raw, Some(path)));
        }
    }

    if let Ok((raw, path)) = agent_task_lifecycle::aggregate_source(spec) {
        return Ok((raw, Some(path)));
    }

    Ok((
        config::read_json_spec_to_string(spec)?,
        source_spec_path(spec),
    ))
}

fn promote_attempt(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    let (source, source_path) = promotion_source(run_id)?;
    promote(AgentTaskPromotionOptions {
        source,
        source_run_id: Some(run_id.to_string()),
        source_path,
        source_worktree_path: options.source_worktree_path.clone(),
        base_ref: Some(options.base.clone()),
        task_base_sha: options.task_base_sha.clone(),
        to_worktree: options.to_worktree.clone(),
        task_id: None,
        artifact_id: None,
        dry_run: false,
        gates: options.gates.clone(),
        provider_command: options.provider_command.clone(),
        provider_invocation: options.provider_invocation.clone(),
    })
}

/// Promotion is the durable boundary between a terminal provider result and
/// controller-owned gates. Reconciliation must reuse this exact report rather
/// than apply the selected artifact again.
fn promote_or_load_attempt(
    options: &AgentTaskCookServiceOptions,
    run_id: &str,
) -> Result<AgentTaskPromotionReport> {
    if let Some(promotion) = persisted_promotion_for_attempt(run_id)? {
        return Ok(promotion);
    }
    let promotion = promote_attempt(options, run_id)?;
    crate::agent_task_lifecycle::record_promotion(
        run_id,
        serde_json::to_value(&promotion).map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize cook promotion".to_string()),
            )
        })?,
    )?;
    Ok(promotion)
}

fn persisted_promotion_for_attempt(run_id: &str) -> Result<Option<AgentTaskPromotionReport>> {
    let record = agent_task_lifecycle::status(run_id)?;
    let Some(value) = record.metadata.get("latest_promotion") else {
        return Ok(None);
    };
    let promotion: AgentTaskPromotionReport =
        serde_json::from_value(value.clone()).map_err(|error| {
            Error::validation_invalid_argument(
                "latest_promotion",
                format!("persisted cook promotion is invalid: {error}"),
                Some(run_id.to_string()),
                None,
            )
        })?;
    if promotion.source.run_id.as_deref() != Some(run_id) {
        return Err(Error::validation_invalid_argument(
            "latest_promotion.source.run_id",
            "persisted cook promotion does not belong to this attempt",
            Some(run_id.to_string()),
            None,
        ));
    }
    Ok(Some(promotion))
}

fn attempt_needs_execution(run_id: &str) -> bool {
    agent_task_lifecycle::status(run_id)
        .map(|record| {
            !matches!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Succeeded
                    | agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
                    | agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
                    | agent_task_lifecycle::AgentTaskRunState::PartialFailure
                    | agent_task_lifecycle::AgentTaskRunState::Failed
                    | agent_task_lifecycle::AgentTaskRunState::Cancelled
            )
        })
        .unwrap_or(true)
}

fn finalize_cook_pr(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
) -> Result<Value> {
    finalize_cook_pr_with_backend(
        options,
        successful_run_id,
        promotion,
        &mut RealAgentTaskPrFinalizationBackend,
    )
}

fn finalize_cook_pr_with_backend<B: AgentTaskPrFinalizationBackend>(
    options: &AgentTaskCookServiceOptions,
    successful_run_id: &str,
    promotion: &AgentTaskPromotionReport,
    backend: &mut B,
) -> Result<Value> {
    if promotion.status != AgentTaskPromotionStatus::Applied {
        return Err(Error::validation_invalid_argument(
            "promotion",
            "agent-task cook finalization requires an applied promotion with green gates",
            None,
            None,
        ));
    }
    let path = promotion
        .provenance
        .get("worktree_path")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "promotion.provenance.worktree_path",
                "promotion provider did not report the applied worktree path",
                None,
                None,
            )
        })?
        .to_string();
    let source_refs = options
        .source_refs
        .iter()
        .cloned()
        .chain(std::iter::once(format!(
            "homeboy://agent-task/run/{successful_run_id}"
        )))
        .collect();
    let artifact_refs = std::iter::once(promotion.patch_artifact.path.clone()).collect();
    crate::agent_task_lifecycle::record_promotion(
        successful_run_id,
        serde_json::to_value(promotion).unwrap_or(Value::Null),
    )?;
    let report = finalize_pr_with_backend(
        AgentTaskPrFinalizationOptions {
            path: path.clone(),
            run_id: successful_run_id.to_string(),
            base: options.base.clone(),
            head: options.head.clone(),
            title: options.title.clone(),
            commit_message: options.commit_message.clone(),
            gate_results: Vec::new(),
            normalized_gate_results: promotion.gate_results.clone(),
            changed_files: promotion.changed_files.clone(),
            evidence: AgentTaskPrEvidence {
                source_refs,
                artifact_refs,
                attempt_summary: format!(
                    "{} deterministic cook gate attempt(s) completed green",
                    promotion.deterministic_gates.len()
                ),
                ai_tool: options.ai_tool.clone(),
                ai_model: options.ai_model.clone(),
                source_relationship: AgentTaskPrSourceRelationship::default(),
                verification: AgentTaskPrVerification {
                    targeted_checks_run: options.gates.verify.clone(),
                    targeted_checks_unavailable: None,
                    ci_expected: vec!["Homeboy CI after push".to_string()],
                    manual_reviewer_check: None,
                },
                runtime_guardrails: AgentTaskPrRuntimeGuardrails::default(),
                lifecycle: crate::agent_task_lifecycle::status(successful_run_id)
                    .ok()
                    .map(|record| record.lifecycle),
            },
            ai_used_for: options.ai_used_for.clone(),
            review_dossier: AgentTaskReviewDossier {
                schema: "homeboy/agent-task-review-dossier/v1".to_string(),
                summary: options.title.clone(),
                what_changed: vec!["Applies the verified agent-task candidate.".to_string()],
                how_to_test: options
                    .gates
                    .verify
                    .iter()
                    .cloned()
                    .map(|command| AgentTaskReviewTestStep {
                        command,
                        expected: "passes".to_string(),
                    })
                    .collect(),
                compatibility: "No compatibility impact was recorded by the cook workflow."
                    .to_string(),
                evidence: Vec::new(),
                ai_assistance: AgentTaskReviewAiAssistance {
                    used: true,
                    tool: options.ai_tool.clone(),
                    model: options
                        .ai_model
                        .clone()
                        .unwrap_or_else(|| "not recorded".to_string()),
                    used_for: options.ai_used_for.clone(),
                },
                source_relationships: Vec::new(),
                overrides: Vec::new(),
            },
            review_profile: resolve_review_profile(&path)?,
            manual_finalization: false,
            protected_branches: options.protected_branches.clone(),
        },
        backend,
    )?;
    Ok(serde_json::to_value(report).unwrap_or(Value::Null))
}

fn cook_report(
    cook_id: String,
    status: &str,
    attempts: Vec<AgentTaskCookAttemptReport>,
    finalization: Option<Value>,
    stop_reason: Option<String>,
    exit_code: i32,
) -> AgentTaskRunResult<AgentTaskCookReport> {
    let (latest_run_id, history_run_ids) = agent_task_lifecycle::cook_index(&cook_id)
        .map(|index| {
            (
                Some(index.latest_run_id),
                index
                    .attempts
                    .into_iter()
                    .map(|attempt| attempt.run_id)
                    .collect(),
            )
        })
        .unwrap_or((None, Vec::new()));
    AgentTaskRunResult {
        value: AgentTaskCookReport {
            schema: "homeboy/agent-task-cook/v1",
            cook_id,
            latest_run_id,
            history_run_ids,
            status: status.to_string(),
            attempts,
            finalization,
            stop_reason,
        },
        exit_code,
    }
}

fn source_spec_path(spec: &str) -> Option<PathBuf> {
    if spec == "-" {
        return None;
    }

    Some(PathBuf::from(spec.strip_prefix('@').unwrap_or(spec)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent_task::{
        AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
    };
    use crate::agent_task_finalization::{
        AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrRef,
    };
    use crate::run_lifecycle_record::{
        ProviderRuntimeLifecycle, ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
        RunLifecycleRecord,
    };

    #[test]
    fn cook_service_retry_uses_the_same_passed_context_after_ambient_mutation() {
        let _env_lock = crate::test_support::env_lock();
        let prior = std::env::var_os(crate::observation::SOURCE_SNAPSHOT_METADATA_ENV);
        let context = crate::agent_task_scheduler::HarvestExecutionContext::default();
        let first_attempt = cook_attempt_harvest_context(&context);
        std::env::set_var(
            crate::observation::SOURCE_SNAPSHOT_METADATA_ENV,
            "ambient state must not affect a passed cook context",
        );
        let retry_attempt = cook_attempt_harvest_context(&context);
        match prior {
            Some(value) => {
                std::env::set_var(crate::observation::SOURCE_SNAPSHOT_METADATA_ENV, value)
            }
            None => std::env::remove_var(crate::observation::SOURCE_SNAPSHOT_METADATA_ENV),
        }

        assert_eq!(format!("{first_attempt:?}"), format!("{retry_attempt:?}"));
        assert_eq!(
            format!("{retry_attempt:?}"),
            "HarvestExecutionContext { source_snapshot: None, lab_offload: None }"
        );
    }

    #[derive(Debug)]
    struct AcceptedDetachedAttemptDispatcher;

    impl AgentTaskCookAttemptDispatcher for AcceptedDetachedAttemptDispatcher {
        fn durable_recipe(&self) -> Result<Value> {
            Ok(serde_json::json!({ "kind": "test-detached" }))
        }

        fn dispatch_attempt(
            &self,
            plan: AgentTaskPlan,
            run_id: &str,
            _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
        ) -> Result<()> {
            agent_task_lifecycle::submit_plan(&plan, Some(run_id))?;
            agent_task_lifecycle::record_detached_lab_run(
                agent_task_lifecycle::DetachedLabRunRecord {
                    run_id,
                    runner_id: "fixture-lab",
                    runner_job_id: "accepted-daemon-job",
                    remote_workspace: "/runner/workspace",
                    remote_command: &["homeboy".to_string(), "agent-task".to_string()],
                },
            )?;
            Ok(())
        }
    }

    #[derive(Clone)]
    struct UnusedExecutor;

    impl AgentTaskExecutorAdapter for UnusedExecutor {
        fn execute(
            &self,
            _request: crate::agent_task::AgentTaskRequest,
            _context: crate::agent_task_scheduler::AgentTaskExecutionContext,
        ) -> crate::agent_task::AgentTaskOutcome {
            panic!("accepted detached attempts must remain daemon-owned")
        }
    }

    #[test]
    fn cook_returns_after_accepted_detached_attempt_without_waiting_for_daemon_completion() {
        crate::test_support::with_isolated_home(|_| {
            let run_id = "cook-detached-attempt-1";
            let plan = AgentTaskPlan::new(
                "cook-detached",
                vec![AgentTaskRequest {
                    schema: crate::agent_task::AGENT_TASK_REQUEST_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    group_key: None,
                    parent_plan_id: None,
                    executor: AgentTaskExecutor {
                        backend: "fixture".to_string(),
                        selector: None,
                        runtime_selection: None,
                        required_capabilities: Vec::new(),
                        secret_env: Vec::new(),
                        model: None,
                        config: Value::Null,
                    },
                    instructions: "complete the task".to_string(),
                    inputs: Value::Null,
                    source_refs: Vec::new(),
                    workspace: AgentTaskWorkspace::default(),
                    component_contracts: Vec::new(),
                    policy: AgentTaskPolicy::default(),
                    limits: AgentTaskLimits::default(),
                    expected_artifacts: Vec::new(),
                    artifact_declarations: Vec::new(),
                    metadata: Value::Null,
                }],
            );
            let result = run_cook(
                AgentTaskCookServiceOptions {
                    cook_id: "cook-detached".to_string(),
                    initial_run_id: run_id.to_string(),
                    initial_plan: plan,
                    to_worktree: "fixture@detached".to_string(),
                    source_worktree_path: None,
                    provider_command: None,
                    provider_invocation: None,
                    gates: VerifyGateOptions::default(),
                    max_attempts: 1,
                    no_finalize: true,
                    base: "main".to_string(),
                    task_base_sha: None,
                    head: None,
                    title: "Detached cook".to_string(),
                    commit_message: "test".to_string(),
                    source_refs: Vec::new(),
                    protected_branches: Vec::new(),
                    ai_tool: "test".to_string(),
                    ai_model: None,
                    ai_used_for: "test".to_string(),
                    attempt_dispatcher: Some(Arc::new(AcceptedDetachedAttemptDispatcher)),
                    harvest_context: Default::default(),
                },
                UnusedExecutor,
            )
            .expect("accepted detached cook returns");

            assert_eq!(result.exit_code, 0);
            assert_eq!(result.value.status, "in_flight");
            assert_eq!(result.value.attempts.len(), 1);
            assert_eq!(result.value.attempts[0].run_id, run_id);
            let record = agent_task_lifecycle::status(run_id).expect("detached attempt record");
            assert_eq!(
                record.state,
                agent_task_lifecycle::AgentTaskRunState::Running
            );
            assert_eq!(record.runner_id(), Some("fixture-lab"));
            assert_eq!(record.runner_job_id(), Some("accepted-daemon-job"));
        });
    }

    #[derive(Default)]
    struct CaptureBackend {
        body: String,
        committed: bool,
        pushed: bool,
        created: bool,
    }

    impl AgentTaskPrFinalizationBackend for CaptureBackend {
        fn hydrate_run(&mut self, _run_id: &str) -> Result<RunLifecycleRecord> {
            Ok(RunLifecycleRecord {
                execution: RunExecutionLifecycle {
                    state: RunExecutionState::Succeeded,
                    started_at: None,
                    finished_at: Some("2026-07-14T00:00:00Z".to_string()),
                    updated_at: None,
                },
                provider_runtime: vec![ProviderRuntimeLifecycle {
                    task_id: "task".to_string(),
                    backend: "opencode".to_string(),
                    state: ProviderRuntimeState::Succeeded,
                    stream_uri: None,
                    external_runtime_ids: Vec::new(),
                    metadata: serde_json::json!({"model": "openai/gpt-5.6-terra"}),
                }],
                ..RunLifecycleRecord::default()
            })
        }
        fn hydrate_gate_proof(&mut self, run_id: &str) -> Result<AgentTaskPrDurableGateProof> {
            Ok(AgentTaskPrDurableGateProof {
                run_id: run_id.to_string(),
                promotion: promotion(run_id),
            })
        }
        fn current_branch(&mut self, _path: &str) -> Result<String> {
            Ok("fix/8058".to_string())
        }
        fn changed_files(&mut self, _path: &str) -> Result<Vec<String>> {
            Ok(vec!["src/lib.rs".to_string()])
        }
        fn commit_all(&mut self, _path: &str, _message: &str) -> Result<()> {
            self.committed = true;
            Ok(())
        }
        fn push_branch(&mut self, _path: &str, _head: &str) -> Result<()> {
            self.pushed = true;
            Ok(())
        }
        fn find_open_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
        ) -> Result<Option<AgentTaskPrRef>> {
            Ok(None)
        }
        fn create_pr(
            &mut self,
            _path: &str,
            _base: &str,
            _head: &str,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            self.created = true;
            self.body = body.to_string();
            Ok(AgentTaskPrRef {
                number: 8058,
                url: "https://github.com/Extra-Chill/homeboy/pull/8058".to_string(),
            })
        }
        fn update_pr(
            &mut self,
            _path: &str,
            _number: u64,
            _title: &str,
            body: &str,
        ) -> Result<AgentTaskPrRef> {
            self.body = body.to_string();
            unreachable!("test creates a PR")
        }
    }

    fn promotion(run_id: &str) -> AgentTaskPromotionReport {
        serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "applied",
            "source": {"kind": "aggregate", "task_id": "task", "run_id": run_id},
            "to_worktree": "homeboy@8058",
            "target": {"worktree": "homeboy@8058", "path": "/repo"},
            "patch_artifact": {"id": "patch", "kind": "patch", "path": "patch"},
            "changed_files": ["src/lib.rs"],
            "gate_results": [{"id": "gate", "name": "cargo test --locked agent_task_promotion --lib", "kind": "command", "status": "passed"}],
            "operator_notification": {"status": "completed", "message": "complete"},
            "provenance": {"worktree_path": "/repo"}
        })).unwrap()
    }

    #[test]
    fn restarted_cook_uses_only_its_exact_persisted_promotion() {
        crate::test_support::with_isolated_home(|_| {
            let plan = AgentTaskPlan::new("cook-persisted", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some("run-persisted")).unwrap();
            agent_task_lifecycle::record_promotion(
                "run-persisted",
                serde_json::to_value(promotion("run-persisted")).unwrap(),
            )
            .unwrap();

            let restored = persisted_promotion_for_attempt("run-persisted")
                .unwrap()
                .expect("durable promotion");
            assert_eq!(restored.source.run_id.as_deref(), Some("run-persisted"));
            assert_eq!(restored.patch_artifact.id, "patch");
        });
    }

    #[test]
    fn persisted_promotion_from_another_attempt_is_rejected() {
        crate::test_support::with_isolated_home(|_| {
            let plan = AgentTaskPlan::new("cook-persisted", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some("run-persisted")).unwrap();
            agent_task_lifecycle::record_promotion(
                "run-persisted",
                serde_json::to_value(promotion("different-run")).unwrap(),
            )
            .unwrap();

            let error = persisted_promotion_for_attempt("run-persisted").unwrap_err();
            assert!(error.message.contains("does not belong to this attempt"));
        });
    }

    #[test]
    fn cook_successful_concrete_attempt_publishes_reviewer_body() {
        crate::test_support::with_isolated_home(|_| {
            let run_id = "cook-8058-attempt-1";
            let plan = AgentTaskPlan::new("cook-8058", Vec::new());
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
            let options = AgentTaskCookServiceOptions {
                cook_id: "cook-8058".to_string(),
                initial_run_id: run_id.to_string(),
                initial_plan: AgentTaskPlan::new("cook-8058", Vec::new()),
                to_worktree: "homeboy@8058".to_string(),
                source_worktree_path: None,
                provider_command: None,
                provider_invocation: None,
                gates: VerifyGateOptions {
                    verify: vec!["cargo test --locked agent_task_promotion --lib".to_string()],
                    private_verify: Vec::new(),
                    private_gate_reveal: Default::default(),
                },
                max_attempts: 1,
                no_finalize: false,
                base: "main".to_string(),
                task_base_sha: None,
                head: Some("fix/8058".to_string()),
                title: "Close #8058".to_string(),
                commit_message: "test".to_string(),
                source_refs: vec!["https://github.com/Extra-Chill/homeboy/issues/8058".to_string()],
                protected_branches: vec!["main".to_string()],
                ai_tool: "OpenCode".to_string(),
                ai_model: Some("openai/gpt-5.6-terra".to_string()),
                ai_used_for: "Drafted test coverage.".to_string(),
                attempt_dispatcher: None,
                harvest_context: crate::agent_task_scheduler::HarvestExecutionContext::default(),
            };
            let mut backend = CaptureBackend::default();
            finalize_cook_pr_with_backend(&options, run_id, &promotion(run_id), &mut backend)
                .unwrap();
            for section in [
                "## Summary",
                "## What changed",
                "## How to test",
                "## Compatibility",
                "## Evidence",
                "## AI assistance",
                "openai/gpt-5.6-terra",
                "1. Run `cargo test --locked agent_task_promotion --lib`; expect passes.",
            ] {
                assert!(
                    backend.body.contains(section),
                    "missing {section}: {}",
                    backend.body
                );
            }
            for forbidden in [
                "Publication intent",
                "homeboy/agent-task",
                "Changed files",
                "Final status",
            ] {
                assert!(
                    !backend.body.contains(forbidden),
                    "unexpected {forbidden}: {}",
                    backend.body
                );
            }
            assert!(backend.committed && backend.pushed && backend.created);
        });
    }

    #[test]
    fn follow_up_baseline_is_clean_and_preserves_binary_mode_and_untracked_candidate_state() {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().expect("tempdir");
        let root = &temp.path().join("repo");
        std::fs::create_dir(root).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(root)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "base"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let target_head = git_output(root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("candidate.bin"), [0_u8, 1, 2, 255]).unwrap();
        std::fs::write(root.join("candidate.sh"), "#!/bin/sh\nexit 0\n").unwrap();
        let mut permissions = std::fs::metadata(root.join("candidate.sh"))
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(root.join("candidate.sh"), permissions).unwrap();
        assert!(Command::new("git")
            .args(["add", "--all"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let patch = Command::new("git")
            .args([
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--find-renames",
                "HEAD",
            ])
            .current_dir(root)
            .output()
            .unwrap();
        assert!(patch.status.success());
        let patch_path = temp.path().join("candidate.patch");
        std::fs::write(&patch_path, patch.stdout).unwrap();
        assert!(Command::new("git")
            .args(["reset"])
            .current_dir(root)
            .status()
            .unwrap()
            .success());
        let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1", "status":"gate_failed",
            "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
            "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":target_head},
            "patch_artifact":{"id":"candidate","kind":"patch","path":patch_path}, "changed_files":["candidate.bin", "candidate.sh"],
            "command_evidence":[], "deterministic_gates":[], "gate_results":[],
            "provenance":{"worktree_path":root}, "operator_notification":{"status":"blocked","message":"red"}
        })).unwrap();
        let baseline = materialize_follow_up_baseline(&report, "first-run").expect("baseline");
        assert!(git_output(&baseline.path, &["status", "--porcelain"])
            .unwrap()
            .is_empty());
        assert_eq!(
            std::fs::read(baseline.path.join("candidate.bin")).unwrap(),
            [0_u8, 1, 2, 255]
        );
        assert!(
            baseline
                .path
                .join("candidate.sh")
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o111
                != 0
        );
        assert!(!baseline.capability.commit().is_empty());
        assert!(!baseline.capability.tree().is_empty());
        assert_eq!(
            baseline.artifact_provenance()["source_patch_artifact_sha256"],
            sha2::Sha256::digest(std::fs::read(&patch_path).unwrap())
                .to_vec()
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        );
    }

    #[test]
    fn follow_up_baseline_refuses_when_promotion_target_head_has_advanced() {
        let temp = tempfile::tempdir().expect("tempdir");
        let root = temp.path().join("repo");
        std::fs::create_dir(&root).unwrap();
        for args in [
            vec!["init"],
            vec!["config", "user.name", "Test"],
            vec!["config", "user.email", "test@example.com"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(&root)
                .status()
                .unwrap()
                .success());
        }
        std::fs::write(root.join("base.txt"), "base\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "A"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        let head_a = git_output(&root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("advanced.txt"), "B\n").unwrap();
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        assert!(Command::new("git")
            .args(["commit", "-m", "B"])
            .current_dir(&root)
            .status()
            .unwrap()
            .success());
        let patch_path = temp.path().join("candidate.patch");
        std::fs::write(&patch_path, "").unwrap();
        let report: AgentTaskPromotionReport = serde_json::from_value(serde_json::json!({
            "schema":"homeboy/agent-task-promotion-report/v1", "status":"gate_failed",
            "source":{"kind":"aggregate","task_id":"candidate-task","run_id":"first-run"},
            "to_worktree":"fixture@target", "target":{"worktree":"fixture@target", "head":head_a},
            "patch_artifact":{"id":"candidate","kind":"patch","path":patch_path},
            "provenance":{"worktree_path":root}, "operator_notification":{"status":"blocked","message":"red"}
        }))
        .unwrap();

        let error = match materialize_follow_up_baseline(&report, "first-run") {
            Ok(_) => panic!("target advancement rejects the stale promotion baseline"),
            Err(error) => error,
        };

        assert!(
            error.message.contains("target HEAD changed"),
            "unexpected error: {}",
            error.message
        );
    }
}
