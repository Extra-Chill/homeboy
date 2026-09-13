use homeboy::agents::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunState};
use homeboy::agents::agent_task_service::{
    CookAiDisclosure, CookFinalization, CookIdentity, CookProviderTransport, CookRecipeStore,
    CookRequest, CookRetryPolicy, CookWorkspace,
};
use homeboy::agents::agent_tasks::scheduler::AgentTaskPlan;
use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use rusqlite::Connection;
use serde_json::Value;
use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

#[test]
fn retry_recovers_a_missing_transport_child_projection_without_provider_dispatch() {
    let context = HermeticTestContext::new();
    let cook_id = "cook-transport-retry-2026-09-13-181954";
    let parent_run_id = format!("{cook_id}-attempt-1");
    let child_run_id = format!("{parent_run_id}-transport-retry-00000000000000000000000000000001");
    let plan = AgentTaskPlan::new(
        format!("{cook_id}-plan"),
        vec![serde_json::from_value(serde_json::json!({
            "task_id": "provider",
            "executor": { "backend": "fixture" },
            "instructions": "fixture must not dispatch a provider"
        }))
        .expect("fixture provider task")],
    );
    let options = CookRequest {
        identity: CookIdentity {
            cook_id: cook_id.to_string(),
            initial_run_id: parent_run_id.clone(),
            initial_plan: plan.clone(),
        },
        workspace: CookWorkspace {
            to_worktree: "fixture@retry-projection".to_string(),
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
        retry_policy: CookRetryPolicy { max_attempts: 2 },
        finalization: CookFinalization {
            no_finalize: true,
            draft_pr: false,
            base: "main".to_string(),
            head: None,
            title: "Retry projection fixture".to_string(),
            commit_message: "Retry projection fixture".to_string(),
            protected_branches: Vec::new(),
        },
        ai_disclosure: CookAiDisclosure {
            ai_tool: "fixture".to_string(),
            ai_model: None,
            ai_used_for: "test".to_string(),
        },
        harvest_context: Default::default(),
    };
    let recipe_store = CookRecipeStore::new(context.path_roots());
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist Cook recipe");
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    for run_id in [&parent_run_id, &child_run_id] {
        store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(serde_json::json!({})))
            .expect("persist lifecycle attempt");
        store
            .mutate_record(run_id, |record| {
                record.state = AgentTaskRunState::Failed;
                record.metadata["cook_id"] = serde_json::json!(cook_id);
                record.metadata["cook_attempt"] = serde_json::json!(1);
                record.metadata["provider_executions_consumed"] = serde_json::json!(0);
                record.metadata["pre_execution_failure"] = serde_json::json!({
                    "retryable": true,
                    "phase": "lab_handoff"
                });
                true
            })
            .expect("terminalize transport attempt");
    }
    recipe_store
        .record_recipe_attempt_replacement(cook_id, &parent_run_id, &child_run_id)
        .expect("persist transport replacement");
    store
        .record_cook_attempt(cook_id, 1, &parent_run_id)
        .expect("index Cook parent");
    store
        .record_cook_attempt(cook_id, 1, &child_run_id)
        .expect("index Cook transport child");

    // A crash after the lifecycle row commits but before its derived resource
    // projection is persisted is a recoverable durable state.
    let connection = Connection::open(store.observation_db_path()).expect("open observation DB");
    connection
        .execute(
            "DELETE FROM control_plane_resource_aliases WHERE resource_type = ?1 AND resource_id = ?2",
            ["agent_task_run", child_run_id.as_str()],
        )
        .expect("remove child aliases");
    connection
        .execute(
            "DELETE FROM control_plane_resources WHERE resource_type = ?1 AND resource_id = ?2",
            ["agent_task_run", child_run_id.as_str()],
        )
        .expect("remove child resource projection");

    let mut command = context.command(TestBinary::HomeboyFixture);
    command.args([
        "agent-task",
        "retry",
        &child_run_id,
        "--idempotency-key",
        "retry-projection-recursion",
    ]);
    let output = bounded_output(&mut command, Duration::from_secs(10));
    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("structured CLI result");
    assert_eq!(envelope["success"], true);
    assert_eq!(envelope["data"]["action"], "retry");
}

fn bounded_output(command: &mut Command, timeout: Duration) -> Output {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    let mut child = command.spawn().expect("start real CLI retry");
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait().expect("inspect real CLI retry").is_some() {
            return child.wait_with_output().expect("collect real CLI retry");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            panic!("real CLI retry exceeded {timeout:?}");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
