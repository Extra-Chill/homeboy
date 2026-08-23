//! Agent-task command run submission, status, run-next, cancel, retry, and resume tests.

use super::support::*;

#[test]
fn validate_plan_reports_invalid_input_without_creating_a_lifecycle_record() {
    with_isolated_home(|_| {
        let (value, status) = validate_plan(ValidatePlanArgs {
            plan: "{".to_string(),
        })
        .expect("validation report");

        assert_eq!(status, 1);
        assert_eq!(value["schema"], "homeboy/agent-task-plan-validation/v1");
        assert_eq!(value["scope"], "local_controller");
        assert_eq!(value["valid"], false);
        assert_eq!(value["failures"][0]["kind"], "invalid_input");
        assert!(agent_task_lifecycle::list_records()
            .expect("records")
            .is_empty());
    });
}

#[test]
fn diagnose_projects_causal_pre_execution_provider_evidence() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-provider-pre-execution";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("submit plan");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["pre_execution_failure"] = serde_json::json!({
                "phase": "worktree_provider_lookup",
                "error_code": "validation.invalid_argument",
                "message": "worktree provider `fixture` ensure command failed with exit code 1",
                "details": {
                    "worktree_provider_operation": "ensure",
                    "worktree_provider_replay_command": "fixture-provider ensure fixture@task",
                    "command_evidence": {
                        "command": "fixture-provider ensure fixture@task",
                        "exit_code": 1,
                        "stderr": "Error: Primary checkout for \"php-transformer\" does not exist. Clone it first.\nextra context",
                    },
                },
            });
        })
        .expect("record provider failure");

        let (diagnosis, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose provider failure");

        assert_eq!(exit_code, 0);
        assert_eq!(diagnosis["root_cause"]["source"], "pre_execution_failure");
        assert_eq!(
            diagnosis["root_cause"]["details"],
            serde_json::json!({
                "operation": "ensure",
                "exit_code": 1,
                "replay_command": "fixture-provider ensure fixture@task",
                "stderr_excerpt": "Error: Primary checkout for \"php-transformer\" does not exist. Clone it first.",
            })
        );
    });
}
use clap::Parser;
use homeboy::agents::agent_task_service::{
    AgentTaskCookAttemptDispatcher, DerivedCookBaselineCapability,
};
use homeboy::core::{Error, Result};
use sha2::{Digest, Sha256};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Barrier;

use crate::cli_surface::{Cli, Commands};
use homeboy::agents::agent_tasks::batch::{persist_fanout_run_batch, FanoutRunBatchChild};

use super::super::AgentTaskCommand;

#[test]
fn bounded_full_status_refs_hydrate_through_the_agent_task_resolver() {
    with_isolated_home(|_| {
        let run_id = "bounded-status-ref";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("submitted");
        let (bounded, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: true,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("bounded status");
        let evidence = AgentTaskEvidenceRef {
            kind: "status".to_string(),
            uri: bounded["details"]["status"]["ref"]
                .as_str()
                .expect("status ref")
                .to_string(),
            label: None,
        };
        let hydrated = homeboy::agents::agent_task_service::hydrate_evidence_ref(
            run_id, &evidence, None, None, None,
        );

        assert_eq!(hydrated.status, "ok");
        assert_eq!(hydrated.content["run_id"], run_id);
    });
}

#[test]
fn status_scope_keeps_the_historical_finalized_candidate_for_a_cancelled_retry() {
    with_isolated_home(|_| {
        let cook_id = "status-scope-cook";
        let source_run_id = "status-scope-attempt-1";
        let retry_run_id = "status-scope-attempt-2";
        let plan = test_plan();
        let task_id = plan.tasks[0].task_id.clone();
        let artifact_dir = tempfile::tempdir().expect("artifact directory");
        let patch = "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n";
        let patch_path = artifact_dir.path().join("candidate.patch");
        std::fs::write(&patch_path, patch).expect("candidate patch");

        agent_task_lifecycle::submit_plan(&plan, Some(source_run_id)).expect("source attempt");
        agent_task_lifecycle::submit_plan(&plan, Some(retry_run_id)).expect("retry attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, source_run_id)
            .expect("source Cook identity");
        agent_task_lifecycle::record_cook_attempt(cook_id, 2, retry_run_id)
            .expect("retry Cook identity");
        let artifact = AgentTaskArtifact {
            id: "historical-patch".to_string(),
            kind: "patch".to_string(),
            path: Some(patch_path.display().to_string()),
            size_bytes: Some(patch.len() as u64),
            sha256: Some(format!("{:x}", Sha256::digest(patch.as_bytes()))),
            metadata: json!({
                "task_id": task_id,
                "run_id": source_run_id,
                "producer_attempt": 1,
                "base_ref": "main",
                "provider_backend": "fixture",
                "repository_identity": "fixture",
                "workspace_identity": "fixture",
            }),
            ..Default::default()
        };
        let aggregate = AgentTaskAggregate {
            schema: "homeboy/agent-task-aggregate/v1".to_string(),
            plan_id: plan.plan_id.clone(),
            status: homeboy::agents::agent_tasks::scheduler::AgentTaskAggregateStatus::Succeeded,
            totals: Default::default(),
            outcomes: vec![AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: task_id.clone(),
                status: AgentTaskOutcomeStatus::Succeeded,
                summary: Some("historical patch".to_string()),
                failure_classification: None,
                artifacts: vec![artifact.clone()],
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: Vec::new(),
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }],
            events: Vec::new(),
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };
        agent_task_lifecycle::record_run_aggregate(source_run_id, &plan, &aggregate)
            .expect("source aggregate");
        let mut hash = Sha256::new();
        hash.update(source_run_id.as_bytes());
        hash.update([0]);
        hash.update(task_id.as_bytes());
        hash.update([0]);
        hash.update(b"historical-patch");
        let artifact_id = format!("agent-task-{:x}", hash.finalize());
        homeboy::core::observation::ObservationStore::open_initialized()
            .expect("observation store")
            .record_verified_artifact_with_id(
                source_run_id,
                "patch",
                &patch_path,
                &artifact_id,
                Some(patch.len() as i64),
                Some(&format!("{:x}", Sha256::digest(patch.as_bytes()))),
                json!({"agent_task":{"task_id":task_id,"logical_artifact_id":"historical-patch"}}),
            )
            .expect("controller artifact");
        homeboy::agents::agent_tasks::lifecycle::reconcile_terminal_artifact_projection(
            source_run_id,
        )
        .expect("controller projection");
        agent_task_lifecycle::record_cook_finalization(
            source_run_id,
            json!({"status":"review_ready","pr_url":"https://example.test/pull/12971"}),
        )
        .expect("historical finalization");
        agent_task_lifecycle::rewrite_record_for_test(retry_run_id, |record| {
            record.state = AgentTaskRunState::Cancelled;
        })
        .expect("cancel retry");

        let status_args = |full: bool, bridge: bool| StatusArgs {
            run_id: retry_run_id.to_string(),
            // `--bridge` and `--exact` conflict in Clap. A positional attempt
            // still resolves its Cook identity and candidate selection.
            exact: !bridge,
            bridge,
            since_cursor: None,
            full,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        };
        for value in [
            status(status_args(false, false)).expect("compact status").0,
            status(status_args(true, false)).expect("full status").0,
            status(status_args(false, true)).expect("bridge status").0,
        ] {
            assert_eq!(
                value["status_scope"]["queried_attempt"]["state"],
                "cancelled"
            );
            assert_eq!(
                value["status_scope"]["cook"]["selection"]["status"],
                "selected"
            );
            assert_eq!(
                value["status_scope"]["cook"]["selection"]["run_id"],
                source_run_id
            );
            assert_eq!(
                value["status_scope"]["cook"]["selection"]["candidate"]["state"],
                "finalized"
            );
            assert_eq!(
                value["status_scope"]["cook"]["finalization"]["pr_url"],
                "https://example.test/pull/12971"
            );
        }
    });
}

#[test]
fn status_scope_reports_a_bounded_cook_selection_as_unavailable() {
    with_isolated_home(|_| {
        let cook_id = "status-scope-degraded-cook";
        let plan = test_plan();
        let run_ids = (1..=65)
            .map(|attempt| format!("status-scope-degraded-{attempt}"))
            .collect::<Vec<_>>();
        for (index, run_id) in run_ids.iter().enumerate() {
            agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("Cook attempt");
            agent_task_lifecycle::record_cook_attempt(cook_id, (index + 1) as u32, run_id)
                .expect("Cook index attempt");
        }
        let retry_run_id = run_ids.last().expect("latest attempt");
        agent_task_lifecycle::rewrite_record_for_test(retry_run_id, |record| {
            record.state = AgentTaskRunState::Cancelled;
        })
        .expect("cancel retry");

        for value in [
            status(StatusArgs {
                run_id: retry_run_id.clone(),
                exact: true,
                bridge: false,
                since_cursor: None,
                full: false,
                bounded: false,
                no_runner_probe: false,
                strict_subject_exit: false,
                watch: false,
                interval: "5s".to_string(),
                timeout: "30m".to_string(),
            })
            .expect("compact status")
            .0,
            status(StatusArgs {
                run_id: retry_run_id.clone(),
                exact: true,
                bridge: false,
                since_cursor: None,
                full: true,
                bounded: false,
                no_runner_probe: false,
                strict_subject_exit: false,
                watch: false,
                interval: "5s".to_string(),
                timeout: "30m".to_string(),
            })
            .expect("full status")
            .0,
            status(StatusArgs {
                run_id: retry_run_id.clone(),
                // Bridge resolution starts from the supplied attempt ID; Clap
                // rejects pairing `--bridge` with `--exact`.
                exact: false,
                bridge: true,
                since_cursor: None,
                full: false,
                bounded: false,
                no_runner_probe: false,
                strict_subject_exit: false,
                watch: false,
                interval: "5s".to_string(),
                timeout: "30m".to_string(),
            })
            .expect("bridge status")
            .0,
        ] {
            assert_eq!(
                value["status_scope"]["queried_attempt"]["state"],
                "cancelled"
            );
            assert_eq!(
                value["status_scope"]["cook"]["selection"]["status"],
                "unavailable"
            );
            assert_eq!(
                value["status_scope"]["cook"]["selection"]["diagnostics"][0]["code"],
                "selection_incomplete"
            );
        }
    });
}

#[test]
fn status_omits_scope_for_an_ordinary_non_cook_attempt() {
    with_isolated_home(|_| {
        let run_id = "ordinary-status-attempt";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("submitted");

        let (value, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status");

        assert!(value.get("status_scope").is_none());
    });
}

#[test]
fn full_status_bounds_unrelated_high_cardinality_cleanup_inventory() {
    with_isolated_home(|_| {
        let run_id = "status-scoped-cleanup";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("submitted");
        let worktrees = (0..10_000)
            .map(|index| format!("/workspace/unrelated-{index}"))
            .collect::<Vec<_>>();
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["automatic_artifact_retention"] = json!({
                "status": "completed",
                "worktree_count": worktrees.len(),
                "worktrees": worktrees,
            });
        })
        .expect("persist unrelated cleanup inventory");

        let (value, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("full status");

        assert_eq!(value["schema"], "homeboy/agent-task-status-full/v2");
        assert_eq!(value["evidence_graph"].as_array().map(Vec::len), Some(8));
        assert_eq!(
            value["evidence_graph"][0]["ref"],
            format!("homeboy://agent-task/run/{run_id}/aggregate")
        );
        assert!(value["evidence_graph"]
            .as_array()
            .expect("stable graph")
            .iter()
            .all(|entry| entry["export_command"].as_str().is_some()));
        assert!(!value.to_string().contains("/workspace/unrelated-9999"));
        assert!(value.to_string().len() < 16 * 1024);

        let persisted = agent_task_lifecycle::status(run_id).expect("persisted record");
        assert_eq!(
            persisted.metadata["automatic_artifact_retention"]["worktrees"]
                .as_array()
                .map(Vec::len),
            Some(10_000)
        );
    });
}

#[derive(Debug)]
struct RecoverableRunnerDispatcher {
    unavailable: AtomicBool,
}

#[derive(Debug, Default)]
struct CountingCookDispatcher {
    prepared: AtomicUsize,
    dispatched: AtomicUsize,
}

static RETRY_RUN_DISPATCHES: AtomicUsize = AtomicUsize::new(0);

fn filesystem_snapshot(root: &Path) -> Vec<(String, Vec<u8>)> {
    fn collect(root: &Path, path: &Path, entries: &mut Vec<(String, Vec<u8>)>) {
        for entry in std::fs::read_dir(path).expect("read snapshot directory") {
            let entry = entry.expect("snapshot entry");
            let path = entry.path();
            if path.is_dir() {
                collect(root, &path, entries);
            } else {
                entries.push((
                    path.strip_prefix(root)
                        .expect("snapshot path under root")
                        .display()
                        .to_string(),
                    std::fs::read(path).expect("snapshot file bytes"),
                ));
            }
        }
    }

    let mut entries = Vec::new();
    if root.exists() {
        collect(root, root, &mut entries);
    }
    entries.sort();
    entries
}

#[derive(Debug)]
struct RetryRunDispatcher;

impl AgentTaskCookAttemptDispatcher for RetryRunDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(json!({ "kind": "retry-run-dispatcher" }))
    }

    fn prepare_for_cook(&self) -> Result<()> {
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        RETRY_RUN_DISPATCHES.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

impl AgentTaskCookAttemptDispatcher for CountingCookDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(json!({ "kind": "counting-cook-dispatcher" }))
    }

    fn prepare_for_cook(&self) -> Result<()> {
        self.prepared.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        _run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        self.dispatched.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}

#[derive(Clone, Default)]
struct CountingCookExecutor {
    executions: Arc<AtomicUsize>,
}

impl AgentTaskExecutorAdapter for CountingCookExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        self.executions.fetch_add(1, Ordering::SeqCst);
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("unexpected execution".to_string()),
            failure_classification: None,
            artifacts: Vec::new(),
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

fn cook_args_from_cli(args: Vec<String>) -> AgentTaskCookArgs {
    let cli = Cli::parse_from(args);
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("cook command");
    };
    *cook
}

#[test]
fn cook_cli_accepts_candidate_completion_and_defaults_to_wait_all() {
    use homeboy::agents::agent_task_scheduler::AgentTaskCandidateCompletionPolicy;

    let default = cook_args_from_cli(vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "candidate fixture".to_string(),
        "--to-worktree".to_string(),
        "fixture@candidate".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--no-finalize".to_string(),
    ]);
    assert_eq!(
        default.candidate_completion,
        AgentTaskCandidateCompletionPolicy::WaitAll
    );

    let first_green = cook_args_from_cli(vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "candidate fixture".to_string(),
        "--to-worktree".to_string(),
        "fixture@candidate".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--candidate-completion".to_string(),
        "first-green".to_string(),
        "--no-finalize".to_string(),
    ]);
    assert_eq!(
        first_green.candidate_completion,
        AgentTaskCandidateCompletionPolicy::FirstGreen
    );
}

#[test]
fn invalid_cook_sources_stop_before_worktree_provider_runner_executor_or_budget() {
    with_temp_home(|| {
        let destination = tempfile::tempdir()
            .expect("destination parent")
            .path()
            .join("missing");
        let cases = [
            (
                vec!["--goal", "Frame the work", "--task", "Do the work"],
                "--goal and --task conflict",
            ),
            (
                vec!["--task", "First task", "--task", "Second task"],
                "repeated --task",
            ),
            (vec!["--tasks", r#"["Wave task"]"#], "--tasks JSON"),
        ];

        for (index, (source, diagnostic)) in cases.into_iter().enumerate() {
            let run_id = format!("invalid-cook-source-{index}");
            let mut command = vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ];
            command.extend(source.into_iter().map(str::to_string));
            command.extend([
                "--to-worktree".to_string(),
                destination.display().to_string(),
                "--backend".to_string(),
                "unconfigured-provider".to_string(),
                "--run-id".to_string(),
                run_id.clone(),
                "--no-finalize".to_string(),
            ]);
            let executor = Arc::new(CountingCookExecutor::default());
            let dispatcher = Arc::new(CountingCookDispatcher::default());

            let error = run_cook_with_executor_and_dispatcher(
                cook_args_from_cli(command),
                executor.clone(),
                Some(dispatcher.clone()),
            )
            .expect_err(diagnostic);

            assert!(error.message.contains(diagnostic), "{error}");
            assert!(
                error.details["tried"].as_array().is_some_and(|hints| hints
                    .iter()
                    .any(|hint| hint
                        .as_str()
                        .is_some_and(|hint| hint.contains("homeboy agent-task cook")))),
                "diagnostic must include a complete replacement command: {error}"
            );
            assert!(
                !destination.exists(),
                "invalid source must not materialize a worktree"
            );
            assert_eq!(dispatcher.prepared.load(Ordering::SeqCst), 0);
            assert_eq!(dispatcher.dispatched.load(Ordering::SeqCst), 0);
            assert_eq!(executor.executions.load(Ordering::SeqCst), 0);
            assert!(
                lifecycle_status(&run_id).is_err(),
                "invalid source must not create a lifecycle record or consume budget"
            );
        }
    });
}

#[test]
fn goal_and_prompt_remain_a_valid_single_source_cook_shape() {
    let args = cook_args_from_cli(vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--goal".to_string(),
        "Frame the outcome".to_string(),
        "--prompt".to_string(),
        "Implement the outcome".to_string(),
        "--to-worktree".to_string(),
        "sample-plugin@fix-issue".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--no-finalize".to_string(),
    ]);

    validate_cook_request(&args).expect("goal plus prompt is one valid Cook source");
}

