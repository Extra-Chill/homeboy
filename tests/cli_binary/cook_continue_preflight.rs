use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::Value;

fn finalized_receipt_fixture(
    cook_id: &str,
    run_id: &str,
) -> (
    HermeticTestContext,
    homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore,
) {
    use homeboy::agents::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunState};
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
    (context, lifecycle_store)
}

#[test]
fn public_continuation_preflight_matches_unscheduled_finalization_receipt_execution() {
    let cook_id = "public-finalization-replay";
    let run_id = "public-finalization-replay-attempt-1";
    let (context, lifecycle_store) = finalized_receipt_fixture(cook_id, run_id);

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
    assert_eq!(report["status"], "continuation_not_scheduled");
    assert_eq!(report["admitted"], false);
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
        [
            "recipe",
            "selection",
            "lifecycle",
            "finalization_receipt",
            "continuation_claim"
        ]
    );
    assert!(!context
        .data_dir()
        .join("agent-task-cook-continuations")
        .exists());
    assert!(!lifecycle_store.aggregate_path(run_id).exists());

    let execution = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", cook_id])
        .output()
        .expect("run public finalization receipt continuation");
    assert_eq!(execution.status.code(), Some(0));
    let execution_envelope: Value =
        serde_json::from_slice(&execution.stdout).unwrap_or_else(|error| {
            panic!(
                "execution output is JSON: {error}\nstdout: {}\nstderr: {}",
                String::from_utf8_lossy(&execution.stdout),
                String::from_utf8_lossy(&execution.stderr)
            )
        });
    assert_eq!(execution_envelope["data"]["status"], report["status"]);
    assert_eq!(execution_envelope["data"]["latest_run_id"], run_id);
    let queue_root = context.data_dir().join("agent-task-cook-continuations");
    assert!(queue_root.is_dir());
    assert_eq!(
        std::fs::read_dir(queue_root)
            .expect("read empty continuation queue")
            .count(),
        0
    );
    assert!(!lifecycle_store.aggregate_path(run_id).exists());
}

#[test]
fn public_continuation_preflight_validates_queued_finalization_receipt_dispatcher() {
    use homeboy::agents::agent_task_service::CookRecipeStore;

    let cook_id = "public-queued-finalization-replay";
    let run_id = "public-queued-finalization-replay-attempt-1";
    let (context, lifecycle_store) = finalized_receipt_fixture(cook_id, run_id);
    let recipe_store = CookRecipeStore::new(context.path_roots());
    recipe_store
        .enqueue_terminal_continuation(cook_id, run_id)
        .expect("enqueue terminal continuation");
    let queue_root = context.data_dir().join("agent-task-cook-continuations");
    let pending = std::fs::read_dir(&queue_root)
        .expect("read continuation queue")
        .next()
        .expect("queued continuation")
        .expect("read queued continuation")
        .path();
    let pending_bytes = std::fs::read(&pending).expect("read queued continuation bytes");

    let output = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", cook_id, "--preflight"])
        .output()
        .expect("run queued finalization replay preflight");

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
    assert!(report["phases"]
        .as_array()
        .expect("preflight phases")
        .iter()
        .any(|phase| phase["phase"] == "transport" && phase["status"] == "passed"));
    assert_eq!(std::fs::read(&pending).unwrap(), pending_bytes);
    assert!(!lifecycle_store.aggregate_path(run_id).exists());

    let execution = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", cook_id])
        .output()
        .expect("run queued finalization receipt continuation");
    assert_eq!(execution.status.code(), Some(0));
    let execution_envelope: Value = serde_json::from_slice(&execution.stdout).unwrap();
    assert_eq!(execution_envelope["data"]["status"], report["status"]);
    assert!(!pending.exists());
    assert!(std::fs::read_dir(&queue_root)
        .expect("read completed continuation queue")
        .any(|entry| entry
            .expect("read completed continuation")
            .path()
            .extension()
            .is_some_and(|extension| extension == "completed")));
    assert!(!lifecycle_store.aggregate_path(run_id).exists());
}

#[test]
fn malformed_queued_finalization_dispatcher_fails_preflight_and_execution() {
    use homeboy::agents::agent_task_service::CookRecipeStore;

    let cook_id = "public-malformed-finalization-dispatcher";
    let run_id = "public-malformed-finalization-dispatcher-attempt-1";
    let (context, lifecycle_store) = finalized_receipt_fixture(cook_id, run_id);
    CookRecipeStore::new(context.path_roots())
        .enqueue_terminal_continuation(cook_id, run_id)
        .expect("enqueue terminal continuation");
    let recipe_path = context
        .data_dir()
        .join("agent-task-cooks")
        .join(cook_id)
        .join("recipe.json");
    let mut recipe: Value =
        serde_json::from_slice(&std::fs::read(&recipe_path).expect("read Cook recipe"))
            .expect("decode Cook recipe");
    recipe["promotion_transport"]["attempt_dispatch"] = serde_json::json!({ "kind": "lab" });
    std::fs::write(
        &recipe_path,
        serde_json::to_vec_pretty(&recipe).expect("encode malformed Cook recipe"),
    )
    .expect("persist malformed Cook recipe");
    let queue_root = context.data_dir().join("agent-task-cook-continuations");
    let queued_before = std::fs::read_dir(&queue_root)
        .expect("read continuation queue")
        .map(|entry| {
            let path = entry.expect("read continuation entry").path();
            (
                path.file_name().unwrap().to_owned(),
                std::fs::read(path).expect("read continuation entry"),
            )
        })
        .collect::<Vec<_>>();

    let preflight = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", cook_id, "--preflight"])
        .output()
        .expect("run malformed dispatcher preflight");

    assert_eq!(preflight.status.code(), Some(1));
    let preflight_envelope: Value = serde_json::from_slice(&preflight.stdout).unwrap();
    assert_eq!(preflight_envelope["data"]["admitted"], false);
    assert_eq!(
        preflight_envelope["data"]["phases"]
            .as_array()
            .unwrap()
            .last()
            .unwrap()["phase"],
        "transport"
    );
    assert!(preflight_envelope["data"]["phases"]
        .to_string()
        .contains("attempt_dispatch"));
    let queued_after = std::fs::read_dir(&queue_root)
        .expect("read continuation queue after preflight")
        .map(|entry| {
            let path = entry.expect("read continuation entry").path();
            (
                path.file_name().unwrap().to_owned(),
                std::fs::read(path).expect("read continuation entry"),
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(queued_after, queued_before);
    assert!(!lifecycle_store.aggregate_path(run_id).exists());

    let execution = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", cook_id])
        .output()
        .expect("run malformed dispatcher continuation");
    assert_eq!(execution.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&execution.stdout).contains("attempt_dispatch"));
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
