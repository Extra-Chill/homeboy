use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use crate::core::api_jobs::{JobEventKind, JobStatus, JobStore, RemoteRunnerJobRequest};
use crate::core::server::RunnerPolicy;
use crate::test_support;

use super::super::run::{run_loop, run_reverse_worker};
use super::support::{
    spawn_cancelling_after_claim_broker, spawn_cancelling_on_second_snapshot_broker,
    spawn_failing_broker, spawn_mock_broker, spawn_mock_broker_with_paths,
    write_reverse_controller_session,
};
use super::worker_options;

#[test]
fn reverse_worker_executes_claimed_job_and_finishes_it() {
    test_support::with_isolated_home(|_| {
        crate::core::runner::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        crate::core::runner::merge(
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
                capture_patch: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker_url, handle) =
            spawn_mock_broker_with_paths(store.clone(), 5, Some(seen_paths.clone()));
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

#[test]
fn reverse_worker_loop_backs_off_when_no_job_is_available() {
    test_support::with_isolated_home(|_| {
        crate::core::runner::create(
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
        crate::core::runner::create(
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
                capture_patch: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                runner_workload: None,
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
        crate::core::runner::create(
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
                capture_patch: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                runner_workload: None,
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
        crate::core::runner::create(
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
                capture_patch: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                runner_workload: None,
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
        crate::core::runner::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        crate::core::runner::merge(
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
                capture_patch: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker_url, handle) =
            spawn_cancelling_on_second_snapshot_broker(store.clone(), 4, Some(seen_paths.clone()));
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
        crate::core::runner::create(
            r#"{"id":"lab","kind":"local","workspace_root":"/tmp"}"#,
            false,
        )
        .expect("create runner");
        crate::core::runner::merge(
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
                capture_patch: false,
                source_snapshot: None,
                require_paths: Vec::new(),
                runner_workload: None,
                lifecycle: None,
                metadata: None,
            })
            .expect("submit job");
        let seen_paths = Arc::new(std::sync::Mutex::new(Vec::new()));
        let (broker_url, handle) =
            spawn_cancelling_on_second_snapshot_broker(store.clone(), 5, Some(seen_paths.clone()));
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