#[test]
fn cook_preflight_scans_at_file_content_not_its_absolute_source_path() {
    let file = tempfile::NamedTempFile::new().expect("prompt file");
    std::fs::write(file.path(), "Implement the outcome.\n").expect("write prompt");
    let args = cook_args_from_cli(vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        format!("@{}", file.path().display()),
        "--to-worktree".to_string(),
        "sample-plugin@fix-issue".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--no-finalize".to_string(),
    ]);

    validate_cook_request(&args).expect("absolute @file source is not provider evidence");

    std::fs::write(file.path(), "Read /private/evidence.json before editing.\n")
        .expect("rewrite prompt");
    let error =
        validate_cook_request(&args).expect_err("absolute path in prompt content is evidence");
    assert_eq!(error.details["field"], "prompt");
    assert!(error.message.contains("undeclared absolute evidence path"));
}

#[test]
fn cook_preflight_rejects_contradictory_retry_budget_with_corrected_command() {
    let args = cook_args_from_cli(vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "Implement the outcome".to_string(),
        "--to-worktree".to_string(),
        "sample-plugin@fix-issue".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--no-finalize".to_string(),
        "--max-attempts".to_string(),
        "2".to_string(),
        "--max-provider-executions".to_string(),
        "1".to_string(),
    ]);

    let error = validate_cook_request(&args).expect_err("unfunded retry intent fails preflight");
    assert_eq!(error.details["field"], "max-provider-executions");
    assert!(
        error
            .message
            .contains("--max-attempts 2 --max-provider-executions 2 --max-same-provider-retries 1 --max-provider-rotations 0"),
        "{error}"
    );
}

#[test]
fn cook_preflight_allows_explicit_execution_cap_to_clamp_configured_rotations() {
    with_isolated_home(|_| {
        let mut config = homeboy::core::defaults::load_config();
        config.agent_task.rotation = Some(
            serde_json::to_value(
                homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationPolicy {
                    entries: vec![
                        homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            model: Some("fallback-one".to_string()),
                            ..Default::default()
                        },
                        homeboy::agents::agent_task_scheduler::AgentTaskProviderRotationEntry {
                            model: Some("fallback-two".to_string()),
                            ..Default::default()
                        },
                    ],
                    ..Default::default()
                },
            )
            .expect("serialize configured rotation"),
        );
        homeboy::core::defaults::save_config(&config).expect("save configured rotation");
        let args = cook_args_from_cli(vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--prompt".to_string(),
            "Implement the outcome".to_string(),
            "--to-worktree".to_string(),
            "sample-plugin@fix-issue".to_string(),
            "--backend".to_string(),
            "fixture".to_string(),
            "--no-finalize".to_string(),
            "--max-attempts".to_string(),
            "1".to_string(),
            "--max-provider-executions".to_string(),
            "1".to_string(),
        ]);

        validate_cook_request(&args)
            .expect("explicit total cap truncates rotations inherited from configuration");
    });
}

impl AgentTaskCookAttemptDispatcher for RecoverableRunnerDispatcher {
    fn durable_recipe(&self) -> Result<Value> {
        Ok(json!({ "kind": "test-recoverable-runner" }))
    }

    fn prepare_for_cook(&self) -> Result<()> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(
                Error::internal_unexpected("fixture runner is unavailable").with_retryable(true)
            );
        }
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        _plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> Result<()> {
        agent_task_lifecycle::record_detached_lab_run(
            agent_task_lifecycle::DetachedLabRunRecord {
                run_id,
                runner_id: "fixture-lab",
                runner_job_id: "fixture-job",
                remote_workspace: "/runner/workspace",
                remote_command: &["homeboy".to_string(), "agent-task".to_string()],
            },
        )?;
        Ok(())
    }
}

fn recoverable_runner_cook_args(source: &std::path::Path) -> AgentTaskCookArgs {
    let args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "exercise durable runner recovery".to_string(),
        "--cwd".to_string(),
        source.display().to_string(),
        "--to-worktree".to_string(),
        source.display().to_string(),
        "--repo".to_string(),
        "fixture".to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--run-id".to_string(),
        "cook-cli-preflight-recovery".to_string(),
        "--verify".to_string(),
        "true".to_string(),
        "--no-finalize".to_string(),
    ];
    let cli = Cli::parse_from(args);
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("cook command");
    };
    *cook
}

#[test]
fn cook_runner_preflight_failure_is_visible_and_resumable_through_public_commands() {
    with_temp_home(|| {
        let root = tempfile::tempdir().expect("fixture root");
        let primary = root.path().join("primary");
        let source = root.path().join("worktree");
        std::fs::create_dir(&primary).expect("create primary checkout");
        init_runtime_component_checkout(&primary);
        let remote = Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/fixture.git",
            ])
            .current_dir(&primary)
            .status()
            .expect("configure fixture remote");
        assert!(remote.success());
        let worktree = Command::new("git")
            .args(["worktree", "add", "-b", "fixture-recovery"])
            .arg(&source)
            .current_dir(&primary)
            .status()
            .expect("create fixture worktree");
        assert!(worktree.success());
        let dispatcher = Arc::new(RecoverableRunnerDispatcher {
            unavailable: AtomicBool::new(true),
        });

        let (failed, exit_code) = run_cook_with_executor_and_dispatcher(
            recoverable_runner_cook_args(&source),
            Arc::new(CapturingExecutor::default()),
            Some(dispatcher.clone()),
        )
        .expect("runner preflight failure is durably reported");
        assert_eq!(exit_code, 1);
        assert_eq!(failed["status"], "durable_failure");
        assert_eq!(
            failed["failure_context"]["diagnostic"]["message"],
            "fixture runner is unavailable"
        );

        let (status_value, status_exit) = status(StatusArgs {
            run_id: "cook-cli-preflight-recovery".to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("cook status resolves through its public alias");
        assert_eq!(status_exit, 0);
        assert_eq!(status_value["state"], "failed");
        assert_eq!(
            status_value["metadata"]["pre_execution_failure"]["retryable"],
            true
        );
        assert_eq!(
            status_value["metadata"]["worktree_provision"]["action"], "existing",
            "status projects the persisted Cook worktree evidence"
        );

        dispatcher.unavailable.store(false, Ordering::SeqCst);
        let (resumed, exit_code) = run_cook_with_executor_and_dispatcher(
            recoverable_runner_cook_args(&source),
            Arc::new(CapturingExecutor::default()),
            Some(dispatcher.clone()),
        )
        .expect("same immutable cook resumes after runner repair");
        assert_eq!(exit_code, 0);
        assert_eq!(resumed["status"], "in_flight");
        assert_eq!(
            status(StatusArgs {
                run_id: "cook-cli-preflight-recovery".to_string(),
                exact: false,
                bridge: false,
                since_cursor: None,
                full: true,
                bounded: false,
                no_runner_probe: false,
                strict_subject_exit: false,
                watch: false,
                interval: "5s".to_string(),
                timeout: "30m".to_string(),
            })
            .expect("resumed Cook status")
            .0["metadata"]["worktree_provision"]["action"],
            "existing",
            "resuming preserves Cook worktree evidence"
        );
    });
}

#[test]
fn status_and_cook_continue_materialize_recipe_only_attempt_without_provider_work() {
    with_temp_home(|| {
        let cook_id = "cook-cli-recipe-only";
        let run_id = "cook-cli-recipe-only-attempt-1";
        let plan = AgentTaskPlan::new(
            "cook-cli-recipe-only-plan",
            vec![serde_json::from_value(json!({
                "task_id": "provider",
                "executor": { "backend": "fixture", "model": "fixture-model" },
                "instructions": "recover the durable Cook lifecycle only"
            }))
            .expect("provider task")],
        );
        let options = homeboy::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: plan,
            to_worktree: "fixture@recipe-only".to_string(),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: Default::default(),
            max_attempts: 1,
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Recipe-only Cook".to_string(),
            commit_message: "Recipe-only Cook".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "fixture".to_string(),
            ai_model: Some("fixture-model".to_string()),
            ai_used_for: "test".to_string(),
            attempt_dispatcher: None,
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist recipe without lifecycle record");
        let data_root = homeboy::core::paths::homeboy_data().expect("data root");
        let before_status = filesystem_snapshot(&data_root);

        let (status_value, status_exit) = status(StatusArgs {
            run_id: cook_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status reports recipe-only Cook without mutation");
        assert_eq!(status_exit, 0);
        assert_eq!(status_value["run_id"], run_id);
        assert_eq!(status_value["status"], "recipe_only_recovery_required");
        assert_eq!(
            status_value["guidance"]["command"],
            format!("homeboy agent-task cook-continue {run_id}")
        );
        assert!(!agent_task_lifecycle::run_record_exists_readonly(run_id)
            .expect("status did not admit"));
        assert!(!agent_task_lifecycle::cook_index_exists(cook_id).expect("status did not index"));
        assert_eq!(filesystem_snapshot(&data_root), before_status);

        let executor = Arc::new(CountingCookExecutor::default());
        let continued = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: false,
                artifact_id: None,
                full: true,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .expect("cook-continue observes repaired queued attempt");
        assert_eq!(continued.0["status"], "accepted_unscheduled");
        assert_eq!(continued.0["latest_run_id"], run_id);
        assert_eq!(executor.executions.load(Ordering::SeqCst), 0);
        let index = agent_task_lifecycle::cook_index(cook_id).expect("single repaired index entry");
        assert_eq!(index.attempts.len(), 1);
        assert_eq!(index.latest_run_id, run_id);
    });
}

#[test]
fn cook_continue_reconciles_a_delayed_runner_attempt_then_advances_its_terminal_recipe_once() {
    with_temp_home(|| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let provider = workspace.path().join("worktree-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@delayed\",\"path\":\"{}\",\"branch\":\"main\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                workspace.path().display()
            ),
        )
        .expect("write worktree provider");
        let mut permissions = std::fs::metadata(&provider)
            .expect("worktree provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions)
            .expect("make worktree provider executable");
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
                        provider.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
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
        homeboy::core::defaults::save_config(&config).expect("save worktree provider config");
        let promotion_count = workspace.path().join("promotion-count");
        let promotion_provider = workspace.path().join("promotion-provider.sh");
        let patch = workspace.path().join("delayed-provider.patch");
        std::fs::write(
            &promotion_provider,
            format!(
                "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '1\\n' >> {}\nprintf '%s\\n' '{{\"schema\":\"homeboy/command-result/v3\",\"success\":false,\"status\":\"failed\",\"error\":{{\"code\":\"validation.invalid_argument\",\"message\":\"promotion request is invalid\",\"details\":{{\"field\":\"promotion_provider.stdin\"}}}}}}'\n",
                promotion_count.display(),
            ),
        )
        .expect("write deterministic promotion provider");
        let cook_id = "cook-continue-delayed";
        let run_id = "cook-continue-delayed-attempt-1";
        let plan = AgentTaskPlan::new(
            "cook-continue-delayed-plan",
            vec![serde_json::from_value(json!({
                "task_id": "provider",
                "executor": { "backend": "fixture", "model": "fixture-model" },
                "instructions": "complete the delayed provider attempt",
                "workspace": { "root": workspace.path() }
            }))
            .expect("provider task")],
        );
        let options = homeboy::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: plan.clone(),
            to_worktree: "fixture@delayed".to_string(),
            source_worktree_path: Some(workspace.path().to_path_buf()),
            provider_command: None,
            provider_invocation: Some(homeboy::core::command_invocation::CommandInvocation {
                argv: vec!["sh".to_string(), promotion_provider.display().to_string()],
                ..Default::default()
            }),
            gates: Default::default(),
            max_attempts: 1,
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Delayed Cook continuation".to_string(),
            commit_message: "Delayed Cook continuation".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "fixture".to_string(),
            ai_model: Some("fixture-model".to_string()),
            ai_used_for: "test".to_string(),
            attempt_dispatcher: None,
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist immutable recipe");
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist provider attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id)
            .expect("bind immutable Cook attempt");
        let executor = Arc::new(CountingCookExecutor::default());
        let before = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: false,
                artifact_id: None,
                full: true,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .expect("cook-continue observes the queued attempt before provider scheduling");
        assert_eq!(before.0["status"], "accepted_unscheduled");
        assert_eq!(before.0["guidance"]["action"], "schedule_queued_run");
        assert_eq!(
            before.0["guidance"]["command"],
            format!("homeboy agent-task run {run_id}")
        );
        assert_eq!(executor.executions.load(Ordering::SeqCst), 0);

        let patch_contents = "diff --git a/delayed-provider.txt b/delayed-provider.txt\nnew file mode 100644\nindex 0000000..e69de29\n--- /dev/null\n+++ b/delayed-provider.txt\n@@ -0,0 +1 @@\n+completed after runner reconciliation\n";
        std::fs::write(&patch, patch_contents).expect("write delayed provider patch");
        let patch_sha256 = format!("{:x}", Sha256::digest(patch_contents.as_bytes()));
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &plan,
            &AgentTaskAggregate {
                schema: "homeboy/agent-task-aggregate/v1".to_string(),
                plan_id: plan.plan_id.clone(),
                status:
                    homeboy::agents::agent_tasks::scheduler::AgentTaskAggregateStatus::Succeeded,
                totals: Default::default(),
                outcomes: vec![AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "provider".to_string(),
                    status: AgentTaskOutcomeStatus::Succeeded,
                    summary: Some("delayed provider completed".to_string()),
                    failure_classification: None,
                    artifacts: vec![AgentTaskArtifact {
                        id: "delayed-provider-patch".to_string(),
                        kind: "patch".to_string(),
                        path: Some(patch.display().to_string()),
                        size_bytes: Some(patch_contents.len() as u64),
                        sha256: Some(patch_sha256),
                        ..Default::default()
                    }],
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: Value::Null,
                    workflow: None,
                    follow_up: None,
                    metadata: Value::Null,
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .expect("publish delayed provider aggregate");

        let after = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: false,
                artifact_id: None,
                full: false,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .expect("the same cook-continue advances the terminal attempt");
        assert_eq!(
            after.1, 1,
            "the rejected provider response surfaces its durable promotion failure"
        );
        assert_eq!(std::fs::read_to_string(&promotion_count).unwrap(), "1\n");
        assert_eq!(after.0["failure_context"]["phase"], "promotion");
        assert_eq!(
            after.0["failure_context"]["diagnostic"],
            json!({
                "code": "validation.invalid_argument",
                "field": "promotion_provider.response.schema",
                "message": "Invalid argument 'promotion_provider.response.schema': expected homeboy/agent-task-promotion-apply-response/v1, got homeboy/command-result/v3"
            })
        );
        assert_ne!(after.0["status"], "observation_in_progress");
        assert_eq!(executor.executions.load(Ordering::SeqCst), 0);

        let replay = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: false,
                artifact_id: None,
                full: true,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .expect("replaying cook-continue is idempotent");
        assert_eq!(replay.1, 0, "the failed continuation claim is not replayed");
        assert_eq!(replay.0["status"], "continuation_recovery_required");
        assert_eq!(
            replay.0["guidance"]["command"],
            format!("homeboy agent-task cook-continue {run_id} --rearm")
        );
        assert_eq!(
            std::fs::read_to_string(&promotion_count).unwrap(),
            "1\n",
            "a failed continuation never silently replays its promotion provider"
        );
        let rearmed = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: true,
                artifact_id: None,
                full: true,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .expect("an explicit rearm may retry the failed continuation");
        assert_eq!(rearmed.1, 1);
        assert_eq!(
            rearmed.0["failure_context"]["phase"], "promotion",
            "the later promotion failure supersedes the controller failure from before rearm"
        );
        assert_eq!(
            std::fs::read_to_string(&promotion_count).unwrap(),
            "1\n1\n",
            "explicit rearm retries the failed provider boundary"
        );
        assert_eq!(
            rearmed.0["failure_context"]["diagnostic"]["deepest_cause"],
            json!({
                "code": "validation.invalid_argument",
                "field": "promotion_provider.response.schema",
                "message": "Invalid argument 'promotion_provider.response.schema': expected homeboy/agent-task-promotion-apply-response/v1, got homeboy/command-result/v3"
            }),
            "full output retains the bounded terminal diagnostic after rearm"
        );
        assert_eq!(
            rearmed.0["failure_context"]["diagnostic"]["details"]["field"],
            "promotion_provider.response.schema",
            "Cook result retains structured terminal failure details"
        );
        let (diagnosis, diagnosis_exit) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: true,
        })
        .expect("diagnose preserves the terminal failure after rearm");
        assert_eq!(diagnosis_exit, 0);
        assert_eq!(
            diagnosis["root_cause"]["class"],
            "validation.invalid_argument"
        );
        assert_eq!(
            diagnosis["root_cause"]["field"],
            "promotion_provider.response.schema"
        );
        assert_eq!(
            diagnosis["root_cause"]["message"],
            "Invalid argument 'promotion_provider.response.schema': expected homeboy/agent-task-promotion-apply-response/v1, got homeboy/command-result/v3"
        );
        assert_eq!(
            diagnosis["root_cause"]["details"]["field"], "promotion_provider.response.schema",
            "diagnose --full retains the structured terminal failure details"
        );
        assert_ne!(
            diagnosis["root_cause"]["source"], "controller_failure",
            "the later promotion terminal record must outrank the stale controller diagnostic"
        );
        assert_eq!(executor.executions.load(Ordering::SeqCst), 0);
    });
}

