use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use homeboy_core::api_jobs::{
    JobEventKind, JobStatus, JobStore, RemoteRunnerJobRequest, RunnerJobLifecycleMetadata,
};
use homeboy_core::secret_env_plan::SecretEnvPlan;
use homeboy_core::server::{RunnerPolicy, RunnerSecretEnvRef};
use homeboy_core::test_support;
use sha2::{Digest, Sha256};

use super::super::run::{
    run_loop, run_reverse_worker, verify_private_at_files, write_private_at_file_snapshot,
};
use super::support::{
    spawn_cancelling_after_claim_broker, spawn_cancelling_on_second_snapshot_broker,
    spawn_failing_broker, spawn_mock_broker, spawn_mock_broker_until_finish,
    spawn_mock_broker_until_finish_with_paths, write_reverse_controller_session,
};
use super::worker_options;

#[test]
fn reverse_worker_executes_claimed_job_and_finishes_it() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        crate::merge(
            Some("lab"),
            &serde_json::json!({
                "policy": RunnerPolicy {
                    allow_raw_exec: Some(true),
                    workspace_roots: vec!["/tmp".to_string()],
                    allowed_commands: vec!["sh".to_string()],
                    ..Default::default()
                }
            })
            .to_string(),
            &[],
        )
        .expect("set policy");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf worker-ok".to_string(),
                ],
                cwd: Some("/tmp".to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker_url, handle) =
            spawn_mock_broker_until_finish_with_paths(store.clone(), 8, Some(seen_paths.clone()));
        write_reverse_controller_session(&broker_url);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url.clone())).expect("run worker");

        assert!(output.claimed);
        let serialized = serde_json::to_value(&output).expect("serialize output");
        assert_eq!(serialized["command"], serde_json::json!("runner.work"));
        assert_eq!(serialized["claimed"], serde_json::json!(true));
        assert!(serialized.get("loop_mode").is_none());
        assert!(serialized.get("iterations").is_none());
        assert!(serialized.get("jobs_claimed").is_none());
        assert!(serialized.get("last_claim").is_none());
        let job = output.job.clone().expect("job");
        let events = store.events(job.id).expect("events");
        let result = events
            .iter()
            .find(|event| event.kind == JobEventKind::Result)
            .and_then(|event| event.data.as_ref())
            .expect("result event data");
        assert_eq!(
            exit_code, 0,
            "worker output: {output:#?}; result: {result:#}"
        );
        assert_eq!(job.status, JobStatus::Succeeded);
        handle.join().expect("mock broker joins");
        assert!(events.iter().any(|event| {
            event.kind == JobEventKind::Result
                && event.data.as_ref().expect("result data")["stdout"]
                    == serde_json::json!("worker-ok")
        }));
        assert!(result["metrics"]["duration_ms"].as_u64().is_some());
        let seen_paths = seen_paths.lock().expect("seen paths");
        assert!(
            !seen_paths.iter().any(|path| path == "/runner/jobs"),
            "claimed worker job must execute locally instead of submitting another reverse broker job"
        );
        if cfg!(target_os = "linux") {
            assert_eq!(
                result["metrics"]["source"],
                serde_json::json!("linux_procfs_process_tree")
            );
            assert!(result["metrics"]["sample_count"].as_u64().is_some());
        }
    });
}

#[cfg(unix)]
#[test]
fn reverse_worker_verifies_private_at_file_then_cleans_it_up() {
    use std::os::unix::fs::PermissionsExt;

    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        crate::merge(
            Some("lab"),
            &serde_json::json!({
                "policy": RunnerPolicy {
                    allow_raw_exec: Some(true),
                    workspace_roots: vec!["/tmp".to_string()],
                    allowed_commands: vec!["sh".to_string()],
                    ..Default::default()
                }
            })
            .to_string(),
            &[],
        )
        .expect("set policy");
        let directory = tempfile::tempdir().expect("private file directory");
        let content = b"private plan";
        let digest = format!("{:x}", Sha256::digest(content));
        let path = directory
            .path()
            .join(format!("private-sha256-{digest}-plan.json"));
        std::fs::write(&path, content).expect("write private plan");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("lock private plan");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "test \"$(cat \"${1#@}\")\" = 'private plan'".to_string(),
                    "--".to_string(),
                    format!("@{}", path.display()),
                ],
                cwd: Some("/tmp".to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit private file job");
        let (broker_url, handle) = spawn_mock_broker_until_finish(store.clone(), 8);
        write_reverse_controller_session(&broker_url);

        let (_, exit_code) = run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        assert!(!path.exists(), "private input is removed after consumption");
        handle.join().expect("mock broker joins");
    });
}

