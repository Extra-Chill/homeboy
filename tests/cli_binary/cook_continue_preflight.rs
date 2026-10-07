use homeboy::core::test_support::{HermeticTestContext, TestBinary};
use serde_json::Value;

#[cfg(target_os = "linux")]
struct KillOnDrop(std::process::Child);

#[cfg(target_os = "linux")]
impl KillOnDrop {
    fn terminate_tree(&mut self) -> homeboy::core::process::ProcessTreeTermination {
        homeboy::core::process::terminate_process_tree(self.0.id())
            .expect("owned process tree terminates within its bounded grace period")
    }
}

#[cfg(target_os = "linux")]
impl Drop for KillOnDrop {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.terminate_tree();
        }
    }
}

#[cfg(target_os = "linux")]
struct HermeticSiblingDaemonGuard<'a> {
    context: &'a HermeticTestContext,
    state_dir: std::path::PathBuf,
    active: bool,
}

#[cfg(target_os = "linux")]
impl HermeticSiblingDaemonGuard<'_> {
    fn stop(&mut self) {
        if !self.active {
            return;
        }
        let mut command = self.context.command(TestBinary::HomeboyFixture);
        command
            .env(homeboy::core::paths::DAEMON_STATE_DIR_ENV, &self.state_dir)
            .env("HOMEBOY_TEST_DAEMON_NAMESPACE", &self.state_dir)
            .args(["daemon", "stop"])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let _ = homeboy::core::test_support::bounded_output(command);
        self.active = false;
    }
}

#[cfg(target_os = "linux")]
impl Drop for HermeticSiblingDaemonGuard<'_> {
    fn drop(&mut self) {
        self.stop();
    }
}

