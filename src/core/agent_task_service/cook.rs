//! Agent-task cook orchestration: the deterministic provider → promote → loop
//! → finalize attempt cycle plus its report/options types and promotion-source
//! resolution. Pure move out of the former `agent_task_service.rs` god-file.

use serde_json::Value;
use std::path::PathBuf;

use crate::core::agent_task_cook_loop::{
    evaluate_cook_loop, AgentTaskCookLoopOptions, AgentTaskCookLoopReport, AgentTaskCookLoopStatus,
};
use crate::core::agent_task_finalization::{
    finalize_pr_with_backend, AgentTaskPrEvidence, AgentTaskPrFinalizationBackend,
    AgentTaskPrFinalizationOptions, AgentTaskPrRuntimeGuardrails, AgentTaskPrSourceRelationship,
    AgentTaskPrVerification, RealAgentTaskPrFinalizationBackend,
};
use crate::core::agent_task_gate::VerifyGateOptions;
use crate::core::agent_task_lifecycle;
use crate::core::agent_task_promotion::{
    promote, AgentTaskPromotionOptions, AgentTaskPromotionReport, AgentTaskPromotionStatus,
};
use crate::core::agent_task_review_dossier::{
    resolve_review_profile, AgentTaskReviewAiAssistance, AgentTaskReviewDossier,
    AgentTaskReviewTestStep,
};
use crate::core::agent_task_scheduler::{AgentTaskExecutorAdapter, AgentTaskPlan};
use crate::core::command_invocation::CommandInvocation;
use crate::core::{config, Error, Result};

use super::execution::run_loaded_plan;
use super::AgentTaskRunResult;

#[derive(Debug, Clone)]
pub struct AgentTaskCookServiceOptions {
    pub cook_id: String,
    pub initial_run_id: String,
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
    let max_attempts = options.max_attempts.max(1);
    let mut attempts = Vec::new();
    let mut run_id = options.initial_run_id.clone();
    let cook_id = options.cook_id.clone();

