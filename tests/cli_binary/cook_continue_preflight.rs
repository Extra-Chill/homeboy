use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::Value;

#[test]
fn public_continuation_preflight_replays_terminal_aggregate_less_finalization_receipt() {
    use homeboy::agents::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunState};
    use homeboy::agents::agent_task_service::{
        CookAiDisclosure, CookFinalization, CookIdentity, CookProviderTransport, CookRecipeStore,
        CookRequest, CookRetryPolicy, CookWorkspace,
    };
    use homeboy::agents::agent_tasks::scheduler::AgentTaskPlan;

    let context = HermeticTestContext::new();
    let cook_id = "public-finalization-replay";
    let run_id = "public-finalization-replay-attempt-1";
    let plan = AgentTaskPlan::new(
        "public-finalization-replay-plan",
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
            provider_command: None,
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
    lifecycle_store
        .mutate_record(run_id, |record| {
            record.state = AgentTaskRunState::Succeeded;
            record.metadata["cook_finalization"] = serde_json::json!({
                "status": "review_ready",
                "pr_number": 13968,
                "pr_url": "https://example.invalid/pull/13968"
            });
            true
        })
        .expect("persist finalization receipt");
    assert!(!lifecycle_store.aggregate_path(run_id).exists());

    let output = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", cook_id, "--preflight"])
        .output()
        .expect("run public finalization replay preflight");

    assert_eq!(output.status.code(), Some(0));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "preflight output is JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    let report = &envelope["data"];
    assert_eq!(report["status"], "review_ready");
    assert_eq!(report["admitted"], true);
    assert_eq!(report["execution_required"], false);
    assert_eq!(
        report["continuation"]["path"],
        "finalization_receipt_replay"
    );
    assert_eq!(report["continuation"]["provider_replay"], false);
    assert_eq!(report["finalization"]["pr_number"], 13968);
    assert_eq!(
        report["phases"]
            .as_array()
            .expect("preflight phases")
            .iter()
            .map(|phase| phase["phase"].as_str().expect("phase name"))
            .collect::<Vec<_>>(),
        ["recipe", "selection", "lifecycle", "finalization_receipt"]
    );
    assert!(!context
        .data_dir()
        .join("agent-task-cook-continuations")
        .exists());
    assert!(!lifecycle_store.aggregate_path(run_id).exists());
}

#[test]
fn public_continuation_preflight_reaches_read_only_handler_without_initializing_state() {
    let context = HermeticTestContext::new();
    let output = context
        .command(TestBinary::HomeboyFixture)
        .args([
            "--placement",
            "local",
            "agent-task",
            "cook-continue",
            "missing-cook",
            "--preflight",
            "--rearm",
        ])
        .output()
        .expect("run public continuation preflight");

    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "preflight output is JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        envelope["data"]["schema"],
        "homeboy/agent-task-cook-continue-preflight/v1"
    );
    assert_eq!(envelope["data"]["admitted"], false);
    assert_eq!(
        envelope["data"]["side_effects"],
        serde_json::json!({
            "process_execution": false,
            "state_mutation": false,
            "provider_dispatch": false,
            "git_mutation": false,
            "git_index_mutation": false,
            "github_mutation": false,
            "finalization": false,
        })
    );
    assert!(!context.data_dir().join("observations.sqlite").exists());
    assert!(!context.data_dir().join("agent-task-runs").exists());
    assert!(!context.data_dir().join("agent-task-cooks").exists());
}

#[test]
fn pressured_public_continuation_preflight_bypasses_startup_resource_admission() {
    let context = HermeticTestContext::new();
    let output = context
        .command(TestBinary::HomeboyFixture)
        .env("HOMEBOY_TEST_LOAD_AVERAGES", "100000,100000,100000")
        .args([
            "agent-task",
            "cook-continue",
            "missing-cook-under-pressure",
            "--preflight",
        ])
        .output()
        .expect("run pressured public continuation preflight");

    assert_eq!(output.status.code(), Some(1));
    let envelope: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "pressured preflight output is JSON: {error}\nstdout: {}\nstderr: {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
    });
    assert_eq!(
        envelope["data"]["schema"],
        "homeboy/agent-task-cook-continue-preflight/v1"
    );
    assert_eq!(envelope["data"]["admitted"], false);
    assert!(
        output.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(!context.data_dir().join("observations.sqlite").exists());
    assert!(!context.data_dir().join("agent-task-runs").exists());
    assert!(!context.data_dir().join("agent-task-cooks").exists());
}