#[cfg(target_os = "linux")]
fn linux_process_snapshot(pid: u32) -> serde_json::Value {
    let root = std::path::PathBuf::from(format!("/proc/{pid}"));
    let stat = std::fs::read_to_string(root.join("stat")).ok();
    let (state, parent_pid, process_group_id, session_id, starttime_ticks) = stat
        .as_deref()
        .and_then(|stat| stat.rsplit_once(')'))
        .map(|(_, remainder)| {
            let fields = remainder.split_whitespace().collect::<Vec<_>>();
            (
                fields.first().copied(),
                fields.get(1).and_then(|value| value.parse::<u32>().ok()),
                fields.get(2).and_then(|value| value.parse::<u32>().ok()),
                fields.get(3).and_then(|value| value.parse::<u32>().ok()),
                fields.get(19).and_then(|value| value.parse::<u64>().ok()),
            )
        })
        .unwrap_or((None, None, None, None, None));
    let command_line = std::fs::read(root.join("cmdline")).ok().map(|bytes| {
        String::from_utf8_lossy(&bytes)
            .split('\0')
            .filter(|part| !part.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    serde_json::json!({
        "pid": pid,
        "state": state,
        "parent_pid": parent_pid,
        "process_group_id": process_group_id,
        "session_id": session_id,
        "starttime_ticks": starttime_ticks,
        "command_line": command_line,
    })
}

#[cfg(target_os = "linux")]
fn linux_descendant_snapshot(root_pid: u32) -> serde_json::Value {
    let Ok(output) = std::process::Command::new("ps")
        .args(["-eo", "pid=,ppid=,pgid=,stat=,comm="])
        .output()
    else {
        return serde_json::json!({ "error": "ps could not be started" });
    };
    let rows = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((
                fields.next()?.parse::<u32>().ok()?,
                fields.next()?.parse::<u32>().ok()?,
                fields.next()?.parse::<u32>().ok()?,
                fields.next()?.to_string(),
                fields.next()?.to_string(),
            ))
        })
        .collect::<Vec<_>>();
    let mut selected = vec![root_pid];
    let mut selected_groups = Vec::new();
    loop {
        let mut changed = false;
        for (pid, parent, group, _, _) in &rows {
            if (selected.contains(parent) || selected_groups.contains(group))
                && !selected.contains(pid)
            {
                selected.push(*pid);
                selected_groups.push(*group);
                changed = true;
            }
            if selected.contains(pid) && !selected_groups.contains(group) {
                selected_groups.push(*group);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    serde_json::Value::Array(
        rows.iter()
            .filter(|(pid, _, group, _, _)| {
                selected.contains(pid) || selected_groups.contains(group)
            })
            .map(|(pid, parent, group, state, command)| {
                serde_json::json!({
                    "pid": pid,
                    "parent_pid": parent,
                    "process_group_id": group,
                    "state": state,
                    "command": command,
                })
            })
            .collect(),
    )
}

#[cfg(target_os = "linux")]
fn linux_lock_file_holders(lock_path: &std::path::Path) -> serde_json::Value {
    let lock_path = std::fs::canonicalize(lock_path).unwrap_or_else(|_| lock_path.to_path_buf());
    let mut holders = Vec::new();
    let Ok(processes) = std::fs::read_dir("/proc") else {
        return serde_json::json!({ "error": "cannot read /proc" });
    };
    for process in processes.flatten() {
        let Some(pid) = process
            .file_name()
            .to_str()
            .and_then(|value| value.parse::<u32>().ok())
        else {
            continue;
        };
        let Ok(descriptors) = std::fs::read_dir(process.path().join("fd")) else {
            continue;
        };
        let matching_descriptors = descriptors
            .flatten()
            .filter_map(|descriptor| std::fs::read_link(descriptor.path()).ok())
            .filter(|path| path == &lock_path)
            .collect::<Vec<_>>();
        if !matching_descriptors.is_empty() {
            holders.push(serde_json::json!({
                "pid": pid,
                "process": linux_process_snapshot(pid),
                "descriptor_count": matching_descriptors.len(),
            }));
        }
    }
    serde_json::Value::Array(holders)
}

#[cfg(target_os = "linux")]
fn write_same_child_evidence(name: &str, value: &serde_json::Value) {
    let Some(directory) = std::env::var_os("HOMEBOY_SAME_CHILD_EVIDENCE_DIR") else {
        return;
    };
    let directory = std::path::PathBuf::from(directory);
    std::fs::create_dir_all(&directory).expect("create same-child evidence directory");
    std::fs::write(
        directory.join(name),
        serde_json::to_vec_pretty(value).expect("serialize same-child evidence"),
    )
    .expect("persist same-child evidence");
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

#[cfg(target_os = "linux")]
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
    use std::os::unix::process::CommandExt;
    use std::process::{Command, Stdio};
    use std::thread;
    use std::time::{Duration, Instant};

    let context = HermeticTestContext::new();
    let _daemon_guard =
        homeboy::core::test_support::HermeticDaemonGuard::new(&context, TestBinary::HomeboyFixture);
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
        "use std::{{fs, path::Path, thread, time::{{Duration, Instant}}}};\nconst OPEN: &str = {:?};\nconst STARTED: &str = {:?};\nconst COUNT: &str = {:?};\n#[test]\nfn retained_patch_passes_after_recovery_marker() {{\n let contents = include_str!(\"../docs/agent-task-smoke.md\").trim();\n if contents == \"after\" {{\n  fs::write(STARTED, \"active\").unwrap();\n  let deadline = Instant::now() + Duration::from_secs(60);\n  while !Path::new(OPEN).exists() && Instant::now() < deadline {{ thread::sleep(Duration::from_millis(10)); }}\n  assert!(Path::new(OPEN).exists(), \"recovery marker missing\");\n  let count_path = Path::new(COUNT);\n  let count = fs::read_to_string(count_path).ok().and_then(|s| s.parse::<u32>().ok()).unwrap_or(0) + 1;\n  fs::write(count_path, count.to_string()).unwrap();\n  thread::sleep(Duration::from_secs(3));\n }} else {{ assert_eq!(contents, \"before\"); }}\n}}\n",
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

    let target_cook = "continue-wave-15503-terminal-child";
    let target_provider_started = context.root().join("target-provider-started");
    let lifecycle_store = AgentTaskLifecycleStore::new(context.path_roots());
    let recipe_store = CookRecipeStore::new(context.path_roots());
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
    let host_home = std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.root().to_path_buf());
    let gate_path = std::env::var("PATH").expect("test process PATH");
    let gate_cargo_home = std::env::var_os("CARGO_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| host_home.join(".cargo"));
    let gate_rustup_home = std::env::var_os("RUSTUP_HOME")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| host_home.join(".rustup"));
    let gate_cargo_target = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| context.root().join("gate-target"));
    let gate_environment = [
        ("PATH", gate_path),
        ("CARGO_HOME", gate_cargo_home.display().to_string()),
        ("RUSTUP_HOME", gate_rustup_home.display().to_string()),
        ("CARGO_TARGET_DIR", gate_cargo_target.display().to_string()),
    ]
    .into_iter()
    .flat_map(|(name, value)| ["--gate-env".to_string(), format!("{name}={value}")])
    .collect::<Vec<_>>();
    let mut target_command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    target_command
        .env("HOMEBOY_TEST_LOAD_AVERAGES", "0,0,0")
        .args([
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
        ]);
    target_command.args(gate_environment);
    target_command.args([
        "--timeout-ms",
        "120000",
        "--max-attempts",
        "1",
        "--no-finalize",
    ]);
    let target_stdout = context.root().join("target.stdout");
    let target_stderr = context.root().join("target.stderr");
    target_command.stdout(Stdio::from(std::fs::File::create(&target_stdout).unwrap()));
    target_command.stderr(Stdio::from(std::fs::File::create(&target_stderr).unwrap()));
    target_command.process_group(0);
    let mut target = KillOnDrop(target_command.spawn().expect("start terminalizing Cook"));
    let target_deadline = Instant::now() + Duration::from_secs(90);
    let mut target_run = None;
    let mut interrupted_gate_owner_pid = None;
    while Instant::now() < target_deadline {
        if let Ok(run_id) =
            resolve_cook_continuation_run_id_in_store(&recipe_store, &lifecycle_store, target_cook)
        {
            if let Ok(record) = lifecycle_store.read_record(&run_id) {
                if record.metadata["promotion_progress"]["phase"] == "gate"
                    && record.metadata["promotion_progress"]["active"] == true
                    && gate_started.exists()
                {
                    interrupted_gate_owner_pid = record.metadata["promotion_progress"]["owner_pid"]
                        .as_u64()
                        .map(|pid| pid as u32);
                    target_run = Some(run_id);
                    break;
                }
            }
        }
        assert!(
            target.0.try_wait().unwrap().is_none(),
            "target Cook exited before reaching its real gate; stdout={} stderr={}",
            std::fs::read_to_string(&target_stdout).unwrap_or_default(),
            std::fs::read_to_string(&target_stderr).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(10));
    }
    let target_run = target_run.expect("terminal child reached its live Cargo gate");
    let interrupted_gate_owner_pid =
        interrupted_gate_owner_pid.expect("gate progress names its owner PID");
    let target_pid = target.0.id();
    assert!(homeboy::core::process::pid_is_running(
        interrupted_gate_owner_pid
    ));
    let gate_owner_before = linux_process_snapshot(interrupted_gate_owner_pid);
    let target_descendants_before = linux_descendant_snapshot(target_pid);
    let target_record_before_kill = lifecycle_store.read_record(&target_run).unwrap();
    assert!(target_record_before_kill.state.is_terminal());
    assert_eq!(
        target_record_before_kill.metadata["promotion_progress"]["owner_pid"],
        interrupted_gate_owner_pid
    );
    let driver_lock = recipe_store
        .data_root()
        .join("agent-task-cooks")
        .join(target_cook)
        .join("driver.lock");
    let driver_lock_content_before = std::fs::read_to_string(&driver_lock).ok();
    let driver_lock_holders_before = linux_lock_file_holders(&driver_lock);
    write_same_child_evidence(
        "same-child-gate-before-kill.json",
        &serde_json::json!({
            "target_pid": target_pid,
            "target_process_group": linux_process_snapshot(target_pid),
            "target_descendants_and_group": target_descendants_before,
            "gate_owner_pid": interrupted_gate_owner_pid,
            "gate_owner": gate_owner_before,
            "durable_record": target_record_before_kill,
            "driver_lock_path": driver_lock,
            "driver_lock_content": driver_lock_content_before,
            "driver_lock_holders": driver_lock_holders_before,
        }),
    );
    // Give the sibling an independent local-dispatch admission root so the
    // target's lease cannot consume its provider slot on small CI hosts. Keep
    // its durable Cook/lifecycle stores shared with the target: the two real
    // providers must still be children in the same durable batch below.
    let sibling_cook = "continue-wave-15503-live-sibling";
    let sibling_started = context.root().join("sibling-provider-started");
    let sibling_stdout = context.root().join("sibling.stdout");
    let sibling_stderr = context.root().join("sibling.stderr");
    let sibling_daemon_root = context.root().join("sibling-daemon");
    let sibling_admission_root = context.root().join("sibling-admission");
    std::fs::create_dir_all(&sibling_daemon_root).unwrap();
    std::fs::create_dir_all(&sibling_admission_root).unwrap();
    let target_lease_root =
        homeboy::core::paths::local_cook_dispatch_leases_dir_in_root(&context.data_dir());
    let sibling_lease_root =
        homeboy::core::paths::local_cook_dispatch_leases_dir_in_root(&sibling_admission_root);
    assert_ne!(target_lease_root, sibling_lease_root);
    write_same_child_evidence(
        "same-child-admission-roots.json",
        &serde_json::json!({
            "data_root": context.data_dir(),
            "lifecycle_root": context.data_dir().join("agent-task-runs"),
            "cook_root": context.data_dir().join("agent-task-cooks"),
            "target_lease_root": target_lease_root,
            "sibling_lease_root": sibling_lease_root,
            "target_daemon_root": context.daemon_dir(),
            "sibling_daemon_root": sibling_daemon_root,
        }),
    );
    let mut sibling_daemon_guard = HermeticSiblingDaemonGuard {
        context: &context,
        state_dir: sibling_daemon_root.clone(),
        active: true,
    };
    let mut sibling_command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    sibling_command
        .env(
            homeboy::core::local_dispatch_admission::TEST_LOCAL_DISPATCH_LEASE_ROOT_ENV,
            &sibling_admission_root,
        )
        .env(
            homeboy::core::paths::DAEMON_STATE_DIR_ENV,
            &sibling_daemon_root,
        )
        .env("HOMEBOY_TEST_DAEMON_NAMESPACE", &sibling_daemon_root)
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
    sibling_command.stdout(Stdio::from(std::fs::File::create(&sibling_stdout).unwrap()));
    sibling_command.stderr(Stdio::from(std::fs::File::create(&sibling_stderr).unwrap()));
    let mut sibling = KillOnDrop(sibling_command.spawn().expect("start live sibling Cook"));
    let sibling_deadline = Instant::now() + Duration::from_secs(90);
    while !sibling_started.exists() && Instant::now() < sibling_deadline {
        assert!(
            sibling.0.try_wait().unwrap().is_none(),
            "sibling exited: {}",
            std::fs::read_to_string(&sibling_stderr).unwrap_or_default()
        );
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        sibling_started.exists(),
        "sibling provider starts while the target still owns its real gate; sibling state={:?}; stderr={} stdout={}",
        sibling.0.try_wait().unwrap(),
        std::fs::read_to_string(&sibling_stderr).unwrap_or_default(),
        std::fs::read_to_string(&sibling_stdout).unwrap_or_default()
    );
    let sibling_run =
        resolve_cook_continuation_run_id_in_store(&recipe_store, &lifecycle_store, sibling_cook)
            .expect("live sibling attempt id");
    let sibling_record = lifecycle_store.read_record(&sibling_run).unwrap();
    let sibling_provider_owner = sibling_record.metadata["provider_executions"][0]["owner_pid"]
        .as_u64()
        .expect("durable provider owner") as u32;
    assert!(homeboy::core::process::pid_is_running(
        sibling_provider_owner
    ));
    // Remove the first gate's marker before killing its owner. Removing it
    // after termination races the daemon's automatic recovery gate, whose
    // fresh marker is the handoff signal this test waits for below.
    std::fs::remove_file(&gate_started).unwrap();
    let termination = target.terminate_tree();
    let reap_deadline = Instant::now() + Duration::from_secs(5);
    while homeboy::core::process::pid_is_running(interrupted_gate_owner_pid)
        && Instant::now() < reap_deadline
    {
        thread::sleep(Duration::from_millis(20));
    }
    let gate_owner_after = linux_process_snapshot(interrupted_gate_owner_pid);
    let target_descendants_after = linux_descendant_snapshot(target_pid);
    let driver_lock_content_after = std::fs::read_to_string(&driver_lock).ok();
    let driver_lock_holders_after = linux_lock_file_holders(&driver_lock);
    let termination_evidence = serde_json::json!({
        "termination": format!("{termination:?}"),
        "gate_owner_pid": interrupted_gate_owner_pid,
        "gate_owner_running_after": homeboy::core::process::pid_is_running(interrupted_gate_owner_pid),
        "gate_owner_after": gate_owner_after,
        "target_descendants_and_group_after": target_descendants_after,
        "driver_lock_content_after": driver_lock_content_after,
        "driver_lock_holders_after": driver_lock_holders_after,
    });
    write_same_child_evidence("same-child-gate-after-kill.json", &termination_evidence);
    assert!(
        !homeboy::core::process::pid_is_running(interrupted_gate_owner_pid),
        "interrupted target gate owner {interrupted_gate_owner_pid} is reaped; evidence={termination_evidence}"
    );
    assert!(
        target_provider_started.exists(),
        "fixture provider executed"
    );

    let target_record = lifecycle_store.read_record(&target_run).unwrap();
    let aggregate = lifecycle_store
        .read_aggregate(&target_run)
        .expect("durable terminal aggregate");
    assert!(target_record.state.is_terminal());
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
    assert!(
        aggregate.outcomes.iter().any(|outcome| {
            outcome
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.class == "agent_task.provider_timeout")
        }),
        "the actual child result retains its provider-timeout classification"
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
            batch.metadata["coordinator"]["owner_pid"] = serde_json::json!(std::process::id());
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
    let batch_record = batch_store
        .read_batch_record("continue-wave-15503-batch")
        .expect("read persisted batch");
    let batch_child_runs = batch_record
        .child_runs
        .iter()
        .map(|child| child.run_id.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        batch_child_runs,
        [target_run.as_str(), sibling_run.as_str()]
    );
    assert_eq!(
        batch_record.metadata["coordinator"]["owner_pid"],
        std::process::id()
    );
    lifecycle_store
        .mutate_record(&target_run, |record| {
            // Keep the batch coordinator live as the generic parent PID while
            // preserving the real queued continuation and gate checkpoint.
            record.metadata["runner_pid"] = serde_json::json!(std::process::id());
            true
        })
        .unwrap();

    let preflight = |run_id: &str, daemon_root: Option<&std::path::Path>| {
        let mut command = context.command(TestBinary::HomeboyFixture);
        if let Some(daemon_root) = daemon_root {
            command
                .env(homeboy::core::paths::DAEMON_STATE_DIR_ENV, daemon_root)
                .env("HOMEBOY_TEST_DAEMON_NAMESPACE", daemon_root);
        }
        command
            .args(["agent-task", "cook-continue", run_id, "--preflight"])
            .output()
            .unwrap()
    };
    let sibling_preflight = preflight(&sibling_run, Some(&sibling_daemon_root));
    assert_eq!(
        sibling_preflight.status.code(),
        Some(1),
        "same-child live provider must remain fenced"
    );
    let sibling_report: Value = serde_json::from_slice(&sibling_preflight.stdout).unwrap();
    assert_eq!(
        sibling_report["data"]["failure_context"]["diagnostic"]["details"]
            ["continuation_admission"]["first_authoritative_denial"],
        "live_owner_in_progress",
        "sibling preflight remains fenced by its own live provider: {sibling_report:#}"
    );
    assert_eq!(
        sibling_report["data"]["failure_context"]["diagnostic"]["details"]
            ["continuation_admission"]["owner_pid"],
        sibling_provider_owner,
        "denial is tied to this child's live provider PID"
    );
    assert!(sibling.0.try_wait().unwrap().is_none());

    let recovery_deadline = Instant::now() + Duration::from_secs(90);
    let resumed_gate_owner_pid = loop {
        let record = lifecycle_store.read_record(&target_run).unwrap();
        let lock_owner_pid = std::fs::read_to_string(&driver_lock)
            .ok()
            .and_then(|owner| owner.trim().parse::<u32>().ok());
        let lock_holders = linux_lock_file_holders(&driver_lock);
        if gate_started.exists()
            && record.metadata["promotion_progress"]["phase"] == "gate"
            && record.metadata["promotion_progress"]["active"] == true
            && lock_owner_pid.is_some_and(|pid| pid != interrupted_gate_owner_pid)
            && lock_holders.as_array().is_some_and(|holders| {
                holders
                    .iter()
                    .any(|holder| holder["pid"] == lock_owner_pid.unwrap())
            })
        {
            let owner_pid = lock_owner_pid.unwrap();
            if homeboy::core::process::pid_is_running(owner_pid) {
                break owner_pid;
            }
        }
        assert!(
            sibling.0.try_wait().unwrap().is_none(),
            "live provider sibling exits while daemon recovers the target"
        );
        if Instant::now() >= recovery_deadline {
            let queued_owner_pid = record.metadata["cook_continuation"]["owner_pid"]
                .as_u64()
                .and_then(|pid| u32::try_from(pid).ok());
            let timeout_evidence = serde_json::json!({
                "interrupted_gate_owner_pid": interrupted_gate_owner_pid,
                "interrupted_gate_owner": linux_process_snapshot(interrupted_gate_owner_pid),
                "record_promotion_owner_pid": record.metadata["promotion_progress"]["owner_pid"],
                "record_promotion_owner": record.metadata["promotion_progress"]["owner_pid"]
                    .as_u64()
                    .and_then(|pid| u32::try_from(pid).ok())
                    .map(linux_process_snapshot),
                "queued_continuation_owner_pid": queued_owner_pid,
                "queued_continuation_owner": queued_owner_pid.map(linux_process_snapshot),
                "target_processes": linux_descendant_snapshot(target_pid),
                "record": record,
                "driver_lock_content": std::fs::read_to_string(&driver_lock).ok(),
                "driver_lock_holders": linux_lock_file_holders(&driver_lock),
                "sibling_provider_pid": sibling_provider_owner,
                "sibling_provider": linux_process_snapshot(sibling_provider_owner),
                "sibling_still_running": sibling.0.try_wait().unwrap().is_none(),
                "target_stderr": std::fs::read_to_string(&target_stderr).unwrap_or_default(),
            });
            write_same_child_evidence("same-child-recovery-timeout.json", &timeout_evidence);
            panic!("queued same-child continuation did not reacquire its real gate: {timeout_evidence}");
        }
        thread::sleep(Duration::from_millis(20));
    };
    let resumed_record = lifecycle_store.read_record(&target_run).unwrap();
    let recovery_evidence = serde_json::json!({
        "batch_id": "continue-wave-15503-batch",
        "batch_child_run_ids": batch_child_runs,
        "batch_coordinator_pid": std::process::id(),
        "interrupted_gate_owner_pid": interrupted_gate_owner_pid,
        "interrupted_gate_owner_running": homeboy::core::process::pid_is_running(interrupted_gate_owner_pid),
        "resumed_gate_owner_pid": resumed_gate_owner_pid,
        "resumed_gate_owner": linux_process_snapshot(resumed_gate_owner_pid),
        "resumed_promotion_progress": resumed_record.metadata["promotion_progress"],
        "cook_continuation": resumed_record.metadata["cook_continuation"],
        "cook_continuation_scheduler": resumed_record.metadata["cook_continuation_scheduler"],
        "cook_operation_claims": resumed_record.metadata["cook_operation_claims"],
        "target_driver_lock_content": std::fs::read_to_string(&driver_lock).ok(),
        "target_driver_lock_holders": linux_lock_file_holders(&driver_lock),
        "sibling_provider_pid": sibling_provider_owner,
        "sibling_provider": linux_process_snapshot(sibling_provider_owner),
    });
    write_same_child_evidence("same-child-scheduler-handoff.json", &recovery_evidence);

    let target_preflight = preflight(&target_run, None);
    let target_report: Value = serde_json::from_slice(&target_preflight.stdout).unwrap();
    write_same_child_evidence(
        "same-child-active-gate-preflight.json",
        &serde_json::json!({
            "exit_code": target_preflight.status.code(),
            "report": target_report,
            "expected_gate_owner_pid": resumed_gate_owner_pid,
            "recovery_evidence": recovery_evidence,
            "target_cook_id": target_cook,
            "target_run_id": target_run,
            "sibling_cook_id": sibling_cook,
            "sibling_run_id": sibling_run,
            "sibling_runner_pid": sibling_record.metadata["runner_pid"],
            "sibling_provider_owner_pid": sibling_provider_owner,
            "target_driver_lock_content": std::fs::read_to_string(&driver_lock).ok(),
            "target_driver_lock_holders": linux_lock_file_holders(&driver_lock),
        }),
    );
    assert_eq!(
        target_preflight.status.code(),
        Some(1),
        "a new live owner of this exact child's gate must remain fenced: {target_report}"
    );
    assert_eq!(
        target_report["data"]["failure_context"]["diagnostic"]["details"]["continuation_admission"]
            ["owner_pid"],
        resumed_gate_owner_pid,
        "denial is tied to the resumed exact-child gate owner"
    );
    assert_eq!(
        target_report["data"]["failure_context"]["diagnostic"]["details"]["continuation_admission"]
            ["phase"],
        "gate"
    );

    std::fs::write(&gate_open, "continue").unwrap();
    let continuation_deadline = Instant::now() + Duration::from_secs(90);
    let mut completed_record = None;
    while Instant::now() < continuation_deadline {
        let record = lifecycle_store.read_record(&target_run).unwrap();
        if std::fs::read_to_string(&gate_count).is_ok_and(|count| count == "1")
            && homeboy::agents::agent_task_service::continuation_state_in_store(
                &recipe_store,
                target_cook,
                &target_run,
            )
            .is_ok_and(|state| {
                state == homeboy::agents::agent_task_service::CookContinuationState::Completed
            })
        {
            completed_record = Some(record);
            break;
        }
        assert!(
            sibling.0.try_wait().unwrap().is_none(),
            "live sibling provider exits during same-child continuation recovery"
        );
        thread::sleep(Duration::from_millis(20));
    }
    let Some(completed_record) = completed_record else {
        let record = lifecycle_store.read_record(&target_run).unwrap();
        let continuation_state = homeboy::agents::agent_task_service::continuation_state_in_store(
            &recipe_store,
            target_cook,
            &target_run,
        )
        .unwrap();
        let completion_timeout_evidence = serde_json::json!({
            "record_state": record.state,
            "promotion_progress": record.metadata["promotion_progress"],
            "cook_continuation": record.metadata["cook_continuation"],
            "cook_continuation_state": format!("{continuation_state:?}"),
            "cook_continuation_scheduler": record.metadata["cook_continuation_scheduler"],
            "cook_operation_claims": record.metadata["cook_operation_claims"],
            "cook_progress": record.metadata["cook_progress"],
            "provider_executions": record.metadata["provider_executions"],
            "gate_count": std::fs::read_to_string(&gate_count).ok(),
            "gate_started": gate_started.exists(),
            "driver_lock_content": std::fs::read_to_string(&driver_lock).ok(),
            "driver_lock_holders": linux_lock_file_holders(&driver_lock),
            "batch_coordinator_pid": std::process::id(),
            "sibling_provider_pid": sibling_provider_owner,
            "sibling_provider_running": homeboy::core::process::pid_is_running(sibling_provider_owner),
        });
        write_same_child_evidence(
            "same-child-recovery-completion-timeout.json",
            &completion_timeout_evidence,
        );
        panic!("same-child recovery did not reach completed continuation state: {completion_timeout_evidence}");
    };
    assert!(
        linux_lock_file_holders(&driver_lock)
            .as_array()
            .is_some_and(Vec::is_empty),
        "Cook driver lock is released after resumed gate completion"
    );
    write_same_child_evidence(
        "same-child-recovery-completed.json",
        &serde_json::json!({
            "record_state": completed_record.state,
            "promotion_progress": completed_record.metadata["promotion_progress"],
            "cook_continuation": completed_record.metadata["cook_continuation"],
            "cook_continuation_scheduler": completed_record.metadata["cook_continuation_scheduler"],
            "provider_executions": completed_record.metadata["provider_executions"],
            "gate_count": std::fs::read_to_string(&gate_count).ok(),
            "driver_lock_content": std::fs::read_to_string(&driver_lock).ok(),
            "driver_lock_holders": linux_lock_file_holders(&driver_lock),
            "batch_coordinator_pid": std::process::id(),
            "sibling_provider_pid": sibling_provider_owner,
            "sibling_provider_running": homeboy::core::process::pid_is_running(sibling_provider_owner),
        }),
    );
    let provider_count = completed_record.metadata["provider_executions"]
        .as_array()
        .unwrap()
        .len();
    let gate_count_after_continue = std::fs::read_to_string(&gate_count).unwrap();
    assert_eq!(
        gate_count_after_continue, "1",
        "real target gate completed once after the old owner was reaped"
    );
    lifecycle_store
        .mutate_record(&target_run, |record| {
            // The batch coordinator remains live after the child's own driver
            // lock and gate owner have gone away.
            record.metadata["runner_pid"] = serde_json::json!(std::process::id());
            true
        })
        .unwrap();
    let terminal_parent_preflight = preflight(&target_run, None);
    let terminal_parent_report: Value =
        serde_json::from_slice(&terminal_parent_preflight.stdout).unwrap();
    let terminal_parent_denial = terminal_parent_report["data"]["failure_context"]["diagnostic"]
        ["details"]["continuation_admission"]["first_authoritative_denial"]
        .as_str();
    assert_ne!(
        terminal_parent_denial,
        Some("live_owner_in_progress"),
        "a live batch coordinator PID is not the terminal child's Cook owner: {terminal_parent_report}"
    );
    write_same_child_evidence(
        "same-child-terminal-parent-preflight.json",
        &serde_json::json!({
            "exit_code": terminal_parent_preflight.status.code(),
            "live_batch_coordinator_pid": std::process::id(),
            "child_driver_lock_holders": linux_lock_file_holders(&driver_lock),
            "child_promotion_progress": completed_record.metadata["promotion_progress"],
            "report": terminal_parent_report,
        }),
    );
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
    sibling.terminate_tree();
    sibling_daemon_guard.stop();
}