#[test]
fn cook_continue_selects_a_recoverable_candidate_without_provider_redispatch() {
    with_temp_home(|| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let origin = tempfile::tempdir().expect("bare origin");
        Command::new("git")
            .args(["init", "--bare", "--initial-branch=main"])
            .current_dir(origin.path())
            .status()
            .expect("initialize local origin")
            .success()
            .then_some(())
            .expect("local origin initialized");
        for arguments in [
            vec!["remote", "add", "origin", origin.path().to_str().unwrap()],
            vec!["push", "-u", "origin", "main"],
        ] {
            Command::new("git")
                .args(arguments)
                .current_dir(workspace.path())
                .status()
                .expect("configure local origin")
                .success()
                .then_some(())
                .expect("local origin configured");
        }
        let provider = workspace.path().join("worktree-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nprintf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@recoverable\",\"path\":\"{}\",\"branch\":\"main\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'\n",
                workspace.path().display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&provider).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&provider, permissions).unwrap();
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
                        provider.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
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
        homeboy::core::defaults::save_config(&config).unwrap();
        let selected = workspace.path().join("selected.patch");
        let alternate = workspace.path().join("alternate.patch");
        let selected_patch = "diff --git a/selected.txt b/selected.txt\nnew file mode 100644\nindex 0000000..e69de29\n--- /dev/null\n+++ b/selected.txt\n@@ -0,0 +1 @@\n+selected\n";
        std::fs::write(&selected, selected_patch).unwrap();
        std::fs::write(
            &alternate,
            selected_patch
                .replace("selected.txt", "alternate.txt")
                .replace("+selected", "+alternate"),
        )
        .unwrap();
        let promotion_provider = workspace.path().join("promotion-provider.sh");
        let provider_patch = workspace.path().join("provider.patch");
        std::fs::write(
            &promotion_provider,
            format!(
                r#"#!/bin/sh
set -eu
python3 -c 'import json,sys; open(sys.argv[1], "w").write(json.load(sys.stdin)["patch"])' '{}'
git -C '{}' apply '{}'
printf '%s\n' '{{"schema":"homeboy/agent-task-promotion-apply-response/v1","workspace_path":"{}"}}'
"#,
                provider_patch.display(),
                workspace.path().display(),
                provider_patch.display(),
                workspace.path().display()
            ),
        )
        .unwrap();
        let mut permissions = std::fs::metadata(&promotion_provider)
            .unwrap()
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&promotion_provider, permissions).unwrap();
        let cook_id = "cook-recoverable-selection";
        let run_id = "cook-recoverable-selection-attempt-1";
        let plan = AgentTaskPlan::new("cook-recoverable-selection-plan", vec![serde_json::from_value(json!({"task_id":"provider","executor":{"backend":"fixture","model":"fixture-model"},"instructions":"recover candidate","workspace":{"root":workspace.path()}})).unwrap()]);
        let options = homeboy::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(), initial_run_id: run_id.to_string(), initial_plan: plan.clone(), to_worktree: "fixture@recoverable".to_string(), source_worktree_path: Some(workspace.path().to_path_buf()), provider_command: None,
            provider_invocation: Some(homeboy::core::command_invocation::CommandInvocation { argv: vec!["sh".to_string(), promotion_provider.display().to_string()], ..Default::default() }), gates: Default::default(), max_attempts: 1, no_finalize: true, draft_pr: false, base: "main".to_string(), task_base_sha: None, head: None, title: "recoverable".to_string(), commit_message: "recoverable".to_string(), source_refs: Vec::new(), protected_branches: Vec::new(), ai_tool: "fixture".to_string(), ai_model: Some("fixture-model".to_string()), ai_used_for: "test".to_string(), attempt_dispatcher: None, harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process().unwrap(),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options).unwrap();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).unwrap();
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).unwrap();
        let provenance = |id: &str, path: &std::path::Path, patch: &str| AgentTaskArtifact {
            id: id.to_string(),
            kind: "patch".to_string(),
            path: Some(path.display().to_string()),
            size_bytes: Some(patch.len() as u64),
            sha256: Some(format!("{:x}", Sha256::digest(patch.as_bytes()))),
            metadata: json!({"task_id":"provider","run_id":run_id,"producer_attempt":1,"base_ref":"main","provider_backend":"fixture","repository_identity":"fixture","workspace_identity":"fixture"}),
            ..Default::default()
        };
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &AgentTaskAggregate { schema: "homeboy/agent-task-aggregate/v1".to_string(), plan_id: plan.plan_id.clone(), status: homeboy::agents::agent_tasks::scheduler::AgentTaskAggregateStatus::CandidateRecoverable, totals: Default::default(), outcomes: vec![AgentTaskOutcome { schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(), task_id: "provider".to_string(), status: AgentTaskOutcomeStatus::CandidateRecoverable, summary: None, failure_classification: None, artifacts: vec![provenance("selected", &selected, selected_patch), provenance("alternate", &alternate, &std::fs::read_to_string(&alternate).unwrap()), AgentTaskArtifact { id: "mime-shaped".to_string(), kind: "log".to_string(), mime: Some("text/x-patch".to_string()), path: Some(workspace.path().join("missing.patch").display().to_string()), size_bytes: Some(1), sha256: Some("a".repeat(64)), metadata: json!({"actionable": false}), ..Default::default() }], typed_artifacts: Vec::new(), evidence_refs: Vec::new(), diagnostics: Vec::new(), outputs: Value::Null, workflow: None, follow_up: None, metadata: Value::Null }], events: Vec::new(), artifact_lineage: Vec::new(), child_runs: Vec::new(), artifact_bindings: Vec::new(), queue: Default::default() }).unwrap();
        let store = homeboy::core::observation::ObservationStore::open_initialized().unwrap();
        let stale_id = format!("agent-task-{}", {
            use sha2::Digest;
            let mut hash = sha2::Sha256::new();
            hash.update(run_id.as_bytes());
            hash.update([0]);
            hash.update(b"provider");
            hash.update([0]);
            hash.update(b"selected");
            format!("{:x}", hash.finalize())
        });
        for artifact in store.list_artifacts(run_id).unwrap() {
            if artifact
                .metadata_json
                .pointer("/agent_task/logical_artifact_id")
                .and_then(Value::as_str)
                == Some("selected")
            {
                store.delete_artifact_record(&artifact.id).unwrap();
            }
        }
        store
            .record_verified_artifact_with_id(
                run_id,
                "patch",
                &selected,
                &stale_id,
                Some(selected_patch.len() as i64),
                Some(&format!("{:x}", Sha256::digest(selected_patch.as_bytes()))),
                json!({"agent_task":{"task_id":"provider","logical_artifact_id":"selected"}}),
            )
            .unwrap();
        homeboy::agents::agent_tasks::lifecycle::reconcile_terminal_artifact_projection(run_id)
            .unwrap();
        let projected =
            homeboy::agents::agent_tasks::lifecycle::verified_controller_artifact_projection_path(
                run_id,
                "provider",
                &provenance("selected", &selected, selected_patch),
            )
            .unwrap()
            .expect("controller projected selected artifact");
        let projected_record = store
            .list_artifacts(run_id)
            .unwrap()
            .into_iter()
            .find(|artifact| artifact.path == projected.display().to_string())
            .expect("projection record");
        assert_eq!(
            projected_record.metadata_json["agent_task"]["projection"],
            "controller_local"
        );
        std::fs::remove_file(&selected).expect("producer artifact can be cleaned up");
        assert!(projected.is_file());
        homeboy::agents::agent_tasks::lifecycle::materialize_recovered_patch_artifact(
            run_id,
            Some("provider"),
            Some("selected"),
        )
        .expect("restart rewrites the aggregate to the controller projection");
        let executor = Arc::new(CountingCookExecutor::default());
        let ambiguous = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: false,
                artifact_id: None,
                full: true,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(
            ambiguous.0["failure_context"]["legal_actions"][2]["command"],
            format!("homeboy agent-task cook-continue {run_id} --rearm --artifact-id alternate")
        );
        assert!(!ambiguous.0.to_string().contains("mime-shaped"));
        let invalid = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: true,
                artifact_id: Some("mime-shaped".to_string()),
                full: true,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(invalid.1, 1);
        let selected_result = continue_cook_with(
            CookContinueArgs {
                cook_or_attempt_id: cook_id.to_string(),
                preflight: false,
                rearm: true,
                artifact_id: Some("selected".to_string()),
                full: true,
            },
            executor.clone(),
            |_| Ok(None),
        )
        .unwrap();
        assert_eq!(
            selected_result.1, 0,
            "selected continuation must complete promotion: {}",
            selected_result.0
        );
        assert_eq!(selected_result.0["status"], "green_no_finalize");
        assert_eq!(
            std::fs::read_to_string(workspace.path().join("selected.txt")).unwrap(),
            "selected\n"
        );
        assert_eq!(executor.executions.load(Ordering::SeqCst), 0);
        assert_eq!(
            agent_task_lifecycle::status(run_id).unwrap().metadata["cook_continue_route"]
                ["artifact_id"],
            "selected"
        );
    });
}

#[test]
fn diagnose_full_preserves_durable_promotion_io_details() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-promotion-io";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id))
            .expect("persist promotion attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["cook_operation_claims"] = json!([{
                "operation_key": format!("promote:{run_id}"),
                "state": "failed",
                "result": {
                    "status": "failed",
                    "code": "internal.io_error",
                    "message": "IO error",
                    "details": {
                        "context": "hydrate Rust gate cache",
                        "path": "/tmp/promotion-verification/gate-1",
                        "source_error": "rustup proxy must live under CARGO_HOME"
                    },
                    "deepest_cause": {
                        "code": "internal.io_error",
                        "message": "IO error"
                    }
                }
            }]);
        })
        .expect("persist promotion I/O failure");

        let (diagnosis, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: true,
        })
        .expect("diagnose persists promotion I/O details");

        assert_eq!(exit_code, 0);
        assert_eq!(diagnosis["root_cause"]["class"], "internal.io_error");
        assert_eq!(
            diagnosis["root_cause"]["details"],
            json!({
                "context": "hydrate Rust gate cache",
                "path": "/tmp/promotion-verification/gate-1",
                "source_error": "rustup proxy must live under CARGO_HOME"
            })
        );
    });
}

#[test]
fn diagnose_prioritizes_the_current_gate_denial_after_a_repaired_controller_failure() {
    with_temp_home(|| {
        let cook_id = "cook-diagnose-current-gate";
        let run_id = "run-cli-diagnose-current-gate";
        let options = homeboy::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: test_plan(),
            to_worktree: "fixture@diagnose-current-gate".to_string(),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: Default::default(),
            max_attempts: 2,
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Diagnose current gate".to_string(),
            commit_message: "Diagnose current gate".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "fixture".to_string(),
            ai_model: Some("fixture-model".to_string()),
            ai_used_for: "test".to_string(),
            attempt_dispatcher: None,
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist Cook recipe");
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id))
            .expect("persist initial controller attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, run_id).expect("record Cook attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["cook_controller_failure"] = json!({
                "code": "provider.to_worktree_failed",
                "message": "the original provider could not resolve the worktree"
            });
        })
        .expect("persist original controller failure");

        // The repaired continuation applied its candidate, then a later gate
        // denial became the durable lifecycle authority.
        agent_task_lifecycle::record_promotion(
            run_id,
            json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "applied",
                "patch_artifact": { "sha256": "candidate" }
            }),
        )
        .expect("persist applied candidate");
        agent_task_lifecycle::record_promotion(
            run_id,
            json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "gate_failed",
                "patch_artifact": { "sha256": "candidate" },
                "deterministic_gates": [{
                    "name": "cargo test -p homeboy-cli",
                    "status": "failed",
                    "message": "gate proof failed"
                }]
            }),
        )
        .expect("persist gate failure");

        let (diagnosis, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: true,
        })
        .expect("diagnose current lifecycle denial");

        assert_eq!(exit_code, 0);
        assert_eq!(
            diagnosis["root_cause"]["class"],
            "agent_task.promotion_gate_failed"
        );
        assert_eq!(diagnosis["root_cause"]["message"], "gate proof failed");
        assert!(diagnosis["diagnostic_chain"]
            .as_array()
            .expect("diagnostic chain")
            .iter()
            .any(|diagnostic| diagnostic["class"] == "provider.to_worktree_failed"));
        assert_eq!(
            diagnosis["next_commands"],
            json!([
                format!("homeboy --placement local agent-task status {run_id} --full"),
                format!("homeboy --placement local agent-task review {run_id}"),
                format!("homeboy agent-task cook-continue {run_id}"),
            ])
        );
        assert!(diagnosis["_homeboy_actionable"]["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .any(
                |action| action["command"] == format!("homeboy agent-task cook-continue {run_id}")
            ));
    });
}

#[test]
fn diagnose_reads_no_change_gate_results_as_the_current_denial() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-no-change-gate";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("persist attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["cook_controller_failure"] = json!({
                "code": "provider.stale_failure",
                "message": "resolved controller failure"
            });
        })
        .expect("persist stale controller failure");
        agent_task_lifecycle::record_promotion(
            run_id,
            json!({
                "schema": "homeboy/agent-task-promotion-report/v1",
                "status": "no_changes_gate_failed",
                "gate_results": [{
                    "id": "gate-1",
                    "name": "cargo test --locked",
                    "kind": "command",
                    "status": "failed",
                    "message": "no-change gate proof failed"
                }]
            }),
        )
        .expect("persist no-change gate failure");

        let (diagnosis, _) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: true,
        })
        .expect("diagnose no-change gate failure");

        assert_eq!(
            diagnosis["root_cause"]["class"],
            "agent_task.promotion_gate_failed"
        );
        assert_eq!(
            diagnosis["root_cause"]["message"],
            "no-change gate proof failed"
        );
        assert_eq!(
            diagnosis["root_cause"]["details"]["gate_results"][0]["id"],
            "gate-1"
        );
    });
}

#[test]
fn diagnose_prioritizes_current_finalization_failure_over_controller_history() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-finalization-failure";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("persist attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.metadata["cook_controller_failure"] = json!({
                "code": "provider.to_worktree_failed",
                "message": "resolved controller failure"
            });
            record.metadata["latest_promotion"] = json!({ "status": "applied" });
            record.metadata["cook_finalization"] = json!({
                "status": "failed",
                "code": "finalization.pr_create_failed",
                "message": "pull request creation was denied"
            });
        })
        .expect("persist finalization failure");

        let (diagnosis, _) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: true,
        })
        .expect("diagnose finalization failure");

        assert_eq!(
            diagnosis["root_cause"]["class"],
            "finalization.pr_create_failed"
        );
        assert_eq!(
            diagnosis["root_cause"]["message"],
            "pull request creation was denied"
        );
        assert!(diagnosis["diagnostic_chain"]
            .as_array()
            .expect("diagnostic chain")
            .iter()
            .any(|diagnostic| diagnostic["class"] == "provider.to_worktree_failed"));
    });
}