    for attempt in 1..=max_attempts {
        agent_task_lifecycle::record_cook_attempt(&cook_id, attempt, &run_id)?;
        agent_task_lifecycle::reconcile_deferred_candidate(&run_id)?;
        let record = agent_task_lifecycle::status(&run_id)?;
        let plan = agent_task_lifecycle::load_plan(&run_id)?;
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

        let promotion = match promote_attempt(&options, &run_id) {
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
                let Some(follow_up_request) = follow_up_request else {
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
                let follow_up_plan = AgentTaskPlan::new(
                    format!("{cook_id}-cook-attempt-{}", attempt + 1),
                    vec![follow_up_request],
                );
                run_loaded_plan(follow_up_plan, Some(&next_run_id), executor.clone())?;
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
    crate::core::agent_task_lifecycle::record_promotion(
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
                lifecycle: crate::core::agent_task_lifecycle::status(successful_run_id)
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
    use crate::core::agent_task::{
        AgentTaskArtifact, AgentTaskExecutor, AgentTaskLimits, AgentTaskOutcome,
        AgentTaskOutcomeStatus, AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace,
        AGENT_TASK_ARTIFACT_SCHEMA, AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
    };
    use crate::core::agent_task_finalization::{
        AgentTaskPrDurableGateProof, AgentTaskPrFinalizationBackend, AgentTaskPrRef,
    };
    use crate::core::agent_task_scheduler::AgentTaskExecutionContext;
    use crate::core::run_lifecycle_record::{
        ProviderRuntimeLifecycle, ProviderRuntimeState, RunExecutionLifecycle, RunExecutionState,
        RunLifecycleRecord,
    };

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

    #[derive(Clone)]
    struct RecoverableCandidateExecutor {
        patch_path: PathBuf,
    }

    impl AgentTaskExecutorAdapter for RecoverableCandidateExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::CandidateRecoverable,
                summary: Some("recoverable candidate".to_string()),
                failure_classification: None,
                artifacts: vec![AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "candidate".to_string(),
                    kind: "patch".to_string(),
                    name: Some("candidate.patch".to_string()),
                    label: None,
                    role: Some("patch".to_string()),
                    semantic_key: None,
                    path: Some(self.patch_path.display().to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: Some(
                        std::fs::metadata(&self.patch_path)
                            .expect("patch metadata")
                            .len(),
                    ),
                    sha256: None,
                    metadata: serde_json::json!({ "role": "patch" }),
                }],
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
        }
    }

    #[test]
    fn cook_promotes_partial_recoverable_candidate_through_deterministic_gate() {
        crate::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let patch_path = temp.path().join("candidate.patch");
            std::fs::write(
                &patch_path,
                "diff --git a/src/lib.rs b/src/lib.rs\n--- a/src/lib.rs\n+++ b/src/lib.rs\n@@ -1 +1 @@\n-old\n+new\n",
            )
            .expect("candidate patch");
            let target = temp.path().join("promotion-target");
            std::fs::create_dir(&target).expect("promotion target");
            crate::test_support::run_git_fixture_command(&target, &["init", "-q"]);
            crate::test_support::run_git_fixture_command(
                &target,
                &["config", "user.email", "test@example.com"],
            );
            crate::test_support::run_git_fixture_command(
                &target,
                &["config", "user.name", "Homeboy Test"],
            );
            std::fs::create_dir(target.join("src")).expect("target source directory");
            std::fs::write(target.join("src/lib.rs"), "old\n").expect("target source");
            crate::test_support::run_git_fixture_command(&target, &["add", "."]);
            crate::test_support::run_git_fixture_command(
                &target,
                &["commit", "-q", "-m", "initial"],
            );
            let run_id = "cook-recoverable-attempt-1";
            let request = AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-1".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "test".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "recover candidate".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                metadata: Value::Null,
            };
            run_loaded_plan(
                AgentTaskPlan::new("cook-recoverable", vec![request]),
                Some(run_id),
                RecoverableCandidateExecutor { patch_path },
            )
            .expect("recoverable run persisted");

            let options = AgentTaskCookServiceOptions {
                cook_id: "cook-recoverable".to_string(),
                initial_run_id: run_id.to_string(),
                to_worktree: "repo@recoverable".to_string(),
                source_worktree_path: None,
                provider_command: None,
                provider_invocation: Some(CommandInvocation {
                    argv: vec![
                        "sh".to_string(),
                        "-c".to_string(),
                        format!(
                            "cat >/dev/null; printf '%s' '{{\"workspace_path\":\"{}\"}}'",
                            target.display()
                        ),
                    ],
                    ..Default::default()
                }),
                gates: VerifyGateOptions {
                    verify: vec!["true".to_string()],
                    ..VerifyGateOptions::default()
                },
                max_attempts: 1,
                no_finalize: true,
                base: "main".to_string(),
                task_base_sha: None,
                head: None,
                title: "recoverable candidate".to_string(),
                commit_message: "test".to_string(),
                source_refs: Vec::new(),
                protected_branches: Vec::new(),
                ai_tool: "OpenCode".to_string(),
                ai_model: None,
                ai_used_for: "Test coverage".to_string(),
            };

            let result = run_cook(
                options,
                RecoverableCandidateExecutor {
                    patch_path: temp.path().join("unused.patch"),
                },
            )
            .expect("cook result");

            assert_eq!(
                result.value.status, "green_no_finalize",
                "{:#?}",
                result.value
            );
            assert_ne!(result.value.status, "provider_failure");
            assert_eq!(result.value.attempts.len(), 1);
            assert_eq!(result.value.attempts[0].run_state, "PartialRecoverable");
            assert_eq!(
                result.value.attempts[0]
                    .promotion
                    .as_ref()
                    .expect("promotion")
                    .status,
                AgentTaskPromotionStatus::Applied
            );
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
}
