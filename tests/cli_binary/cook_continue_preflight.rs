use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::Value;

#[cfg(unix)]
struct KillOnDrop(std::process::Child);

#[cfg(unix)]
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

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
            provider_ci: None,
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
    use homeboy::agents::agent_task_service::{
        continuation_state_in_store, CookContinuationState, CookRecipeStore,
    };

    let cook_id = "public-finalization-replay";
    let run_id = "public-finalization-replay-attempt-1";
    let (context, lifecycle_store) = finalized_receipt_fixture(cook_id, run_id);
    let recipe_store = CookRecipeStore::new(context.path_roots());

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
    assert_eq!(
        continuation_state_in_store(&recipe_store, cook_id, run_id).expect("continuation state"),
        CookContinuationState::Absent
    );
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
    // Continuation state is owned by the lifecycle record: an unscheduled
    // finalization replay never publishes continuation work at all.
    assert_eq!(
        continuation_state_in_store(&recipe_store, cook_id, run_id).expect("continuation state"),
        CookContinuationState::Absent
    );
    assert!(!lifecycle_store.aggregate_path(run_id).exists());
}

#[test]
fn public_continuation_preflight_validates_queued_finalization_receipt_dispatcher() {
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
    assert_eq!(
        continuation_state_in_store(&recipe_store, cook_id, run_id).expect("continuation state"),
        CookContinuationState::Pending
    );

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
    // Preflight is an observation, so the pending claim survives it untouched.
    assert_eq!(
        continuation_state_in_store(&recipe_store, cook_id, run_id).expect("continuation state"),
        CookContinuationState::Pending
    );
    assert!(!lifecycle_store.aggregate_path(run_id).exists());

    let execution = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", cook_id])
        .output()
        .expect("run queued finalization receipt continuation");
    assert_eq!(execution.status.code(), Some(0));
    let execution_envelope: Value = serde_json::from_slice(&execution.stdout).unwrap();
    assert_eq!(execution_envelope["data"]["status"], report["status"]);
    assert_eq!(
        continuation_state_in_store(&recipe_store, cook_id, run_id).expect("continuation state"),
        CookContinuationState::Completed
    );
    assert!(!lifecycle_store.aggregate_path(run_id).exists());
}

