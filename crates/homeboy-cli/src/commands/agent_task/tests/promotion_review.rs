//! Agent-task command promotion source resolution and review/loop reporting tests.

use super::support::*;
use crate::core::agent_task_service::DerivedCookBaselineCapability;

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
            provider_argv: Vec::new(),
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
            provider_argv: Vec::new(),
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
                "run-review-completed",
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
fn cook_returns_durable_id_when_promotion_provider_is_missing() {
    with_temp_home(|| {
        let (value, exit_code) = run_cook_with_executor(
            AgentTaskCookArgs {
                dispatch: DispatchArgs {
                    prompt: None,
                    tasks: Vec::new(),
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
                    concurrency: 1,
                    run_id: Some("cook-missing-provider".to_string()),
                    core: DispatchCoreArgs {
                        tasks_json: None,
                        provider_config: None,
                        client_context: None,
                        attempts: 1,
                        same_provider_retries: 0,
                        provider_rotations: 0,
                        queue_only: false,
                        timeout_ms: None,
                        resolved_provider_policy: None,
                    },
                },
                attempt_run_id: Some("cook-missing-provider-attempt-1-controller".to_string()),
                attempt_plan: None,
                goal: Some("cook fixture".to_string()),
                to_worktree: "homeboy@fix-agent-task-runner-cook".to_string(),
                provider_command: None,
                provider_argv: Vec::new(),
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
        .expect("cook reported controlled failure");

        assert_eq!(exit_code, 1);
        assert_eq!(value["schema"], "homeboy/agent-task-cook/v1");
        assert_eq!(value["cook_id"], "cook-missing-provider");
        assert_eq!(
            value["latest_run_id"],
            "cook-missing-provider-attempt-1-controller"
        );
        assert_eq!(
            value["history_run_ids"].as_array().expect("history").len(),
            1
        );
        assert_eq!(value["status"], "policy_failure");
        assert_eq!(value["attempts"][0]["run_id"], value["latest_run_id"]);
        assert!(value["stop_reason"]
            .as_str()
            .expect("stop reason")
            .contains("no worktree providers are configured"));
    });
}

#[derive(Debug, Clone)]
struct CommittingExecutor {
    workspace: std::path::PathBuf,
}

impl AgentTaskExecutorAdapter for CommittingExecutor {
    fn execute(
        &self,
        request: AgentTaskRequest,
        _context: AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        let workspace = std::path::PathBuf::from(
            request
                .workspace
                .root
                .as_deref()
                .expect("isolated workspace"),
        );
        assert_ne!(
            workspace, self.workspace,
            "executor must not receive the source workspace"
        );
        std::fs::write(workspace.join("agent-change.txt"), "committed work\n")
            .expect("write executor change");
        let status = Command::new("git")
            .args(["add", "agent-change.txt"])
            .current_dir(&workspace)
            .status()
            .expect("stage executor change");
        assert!(status.success());
        let status = Command::new("git")
            .args(["commit", "-m", "agent: make committed change"])
            .current_dir(&workspace)
            .status()
            .expect("commit executor change");
        assert!(status.success());

        AgentTaskOutcome {
            schema: AGENT_TASK_OUTCOME_SCHEMA.to_string(),
            task_id: request.task_id,
            status: AgentTaskOutcomeStatus::Succeeded,
            summary: Some("committed work".to_string()),
            failure_classification: None,
            artifacts: vec![
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "agent-result".to_string(),
                    kind: "agent_result".to_string(),
                    name: Some("agent-result.json".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(workspace.join("plugin.php").display().to_string()),
                    url: None,
                    mime: Some("application/json".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                },
                AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "transcript".to_string(),
                    kind: "transcript".to_string(),
                    name: Some("transcript.log".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some(workspace.join("plugin.php").display().to_string()),
                    url: None,
                    mime: Some("text/plain".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                },
            ],
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

/// Mimics the typed Lab lifecycle mirror: the provider executes elsewhere, but
/// the completed aggregate is written under the controller-owned attempt id.
#[derive(Debug, Clone)]
struct MirroredAttemptDispatcher {
    executor: CommittingExecutor,
    prepared: Arc<std::sync::atomic::AtomicBool>,
}

impl crate::core::agent_task_service::AgentTaskCookAttemptDispatcher for MirroredAttemptDispatcher {
    fn durable_recipe(&self) -> homeboy::core::Result<serde_json::Value> {
        Ok(serde_json::json!({ "kind": "local" }))
    }

    fn prepare_for_cook(&self) -> homeboy::core::Result<()> {
        self.prepared
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Ok(())
    }

    fn dispatch_attempt(
        &self,
        plan: AgentTaskPlan,
        run_id: &str,
        _derived_cook_baseline: Option<&DerivedCookBaselineCapability>,
    ) -> homeboy::core::Result<()> {
        assert!(
            self.prepared.load(std::sync::atomic::Ordering::SeqCst),
            "cook must prepare the dispatcher before pinning and dispatching its attempt"
        );
        homeboy::core::agent_tasks::service::run_loaded_plan(
            plan,
            Some(run_id),
            self.executor.clone(),
        )
        .map(|_| ())
    }
}

#[test]
fn cook_promotes_mirrored_remote_attempt_into_controller_target() {
    with_temp_home(|| {
        let mut config = homeboy::core::defaults::load_config();
        config.agent_task.rotation = Some(
            homeboy::core::agent_task_scheduler::AgentTaskProviderRotationPolicy {
                entries: vec![
                    homeboy::core::agent_task_scheduler::AgentTaskProviderRotationEntry {
                        model: Some("openai/gpt-5.6-terra".to_string()),
                        ..Default::default()
                    },
                    homeboy::core::agent_task_scheduler::AgentTaskProviderRotationEntry {
                        model: Some("fallback-model".to_string()),
                        ..Default::default()
                    },
                ],
                ..Default::default()
            },
        );
        homeboy::core::defaults::save_config(&config).expect("save provider rotation");
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        std::fs::create_dir(&source).expect("create source");
        init_runtime_component_checkout(&source);
        let status = Command::new("git")
            .args([
                "clone",
                source.to_str().expect("source path"),
                target.to_str().expect("target path"),
            ])
            .status()
            .expect("clone target");
        assert!(status.success());
        let expected_patch = temp.path().join("expected.patch");
        let promotion_request = temp.path().join("promotion-request.json");
        std::fs::write(
            &expected_patch,
            "diff --git a/agent-change.txt b/agent-change.txt\nnew file mode 100644\nindex 0000000..f3f8b32\n--- /dev/null\n+++ b/agent-change.txt\n@@ -0,0 +1 @@\n+committed work\n",
        )
        .expect("write expected patch");
        let provider = temp.path().join("promotion-provider.sh");
        std::fs::write(
            &provider,
            format!(
                "#!/bin/sh\nset -eu\ncat > {}\ngit -C {} apply {}\nprintf '%s\\n' '{{\"schema\":\"homeboy/agent-task-promotion-apply-response/v1\",\"workspace_path\":\"{}\"}}'\n",
                promotion_request.display(),
                target.display(),
                expected_patch.display(),
                target.display(),
            ),
        )
        .expect("write promotion provider");

        let executor = CommittingExecutor {
            workspace: source.clone(),
        };
        let prepared = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (value, exit_code) = run_cook_with_executor_and_dispatcher(
            AgentTaskCookArgs {
                dispatch: DispatchArgs {
                    prompt: Some("commit a change".to_string()),
                    tasks: Vec::new(),
                    cwd: Some(source.display().to_string()),
                    workspace: None,
                    repo: Some("fixture-component".to_string()),
                    task_url: None,
                    backend: Some("fixture".to_string()),
                    selector: None,
                    model: None,
                    required_capabilities: Vec::new(),
                    secret_env: Vec::new(),
                    concurrency: 1,
                    run_id: Some("cook-committed-work".to_string()),
                    core: DispatchCoreArgs {
                        tasks_json: None,
                        provider_config: None,
                        client_context: None,
                        attempts: 1,
                        same_provider_retries: 0,
                        provider_rotations: 0,
                        queue_only: false,
                        timeout_ms: None,
                        resolved_provider_policy: None,
                    },
                },
                attempt_run_id: None,
                attempt_plan: None,
                goal: None,
                to_worktree: "fixture-component@promoted".to_string(),
                provider_command: None,
                provider_argv: vec!["sh".to_string(), provider.display().to_string()],
                gates: VerifyGateArgs {
                    verify: vec!["true".to_string()],
                    private_verify: Vec::new(),
                    private_gate_reveal: AgentTaskGateRevealPolicy::FullEvidence,
                },
                max_attempts: 1,
                no_finalize: true,
                base: "main".to_string(),
                head: None,
                title: None,
                commit_message: None,
                protected_branches: review::default_protected_branches(),
                ai_tool: "OpenCode (GPT-5.6 Sol)".to_string(),
                ai_used_for: "test".to_string(),
            },
            executor.clone(),
            Some(Arc::new(MirroredAttemptDispatcher {
                executor,
                prepared: prepared.clone(),
            })),
        )
        .expect("cook completes");

        assert!(prepared.load(std::sync::atomic::Ordering::SeqCst));
        assert_eq!(exit_code, 0, "{value:#}");
        assert_eq!(value["status"], "green_no_finalize");
        let attempt_run_id = value["attempts"][0]["run_id"]
            .as_str()
            .expect("cook report attempt run id");
        let lifecycle = lifecycle_status(attempt_run_id).expect("local cook lifecycle");
        assert_eq!(lifecycle.lifecycle.provider_runtime.len(), 1);
        assert_eq!(
            lifecycle.lifecycle.provider_runtime[0].metadata["model"],
            "openai/gpt-5.6-terra"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["patch_artifact"]["id"],
            "cook-fixture-component-attempt-1-committed-changes"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["changed_files"],
            json!(["agent-change.txt"])
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["provenance"]["artifact_metadata"]["change_source"],
            "local_commits"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["provenance"]["artifact_metadata"]
                ["artifact_provenance"],
            "homeboy_generated_committed_patch"
        );
        assert_eq!(
            value["attempts"][0]["promotion"]["provenance"]["artifact_metadata"]["commits"]
                .as_array()
                .map(Vec::len),
            Some(1)
        );
        let status = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&source)
            .output()
            .expect("read workspace status");
        assert!(String::from_utf8_lossy(&status.stdout).trim().is_empty());
        assert_eq!(
            std::fs::read_to_string(target.join("agent-change.txt")).expect("target patch applied"),
            "committed work\n"
        );
        let request: Value = serde_json::from_str(
            &std::fs::read_to_string(&promotion_request).expect("read promotion request"),
        )
        .expect("typed promotion request");
        assert_eq!(
            request["schema"],
            "homeboy/agent-task-promotion-apply-request/v1"
        );
        assert_eq!(request["to_workspace"], "fixture-component@promoted");
        assert_eq!(request["changed_files"], json!(["agent-change.txt"]));
        assert!(request["patch"]
            .as_str()
            .expect("inline selected patch")
            .contains("committed work"));
    });
}
