use std::os::fd::AsRawFd;
use std::process::Stdio;
use std::time::{Duration, Instant};

use homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore;
use homeboy_core::test_support::{HermeticTestContext, TestBinary};

#[test]
fn local_cook_identity_is_discoverable_while_runtime_admission_is_locked() {
    let context = HermeticTestContext::new();
    let cook_id = "cook-runtime-seal-lock-regression";
    let runtime_root = homeboy_core::controller_runtime::runtime_root_in(&context.data_dir())
        .expect("runtime root");
    std::fs::create_dir_all(&runtime_root).expect("runtime root directory");
    let lock_path = runtime_root.join("admission.lock");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(&lock_path)
        .expect("create isolated runtime admission lock");
    let locked = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0;
    assert!(locked, "test must own the isolated runtime admission lock");

    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        .args([
            "agent-task",
            "cook",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/14528",
            "--head",
            "fix/14528-bounded-cook-admission-direct",
            "--base",
            "main",
            "--backend",
            "fixture",
            "--prompt",
            "exercise bounded runtime admission",
            "--verify",
            "true",
            "--placement",
            "local",
            "--run-id",
            cook_id,
            "--no-finalize",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut child = command.spawn().expect("spawn controlled Cook process");

    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let deadline = Instant::now() + Duration::from_secs(4);
    let mut observed = None;
    while Instant::now() < deadline {
        if let Ok(record) = store.read_record(cook_id) {
            if record.metadata["detached_cook_handoff"]["state"] == "pending"
                && record.metadata["cook_progress"]["phase"] == "controller_runtime_seal"
            {
                observed = Some(record);
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let Some(record) = observed else {
        let _ = child.kill();
        let _ = child.wait();
        let unlock_result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
        drop(lock);
        assert_eq!(unlock_result, 0, "release isolated admission lock");
        panic!("Cook identity was not durable while runtime admission was locked");
    };
    let original_launcher = record.metadata["detached_cook_handoff"]["launcher_id"].clone();
    let mut replay = context.controller_runtime_command(TestBinary::HomeboyFixture);
    replay
        .args([
            "agent-task",
            "cook",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/14528",
            "--head",
            "fix/14528-bounded-cook-admission-direct",
            "--base",
            "main",
            "--backend",
            "fixture",
            "--prompt",
            "concurrent replay must not steal startup custody",
            "--verify",
            "true",
            "--placement",
            "local",
            "--run-id",
            cook_id,
            "--no-finalize",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let replay_output = replay.output().expect("run concurrent replay");
    let after_replay = store.read_record(cook_id).ok();
    let remained_blocked = child.try_wait().expect("observe Cook process").is_none();
    let _ = child.kill();
    let _ = child.wait();
    let unlock_result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    drop(lock);

    assert_eq!(unlock_result, 0, "release isolated admission lock");
    assert!(
        remained_blocked,
        "Cook should be held at the fake admission lock"
    );
    assert_eq!(record.run_id, cook_id);
    assert_eq!(record.metadata["detached_cook_handoff"]["state"], "pending");
    assert_eq!(
        record.metadata["cook_progress"]["phase"],
        "controller_runtime_seal"
    );
    assert!(record.metadata["cook_progress"]["detail"]
        .as_str()
        .is_some_and(|detail| detail.contains("identity is durable")));
    assert_eq!(replay_output.status.code(), Some(2));
    let after_replay = after_replay.expect("original parent remains discoverable");
    assert_eq!(
        after_replay.metadata["detached_cook_handoff"]["launcher_id"],
        original_launcher
    );
    assert_eq!(
        after_replay.metadata["cook_progress"]["phase"],
        "controller_runtime_seal"
    );
}

#[test]
fn local_cook_hash_deadline_returns_named_failure_and_terminalizes_identity() {
    let context = HermeticTestContext::new();
    let cook_id = "cook-runtime-seal-hash-deadline";
    let source = context.data_dir().join("small-controller-fixture");
    std::fs::write(&source, b"#!/bin/sh\nexit 0\n").expect("write small controller fixture");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o700))
            .expect("make controller fixture executable");
    }
    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        .args([
            "agent-task",
            "cook",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/14528",
            "--head",
            "fix/14528-bounded-cook-admission-direct",
            "--base",
            "main",
            "--backend",
            "fixture",
            "--prompt",
            "exercise bounded runtime seal hashing",
            "--verify",
            "true",
            "--placement",
            "local",
            "--run-id",
            cook_id,
            "--no-finalize",
        ])
        .env("HOMEBOY_TEST_COOK_RUNTIME_SEAL_ADMISSION_TIMEOUT_MS", "500")
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE", &source)
        .env("HOMEBOY_TEST_CONTROLLER_RUNTIME_SOURCE", &source)
        .env(
            "HOMEBOY_TEST_CONTROLLER_RUNTIME_SEAL_CHECKPOINT_DELAY_MS",
            "750",
        );
    let output = command.output().expect("run hash-deadline Cook process");

    assert_eq!(output.status.code(), Some(2));
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("structured seal failure");
    let serialized = serde_json::to_string(&envelope).expect("render seal failure");
    assert!(serialized.contains("cook_runtime_seal"), "{serialized}");
    assert!(serialized.contains("hash_executable"), "{serialized}");
    assert!(serialized.contains(cook_id), "{serialized}");
    assert!(
        serialized.contains("homeboy agent-task status"),
        "{serialized}"
    );
    let progress = String::from_utf8_lossy(&output.stderr);
    assert!(progress.contains("homeboy/agent-task-admission-progress/v1"));
    assert!(progress.contains(cook_id));
    assert!(progress.contains("controller_runtime_seal"));

    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let record = store
        .read_record(cook_id)
        .expect("durable failed Cook parent");
    assert_eq!(
        record.state,
        homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Failed,
        "record={} error={serialized}",
        serde_json::to_string(&record).unwrap_or_default()
    );
    assert_eq!(
        record.metadata["cook_progress"]["phase"],
        "controller_runtime_seal"
    );
    assert!(
        record.metadata["detached_cook_handoff"]["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("hash_executable")),
        "record={} error={serialized}",
        serde_json::to_string(&record).unwrap_or_default()
    );
}

#[test]
fn generated_cook_id_is_emitted_before_bounded_admission_failure() {
    let context = HermeticTestContext::new();
    let runtime_root = homeboy_core::controller_runtime::runtime_root_in(&context.data_dir())
        .expect("runtime root");
    std::fs::create_dir_all(&runtime_root).expect("runtime root directory");
    let lock = std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(runtime_root.join("admission.lock"))
        .expect("create isolated runtime admission lock");
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "hold isolated runtime admission lock"
    );

    let mut command = context.controller_runtime_command(TestBinary::HomeboyFixture);
    command
        .args([
            "agent-task",
            "cook",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/14528",
            "--head",
            "fix/14528-bounded-cook-admission-direct",
            "--base",
            "main",
            "--backend",
            "fixture",
            "--prompt",
            "the generated Cook ID must be addressable before pin admission",
            "--verify",
            "true",
            "--placement",
            "local",
            "--no-finalize",
        ])
        .env(
            "HOMEBOY_TEST_COOK_RUNTIME_SEAL_ADMISSION_TIMEOUT_MS",
            "1500",
        );
    let output = command.output().expect("run generated-ID Cook process");
    let unlock_result = unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) };
    drop(lock);

    assert_eq!(unlock_result, 0);
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    let progress_line = stderr
        .lines()
        .find(|line| line.contains("homeboy/agent-task-admission-progress/v1"))
        .expect("machine-readable progress includes generated Cook ID");
    let progress: serde_json::Value = serde_json::from_str(progress_line).expect("progress JSON");
    let cook_id = progress["run_id"].as_str().expect("generated ID");
    let result: serde_json::Value = serde_json::from_slice(&output.stdout).expect("CLI result");
    assert_eq!(progress["phase"], "controller_runtime_seal");
    assert!(result.to_string().contains(cook_id));
    assert!(result
        .to_string()
        .contains("controller generation admission queue wait exceeded"));

    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let record = store
        .read_record(cook_id)
        .expect("generated durable Cook parent");
    assert_eq!(
        record.state,
        homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Failed,
        "record={} result={result}",
        serde_json::to_string(&record).unwrap_or_default()
    );
    assert!(record.metadata["detached_cook_handoff"]["reason"]
        .as_str()
        .is_some_and(
            |reason| reason.contains("controller generation admission queue wait exceeded")
        ));
}
