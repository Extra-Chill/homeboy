use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::Value;

fn finalized_receipt_fixture(
    cook_id: &str,
    run_id: &str,
) -> (
    HermeticTestContext,
    homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore,
) {
    use homeboy::agents::agent_task_lifecycle::AgentTaskRunState;

    let (context, lifecycle_store) = cook_fixture(cook_id, run_id, |lifecycle_store, _| {
        lifecycle_store
            .mutate_record(run_id, |record| {
                record.state = AgentTaskRunState::Succeeded;
                true
            })
            .expect("terminalize run");
        homeboy::agents::agent_task_lifecycle::record_cook_finalization_in_store(
            lifecycle_store,
            run_id,
            serde_json::json!({
                "status": "review_ready",
                "pr_number": 13968,
                "pr_url": "https://example.invalid/pull/13968"
            }),
        )
        .expect("persist finalization receipt");
    });
    assert!(!lifecycle_store.aggregate_path(run_id).exists());
    (context, lifecycle_store)
}

/// One durable Cook with a single attempt, terminalized by `terminalize`.
fn cook_fixture(
    cook_id: &str,
    run_id: &str,
    terminalize: impl FnOnce(
        &homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore,
        &homeboy::agents::agent_tasks::scheduler::AgentTaskPlan,
    ),
) -> (
    HermeticTestContext,
    homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore,
) {
    use homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore;
    use homeboy::agents::agent_task_service::{
        CookAiDisclosure, CookFinalization, CookIdentity, CookProviderTransport, CookRecipeStore,
        CookRequest, CookRetryPolicy, CookWorkspace,
    };
    use homeboy::agents::agent_tasks::scheduler::AgentTaskPlan;

    let context = HermeticTestContext::new();
    let plan = AgentTaskPlan::new(
        format!("{cook_id}-plan"),
        vec![serde_json::from_value(serde_json::json!({
            "task_id": "provider",
            "executor": { "backend": "fixture" },
            "instructions": "must not be admitted again"
        }))
        .expect("fixture provider task")],
    );
    let options = CookRequest {
        identity: CookIdentity {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.to_string(),
            initial_plan: plan.clone(),
        },
        workspace: CookWorkspace {
            to_worktree: "fixture@finalized".to_string(),
            source_worktree_path: None,
            task_base_sha: None,
            source_refs: Vec::new(),
        },
        provider_transport: CookProviderTransport {
            provider_invocation: None,
            attempt_dispatcher: None,
        },
        gates: Default::default(),
        retry_policy: CookRetryPolicy { max_attempts: 1 },
        finalization: CookFinalization {
            no_finalize: false,
            draft_pr: false,
            base: "main".to_string(),
            head: None,
            title: "Finalized fixture".to_string(),
            commit_message: "Finalized fixture".to_string(),
            protected_branches: Vec::new(),
        },
        ai_disclosure: CookAiDisclosure {
            ai_tool: "fixture".to_string(),
            ai_model: None,
            ai_used_for: "test".to_string(),
        },
        harvest_context: Default::default(),
    };
    CookRecipeStore::new(context.path_roots())
        .persist_initial_recipe(&options)
        .expect("persist Cook recipe");
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(serde_json::json!({})))
        .expect("persist lifecycle record");
    terminalize(&lifecycle_store, &plan);
    (context, lifecycle_store)
}

fn run_json(
    context: &HermeticTestContext,
    args: &[&str],
) -> (Option<i32>, Value, std::process::Output) {
    let output = context
        .command(TestBinary::HomeboyFixture)
        .args(args)
        .output()
        .expect("run homeboy");
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "output is JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    (output.status.code(), envelope, output)
}

/// A finalized Cook used to be replayed through `cook-continue`. `recover`
/// names no step for it, and neither preflight nor recovery touches it.
#[test]
fn recover_preflight_reports_no_step_for_a_finalized_cook() {
    use homeboy::agents::agent_task_service::{
        continuation_state_in_store, CookContinuationState, CookRecipeStore,
    };

    let cook_id = "public-finalization-replay";
    let run_id = "public-finalization-replay-attempt-1";
    let (context, lifecycle_store) = finalized_receipt_fixture(cook_id, run_id);
    let recipe_store = CookRecipeStore::new(context.path_roots());

    for preflight in [true, false] {
        let mut args = vec!["agent-task", "recover", cook_id];
        if preflight {
            args.push("--preflight");
        }
        let (code, envelope, _) = run_json(&context, &args);
        assert_eq!(code, Some(0), "{envelope:#}");
        let report = &envelope["data"];
        assert_eq!(report["schema"], "homeboy/agent-task-cook-recovery/v1");
        assert_eq!(report["cook_id"], cook_id);
        assert_eq!(report["run_id"], run_id);
        assert_eq!(report["status"], "review_ready");
        assert_eq!(report["step"]["kind"], "none", "{report:#}");
        assert_eq!(report["preflight"], preflight);
        assert!(report["admission"].is_null());
        assert!(report["result"].is_null());
        assert_eq!(
            continuation_state_in_store(&recipe_store, cook_id, run_id)
                .expect("continuation state"),
            CookContinuationState::Absent
        );
        assert!(!lifecycle_store.aggregate_path(run_id).exists());
    }
}