#[test]
fn diagnose_projects_missing_runner_pid_without_an_aggregate_and_keeps_replay_readiness() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-missing-runner-pid";
        let mut plan = test_plan();
        plan.tasks[0].workspace.root = Some("/runner/workspace/homeboy".to_string());
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("persist attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.state = AgentTaskRunState::Cancelled;
            record.metadata["runner_id"] = json!("homeboy-lab");
            record.metadata["cancel_reason"] = json!("missing_runner_pid");
            record.metadata["provider_executions_consumed"] = json!(0);
            record.metadata["runner_execution_record"] = json!({
                "status": "planned",
                "runner_id": "homeboy-lab"
            });
        })
        .expect("persist missing runner PID cancellation");

        let (diagnosis, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose cancellation");

        assert_eq!(exit_code, 0);
        assert_eq!(
            diagnosis["root_cause"]["class"],
            "agent_task.runner_missing_pid"
        );
        assert_eq!(diagnosis["causal_phase"], "runner_submission");
        assert_eq!(
            diagnosis["causal_chain"][0]["failure_classification"],
            "runner_cancellation"
        );
        assert_eq!(diagnosis["runner_diagnostic_probe"]["performed"], false);
        assert_eq!(
            diagnosis["runner_diagnostic_probe"]["skipped_reason"],
            "missing_runner_job_id"
        );
        assert_eq!(
            diagnosis["durable_read"]["unavailable_sources"][0]["source"],
            "aggregate"
        );
        assert_eq!(diagnosis["next_action_basis"], "diagnosis");
        assert_eq!(diagnosis["retry_replay"]["readiness"], "ready");
    });
}

#[test]
fn diagnose_probes_a_runner_owned_terminal_record_when_the_job_is_known() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-runner-evidence";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("persist attempt");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.state = AgentTaskRunState::Cancelled;
            record.metadata["runner_id"] = json!("homeboy-lab");
            record.metadata["runner_job_id"] = json!("job-12506");
        })
        .expect("persist runner-owned cancellation");

        let (diagnosis, _) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose runner-owned terminal record");

        assert_eq!(diagnosis["runner_diagnostic_probe"]["performed"], true);
        assert!(diagnosis["runner_diagnostic_probe"]["error"]
            .as_str()
            .expect("runner probe error")
            .contains("runner subsystem is unavailable"));
    });
}

#[test]
fn controller_proxy_status_and_logs_resolve_before_runner_child_is_known() {
    with_temp_home(|| {
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        agent_task_lifecycle::record_lab_offload_planned(
            homeboy::agents::agent_tasks::lifecycle::LabOffloadProxyPlan {
                run_id: "run-cli-controller-proxy",
                runner_id: "homeboy-lab",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
                durable_plan: None,
            },
        )
        .expect("controller proxy persisted");

        let (status_value, status_exit) = status(StatusArgs {
            run_id: "run-cli-controller-proxy".to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("controller status resolves");
        let (logs_value, logs_exit) = logs(LogsArgs {
            run_id: "run-cli-controller-proxy".to_string(),
            raw: false,
        })
        .expect("controller logs resolve");

        assert_eq!(status_exit, 0);
        assert_eq!(logs_exit, 0);
        assert_eq!(status_value["state"], "queued");
        assert_eq!(
            status_value["metadata"]["runner_execution_record"]["status"],
            "planned"
        );
        assert_eq!(logs_value["run_id"], "run-cli-controller-proxy");
    });
}

#[test]
fn controller_proxy_run_uses_transport_recovery_without_provider_dispatch() {
    with_temp_home(|| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        agent_task_lifecycle::record_lab_offload_planned(
            homeboy::agents::agent_tasks::lifecycle::LabOffloadProxyPlan {
                run_id: "run-cli-transport-proxy",
                runner_id: "remote-runner-42",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
                durable_plan: None,
            },
        )
        .expect("controller proxy persisted");
        let executor = Arc::new(CapturingExecutor::default());

        let error = run_submitted_with_executor(
            "run-cli-transport-proxy".to_string(),
            None,
            executor.clone(),
        )
        .expect_err("transport proxy needs runner recovery");

        assert!(error
            .message
            .contains("provider execution was not attempted"));
        assert!(error
            .hints
            .iter()
            .any(|hint| hint.message == "Next: homeboy runner connect remote-runner-42"));
        assert!(executor
            .observed_request
            .lock()
            .expect("executor lock")
            .is_none());

        let record =
            agent_task_lifecycle::status("run-cli-transport-proxy").expect("proxy state preserved");
        assert_eq!(record.state, AgentTaskRunState::Queued);
        assert_eq!(record.metadata["retryable"], true);
        assert_eq!(record.metadata["transport_recovery"], "required");
    });
}

#[test]
fn controller_proxy_resume_uses_transport_recovery_without_provider_dispatch() {
    with_temp_home(|| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        agent_task_lifecycle::record_lab_offload_planned(
            homeboy::agents::agent_tasks::lifecycle::LabOffloadProxyPlan {
                run_id: "run-cli-resume-transport-proxy",
                runner_id: "remote-runner-42",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
                durable_plan: None,
            },
        )
        .expect("controller proxy persisted");
        let executor = Arc::new(CapturingExecutor::default());

        let error = run_resume_with_executor_and_bridge(
            "run-cli-resume-transport-proxy".to_string(),
            false,
            None,
            false,
            executor.clone(),
        )
        .expect_err("transport proxy needs runner recovery");

        assert!(error
            .message
            .contains("provider execution was not attempted"));
        assert!(executor
            .observed_request
            .lock()
            .expect("executor lock")
            .is_none());
        assert_eq!(
            agent_task_lifecycle::status("run-cli-resume-transport-proxy")
                .expect("proxy state preserved")
                .state,
            AgentTaskRunState::Queued
        );
    });
}

#[test]
fn controller_proxy_run_resumes_on_its_recorded_runner_workspace() {
    with_temp_home(|| {
        let workspace = tempfile::tempdir().expect("runner workspace");
        let continuation = workspace.path().join("runner-continuation");
        homeboy::runner::runners::create(r#"{"id":"lab-local","kind":"local"}"#, false)
            .expect("local runner created");
        let command = vec![
            "sh".to_string(),
            "-c".to_string(),
            format!(
                "pwd > {} && touch {}",
                continuation.display(),
                continuation.display()
            ),
        ];
        agent_task_lifecycle::record_lab_offload_planned(
            homeboy::agents::agent_tasks::lifecycle::LabOffloadProxyPlan {
                run_id: "run-cli-runner-resume-proxy",
                runner_id: "lab-local",
                remote_workspace: &workspace.path().display().to_string(),
                remote_command: &command,
                durable_plan: None,
            },
        )
        .expect("controller proxy persisted");

        let executor = Arc::new(CapturingExecutor::default());
        let error = run_submitted_with_executor(
            "run-cli-runner-resume-proxy".to_string(),
            None,
            executor.clone(),
        )
        .expect_err("runner continuation awaits its durable result");
        assert!(
            !continuation.exists(),
            "runner transport was not registered"
        );
        assert!(error.message.contains("owned by runner transport recovery"));
        assert!(executor
            .observed_request
            .lock()
            .expect("executor lock")
            .is_none());
    });
}

#[test]
fn run_next_leaves_transport_proxy_queued_for_runner_recovery() {
    with_temp_home(|| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        agent_task_lifecycle::record_lab_offload_planned(
            homeboy::agents::agent_tasks::lifecycle::LabOffloadProxyPlan {
                run_id: "run-cli-queued-proxy",
                runner_id: "remote-runner-42",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
                durable_plan: None,
            },
        )
        .expect("controller proxy persisted");
        let executor = Arc::new(CapturingExecutor::default());

        let (_, exit_code) = run_next_with_executor_and_fanout(executor.clone(), None)
            .expect("run-next skips proxy");

        assert_eq!(exit_code, 0);
        assert!(executor
            .observed_request
            .lock()
            .expect("executor lock")
            .is_none());
        assert_eq!(
            agent_task_lifecycle::status("run-cli-queued-proxy")
                .expect("proxy status")
                .state,
            AgentTaskRunState::Queued
        );
    });
}

#[test]
fn submit_run_status_reports_terminal_state() {
    with_temp_home(|| {
        let plan = AgentTaskPlan::new(
            "plan-cli-terminal",
            vec![AgentTaskRequest {
                schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
                task_id: "task-cli-terminal".to_string(),
                group_key: None,
                parent_plan_id: None,
                executor: AgentTaskExecutor {
                    backend: "missing-provider-test".to_string(),
                    selector: None,
                    runtime_selection: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    model: None,
                    config: Value::Null,
                },
                instructions: "exercise durable terminal status".to_string(),
                inputs: Value::Null,
                source_refs: Vec::new(),
                workspace: AgentTaskWorkspace::default(),
                component_contracts: Vec::new(),
                policy: AgentTaskPolicy::default(),
                limits: AgentTaskLimits::default(),
                expected_artifacts: Vec::new(),
                artifact_declarations: Vec::new(),
                output_declarations: Vec::new(),
                runtime_tools: Vec::new(),
                metadata: Value::Null,
            }],
        );
        let plan_file = tempfile::NamedTempFile::new().expect("plan file");
        std::fs::write(
            plan_file.path(),
            serde_json::to_string(&plan).expect("plan json"),
        )
        .expect("write plan");
        let plan_path = format!("@{}", plan_file.path().display());

        submit(SubmitArgs {
            plan: plan_path,
            run_id: Some("run-cli-terminal".to_string()),
        })
        .expect("submitted");
        let (_, run_exit_code) = run_submitted(RunArgs {
            run_id: "run-cli-terminal".to_string(),
            timeout_ms: None,
        })
        .expect("run completed");
        let (status_json, status_exit_code) = status(StatusArgs {
            run_id: "run-cli-terminal".to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status loaded");
        let (bridge_status_json, bridge_status_exit_code) = status(StatusArgs {
            run_id: "run-cli-terminal".to_string(),
            exact: false,
            bridge: true,
            since_cursor: Some(0),
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("bridge status loaded");
        assert_eq!(
            status_json["action_eligibility"]["schema"],
            "homeboy/agent-task-lifecycle-action-eligibility/v1"
        );
        let record: AgentTaskRunRecord = serde_json::from_value(status_json).expect("record");

        assert_eq!(run_exit_code, 1);
        assert_eq!(status_exit_code, 0);
        assert_eq!(bridge_status_exit_code, 0);
        assert_eq!(
            bridge_status_json["schema"],
            "homeboy/agent-task-run-status/v1"
        );
        assert!(bridge_status_json["normalized_events"].is_array());
        assert_eq!(
            bridge_status_json["action_eligibility"]["schema"],
            "homeboy/agent-task-lifecycle-action-eligibility/v1"
        );
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
        assert_eq!(record.totals.expect("totals").failed, 1);
    });
}

#[test]
fn failed_run_status_logs_and_review_include_outcome_diagnostic_summary() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnostic-summary";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(DiagnosticFailureExecutor),
        )
        .expect("run completed with failed outcome");

        let (status_value, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status loaded");
        let (logs_value, _) = logs(LogsArgs {
            run_id: run_id.to_string(),
            raw: false,
        })
        .expect("logs loaded");
        let (review_value, _) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            full: true,
            to_worktree: None,
            provider_command: None,
            provider_argv: Vec::new(),
        })
        .expect("review loaded");

        for value in [&status_value, &logs_value, &review_value] {
            assert_eq!(
                value["diagnostic_summary"]["message"],
                "Requested provider \"example-oauth\" is not registered. Registered provider plugins: []"
            );
            assert_eq!(value["diagnostic_summary"]["class"], "provider_discovery");
            assert_eq!(value["diagnostic_summary"]["task_id"], "task-a");
        }
    });
}

#[test]
fn evidence_command_hydrates_homeboy_and_file_refs_with_filters_and_redaction() {
    with_isolated_home(|home| {
        let file_path = home.path().join("executor-result.json");
        std::fs::write(
            &file_path,
            r#"{"message":"failed","api_key":"super-secret","details":"useful"}"#,
        )
        .expect("write evidence file");
        let run_id = "run-cli-evidence";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(EvidenceFixtureExecutor {
                run_id: run_id.to_string(),
                file_uri: format!("file://{}", file_path.display()),
            }),
        )
        .expect("run completed");

        let (value, exit_code) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: None,
            task: Some("task-a".to_string()),
            failure_only: true,
            full: false,
        })
        .expect("evidence loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-evidence/v1");
        assert_eq!(value["durable_read"]["phase"], "controller_local");
        assert!(value["durable_read"]["unavailable_sources"]
            .as_array()
            .expect("durable source availability")
            .is_empty());
        assert_eq!(value["count"], 4);
        let entries = value["evidence"].as_array().expect("evidence array");
        let file_entry = entries
            .iter()
            .find(|entry| entry["kind"] == "executor-result")
            .expect("file evidence");
        assert_eq!(file_entry["source"], "file");
        assert_eq!(file_entry["content"]["format"], "json");
        assert_eq!(file_entry["content"]["value"]["api_key"], "[REDACTED]");
        assert_eq!(file_entry["content"]["value"]["details"], "useful");
        let aggregate_entry = entries
            .iter()
            .find(|entry| entry["kind"] == "executor-normalized-output")
            .expect("homeboy evidence");
        assert_eq!(aggregate_entry["source"], "homeboy");
        assert_eq!(aggregate_entry["content"]["status"], "failed");
    });
}

#[test]
fn diagnose_hydrates_executor_result_evidence_root_cause() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let evidence_path = evidence_dir.path().join("executor-result.json");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "status": "provider_error",
                "diagnostics": [
                    {
                        "class": "runtime.required_typed_artifacts_missing",
                        "message": "Agent runtime did not produce required typed artifacts: concept_packet, design_packet."
                    },
                    {
                        "class": "agent_runtime.task_run_failed",
                        "message": "RecipeValidationError: configured provider runtime path does not exist"
                    }
                ],
                "command": "agent-runtime task run",
                "exit_code": 1,
                "stderr": "ability unavailable\nsecret=raw-secret"
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        run_loaded_plan(
            test_plan(),
            Some("run-cli-diagnose-evidence"),
            Arc::new(ExecutorResultEvidenceFailureExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            }),
        )
        .expect("run completed with failed outcome");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: "run-cli-diagnose-evidence".to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-diagnose/v1");
        assert_eq!(
            value["root_cause"]["class"],
            "agent_runtime.task_run_failed"
        );
        assert_eq!(
            value["root_cause"]["message"],
            "RecipeValidationError: configured provider runtime path does not exist"
        );
        assert_eq!(
            value["hydrated_evidence"][0]["summary"]["command"],
            "agent-runtime task run"
        );
        assert_eq!(value["hydrated_evidence"][0]["summary"]["exit_code"], 1);
        assert!(value["hydrated_evidence"][0]["summary"]["stderr_excerpt"]
            .as_str()
            .expect("stderr excerpt")
            .contains("[REDACTED]"));
        assert_eq!(
            value["next_commands"][0],
            "homeboy --placement local agent-task status run-cli-diagnose-evidence --full"
        );

        let (status_value, status_exit_code) = status(StatusArgs {
            run_id: "run-cli-diagnose-evidence".to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status loaded");
        assert_eq!(status_exit_code, 0);
        assert_eq!(
            status_value["diagnostic_summary"]["class"],
            "agent_runtime.task_run_failed"
        );
        assert_eq!(
            status_value["diagnostic_summary"]["message"],
            "RecipeValidationError: configured provider runtime path does not exist"
        );
    });
}

