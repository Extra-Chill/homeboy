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
            provider_ci: None,
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
                record.metadata["provider_executions_consumed"] = serde_json::json!(1);
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

    let mut status = context.command(TestBinary::HomeboyFixture);
    status.args(["agent-task", "status", &child_run_id]);
    let status = bounded_output(&mut status, Duration::from_secs(10));
    assert_eq!(
        status.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&status.stderr)
    );

    let mut retry = context.command(TestBinary::HomeboyFixture);
    retry.args([
        "agent-task",
        "retry",
        &child_run_id,
        "--idempotency-key",
        "retry-projection-recursion",
    ]);
    let output = bounded_output(&mut retry, Duration::from_secs(10));
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

#[test]
fn historical_runtime_retry_rejects_before_action_admission_then_replays_once() {
    let (Ok(old_binary), Ok(new_binary)) = (
        std::env::var("HOMEBOY_15504_OLD_BINARY"),
        std::env::var("HOMEBOY_15504_NEW_BINARY"),
    ) else {
        return;
    };
    exercise_historical_runtime_retry(&old_binary, "homeboy 0.399.35+");
    exercise_historical_runtime_retry(&new_binary, "homeboy 0.400.2+");
}

fn exercise_historical_runtime_retry(compatible_binary: &str, expected_identity_prefix: &str) {
    let context = HermeticTestContext::new();
    let cook_id = "cook-runtime-atomic-replay";
    let run_id = format!("{cook_id}-attempt-1");
    let retry_id = format!("{cook_id}-attempt-2");
    let provider_started = context.temp_dir().join("fixture-provider-started");
    let plan_path = context.temp_dir().join("runtime-retry-plan.json");
    let plan = AgentTaskPlan::new(
        format!("{cook_id}-plan"),
        vec![serde_json::from_value(serde_json::json!({
            "task_id": "provider",
            "executor": { "backend": "fixture", "model": "fixture-model" },
            "instructions": "Run the harmless fixture provider exactly once."
        }))
        .expect("fixture provider task")],
    );
    std::fs::write(
        &plan_path,
        serde_json::to_vec(&plan).expect("serialize retry plan"),
    )
    .expect("write retry plan");
    let options = CookRequest {
        identity: CookIdentity {
            cook_id: cook_id.to_string(),
            initial_run_id: run_id.clone(),
            initial_plan: plan.clone(),
        },
        workspace: CookWorkspace {
            to_worktree: "fixture@runtime-atomic-replay".to_string(),
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
            provider_ci: None,
            base: "main".to_string(),
            head: None,
            title: "Runtime-compatible retry".to_string(),
            commit_message: "Runtime-compatible retry".to_string(),
            protected_branches: Vec::new(),
        },
        ai_disclosure: CookAiDisclosure {
            ai_tool: "fixture".to_string(),
            ai_model: Some("fixture-model".to_string()),
            ai_used_for: "runtime retry integration test".to_string(),
        },
        harvest_context: Default::default(),
    };
    let recipe_store = CookRecipeStore::new(context.path_roots());
    recipe_store
        .persist_initial_recipe(&options)
        .expect("persist Cook recipe");
    let recipe_path = context
        .data_dir()
        .join("agent-task-cooks")
        .join(cook_id)
        .join("recipe.json");
    let mut recipe: Value =
        serde_json::from_slice(&std::fs::read(&recipe_path).expect("read recipe"))
            .expect("parse recipe");
    let old_version = Command::new(compatible_binary)
        .arg("--version")
        .output()
        .expect("run preserved old binary identity");
    assert!(old_version.status.success());
    let old_display = String::from_utf8_lossy(&old_version.stdout)
        .lines()
        .next()
        .expect("old runtime display")
        .trim()
        .to_string();
    assert!(
        old_display.starts_with(expected_identity_prefix),
        "{old_display}"
    );
    recipe["runtime_generation"] = old_display.clone().into();
    std::fs::write(
        &recipe_path,
        serde_json::to_vec_pretty(&recipe).expect("encode recipe"),
    )
    .expect("persist historical recipe runtime");

    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let mut source_submit = Command::new(compatible_binary);
    source_submit
        .args([
            "agent-task".to_string(),
            "submit".to_string(),
            "--plan".to_string(),
            format!("@{}", plan_path.to_str().expect("plan path UTF-8")),
            "--run-id".to_string(),
            run_id.clone(),
        ])
        .env("HOME", context.home())
        .env("XDG_CONFIG_HOME", context.root().join(".config"))
        .env("XDG_DATA_HOME", context.root().join("data"))
        .env(
            homeboy::core::paths::HOMEBOY_DATA_DIR_ENV,
            context.data_dir(),
        )
        .env(
            homeboy::core::paths::DAEMON_STATE_DIR_ENV,
            context.daemon_dir(),
        )
        .env("HOMEBOY_TEST_DAEMON_NAMESPACE", context.daemon_dir())
        .env("HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP", "1")
        .env("HOMEBOY_ARTIFACT_ROOT", context.artifact_dir())
        .env("HOMEBOY_RUNTIME_TMPDIR", context.runtime_dir())
        .env("TMPDIR", context.temp_dir())
        .env("TEMP", context.temp_dir())
        .env("TMP", context.temp_dir())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_USE_ENV", "1")
        .env(
            "HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE",
            compatible_binary,
        )
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_SOURCE", compatible_binary)
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY", &old_display);
    let source_submitted = bounded_output(&mut source_submit, Duration::from_secs(30));
    assert!(
        source_submitted.status.success(),
        "old-runtime source submit failed: {}",
        String::from_utf8_lossy(&source_submitted.stdout)
    );
    store
        .mutate_record(&run_id, |record| {
            record.state = AgentTaskRunState::Failed;
            record.metadata["cook_id"] = serde_json::json!(cook_id);
            record.metadata["cook_attempt"] = serde_json::json!(1);
            record.metadata["pre_execution_failure"] = serde_json::json!({
                "retryable": true,
                "phase": "runtime_readiness"
            });
            record.metadata["provider_executions_consumed"] = serde_json::json!(0);
            true
        })
        .expect("record zero-execution failure");
    store
        .record_cook_attempt(cook_id, 1, &run_id)
        .expect("bind failed source attempt");

    let run_retry = |binary: &str| {
        let mut command = Command::new(binary);
        command
            .args([
                "--placement",
                "local",
                "agent-task",
                "retry",
                &run_id,
                "--new-run-id",
                &retry_id,
                "--run",
                "--idempotency-key",
                "runtime-compatible-replay-1",
            ])
            .env("HOME", context.home())
            .env("XDG_CONFIG_HOME", context.root().join(".config"))
            .env("XDG_DATA_HOME", context.root().join("data"))
            .env(
                homeboy::core::paths::HOMEBOY_DATA_DIR_ENV,
                context.data_dir(),
            )
            .env(
                homeboy::core::paths::DAEMON_STATE_DIR_ENV,
                context.daemon_dir(),
            )
            .env("HOMEBOY_TEST_DAEMON_NAMESPACE", context.daemon_dir())
            .env("HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP", "1")
            .env("HOMEBOY_ARTIFACT_ROOT", context.artifact_dir())
            .env("HOMEBOY_RUNTIME_TMPDIR", context.runtime_dir())
            .env("TMPDIR", context.temp_dir())
            .env("TEMP", context.temp_dir())
            .env("TMP", context.temp_dir())
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .env("HOMEBOY_FIXTURE_PROVIDER_STARTED_FILE", &provider_started)
            .env_remove("HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE")
            .env_remove("HOMEBOY_TEST_CONTROLLER_RUNTIME_SOURCE")
            .env_remove("HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY")
            .env_remove("HOMEBOY_TEST_CONTROLLER_RUNTIME_USE_ENV");
        if binary == compatible_binary {
            command
                .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_USE_ENV", "1")
                .env(
                    "HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE",
                    compatible_binary,
                )
                .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_SOURCE", compatible_binary)
                .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY", &old_display);
        }
        bounded_output(&mut command, Duration::from_secs(60))
    };

    let candidate_binary = env!("CARGO_BIN_EXE_homeboy");
    let rejected = run_retry(&candidate_binary);
    assert!(
        !rejected.status.success(),
        "candidate unexpectedly accepted old pin"
    );
    let recipe_after_rejection = recipe_store
        .load_recipe(cook_id)
        .expect("recipe after refusal");
    assert_eq!(recipe_after_rejection.attempts.len(), 1);
    assert_eq!(
        store
            .read_cook_index(cook_id)
            .expect("Cook index")
            .latest_run_id,
        run_id
    );
    assert!(
        !homeboy::agents::agent_task_lifecycle::run_record_exists_in_store(&store, &retry_id)
            .expect("check reserved successor"),
        "incompatible retry must not persist its declared successor"
    );

    let compatible = run_retry(compatible_binary);
    assert!(
        compatible.status.success(),
        "old-runtime compatible replay failed ({}): stdout={} stderr={}",
        compatible.status,
        String::from_utf8_lossy(&compatible.stdout),
        String::from_utf8_lossy(&compatible.stderr),
    );
    let successor = store.read_record(&retry_id).expect("compatible successor");
    assert_eq!(successor.metadata["retry_of"], run_id);
    assert_eq!(
        store
            .read_cook_index(cook_id)
            .expect("advanced Cook index")
            .latest_run_id,
        retry_id
    );
    let mut run_successor = Command::new(compatible_binary);
    run_successor
        .args(["--placement", "local", "agent-task", "run", &retry_id])
        .env("HOME", context.home())
        .env("XDG_CONFIG_HOME", context.root().join(".config"))
        .env("XDG_DATA_HOME", context.root().join("data"))
        .env(
            homeboy::core::paths::HOMEBOY_DATA_DIR_ENV,
            context.data_dir(),
        )
        .env(
            homeboy::core::paths::DAEMON_STATE_DIR_ENV,
            context.daemon_dir(),
        )
        .env("HOMEBOY_TEST_DAEMON_NAMESPACE", context.daemon_dir())
        .env("HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP", "1")
        .env("HOMEBOY_ARTIFACT_ROOT", context.artifact_dir())
        .env("HOMEBOY_RUNTIME_TMPDIR", context.runtime_dir())
        .env("TMPDIR", context.temp_dir())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_FIXTURE_PROVIDER_STARTED_FILE", &provider_started)
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_USE_ENV", "1")
        .env(
            "HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE",
            compatible_binary,
        )
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_SOURCE", compatible_binary)
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY", &old_display);
    let run_output = bounded_output(&mut run_successor, Duration::from_secs(60));
    assert!(
        run_output.status.success(),
        "old-runtime provider execution failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run_output.stdout),
        String::from_utf8_lossy(&run_output.stderr)
    );
    let provider_deadline = Instant::now() + Duration::from_secs(30);
    while !provider_started.exists() && Instant::now() < provider_deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(
        provider_started.exists(),
        "compatible retry did not admit fixture provider; successor={}",
        store
            .read_record(&retry_id)
            .expect("inspect successor")
            .metadata
    );
    assert_eq!(
        std::fs::read_to_string(provider_started)
            .expect("fixture provider admission marker")
            .lines()
            .count(),
        1,
        "compatible replay admits the fixture provider exactly once"
    );
    let completed = store
        .read_record(&retry_id)
        .expect("read admitted provider attempt");
    assert_eq!(completed.metadata["provider_executions_consumed"], 1);
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
