//! Provider-stall detection and rotation (#14965).
//!
//! A stalled provider produced no stdout/stderr/workspace progress within its
//! liveness window and is killed and classified `stalled` — see
//! `agent_task_timeout::DEFAULT_PROVIDER_LIVENESS_TIMEOUT_MS` and
//! `agent_task_provider::command_runner::classify_stall_or_rate_limit` for
//! where that classification is produced from a real subprocess. These tests
//! exercise the scheduler's reaction to an already-classified stall: does it
//! rotate (carrying forward any partial work a configured rotation entry
//! opted into adopting), or does it terminate cleanly instead of hanging.

use super::concurrency::concurrency_tests::init_git_workspace;
use super::shared::*;

mod stall_rotation_tests {
    use super::*;

    /// A fake provider that stalls on its first attempt after leaving one
    /// uncommitted edit in its attempt workspace, then succeeds on whichever
    /// attempt follows. Records whether that edit was already present in the
    /// workspace at the start of each call, so a test can prove whether a
    /// later provider inherited the stalled attempt's partial work.
    struct StallsOnceLeavingUncommittedEditExecutor {
        calls: Arc<AtomicUsize>,
        candidate_present_at_start: Arc<Mutex<Vec<bool>>>,
    }

    impl AgentTaskExecutorAdapter for StallsOnceLeavingUncommittedEditExecutor {
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
            let candidate_path = std::path::Path::new(root).join("candidate.txt");
            self.candidate_present_at_start
                .lock()
                .expect("observed presence")
                .push(candidate_path.is_file());
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                fs::write(&candidate_path, "wip\n").expect("leave uncommitted wip");
                let mut result = outcome(request.task_id, AgentTaskOutcomeStatus::ProviderError);
                result.failure_classification = Some(AgentTaskFailureClassification::Stalled);
                result.summary = Some(
                    "provider produced no process output, structured runtime progress, or \
                     workspace file activity before its liveness deadline"
                        .to_string(),
                );
                return result;
            }
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    /// A fake provider that always stalls: it never produces output, never
    /// touches the workspace, and never succeeds. Models the #14914 incident
    /// this issue was filed from ("20 min with zero output").
    struct AlwaysStallsExecutor {
        calls: Arc<AtomicUsize>,
    }