#[test]
fn diagnose_routes_timed_out_review_form_continuation_away_from_generic_retry() {
    with_temp_home(|| {
        let cook_id = "cook-diagnose-review-form";
        let source_run_id = "cook-diagnose-review-form-attempt-1";
        let run_id = "cook-diagnose-review-form-attempt-2";
        let mut plan = test_plan();
        plan.tasks[0].inputs = json!({
            "cook_loop": {
                "review_form_required": true,
                "execution_budget_authority": {
                    "kind": "fresh_cook_review",
                    "max_same_provider_retries": 1
                }
            }
        });
        let options = homeboy::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: plan.clone(),
            to_worktree: "fixture@review-form".to_string(),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: Default::default(),
            max_attempts: 1,
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Review form continuation".to_string(),
            commit_message: "Review form continuation".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "fixture".to_string(),
            ai_model: Some("fixture-model".to_string()),
            ai_used_for: "test".to_string(),
            attempt_dispatcher: None,
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist Cook recipe");
        let mut observed_plan = plan.clone();
        observed_plan.tasks[0].instructions = "runtime detail is not recipe identity".to_string();
        agent_task_lifecycle::submit_plan(&observed_plan, Some(source_run_id))
            .expect("persist source run");
        agent_task_lifecycle::submit_plan(&observed_plan, Some(run_id))
            .expect("persist review run");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, source_run_id)
            .expect("record source attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 2, run_id)
            .expect("record review attempt");

        let source_promotion = json!({
            "schema": "homeboy/agent-task-promotion-report/v1",
            "status": "applied",
            "source": { "kind": "aggregate", "task_id": "task-a", "run_id": source_run_id },
            "to_worktree": "fixture@review-form",
            "target": { "worktree": "fixture@review-form", "path": "/tmp/review-form-candidate" },
            "patch_artifact": { "id": "patch", "kind": "patch", "path": "patch" },
            "changed_files": ["src/lib.rs"],
            "deterministic_gates": [],
            "gate_results": [],
            "operator_notification": { "status": "completed", "message": "complete" },
            "provenance": { "candidate": { "sha256": "exact-candidate" } }
        });
        agent_task_lifecycle::record_promotion(source_run_id, source_promotion.clone())
            .expect("persist applied source promotion");
        let mut review_promotion = source_promotion;
        review_promotion["source"]["run_id"] = json!(run_id);
        review_promotion["provenance"]["cook_follow_up"] = json!({
            "kind": "review_form_only",
            "source_run_id": source_run_id,
        });
        agent_task_lifecycle::record_promotion(run_id, review_promotion)
            .expect("persist copied review promotion");
        agent_task_lifecycle::record_run_aggregate(
            run_id,
            &observed_plan,
            &AgentTaskAggregate {
                schema: "homeboy/agent-task-aggregate/v1".to_string(),
                plan_id: observed_plan.plan_id.clone(),
                status: homeboy::agents::agent_tasks::scheduler::AgentTaskAggregateStatus::Failed,
                totals: Default::default(),
                outcomes: vec![AgentTaskOutcome {
                    schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                    task_id: "task-a".to_string(),
                    status: AgentTaskOutcomeStatus::Timeout,
                    summary: Some("review form timed out".to_string()),
                    failure_classification: Some(AgentTaskFailureClassification::Timeout),
                    artifacts: Vec::new(),
                    typed_artifacts: Vec::new(),
                    evidence_refs: Vec::new(),
                    diagnostics: Vec::new(),
                    outputs: Value::Null,
                    workflow: None,
                    follow_up: None,
                    metadata: Value::Null,
                }],
                events: Vec::new(),
                artifact_lineage: Vec::new(),
                child_runs: Vec::new(),
                artifact_bindings: Vec::new(),
                queue: Default::default(),
            },
        )
        .expect("persist timeout aggregate");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.state = AgentTaskRunState::PartialFailure;
        })
        .expect("mark review attempt terminal");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: true,
        })
        .expect("diagnose timed-out review form");

        let continuation = format!("homeboy agent-task cook-continue {run_id}");
        assert_eq!(exit_code, 0);
        assert_eq!(value["retry_replay"]["readiness"], "unavailable");
        assert!(value["retry_replay"]["action"].is_null());
        assert!(value["next_commands"]
            .as_array()
            .expect("next commands")
            .iter()
            .any(|command| command == &json!(continuation)));
        let actions = value["_homeboy_actionable"]["next_actions"]
            .as_array()
            .expect("actionable next actions");
        assert!(actions
            .iter()
            .any(|action| action["command"] == continuation));
        assert!(value.to_string().contains("cook-continue"));
        assert!(!value.to_string().contains("agent-task retry"));
    });
}

#[test]
fn diagnose_prioritizes_structured_policy_denial_over_successful_provider_exit() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let evidence_path = evidence_dir.path().join("executor-result.json");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "failure_classification": "policy_denied",
                "diagnostics": [
                    {
                        "class": "provider.runtime_unavailable",
                        "message": "Provider runtime reported an unavailable capability."
                    },
                    {
                        "class": "agent_task.provider_malformed_json",
                        "message": "Executor wrapper reported malformed provider output."
                    },
                    {
                        "class": "provider.process_exit",
                        "message": "OpenCode CLI exited with status 0"
                    },
                    {
                        "class": "agent_tool.command_denied",
                        "message": "Tool 'grep' was denied by the external-directory permission policy.",
                        "data": {
                            "tool": "grep",
                            "permission": "external_directory_read",
                            "requested_path": "/Users/chubes/Developer/homeboy",
                            "canonical_path": "/Users/chubes/Developer/homeboy@fix-11827-diagnose-policy-root-cause"
                        }
                    },
                    {
                        "class": "agent_task.required_output_missing",
                        "message": "Required output review_form was not produced."
                    },
                    {
                        "class": "agent_task.provider_outcome_contract_violation",
                        "message": "Provider result violates the required output contract."
                    }
                ]
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        let run_id = "run-cli-diagnose-policy-denial";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ExecutorResultEvidenceFailureExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            }),
        )
        .expect("run completed with failed outcome");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["root_cause"]["class"], "agent_tool.command_denied");
        assert_eq!(value["root_cause"]["details"]["tool"], "grep");
        assert_eq!(
            value["root_cause"]["details"]["permission"],
            "external_directory_read"
        );
        assert_eq!(
            value["root_cause"]["details"]["requested_path"],
            "/Users/chubes/Developer/homeboy"
        );
        assert_eq!(
            value["root_cause"]["details"]["canonical_path"],
            "/Users/chubes/Developer/homeboy@fix-11827-diagnose-policy-root-cause"
        );
        assert!(value["diagnostic_chain"]
            .as_array()
            .expect("diagnostic chain")
            .iter()
            .any(|diagnostic| diagnostic["class"] == "agent_task.required_output_missing"));
        assert_eq!(
            value["diagnostic_chain"]
                .as_array()
                .expect("diagnostic chain")[0]["class"],
            "agent_tool.command_denied"
        );
        assert_ne!(value["root_cause"]["class"], "provider.process_exit");

        let (status_value, status_exit_code) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status loaded");

        assert_eq!(status_exit_code, 0);
        assert_eq!(
            status_value["diagnostic_summary"]["class"],
            "agent_tool.command_denied"
        );
        assert_eq!(
            status_value["diagnostic_summary"]["details"]["tool"],
            "grep"
        );
        assert_eq!(
            status_value["diagnostic_summary"]["details"]["permission"],
            "external_directory_read"
        );
        assert_eq!(
            status_value["diagnostic_summary"]["details"]["canonical_path"],
            "/Users/chubes/Developer/homeboy@fix-11827-diagnose-policy-root-cause"
        );
    });
}

#[test]
fn diagnose_prioritizes_provider_stream_cause_over_malformed_wrapper() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let evidence_path = evidence_dir.path().join("executor-result.json");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "diagnostics": [{
                    "class": "agent_task.provider_malformed_json",
                    "message": "executor wrapper returned malformed JSON",
                    "data": {
                        "stdout": serde_json::to_string(&json!({
                            "diagnostics": [{
                                "class": "provider.runtime_unavailable",
                                "message": "runtime executable is unavailable"
                            }]
                        })).expect("stream json"),
                        "stderr": "token=raw-secret"
                    }
                }, {
                    "class": "agent_task.required_typed_artifacts_missing",
                    "message": "agent task did not produce required typed artifacts: concept_packet"
                }]
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        let run_id = "run-cli-diagnose-provider-stream";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ExecutorResultEvidenceFailureExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            }),
        )
        .expect("run completed with failed outcome");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["root_cause"]["class"], "provider.runtime_unavailable");
        assert_eq!(value["root_cause"]["source"], "hydrated_process_stream");
        assert_eq!(value["root_cause"]["owner"], "provider_runtime");
        assert!(value["diagnostic_chain"]
            .as_array()
            .expect("diagnostic chain")
            .iter()
            .any(|diagnostic| diagnostic["owner"] == "executor_wrapper"));
        assert_eq!(
            value["hydrated_evidence"][0]["summary"]["diagnostics"][0]["class"],
            "agent_task.provider_malformed_json"
        );
        assert!(value["hydrated_evidence"][0]["summary"]["process_streams"]
            .as_array()
            .expect("process streams")
            .iter()
            .any(|stream| stream["excerpt"] == "token=[REDACTED]"));

        let (status_value, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status loaded");
        assert_eq!(
            status_value["diagnostic_summary"]["class"],
            "provider.runtime_unavailable"
        );
        assert_eq!(
            status_value["diagnostic_summary"]["source"],
            "hydrated_process_stream"
        );
    });
}

#[test]
fn diagnose_surfaces_a_raw_provider_cause_from_a_bounded_stream_uri() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let stream_path = evidence_dir.path().join("executor.stderr");
        std::fs::write(
            &stream_path,
            "The 'gpt-5.6-terra' model requires a newer version of Codex. token=raw-secret",
        )
        .expect("write stream");
        let evidence_path = evidence_dir.path().join("executor-result.json");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "diagnostics": [{
                    "class": "agent_task.provider_malformed_json",
                    "message": "executor wrapper returned malformed JSON",
                    "data": { "stderr_uri": format!("file://{}", stream_path.display()) }
                }]
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        let run_id = "run-cli-diagnose-provider-stream-uri";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ExecutorResultEvidenceFailureExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            }),
        )
        .expect("run completed with failed outcome");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["root_cause"]["class"], "provider.process_stream");
        assert_eq!(value["root_cause"]["owner"], "provider_runtime");
        assert!(value["root_cause"]["message"]
            .as_str()
            .expect("root cause message")
            .contains("requires a newer version of Codex"));
        assert!(!value["root_cause"]["message"]
            .as_str()
            .expect("root cause message")
            .contains("raw-secret"));
        assert_eq!(
            value["hydrated_evidence"][0]["summary"]["process_streams"][0]["source"],
            "uri"
        );
    });
}

#[test]
fn diagnose_reports_unavailable_process_streams_without_failing() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let evidence_path = evidence_dir.path().join("executor-result.json");
        let missing_stream = evidence_dir.path().join("missing.stderr");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "diagnostics": [{
                    "class": "agent_task.provider_malformed_json",
                    "message": "executor wrapper returned malformed JSON",
                    "data": {
                        "stdout_path": missing_stream,
                        "stderr_path": evidence_dir.path()
                    }
                }]
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        let run_id = "run-cli-diagnose-missing-stream";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ExecutorResultEvidenceFailureExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            }),
        )
        .expect("run completed with failed outcome");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["root_cause"]["class"],
            "agent_task.provider_malformed_json"
        );
        assert_eq!(
            value["hydrated_evidence"][0]["summary"]["process_streams"][0]["status"],
            "unavailable"
        );
        assert_eq!(
            value["hydrated_evidence"][0]["summary"]["process_streams"][1]["status"],
            "unavailable"
        );
    });
}

#[test]
fn diagnose_derives_next_actions_from_the_failure_classification() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-actionable-timeout";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ClassifiedFailureExecutor {
                classification: AgentTaskFailureClassification::Timeout,
            }),
        )
        .expect("run completed with a classified failure");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        // Pre-existing fields are unchanged: the actionable envelope is additive.
        assert_eq!(value["schema"], "homeboy/agent-task-diagnose/v1");
        assert_eq!(value["run_id"], run_id);
        assert_eq!(
            value["causal_chain"][0]["failure_classification"],
            "timeout"
        );
        assert_eq!(
            value["next_commands"],
            json!([
                format!("homeboy --placement local agent-task status {run_id} --full"),
                format!("homeboy --placement local agent-task artifacts {run_id}"),
                format!("homeboy --placement local agent-task review {run_id}"),
            ])
        );

        assert_eq!(value["next_action_basis"], "diagnosis");
        let actionable = &value["_homeboy_actionable"];
        assert_eq!(actionable["run"]["id"], run_id);
        assert_eq!(actionable["run"]["kind"], "agent_task");
        assert_eq!(actionable["run"]["location"], "local");
        assert_eq!(
            actionable["run"]["status_command"],
            format!("homeboy --placement local agent-task status {run_id} --full")
        );
        assert_eq!(actionable["refs"]["agent_tasks"][0]["id"], run_id);
        assert!(actionable["evidence"]
            .as_array()
            .expect("evidence refs")
            .iter()
            .any(|evidence| {
                evidence["uri"] == "target/agent-task-review/transcript.log"
                    && evidence["id"] == "task-a:transcript"
            }));

        let commands: Vec<&str> = actionable["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .map(|action| action["command"].as_str().expect("command"))
            .collect();
        assert_eq!(
            commands,
            vec![
                format!("homeboy --placement local agent-task evidence {run_id} --task task-a --failure-only"),
                format!("homeboy --placement local agent-task review {run_id}"),
            ]
        );
    });
}

#[test]
fn diagnose_falls_back_to_the_generic_set_for_an_unclassified_failure() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnose-actionable-unknown";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ClassifiedFailureExecutor {
                classification: AgentTaskFailureClassification::Unknown,
            }),
        )
        .expect("run completed with an unclassified failure");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["next_action_basis"], "generic_fallback");
        let commands: Vec<&str> = value["_homeboy_actionable"]["next_actions"]
            .as_array()
            .expect("next actions")
            .iter()
            .map(|action| action["command"].as_str().expect("command"))
            .collect();
        assert_eq!(
            commands,
            vec![
                format!("homeboy --placement local agent-task status {run_id} --full"),
                format!("homeboy --placement local agent-task artifacts {run_id}"),
                format!("homeboy --placement local agent-task review {run_id}"),
            ]
        );
    });
}

#[test]
fn diagnose_next_actions_name_the_artifacts_that_were_not_produced() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let evidence_path = evidence_dir.path().join("executor-result.json");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({ "status": "provider_error" })).expect("evidence json"),
        )
        .expect("write evidence");

        let run_id = "run-cli-diagnose-actionable-missing-artifacts";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ExecutorResultEvidenceFailureExecutor {
                evidence_uri: format!("file://{}", evidence_path.display()),
            }),
        )
        .expect("run completed with missing declared artifacts");

        let (value, exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["missing_artifacts"][0]["missing"],
            json!(["concept_packet", "design_packet"])
        );
        assert_eq!(value["next_action_basis"], "diagnosis");

        let actions = value["_homeboy_actionable"]["next_actions"]
            .as_array()
            .expect("next actions")
            .clone();
        assert!(actions.iter().any(|action| {
            action["command"]
                == json!(format!(
                    "homeboy agent-task replay-provider-boundary {run_id} --task task-a"
                ))
                && action["label"]
                    .as_str()
                    .expect("label")
                    .contains("concept_packet, design_packet")
        }));
        assert!(actions.iter().any(|action| {
            action["command"]
                == json!(format!(
                    "homeboy --placement local agent-task artifacts {run_id} --full"
                ))
                && action["kind"] == "artifacts"
        }));
    });
}

#[test]
fn replay_provider_boundary_projects_latest_executor_input() {
    with_temp_home(|| {
        let evidence_dir = tempfile::tempdir().expect("evidence dir");
        let stale_evidence_path = evidence_dir.path().join("stale-executor-input.txt");
        let evidence_path = evidence_dir.path().join("executor-input.json");
        std::fs::write(
            &stale_evidence_path,
            "permission denied before JSON capture",
        )
        .expect("write stale evidence");
        std::fs::write(
            &evidence_path,
            serde_json::to_string(&json!({
                "task_id": "task-a",
                "executor": {
                    "backend": "sample-runtime",
                    "config": {
                        "runtime_component_paths": {
                            "agent_runtime": "/runner/data-machine-patched"
                        },
                        "runtime_env": {
                            "SAMPLE_RUNTIME_DATA_MACHINE_PATH": "/runner/data-machine-patched"
                        },
                        "runtime_env_path_aliases": {
                            "agent_runtime": "WP_CODEBOX_DATA_MACHINE_PATH"
                        }
                    }
                },
                "inputs": {
                    "runtime_task": {
                        "ability": "runtime-package/run",
                        "input": {
                            "package": {
                                "source": "data-machine"
                            }
                        }
                    }
                },
                "artifact_declarations": [
                    { "name": "runtime-package", "required": true }
                ]
            }))
            .expect("evidence json"),
        )
        .expect("write evidence");

        run_loaded_plan(
            test_plan(),
            Some("run-cli-provider-boundary-replay"),
            Arc::new(ExecutorInputEvidenceExecutor {
                evidence_uris: vec![
                    format!("file://{}", stale_evidence_path.display()),
                    format!("file://{}", evidence_path.display()),
                ],
            }),
        )
        .expect("run completed");

        let (value, exit_code) = replay_provider_boundary(ReplayProviderBoundaryArgs {
            run_id: "run-cli-provider-boundary-replay".to_string(),
            task: Some("task-a".to_string()),
        })
        .expect("replay report");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["schema"],
            "homeboy/agent-task-provider-boundary-replay/v1"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["runtime_task"]["ability"],
            "runtime-package/run"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["runtime_component_paths"]["agent_runtime"],
            "/runner/data-machine-patched"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["runtime_env"]["WP_CODEBOX_DATA_MACHINE_PATH"],
            "/runner/data-machine-patched"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["package_descriptor"]["source"],
            "data-machine"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["artifact_declarations"][0]["name"],
            "runtime-package"
        );
        assert_eq!(value["typed_evidence"]["kind"], "provider-boundary-replay");
        assert_eq!(
            value["selected_evidence"]["uri"],
            format!("file://{}", evidence_path.display())
        );
    });
}

