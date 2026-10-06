use std::io::Write;
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
    store
        .mutate_record(cook_id, |record| {
            record.metadata["detached_cook_handoff"]["admission_deadline_at"] =
                serde_json::json!("2020-01-01T00:00:00Z");
            true
        })
        .expect("runtime seal wait outlasts the fallback lease");
    let expired = homeboy::agents::agent_task_lifecycle::expire_detached_cook_admission_in_store(
        &store, cook_id,
    )
    .expect("reconcile the blocked real launcher");
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
        !expired,
        "runtime lock contention must not expire a live Cook launcher"
    );
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
fn failed_preseal_cook_replays_original_command_with_captured_stdin() {
    let context = HermeticTestContext::new();
    let cook_id = "cook-runtime-seal-replay";
    let prompt = "replayed stdin prompt marker: replay-proof-15547\n";
    let repository = context.root().join("replay-repository");
    std::fs::create_dir_all(&repository).expect("create replay repository");
    for args in [
        vec!["init", "--initial-branch=main"],
        vec!["config", "user.name", "Homeboy Test"],
        vec!["config", "user.email", "homeboy-test@example.invalid"],
    ] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repository)
            .output()
            .expect("run git repository setup");
        assert!(output.status.success(), "git setup: {output:?}");
    }
    std::fs::write(repository.join("README.md"), "replay fixture\n")
        .expect("write replay repository file");
    for args in [vec!["add", "README.md"], vec!["commit", "-m", "fixture"]] {
        let output = std::process::Command::new("git")
            .args(args)
            .current_dir(&repository)
            .output()
            .expect("commit replay repository fixture");
        assert!(output.status.success(), "git commit: {output:?}");
    }
    homeboy_core::test_support::write_component_registration(
        context.home(),
        "homeboy",
        &repository,
    );
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
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0,
        "hold isolated runtime admission lock"
    );

    let mut initial = context.controller_runtime_command(TestBinary::HomeboyFixture);
    initial
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
            "-",
            "--verify",
            "true",
            "--placement",
            "local",
            "--run-id",
            cook_id,
            "--no-finalize",
        ])
        .env("HOMEBOY_TEST_COOK_RUNTIME_SEAL_ADMISSION_TIMEOUT_MS", "500")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = initial.spawn().expect("spawn initial Cook process");
    child
        .stdin
        .take()
        .expect("initial Cook stdin")
        .write_all(prompt.as_bytes())
        .expect("send original prompt");
    let failed = child
        .wait_with_output()
        .expect("wait for failed preseal Cook");
    assert_eq!(failed.status.code(), Some(2));
    let failure: serde_json::Value =
        serde_json::from_slice(&failed.stdout).expect("structured preseal failure");
    let replay_command = failure["diagnostics"]["details"]["next_actions"][0]
        .as_str()
        .expect("concrete original Cook replay command");
    assert_eq!(failure["diagnostics"]["details"]["source_run_id"], cook_id);
    assert!(
        replay_command.contains("agent-task cook"),
        "{replay_command}"
    );
    let replay_words = shlex::split(replay_command).expect("shell-safe replay argv");
    let run_id_option = replay_words
        .iter()
        .position(|word| word == "--run-id")
        .expect("replay has an explicit run ID");
    let replay_run_id = replay_words[run_id_option + 1].clone();
    assert_ne!(replay_run_id, cook_id, "replay gets a fresh Cook identity");
    assert_eq!(
        failure["diagnostics"]["details"]["replay_run_id"],
        replay_run_id
    );
    assert!(
        replay_command.contains(" < "),
        "stdin redirection: {replay_command}"
    );
    assert!(!replay_command.contains("agent-task retry"));
    assert_eq!(
        failure["diagnostics"]["details"]["retry_command"], replay_command,
        "retry guidance must execute the original Cook, not generic plan retry"
    );
    assert_eq!(
        failure["diagnostics"]["details"]["replay_command_kind"],
        "original_cook_invocation"
    );
    assert!(!replay_command.contains(prompt.trim()));

    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let failed_record = store.read_record(cook_id).expect("failed Cook parent");
    assert_eq!(
        failed_record.state,
        homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Failed
    );
    let original_launcher = failed_record.metadata["detached_cook_handoff"]["launcher_id"]
        .as_str()
        .expect("original launcher custody")
        .to_string();
    assert_eq!(
        unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_UN) },
        0,
        "clear isolated admission blocker"
    );
    drop(lock);

    let redirect = replay_words
        .iter()
        .position(|word| word == "<")
        .expect("persisted stdin redirection");
    assert_eq!(replay_words.get(redirect + 2), None, "one redirect target");
    let prompt_snapshot_path = &replay_words[redirect + 1];
    assert_eq!(
        std::fs::read_to_string(prompt_snapshot_path).expect("read durable captured prompt"),
        prompt,
        "retry input must match the snapshot captured by the first invocation"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let snapshot = std::fs::metadata(prompt_snapshot_path).expect("prompt permissions");
        assert_eq!(snapshot.permissions().mode() & 0o777, 0o600);
        let private_parent = std::path::Path::new(prompt_snapshot_path).parent().unwrap();
        assert_eq!(
            std::fs::metadata(private_parent)
                .expect("private prompt directory")
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }
    let prompt_snapshot =
        std::fs::File::open(prompt_snapshot_path).expect("durable captured prompt");
    let mut replay = context.controller_runtime_command(TestBinary::HomeboyFixture);
    replay
        .args(&replay_words[1..redirect])
        .stdin(prompt_snapshot)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let replayed = replay
        .output()
        .expect("execute emitted original Cook replay");
    assert_eq!(
        replayed.status.code(),
        Some(0),
        "replay stdout={} stderr={} store={:?}",
        String::from_utf8_lossy(&replayed.stdout),
        String::from_utf8_lossy(&replayed.stderr),
        store.read_record(cook_id).ok()
    );
    let still_failed = store
        .read_record(cook_id)
        .expect("original failed Cook remains addressable");
    assert_eq!(
        still_failed.state,
        homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Failed
    );
    let replay_deadline = Instant::now() + Duration::from_secs(20);
    let completed = loop {
        let record = store
            .read_record(&replay_run_id)
            .expect("replayed Cook parent");
        if record.state.is_terminal() {
            break record;
        }
        assert!(
            Instant::now() < replay_deadline,
            "replayed Cook did not become terminal: {record:?}"
        );
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        completed.state,
        homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Succeeded,
        "replay must finish the new durable Cook; record={completed:?}; log={}",
        std::fs::read_to_string(
            context
                .data_dir()
                .join("agent-task-detached")
                .join(&replay_run_id)
                .join("cook.log")
        )
        .unwrap_or_else(|error| format!("<unavailable: {error}>"))
    );
    assert_ne!(
        completed.metadata["detached_cook_handoff"]["launcher_id"], original_launcher,
        "the replay launcher owns the fresh Cook handoff"
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
