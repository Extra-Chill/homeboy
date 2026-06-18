//! Tests for the `agent-task` command tree. Kept in a sibling file so the
//! module root stays a thin dispatcher under the structural thresholds.

use std::process::Command;

use super::super::agent_task_dispatch::DispatchArgs;
use super::args::{
    AgentTaskLoopArgs, CompileLoopArgs, ReviewArgs, StatusArgs, SubmitArgs, VerifyGateArgs,
};
use super::controller::{
    apply_from_spec_dispatch_defaults, apply_from_spec_dispatch_defaults_with_cwd,
    controller_run_action_with_executor, controller_run_next_with_executor,
    dispatch_args_from_controller_request,
};
use super::run::{
    retry, run_loaded_plan, run_loop_with_executor, run_next_with_executor,
    run_resume_with_executor, run_submitted, submit,
};
use super::status::{cancel, logs, status};
use super::{review, CancelArgs, ProvidersArgs, RetryArgs};
use homeboy::core::agent_tasks::controller_service::{
    AgentTaskRepoLoopSpec, ControllerFromSpecRequest,
};
use homeboy::core::agent_tasks::gate::AgentTaskGateRevealPolicy;
use homeboy::core::agent_tasks::provider::ExtensionProviderAgentTaskExecutor;
use homeboy::core::agent_tasks::provider::{
    AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA, AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA,
};
use homeboy::core::agent_tasks::scheduler::{AgentTaskExecutorAdapter, AgentTaskPlan};

use crate::test_support::with_isolated_home;
use homeboy::core::agent_tasks::controller_service as agent_task_controller_service;
use homeboy::core::agent_tasks::lifecycle::{
    self as agent_task_lifecycle, status as lifecycle_status, AgentTaskRunRecord, AgentTaskRunState,
};
use homeboy::core::agent_tasks::loop_controller::{
    self as agent_task_loop_controller, AgentTaskLoopActionStatus, AgentTaskLoopPolicyAction,
};
use homeboy::core::agent_tasks::scheduler::{AgentTaskExecutionContext, AgentTaskState};
use homeboy::core::agent_tasks::{
    AgentTaskArtifact, AgentTaskDiagnostic, AgentTaskEvidenceRef, AgentTaskExecutor,
    AgentTaskFailureClassification, AgentTaskLimits, AgentTaskOutcome, AgentTaskOutcomeStatus,
    AgentTaskPolicy, AgentTaskRequest, AgentTaskWorkspace, AGENT_TASK_ARTIFACT_SCHEMA,
    AGENT_TASK_OUTCOME_SCHEMA, AGENT_TASK_REQUEST_SCHEMA,
};
use serde_json::{json, Value};
use std::sync::{Arc, Mutex};

use super::contract;
use super::loop_definition;
use super::{ContractArgs, ContractFormat};

#[test]
fn providers_output_includes_core_capability_contract() {
    with_isolated_home(|_| {
        let (value, status) = review::providers(ProvidersArgs {
            secret_env: Vec::new(),
        })
        .expect("providers output");

        assert_eq!(status, 0);
        assert_eq!(
            value["capability_contract"]["schema"],
            AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
        );
        assert_eq!(
            value["capability_contract"]["provider_schema"],
            AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
        );
        assert_eq!(
            value["capability_contract"]["request_schema"],
            AGENT_TASK_REQUEST_SCHEMA
        );
        assert_eq!(
            value["capability_contract"]["outcome_schema"],
            AGENT_TASK_OUTCOME_SCHEMA
        );
    });
}

