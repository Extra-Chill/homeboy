//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

mod provider_rotation_tests {
    use super::*;

    struct DirtyCandidateThenSuccessExecutor {
        observed_roots: Arc<Mutex<Vec<std::path::PathBuf>>>,
        calls: AtomicUsize,
    }

    struct AdoptionExecutor {
        observed: Arc<Mutex<Option<crate::agent_task::AgentTaskAttemptWorkspace>>>,
    }

    struct AdoptCandidateThenSuccessExecutor {
        calls: AtomicUsize,
        patch: String,
    }

    struct ProviderReportedRotationExecutor {
        calls: AtomicUsize,
    }

    struct DirtyCandidateThenTerminalExecutor {
        calls: AtomicUsize,
        terminal: AgentTaskOutcomeStatus,
        terminal_outputs: Value,
    }

    impl AgentTaskExecutorAdapter for ProviderReportedRotationExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let mut result = outcome(
                request.task_id,
                if call == 0 {
                    AgentTaskOutcomeStatus::ProviderError
                } else {
                    AgentTaskOutcomeStatus::Succeeded
                },
            );
            if call == 0 {
                result.failure_classification = Some(AgentTaskFailureClassification::Provider);
            } else {
                result.metadata = json!({ "model": "openai/gpt-5.6-actual" });
            }
            result
        }
    }

    impl AgentTaskExecutorAdapter for DirtyCandidateThenTerminalExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let root = request
                .workspace
                .root
                .as_deref()
                .expect("attempt workspace");
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                fs::write(
                    std::path::Path::new(root).join("candidate.txt"),
                    "candidate\n",
                )
                .expect("candidate edit");
                let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::Timeout);
                outcome.failure_classification = Some(AgentTaskFailureClassification::Timeout);
                return outcome;
            }

            let mut outcome = outcome(request.task_id, self.terminal);
            outcome.outputs = self.terminal_outputs.clone();
            if self.terminal == AgentTaskOutcomeStatus::ProviderError {
                outcome.failure_classification = Some(AgentTaskFailureClassification::Provider);
            }
            outcome
        }
    }

    impl AgentTaskExecutorAdapter for AdoptionExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let root = request.workspace.root.as_deref().expect("attempt root");
            assert!(std::path::Path::new(root).join("adopted.txt").is_file());
            self.observed
                .lock()
                .expect("observed workspace")
                .replace(request.workspace.attempt.expect("attempt ownership"));
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    impl AgentTaskExecutorAdapter for AdoptCandidateThenSuccessExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let root = request.workspace.root.as_deref().expect("attempt root");
                let patch_path = std::path::Path::new(root).join("candidate.patch");
                fs::write(&patch_path, &self.patch).expect("candidate patch");
                let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::ProviderError);
                outcome.failure_classification = Some(AgentTaskFailureClassification::Provider);
                outcome.artifacts.push(AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "candidate".to_string(),
                    kind: "patch".to_string(),
                    name: Some("candidate.patch".to_string()),
                    label: None,
                    role: Some("patch".to_string()),
                    semantic_key: None,
                    path: Some(patch_path.display().to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                });
                return outcome;
            }
            let root = request.workspace.root.as_deref().expect("attempt root");
            assert!(std::path::Path::new(root).join("candidate.txt").is_file());
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    impl AgentTaskExecutorAdapter for DirtyCandidateThenSuccessExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let call = self.calls.fetch_add(1, Ordering::SeqCst);
            let root = request
                .workspace
                .root
                .as_deref()
                .map(std::path::PathBuf::from)
                .expect("attempt workspace");
            self.observed_roots
                .lock()
                .expect("observed roots")
                .push(root.clone());
            assert_eq!(
                request.executor.config["workspace_root"],
                root.display().to_string(),
                "provider workspace config follows the isolated attempt root"
            );
            assert!(
                request
                    .executor
                    .config
                    .get("workspace_permission_root")
                    .is_none(),
                "scheduler must not add provider-owned config without a capability declaration"
            );
            assert_eq!(
                request.executor.config["cwd"],
                root.display().to_string(),
                "provider cwd follows the isolated attempt root"
            );
            if call == 0 {
                fs::write(root.join("candidate.txt"), "candidate\n").expect("candidate edit");
                let mut outcome = outcome(request.task_id, AgentTaskOutcomeStatus::Timeout);
                outcome.failure_classification = Some(AgentTaskFailureClassification::Timeout);
                return outcome;
            }
            assert!(
                !root.join("candidate.txt").exists(),
                "the rotated provider must receive a clean attempt checkout"
            );
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    fn rotation_policy(
        entries: Vec<AgentTaskProviderRotationEntry>,
    ) -> AgentTaskProviderRotationPolicy {
        AgentTaskProviderRotationPolicy {
            entries,
            max_attempts: None,
            ..AgentTaskProviderRotationPolicy::default()
        }
    }

    fn entry(backend: &str) -> AgentTaskProviderRotationEntry {
        AgentTaskProviderRotationEntry {
            backend: Some(backend.to_string()),
            ..AgentTaskProviderRotationEntry::default()
        }
    }

    fn enable_rotation(plan: &mut AgentTaskPlan) {
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: 10,
            max_same_provider_retries: 0,
            max_provider_rotations: 10,
        };
    }

    fn provider_failure() -> (
        AgentTaskOutcomeStatus,
        Option<AgentTaskFailureClassification>,
    ) {
        (
            AgentTaskOutcomeStatus::ProviderError,
            Some(AgentTaskFailureClassification::Provider),
        )
    }

    fn success() -> (
        AgentTaskOutcomeStatus,
        Option<AgentTaskFailureClassification>,
    ) {
        (AgentTaskOutcomeStatus::Succeeded, None)
    }

    #[test]
    fn total_execution_budget_of_one_prevents_provider_rotation() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: 1,
            max_same_provider_retries: 0,
            max_provider_rotations: 1,
        };

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["exhausted"],
            "total_executions"
        );
    }

    #[test]
    fn rotates_to_next_entry_on_provider_failure_and_stops_at_first_success() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let observed = Arc::clone(&executor.observed);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.model = Some("primary-model".to_string());
        plan.options.rotation = Some(rotation_policy(vec![
            AgentTaskProviderRotationEntry {
                backend: Some("fallback-backend-a".to_string()),
                selector: Some("fallback-a.agent-task-executor".to_string()),
                model: Some("fallback-model-a".to_string()),
                provider_config: json!({ "provider": "fallback-provider-a" }),
                adoption: None,
            },
            entry("fallback-backend-b"),
        ]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 1);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed[0].executor.backend, "test");
        assert_eq!(observed[1].executor.backend, "fallback-backend-a");
        assert_eq!(
            observed[1].executor.selector.as_deref(),
            Some("fallback-a.agent-task-executor")
        );
        assert_eq!(
            observed[1].executor.model.as_deref(),
            Some("fallback-model-a")
        );
        assert_eq!(
            observed[1]
                .executor
                .config
                .get("provider")
                .and_then(Value::as_str),
            Some("fallback-provider-a")
        );

        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["attempt"], 1);
        assert_eq!(attempts[0]["backend"], "test");
        assert_eq!(attempts[0]["requested_model"], "primary-model");
        assert_eq!(attempts[0]["attempted_model"], "primary-model");
        assert_eq!(attempts[0]["failure_classification"], "provider");
        assert_eq!(attempts[0]["status"], "provider_error");
        assert_eq!(attempts[1]["attempt"], 2);
        assert_eq!(attempts[1]["backend"], "fallback-backend-a");
        assert_eq!(attempts[1]["requested_model"], "primary-model");
        assert_eq!(attempts[1]["attempted_model"], "fallback-model-a");
        assert!(attempts[1].get("candidate_producing_model").is_none());
        assert_eq!(attempts[1]["status"], "succeeded");
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["executions_used"], 2,
            "only dispatched provider attempts consume the execution budget"
        );
        assert!(aggregate.events.iter().any(|event| {
            event.message.as_deref()
                == Some("provider rotation queued: entry 1 of 2; backend=fallback-backend-a, model=fallback-model-a")
        }));
    }

    #[test]
    fn same_backend_model_rotation_announces_and_records_the_producing_model() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.backend = "opencode".to_string();
        plan.tasks[0].executor.model = Some("openai/gpt-5.6-sol".to_string());
        plan.options.rotation = Some(rotation_policy(vec![AgentTaskProviderRotationEntry {
            backend: Some("opencode".to_string()),
            model: Some("openai/gpt-5.6-terra".to_string()),
            ..Default::default()
        }]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts[1]["backend"], "opencode");
        assert_eq!(attempts[1]["requested_model"], "openai/gpt-5.6-sol");
        assert_eq!(attempts[1]["attempted_model"], "openai/gpt-5.6-terra");
        assert!(attempts[1].get("candidate_producing_model").is_none());
        assert!(aggregate.events.iter().any(|event| {
            event.message.as_deref()
                == Some("provider rotation queued: entry 1 of 1; backend=opencode, model=openai/gpt-5.6-terra")
        }));
    }

    #[test]
    fn rotation_records_a_provider_reported_model_that_differs_from_the_attempted_model() {
        let scheduler = AgentTaskScheduler::new(Arc::new(ProviderReportedRotationExecutor {
            calls: AtomicUsize::new(0),
        }));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.model = Some("openai/gpt-5.6-sol".to_string());
        plan.options.rotation = Some(rotation_policy(vec![AgentTaskProviderRotationEntry {
            backend: Some("opencode".to_string()),
            model: Some("openai/gpt-5.6-terra".to_string()),
            ..Default::default()
        }]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        let attempt = &aggregate.outcomes[0].metadata["provider_rotation"]["attempts"][1];
        assert_eq!(attempt["attempted_model"], "openai/gpt-5.6-terra");
        assert_eq!(
            attempt["candidate_producing_model"],
            "openai/gpt-5.6-actual"
        );
    }

    #[test]
    fn rotation_preserves_uncommitted_candidate_and_dispatches_next_provider_from_clean_baseline() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        let observed_roots = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(Arc::new(DirtyCandidateThenSuccessExecutor {
            observed_roots: Arc::clone(&observed_roots),
            calls: AtomicUsize::new(0),
        }))
        .with_run_id("cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874");
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].executor.model = Some("primary-model".to_string());
        plan.tasks[0].executor.config = json!({
            "workspace_root": workspace.display().to_string(),
            "cwd": workspace.display().to_string(),
            "nested": { "workspace_root": workspace.display().to_string() },
        });
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        // Scheduling against a run id needs that run to exist durably:
        // `reserve_provider_execution` reads the record before it will let a
        // provider execute, so without this every task fails admission with
        // "agent-task run record not found" instead of rotating.
        crate::agent_tasks::lifecycle::submit_plan(
            &plan,
            Some("cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874"),
        )
        .expect("submit plan");
        crate::agent_tasks::lifecycle::mark_running(
            "cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874",
        )
        .expect("mark running");

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        assert!(
            !workspace.join("candidate.txt").exists(),
            "the managed task worktree must remain untouched"
        );
        let roots = observed_roots.lock().expect("observed roots");
        assert_eq!(roots.len(), 2);
        assert_ne!(roots[0], workspace);
        assert_ne!(roots[1], workspace);
        assert_ne!(roots[0], roots[1]);
        let candidate = aggregate.outcomes[0]
            .artifacts
            .iter()
            .find(|artifact| artifact.id == "task-1-attempt-1-uncommitted-changes")
            .expect("failed attempt patch candidate is retained for promotion");
        assert_eq!(candidate.kind, "patch");
        assert_eq!(candidate.metadata["producer_attempt"], 1);
        assert_eq!(candidate.metadata["provider_rotation_index"], 0);
        assert_eq!(candidate.metadata["provider_backend"], "test");
        assert_eq!(candidate.metadata["provider_model"], "primary-model");
        assert!(candidate
            .sha256
            .as_deref()
            .is_some_and(|sha256| sha256.len() == 64));
        assert_eq!(
            candidate.metadata["run_id"],
            "cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874"
        );
        assert_eq!(candidate.metadata["task_id"], "task-1");
        let patch = fs::read_to_string(candidate.path.as_deref().expect("candidate path"))
            .expect("candidate patch remains available");
        assert!(patch.contains("diff --git a/candidate.txt b/candidate.txt"));
        assert!(candidate
            .path
            .as_deref()
            .is_some_and(|path| path.contains("agent-task/attempt-patches/cook-detached-37abbb52-d638-495c-b270-46fdc965fc9c-attempt-1-fb890874/task-1")));
        // The first attempt left an uncommitted candidate (captured above as a
        // promoted patch artifact), so scheduler cleanup retains its checkout for
        // lifecycle cleanup rather than force-removing it (#8579). The second,
        // cleanly-succeeding attempt holds no work and its checkout is retired.
        assert!(
            roots[0].exists(),
            "attempt with a retained uncommitted candidate keeps its checkout for lifecycle cleanup"
        );
        assert!(
            !roots[1].exists(),
            "clean succeeding attempt checkout is retired after its executor thread stops"
        );
    }

    #[test]
    fn timeout_candidate_remains_recoverable_after_failed_rotation() {
        retained_timeout_candidate_survives_terminal_rotation(
            AgentTaskOutcomeStatus::ProviderError,
            Value::Null,
            "candidate_recoverable",
        );
    }

    #[test]
    fn timeout_candidate_remains_recoverable_after_empty_rotation() {
        retained_timeout_candidate_survives_terminal_rotation(
            AgentTaskOutcomeStatus::Succeeded,
            json!({
                "provider_run_result": {
                    "completed": false,
                    "reply": "",
                    "messages": [],
                    "tool_calls": []
                }
            }),
            "candidate_recoverable",
        );
    }

    fn retained_timeout_candidate_survives_terminal_rotation(
        terminal: AgentTaskOutcomeStatus,
        terminal_outputs: Value,
        expected_terminal_status: &str,
    ) {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let run_id = "timeout-candidate-terminal-rotation";
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        assert!(Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/repo.git",
            ])
            .current_dir(&workspace)
            .status()
            .expect("configure repository identity")
            .success());
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit run");
        let aggregate = AgentTaskScheduler::new(Arc::new(DirtyCandidateThenTerminalExecutor {
            calls: AtomicUsize::new(0),
            terminal,
            terminal_outputs,
        }))
        .with_run_id(run_id)
        .run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::PartialRecoverable,
            "the retained timeout candidate must remain eligible for controller promotion: {aggregate:#?}"
        );
        assert_eq!(aggregate.totals.candidate_recoverable, 1);
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.status, AgentTaskOutcomeStatus::CandidateRecoverable);
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::Provider)
        );
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.metadata["producer_attempt"] == 1
                && artifact.metadata["provider_rotation_index"] == 0
        }));
        let attempts = outcome.metadata["provider_rotation"]["attempts"]
            .as_array()
            .expect("rotation evidence");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["status"], "timeout");
        assert_eq!(attempts[1]["status"], expected_terminal_status);
    }

    #[test]
    fn rotation_adopts_only_the_explicit_portable_candidate() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        assert!(Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/repo.git"
            ])
            .current_dir(&workspace)
            .status()
            .expect("remote")
            .success());
        let patch = "diff --git a/candidate.txt b/candidate.txt\nnew file mode 100644\n--- /dev/null\n+++ b/candidate.txt\n@@ -0,0 +1 @@\n+candidate\n".to_string();
        let scheduler = AdoptCandidateThenSuccessExecutor {
            calls: AtomicUsize::new(0),
            patch,
        };
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.options.rotation = Some(rotation_policy(vec![AgentTaskProviderRotationEntry {
            backend: Some("provider-b".to_string()),
            adoption: Some(AgentTaskCandidateAdoption {
                source_run_id: String::new(),
                source_task_id: String::new(),
                source_attempt: 0,
                provider_backend: String::new(),
                provider_selector: None,
                provider_model: None,
                task_base_sha: String::new(),
                repository_identity: String::new(),
                workspace_identity: String::new(),
                artifact_id: String::new(),
                sha256: String::new(),
                decision: AgentTaskCandidateAdoptionDecision::AdoptPreviousCandidate,
                content: None,
            }),
            ..Default::default()
        }]));
        enable_rotation(&mut plan);

        // Recording a provider execution is durable, so the run it belongs to
        // has to exist before the scheduler dispatches it.
        crate::agent_task_lifecycle::submit_plan(&plan, Some("run-adopt"))
            .expect("durable run record");
        let aggregate = AgentTaskScheduler::new(Arc::new(scheduler))
            .with_run_id("run-adopt")
            .run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        assert_eq!(
            aggregate.outcomes[0].metadata["candidate_adoption"]["artifact_id"],
            "candidate"
        );
        assert!(aggregate.outcomes[0].artifacts.iter().any(|artifact| {
            artifact.id == "candidate"
                && artifact
                    .url
                    .as_deref()
                    .is_some_and(|url| url.contains("/artifacts#task=task-1"))
        }));
    }

    #[test]
    fn explicit_candidate_adoption_materializes_verified_patch_with_provenance() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        let candidate_file = workspace.join("adopted.txt");
        fs::write(&candidate_file, "candidate\n").expect("candidate change");
        git_output(&workspace, &["add", "adopted.txt"]).expect("stage candidate");
        let patch = git_output_raw(
            &workspace,
            &[
                "diff",
                "--cached",
                "--binary",
                "--full-index",
                "--",
                "adopted.txt",
            ],
        )
        .expect("candidate patch");
        git_output(&workspace, &["reset", "--hard", "HEAD"]).expect("restore clean task base");
        let patch_path = temp.path().join("candidate.patch");
        fs::write(&patch_path, &patch).expect("persist candidate patch");
        let expected_fingerprint = fingerprint(patch.as_bytes());
        let observed = Arc::new(Mutex::new(None));
        let scheduler = AgentTaskScheduler::new(Arc::new(AdoptionExecutor {
            observed: Arc::clone(&observed),
        }));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].workspace.attempt = Some(crate::agent_task::AgentTaskAttemptWorkspace {
            identity: "adoption-request".to_string(),
            base_ref: "ignored-before-materialization".to_string(),
            base_fingerprint: "ignored-before-materialization".to_string(),
            adoption: Some(crate::agent_task::AgentTaskCandidateAdoption {
                source_attempt: "attempt-provider-a".to_string(),
                patch_path: patch_path.display().to_string(),
                patch_fingerprint: expected_fingerprint.clone(),
                provider_backend: "provider-a".to_string(),
                provider_model: Some("model-a".to_string()),
                decision: "continue verified candidate".to_string(),
            }),
        });

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        let attempt = observed
            .lock()
            .expect("observed workspace")
            .clone()
            .expect("attempt");
        assert_ne!(attempt.identity, "adoption-request");
        assert!(attempt.base_fingerprint.starts_with("sha256:"));
        let adoption = attempt.adoption.expect("adoption provenance");
        assert_eq!(adoption.source_attempt, "attempt-provider-a");
        assert_eq!(adoption.patch_fingerprint, expected_fingerprint);
        assert_eq!(adoption.provider_backend, "provider-a");
        assert_eq!(adoption.provider_model.as_deref(), Some("model-a"));
    }

    #[test]
    fn primary_success_does_not_rotate() {
        let executor = RotationScriptedExecutor::new(vec![success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation")
            .is_none());
    }

    #[test]
    fn initial_attempt_applies_the_first_rotation_entry_model() {
        // #9013: with a configured `rotation.entries` default and no explicit
        // --model, the first entry describes the initial attempt. Its model must
        // be applied to the very first request so it is persisted before
        // execution — otherwise the cook runs with a null model and fails
        // finalization after publishing a PR.
        let executor = RotationScriptedExecutor::new(vec![success()]);
        let observed = Arc::clone(&executor.observed);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        assert!(
            plan.tasks[0].executor.model.is_none(),
            "fixture starts with no explicit model"
        );
        plan.options.rotation = Some(rotation_policy(vec![AgentTaskProviderRotationEntry {
            backend: None,
            selector: None,
            model: Some("configured-default-model".to_string()),
            provider_config: json!({}),
            adoption: None,
        }]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(
            observed[0].executor.model.as_deref(),
            Some("configured-default-model"),
            "the initial attempt must carry the configured rotation-entry model"
        );
    }

    #[test]
    fn initial_attempt_preserves_an_explicit_model_over_the_first_rotation_entry() {
        // The initial application only fills gaps: an explicit --model already on
        // the request wins over the configured entry default.
        let executor = RotationScriptedExecutor::new(vec![success()]);
        let observed = Arc::clone(&executor.observed);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.model = Some("explicit-cli-model".to_string());
        plan.options.rotation = Some(rotation_policy(vec![AgentTaskProviderRotationEntry {
            backend: None,
            selector: None,
            model: Some("configured-default-model".to_string()),
            provider_config: json!({}),
            adoption: None,
        }]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(
            observed[0].executor.model.as_deref(),
            Some("explicit-cli-model"),
            "an explicit model must not be overridden by the rotation-entry default"
        );
    }

    #[test]
    fn one_provider_execution_budget_never_rotates() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: 1,
            max_same_provider_retries: 0,
            max_provider_rotations: 0,
        };
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(aggregate.events.iter().all(|event| !event
            .message
            .as_deref()
            .is_some_and(|message| message.contains("provider rotation queued"))));
        let diagnostic = aggregate.outcomes[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "agent_task.execution_budget_exhausted")
            .expect("execution budget exhaustion diagnostic");
        assert_eq!(
            diagnostic.data["exhausted_budget"],
            "max_provider_executions"
        );
    }

    #[test]
    fn rotates_on_transient_and_timeout_classifications() {
        for classification in [
            AgentTaskFailureClassification::Transient,
            AgentTaskFailureClassification::Timeout,
            AgentTaskFailureClassification::Stalled,
            AgentTaskFailureClassification::RateLimited,
        ] {
            let status = if classification == AgentTaskFailureClassification::Timeout {
                AgentTaskOutcomeStatus::Timeout
            } else {
                AgentTaskOutcomeStatus::ProviderError
            };
            let executor =
                RotationScriptedExecutor::new(vec![(status, Some(classification)), success()]);
            let calls = Arc::clone(&executor.calls);
            let scheduler = AgentTaskScheduler::new(Arc::new(executor));
            let mut plan = plan_with_tasks(1);
            plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
            enable_rotation(&mut plan);

            let aggregate = scheduler.run(plan);

            assert_eq!(
                aggregate.status,
                AgentTaskAggregateStatus::Succeeded,
                "classification {classification:?} should rotate"
            );
            assert_eq!(calls.load(Ordering::SeqCst), 2);
        }
    }

    #[test]
    fn timed_out_attempt_rotates_after_recovering_a_malformed_scratch_index() {
        struct TimeoutThenSuccessExecutor {
            calls: AtomicUsize,
            cancellation: std::sync::mpsc::Sender<()>,
            cancel_calls: Arc<AtomicUsize>,
            cancellation_receiver: Mutex<std::sync::mpsc::Receiver<()>>,
            scratch_index: std::path::PathBuf,
            scratch_roots: Arc<Mutex<Vec<String>>>,
        }

        impl AgentTaskExecutorAdapter for TimeoutThenSuccessExecutor {
            fn execute(
                &self,
                request: AgentTaskRequest,
                _context: AgentTaskExecutionContext,
            ) -> AgentTaskOutcome {
                self.scratch_roots.lock().expect("scratch roots").push(
                    request.executor.config["runtime_env"]["TMPDIR"]
                        .as_str()
                        .expect("scheduler scratch root")
                        .to_string(),
                );
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    fs::write(&self.scratch_index, "{ stale").expect("corrupt stale index");
                    self.cancellation_receiver
                        .lock()
                        .expect("cancellation receiver")
                        .recv_timeout(Duration::from_secs(5))
                        .expect("scheduler cancelled the timed-out first attempt");
                    return outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded);
                }
                outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
            }

            fn cancel(&self, _task_id: &str) {
                self.cancel_calls.fetch_add(1, Ordering::SeqCst);
                let _ = self.cancellation.send(());
            }
        }

        let _home = homeboy_core::test_support::HomeGuard::new();
        let run_id = "timeout-rotation-scratch-recovery";
        let scratch_index = homeboy_core::paths::homeboy_data()
            .expect("homeboy data")
            .join("controller-scratch/test-indexes")
            .join(run_id)
            .join("resources.json");
        let scratch_roots = Arc::new(Mutex::new(Vec::new()));
        let (cancellation, cancellation_receiver) = std::sync::mpsc::channel();
        let cancel_calls = Arc::new(AtomicUsize::new(0));
        let scheduler = AgentTaskScheduler::new(Arc::new(TimeoutThenSuccessExecutor {
            calls: AtomicUsize::new(0),
            cancellation,
            cancel_calls: Arc::clone(&cancel_calls),
            cancellation_receiver: Mutex::new(cancellation_receiver),
            scratch_index: scratch_index.clone(),
            scratch_roots: Arc::clone(&scratch_roots),
        }))
        .with_run_id(run_id);
        let mut plan = plan_with_tasks(1);
        // The first worker returns only after the scheduler reaches the normal
        // timeout cancellation path, so test scheduling cannot decide whether
        // this is a timeout or a successful attempt.
        plan.tasks[0].limits.timeout_ms = Some(500);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("durable run record");

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(cancel_calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            aggregate.outcomes[0].metadata["provider_rotation"]["attempts"][0]["status"],
            "timeout"
        );
        assert_eq!(
            aggregate.outcomes[0].metadata["provider_rotation"]["attempts"][1]["status"],
            "succeeded"
        );
        let scratch_roots = scratch_roots.lock().expect("scratch roots").clone();
        assert_eq!(scratch_roots.len(), 2);
        assert_ne!(
            scratch_roots[0], scratch_roots[1],
            "the rotated provider receives a new scratch root"
        );
        serde_json::from_str::<Value>(&fs::read_to_string(scratch_index).expect("scratch index"))
            .expect("malformed scratch index recovered");
        assert!(aggregate.events.iter().any(|event| {
            event.message.as_deref()
                == Some("provider rotation queued: entry 1 of 1; backend=fallback-backend-a, model=not recorded")
        }));
    }

    #[test]
    fn does_not_rotate_on_task_level_failure_classifications() {
        for classification in [
            AgentTaskFailureClassification::ExecutionFailed,
            AgentTaskFailureClassification::PolicyDenied,
            AgentTaskFailureClassification::InvalidInput,
            AgentTaskFailureClassification::CapabilityMissing,
        ] {
            let executor = RotationScriptedExecutor::new(vec![
                (AgentTaskOutcomeStatus::Failed, Some(classification)),
                success(),
            ]);
            let calls = Arc::clone(&executor.calls);
            let scheduler = AgentTaskScheduler::new(Arc::new(executor));
            let mut plan = plan_with_tasks(1);
            plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
            enable_rotation(&mut plan);

            let aggregate = scheduler.run(plan);

            assert_eq!(
                aggregate.status,
                AgentTaskAggregateStatus::Failed,
                "classification {classification:?} must not rotate"
            );
            assert_eq!(
                calls.load(Ordering::SeqCst),
                1,
                "classification {classification:?} must not re-dispatch"
            );
            assert_eq!(
                aggregate.outcomes[0].failure_classification,
                Some(classification)
            );
            assert!(aggregate.outcomes[0]
                .metadata
                .pointer("/provider_rotation")
                .is_none());
        }
    }

    #[test]
    fn rotation_exhausts_entries_and_records_attempt_sequence_in_order() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure()]);
        let observed = Arc::clone(&executor.observed);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![
            entry("fallback-backend-a"),
            entry("fallback-backend-b"),
        ]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed.len(), 3);
        assert_eq!(observed[1].executor.backend, "fallback-backend-a");
        assert_eq!(observed[2].executor.backend, "fallback-backend-b");
        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts.len(), 3);
        assert_eq!(
            attempts
                .iter()
                .map(|attempt| attempt["rotation_index"].as_u64().expect("rotation index"))
                .collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert!(attempts
            .iter()
            .all(|attempt| attempt["failure_classification"] == "provider"));
    }

    #[test]
    fn rotation_respects_configured_max_attempts_bound() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                entry("fallback-backend-a"),
                entry("fallback-backend-b"),
                entry("fallback-backend-c"),
            ],
            max_attempts: Some(2),
            ..AgentTaskProviderRotationPolicy::default()
        });
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts.len(), 2);
    }

    #[test]
    fn request_metadata_rotation_overrides_plan_policy() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let observed = Arc::clone(&executor.observed);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("plan-fallback")]));
        enable_rotation(&mut plan);
        plan.tasks[0].metadata = json!({
            "provider_rotation": {
                "entries": [{ "backend": "request-fallback" }]
            }
        });

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed[1].executor.backend, "request-fallback");
    }

    #[test]
    fn no_rotation_policy_keeps_single_attempt_behavior_unchanged() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget::new(1, 0, 0);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Provider)
        );
        assert!(aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation")
            .is_none());
        assert!(!aggregate.events.iter().any(|event| event
            .message
            .as_deref()
            .is_some_and(|message| { message.contains("provider rotation") })));
    }
}
