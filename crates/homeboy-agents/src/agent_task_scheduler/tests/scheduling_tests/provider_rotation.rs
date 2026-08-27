//! Scheduler dispatch, concurrency, retry, dependency-binding, matrix, and
//! cancellation behavior.

use super::shared::*;

mod provider_rotation_tests {
    use super::*;

    struct AdoptionExecutor {
        observed: Arc<Mutex<Option<crate::agent_task::AgentTaskAttemptWorkspace>>>,
    }

    struct ProviderReportedRotationExecutor {
        calls: AtomicUsize,
    }

    struct DirtyCandidateThenTerminalExecutor {
        calls: Arc<AtomicUsize>,
        terminal: AgentTaskOutcomeStatus,
        terminal_outputs: Value,
    }

    struct CandidateMissingReviewFormExecutor {
        calls: Arc<AtomicUsize>,
    }

    struct ProviderReportedTimeoutExecutor {
        calls: Arc<AtomicUsize>,
        returns_patch: bool,
    }

    /// Executor that reproduces the #13644 repro shape: the primary backend
    /// ("test") is presently over its usage cap and reports that as a
    /// `RateLimited` outcome carrying the usage-cap diagnostic Homeboy's
    /// provider layer attaches (`annotate_usage_cap`); the rotation fallback
    /// backend is healthy. Records every dispatched `(task_id, backend)` pair
    /// so a test can prove a later task in the same plan skips the
    /// already-known-capped backend entirely instead of spending an attempt
    /// rediscovering it.
    struct UsageCapAwareExecutor {
        observed: Arc<Mutex<Vec<(String, String)>>>,
        reset_at: chrono::DateTime<chrono::Utc>,
    }

    impl AgentTaskExecutorAdapter for UsageCapAwareExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.observed
                .lock()
                .expect("observed requests")
                .push((request.task_id.clone(), request.executor.backend.clone()));
            if request.executor.backend == "test" {
                let mut result = outcome(request.task_id, AgentTaskOutcomeStatus::ProviderError);
                result.failure_classification = Some(AgentTaskFailureClassification::RateLimited);
                result.diagnostics.push(AgentTaskDiagnostic {
                    class:
                        crate::agent_task_provider::AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS
                            .to_string(),
                    message: format!(
                        "provider usage cap reached; resets at {}",
                        self.reset_at.to_rfc3339()
                    ),
                    data: json!({ "reset_at": self.reset_at.to_rfc3339() }),
                });
                return result;
            }
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
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