#[test]
fn contract_output_exports_core_agent_task_metadata() {
    let (value, status) = contract::contract(ContractArgs {
        format: ContractFormat::Json,
    })
    .expect("contract output");

    assert_eq!(status, 0);
    assert_eq!(value["schema"], "homeboy/agent-task-core-contract/v1");
    assert_eq!(value["schemas"]["request"], AGENT_TASK_REQUEST_SCHEMA);
    assert_eq!(
        value["schemas"]["artifact_declaration"],
        "homeboy/agent-task-artifact-declaration/v1"
    );
    assert_eq!(
        value["schemas"]["evidence_ref"],
        "homeboy/agent-task-evidence-ref/v1"
    );
    assert_eq!(
        value["schemas"]["secret_env_requirement"],
        "homeboy/secret-env-requirement/v1"
    );
    assert_eq!(
        value["schemas"]["loop_definition"],
        "homeboy/agent-task-loop-definition/v1"
    );
    assert_eq!(
        value["schemas"]["provider"],
        AGENT_TASK_EXECUTOR_PROVIDER_SCHEMA
    );
    assert_eq!(
        value["provider_capability"]["schema"],
        AGENT_TASK_PROVIDER_CAPABILITY_CONTRACT_SCHEMA
    );
    assert_eq!(
        value["provider_capability"]["request_required_fields"],
        json!(["schema", "task_id", "executor.backend", "instructions"])
    );
    assert_eq!(
        value["provider_capability"]["redacted_metadata_keys"],
        json!(["secret_env_values", "secretEnvValues", "secrets"])
    );
    assert!(value["enums"]["outcome_status"]
        .as_array()
        .expect("outcome statuses")
        .contains(&json!("provider_error")));
    assert!(value["enums"]["failure_classification"]
        .as_array()
        .expect("failure classifications")
        .contains(&json!("capability_missing")));
    assert!(value["redaction_defaults"]["sensitive_keys"]
        .as_array()
        .expect("sensitive keys")
        .contains(&json!("refresh_token")));
}

#[test]
fn compile_loop_command_emits_agent_task_plan() {
    let temp = tempfile::tempdir().expect("tempdir");
    let definition_path = temp.path().join("loop-definition.json");
    std::fs::write(
        &definition_path,
        serde_json::to_string(&json!({
            "schema": "homeboy/agent-task-loop-definition/v1",
            "loop_id": "cli/loop",
            "tasks": [
                { "task_id": "idea", "request": agent_task_request_json("idea") },
                {
                    "task_id": "design",
                    "request": agent_task_request_json("design"),
                    "depends_on": ["idea"],
                    "bindings": {
                        "concept_packet": { "task_id": "idea", "path": "/outputs/concept_packet" }
                    }
                }
            ]
        }))
        .expect("definition json"),
    )
    .expect("write definition");

    let (value, status) = loop_definition::compile_loop(CompileLoopArgs {
        definition: format!("@{}", definition_path.display()),
    })
    .expect("compile loop");

    assert_eq!(status, 0);
    assert_eq!(value["schema"], "homeboy/agent-task-plan/v1");
    assert_eq!(value["plan_id"], "cli/loop");
    assert_eq!(value["tasks"].as_array().expect("tasks").len(), 2);
    assert_eq!(
        value["output_dependencies"]["design"]["bindings"]["concept_packet"]["task_id"],
        "idea"
    );
}

#[test]
fn compile_loop_command_emits_plan_from_repo_loop_spec() {
    let temp = tempfile::tempdir().expect("tempdir");
    let definition_path = temp.path().join("repo-loop.json");
    std::fs::write(
        &definition_path,
        serde_json::to_string(&json!({
            "schema": "wpsg/loop-spec/v1",
            "loop_id": "wpsg/site-loop",
            "metadata": {
                "group_key": "wpsg-site",
                "dispatch_defaults": {
                    "backend": "fixture",
                    "selector": "local",
                    "cwd": temp.path().display().to_string(),
                    "repo": "wp-site-generator@fixture"
                }
            },
            "agents": [
                { "agent_id": "builder", "tools": ["write-file"], "abilities": ["render-blocks"] }
            ],
            "artifacts": [
                { "artifact_id": "site_brief", "kind": "wpsg/SiteBrief/v1", "required": true },
                { "artifact_id": "theme_patch", "kind": "homeboy/Patch/v1", "required": true }
            ],
            "workflows": [
                {
                    "workflow_id": "brief",
                    "agent_id": "builder",
                    "prompt": "Draft the site brief.",
                    "emits": ["site_brief"]
                },
                {
                    "workflow_id": "build",
                    "prompt": "Build from the site brief.",
                    "consumes": ["site_brief"],
                    "emits": ["theme_patch"]
                }
            ]
        }))
        .expect("definition json"),
    )
    .expect("write definition");

    let (value, status) = loop_definition::compile_loop(CompileLoopArgs {
        definition: format!("@{}", definition_path.display()),
    })
    .expect("compile loop");

    assert_eq!(status, 0);
    assert_eq!(value["schema"], "homeboy/agent-task-plan/v1");
    assert_eq!(value["plan_id"], "wpsg/site-loop");
    assert_eq!(value["group_key"], "wpsg-site");
    assert_eq!(value["tasks"][0]["task_id"], "brief");
    assert_eq!(value["tasks"][0]["executor"]["backend"], "fixture");
    assert_eq!(
        value["tasks"][0]["executor"]["required_capabilities"],
        json!(["tool:write-file", "ability:render-blocks"])
    );
    assert_eq!(
        value["tasks"][0]["workspace"]["slug"],
        "wp-site-generator@fixture"
    );
    assert_eq!(
        value["output_dependencies"]["build"]["depends_on"],
        json!(["brief"])
    );
    assert_eq!(
        value["output_dependencies"]["build"]["bindings"]["site_brief"]["task_id"],
        "brief"
    );
    assert_eq!(
        value["artifact_outputs"]["brief"][0]["kind"],
        "wpsg/SiteBrief/v1"
    );
}