#[test]
fn replay_provider_boundary_hydrates_persisted_plan_executor_input() {
    with_temp_home(|| {
        let run_id = "run-cli-provider-boundary-plan-replay";
        let mut plan = test_plan();
        plan.tasks[0].executor.config = json!({
            "workspace": { "root": "/candidate/workspace" },
            "workspace_root": "/candidate/workspace"
        });
        run_loaded_plan(
            plan,
            Some(run_id),
            Arc::new(ExecutorInputEvidenceExecutor {
                evidence_uris: vec![format!(
                    "homeboy://agent-task/run/{run_id}/plan#task=task-a"
                )],
            }),
        )
        .expect("run completed");

        let (value, exit_code) = replay_provider_boundary(ReplayProviderBoundaryArgs {
            run_id: run_id.to_string(),
            task: Some("task-a".to_string()),
        })
        .expect("replay persisted plan input");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["selected_evidence"]["uri"],
            format!("homeboy://agent-task/run/{run_id}/plan#task=task-a")
        );
        assert_eq!(
            value["normalized_provider_boundary"]["provider_config"]["workspace"]["root"],
            "/candidate/workspace"
        );
        assert_eq!(
            value["normalized_provider_boundary"]["provider_config"]["workspace_root"],
            "/candidate/workspace"
        );
    });
}

#[test]
fn generic_contract_fixtures_surface_runtime_import_before_missing_artifact() {
    with_temp_home(|| {
        let run_id = "run-contract-import-diagnostics";
        let outcome = fixture_outcome(
            "../../../../../../tests/fixtures/agent_task_contract/nested_runtime_import_failure.json",
        );

        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(FixtureOutcomeExecutor { outcome }),
        )
        .expect("run completed with fixture outcome");

        let (diagnose_value, diagnose_exit_code) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("diagnose loaded");
        assert_eq!(diagnose_exit_code, 0);
        assert_eq!(
            diagnose_value["root_cause"]["class"],
            "runtime.import_failed"
        );
        assert_eq!(
            diagnose_value["root_cause"]["message"],
            "ImportError: cannot import runtime package module 'neutral_runtime.adapter'"
        );
        assert_eq!(
            diagnose_value["missing_artifacts"][0]["missing"],
            json!(["answer_packet"])
        );

        let (status_value, status_exit_code) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("status loaded");
        assert_eq!(status_exit_code, 0);
        assert_eq!(
            status_value["diagnostic_summary"]["class"],
            "runtime.import_failed"
        );
        assert_eq!(
            status_value["failure_reasons"][0]["message"],
            "ImportError: cannot import runtime package module 'neutral_runtime.adapter'"
        );
    });
}

#[test]
fn execution_states_distinguish_patch_noop_provider_failure_and_gate_failure() {
    let patch = execution_states(
        fixture_execution_outcome(
            AgentTaskOutcomeStatus::Succeeded,
            None,
            vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch".to_string(),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: None,
                url: None,
                mime: None,
                size_bytes: Some(1),
                sha256: None,
                metadata: Value::Null,
            }],
            Value::Null,
        ),
        "applied",
    );
    assert_eq!(patch["provider"][0]["state"], "succeeded");
    assert_eq!(patch["candidate"]["state"], "patch_available");
    assert_eq!(patch["gate"]["state"], "passed");
    assert_eq!(patch["promotion"]["state"], "applied");

    let missing = execution_states(
        fixture_execution_outcome(
            AgentTaskOutcomeStatus::Succeeded,
            None,
            vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "missing-patch".to_string(),
                kind: "patch".to_string(),
                name: None,
                label: None,
                role: None,
                semantic_key: None,
                path: None,
                url: None,
                mime: None,
                size_bytes: Some(1),
                sha256: None,
                metadata: json!({ "executor_artifact_finalized": true }),
            }],
            Value::Null,
        ),
        "not_attempted",
    );
    assert_eq!(missing["candidate"]["state"], "missing");
    assert_eq!(missing["candidate"]["tasks"][0]["reason_code"], "missing");

    let no_op_aggregate = aggregate_for_execution_outcome(fixture_execution_outcome(
        AgentTaskOutcomeStatus::NoOp,
        None,
        Vec::new(),
        json!({
            "diagnostics": [{
                "class": "provider_process",
                "message": "OpenCode CLI exited with status 0."
            }]
        }),
    ));
    assert!(super::super::status::failure_reasons_from_aggregate(&no_op_aggregate).is_empty());
    let no_op = super::super::status::execution_states_from_aggregate(
        &no_op_aggregate,
        &json!({ "metadata": { "latest_promotion": { "status": "no_changes" } } }),
    );
    assert_eq!(no_op["provider"][0]["state"], "succeeded");
    assert_eq!(no_op["candidate"]["state"], "no_changes_produced");
    assert_eq!(
        no_op["candidate"]["tasks"][0]["reason_code"],
        "no_changes_produced"
    );

    let provider_failure = aggregate_for_execution_outcome(fixture_execution_outcome(
        AgentTaskOutcomeStatus::ProviderError,
        Some(AgentTaskFailureClassification::Provider),
        Vec::new(),
        json!({
            "diagnostics": [{
                "class": "provider_process",
                "message": "OpenCode CLI exited with status 1."
            }]
        }),
    ));
    assert_eq!(
        super::super::status::failure_reasons_from_aggregate(&provider_failure)[0]["message"],
        "OpenCode CLI exited with status 1."
    );
    let gate_failure = super::super::status::execution_states_from_aggregate(
        &provider_failure,
        &json!({ "metadata": { "latest_promotion": { "status": "gate_failed" } } }),
    );
    assert_eq!(gate_failure["provider"][0]["state"], "failed");
    assert_eq!(gate_failure["gate"]["state"], "failed");
}

#[test]
fn execution_states_prefer_adopted_normalized_gate_outcome_over_stale_attempt_failure() {
    let states = super::super::status::execution_states_from_aggregate(
        &aggregate_for_execution_outcome(fixture_execution_outcome(
            AgentTaskOutcomeStatus::ProviderError,
            Some(AgentTaskFailureClassification::Provider),
            Vec::new(),
            Value::Null,
        )),
        &json!({
            "metadata": {
                "latest_promotion": {
                    "schema": "homeboy/agent-task-promotion-report/v1",
                    "status": "gate_failed",
                    "source": {"kind": "aggregate", "task_id": "task", "run_id": "run"},
                    "to_worktree": "worktree",
                    "target": {"worktree": "worktree"},
                    "patch_artifact": {"id": "patch", "kind": "patch", "path": "patch"},
                    "deterministic_gates": [{
                        "id": "gate",
                        "visibility": "visible",
                        "reveal_policy": "full_evidence",
                        "status": "accepted_inherited_failure",
                        "command": ["sh", "-lc", "cargo test"],
                        "exit_code": 1,
                        "baseline_comparison": {
                            "base_ref": "immutable-base",
                            "exit_code": 1,
                            "failure_fingerprint": "inherited",
                            "matches_candidate_failure": true
                        }
                    }],
                    "operator_notification": {"status": "completed", "message": "complete"}
                }
            }
        }),
    );
    assert_eq!(states["gate"]["state"], "accepted_inherited_failure");
    assert_eq!(states["promotion"]["state"], "gate_failed");
}

#[test]
fn execution_states_keep_promoted_candidate_after_a_failed_provider_attempt() {
    let states = super::super::status::execution_states_from_aggregate(
        &aggregate_for_execution_outcome(fixture_execution_outcome(
            AgentTaskOutcomeStatus::Failed,
            Some(AgentTaskFailureClassification::Provider),
            Vec::new(),
            Value::Null,
        )),
        &json!({
            "metadata": {
                "latest_promotion": {
                    "status": "applied",
                    "patch_artifact": { "id": "canonical-patch" }
                }
            }
        }),
    );

    assert_eq!(states["provider"][0]["state"], "failed");
    assert_eq!(states["candidate"]["state"], "promoted");
    assert_eq!(states["promotion"]["state"], "applied");
}

fn execution_states(outcome: AgentTaskOutcome, promotion_status: &str) -> Value {
    super::super::status::execution_states_from_aggregate(
        &aggregate_for_execution_outcome(outcome),
        &json!({ "metadata": { "latest_promotion": { "status": promotion_status } } }),
    )
}

fn aggregate_for_execution_outcome(outcome: AgentTaskOutcome) -> AgentTaskAggregate {
    serde_json::from_value(json!({
        "schema": "homeboy/agent-task-aggregate/v1",
        "plan_id": "plan",
        "status": "succeeded",
        "totals": { "skipped": 0 },
        "outcomes": [outcome],
    }))
    .expect("aggregate fixture")
}

fn fixture_execution_outcome(
    status: AgentTaskOutcomeStatus,
    failure_classification: Option<AgentTaskFailureClassification>,
    artifacts: Vec<AgentTaskArtifact>,
    outputs: Value,
) -> AgentTaskOutcome {
    AgentTaskOutcome {
        schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
        task_id: String::new(),
        status,
        summary: None,
        failure_classification,
        artifacts,
        typed_artifacts: Vec::new(),
        evidence_refs: Vec::new(),
        diagnostics: Vec::new(),
        outputs,
        workflow: None,
        follow_up: None,
        metadata: Value::Null,
    }
}

#[test]
fn generic_contract_fixtures_hydrate_local_file_and_path_evidence() {
    with_isolated_home(|home| {
        let structured_path = home.path().join("structured-result.json");
        let log_path = home.path().join("runtime.log");
        std::fs::write(
            &structured_path,
            serde_json::to_string(&json!({
                "status": "provider_error",
                "diagnostics": [{
                    "class": "runtime.import_failed",
                    "message": "ImportError: cannot import runtime package module 'neutral_runtime.adapter'"
                }],
                "access_token": "secret-token"
            }))
            .expect("structured evidence json"),
        )
        .expect("write structured evidence");
        std::fs::write(&log_path, "runtime import failed").expect("write log evidence");

        let raw = include_str!(
            "../../../../../../tests/fixtures/agent_task_contract/local_file_evidence_refs.json"
        )
        .replace(
            "__LOCAL_FILE_URI__",
            &format!("file://{}", structured_path.display()),
        )
        .replace("__LOCAL_PATH__", &log_path.display().to_string());
        let outcome: AgentTaskOutcome = serde_json::from_str(&raw).expect("fixture outcome");
        let run_id = "run-contract-local-evidence";

        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(FixtureOutcomeExecutor { outcome }),
        )
        .expect("run completed with fixture outcome");

        let (value, exit_code) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: None,
            task: Some("task-a".to_string()),
            failure_only: true,
            full: false,
        })
        .expect("evidence loaded");
        assert_eq!(exit_code, 0);
        let entries = value["evidence"].as_array().expect("evidence entries");
        let structured = entries
            .iter()
            .find(|entry| entry["kind"] == "executor-result")
            .expect("structured evidence");
        assert_eq!(structured["source"], "file");
        assert_eq!(
            structured["content"]["value"]["diagnostics"][0]["class"],
            "runtime.import_failed"
        );
        assert_eq!(structured["content"]["value"]["access_token"], "[REDACTED]");

        let plain = entries
            .iter()
            .find(|entry| entry["kind"] == "runtime-log")
            .expect("plain path evidence");
        assert_eq!(plain["source"], "file");
        assert_eq!(plain["content"]["text"], "runtime import failed");
    });
}

#[test]
fn generic_contract_fixtures_accept_successful_required_artifact_handoff() {
    let outcome = fixture_outcome(
        "../../../../../../tests/fixtures/agent_task_contract/successful_required_artifact_handoff.json",
    );

    assert_eq!(outcome.status, AgentTaskOutcomeStatus::Succeeded);
    assert!(outcome
        .typed_artifacts
        .iter()
        .any(|artifact| artifact.name == "answer_packet"));
    assert!(outcome
        .artifacts
        .iter()
        .any(|artifact| artifact.metadata["handoff_schema"]
            == "homeboy/agent-task-artifact-handoff/v1"));
}

fn fixture_outcome(relative_path: &str) -> AgentTaskOutcome {
    let raw = match relative_path {
        "../../../../../../tests/fixtures/agent_task_contract/successful_required_artifact_handoff.json" => include_str!("../../../../../../tests/fixtures/agent_task_contract/successful_required_artifact_handoff.json"),
        "../../../../../../tests/fixtures/agent_task_contract/nested_runtime_import_failure.json" => include_str!("../../../../../../tests/fixtures/agent_task_contract/nested_runtime_import_failure.json"),
        "../../../../../../tests/fixtures/agent_task_contract/missing_required_artifact.json" => include_str!("../../../../../../tests/fixtures/agent_task_contract/missing_required_artifact.json"),
        _ => panic!("unknown fixture {relative_path}"),
    };
    serde_json::from_str(raw).expect("fixture outcome")
}

struct FixtureOutcomeExecutor {
    outcome: AgentTaskOutcome,
}

impl AgentTaskExecutorAdapter for FixtureOutcomeExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let mut outcome = self.outcome.clone();
        outcome.task_id = request.task_id;
        outcome
    }
}

#[test]
fn evidence_command_hydrates_plain_local_path_refs_and_summarizes_unsupported_refs() {
    with_isolated_home(|home| {
        let file_path = home.path().join("plain-evidence.txt");
        std::fs::write(&file_path, "plain local evidence").expect("write evidence file");
        let run_id = "run-cli-evidence-local-path";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(EvidencePathFixtureExecutor {
                local_path: file_path.display().to_string(),
                unsupported_uri: "provider-result://opaque/ref".to_string(),
            }),
        )
        .expect("run completed");

        let (value, exit_code) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: None,
            task: Some("task-a".to_string()),
            failure_only: true,
            full: false,
        })
        .expect("evidence loaded");

        assert_eq!(exit_code, 0);
        assert!(value["count"].as_u64().expect("evidence count") >= 2);
        let entries = value["evidence"].as_array().expect("evidence array");
        let path_entry = entries
            .iter()
            .find(|entry| entry["uri"] == file_path.display().to_string())
            .expect("local path evidence");
        assert_eq!(path_entry["source"], "file");
        assert_eq!(path_entry["content"]["text"], "plain local evidence");

        let unsupported = entries
            .iter()
            .find(|entry| entry["source"] == "unsupported")
            .expect("unsupported evidence");
        assert_eq!(unsupported["status"], "ok");
        assert_eq!(
            unsupported["content"]["unsupported_ref"],
            "provider-result://opaque/ref"
        );
        assert!(unsupported["content"]["next_action"].is_string());
    });
}

struct EvidencePathFixtureExecutor {
    local_path: String,
    unsupported_uri: String,
}

impl AgentTaskExecutorAdapter for EvidencePathFixtureExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some("failed with path evidence".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Provider),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![
                AgentTaskEvidenceRef {
                    kind: "executor-result".to_string(),
                    uri: self.local_path.clone(),
                    label: Some("Plain path".to_string()),
                },
                AgentTaskEvidenceRef {
                    kind: "executor-result".to_string(),
                    uri: self.unsupported_uri.clone(),
                    label: Some("Unsupported ref".to_string()),
                },
            ],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

#[test]
fn evidence_command_truncates_large_file_evidence() {
    with_isolated_home(|home| {
        let file_path = home.path().join("large.log");
        std::fs::write(&file_path, "x".repeat(20 * 1024)).expect("write evidence file");
        let run_id = "run-cli-evidence-truncated";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(EvidenceFixtureExecutor {
                run_id: run_id.to_string(),
                file_uri: format!("file://{}", file_path.display()),
            }),
        )
        .expect("run completed");

        let (value, _) = evidence(EvidenceArgs {
            run_id: run_id.to_string(),
            kind: Some("executor-result".to_string()),
            task: Some("task-a".to_string()),
            failure_only: false,
            full: false,
        })
        .expect("evidence loaded");

        assert_eq!(value["count"], 1);
        assert_eq!(value["evidence"][0]["truncated"], true);
        assert_eq!(value["evidence"][0]["bytes_read"], 16 * 1024);
        assert_eq!(value["evidence"][0]["omitted_bytes"], 4 * 1024);
    });
}

