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
        observed: Arc<Mutex<Option<crate::core::agent_task::AgentTaskAttemptWorkspace>>>,
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
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![entry("fallback-backend-a")]));
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
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
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.rotation = Some(rotation_policy(vec![
            AgentTaskProviderRotationEntry {
                backend: Some("fallback-backend-a".to_string()),
                selector: Some("fallback-a.agent-task-executor".to_string()),
                model: Some("fallback-model-a".to_string()),
                provider_config: json!({ "provider": "fallback-provider-a" }),
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
        assert_eq!(attempts[0]["failure_classification"], "provider");
        assert_eq!(attempts[0]["status"], "provider_error");
        assert_eq!(attempts[1]["attempt"], 2);
        assert_eq!(attempts[1]["backend"], "fallback-backend-a");
        assert_eq!(attempts[1]["status"], "succeeded");
        assert!(aggregate.events.iter().any(|event| {
            event.message.as_deref() == Some("provider rotation queued: entry 1 of 2")
        }));
    }

    #[test]
    fn rotation_preserves_uncommitted_candidate_and_dispatches_next_provider_from_clean_baseline() {
        let _home = crate::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        super::super::concurrency::concurrency_tests::init_git_workspace(&workspace);
        let observed_roots = Arc::new(Mutex::new(Vec::new()));
        let scheduler = AgentTaskScheduler::new(DirtyCandidateThenSuccessExecutor {
            observed_roots: Arc::clone(&observed_roots),
            calls: AtomicUsize::new(0),
        })
        .with_run_id("run-8081");
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
        assert_eq!(candidate.metadata["run_id"], "run-8081");
        assert_eq!(candidate.metadata["task_id"], "task-1");
        let patch = fs::read_to_string(candidate.path.as_deref().expect("candidate path"))
            .expect("candidate patch remains available");
        assert!(patch.contains("diff --git a/candidate.txt b/candidate.txt"));
        assert!(candidate
            .path
            .as_deref()
            .is_some_and(|path| path.contains("agent-task/attempt-patches/run-8081/task-1")));
        assert!(
            !roots[0].exists() && !roots[1].exists(),
            "attempt checkouts are retired after their executor threads stop"
        );
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
        let scheduler = AgentTaskScheduler::new(AdoptionExecutor {
            observed: Arc::clone(&observed),
        });
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan.tasks[0].workspace.attempt =
            Some(crate::core::agent_task::AgentTaskAttemptWorkspace {
                identity: "adoption-request".to_string(),
                base_ref: "ignored-before-materialization".to_string(),
                base_fingerprint: "ignored-before-materialization".to_string(),
                adoption: Some(crate::core::agent_task::AgentTaskCandidateAdoption {
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
        let scheduler = AgentTaskScheduler::new(executor);
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
    fn one_provider_execution_budget_never_rotates() {
        let executor = RotationScriptedExecutor::new(vec![provider_failure(), success()]);
        let calls = Arc::clone(&executor.calls);
        let scheduler = AgentTaskScheduler::new(executor);
        let mut plan = plan_with_tasks(1);
        plan.options.execution_budget = AgentTaskExecutionBudget {
            version: AgentTaskExecutionBudget::VERSION,
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
            let scheduler = AgentTaskScheduler::new(executor);
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
            let scheduler = AgentTaskScheduler::new(executor);
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
        let scheduler = AgentTaskScheduler::new(executor);
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
        let scheduler = AgentTaskScheduler::new(executor);
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
        let scheduler = AgentTaskScheduler::new(executor);
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
        let scheduler = AgentTaskScheduler::new(executor);
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