#[test]
fn compile_loop_command_rejects_controller_only_sections() {
    let error = loop_definition::compile_loop(CompileLoopArgs {
        definition: serde_json::to_string(&json!({
            "loop_id": "repo-loop-with-controller-policy",
            "workflows": [
                { "workflow_id": "brief", "prompt": "Draft the site brief." }
            ],
            "policy": { "policy_id": "runtime-policy", "transitions": [] }
        }))
        .expect("definition json"),
    })
    .expect_err("controller-only section is rejected");

    assert!(error.message.contains("controller-only sections"));
    assert!(error.details["tried"]
        .as_array()
        .expect("diagnostics")
        .iter()
        .any(|diagnostic| diagnostic.as_str().unwrap_or_default().contains("policy")));
}

#[test]
fn from_spec_dispatch_defaults_use_spec_git_checkout() {
    let repo = tempfile::tempdir().expect("repo dir");
    let git_status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .status()
        .expect("git init runs");
    assert!(git_status.success());
    let spec_dir = repo.path().join(".github/homeboy/controllers");
    std::fs::create_dir_all(&spec_dir).expect("spec dir");
    let spec_path = spec_dir.join("loop.json");
    std::fs::write(&spec_path, "{}").expect("spec file");
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-cli-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: Value::Null,
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
        dependencies: Vec::new(),
        gates: Vec::new(),
        metrics: Vec::new(),
        gate_bundles: Vec::new(),
        policy: None,
        phases: Vec::new(),
        actions: Vec::new(),
        initial_event: None,
    };

    apply_from_spec_dispatch_defaults(&mut spec, &format!("@{}", spec_path.display()));
    let expected_root = std::fs::canonicalize(repo.path()).expect("canonical repo path");

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(
        spec.metadata["dispatch_defaults"]["repo"],
        repo.path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string()
    );
}

#[test]
fn from_spec_dispatch_defaults_fall_back_to_current_git_checkout() {
    let repo = tempfile::tempdir().expect("repo dir");
    let git_status = Command::new("git")
        .arg("-C")
        .arg(repo.path())
        .arg("init")
        .status()
        .expect("git init runs");
    assert!(git_status.success());
    let mut spec = AgentTaskRepoLoopSpec {
        schema: None,
        loop_id: "repo-loop-cli-cwd-defaults".to_string(),
        phase: "init".to_string(),
        config_version: "v1".to_string(),
        metadata: Value::Null,
        entities: Vec::new(),
        agents: Vec::new(),
        tools: Vec::new(),
        abilities: Vec::new(),
        workflows: Vec::new(),
        artifacts: Vec::new(),
        dependencies: Vec::new(),
        gates: Vec::new(),
        metrics: Vec::new(),
        gate_bundles: Vec::new(),
        policy: None,
        phases: Vec::new(),
        actions: Vec::new(),
        initial_event: None,
    };
    spec.workflows.push(
        homeboy::core::agent_tasks::controller_service::AgentTaskRepoLoopSpecWorkflow {
            workflow_id: "store-idea".to_string(),
            agent_id: None,
            prompt: Some("cook the next workflow".to_string()),
            tasks: Vec::new(),
            entity_ids: Vec::new(),
            tools: Vec::new(),
            abilities: Vec::new(),
            artifacts: Vec::new(),
            consumes: Vec::new(),
            emits: Vec::new(),
            dependencies: Vec::new(),
            gates: Vec::new(),
            metrics: Vec::new(),
            inputs: Value::Null,
        },
    );

    apply_from_spec_dispatch_defaults_with_cwd(&mut spec, "-", || Some(repo.path().to_path_buf()));
    let expected_root = std::fs::canonicalize(repo.path()).expect("canonical repo path");
    let expected_repo = repo
        .path()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .to_string();

    assert_eq!(
        spec.metadata["dispatch_defaults"]["cwd"],
        expected_root.display().to_string()
    );
    assert_eq!(spec.metadata["dispatch_defaults"]["repo"], expected_repo);

    with_isolated_home(|_| {
        let report = agent_task_controller_service::init_from_spec(ControllerFromSpecRequest {
            spec: spec.clone(),
        })
        .expect("from-spec initialized");
        match &report.actions[0].action {
            AgentTaskLoopPolicyAction::SpawnTask { request, .. } => {
                assert_eq!(
                    request["dispatch"]["cwd"].as_str(),
                    Some(expected_root.display().to_string().as_str())
                );
                assert_eq!(
                    request["dispatch"]["repo"].as_str(),
                    Some(expected_repo.as_str())
                );
            }
            other => panic!("expected compiled spawn task, got {other:?}"),
        }
    });
}