#[cfg(unix)]
#[test]
fn reverse_worker_rejects_tampered_private_at_file() {
    use std::os::unix::fs::PermissionsExt;

    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        let directory = tempfile::tempdir().expect("private file directory");
        let digest = format!("{:x}", Sha256::digest(b"expected"));
        let path = directory
            .path()
            .join(format!("private-sha256-{digest}-plan.json"));
        std::fs::write(&path, b"tampered").expect("write tampered plan");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .expect("lock private plan");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec!["sh".to_string(), format!("@{}", path.display())],
                cwd: Some("/tmp".to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit tampered file job");
        let (broker_url, handle) = spawn_mock_broker(store.clone(), 3);
        write_reverse_controller_session(&broker_url);

        let error =
            run_reverse_worker(worker_options(broker_url)).expect_err("tampered file rejected");

        assert!(
            error
                .message
                .contains("does not match its SHA-256 identity"),
            "unexpected error: {error:#?}"
        );
        assert!(!path.exists(), "tampered private input is removed");
        handle.join().expect("mock broker joins");
    });
}

#[cfg(unix)]
#[test]
fn private_at_file_snapshot_survives_source_replacement_before_exec() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");
    let verified = b"verified plan";
    let digest = format!("{:x}", Sha256::digest(verified));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    std::fs::write(&source, verified).expect("write private plan");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o600))
        .expect("lock private plan");
    let request = RemoteRunnerJobRequest {
        runner_id: "lab".to_string(),
        project_id: None,
        operation: "runner.exec".to_string(),
        command: vec!["sh".to_string(), format!("@{}", source.display())],
        cwd: Some("/tmp".to_string()),
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: false,
        source_snapshot: None,
        path_materialization_plan: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        metadata: None,
    };
    let mut envelope = request.execution_envelope();

    let cleanup = verify_private_at_files(&mut envelope).expect("verify private input");
    let snapshot = envelope.dispatch.as_ref().expect("dispatch").command[1]
        .strip_prefix('@')
        .expect("rewritten @file")
        .to_string();
    assert_ne!(
        std::path::Path::new(&snapshot).parent(),
        source.parent(),
        "verified snapshot is outside the source-owner directory"
    );
    assert_eq!(
        std::fs::metadata(
            std::path::Path::new(&snapshot)
                .parent()
                .expect("snapshot parent")
        )
        .expect("snapshot parent metadata")
        .permissions()
        .mode()
            & 0o777,
        0o700,
        "worker-owned snapshot directory is private"
    );
    std::fs::write(&source, b"replaced after validation").expect("replace source");

    assert_eq!(std::fs::read(&snapshot).expect("read snapshot"), verified);
    drop(cleanup);
    assert!(!source.exists(), "source cleanup runs after replacement");
    assert!(
        !std::path::Path::new(&snapshot).exists(),
        "snapshot cleanup runs"
    );
}

#[cfg(unix)]
#[test]
fn private_at_file_unsafe_permissions_are_cleaned_before_error() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");
    let content = b"private plan";
    let digest = format!("{:x}", Sha256::digest(content));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    std::fs::write(&source, content).expect("write private plan");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o644))
        .expect("make private plan unsafe");
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("unsafe mode rejected");

    assert!(error.message.contains("private input cleanup succeeded"));
    assert!(!source.exists(), "unsafe plaintext is cleaned");
}

#[cfg(unix)]
#[test]
fn private_at_file_stat_failure_reports_cleanup_result() {
    let directory = tempfile::tempdir().expect("private file directory");
    let digest = format!("{:x}", Sha256::digest(b"private plan"));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("missing input rejected");

    assert!(error.message.contains("stat private runner @file"));
    assert!(error.message.contains("private input cleanup succeeded"));
}