#[test]
fn malformed_queued_finalization_dispatcher_fails_preflight_and_execution() {
    use homeboy::agents::agent_task_service::{
        continuation_state_in_store, CookContinuationState, CookRecipeStore,
    };

    let cook_id = "public-malformed-finalization-dispatcher";
    let run_id = "public-malformed-finalization-dispatcher-attempt-1";
    let (context, lifecycle_store) = finalized_receipt_fixture(cook_id, run_id);
    let recipe_store = CookRecipeStore::new(context.path_roots());
    recipe_store
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
    // A rejected transport must not consume or advance the queued claim.
    assert_eq!(
        continuation_state_in_store(&recipe_store, cook_id, run_id).expect("continuation state"),
        CookContinuationState::Pending
    );
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

#[cfg(unix)]
#[test]
fn public_continuation_resumes_terminal_child_while_real_sibling_provider_remains_live() {
    use homeboy::agents::agent_task_batch::{
        persist_fanout_run_batch_in_store, AgentTaskBatchStore, FanoutRunBatchChild,
    };
    use homeboy::agents::agent_task_lifecycle::{AgentTaskLifecycleStore, AgentTaskRunState};
    use homeboy::agents::agent_task_service::{
        resolve_cook_continuation_run_id_in_store, CookRecipeStore,
    };
    use homeboy::core::test_support::bounded_output;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    let context = HermeticTestContext::new();
    let (_checkout_guard, checkout) =
        homeboy::core::test_support::shared_committed_git_repo_fixture("continue-wave-15503");
    std::fs::create_dir_all(checkout.join("docs")).unwrap();
    std::fs::create_dir_all(checkout.join("src")).unwrap();
    std::fs::create_dir_all(checkout.join("tests")).unwrap();
    std::fs::write(checkout.join("docs/agent-task-smoke.md"), "before\n").unwrap();
    let gate_open = context.root().join("gate-open");
    let gate_started = context.root().join("gate-started");
    let gate_count = context.root().join("gate-count");
    std::fs::write(checkout.join("Cargo.toml"), "[package]\nname = \"continuation-wave-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n").unwrap();
    std::fs::write(checkout.join("src/lib.rs"), "pub fn fixture() {}\n").unwrap();
    let gate_test = format!(
        "use std::{{fs, path::Path, thread, time::Duration}};\nconst OPEN: &str = {:?};\nconst STARTED: &str = {:?};\nconst COUNT: &str = {:?};\n#[test]\nfn retained_patch_passes_after_recovery_marker() {{\n let contents = include_str!(\"../docs/agent-task-smoke.md\").trim();\n if contents == \"after\" {{\n  assert!(Path::new(OPEN).exists(), \"recovery marker missing\");\n  let count_path = Path::new(COUNT);\n  let count = fs::read_to_string(count_path).ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0) + 1;\n  fs::write(count_path, count.to_string()).unwrap();\n  fs::write(STARTED, \"active\").unwrap();\n  thread::sleep(Duration::from_secs(3));\n }} else {{ assert_eq!(contents, \"before\"); }}\n}}\n",
        gate_open.display().to_string(),
        gate_started.display().to_string(),
        gate_count.display().to_string(),
    );
    std::fs::write(checkout.join("tests/retained_candidate.rs"), gate_test).unwrap();
    let lock = Command::new("cargo")
        .args(["generate-lockfile", "--offline"])
        .current_dir(&checkout)
        .output()
        .unwrap();
    assert!(
        lock.status.success(),
        "fixture lockfile: {}",
        String::from_utf8_lossy(&lock.stderr)
    );
    homeboy::core::test_support::run_git_fixture_command(&checkout, &["add", "."]);
    homeboy::core::test_support::run_git_fixture_command(
        &checkout,
        &["commit", "-m", "seed cook candidate"],
    );
    homeboy::core::test_support::run_git_fixture_command(
        &checkout,
        &[
            "remote",
            "add",
            "origin",
            checkout.to_str().expect("local origin path"),
        ],
    );

    let component_id = "continue-wave-15503";
    let mut register = context.command(TestBinary::HomeboyFixture);
    register.args([
        "component",
        "create",
        "--local-path",
        checkout.to_str().unwrap(),
    ]);
    let registered = bounded_output(register);
    assert!(
        registered.status.success(),
        "register: {}",
        String::from_utf8_lossy(&registered.stdout)
    );
    let create_worktree = |branch: &str| {
        let mut command = context.command(TestBinary::HomeboyFixture);
        command.args([
            "worktree",
            "create",
            component_id,
            "--branch",
            branch,
            "--from",
            "HEAD",
        ]);
        let output = bounded_output(command);
        assert!(
            output.status.success(),
            "create {branch} worktree: {}",
            String::from_utf8_lossy(&output.stdout)
        );
        checkout
            .parent()
            .unwrap()
            .join(format!("{component_id}@{branch}"))
    };
    let worktree = create_worktree("child");
    let sibling_worktree = create_worktree("sibling");
    let gate_sibling_worktree = create_worktree("gate-owner");

    let target_cook = "continue-wave-15503-terminal-child";
    let target_provider_started = context.root().join("target-provider-started");
    std::fs::write(&gate_open, "target gates admitted\n").unwrap();
    let provider_script = context.root().join("timeout-provider.js");
    std::fs::write(
        &provider_script,
        format!(
            "const fs=require('fs');const path=require('path');const req=JSON.parse(fs.readFileSync(0,'utf8'));fs.writeFileSync({:?},req.artifacts_path);const patch=path.join(req.artifacts_path,'changes.patch');fs.writeFileSync(patch,'diff --git a/docs/agent-task-smoke.md b/docs/agent-task-smoke.md\\n--- a/docs/agent-task-smoke.md\\n+++ b/docs/agent-task-smoke.md\\n@@ -1 +1 @@\\n-before\\n+after\\n');process.stdout.write(JSON.stringify({{schema:'homeboy/agent-task-outcome/v1',task_id:req.task_id,status:'timeout',summary:'fixture provider timed out after retaining its patch',failure_classification:'timeout',artifacts:[{{schema:'homeboy/agent-task-artifact/v1',id:'patch',kind:'patch',path:patch}}],diagnostics:[{{class:'agent_task.provider_timeout',message:'fixture provider timeout',data:{{timeout_ms:1000}}}}]}}));\n",
            target_provider_started.display().to_string()
        ),
    )
    .expect("write timeout provider");
    let runtime_dir = context
        .config_dir()
        .join("agent-runtimes/continuation-wave");
    std::fs::create_dir_all(&runtime_dir).expect("runtime manifest directory");
    std::fs::write(
        runtime_dir.join("continuation-wave.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema": "homeboy/agent-runtime-manifest/v1",
            "id": "continuation-wave",
            "agent_task_executors": [{
                "id": "continuation-wave-provider",
                "backend": "recoverable-fixture",
                "command_argv": ["node", provider_script.display().to_string()],
                "capabilities": ["structured_outcome"]
            }]
        }))
        .unwrap(),
    )
    .expect("write fixture runtime manifest");
    let mut target = context.controller_runtime_command(TestBinary::HomeboyFixture);
    target.env("HOMEBOY_TEST_LOAD_AVERAGES", "0,0,0").args([
        "--wait",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--run-id",
        target_cook,
        "--repo",
        component_id,
        "--backend",
        "recoverable-fixture",
        "--model",
        "fixture-model",
        "--prompt",
        "write the deterministic fixture patch",
        "--cwd",
        worktree.to_str().unwrap(),
        "--to-worktree",
        worktree.to_str().unwrap(),
        "--verify",
        "cargo test --locked -q",
        "--gate-environment-mode",
        "replace",
        "--gate-env",
        "PATH=/home/chubes/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:/usr/bin:/bin",
        "--gate-env",
        "HOME=/home/chubes/.nr/h15503",
        "--gate-env",
        "CARGO_HOME=/home/chubes/.cargo",
        "--gate-env",
        "CARGO_TARGET_DIR=/home/chubes/.nr/h15503/candidate-target",
        "--timeout-ms",
        "120000",
        "--max-attempts",
        "1",
        "--no-finalize",
    ]);
    let target_output = bounded_output(target);
    assert!(
        matches!(target_output.status.code(), Some(0 | 1)),
        "Cook completes its provider-timeout gate path: {}",
        String::from_utf8_lossy(&target_output.stdout)
    );
    assert!(
        target_provider_started.exists(),
        "fixture provider executed"
    );
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let recipe_store = CookRecipeStore::new(context.path_roots());
    let target_run =
        resolve_cook_continuation_run_id_in_store(&recipe_store, &lifecycle_store, target_cook)
            .expect("terminal attempt id");

    let sibling_cook = "continue-wave-15503-live-sibling";
    let sibling_started = context.root().join("sibling-provider-started");
    let sibling_stdout = context.root().join("sibling.stdout");
    let sibling_stderr = context.root().join("sibling.stderr");
    let mut sibling = context.controller_runtime_command(TestBinary::HomeboyFixture);
    sibling
        .env("HOMEBOY_FIXTURE_PROVIDER_STARTED_FILE", &sibling_started)
        .env("HOMEBOY_FIXTURE_PROVIDER_DELAY_MS", "120000")
        .args([
            "--wait",
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--run-id",
            sibling_cook,
            "--repo",
            component_id,
            "--backend",
            "fixture",
            "--model",
            "fixture-model",
            "--prompt",
            "keep this sibling provider live",
            "--cwd",
            sibling_worktree.to_str().unwrap(),
            "--to-worktree",
            sibling_worktree.to_str().unwrap(),
            "--verify",
            "true",
            "--max-attempts",
            "1",
            "--no-finalize",
        ]);
    sibling.stdout(Stdio::from(std::fs::File::create(&sibling_stdout).unwrap()));
    sibling.stderr(Stdio::from(std::fs::File::create(&sibling_stderr).unwrap()));
    let mut sibling = KillOnDrop(sibling.spawn().expect("start live sibling Cook"));
    let deadline = Instant::now() + Duration::from_secs(30);
    while !sibling_started.exists() && Instant::now() < deadline {
        assert!(
            sibling.0.try_wait().unwrap().is_none(),
            "sibling exited: {}",
            std::fs::read_to_string(&sibling_stderr).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert!(sibling_started.exists(), "sibling provider started");
    let sibling_run =
        resolve_cook_continuation_run_id_in_store(&recipe_store, &lifecycle_store, sibling_cook)
            .expect("live sibling attempt id");
    let sibling_record = lifecycle_store.read_record(&sibling_run).unwrap();
    let sibling_provider_owner = sibling_record.metadata["provider_executions"][0]["owner_pid"]
        .as_u64()
        .expect("durable provider owner") as u32;
    assert!(
        homeboy::core::process::pid_is_running(sibling_provider_owner),
        "same-child provider process remains alive"
    );

    let gate_sibling_cook = "continue-wave-15503-live-gate-sibling";
    let gate_sibling_stdout = context.root().join("gate-sibling.stdout");
    let gate_sibling_stderr = context.root().join("gate-sibling.stderr");
    let _ = std::fs::remove_file(&gate_started);
    let mut gate_sibling_command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    gate_sibling_command.args([
        "--wait",
        "--placement",
        "local",
        "agent-task",
        "cook",
        "--run-id",
        gate_sibling_cook,
        "--repo",
        component_id,
        "--backend",
        "fixture",
        "--model",
        "fixture-model",
        "--prompt",
        "exercise a live native cargo gate",
        "--cwd",
        gate_sibling_worktree.to_str().unwrap(),
        "--to-worktree",
        gate_sibling_worktree.to_str().unwrap(),
        "--verify",
        "cargo test --locked -q",
        "--gate-environment-mode",
        "replace",
        "--gate-env",
        "PATH=/home/chubes/.rustup/toolchains/stable-x86_64-unknown-linux-gnu/bin:/usr/bin:/bin",
        "--gate-env",
        "HOME=/home/chubes/.nr/h15503",
        "--gate-env",
        "RUSTUP_HOME=/home/chubes/.rustup",
        "--gate-env",
        "CARGO_TARGET_DIR=/home/chubes/.nr/h15503/candidate-target",
        "--max-attempts",
        "1",
        "--no-finalize",
    ]);
    gate_sibling_command.stdout(Stdio::from(
        std::fs::File::create(&gate_sibling_stdout).unwrap(),
    ));
    gate_sibling_command.stderr(Stdio::from(
        std::fs::File::create(&gate_sibling_stderr).unwrap(),
    ));
    let mut gate_sibling = KillOnDrop(gate_sibling_command.spawn().expect("start gate-owner Cook"));
    let gate_start_deadline = Instant::now() + Duration::from_secs(30);
    while !gate_started.exists() && Instant::now() < gate_start_deadline {
        assert!(
            gate_sibling.0.try_wait().unwrap().is_none(),
            "gate-owner Cook exited early; gate receipt={:?}; stdout={} stderr={}",
            resolve_cook_continuation_run_id_in_store(
                &recipe_store,
                &lifecycle_store,
                gate_sibling_cook,
            )
            .ok()
            .and_then(|run| lifecycle_store.read_record(&run).ok())
            .map(|record| record.metadata["latest_promotion"]["gate_results"].clone()),
            std::fs::read_to_string(&gate_sibling_stdout).unwrap_or_default(),
            std::fs::read_to_string(&gate_sibling_stderr).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert!(gate_started.exists(), "real child-owned cargo gate started");
    let gate_sibling_run = resolve_cook_continuation_run_id_in_store(
        &recipe_store,
        &lifecycle_store,
        gate_sibling_cook,
    )
    .expect("live gate-owner attempt id");
    let gate_sibling_record = lifecycle_store.read_record(&gate_sibling_run).unwrap();
    let gate_owner_pid = gate_sibling_record.metadata["promotion_progress"]["owner_pid"]
        .as_u64()
        .expect("durable gate promotion owner") as u32;
    assert_eq!(
        gate_sibling_record.metadata["promotion_progress"]["active"],
        true
    );
    assert!(
        homeboy::core::process::pid_is_running(gate_owner_pid),
        "same-child gate/promotion owner process remains alive"
    );

    let target_record = lifecycle_store.read_record(&target_run).unwrap();
    let aggregate = lifecycle_store
        .read_aggregate(&target_run)
        .expect("durable terminal aggregate");
    assert!(
        matches!(
            target_record.state,
            AgentTaskRunState::Succeeded
                | AgentTaskRunState::CandidateRecoverable
                | AgentTaskRunState::PartialRecoverable
        ),
        "record state {:?}; Cook output: {}",
        target_record.state,
        String::from_utf8_lossy(&target_output.stdout),
    );
    assert!(
        String::from_utf8_lossy(&target_output.stdout)
            .contains("\"run_state\": \"PartialRecoverable\""),
        "Cook result retained its recoverable terminal classification"
    );
    assert!(target_record.metadata["provider_executions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|execution| execution["state"] != "running"));
    let retained_patch = aggregate
        .outcomes
        .iter()
        .flat_map(|outcome| &outcome.artifacts)
        .find(|artifact| artifact.kind == "patch")
        .and_then(|artifact| artifact.path.as_deref())
        .unwrap();
    assert!(
        std::fs::metadata(retained_patch).unwrap().len() > 0,
        "substantive patch is retained"
    );
    assert_eq!(
        std::fs::read_to_string(worktree.join("docs/agent-task-smoke.md")).unwrap(),
        "after\n",
        "timed-out provider candidate is retained and promoted into its native worktree"
    );

    let batch_store = AgentTaskBatchStore::new(context.path_roots());
    persist_fanout_run_batch_in_store(
        &batch_store,
        "continue-wave-15503-batch",
        "continue-wave-15503-batch",
        &[
            FanoutRunBatchChild {
                task_id: target_cook.to_string(),
                run_id: target_run.clone(),
            },
            FanoutRunBatchChild {
                task_id: sibling_cook.to_string(),
                run_id: sibling_run.clone(),
            },
            FanoutRunBatchChild {
                task_id: gate_sibling_cook.to_string(),
                run_id: gate_sibling_run.clone(),
            },
        ],
        serde_json::json!({}),
    )
    .unwrap();
    let claim = batch_store
        .claim_fanout_run_batch("continue-wave-15503-batch")
        .unwrap()
        .unwrap();
    batch_store
        .mutate_batch("continue-wave-15503-batch", |batch| {
            batch.metadata["coordinator"]["stage"] = serde_json::json!("running");
            batch.state = homeboy::agents::agent_task_batch::AgentTaskBatchState::Running;
            for child in &mut batch.child_runs {
                // Cook status tracks the recoverable candidate while the
                // lifecycle run records successful delivery of its timeout
                // response from the fixture runtime.
                child.state = if child.run_id == target_run {
                    AgentTaskRunState::PartialRecoverable
                } else {
                    AgentTaskRunState::Running
                };
            }
            Ok(())
        })
        .expect("keep claimed parallel wave running");
    assert_eq!(claim.len(), 36, "durable coordinator claim is retained");
    lifecycle_store
        .mutate_record(&target_run, |record| {
            // The test process claimed the durable batch coordinator above;
            // keep it live as the original parent owner. Reopen only this
            // just-terminalized Cook continuation so the public CLI exercises
            // the same pre-claim boundary as an interrupted batch child.
            record.metadata["runner_pid"] = serde_json::json!(std::process::id());
            record
                .metadata
                .as_object_mut()
                .expect("run metadata object")
                .remove("cook_continuation");
            true
        })
        .unwrap();
    assert_eq!(
        homeboy::agents::agent_task_service::continuation_state_in_store(
            &recipe_store,
            target_cook,
            &target_run,
        )
        .unwrap(),
        homeboy::agents::agent_task_service::CookContinuationState::Absent,
        "target child is staged at the pre-continuation boundary"
    );

    let preflight = |run_id: &str| {
        context
            .command(TestBinary::HomeboyFixture)
            .args(["agent-task", "cook-continue", run_id, "--preflight"])
            .output()
            .unwrap()
    };
    let sibling_preflight = preflight(&sibling_run);
    assert_eq!(
        sibling_preflight.status.code(),
        Some(1),
        "same-child live provider must remain fenced"
    );
    let sibling_report: Value = serde_json::from_slice(&sibling_preflight.stdout).unwrap();
    assert_eq!(
        sibling_report["data"]["failure_context"]["diagnostic"]["details"]
            ["continuation_admission"]["first_authoritative_denial"],
        "live_owner_in_progress"
    );
    assert_eq!(
        sibling_report["data"]["failure_context"]["diagnostic"]["details"]
            ["continuation_admission"]["owner_pid"],
        sibling_provider_owner,
        "denial is tied to this child's live provider PID"
    );
    assert!(sibling.0.try_wait().unwrap().is_none());
    let gate_owner_preflight = preflight(&gate_sibling_run);
    assert_eq!(
        gate_owner_preflight.status.code(),
        Some(1),
        "live child-owned gate process remains fenced"
    );
    let gate_owner_report: Value = serde_json::from_slice(&gate_owner_preflight.stdout).unwrap();
    assert_eq!(
        gate_owner_report["data"]["failure_context"]["diagnostic"]["details"]
            ["continuation_admission"]["phase"],
        "gate",
        "live gate-owner denial must name its phase: {gate_owner_report}"
    );
    assert!(gate_sibling.0.try_wait().unwrap().is_none());
    let target_preflight = preflight(&target_run);
    let target_report: Value = serde_json::from_slice(&target_preflight.stdout).unwrap();
    assert_eq!(
        target_preflight.status.code(),
        Some(0),
        "terminal child must pass with coordinator and sibling live: {target_report}"
    );
    assert_eq!(target_report["data"]["admitted"], true);
    assert!(sibling.0.try_wait().unwrap().is_none());

    let continued = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", &target_run])
        .output()
        .expect("continue terminal child while both siblings remain live");
    assert!(
        continued.status.success(),
        "public continuation: {}",
        String::from_utf8_lossy(&continued.stdout)
    );
    let provider_count = lifecycle_store.read_record(&target_run).unwrap().metadata
        ["provider_executions"]
        .as_array()
        .unwrap()
        .len();
    let gate_count_after_continue = std::fs::read_to_string(&gate_count).unwrap();
    let replay = context
        .command(TestBinary::HomeboyFixture)
        .args(["agent-task", "cook-continue", &target_run])
        .output()
        .unwrap();
    assert!(
        replay.status.success(),
        "idempotent public replay: {}",
        String::from_utf8_lossy(&replay.stdout)
    );
    assert_eq!(
        lifecycle_store.read_record(&target_run).unwrap().metadata["provider_executions"]
            .as_array()
            .unwrap()
            .len(),
        provider_count
    );
    assert_eq!(
        std::fs::read_to_string(&gate_count).unwrap(),
        gate_count_after_continue,
        "idempotent replay must not repeat gates"
    );
    assert!(
        sibling.0.try_wait().unwrap().is_none(),
        "unrelated sibling remains live after continuation"
    );
    sibling.0.kill().unwrap();
    let _ = sibling.0.wait();
}