#[test]
fn terminal_provider_failure_with_large_promotion_evidence_keeps_full_readers_lossless() {
    with_isolated_home(|_| {
        let run_id = "run-cli-promotion-heavy-reader";
        run_loaded_plan(
            test_plan(),
            Some(run_id),
            Arc::new(ClassifiedFailureExecutor {
                classification: AgentTaskFailureClassification::PolicyDenied,
            }),
        )
        .expect("terminal provider failure");
        let patch = "x".repeat(30 * 1024);
        for attempt in 1..=3 {
            agent_task_lifecycle::record_promotion(
                run_id,
                json!({
                    "attempt": attempt,
                    "provenance": { "gate_feedback_baseline": { "current_diff": patch } },
                }),
            )
            .expect("persist multi-attempt promotion fixture");
        }

        let started = std::time::Instant::now();
        let (status_value, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            bridge: false,
            since_cursor: None,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("read status without a runner");
        let (diagnose_value, _) = diagnose(DiagnoseArgs {
            run_id: run_id.to_string(),
            full: true,
        })
        .expect("diagnose without a runner");
        let (logs_value, _) = logs(LogsArgs {
            run_id: run_id.to_string(),
            raw: false,
        })
        .expect("logs without a runner");

        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        assert!(status_value.to_string().contains(&patch));
        assert_eq!(diagnose_value["schema"], "homeboy/agent-task-diagnose/v1");
        assert_eq!(
            diagnose_value["root_cause"]["class"],
            "fixture.classified_failure"
        );
        assert_eq!(logs_value["schema"], "homeboy/agent-task-run-log/v2");
        assert!(!logs_value.to_string().contains(&patch));
    });
}

struct EvidenceFixtureExecutor {
    run_id: String,
    file_uri: String,
}

impl AgentTaskExecutorAdapter for EvidenceFixtureExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id.clone(),
            status: AgentTaskOutcomeStatus::Failed,
            summary: Some("failed with evidence".to_string()),
            failure_classification: Some(AgentTaskFailureClassification::Provider),
            artifacts: Vec::new(),
            typed_artifacts: Vec::new(),
            evidence_refs: vec![
                AgentTaskEvidenceRef {
                    kind: "executor-result".to_string(),
                    uri: self.file_uri.clone(),
                    label: Some("Executor result".to_string()),
                },
                AgentTaskEvidenceRef {
                    kind: "executor-normalized-output".to_string(),
                    uri: format!(
                        "homeboy://agent-task/run/{}/aggregate#outcome={}",
                        self.run_id, request.task_id
                    ),
                    label: Some("Normalized output".to_string()),
                },
            ],
            diagnostics: Vec::new(),
            outputs: json!({ "api_key": "super-secret", "result": "failed" }),
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

#[test]
fn run_plan_record_run_id_persists_running_status_before_executor_runs() {
    with_temp_home(|| {
        let run_id = "run-plan-durable";
        let observed_status = Arc::new(Mutex::new(None));
        let executor = Arc::new(InspectingExecutor {
            run_id: run_id.to_string(),
            observed_status: Arc::clone(&observed_status),
        });

        let (value, exit_code) =
            run_loaded_plan(test_plan(), Some(run_id), executor).expect("run-plan completed");

        let observed = observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("executor observed durable status");
        assert_eq!(exit_code, 0);
        assert_eq!(value["view"], "summary");
        assert!(value.get("tasks").is_some());
        assert!(value.get("outcomes").is_none());
        assert_eq!(observed.state, AgentTaskRunState::Running);
        assert_eq!(observed.tasks[0].state, AgentTaskState::Running);
        assert_eq!(observed.metadata["runner_pid"], std::process::id());
        assert!(observed.aggregate_path.is_none());

        let completed = lifecycle_status(run_id).expect("completed status loaded");
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
        assert!(completed.aggregate_path.is_some());
    });
}

#[test]
fn run_plan_lab_context_returns_lossless_terminal_aggregate() {
    with_temp_home(|| {
        let env = homeboy::core::lab_contract::LAB_EXECUTION_RUNNER_ID_ENV;
        let previous = std::env::var_os(env);
        std::env::set_var(env, "homeboy-lab");

        let result = run_loaded_plan(
            test_plan(),
            Some("run-plan-lab-terminal"),
            Arc::new(CapturingExecutor::default()),
        );

        match previous {
            Some(value) => std::env::set_var(env, value),
            None => std::env::remove_var(env),
        }
        let (value, exit_code) = result.expect("Lab run-plan completed");
        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-aggregate/v1");
        assert!(value.get("view").is_none());
        assert!(value
            .get("outcomes")
            .and_then(Value::as_array)
            .is_some_and(|outcomes| !outcomes.is_empty()));
        assert!(value.get("tasks").is_none());
    });
}

#[test]
fn run_plan_fails_fast_when_required_secret_env_is_missing() {
    with_temp_home(|| {
        let missing_secret = "HOMEBOY_AGENT_TASK_MISSING_PROVIDER_SECRET_TEST";
        std::env::remove_var(missing_secret);
        let mut plan = test_plan();
        plan.tasks[0].executor.secret_env = vec![missing_secret.to_string()];
        let executor = Arc::new(CapturingExecutor::default());
        let observed_request = Arc::clone(&executor.observed_request);

        let error = run_loaded_plan(plan, Some("run-plan-missing-secret"), executor)
            .expect_err("missing secret should fail before executor dispatch");

        assert!(observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .is_none());
        assert_eq!(error.details["field"], "secret_env");
        assert!(error.to_string().contains(missing_secret));
        assert!(error.details["tried"]
            .as_array()
            .expect("remediation hints")
            .iter()
            .any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("runner-required secret env contracts"))));
        assert!(!error.to_string().contains("secret-value"));
        let failed = lifecycle_status("run-plan-missing-secret")
            .expect("pre-execution failure remains inspectable");
        assert_eq!(failed.state, AgentTaskRunState::Failed);
        assert_eq!(
            failed.metadata["pre_execution_failure"]["phase"],
            "prepare_plan_for_execution"
        );
        assert_eq!(
            failed.metadata["pre_execution_failure"]["failure_code"],
            "secret_env"
        );
        assert_eq!(
            failed.metadata["pre_execution_failure"]["provider_executions_consumed"],
            0
        );
    });
}

#[test]
fn run_next_claims_oldest_queued_run_and_leaves_later_runs_queued() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-a"))
            .expect("first submitted");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b"))
            .expect("second submitted");
        let submitted = lifecycle_status("run-next-a").expect("first submitted status");
        let source = submitted.metadata["controller_runtime"]["originating"]["executable"]
            .as_str()
            .map(std::path::PathBuf::from)
            .expect("controller fixture source");
        assert_eq!(source, controller_runtime_test_executable());
        assert_ne!(
            source,
            std::env::current_exe().expect("current test executable"),
            "dependent core must not pin the libtest harness"
        );
        homeboy::agents::agent_task_lifecycle::validate_controller_runtime("run-next-a")
            .expect("pinned controller identity validates");
        let observed_status = Arc::new(Mutex::new(None));

        let (_value, exit_code) = run_next_with_executor_and_fanout(
            Arc::new(InspectingExecutor {
                run_id: "run-next-a".to_string(),
                observed_status: Arc::clone(&observed_status),
            }),
            None,
        )
        .expect("claimed run completed");

        let observed = observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("executor observed claimed status");
        let first = lifecycle_status("run-next-a").expect("first status");
        let second = lifecycle_status("run-next-b").expect("second status");

        assert_eq!(exit_code, 0);
        assert_eq!(observed.state, AgentTaskRunState::Running);
        assert_eq!(first.state, AgentTaskRunState::Succeeded);
        assert_eq!(second.state, AgentTaskRunState::Queued);
    });
}

#[test]
fn run_next_fanout_claims_ready_children_without_inspecting_unrelated_stale_queue_records() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("stale-global-cook"))
            .expect("stale record submitted");
        let stale_plan = homeboy_core::paths::homeboy_data()
            .expect("data path")
            .join("agent-task-runs/stale-global-cook/plan.json");
        let mut stale: Value =
            serde_json::from_slice(&std::fs::read(&stale_plan).expect("stale plan")).expect("JSON");
        stale["options"]["execution_budget"]["version"] = json!(999);
        std::fs::write(
            &stale_plan,
            serde_json::to_vec(&stale).expect("encode stale plan"),
        )
        .expect("persist stale plan");

        let mut target_a = test_plan();
        target_a.metadata["batch_id"] = json!("target-fanout");
        agent_task_lifecycle::submit_plan(&target_a, Some("target-child-a"))
            .expect("first target child submitted");
        let mut target_b = test_plan();
        target_b.metadata["batch_id"] = json!("target-fanout");
        agent_task_lifecycle::submit_plan(&target_b, Some("target-child-b"))
            .expect("second target child submitted");
        persist_fanout_run_batch(
            "target-fanout",
            "target-fanout",
            &[
                FanoutRunBatchChild {
                    task_id: "a".to_string(),
                    run_id: "target-child-a".to_string(),
                },
                FanoutRunBatchChild {
                    task_id: "b".to_string(),
                    run_id: "target-child-b".to_string(),
                },
            ],
            json!({}),
        )
        .expect("fanout persisted");

        let (_value, exit_code) = run_next_with_executor_and_fanout(
            Arc::new(InspectingExecutor::noop("target-child-a")),
            Some("target-fanout".to_string()),
        )
        .expect("scoped queue claim succeeds");

        assert_eq!(exit_code, 0);
        let stale = agent_task_lifecycle::exact_record("stale-global-cook").expect("stale record");
        assert_eq!(stale.state, AgentTaskRunState::Queued);
        assert!(
            stale.metadata.get("queue_quarantine").is_none(),
            "scoped dispatch must not inspect or quarantine unrelated work"
        );
        assert_eq!(
            lifecycle_status("target-child-a")
                .expect("first target status")
                .state,
            AgentTaskRunState::Succeeded
        );
        assert_eq!(
            lifecycle_status("target-child-b")
                .expect("second target status")
                .state,
            AgentTaskRunState::Queued
        );
    });
}

#[test]
fn run_next_returns_unclaimed_when_no_queued_runs_exist() {
    with_temp_home(|| {
        let (value, exit_code) = run_next_with_executor_and_fanout(
            Arc::new(InspectingExecutor {
                run_id: "unused".to_string(),
                observed_status: Arc::new(Mutex::new(None)),
            }),
            None,
        )
        .expect("run-next checked queue");

        assert_eq!(exit_code, 0);
        assert_eq!(value["claimed"], false);
    });
}

#[test]
fn cancel_command_marks_queued_run_cancelled() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-cli-cancel")).expect("submitted");

        let (value, exit_code) = cancel(CancelArgs {
            run_id: "run-cli-cancel".to_string(),
            reason: Some("not selected".to_string()),
        })
        .expect("cancelled");
        // The reported outcome must describe the durable effect, not merely that
        // the request was accepted (#12572).
        assert_eq!(value["cancellation"]["outcome"], "cancelled");
        assert_eq!(value["cancellation"]["terminal"], true);
        assert_eq!(value["cancellation"]["run_id"], "run-cli-cancel");
        let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

        assert_eq!(exit_code, 0);
        assert_eq!(record.state, AgentTaskRunState::Cancelled);
        assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(record.metadata["cancel_reason"], json!("not selected"));
    });
}

/// A provider that reserved a terminal result keeps the run joinable, so
/// cancellation is deliberately not applied. That must be reported as the
/// deferral it is — never as a completed cancellation — and it must not spend
/// the bounded convergence wait on a record cancellation never touched (#12572).
#[test]
fn cancel_command_reports_a_deferred_cancellation_without_claiming_the_run_is_cancelled() {
    with_temp_home(|| {
        let run_id = "run-cli-cancel-deferred";
        agent_task_lifecycle::submit_plan(&test_plan(), Some(run_id)).expect("submitted");
        agent_task_lifecycle::rewrite_record_for_test(run_id, |record| {
            record.state = AgentTaskRunState::Running;
            record.metadata["provider_executions"] = json!([{ "state": "succeeded" }]);
        })
        .expect("persist a terminal provider reservation");

        let started = std::time::Instant::now();
        let (value, exit_code) = cancel(CancelArgs {
            run_id: run_id.to_string(),
            reason: None,
        })
        .expect("cancellation request accepted");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["cancellation"]["outcome"],
            "deferred_for_terminal_provider"
        );
        assert_eq!(value["cancellation"]["terminal"], false);
        assert_eq!(value["state"], "running");
        assert!(
            value["summary"]
                .as_str()
                .expect("summary")
                .contains("deliberately not applied"),
            "unexpected summary: {}",
            value["summary"]
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "a deliberate deferral must not consume the convergence wait"
        );
    });
}

#[test]
fn exact_full_status_displays_retained_safe_quarantine_diagnostic() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-cli-quarantine-diagnostic"))
            .expect("submitted");
        homeboy::agents::agent_task_lifecycle::quarantine_queued_run_exact(
            "run-cli-quarantine-diagnostic",
            "maintenance\nwindow\u{0000}",
        )
        .expect("quarantined");

        let (value, exit_code) = status(StatusArgs {
            run_id: "run-cli-quarantine-diagnostic".to_string(),
            exact: true,
            bridge: false,
            since_cursor: None,
            full: true,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("exact full status");

        assert_eq!(exit_code, 0);
        assert_eq!(
            value["metadata"]["queue_quarantine"]["category"],
            "operator_quarantine"
        );
        assert_eq!(
            value["metadata"]["queue_quarantine"]["summary"],
            "operator quarantined this queued run"
        );
        assert_eq!(
            value["metadata"]["queue_quarantine"]["operator_reason"],
            "maintenancewindow"
        );
    });
}

#[test]
fn retry_command_submits_new_queued_run() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-retry-source"))
            .expect("submitted");

        let (value, exit_code) = retry(RetryArgs {
            run_id: "run-retry-source".to_string(),
            new_run_id: Some("run-retry-cli".to_string()),
            run: false,
            force: false,
        })
        .expect("retry queued");
        let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

        assert_eq!(exit_code, 0);
        assert_eq!(record.run_id, "run-retry-cli");
        assert_eq!(record.state, AgentTaskRunState::Queued);
        assert_eq!(record.metadata["retry_of"], json!("run-retry-source"));
    });
}

#[test]
fn cook_retry_run_executes_the_replacement_through_its_cook_lifecycle() {
    with_temp_home(|| {
        RETRY_RUN_DISPATCHES.store(0, Ordering::SeqCst);
        let cook_id = "cook-retry-run";
        let source_run_id = "cook-retry-run-attempt-1";
        let plan = test_plan();
        let options = homeboy::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: source_run_id.to_string(),
            initial_plan: plan.clone(),
            to_worktree: "fixture@retry-run".to_string(),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: Default::default(),
            max_attempts: 2,
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Cook retry run".to_string(),
            commit_message: "Cook retry run".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "fixture".to_string(),
            ai_model: None,
            ai_used_for: "test".to_string(),
            attempt_dispatcher: Some(Arc::new(RetryRunDispatcher)),
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist immutable Cook recipe");
        agent_task_lifecycle::submit_plan(&plan, Some(source_run_id))
            .expect("persist source Cook attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, source_run_id)
            .expect("bind source attempt to Cook");
        agent_task_lifecycle::record_pre_execution_failure(
            source_run_id,
            &plan,
            "provider_execution",
            &Error::internal_unexpected("provider interrupted").with_retryable(true),
        )
        .expect("terminalize retryable source attempt");

        let executor = Arc::new(CountingCookExecutor::default());
        let (value, _exit_code) = retry_with(
            RetryArgs {
                run_id: source_run_id.to_string(),
                new_run_id: None,
                run: true,
                force: false,
            },
            executor.clone(),
            |_| Ok(Some(Arc::new(RetryRunDispatcher))),
        )
        .expect("retry --run resumes the Cook lifecycle");

        let replacement = agent_task_lifecycle::status(&format!("{cook_id}-attempt-2-retry"))
            .expect("read executed replacement");
        let recipe = homeboy::agents::agent_task_service::load_recipe(cook_id)
            .expect("read immutable Cook recipe");
        assert_ne!(value["status"], "accepted_unscheduled");
        assert_eq!(RETRY_RUN_DISPATCHES.load(Ordering::SeqCst), 1);
        assert_eq!(replacement.state, AgentTaskRunState::Queued);
        assert_eq!(replacement.metadata["retry_of"], source_run_id);
        assert_eq!(replacement.metadata["cook_id"], cook_id);
        assert_eq!(replacement.metadata["cook_attempt"], 2);
        assert_eq!(recipe.attempts.len(), 2);
        assert_eq!(recipe.attempts[0].run_id, source_run_id);
        assert_eq!(recipe.attempts[1].run_id, replacement.run_id);
        assert_eq!(
            agent_task_lifecycle::cook_index(cook_id)
                .expect("read Cook index")
                .latest_run_id,
            replacement.run_id
        );
    });
}

#[test]
fn competing_retry_run_consumers_dispatch_a_queued_cook_replacement_exactly_once() {
    with_temp_home(|| {
        RETRY_RUN_DISPATCHES.store(0, Ordering::SeqCst);
        let cook_id = "cook-retry-run-competing";
        let source_run_id = "cook-retry-run-competing-attempt-1";
        let plan = test_plan();
        let options = homeboy::agents::agent_task_service::AgentTaskCookServiceOptions {
            cook_id: cook_id.to_string(),
            initial_run_id: source_run_id.to_string(),
            initial_plan: plan.clone(),
            to_worktree: "fixture@retry-run-competing".to_string(),
            source_worktree_path: None,
            provider_command: None,
            provider_invocation: None,
            gates: Default::default(),
            max_attempts: 2,
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            task_base_sha: None,
            head: None,
            title: "Competing Cook retry run".to_string(),
            commit_message: "Competing Cook retry run".to_string(),
            source_refs: Vec::new(),
            protected_branches: Vec::new(),
            ai_tool: "fixture".to_string(),
            ai_model: None,
            ai_used_for: "test".to_string(),
            attempt_dispatcher: Some(Arc::new(RetryRunDispatcher)),
            harvest_context: homeboy::agents::agent_task_scheduler::HarvestExecutionContext::from_current_process()
                .expect("harvest context"),
        };
        homeboy::agents::agent_task_service::persist_initial_recipe(&options)
            .expect("persist immutable Cook recipe");
        agent_task_lifecycle::submit_plan(&plan, Some(source_run_id))
            .expect("persist source Cook attempt");
        agent_task_lifecycle::record_cook_attempt(cook_id, 1, source_run_id)
            .expect("bind source attempt to Cook");
        agent_task_lifecycle::record_pre_execution_failure(
            source_run_id,
            &plan,
            "provider_execution",
            &Error::internal_unexpected("provider interrupted").with_retryable(true),
        )
        .expect("terminalize retryable source attempt");

        let replacement =
            homeboy::agents::agent_task_service::retry(source_run_id, None, false, false)
                .expect("reserve one queued replacement");
        let barrier = Arc::new(Barrier::new(2));
        let consumers = (0..2)
            .map(|_| {
                let barrier = barrier.clone();
                let run_id = replacement.record.run_id.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    super::super::run::consume_queued_cook_retry_with(
                        CookContinueArgs {
                            cook_or_attempt_id: run_id,
                            preflight: false,
                            rearm: false,
                            artifact_id: None,
                            full: false,
                        },
                        Arc::new(CountingCookExecutor::default()),
                        |_| Ok(Some(Arc::new(RetryRunDispatcher))),
                    )
                })
            })
            .collect::<Vec<_>>();
        for consumer in consumers {
            consumer
                .join()
                .expect("retry consumer joins")
                .expect("retry consumer converges");
        }

        assert_eq!(RETRY_RUN_DISPATCHES.load(Ordering::SeqCst), 1);
        let record = agent_task_lifecycle::status(&replacement.record.run_id)
            .expect("read replacement record");
        assert_eq!(record.metadata["retry_of"], source_run_id);
        assert_eq!(record.metadata["cook_id"], cook_id);
        assert_eq!(record.metadata["cook_attempt"], 2);
        assert_eq!(
            record.metadata["cook_operation_claims"][0]["operation_key"],
            format!("retry-run:{}", replacement.record.run_id)
        );
    });
}