#[cfg(unix)]
#[test]
fn private_at_file_read_failure_is_cleaned_before_error() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");
    let content = b"private plan";
    let digest = format!("{:x}", Sha256::digest(content));
    let source = directory
        .path()
        .join(format!("private-sha256-{digest}-plan.json"));
    std::fs::write(&source, content).expect("write private plan");
    std::fs::set_permissions(&source, std::fs::Permissions::from_mode(0o000))
        .expect("make private plan unreadable");
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("unreadable input rejected");

    assert!(error.message.contains("read private runner @file"));
    assert!(error.message.contains("private input cleanup succeeded"));
    assert!(!source.exists(), "unreadable plaintext is cleaned");
}

#[cfg(unix)]
#[test]
fn private_at_file_snapshot_is_atomically_published_with_owner_only_mode() {
    use std::os::unix::fs::PermissionsExt;

    let directory = tempfile::tempdir().expect("private file directory");

    let snapshot = write_private_at_file_snapshot(directory.path(), b"complete verified content")
        .expect("write snapshot");

    assert_eq!(
        std::fs::read(&snapshot).expect("read snapshot"),
        b"complete verified content"
    );
    assert_eq!(
        std::fs::metadata(&snapshot)
            .expect("snapshot metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let entries = std::fs::read_dir(directory.path())
        .expect("list snapshot directory")
        .filter_map(std::result::Result::ok)
        .filter_map(|entry| entry.file_name().into_string().ok())
        .collect::<Vec<_>>();
    assert!(entries.iter().all(|entry| !entry.ends_with(".tmp")));
}

fn private_at_file_request(path: &std::path::Path) -> RemoteRunnerJobRequest {
    RemoteRunnerJobRequest {
        runner_id: "lab".to_string(),
        project_id: None,
        operation: "runner.exec".to_string(),
        command: vec!["sh".to_string(), format!("@{}", path.display())],
        cwd: Some("/tmp".to_string()),
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: false,
        source_snapshot: None,
        path_materialization_plan: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        metadata: None,
    }
}

#[cfg(not(unix))]
#[test]
fn private_at_file_is_rejected_without_unix_filesystem_guarantees() {
    let directory = tempfile::tempdir().expect("private file directory");
    let source = directory.path().join(
        "private-sha256-aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-plan.json",
    );
    let request = private_at_file_request(&source);
    let mut envelope = request.execution_envelope();

    let error = verify_private_at_files(&mut envelope).expect_err("private input rejected");

    assert!(error
        .message
        .contains("require Unix owner-only filesystem guarantees"));
}

#[test]
fn reverse_worker_drains_fifo_jobs_after_completion_and_reconnect() {
    test_support::with_isolated_home(|_| {
        create_shell_runner();
        let store = JobStore::default();
        let mut first = run_id_echo_request();
        first.command[2] = "printf first".to_string();
        first.lifecycle = Some(RunnerJobLifecycleMetadata {
            source: Some("lab-handoff".to_string()),
            kind: Some("agent-task".to_string()),
            durable_run_id: Some("lab-run-first".to_string()),
            active_child_count: None,
            active_cell_count: None,
        });
        let mut second = first.clone();
        second.command[2] = "printf second".to_string();
        second.lifecycle.as_mut().expect("lifecycle").durable_run_id =
            Some("lab-run-second".to_string());

        let first_job = store.submit_remote_runner_job(first).expect("queue first");
        let second_job = store
            .submit_remote_runner_job(second)
            .expect("queue second");

        let (broker_url, handle) = spawn_mock_broker_until_finish(store.clone(), 8);
        let (first_output, first_exit) =
            run_reverse_worker(worker_options(broker_url)).expect("first worker");
        assert_eq!(first_exit, 0);
        assert_eq!(first_output.job.expect("first job").id, first_job.id);
        handle.join().expect("first broker joins");
        assert_eq!(
            store.get(second_job.id).expect("queued second").status,
            JobStatus::Queued
        );

        // A reconnect is only another worker claim: the broker retains the
        // queue and its FIFO order instead of involving controller execution.
        let (broker_url, handle) = spawn_mock_broker_until_finish(store.clone(), 8);
        let (second_output, second_exit) =
            run_reverse_worker(worker_options(broker_url)).expect("reconnected worker");
        assert_eq!(second_exit, 0);
        assert_eq!(second_output.job.expect("second job").id, second_job.id);
        handle.join().expect("second broker joins");

        let (broker_url, handle) = spawn_mock_broker(store.clone(), 1);
        let (idle_output, idle_exit) =
            run_reverse_worker(worker_options(broker_url)).expect("duplicate wake");
        assert_eq!(idle_exit, 0);
        assert!(!idle_output.claimed);
        handle.join().expect("idle broker joins");

        assert_eq!(
            result_event_data(&store, first_job.id)["stdout"],
            serde_json::json!("first")
        );
        assert_eq!(
            result_event_data(&store, second_job.id)["stdout"],
            serde_json::json!("second")
        );
        assert_eq!(
            store
                .get(second_job.id)
                .expect("finished second")
                .claimed_by_runner_id,
            Some("lab".to_string())
        );
    });
}

#[test]
fn reverse_worker_streams_redacted_child_progress_without_trusting_stdout_lifecycle_fields() {
    test_support::with_isolated_home(|_| {
        create_shell_runner();
        let temp = tempfile::tempdir().expect("tempdir");
        let token_path = temp.path().join("token");
        std::fs::write(&token_path, "child-secret\n").expect("token file");
        crate::merge(
            Some("lab"),
            &serde_json::json!({
                "secret_env": {
                    "TOKEN": RunnerSecretEnvRef {
                        env: None,
                        file: Some(token_path.display().to_string()),
                        secret: None,
                    }
                }
            })
            .to_string(),
            &[],
        )
        .expect("configure named runner secret");
        let store = JobStore::default();
        let mut request = run_id_echo_request();
        request.command[2] = "printf 'HOMEBOY_RUNNER_PROGRESS {\"schema\":\"homeboy/runner-progress/v1\",\"phase\":\"import\",\"current_item\":\"%s\",\"completed\":1,\"total\":2,\"metadata\":{\"api_key\":\"%s\"}}\\n' \"$TOKEN\" \"$TOKEN\"; printf 'HOMEBOY_RUNNER_PROGRESS {not-json}\\n'; printf 'HOMEBOY_RUNNER_PROGRESS {\"schema\":\"homeboy/runner-progress/v1\",\"phase\":\"done\",\"status\":\"succeeded\"}\\n'; sleep 0.1; dd if=/dev/zero bs=1024 count=4097 2>/dev/null; printf tail".to_string();
        request.secret_env_names = vec!["TOKEN".to_string()];
        store.submit_remote_runner_job(request).expect("submit job");
        let (broker_url, handle) = spawn_mock_broker_until_finish(store.clone(), 8);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        let job = output.job.expect("job");
        handle.join().expect("mock broker joins");
        let events = store.events(job.id).expect("persisted events");
        let progress_index = events
            .iter()
            .position(|event| {
                event.kind == JobEventKind::Progress
                    && event
                        .data
                        .as_ref()
                        .and_then(|data| data.get("phase"))
                        .and_then(serde_json::Value::as_str)
                        == Some("import")
            })
            .expect("child progress event persisted before finish");
        let result_index = events
            .iter()
            .position(|event| event.kind == JobEventKind::Result)
            .expect("result event");
        assert!(
            progress_index < result_index,
            "progress must stream before terminal result"
        );
        let progress = events[progress_index].data.as_ref().expect("progress data");
        assert_eq!(progress["phase"], "import");
        assert_eq!(progress["current_item"], "[REDACTED]");
        assert_eq!(progress["metadata"]["api_key"], "[REDACTED]");
        assert!(events.iter().all(|event| event.kind != JobEventKind::Status
            || event.message.as_deref() != Some("succeeded")));
        let result = result_event_data(&store, job.id);
        assert!(result["stdout"].as_str().expect("stdout").ends_with("tail"));
        assert_eq!(result["capture"]["stdout"]["truncated"], true);
        assert!(
            result["capture"]["stdout"]["bytes_retained"]
                .as_u64()
                .unwrap()
                <= 4 * 1024 * 1024
        );
    });
}

#[test]
fn reverse_worker_injects_lifecycle_run_id_into_claimed_job_env() {
    test_support::with_isolated_home(|_| {
        create_shell_runner();
        let store = JobStore::default();
        let mut request = run_id_echo_request();
        request.env.insert(
            "HOMEBOY_ACTIVE_RUN_ID".to_string(),
            "conflicting-active".to_string(),
        );
        request.env.insert(
            "HOMEBOY_RUN_ID".to_string(),
            "conflicting-homeboy".to_string(),
        );
        request.env.insert(
            "HOMEBOY_BENCH_RUN_ID".to_string(),
            "conflicting-bench".to_string(),
        );
        request.env.insert(
            "WORKFLOW_BENCH_RUN_ID".to_string(),
            "conflicting-workflow".to_string(),
        );
        request.lifecycle = Some(RunnerJobLifecycleMetadata {
            source: None,
            kind: None,
            durable_run_id: Some("durable-run-123".to_string()),
            active_child_count: None,
            active_cell_count: None,
        });
        store.submit_remote_runner_job(request).expect("submit job");
        let (broker_url, handle) = spawn_mock_broker_until_finish(store.clone(), 8);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Succeeded);
        handle.join().expect("mock broker joins");
        let result = result_event_data(&store, job.id);
        assert_eq!(
            result["stdout"],
            serde_json::json!("durable-run-123|durable-run-123|durable-run-123|unset")
        );
    });
}

#[test]
fn reverse_worker_uses_metadata_run_id_for_claimed_job_env() {
    test_support::with_isolated_home(|_| {
        create_shell_runner();
        let store = JobStore::default();
        let mut request = run_id_echo_request();
        request.metadata = Some(serde_json::json!({
            "run_id": "metadata-run-456",
        }));
        store.submit_remote_runner_job(request).expect("submit job");
        let (broker_url, handle) = spawn_mock_broker_until_finish(store.clone(), 8);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Succeeded);
        handle.join().expect("mock broker joins");
        let result = result_event_data(&store, job.id);
        assert_eq!(
            result["stdout"],
            serde_json::json!("metadata-run-456|metadata-run-456|metadata-run-456|unset")
        );
    });
}

