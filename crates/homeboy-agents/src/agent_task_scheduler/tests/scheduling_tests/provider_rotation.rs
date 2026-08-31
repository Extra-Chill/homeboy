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

    struct ExternalEvidenceRetryExecutor {
        observed: Arc<Mutex<Vec<(String, String)>>>,
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

    struct ReadinessRoutingExecutor {
        observed: Arc<Mutex<Vec<AgentTaskRequest>>>,
        reset_at: chrono::DateTime<chrono::Utc>,
    }

    impl AgentTaskExecutorAdapter for ReadinessRoutingExecutor {
        fn provider_route_readiness(&self, request: &AgentTaskRequest) -> ProviderRouteReadiness {
            match request.executor.model() {
                Some("xai/grok-4.6") => ProviderRouteReadiness {
                    ready: false,
                    state: "provider_account_blocked".to_string(),
                    reason: "account is blocked".to_string(),
                    reset_at: None,
                    classification: Some("account".to_string()),
                    retryable: true,
                    remediation: Some("switch account".to_string()),
                    cache_identity: Some("account-a".to_string()),
                    provider_identity: Some("provider-a".to_string()),
                    capacity_key: Some("account-a".to_string()),
                    diagnostic_data: None,
                },
                Some("zai-coding-plan/glm-5.3" | "opencode-go/kimi-k3") => ProviderRouteReadiness {
                    ready: false,
                    state: "usage_capped".to_string(),
                    reason: "five-hour usage cap is active".to_string(),
                    reset_at: Some(self.reset_at),
                    classification: Some("capacity".to_string()),
                    retryable: true,
                    remediation: Some("wait for reset".to_string()),
                    cache_identity: Some("account-b".to_string()),
                    provider_identity: Some("provider-b".to_string()),
                    capacity_key: Some("account-b".to_string()),
                    diagnostic_data: None,
                },
                _ => ProviderRouteReadiness::dispatchable(),
            }
        }

        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.observed
                .lock()
                .expect("observed requests")
                .push(request.clone());
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
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

    impl AgentTaskExecutorAdapter for ExternalEvidenceRetryExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            let evidence = request.executor.config["evidence_inputs"][0]["path"]
                .as_str()
                .expect("evidence path")
                .to_string();
            assert_eq!(
                std::fs::read_to_string(&evidence).expect("read evidence"),
                "evidence\n"
            );
            let workspace = request
                .workspace
                .root
                .as_deref()
                .expect("attempt workspace");
            assert!(!std::path::Path::new(&evidence).starts_with(workspace));
            let mut observed = self.observed.lock().expect("observed attempts");
            observed.push((evidence, workspace.to_string()));
            let mut result = outcome(
                request.task_id,
                if observed.len() == 1 {
                    AgentTaskOutcomeStatus::ProviderError
                } else {
                    AgentTaskOutcomeStatus::Succeeded
                },
            );
            if observed.len() == 1 {
                result.failure_classification = Some(AgentTaskFailureClassification::Provider);
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
        plan.tasks[0].metadata = json!({
            "resolved_runtime_identity": { "provider_id": "primary.provider" }
        });
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
        assert!(
            observed[1]
                .metadata
                .get("resolved_runtime_identity")
                .is_none(),
            "a backend-changing fallback must resolve its own runtime identity"
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
    fn external_provider_evidence_survives_start_and_retry_without_dirtying_candidate() {
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        let evidence = temp.path().join("provider-evidence/blobs/fixture");
        std::fs::create_dir_all(evidence.parent().expect("evidence parent"))
            .expect("evidence store");
        std::fs::write(&evidence, "evidence\n").expect("evidence blob");
        let observed = Arc::new(Mutex::new(Vec::new()));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].executor.config = json!({
            "evidence_inputs": [{
                "path": evidence,
                "ownership": {"owner": "controller-artifact-store"}
            }]
        });
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        enable_rotation(&mut plan);

        let aggregate = AgentTaskScheduler::new(Arc::new(ExternalEvidenceRetryExecutor {
            observed: Arc::clone(&observed),
        }))
        .run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let observed = observed.lock().expect("observed attempts");
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].0, observed[1].0, "retry reuses one blob");
        assert_ne!(observed[0].1, observed[1].1, "retry owns a fresh attempt");
        let status = Command::new("git")
            .args(["status", "--porcelain=v1"])
            .current_dir(&workspace)
            .output()
            .expect("candidate status");
        assert!(status.status.success());
        assert!(status.stdout.is_empty(), "candidate remains clean");
        assert!(
            evidence.is_file(),
            "attempt cleanup does not own controller blob"
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
            max_provider_rotations: 3,
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
    fn account_block_rotates_to_the_next_configured_entry_without_retrying_it() {
        let executor = RotationScriptedExecutor::new(vec![
            (
                AgentTaskOutcomeStatus::ProviderError,
                Some(AgentTaskFailureClassification::ProviderAccountBlocked),
            ),
            success(),
        ]);
        let observed = Arc::clone(&executor.observed);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.backend = "opencode".to_string();
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("xai/grok-4.6".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("zai-coding-plan/glm-5.3".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("opencode-go/kimi-k3".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("anthropic/claude-sonnet-5".to_string()),
                    ..Default::default()
                },
            ],
            max_attempts: Some(4),
            ..Default::default()
        });
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: 4,
            max_same_provider_retries: 1,
            max_provider_rotations: 3,
        };

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(
            observed
                .iter()
                .map(|request| request.executor.model.as_deref())
                .collect::<Vec<_>>(),
            vec![Some("xai/grok-4.6"), Some("zai-coding-plan/glm-5.3")]
        );
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["same_provider_retries_used"], 0,
            "an explicitly non-retryable account rejection must rotate immediately"
        );
    }

    #[test]
    fn readiness_skips_blocked_and_capped_routes_before_the_first_provider_execution() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(Arc::new(ReadinessRoutingExecutor {
            observed: Arc::clone(&observed),
            reset_at: chrono::Utc::now() + chrono::Duration::hours(2),
        }));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.backend = "opencode".to_string();
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("xai/grok-4.6".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("zai-coding-plan/glm-5.3".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("opencode-go/kimi-k3".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    backend: Some("opencode".to_string()),
                    model: Some("openai/gpt-5.6-terra".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: 1,
            max_same_provider_retries: 0,
            max_provider_rotations: 3,
        };

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed.len(), 1, "only a dispatchable route executes");
        assert_eq!(observed[0].executor.model(), Some("openai/gpt-5.6-terra"));
        let skipped = aggregate.outcomes[0].metadata["provider_readiness_routing"]["skipped"]
            .as_array()
            .expect("ordered readiness skips");
        assert_eq!(skipped.len(), 3);
        assert_eq!(skipped[0]["model"], "xai/grok-4.6");
        assert_eq!(skipped[0]["state"], "provider_account_blocked");
        assert_eq!(skipped[1]["model"], "zai-coding-plan/glm-5.3");
        assert_eq!(skipped[2]["model"], "opencode-go/kimi-k3");
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["executions_used"],
            1
        );
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["provider_rotations_used"], 0,
            "readiness routing spends neither executions nor paid rotations"
        );
        assert!(aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .is_none());
    }

    #[test]
    fn readiness_exhaustion_records_zero_dispatch_budget_and_route_evidence() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(Arc::new(ReadinessRoutingExecutor {
            observed: Arc::clone(&observed),
            reset_at: chrono::Utc::now() + chrono::Duration::hours(2),
        }));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.backend = "opencode".to_string();
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![
                AgentTaskProviderRotationEntry {
                    model: Some("xai/grok-4.6".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("zai-coding-plan/glm-5.3".to_string()),
                    ..Default::default()
                },
                AgentTaskProviderRotationEntry {
                    model: Some("opencode-go/kimi-k3".to_string()),
                    ..Default::default()
                },
            ],
            ..Default::default()
        });
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: 2,
            max_same_provider_retries: 0,
            max_provider_rotations: 2,
        };
        let aggregate = scheduler.run(plan);
        assert!(observed.lock().expect("observed requests").is_empty());
        assert!(aggregate
            .events
            .iter()
            .all(|event| event.state != AgentTaskState::Running));
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.metadata["execution_budget"]["executions_used"], 0);
        assert_eq!(
            outcome.metadata["execution_budget"]["remaining_provider_executions"],
            2
        );
        assert_eq!(
            outcome.metadata["execution_budget"]["provider_rotations_used"],
            0
        );
        assert!(outcome.metadata["execution_budget"]["exhausted"].is_null());
        let skipped = outcome.metadata["provider_readiness_routing"]["skipped"]
            .as_array()
            .expect("readiness evidence");
        assert_eq!(skipped.len(), 3);
        assert_eq!(skipped[0]["classification"], "account");
        assert_eq!(skipped[0]["remediation"], "switch account");
        assert_eq!(skipped[0]["cache_identity"], "account-a");
        assert_eq!(
            outcome.failure_classification,
            Some(AgentTaskFailureClassification::RateLimited)
        );
        assert_eq!(
            outcome.metadata["provider_readiness_exhaustion"]["retryable"],
            true
        );
    }

    #[test]
    fn readiness_exhaustion_after_rotation_preserves_prior_execution_evidence() {
        struct UnreadyAfterFirstExecution {
            calls: AtomicUsize,
        }

        impl AgentTaskExecutorAdapter for UnreadyAfterFirstExecution {
            fn provider_route_readiness(
                &self,
                _request: &AgentTaskRequest,
            ) -> ProviderRouteReadiness {
                if self.calls.load(Ordering::SeqCst) == 0 {
                    ProviderRouteReadiness::dispatchable()
                } else {
                    ProviderRouteReadiness {
                        ready: false,
                        state: "usage_capped".to_string(),
                        reason: "fallback is capped".to_string(),
                        reset_at: None,
                        classification: Some("capacity".to_string()),
                        retryable: true,
                        remediation: Some("wait".to_string()),
                        cache_identity: Some("fallback-account".to_string()),
                        provider_identity: Some("fallback-provider".to_string()),
                        capacity_key: Some("fallback-account".to_string()),
                        diagnostic_data: None,
                    }
                }
            }

            fn execute(
                &self,
                request: AgentTaskRequest,
                _context: AgentTaskExecutionContext,
            ) -> AgentTaskOutcome {
                self.calls.fetch_add(1, Ordering::SeqCst);
                AgentTaskOutcome {
                    task_id: request.task_id,
                    status: AgentTaskOutcomeStatus::ProviderError,
                    failure_classification: Some(
                        AgentTaskFailureClassification::ProviderAccountBlocked,
                    ),
                    ..Default::default()
                }
            }
        }

        let scheduler = AgentTaskScheduler::new(Arc::new(UnreadyAfterFirstExecution {
            calls: AtomicUsize::new(0),
        }));
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend")]));
        enable_rotation(&mut plan);

        let aggregate = scheduler.run(plan);
        let outcome = &aggregate.outcomes[0];
        assert_eq!(outcome.metadata["execution_budget"]["executions_used"], 1);
        assert_eq!(
            outcome.metadata["provider_rotation"]["attempts"]
                .as_array()
                .expect("prior rotation attempts")
                .len(),
            1
        );
    }

    #[test]
    fn unresolved_primary_uses_ready_fallback_with_one_execution_and_zero_paid_rotations() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(Arc::new(ReadinessRoutingExecutor {
            observed: Arc::clone(&observed),
            reset_at: chrono::Utc::now() + chrono::Duration::hours(1),
        }));
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.backend = "opencode".to_string();
        plan.tasks[0].executor.model = Some("xai/grok-4.6".to_string());
        plan.options.rotation = Some(rotation_policy(vec![AgentTaskProviderRotationEntry {
            model: Some("anthropic/claude-sonnet-5".to_string()),
            ..Default::default()
        }]));
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
            deadline_unix_ms: None,
            max_provider_executions: 1,
            max_same_provider_retries: 0,
            max_provider_rotations: 1,
        };

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let observed = observed.lock().expect("observed requests");
        assert_eq!(observed.len(), 1);
        assert_eq!(
            observed[0].executor.model(),
            Some("anthropic/claude-sonnet-5")
        );
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["executions_used"],
            1
        );
        assert_eq!(
            aggregate.outcomes[0].metadata["execution_budget"]["provider_rotations_used"],
            0
        );
    }

    #[test]
    fn admission_selected_fallback_failure_rotates_only_forward() {
        struct FailSelectedFallback {
            observed: Arc<Mutex<Vec<String>>>,
        }

        impl AgentTaskExecutorAdapter for FailSelectedFallback {
            fn provider_route_readiness(
                &self,
                request: &AgentTaskRequest,
            ) -> ProviderRouteReadiness {
                if request.executor.model() != Some("primary") {
                    return ProviderRouteReadiness::dispatchable();
                }
                ProviderRouteReadiness {
                    ready: false,
                    state: "provider_account_blocked".to_string(),
                    reason: "primary rejected".to_string(),
                    reset_at: None,
                    classification: Some("account".to_string()),
                    retryable: false,
                    remediation: Some("use fallback".to_string()),
                    cache_identity: None,
                    provider_identity: None,
                    capacity_key: None,
                    diagnostic_data: None,
                }
            }

            fn execute(
                &self,
                request: AgentTaskRequest,
                _context: AgentTaskExecutionContext,
            ) -> AgentTaskOutcome {
                let model = request.executor.model().unwrap_or_default().to_string();
                self.observed
                    .lock()
                    .expect("observed models")
                    .push(model.clone());
                let mut result = outcome(
                    request.task_id,
                    if model == "fallback" {
                        AgentTaskOutcomeStatus::ProviderError
                    } else {
                        AgentTaskOutcomeStatus::Succeeded
                    },
                );
                if model == "fallback" {
                    result.failure_classification = Some(AgentTaskFailureClassification::Provider);
                }
                result
            }
        }

        let root = tempfile::tempdir().expect("tempdir");
        let script = root.path().join("readiness.js");
        std::fs::write(
            &script,
            "const fs=require('fs');const r=JSON.parse(fs.readFileSync(0,'utf8'));const model=r.effective_config.model;process.stdout.write(JSON.stringify({schema:'homeboy/agent-task-provider-readiness-result/v1',ready:model!=='primary',classification:model==='primary'?'account':'ready',retryable:false,remediation:'switch',reason:model==='primary'?'blocked':'',cache_key:model,identity:{model}}));",
        )
        .expect("readiness script");
        let mut provider: crate::agent_task_provider::AgentTaskExecutorProvider =
            serde_json::from_value(json!({
                "id": "test.provider",
                "backend": "test",
            }))
            .expect("provider");
        provider.readiness_invocation = Some(homeboy_core::command_invocation::CommandInvocation {
            argv: vec!["node".to_string(), script.display().to_string()],
            ..Default::default()
        });
        let catalog = crate::agent_task_provider::AgentTaskProviderCatalog {
            providers: vec![provider],
            diagnostics: Vec::new(),
            version: None,
        };
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].executor.model = Some("primary".to_string());
        plan.options.rotation = Some(rotation_policy(vec![
            AgentTaskProviderRotationEntry {
                model: Some("primary".to_string()),
                ..Default::default()
            },
            AgentTaskProviderRotationEntry {
                model: Some("fallback".to_string()),
                ..Default::default()
            },
            AgentTaskProviderRotationEntry {
                model: Some("forward".to_string()),
                ..Default::default()
            },
        ]));
        enable_rotation(&mut plan);
        let admitted =
            crate::agent_task_provider::admit_plan_provider_dispatchability_with_providers(
                &plan,
                &catalog,
                &mut crate::agent_task_provider::ProviderRuntimeReadinessCache::default(),
            )
            .expect("fallback admission");
        assert_eq!(admitted.tasks[0].executor.model(), Some("fallback"));
        assert_eq!(
            admitted.tasks[0].metadata["provider_readiness_routing"]["next_rotation_index"],
            2
        );

        let route_evidence: Vec<ProviderRouteEvidence> = serde_json::from_value(
            admitted.tasks[0].metadata["provider_readiness_routing"]["skipped"].clone(),
        )
        .expect("typed route evidence");
        let encoded = serde_json::to_string(&route_evidence).expect("route evidence serialization");
        let roundtrip: Vec<ProviderRouteEvidence> =
            serde_json::from_str(&encoded).expect("route evidence roundtrip");
        assert_eq!(roundtrip, route_evidence);
        assert_eq!(
            roundtrip[0]
                .checks
                .as_ref()
                .expect("typed checks")
                .runtime
                .ready,
            false
        );
        assert_eq!(
            roundtrip[0]
                .runtime_evidence
                .as_ref()
                .expect("typed runtime evidence")
                .classification,
            "account"
        );

        let observed = Arc::new(Mutex::new(Vec::new()));
        let aggregate = AgentTaskScheduler::new(Arc::new(FailSelectedFallback {
            observed: Arc::clone(&observed),
        }))
        .run(admitted);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(
            *observed.lock().expect("observed models"),
            vec!["fallback".to_string(), "forward".to_string()]
        );

        let durable_observed = Arc::new(Mutex::new(Vec::new()));
        let durable = AgentTaskScheduler::new(Arc::new(FailSelectedFallback {
            observed: Arc::clone(&durable_observed),
        }))
        .run(plan);
        assert_eq!(durable.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(
            *durable_observed.lock().expect("durable observed models"),
            vec!["fallback".to_string(), "forward".to_string()]
        );
    }

    #[test]
    fn completion_uses_the_capacity_identity_bound_before_concurrent_execution() {
        struct BoundCapacityExecutor {
            completed: Arc<Mutex<Vec<String>>>,
        }

        impl AgentTaskExecutorAdapter for BoundCapacityExecutor {
            fn provider_route_capacity_key(&self, request: &AgentTaskRequest) -> String {
                format!("recomputed-{}", request.task_id)
            }

            fn provider_route_readiness(
                &self,
                request: &AgentTaskRequest,
            ) -> ProviderRouteReadiness {
                let mut readiness = ProviderRouteReadiness::dispatchable();
                readiness.capacity_key = Some(format!("reported-{}", request.task_id));
                readiness
            }

            fn record_provider_outcome(
                &self,
                _request: &AgentTaskRequest,
                capacity_key: &str,
                _outcome: &AgentTaskOutcome,
            ) {
                self.completed
                    .lock()
                    .expect("completed identities")
                    .push(capacity_key.to_string());
            }

            fn execute(
                &self,
                request: AgentTaskRequest,
                _context: AgentTaskExecutionContext,
            ) -> AgentTaskOutcome {
                outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
            }
        }

        let completed = Arc::new(Mutex::new(Vec::new()));
        let mut plan = plan_with_tasks(8);
        plan.options.max_concurrency = 8;
        let aggregate = AgentTaskScheduler::new(Arc::new(BoundCapacityExecutor {
            completed: Arc::clone(&completed),
        }))
        .run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let completed = completed.lock().expect("completed identities");
        assert_eq!(completed.len(), 8);
        assert!(completed.iter().all(|key| key.starts_with("reported-")));
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
            .join("controller-scratch/resources.json");
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
        let diagnostic = aggregate.outcomes[0]
            .diagnostics
            .iter()
            .find(|diagnostic| diagnostic.class == "agent_task.provider_rotation_exhausted")
            .expect("rotation exhaustion diagnostic");
        assert_eq!(
            diagnostic.message,
            "all configured provider routes were rejected: test/default model: provider; fallback-backend-a/default model: provider; fallback-backend-b/default model: provider"
        );
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