    impl AgentTaskExecutorAdapter for AlwaysStallsExecutor {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            self.calls.fetch_add(1, Ordering::SeqCst);
            let mut result = outcome(request.task_id, AgentTaskOutcomeStatus::ProviderError);
            result.failure_classification = Some(AgentTaskFailureClassification::Stalled);
            result.summary =
                Some("provider produced no output before its liveness deadline".to_string());
            result
        }
    }

    fn adopting_entry(backend: &str) -> AgentTaskProviderRotationEntry {
        AgentTaskProviderRotationEntry {
            backend: Some(backend.to_string()),
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
            ..AgentTaskProviderRotationEntry::default()
        }
    }

    fn non_adopting_entry(backend: &str) -> AgentTaskProviderRotationEntry {
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

    fn git_workspace_plan(workspace: &std::path::Path) -> AgentTaskPlan {
        init_git_workspace(workspace);
        let mut plan = plan_with_tasks(1);
        plan.tasks[0].workspace.root = Some(workspace.display().to_string());
        plan
    }

    /// Candidate-adoption identity verification resolves the canonical
    /// (remote-derived) repository identity only — it deliberately does not
    /// fall back to the local-checkout identity `finalize_candidate_artifacts`
    /// accepts for harvest bookkeeping (see `validate_and_apply_candidate_adoption`).
    /// A configured `origin` is required for adoption to verify, matching every
    /// real Cook target.
    fn git_workspace_plan_with_remote(workspace: &std::path::Path) -> AgentTaskPlan {
        let plan = git_workspace_plan(workspace);
        assert!(Command::new("git")
            .args([
                "remote",
                "add",
                "origin",
                "https://github.com/example/repo.git",
            ])
            .current_dir(workspace)
            .status()
            .expect("configure repository identity")
            .success());
        plan
    }

    /// Acceptance: "a fake provider that emits nothing: the run is classified
    /// `provider_stalled` and rotates". A stall with no partial work is the
    /// common case (#14914's "20 min with zero output"): nothing to carry
    /// forward, so a plain clean-checkout rotation already fully recovers it.
    #[test]
    fn a_provider_that_emits_nothing_is_classified_stalled_and_rotates_to_the_next_provider() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let mut plan = git_workspace_plan(&workspace);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![non_adopting_entry("fallback-backend")],
            max_attempts: None,
            liveness_timeout_ms: None,
        });
        enable_rotation(&mut plan);
        // This scenario has no partial work: the fake provider never writes
        // to the workspace, matching the #14914 "20 min with zero output"
        // shape.
        let scheduler = AgentTaskScheduler::new(Arc::new(AlwaysStallsExecutorThenSucceeds {
            calls: Arc::new(AtomicUsize::new(0)),
        }));

        let aggregate = scheduler.run(plan);

        assert_eq!(aggregate.status, AgentTaskAggregateStatus::Succeeded);
        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(
            attempts.len(),
            2,
            "records both the stalled and the rotated attempt"
        );
        assert_eq!(attempts[0]["status"], "provider_error");
        assert_eq!(attempts[0]["failure_classification"], "stalled");
        assert_eq!(attempts[0]["backend"], "test");
        assert_eq!(attempts[1]["status"], "succeeded");
        assert_eq!(attempts[1]["backend"], "fallback-backend");
        assert!(aggregate.events.iter().any(|event| {
            event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("provider rotation queued"))
        }));
    }

    struct AlwaysStallsExecutorThenSucceeds {
        calls: Arc<AtomicUsize>,
    }

    impl AgentTaskExecutorAdapter for AlwaysStallsExecutorThenSucceeds {
        fn execute(
            &self,
            request: AgentTaskRequest,
            _context: AgentTaskExecutionContext,
        ) -> AgentTaskOutcome {
            if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                let mut result = outcome(request.task_id, AgentTaskOutcomeStatus::ProviderError);
                result.failure_classification = Some(AgentTaskFailureClassification::Stalled);
                result.summary =
                    Some("provider produced no output before its liveness deadline".to_string());
                return result;
            }
            outcome(request.task_id, AgentTaskOutcomeStatus::Succeeded)
        }
    }

    /// Acceptance: "keeping the worktree's current state so partial work
    /// carries over" — when the operator's rotation entry explicitly opts a
    /// route into adopting the previous candidate (`adoption:
    /// adopt_previous_candidate`, the same mechanism every other rotation
    /// path already uses), a stall that left uncommitted edits behind must
    /// not strand them: the next provider's attempt workspace already
    /// contains them before it runs.
    #[test]
    fn a_stall_with_an_adopting_rotation_entry_carries_the_uncommitted_edit_into_the_next_provider()
    {
        // Candidate-adoption matching binds an artifact's repository/workspace
        // identity from the durable run, so this scenario needs a real
        // submitted run rather than the unrecorded-run default the other
        // tests in this file use.
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let run_id = "stall-adoption-carries-forward";
        let mut plan = git_workspace_plan_with_remote(&workspace);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![adopting_entry("fallback-backend")],
            max_attempts: None,
            liveness_timeout_ms: None,
        });
        enable_rotation(&mut plan);
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit run");
        let executor = StallsOnceLeavingUncommittedEditExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
            candidate_present_at_start: Arc::new(Mutex::new(Vec::new())),
        };
        let candidate_present_at_start = Arc::clone(&executor.candidate_present_at_start);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor)).with_run_id(run_id);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        let observed = candidate_present_at_start
            .lock()
            .expect("observed presence");
        assert_eq!(*observed, vec![false, true], "the first attempt starts clean; the rotated-to attempt already carries the first attempt's uncommitted edit");
        let attempts = aggregate.outcomes[0]
            .metadata
            .pointer("/provider_rotation/attempts")
            .and_then(Value::as_array)
            .expect("rotation attempts evidence");
        assert_eq!(attempts.len(), 2);
        assert_eq!(attempts[0]["failure_classification"], "stalled");
        assert_eq!(attempts[1]["backend"], "fallback-backend");
        assert_eq!(attempts[1]["status"], "succeeded");
    }

    /// The scheduler never adopts a candidate implicitly (see
    /// `select_candidate_adoption`): without an `adoption` template on the
    /// rotation entry, a stall still rotates cleanly, but the next provider
    /// starts from the task's clean base rather than inheriting the
    /// uncommitted edit.
    #[test]
    fn a_stall_without_an_adopting_rotation_entry_still_rotates_but_does_not_carry_the_edit() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let mut plan = git_workspace_plan(&workspace);
        plan.options.rotation = Some(AgentTaskProviderRotationPolicy {
            entries: vec![non_adopting_entry("fallback-backend")],
            max_attempts: None,
            liveness_timeout_ms: None,
        });
        enable_rotation(&mut plan);
        let executor = StallsOnceLeavingUncommittedEditExecutor {
            calls: Arc::new(AtomicUsize::new(0)),
            candidate_present_at_start: Arc::new(Mutex::new(Vec::new())),
        };
        let candidate_present_at_start = Arc::clone(&executor.candidate_present_at_start);
        let scheduler = AgentTaskScheduler::new(Arc::new(executor));

        let aggregate = scheduler.run(plan);

        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "{aggregate:#?}"
        );
        let observed = candidate_present_at_start
            .lock()
            .expect("observed presence");
        assert_eq!(
            *observed,
            vec![false, false],
            "without an explicit adoption opt-in the rotated-to attempt starts clean, not implicitly inheriting the stalled attempt's edit"
        );
        assert!(aggregate.events.iter().any(|event| {
            event
                .message
                .as_deref()
                .is_some_and(|message| message.contains("provider rotation queued"))
        }));
    }

    /// Acceptance: "if not [rotation allowed], stops with a clear terminal
    /// reason instead of hanging". No rotation policy at all: the stalled
    /// attempt's uncommitted edit is preserved as a recoverable candidate
    /// (not silently discarded) and the run reaches a definite terminal
    /// status rather than hanging or requiring an operator to notice by hand.
    #[test]
    fn a_stall_with_no_rotation_policy_terminates_as_a_recoverable_candidate() {
        // As above: recoverability requires the base-bound-patch-candidate
        // check, which binds provenance from the durable run.
        let _home = homeboy_core::test_support::HomeGuard::new();
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let run_id = "stall-no-rotation-terminal-candidate";
        let plan = git_workspace_plan(&workspace);
        // No `plan.options.rotation` at all.
        crate::agent_task_lifecycle::submit_plan(&plan, Some(run_id)).expect("submit run");
        let calls = Arc::new(AtomicUsize::new(0));
        let scheduler =
            AgentTaskScheduler::new(Arc::new(StallsOnceLeavingUncommittedEditExecutor {
                calls: Arc::clone(&calls),
                candidate_present_at_start: Arc::new(Mutex::new(Vec::new())),
            }))
            .with_run_id(run_id);

        let aggregate = scheduler.run(plan);

        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "no rotation configured; never dispatches a second attempt"
        );
        assert_eq!(
            aggregate.status,
            AgentTaskAggregateStatus::PartialRecoverable,
            "{aggregate:#?}"
        );
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::CandidateRecoverable
        );
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Stalled)
        );
    }

    /// A pure "zero output, no progress" stall — the #14914 incident shape —
    /// with no rotation policy configured terminates as a plain provider
    /// error rather than hanging, since there is no patch to recover.
    #[test]
    fn a_stall_with_no_partial_work_and_no_rotation_policy_terminates_cleanly() {
        let calls = Arc::new(AtomicUsize::new(0));
        let scheduler = AgentTaskScheduler::new(Arc::new(AlwaysStallsExecutor {
            calls: Arc::clone(&calls),
        }));
        let plan = plan_with_tasks(1);

        let aggregate = scheduler.run(plan);

        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_ne!(
            aggregate.status,
            AgentTaskAggregateStatus::Succeeded,
            "a stall must never silently look like success"
        );
        assert_eq!(
            aggregate.outcomes[0].status,
            AgentTaskOutcomeStatus::ProviderError
        );
        assert_eq!(
            aggregate.outcomes[0].failure_classification,
            Some(AgentTaskFailureClassification::Stalled)
        );
    }
}