#[test]
fn reverse_worker_executes_from_envelope_dispatch_fields() {
    test_support::with_isolated_home(|_| {
        create_shell_runner();
        let temp = tempfile::tempdir().expect("tempdir");
        let token_b_path = temp.path().join("token-b");
        std::fs::write(&token_b_path, "secret-b\n").expect("token B file");
        crate::merge(
            Some("lab"),
            &serde_json::json!({
                "secret_env": {
                    "TOKEN_B": RunnerSecretEnvRef {
                        env: None,
                        file: Some(token_b_path.display().to_string()),
                        secret: None,
                    },
                }
            })
            .to_string(),
            &[],
        )
        .expect("configure named runner secrets");
        let path = temp.path().join("jobs.json");
        let store = JobStore::open_without_reconciliation(&path).expect("open durable store");
        let mut request = run_id_echo_request();
        request.command = vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s|%s' \"$PUBLIC_VALUE\" \"$TOKEN_B\"".to_string(),
        ];
        request
            .env
            .insert("PUBLIC_VALUE".to_string(), "visible".to_string());
        request.secret_env_plan = SecretEnvPlan::from_secret_env_names(["TOKEN_B".to_string()]);
        request.require_paths = vec!["/tmp".to_string()];
        request.lifecycle = Some(RunnerJobLifecycleMetadata {
            source: Some("reverse-broker".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: Some("envelope-run-789".to_string()),
            active_child_count: Some(1),
            active_cell_count: Some(2),
        });
        store.submit_remote_runner_job(request).expect("submit job");
        assert!(!std::fs::read_to_string(&path)
            .expect("read durable job")
            .contains("secret-b"));
        drop(store);
        let store = JobStore::open_without_reconciliation(&path).expect("reopen durable store");
        let (broker_url, handle) = spawn_mock_broker_until_finish(store.clone(), 8);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Succeeded);
        handle.join().expect("mock broker joins");
        let result = result_event_data(&store, job.id);
        assert_eq!(result["stdout"], serde_json::json!("visible|[REDACTED]"));
        assert_eq!(
            result["data"]["execution_record"]["path_materialization_plan"]["entries"][0]
                ["remote_path"],
            serde_json::json!("/tmp")
        );
    });
}