#[test]
fn retry_force_alias_parses_as_force() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "retry",
        "retry-source",
        "--allow-duplicate",
    ])
    .expect("retry force alias parses");
    let Commands::AgentTask(args) = cli.command else {
        panic!("agent-task retry command");
    };
    let AgentTaskCommand::Retry(args) = args.command else {
        panic!("retry command");
    };
    assert!(args.force);
}

#[test]
fn replacement_gate_proof_command_requires_typed_proof_and_operator_authorization() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "record-replacement-gate-proof",
        "cook-11290-attempt-1",
        "--promotion",
        "@replacement.json",
        "--authorize-external-proof",
        "Chris approved durable evidence",
    ])
    .expect("replacement proof command parses");
    let Commands::AgentTask(args) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::RecordReplacementGateProof(args) = args.command else {
        panic!("replacement proof command");
    };
    assert_eq!(args.run_id, "cook-11290-attempt-1");
    assert_eq!(args.promotion, "@replacement.json");
    assert_eq!(
        args.authorize_external_proof.as_deref(),
        Some("Chris approved durable evidence")
    );
}

#[test]
fn verify_replacement_command_accepts_corrected_gates_and_authorization() {
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "verify-replacement",
        "cook-12788",
        "--verify",
        "cargo test exact::replacement",
        "--authorize-external-proof",
        "Chris approved corrected gate evidence",
    ])
    .expect("replacement verification command parses");
    let Commands::AgentTask(args) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::VerifyReplacement(args) = args.command else {
        panic!("replacement verification command");
    };
    assert_eq!(args.cook_or_attempt_id, "cook-12788");
    assert_eq!(args.gates.verify, ["cargo test exact::replacement"]);
    assert_eq!(
        args.authorize_external_proof,
        "Chris approved corrected gate evidence"
    );
}

#[test]
fn verify_replacement_file_gates_are_snapshotted_before_execution() {
    let temp = tempfile::tempdir().expect("tempdir");
    let public = temp.path().join("public-gate.sh");
    let private = temp.path().join("private-gate.sh");
    std::fs::write(&public, "test -f Cargo.toml\n").expect("write public gate");
    std::fs::write(&private, "test -n \"$TOKEN\"\n").expect("write private gate");
    let cli = Cli::try_parse_from([
        "homeboy",
        "agent-task",
        "verify-replacement",
        "cook-12788",
        "--verify-file",
        public.to_str().expect("public path"),
        "--private-verify-file",
        private.to_str().expect("private path"),
        "--authorize-external-proof",
        "Chris approved corrected gate evidence",
    ])
    .expect("replacement verification command parses");
    let Commands::AgentTask(args) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::VerifyReplacement(mut args) = args.command else {
        panic!("replacement verification command");
    };
    args.gates
        .snapshot_file_inputs()
        .expect("snapshot gate files");
    std::fs::write(&public, "exit 1\n").expect("mutate public gate");
    std::fs::write(&private, "exit 1\n").expect("mutate private gate");

    assert_eq!(args.gates.verify, ["test -f Cargo.toml\n"]);
    assert_eq!(args.gates.private_verify, ["test -n \"$TOKEN\"\n"]);
    assert!(args.gates.verify_file.is_empty());
    assert!(args.gates.private_verify_file.is_empty());
    assert_eq!(args.gates.input_sources[0].source_kind, "file");
    assert_eq!(args.gates.input_sources[1].path, None);
}

#[test]
fn resume_command_executes_existing_run() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-resume-cli")).expect("submitted");
        let observed_status = Arc::new(Mutex::new(None));

        let (_value, exit_code) = run_resume_with_executor_and_bridge(
            "run-resume-cli".to_string(),
            false,
            None,
            false,
            Arc::new(InspectingExecutor {
                run_id: "run-resume-cli".to_string(),
                observed_status: Arc::clone(&observed_status),
            }),
        )
        .expect("resumed");

        let observed = observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("executor observed status");
        let completed = lifecycle_status("run-resume-cli").expect("completed status");

        assert_eq!(exit_code, 0);
        assert!(observed.metadata["resume_requested_at"].is_string());
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
    });
}

#[test]
fn bridge_resume_reprojects_historical_lab_artifacts_and_preserves_status_cursor_shape() {
    with_temp_home(|| {
        let run_id = "run-cli-bridge-reconcile";
        let task_id = "task-a";
        let artifact_id = "patch";
        let patch = "diff --git a/file b/file\n--- a/file\n+++ b/file\n@@ -1 +1 @@\n-old\n+new\n";
        let mut hasher = Sha256::new();
        hasher.update(patch.as_bytes());
        let sha256 = format!("{:x}", hasher.finalize());
        let plan = test_plan();

        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted historical plan");
        let aggregate: AgentTaskAggregate = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": plan.plan_id,
            "status": "succeeded",
            "totals": { "skipped": 0, "succeeded": 1, "failed": 0 },
            "events": [
                { "task_id": task_id, "state": "running", "attempt": 1 },
                { "task_id": task_id, "state": "succeeded", "attempt": 1 }
            ],
            "outcomes": [{
                "schema": AGENT_TASK_OUTCOME_SCHEMA,
                "task_id": task_id,
                "status": "succeeded",
                "artifacts": [{
                    "schema": AGENT_TASK_ARTIFACT_SCHEMA,
                    "id": artifact_id,
                    "kind": "patch",
                    "path": "/runner/executor-finalized/patch.diff",
                    "size_bytes": patch.len(),
                    "sha256": sha256,
                    "metadata": { "executor_artifact_finalized": true }
                }]
            }]
        }))
        .expect("historical aggregate");
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)
            .expect("persist historical aggregate");
        agent_task_lifecycle::record_runner_job_identity(
            run_id,
            "lab-historical-missing",
            "job-historical",
        )
        .expect("persist Lab identity");

        let finalized = homeboy::core::paths::artifact_root()
            .expect("artifact root")
            .join("executor-finalized")
            .join(run_id)
            .join(artifact_id);
        std::fs::create_dir_all(finalized.parent().expect("finalized parent"))
            .expect("create finalized parent");
        std::fs::write(&finalized, patch).expect("write controller-finalized bytes");

        let store = homeboy::core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        assert!(store
            .list_artifacts(run_id)
            .expect("artifacts before bridge")
            .is_empty());

        let (value, exit_code) = resume(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: true,
            since_cursor: Some(1),
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("bridge resume reconciles terminal projection");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-run-status/v1");
        assert_eq!(value["latest_event_cursor"], 2);
        assert_eq!(value["normalized_events"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            value["normalized_events"][0]["type"],
            "agent_task.state_changed"
        );
        assert_eq!(value["normalized_events"][0]["status"], "succeeded");
        let artifacts = store.list_artifacts(run_id).expect("projected artifacts");
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].artifact_type, "file");
        assert_eq!(
            homeboy::agents::agent_tasks::lifecycle::verified_controller_artifact_projection_path(
                run_id,
                task_id,
                &aggregate.outcomes[0].artifacts[0],
            )
            .expect("projection lookup"),
            Some(std::path::PathBuf::from(&artifacts[0].path))
        );

        resume(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: true,
            since_cursor: Some(1),
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("bridge reconciliation is idempotent");
        assert_eq!(
            store
                .list_artifacts(run_id)
                .expect("artifacts after retry")
                .len(),
            1
        );
    });
}

#[test]
fn non_bridge_resume_keeps_aggregate_output_shape() {
    with_temp_home(|| {
        let run_id = "run-cli-resume-ordinary";
        let plan = test_plan();
        agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submitted plan");
        let aggregate: AgentTaskAggregate = serde_json::from_value(json!({
            "schema": "homeboy/agent-task-aggregate/v1",
            "plan_id": plan.plan_id,
            "status": "succeeded",
            "totals": { "skipped": 0, "succeeded": 1, "failed": 0 },
            "outcomes": [{
                "schema": AGENT_TASK_OUTCOME_SCHEMA,
                "task_id": "task-a",
                "status": "succeeded"
            }]
        }))
        .expect("terminal aggregate");
        agent_task_lifecycle::record_run_aggregate(run_id, &plan, &aggregate)
            .expect("persist terminal aggregate");

        let (value, exit_code) = resume(StatusArgs {
            run_id: run_id.to_string(),
            exact: false,
            bridge: false,
            since_cursor: None,
            full: false,
            bounded: false,
            no_runner_probe: false,
            strict_subject_exit: false,
            watch: false,
            interval: "5s".to_string(),
            timeout: "30m".to_string(),
        })
        .expect("ordinary resume returns terminal aggregate");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-aggregate/v1");
        assert_eq!(value["status"], "succeeded");
        assert!(value.get("latest_event_cursor").is_none());
    });
}

#[test]
fn run_plan_maps_resolved_component_worktree_before_provider_dispatch() {
    with_temp_home(|| {
        let workspace = tempfile::tempdir().expect("workspace");
        init_runtime_component_checkout(workspace.path());
        let workspace_root = workspace.path().display().to_string();
        let observed_request = Arc::new(Mutex::new(None));
        let executor = Arc::new(CapturingExecutor {
            observed_request: Arc::clone(&observed_request),
        });
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("sample-agent-runtime".to_string());
        plan.tasks[0].workspace.branch = Some("fix/runtime-guidance".to_string());
        plan.tasks[0].workspace.base_ref = Some("origin/main".to_string());
        plan.tasks[0].workspace.task_url =
            Some("https://github.com/example/sample-agent-runtime/issues/179".to_string());
        plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
        plan.tasks[0].workspace.materialization = json!({
            "root": workspace_root
        });

        let (_value, exit_code) =
            run_loaded_plan(plan, None, executor).expect("run-plan completed");
        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");

        assert_eq!(exit_code, 0);
        assert_eq!(
            observed.workspace.mode,
            homeboy::agents::agent_tasks::AgentTaskWorkspaceMode::Existing
        );
        let executor_workspace = std::path::PathBuf::from(
            observed
                .workspace
                .root
                .as_deref()
                .expect("executor workspace"),
        );
        assert_ne!(executor_workspace, workspace.path());
        assert!(
            !executor_workspace.exists(),
            "the isolated attempt workspace is retired after the executor returns"
        );
        assert_eq!(
            observed.workspace.slug.as_deref(),
            Some("sample-agent-runtime")
        );
        assert!(observed.workspace.kind.is_none());
        assert!(observed.workspace.component_id.is_none());
        assert!(observed.workspace.branch.is_none());
        assert!(observed.workspace.base_ref.is_none());
        assert!(observed.workspace.task_url.is_none());
        assert!(observed.workspace.cleanup.is_none());
        assert!(observed.workspace.materialization.is_null());
    });
}

#[test]
fn run_plan_rejects_component_worktree_without_branch() {
    with_temp_home(|| {
        let mut plan = test_plan();
        plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
        plan.tasks[0].workspace.component_id = Some("sample-agent-runtime".to_string());

        let error = run_loaded_plan(plan, None, Arc::new(CapturingExecutor::default()))
            .expect_err("component worktree without branch rejected");
        let message = error.to_string();

        assert!(message.contains("workspace.branch"));
        assert!(message.contains("requires branch"));
    });
}

/// Build cook args toggling whether a `--verify` gate and `--no-finalize` are
/// present, so the gate-requirement validation (#7608) can be exercised both
/// ways. The run id is fixed; callers rely only on the early gate check.
fn gate_requirement_cook_args(
    source: &std::path::Path,
    with_verify: bool,
    no_finalize: bool,
) -> AgentTaskCookArgs {
    let mut args = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
        "--prompt".to_string(),
        "read-only exploration".to_string(),
        "--cwd".to_string(),
        source.display().to_string(),
        "--to-worktree".to_string(),
        source.display().to_string(),
        "--backend".to_string(),
        "fixture".to_string(),
        "--run-id".to_string(),
        "cook-gate-requirement".to_string(),
    ];
    if with_verify {
        args.push("--verify".to_string());
        args.push("true".to_string());
    }
    if no_finalize {
        args.push("--no-finalize".to_string());
    }
    let cli = Cli::parse_from(args);
    let Commands::AgentTask(agent_task) = cli.command else {
        panic!("agent-task command");
    };
    let AgentTaskCommand::Cook(cook) = agent_task.command else {
        panic!("cook command");
    };
    *cook
}

#[test]
fn cook_without_gate_but_with_no_finalize_passes_the_gate_requirement() {
    // #7608: a read-only / exploratory `--no-finalize` cook has nothing to
    // publish, so it must not be rejected for lacking a deterministic gate.
    // It should get past the gate-requirement check; whatever it fails on
    // afterwards, it must NOT be the "requires ... --verify" rejection.
    with_temp_home(|| {
        let source = tempfile::tempdir().expect("source checkout");
        init_runtime_component_checkout(source.path());
        let target_root = tempfile::tempdir().expect("linked worktree parent");
        let target = target_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fixture-gate-no-finalize",
                target.to_str().expect("target path"),
                "HEAD",
            ])
            .current_dir(source.path())
            .status()
            .expect("create linked worktree")
            .success());

        let result = run_cook_with_executor(
            gate_requirement_cook_args(&target, false, true),
            Arc::new(CapturingExecutor::default()),
        );

        if let Err(error) = result {
            assert!(
                !error.message.contains("deterministic") && !error.message.contains("--verify"),
                "a --no-finalize cook must not be rejected for a missing gate, got: {}",
                error.message
            );
        }
    });
}

#[test]
fn cook_without_gate_and_finalizing_reports_actionable_gate_error() {
    // #7608: a finalizing cook (no --no-finalize) still requires a gate, but
    // the rejection must be actionable — naming the flag and giving
    // copy-pasteable next steps rather than a bare validation stub.
    with_temp_home(|| {
        let source = tempfile::tempdir().expect("source checkout");
        init_runtime_component_checkout(source.path());
        let target_root = tempfile::tempdir().expect("linked worktree parent");
        let target = target_root.path().join("task");
        assert!(Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "fixture-gate-finalize",
                target.to_str().expect("target path"),
                "HEAD",
            ])
            .current_dir(source.path())
            .status()
            .expect("create linked worktree")
            .success());

        let error = validate_cook_request(&gate_requirement_cook_args(&target, false, false))
            .expect_err("finalizing cook without a gate is rejected");

        assert_eq!(error.details["field"], "verify");
        assert!(
            error.message.contains("--verify") || error.message.contains("--private-verify"),
            "error should name the verify flag: {}",
            error.message
        );
        let hints = error.details["tried"]
            .as_array()
            .expect("actionable remediation hints");
        assert!(
            hints
                .iter()
                .any(|hint| hint.as_str().is_some_and(|hint| hint.contains("--verify"))),
            "a hint must give a copy-pasteable --verify example: {hints:?}"
        );
        assert!(
            hints.iter().any(|hint| hint
                .as_str()
                .is_some_and(|hint| hint.contains("--no-finalize"))),
            "a hint must point read-only cooks at --no-finalize: {hints:?}"
        );
    });
}