    impl AgentTaskExecutorAdapter for CandidateMissingReviewFormExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let root = request
                .workspace
                .root
                .as_deref()
                .expect("attempt workspace");
            fs::write(
                std::path::Path::new(root).join("candidate.txt"),
                "candidate\n",
            )
            .expect("candidate edit");
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    impl AgentTaskExecutorAdapter for ProviderReportedTimeoutExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            if self.calls.fetch_add(1, Ordering::SeqCst) > 0 {
                return outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded);
            }

            let mut result = outcome(request.task_id, AgentTaskOutcomeStatus::Timeout);
            result.failure_classification = Some(AgentTaskFailureClassification::Timeout);
            if self.returns_patch {
                let patch = "diff --git a/candidate.txt b/candidate.txt\nnew file mode 100644\n--- /dev/null\n+++ b/candidate.txt\n@@ -0,0 +1 @@\n+candidate\n";
                let path = std::path::Path::new(
                    request
                        .workspace
                        .root
                        .as_deref()
                        .expect("attempt workspace"),
                )
                .join("candidate.patch");
                fs::write(&path, patch).expect("candidate patch");
                result.artifacts.push(AgentTaskArtifact {
                    schema: AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "provider-timeout-patch".to_string(),
                    kind: "patch".to_string(),
                    name: Some("candidate.patch".to_string()),
                    label: None,
                    role: Some("patch".to_string()),
                    semantic_key: None,
                    path: Some(path.display().to_string()),
                    url: None,
                    mime: Some("text/x-patch".to_string()),
                    size_bytes: Some(patch.len() as u64),
                    sha256: Some(homeboy_engine_primitives::content_hash::sha256_hex(
                        patch.as_bytes(),
                    )),
                    metadata: Value::Null,
                });
            }
            result
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
    fn timeout_candidate_converges_before_failed_rotation() {
        retained_timeout_candidate_converges_before_rotation();
    }

    #[test]
    fn timeout_candidate_converges_before_empty_rotation() {
        retained_timeout_candidate_converges_before_rotation();
    }

    #[test]
    fn timeout_candidate_converges_before_another_provider_rotation() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let run_id = "timeout-candidate-convergence";
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
        let calls = Arc::new(AtomicUsize::new(0));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit run");

        let aggregate = AgentTaskScheduler::new(Arc::new(DirtyCandidateThenTerminalExecutor {
            calls: Arc::clone(&calls),
            terminal: AgentTaskOutcomeStatus::Succeeded,
            terminal_outputs: Value::Null,
        }))
        .with_run_id(run_id)
        .run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::PartialRecoverable
        );
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::CandidateRecoverable
        );
        assert!(aggregate.events.iter().all(|event| !event
            .message
            .as_deref()
            .is_some_and(|message| message.contains("provider rotation queued"))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn provider_reported_timeout_with_fingerprinted_patch_converges() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = AgentTaskScheduler::new(Arc::new(ProviderReportedTimeoutExecutor {
            calls: Arc::clone(&calls),
            returns_patch: true,
        }))
        .run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::PartialRecoverable
        );
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::CandidateRecoverable
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(aggregate.outcomes[0].diagnostics.iter().any(|diagnostic| {
            diagnostic.class == "agent_task.provider_timeout_recoverable_candidate"
        }));
        assert!(aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation")
            .is_none());
    }

    #[test]
    fn provider_reported_timeout_without_patch_rotates() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = AgentTaskScheduler::new(Arc::new(ProviderReportedTimeoutExecutor {
            calls: Arc::clone(&calls),
            returns_patch: false,
        }))
        .run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        assert_eq!(
            aggregate.outcomes[0].metadata["provider_rotation"]["attempts"]
                .as_array()
                .map(Vec::len),
            Some(2)
        );
    }

    #[test]
    fn missing_review_form_with_a_valid_patch_converges_without_rotation() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = AgentTaskScheduler::new(Arc::new(CandidateMissingReviewFormExecutor {
            calls: Arc::clone(&calls),
        }))
        .run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::Succeeded
        );
        assert!(aggregate.outcomes[0].outputs["review_form"].is_null());
        assert!(aggregate.events.iter().all(|event| !event
            .message
            .as_deref()
            .is_some_and(|message| message.contains("provider rotation queued"))));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    fn retained_timeout_candidate_converges_before_rotation() {
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
        let calls = Arc::new(AtomicUsize::new(0));
        let aggregate = AgentTaskScheduler::new(Arc::new(DirtyCandidateThenTerminalExecutor {
            calls: Arc::clone(&calls),
            terminal: AgentTaskOutcomeStatus::ProviderError,
            terminal_outputs: Value::Null,
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
            Some(AgentTaskFailureClassification::Timeout)
        );
        assert!(outcome.artifacts.iter().any(|artifact| {
            artifact.kind == "patch"
                && artifact.metadata["producer_attempt"] == 1
                && artifact.metadata["provider_rotation_index"] == 0
        }));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert!(outcome.metadata.pointer("/provider_rotation").is_none());
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

    #[test]
    fn a_later_task_skips_a_provider_usage_cap_learned_earlier_in_the_same_plan() {
        // #13644: a flat-rate provider hitting its 5-hour usage cap is not
        // unhealthy or misconfigured; it is temporarily out of quota until a
        // known reset time. Once one task in a plan learns that, a fanout
        // sibling must not spend its own attempt rediscovering the same cap —
        // it should skip straight to the next rotation entry.
        let observed = Arc::new(Mutex::new(Vec::new()));
        let reset_at = chrono::Utc::now() + chrono::Duration::hours(1);
        let scheduler = AgentTaskScheduler::new(Arc::new(UsageCapAwareExecutor {
            observed: Arc::clone(&observed),
            reset_at,
        }));
        let mut plan = plan_with_tasks(2);
        // Force sequential dispatch so task-2 starts only after task-1's
        // rotation has taught the scheduler about the cap.
        plan.options.max_concurrency = 1;
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(aggregate.totals.succeeded, 2);

        let observed = observed.lock().expect("observed requests");
        let backends_for = |task_id: &str| -> Vec<String> {
            observed
                .iter()
                .filter(|(observed_task_id, _)| observed_task_id == task_id)
                .map(|(_, backend)| backend.clone())
                .collect()
        };
        assert_eq!(
            backends_for("task-1"),
            vec!["test".to_string(), "fallback-backend-a".to_string()],
            "task-1 discovers the cap firsthand and rotates past it"
        );
        assert_eq!(
            backends_for("task-2"),
            vec!["fallback-backend-a".to_string()],
            "task-2 must skip the already-known-capped primary backend entirely, \
             spending no attempt rediscovering it"
        );
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-2"
                && event
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("provider usage cap active"))
                && event
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("backend=test"))
        }));
    }

    #[test]
    fn a_task_fails_fast_when_every_reachable_rotation_entry_is_presently_capped() {
        // #13644: when there is nowhere left to rotate to because every
        // reachable entry is already known to be over its usage cap, Homeboy
        // must fail the task without ever dispatching a provider already known
        // to refuse the request.
        struct AlwaysCappedExecutor {
            calls: Arc<AtomicUsize>,
            reset_at: chrono::DateTime<chrono::Utc>,
            observed: Arc<Mutex<Vec<(String, String)>>>,
        }

        impl AgentTaskExecutorAdapter for AlwaysCappedExecutor {
            fn execute(
                &self,
                request: AgentTaskRequest,
                _context: AgentTaskExecutionContext,
            ) -> AgentTaskOutcome {
                self.calls.fetch_add(1, Ordering::SeqCst);
                self.observed
                    .lock()
                    .expect("observed")
                    .push((request.task_id.clone(), request.executor.backend.clone()));
                let mut result = outcome(request.task_id, AgentTaskOutcomeStatus::ProviderError);
                result.failure_classification = Some(AgentTaskFailureClassification::RateLimited);
                result.diagnostics.push(AgentTaskDiagnostic {
                    class:
                        crate::agent_task_provider::AGENT_TASK_PROVIDER_USAGE_CAP_DIAGNOSTIC_CLASS
                            .to_string(),
                    message: format!(
                        "provider usage cap reached; resets at {}",
                        self.reset_at.to_rfc3339()
                    ),
                    data: json!({ "reset_at": self.reset_at.to_rfc3339() }),
                });
                result
            }
        }

        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::new(Mutex::new(Vec::new()));
        let reset_at = chrono::Utc::now() + chrono::Duration::hours(1);
        let scheduler = AgentTaskScheduler::new(Arc::new(AlwaysCappedExecutor {
            calls: Arc::clone(&calls),
            reset_at,
            observed: Arc::clone(&observed),
        }));
        // Three sequential tasks: between them, the first two genuinely
        // dispatch to (and thereby cap) both the primary backend and the
        // single rotation entry, in whichever order the scheduler's rotation
        // requeueing happens to interleave with. By the third task, both
        // routes are known-capped no matter that interleaving, so its
        // dispatch is unambiguous: it must skip both and never call the
        // executor at all.
        let mut plan = plan_with_tasks(3);
        plan.options.max_concurrency = 1;
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Failed);
        // task-1 and task-2 between them pay to learn both the primary and
        // the fallback are capped; task-3 then has nowhere left to rotate to
        // and must not dispatch at all.
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let observed_for = |task_id: &str| -> usize {
            observed
                .lock()
                .expect("observed requests")
                .iter()
                .filter(|(observed_task_id, _)| observed_task_id == task_id)
                .count()
        };
        assert_eq!(
            observed_for("task-3"),
            0,
            "task-3 must not spend an attempt on a route both prior tasks already learned is capped"
        );
        let task_3_outcome = aggregate
            .outcomes
            .iter()
            .find(|outcome| outcome.task_id == "task-3")
            .expect("task-3 outcome");
        assert!(task_3_outcome
            .diagnostics
            .iter()
            .any(|diagnostic| { diagnostic.class == "agent_task.provider_usage_cap_exhausted" }));
        assert!(aggregate.events.iter().any(|event| {
            event.task_id == "task-3"
                && event
                    .message
                    .as_deref()
                    .is_some_and(|message| message.contains("provider usage cap active"))
        }));
    }
}