#[test]
fn reverse_worker_loop_backs_off_when_no_job_is_available() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        let store = JobStore::default();
        let (broker_url, handle) = spawn_mock_broker(store, 1);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_after_sleep = stop.clone();
        let mut sleeps = Vec::new();
        let (output, exit_code) = run_loop(worker_options(broker_url), stop, |duration| {
            sleeps.push(duration);
            stop_after_sleep.store(true, Ordering::SeqCst);
        })
        .expect("run loop");

        assert_eq!(exit_code, 0);
        assert!(!output.claimed);
        assert!(output.stopped);
        assert_eq!(output.iterations, 1);
        assert_eq!(output.last_claim, None);
        assert_eq!(output.last_result, None);
        assert_eq!(output.last_error, None);
        assert_eq!(sleeps, vec![Duration::from_millis(1)]);
        handle.join().expect("mock broker joins");
    });
}

#[test]
fn reverse_worker_reports_execution_failure_to_broker() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec!["not-allowed".to_string()],
                cwd: Some("/tmp".to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let (broker_url, handle) = spawn_mock_broker(store.clone(), 5);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 1);
        assert!(output.claimed);
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Failed);
        handle.join().expect("mock broker joins");
        let events = store.events(job.id).expect("events");
        assert!(events.iter().any(|event| event.kind == JobEventKind::Error));
    });
}