/// A queued terminal continuation belongs to its daemon worker. Recovery of a
/// finalized Cook observes it and leaves the pending claim untouched.
#[test]
fn recover_preflight_leaves_a_queued_finalized_continuation_to_its_worker() {
    use homeboy::agents::agent_task_service::{
        continuation_state_in_store, CookContinuationState, CookRecipeStore,
    };

    let cook_id = "public-queued-finalization-replay";
    let run_id = "public-queued-finalization-replay-attempt-1";
    let (context, lifecycle_store) = finalized_receipt_fixture(cook_id, run_id);
    let recipe_store = CookRecipeStore::new(context.path_roots());
    recipe_store
        .enqueue_terminal_continuation(cook_id, run_id)
        .expect("enqueue terminal continuation");

    let (code, envelope, _) = run_json(&context, &["agent-task", "recover", run_id, "--preflight"]);
    assert_eq!(code, Some(0), "{envelope:#}");
    assert_eq!(envelope["data"]["cook_id"], cook_id);
    assert_eq!(envelope["data"]["step"]["kind"], "none");
    assert_eq!(
        continuation_state_in_store(&recipe_store, cook_id, run_id).expect("continuation state"),
        CookContinuationState::Pending
    );
    assert!(!lifecycle_store.aggregate_path(run_id).exists());
}

#[test]
fn recover_preflight_rejects_an_unknown_id_without_initializing_state() {
    let context = HermeticTestContext::new();
    let (code, envelope, _) = run_json(
        &context,
        &[
            "--placement",
            "local",
            "agent-task",
            "recover",
            "missing-cook",
            "--preflight",
        ],
    );

    assert_eq!(code, Some(2), "{envelope:#}");
    assert_eq!(envelope["success"], false, "{envelope:#}");
    assert!(
        envelope.to_string().contains("missing-cook"),
        "{envelope:#}"
    );
    assert!(!context.data_dir().join("agent-task-runs").exists());
    assert!(!context.data_dir().join("agent-task-cooks").exists());
}

#[test]
fn pressured_recover_preflight_bypasses_startup_resource_admission() {
    let context = HermeticTestContext::new();
    let output = context
        .command(TestBinary::HomeboyFixture)
        .env("HOMEBOY_TEST_LOAD_AVERAGES", "100000,100000,100000")
        .args([
            "agent-task",
            "recover",
            "missing-cook-under-pressure",
            "--preflight",
        ])
        .output()
        .expect("run pressured recover preflight");

    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "pressured preflight output is JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    // The handler itself answers (unknown Cook), not resource admission.
    assert_eq!(output.status.code(), Some(2), "{envelope:#}");
    assert!(
        envelope.to_string().contains("missing-cook-under-pressure"),
        "{envelope:#}"
    );
    assert!(
        !envelope.to_string().contains("resource"),
        "resource admission must not refuse a bounded preflight: {envelope:#}"
    );
    assert!(!context.data_dir().join("agent-task-runs").exists());
    assert!(!context.data_dir().join("agent-task-cooks").exists());
}

/// A retryable pre-execution Cook failure used to be re-dispatched on a new
/// provider route through `retry <attempt> --backend/--selector/--model`.
/// `recover --preflight` names the retry step for it, and `retry` no longer
/// carries a Cook provider-route surface: the route is a `recover` override.
#[test]
fn recover_preflight_names_the_retry_step_retry_route_overrides_used_to_reach() {
    use homeboy::agents::agent_task_lifecycle::{
        record_cook_attempt_in_store, record_pre_execution_failure_in_store,
    };

    let cook_id = "public-retryable-pre-execution";
    let run_id = "public-retryable-pre-execution-attempt-1";
    let (context, lifecycle_store) = cook_fixture(cook_id, run_id, |lifecycle_store, plan| {
        record_cook_attempt_in_store(lifecycle_store, cook_id, 1, run_id)
            .expect("bind attempt to Cook");
        record_pre_execution_failure_in_store(
            lifecycle_store,
            run_id,
            plan,
            "provider_readiness",
            &homeboy::core::Error::internal_unexpected("injected readiness timeout")
                .with_retryable(true),
        )
        .expect("persist retryable readiness failure");
    });

    for id in [cook_id, run_id] {
        let (code, envelope, _) = run_json(&context, &["agent-task", "recover", id, "--preflight"]);
        assert_eq!(code, Some(0), "{envelope:#}");
        let report = &envelope["data"];
        assert_eq!(report["cook_id"], cook_id, "{report:#}");
        assert_eq!(report["run_id"], run_id, "{report:#}");
        assert_eq!(report["status"], "pre_execution_failure", "{report:#}");
        assert_eq!(report["step"]["kind"], "retry", "{report:#}");
        assert!(report["result"].is_null(), "{report:#}");
    }

    for route in [
        ["--backend", "replacement"],
        ["--selector", "replacement-selector"],
        ["--model", "replacement-model"],
    ] {
        let output = context
            .command(TestBinary::HomeboyFixture)
            .args(["agent-task", "retry", run_id])
            .args(route)
            .output()
            .expect("run retry");
        assert_eq!(output.status.code(), Some(2), "{route:?}");
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("unexpected argument"),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
    let records = homeboy::agents::agent_task_lifecycle::list_records_in_store(&lifecycle_store)
        .expect("records");
    assert_eq!(
        records.len(),
        1,
        "preflight and refused retries reserve nothing"
    );
}