#[test]
fn controller_dispatch_args_preserve_top_level_workspace_context_in_plan() {
    let repo = tempfile::tempdir().expect("repo dir");
    let repo_path = repo.path().display().to_string();
    let request = json!({
        "mode": "dispatch",
        "cwd": repo_path.clone(),
        "repo": "wp-site-generator@canonical-loop-main-20260616",
        "dispatch": {
            "prompt": "cook the next workflow",
            "backend": "codebox"
        }
    });

    let args = dispatch_args_from_controller_request(&request).expect("dispatch args");
    let dispatch_request = homeboy::core::agent_tasks::dispatch_service::AgentTaskDispatchRequest {
        prompt: args.prompt,
        tasks: args.tasks,
        tasks_json: args.tasks_json,
        cwd: args.cwd,
        workspace: args.workspace,
        repo: args.repo,
        task_url: args.task_url,
        backend: args.backend.expect("backend"),
        selector: args.selector,
        model: args.model,
        required_capabilities: args.required_capabilities,
        secret_env: args.secret_env,
        provider_config: args.provider_config,
        client_context: args.client_context,
        concurrency: args.concurrency,
        attempts: args.attempts,
        run_id: args.run_id,
        queue_only: args.queue_only,
    };
    let plan = homeboy::core::agent_tasks::dispatch_service::build_dispatch_plan_with_provider_requirements(
            &dispatch_request,
            |_backend, _selector| false,
        )
        .expect("dispatch plan");
    let task = plan.tasks.first().expect("plan task");

    assert_eq!(task.workspace.root.as_deref(), Some(repo_path.as_str()));
    assert_eq!(
        task.workspace.slug.as_deref(),
        Some("wp-site-generator@canonical-loop-main-20260616")
    );
    assert_eq!(
        task.executor.config["workspace_root"].as_str(),
        Some(repo_path.as_str())
    );
    assert_eq!(
        task.executor.config["repo"].as_str(),
        Some("wp-site-generator@canonical-loop-main-20260616")
    );
    assert_eq!(
        plan.metadata["workspace_root"].as_str(),
        Some(repo_path.as_str())
    );
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
        let (_, run_exit_code) = run_submitted(StatusArgs {
            run_id: "run-cli-terminal".to_string(),
            full: false,
        })
        .expect("run completed");
        let (status_json, status_exit_code) = status(StatusArgs {
            run_id: "run-cli-terminal".to_string(),
            full: true,
        })
        .expect("status loaded");
        let record: AgentTaskRunRecord = serde_json::from_value(status_json).expect("record");

        assert_eq!(run_exit_code, 1);
        assert_eq!(status_exit_code, 0);
        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(record.tasks[0].state, AgentTaskState::Failed);
        assert_eq!(record.totals.expect("totals").failed, 1);
    });
}