#[test]
fn reverse_worker_loop_reports_failed_job_status() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec!["not-allowed".to_string()],
                cwd: Some("/tmp".to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let (broker_url, handle) = spawn_mock_broker(store.clone(), 6);
        let stop = Arc::new(AtomicBool::new(false));
        let stop_after_sleep = stop.clone();
        let mut options = worker_options(broker_url);
        options.loop_mode = true;

        let (output, exit_code) = run_loop(options, stop, |_| {
            stop_after_sleep.store(true, Ordering::SeqCst);
        })
        .expect("run loop");

        assert_eq!(exit_code, 0);
        assert!(output.claimed);
        assert_eq!(output.jobs_claimed, 1);
        assert_eq!(output.last_result, Some(1));
        assert_eq!(output.last_error.as_deref(), Some("job exited with code 1"));
        assert!(output.last_claim.is_some());
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Failed);
        handle.join().expect("mock broker joins");
    });
}

#[test]
fn reverse_worker_loop_stops_without_claiming_when_stop_is_already_set() {
    let stop = Arc::new(AtomicBool::new(true));
    let (output, exit_code) = run_loop(
        worker_options("http://127.0.0.1:1".to_string()),
        stop,
        |_| panic!("worker should not sleep when already stopped"),
    )
    .expect("run loop");

    assert_eq!(exit_code, 0);
    assert!(output.stopped);
    assert_eq!(output.iterations, 0);
    assert_eq!(output.jobs_claimed, 0);
    assert_eq!(output.last_claim, None);
    assert_eq!(output.last_result, None);
    assert_eq!(output.last_error, None);
}

#[test]
fn reverse_worker_skips_execution_when_claim_is_cancelled_before_start() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf should-not-run".to_string(),
                ],
                cwd: Some("/tmp".to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker_url, handle) =
            spawn_cancelling_after_claim_broker(store.clone(), 2, Some(seen_paths.clone()));

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        assert!(output.claimed);
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Cancelled);
        handle.join().expect("mock broker joins");
        let seen_paths = seen_paths.lock().expect("seen paths");
        assert!(!seen_paths.iter().any(|path| path.ends_with("/events")));
        assert!(!seen_paths.iter().any(|path| path.ends_with("/finish")));
    });
}

#[test]
fn reverse_worker_skips_finish_when_cancelled_after_execution() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        crate::merge(
            Some("lab"),
            &serde_json::json!({
                "policy": RunnerPolicy {
                    allow_raw_exec: Some(true),
                    workspace_roots: vec!["/tmp".to_string()],
                    allowed_commands: vec!["sh".to_string()],
                    ..Default::default()
                }
            })
            .to_string(),
            &[],
        )
        .expect("set policy");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    "printf worker-ok".to_string(),
                ],
                cwd: Some("/tmp".to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker_url, handle) =
            spawn_cancelling_on_second_snapshot_broker(store.clone(), 8, Some(seen_paths.clone()));
        write_reverse_controller_session(&broker_url);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Cancelled);
        handle.join().expect("mock broker joins");
        let seen_paths = seen_paths.lock().expect("seen paths");
        assert!(!seen_paths.iter().any(|path| path.ends_with("/finish")));
        let events = store.events(job.id).expect("events");
        assert!(!events
            .iter()
            .any(|event| event.kind == JobEventKind::Result));
    });
}

#[test]
fn reverse_worker_interrupts_running_job_when_broker_cancel_is_observed() {
    test_support::with_isolated_home(|_| {
        crate::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        crate::merge(
            Some("lab"),
            &serde_json::json!({
                "policy": RunnerPolicy {
                    allow_raw_exec: Some(true),
                    workspace_roots: vec!["/tmp".to_string()],
                    allowed_commands: vec!["sh".to_string()],
                    ..Default::default()
                }
            })
            .to_string(),
            &[],
        )
        .expect("set policy");
        let cwd = std::env::temp_dir().join(format!(
            "homeboy-reverse-worker-cancel-{}",
            std::process::id()
        ));
        std::fs::create_dir_all(&cwd).expect("create test cwd");
        let marker = cwd.join("should-not-exist");
        let store = JobStore::default();
        store
            .submit_remote_runner_job(RemoteRunnerJobRequest {
                runner_id: "lab".to_string(),
                project_id: None,
                operation: "runner.exec".to_string(),
                command: vec![
                    "sh".to_string(),
                    "-c".to_string(),
                    format!("sleep 1; touch {}", marker.display()),
                ],
                cwd: Some(cwd.display().to_string()),
                env: Default::default(),
                secret_env_names: Vec::new(),
                secret_env_plan: Default::default(),
                env_materialization: None,
                capture_patch: false,
                source_snapshot: None,
                path_materialization_plan: None,
                require_paths: Vec::new(),
                extension_env_providers: Vec::new(),
                lab_runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker_url, handle) =
            spawn_cancelling_on_second_snapshot_broker(store.clone(), 8, Some(seen_paths.clone()));
        write_reverse_controller_session(&broker_url);

        let (output, exit_code) =
            run_reverse_worker(worker_options(broker_url)).expect("run worker");

        assert_eq!(exit_code, 0);
        let job = output.job.expect("job");
        assert_eq!(job.status, JobStatus::Cancelled);
        assert!(
            !marker.exists(),
            "cancelled reverse worker job left the child command running"
        );
        handle.join().expect("mock broker joins");
        let seen_paths = seen_paths.lock().expect("seen paths");
        assert!(!seen_paths.iter().any(|path| path.ends_with("/finish")));
        let events = store.events(job.id).expect("events");
        assert!(!events
            .iter()
            .any(|event| event.kind == JobEventKind::Result));
    });
}

fn create_shell_runner() {
    crate::create(
        r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
        false,
    )
    .expect("create runner");
    crate::merge(
        Some("lab"),
        &serde_json::json!({
            "policy": RunnerPolicy {
                allow_raw_exec: Some(true),
                workspace_roots: vec!["/tmp".to_string()],
                allowed_commands: vec!["sh".to_string()],
                ..Default::default()
            }
        })
        .to_string(),
        &[],
    )
    .expect("set policy");
}

fn run_id_echo_request() -> RemoteRunnerJobRequest {
    RemoteRunnerJobRequest {
        runner_id: "lab".to_string(),
        project_id: None,
        operation: "runner.exec".to_string(),
        command: vec![
            "sh".to_string(),
            "-c".to_string(),
            "printf '%s|%s|%s|%s' \"$HOMEBOY_ACTIVE_RUN_ID\" \"$HOMEBOY_RUN_ID\" \"$HOMEBOY_BENCH_RUN_ID\" \"${WORKFLOW_BENCH_RUN_ID-unset}\"".to_string(),
        ],
        cwd: Some("/tmp".to_string()),
        env: Default::default(),
        secret_env_names: Vec::new(),
        secret_env_plan: Default::default(),
        env_materialization: None,
        capture_patch: false,
        source_snapshot: None,
            path_materialization_plan: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
        lab_runner_workload: None,
        lifecycle: None,
        metadata: None,
    }
}

fn result_event_data(store: &JobStore, job_id: uuid::Uuid) -> serde_json::Value {
    store
        .events(job_id)
        .expect("events")
        .into_iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data)
        .expect("result event data")
}

#[test]
fn reverse_worker_loop_bounds_transient_broker_failures() {
    let (broker_url, handle) = spawn_failing_broker(2);
    let mut options = worker_options(broker_url);
    options.broker_retry_limit = 1;
    let stop = Arc::new(AtomicBool::new(false));
    let mut sleeps = 0;
    let err = run_loop(options, stop, |_| {
        sleeps += 1;
    })
    .expect_err("broker failures should exceed retry budget");

    assert!(err.to_string().contains("broker request failed"));
    assert_eq!(sleeps, 1);
    handle.join().expect("mock broker joins");
}