#[test]
fn failed_run_status_logs_and_review_include_outcome_diagnostic_summary() {
    with_temp_home(|| {
        let run_id = "run-cli-diagnostic-summary";
        run_loaded_plan(test_plan(), Some(run_id), DiagnosticFailureExecutor)
            .expect("run completed with failed outcome");

        let (status_value, _) = status(StatusArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("status loaded");
        let (logs_value, _) = logs(StatusArgs {
            run_id: run_id.to_string(),
            full: false,
        })
        .expect("logs loaded");
        let (review_value, _) = review::review(ReviewArgs {
            run_id: run_id.to_string(),
            to_worktree: None,
            provider_command: None,
        })
        .expect("review loaded");

        for value in [&status_value, &logs_value, &review_value] {
            assert_eq!(
                value["diagnostic_summary"]["message"],
                "Requested provider \"codex\" is not registered. Registered provider plugins: []"
            );
            assert_eq!(value["diagnostic_summary"]["class"], "provider_discovery");
            assert_eq!(value["diagnostic_summary"]["task_id"], "task-a");
        }
    });
}

#[test]
fn run_plan_record_run_id_persists_running_status_before_executor_runs() {
    with_temp_home(|| {
        let run_id = "run-plan-durable";
        let observed_status = Arc::new(Mutex::new(None));
        let executor = InspectingExecutor {
            run_id: run_id.to_string(),
            observed_status: Arc::clone(&observed_status),
        };

        let (_value, exit_code) =
            run_loaded_plan(test_plan(), Some(run_id), executor).expect("run-plan completed");

        let observed = observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("executor observed durable status");
        assert_eq!(exit_code, 0);
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
fn run_plan_fails_fast_when_required_secret_env_is_missing() {
    with_temp_home(|| {
        let missing_secret = "HOMEBOY_AGENT_TASK_MISSING_PROVIDER_SECRET_TEST";
        std::env::remove_var(missing_secret);
        let mut plan = test_plan();
        plan.tasks[0].executor.secret_env = vec![missing_secret.to_string()];
        let executor = CapturingExecutor::default();
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
        assert!(lifecycle_status("run-plan-missing-secret").is_err());
    });
}

#[test]
fn run_next_claims_oldest_queued_run_and_leaves_later_runs_queued() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-a"))
            .expect("first submitted");
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-next-b"))
            .expect("second submitted");
        let observed_status = Arc::new(Mutex::new(None));

        let (_value, exit_code) = run_next_with_executor(InspectingExecutor {
            run_id: "run-next-a".to_string(),
            observed_status: Arc::clone(&observed_status),
        })
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
fn run_next_returns_unclaimed_when_no_queued_runs_exist() {
    with_temp_home(|| {
        let (value, exit_code) = run_next_with_executor(InspectingExecutor {
            run_id: "unused".to_string(),
            observed_status: Arc::new(Mutex::new(None)),
        })
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
        let record: AgentTaskRunRecord = serde_json::from_value(value).expect("record");

        assert_eq!(exit_code, 0);
        assert_eq!(record.state, AgentTaskRunState::Cancelled);
        assert_eq!(record.tasks[0].state, AgentTaskState::Cancelled);
        assert_eq!(record.metadata["cancel_reason"], json!("not selected"));
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
fn resume_command_executes_existing_run() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-resume-cli")).expect("submitted");
        let observed_status = Arc::new(Mutex::new(None));

        let (_value, exit_code) = run_resume_with_executor(
            "run-resume-cli".to_string(),
            InspectingExecutor {
                run_id: "run-resume-cli".to_string(),
                observed_status: Arc::clone(&observed_status),
            },
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
fn run_plan_maps_resolved_component_worktree_before_provider_dispatch() {
    let observed_request = Arc::new(Mutex::new(None));
    let executor = CapturingExecutor {
        observed_request: Arc::clone(&observed_request),
    };
    let mut plan = test_plan();
    plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
    plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());
    plan.tasks[0].workspace.branch = Some("fix/179-homeboy-codebox-guidance".to_string());
    plan.tasks[0].workspace.base_ref = Some("origin/main".to_string());
    plan.tasks[0].workspace.task_url =
        Some("https://github.com/Extra-Chill/wp-coding-agents/issues/179".to_string());
    plan.tasks[0].workspace.cleanup = Some("preserve".to_string());
    plan.tasks[0].workspace.materialization = json!({
        "root": "/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance"
    });

    let (_value, exit_code) = run_loaded_plan(plan, None, executor).expect("run-plan completed");
    let observed = observed_request
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
        .expect("provider saw request");

    assert_eq!(exit_code, 0);
    assert_eq!(
        observed.workspace.mode,
        homeboy::core::agent_tasks::AgentTaskWorkspaceMode::Existing
    );
    assert_eq!(
        observed.workspace.root.as_deref(),
        Some("/tmp/homeboy-worktrees/wp-coding-agents@fix-179-homeboy-codebox-guidance")
    );
    assert_eq!(observed.workspace.slug.as_deref(), Some("wp-coding-agents"));
    assert!(observed.workspace.kind.is_none());
    assert!(observed.workspace.component_id.is_none());
    assert!(observed.workspace.branch.is_none());
    assert!(observed.workspace.base_ref.is_none());
    assert!(observed.workspace.task_url.is_none());
    assert!(observed.workspace.cleanup.is_none());
    assert!(observed.workspace.materialization.is_null());
}

#[test]
fn run_plan_rejects_component_worktree_without_branch() {
    let mut plan = test_plan();
    plan.tasks[0].workspace.kind = Some("component-worktree".to_string());
    plan.tasks[0].workspace.component_id = Some("wp-coding-agents".to_string());

    let error = run_loaded_plan(plan, None, CapturingExecutor::default())
        .expect_err("component worktree without branch rejected");
    let message = error.to_string();

    assert!(message.contains("workspace.branch"));
    assert!(message.contains("requires branch"));
}

#[test]
fn controller_run_next_executes_spawn_task_plan_and_records_dedupe_lineage() {
    with_temp_home(|| {
        let observed_request = Arc::new(Mutex::new(None));
        let mut controller = agent_task_loop_controller::create_controller(
            "loop-controller-run-next",
            "repair",
            "v1",
        )
        .expect("controller created");
        let mut plan = test_plan();
        plan.tasks[0].executor.selector = Some("homeboy-lab".to_string());
        plan.tasks[0].executor.config = json!({
            "artifact_root": "/tmp/homeboy-lab-artifacts/controller-run-next"
        });

        controller.record_action(
            AgentTaskLoopPolicyAction::SpawnTask {
                dedupe_key: "finding:abc:repair".to_string(),
                entity_id: None,
                request: json!({
                    "mode": "run_plan",
                    "run_id": "controller-run-next-a",
                    "plan": plan,
                }),
            },
            "finding emitted",
        );
        agent_task_loop_controller::write_controller(&controller).expect("controller written");

        let (value, exit_code) = controller_run_next_with_executor(
            "loop-controller-run-next".to_string(),
            CapturingExecutor {
                observed_request: Arc::clone(&observed_request),
            },
        )
        .expect("controller action executed");

        let observed = observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
            .expect("provider saw request");
        let loaded = agent_task_loop_controller::load_controller("loop-controller-run-next")
            .expect("controller loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["claimed"], true);
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Completed
        );
        assert_eq!(
            loaded.dedupe_keys["finding:abc:repair"].run_id.as_deref(),
            Some("controller-run-next-a")
        );
        assert_eq!(loaded.task_lineage[0].run_id, "controller-run-next-a");
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.claimed"));
        assert!(loaded
            .history
            .iter()
            .any(|event| event.event_type == "controller.action.completed"));
        assert_eq!(observed.executor.selector.as_deref(), Some("homeboy-lab"));
        assert_eq!(
            observed.executor.config["artifact_root"],
            json!("/tmp/homeboy-lab-artifacts/controller-run-next")
        );
    });
}

#[test]
fn controller_run_executes_requested_action_id_only() {
    with_temp_home(|| {
        let mut controller = agent_task_loop_controller::create_controller(
            "loop-controller-run-action",
            "repair",
            "v1",
        )
        .expect("controller created");
        controller.record_action(
            AgentTaskLoopPolicyAction::WaitForEvent(
                agent_task_loop_controller::AgentTaskLoopWait {
                    wait_key: "wait-a".to_string(),
                    event_type: "task.completed".to_string(),
                    entity_id: None,
                    external_ref: None,
                    timeout_at: None,
                    escalation_policy: None,
                    status: agent_task_loop_controller::AgentTaskLoopWaitStatus::Open,
                    satisfied_by_event_id: None,
                },
            ),
            "wait first",
        );
        controller.record_action(
            AgentTaskLoopPolicyAction::Complete {
                reason: Some("done".to_string()),
            },
            "complete second",
        );
        agent_task_loop_controller::write_controller(&controller).expect("controller written");

        let (_value, exit_code) = controller_run_action_with_executor(
            "loop-controller-run-action".to_string(),
            "action-2".to_string(),
            CapturingExecutor::default(),
        )
        .expect("specific action executed");
        let loaded = agent_task_loop_controller::load_controller("loop-controller-run-action")
            .expect("controller loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(
            loaded.next_actions[0].status,
            AgentTaskLoopActionStatus::Pending
        );
        assert_eq!(
            loaded.next_actions[1].status,
            AgentTaskLoopActionStatus::Completed
        );
    });
}

#[test]
fn promotion_source_resolves_completed_run_id() {
    with_temp_home(|| {
        let run_id = "run-promotion-source";

        run_loaded_plan(test_plan(), Some(run_id), InspectingExecutor::noop(run_id))
            .expect("run completed");

        let (raw, path) = review::read_promotion_source(run_id).expect("promotion source resolved");

        assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
        assert_eq!(
            path.as_ref()
                .and_then(|path| path.file_name())
                .and_then(|name| name.to_str()),
            Some("aggregate.json")
        );
    });
}

#[test]
fn promotion_source_reads_bare_json_file_path() {
    let file = tempfile::NamedTempFile::new().expect("source file");
    std::fs::write(
        file.path(),
        r#"{"schema":"homeboy/agent-task-aggregate/v1"}"#,
    )
    .expect("write source");

    let (raw, path) = review::read_promotion_source(&file.path().display().to_string())
        .expect("promotion source file resolved");

    assert!(raw.contains("homeboy/agent-task-aggregate/v1"));
    assert_eq!(path.as_deref(), Some(file.path()));
}

#[test]
fn review_reports_queued_run_without_chat_state() {
    with_temp_home(|| {
        agent_task_lifecycle::submit_plan(&test_plan(), Some("run-review-queued"))
            .expect("submitted");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-queued".to_string(),
            to_worktree: None,
            provider_command: None,
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["schema"], "homeboy/agent-task-review/v1");
        assert_eq!(value["run_id"], "run-review-queued");
        assert_eq!(value["state"], "queued");
        assert_eq!(value["transport"]["chat_state_required"], false);
        assert!(value["aggregate_review"].is_null());
        assert_eq!(value["logs"]["events"][0]["state"], "queued");
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("run-next"));
    });
}

#[test]
fn review_reports_completed_aggregate_and_promotion_hints() {
    with_temp_home(|| {
        run_loaded_plan(
            test_plan(),
            Some("run-review-completed"),
            ApplyArtifactExecutor,
        )
        .expect("run completed");

        let (value, exit_code) = review::review(ReviewArgs {
            run_id: "run-review-completed".to_string(),
            to_worktree: Some("homeboy@fix-review-flow".to_string()),
            provider_command: None,
        })
        .expect("review loaded");

        assert_eq!(exit_code, 0);
        assert_eq!(value["state"], "succeeded");
        assert_eq!(value["aggregate_review"]["summary"]["apply_candidates"], 1);
        assert_eq!(value["artifacts"]["artifacts"][0]["id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["task_id"], "task-a");
        assert_eq!(value["promotion_candidates"][0]["artifact_id"], "patch-a");
        assert_eq!(value["promotion_candidates"][0]["ready"], true);
        assert_eq!(
            value["promotion_candidates"][0]["command"],
            json!([
                "homeboy",
                "agent-task",
                "promote",
                value["aggregate_path"].as_str().expect("aggregate path"),
                "--task-id",
                "task-a",
                "--artifact-id",
                "patch-a",
                "--to-worktree",
                "homeboy@fix-review-flow"
            ])
        );
        assert!(value["next_actions"][0]
            .as_str()
            .expect("next action")
            .contains("promotion_candidates"));
    });
}

#[test]
fn loop_returns_durable_id_when_promotion_provider_is_missing() {
    with_temp_home(|| {
        let (value, exit_code) = run_loop_with_executor(
            AgentTaskLoopArgs {
                dispatch: DispatchArgs {
                    prompt: None,
                    tasks: Vec::new(),
                    tasks_json: None,
                    cwd: None,
                    workspace: None,
                    repo: Some("homeboy".to_string()),
                    task_url: Some(
                        "https://github.com/Extra-Chill/homeboy/issues/3675".to_string(),
                    ),
                    backend: Some("fixture".to_string()),
                    selector: None,
                    model: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    provider_config: None,
                    client_context: None,
                    concurrency: 1,
                    attempts: 1,
                    run_id: Some("cook-loop-missing-provider".to_string()),
                    queue_only: false,
                },
                goal: Some("cook fixture".to_string()),
                to_worktree: "homeboy@fix-agent-task-runner-cook".to_string(),
                provider_command: None,
                gates: VerifyGateArgs {
                    verify: vec!["cargo test --lib".to_string()],
                    private_verify: Vec::new(),
                    private_gate_reveal: AgentTaskGateRevealPolicy::SummaryOnly,
                },
                max_attempts: 2,
                no_finalize: false,
                base: "main".to_string(),
                head: None,
                title: None,
                commit_message: None,
                protected_branches: review::default_protected_branches(),
                ai_tool: "OpenCode (GPT-5.5)".to_string(),
                ai_used_for: "test".to_string(),
            },
            ExtensionProviderAgentTaskExecutor::default(),
        )
        .expect("loop reported controlled failure");

        assert_eq!(exit_code, 1);
        assert_eq!(value["schema"], "homeboy/agent-task-loop/v1");
        assert_eq!(value["loop_id"], "cook-loop-missing-provider");
        assert_eq!(value["status"], "policy_failure");
        assert_eq!(value["attempts"][0]["run_id"], "cook-loop-missing-provider");
        assert!(value["stop_reason"]
            .as_str()
            .expect("stop reason")
            .contains("workspace provider command"));
    });
}

struct InspectingExecutor {
    run_id: String,
    observed_status: Arc<Mutex<Option<AgentTaskRunRecord>>>,
}

impl InspectingExecutor {
    fn noop(run_id: &str) -> Self {
        Self {
            run_id: run_id.to_string(),
            observed_status: Arc::new(Mutex::new(None)),
        }
    }
}

impl AgentTaskExecutorAdapter for InspectingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let record = lifecycle_status(&self.run_id).expect("status exists before executor runs");
        *self
            .observed_status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(record);

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
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

#[derive(Clone, Default)]
struct CapturingExecutor {
    observed_request: Arc<Mutex<Option<AgentTaskRequest>>>,
}

impl AgentTaskExecutorAdapter for CapturingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        *self
            .observed_request
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(request.clone());

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("ok".to_string()),
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

struct DiagnosticFailureExecutor;

impl AgentTaskExecutorAdapter for DiagnosticFailureExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
                schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
                task_id: request.task_id,
                status: AgentTaskOutcomeStatus::ProviderError,
                summary: Some("Embedded agent runtime failed.".to_string()),
                failure_classification: Some(AgentTaskFailureClassification::Provider),
                artifacts: Vec::new(),
                typed_artifacts: Vec::new(),
                evidence_refs: Vec::new(),
                diagnostics: vec![AgentTaskDiagnostic {
                    class: "provider_discovery".to_string(),
                    message: "Requested provider \"codex\" is not registered. Registered provider plugins: []"
                        .to_string(),
                    data: json!({ "registered_provider_plugins": [] }),
                }],
                outputs: Value::Null,
                workflow: None,
                follow_up: None,
                metadata: Value::Null,
            }
    }
}

struct ApplyArtifactExecutor;

impl AgentTaskExecutorAdapter for ApplyArtifactExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("produced patch".to_string()),
            failure_classification: None,
            artifacts: vec![AgentTaskArtifact {
                schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                id: "patch-a".to_string(),
                kind: "patch".to_string(),
                name: Some("changes.patch".to_string()),
                path: Some("target/agent-task-review/changes.patch".to_string()),
                url: None,
                mime: Some("text/x-diff".to_string()),
                size_bytes: Some(42),
                sha256: Some("abc123".to_string()),
                metadata: Value::Null,
            }],
            typed_artifacts: Vec::new(),
            evidence_refs: vec![AgentTaskEvidenceRef {
                kind: "transcript".to_string(),
                uri: "target/agent-task-review/transcript.log".to_string(),
                label: Some("transcript".to_string()),
            }],
            diagnostics: Vec::new(),
            outputs: Value::Null,
            workflow: None,
            follow_up: None,
            metadata: Value::Null,
        }
    }
}

fn with_temp_home(run: impl FnOnce()) {
    with_isolated_home(|_| run());
}

fn test_plan() -> AgentTaskPlan {
    AgentTaskPlan::new(
        "plan-a",
        vec![AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "task-a".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "test".to_string(),
                selector: Some("fixture".to_string()),
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: Value::Null,
            },
            instructions: "run".to_string(),
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
    )
}

fn agent_task_request_json(task_id: &str) -> Value {
    let mut plan = test_plan();
    let mut request = plan.tasks.pop().expect("test task");
    request.task_id = task_id.to_string();
    request.instructions = format!("run {task_id}");
    serde_json::to_value(request).expect("request json")
}
