use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::json;

use crate::core::api_jobs::{
    Job, RemoteRunnerJobClaim, RemoteRunnerJobRequest, RemoteRunnerJobResult,
};
use crate::core::error::{Error, Result};

use super::broker_http;
use super::capabilities::RunnerCapabilityPreflight;
use super::execution::{exec, RunnerExecOptions};

#[derive(Debug, Clone)]
pub struct ReverseRunnerWorkerOptions {
    pub runner_id: String,
    pub broker_url: String,
    pub project_id: Option<String>,
    pub lease_ms: u64,
    pub concurrency_limit: Option<usize>,
    pub loop_mode: bool,
    pub idle_backoff_ms: u64,
    pub max_idle_backoff_ms: u64,
    pub broker_failure_backoff_ms: u64,
    pub broker_retry_limit: u32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReverseRunnerWorkerOutput {
    pub command: &'static str,
    pub runner_id: String,
    pub broker_url: String,
    pub claimed: bool,
    #[serde(skip_serializing_if = "is_false")]
    pub loop_mode: bool,
    #[serde(skip_serializing_if = "is_zero")]
    pub iterations: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub jobs_claimed: u64,
    #[serde(skip_serializing_if = "is_zero")]
    pub broker_failures: u32,
    #[serde(skip_serializing_if = "is_false")]
    pub stopped: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_claim: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_result: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<Job>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero<T>(value: &T) -> bool
where
    T: PartialEq + From<u8>,
{
    *value == T::from(0)
}

pub fn run_reverse_worker(
    options: ReverseRunnerWorkerOptions,
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    let stop = Arc::new(AtomicBool::new(false));
    if options.loop_mode {
        crate::core::process::install_shutdown_handler(stop.clone(), "runner worker")?;
    }
    run_reverse_worker_with_stop(options, stop)
}

fn run_reverse_worker_with_stop(
    options: ReverseRunnerWorkerOptions,
    stop: Arc<AtomicBool>,
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    if options.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "reverse runner worker requires a runner id",
            None,
            None,
        ));
    }
    if options.loop_mode {
        run_loop(options, stop, std::thread::sleep)
    } else {
        run_once_output(options, 1, 0, 0, false)
    }
}

fn run_loop(
    options: ReverseRunnerWorkerOptions,
    stop: Arc<AtomicBool>,
    mut sleep: impl FnMut(Duration),
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    let mut iterations = 0;
    let mut jobs_claimed = 0;
    let mut broker_failures = 0;
    let mut idle_backoff = Duration::from_millis(options.idle_backoff_ms.max(1));
    let max_idle_backoff = Duration::from_millis(options.max_idle_backoff_ms.max(1));
    let broker_failure_backoff = Duration::from_millis(options.broker_failure_backoff_ms.max(1));
    let mut last_job = None;
    let mut last_exit_code = None;
    let mut last_claim = None;
    let mut last_result = None;
    let mut last_error = None;

    log_worker_event(
        &options,
        "started",
        json!({
            "loop": true,
            "idle_backoff_ms": idle_backoff.as_millis(),
            "max_idle_backoff_ms": max_idle_backoff.as_millis(),
            "broker_failure_backoff_ms": broker_failure_backoff.as_millis(),
            "broker_retry_limit": options.broker_retry_limit,
        }),
    );

    while !stop.load(Ordering::SeqCst) {
        iterations += 1;
        match run_once_output(
            options.clone(),
            iterations,
            jobs_claimed,
            broker_failures,
            false,
        ) {
            Ok((output, exit_code)) => {
                broker_failures = 0;
                if output.claimed {
                    jobs_claimed += 1;
                    idle_backoff = Duration::from_millis(options.idle_backoff_ms.max(1));
                    last_claim = output.job.as_ref().map(|job| job.id.to_string());
                    last_result = Some(exit_code);
                    if exit_code == 0 {
                        last_error = None;
                    } else {
                        last_error = Some(format!("job exited with code {exit_code}"));
                    }
                    last_job = output.job;
                    last_exit_code = Some(exit_code);
                    log_worker_event(
                        &options,
                        "job_finished",
                        json!({
                            "iteration": iterations,
                            "job_id": last_claim.as_ref(),
                            "exit_code": exit_code,
                            "jobs_claimed": jobs_claimed,
                        }),
                    );
                } else {
                    log_worker_event(
                        &options,
                        "idle",
                        json!({
                            "iteration": iterations,
                            "sleep_ms": idle_backoff.as_millis(),
                        }),
                    );
                    sleep(idle_backoff);
                    idle_backoff = std::cmp::min(idle_backoff.saturating_mul(2), max_idle_backoff);
                }
            }
            Err(err) => {
                broker_failures = broker_failures.saturating_add(1);
                last_error = Some(err.to_string());
                log_worker_event(
                    &options,
                    "broker_failure",
                    json!({
                        "iteration": iterations,
                        "broker_failures": broker_failures,
                        "retry_limit": options.broker_retry_limit,
                        "sleep_ms": broker_failure_backoff.as_millis(),
                        "error": err.to_string(),
                    }),
                );
                if broker_failures > options.broker_retry_limit {
                    return Err(err);
                }
                sleep(broker_failure_backoff);
            }
        }
    }

    log_worker_event(
        &options,
        "stopped",
        json!({
            "iterations": iterations,
            "jobs_claimed": jobs_claimed,
            "broker_failures": broker_failures,
        }),
    );
    Ok((
        ReverseRunnerWorkerOutput {
            command: "runner.work",
            runner_id: options.runner_id,
            broker_url: options.broker_url,
            claimed: jobs_claimed > 0,
            loop_mode: true,
            iterations,
            jobs_claimed,
            broker_failures,
            stopped: true,
            last_claim,
            last_result,
            last_error,
            job: last_job,
            exit_code: last_exit_code,
        },
        0,
    ))
}

fn run_once_output(
    options: ReverseRunnerWorkerOptions,
    iterations: u64,
    jobs_claimed: u64,
    broker_failures: u32,
    stopped: bool,
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    if options.broker_url.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "broker_url",
            "reverse runner worker requires a broker URL",
            None,
            None,
        ));
    }

    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let claim = claim_job(&client, &options)?;
    let Some(claim) = claim else {
        let loop_mode = options.loop_mode;
        return Ok((
            ReverseRunnerWorkerOutput {
                command: "runner.work",
                runner_id: options.runner_id,
                broker_url: options.broker_url,
                claimed: false,
                loop_mode,
                iterations: if loop_mode { iterations } else { 0 },
                jobs_claimed: if loop_mode { jobs_claimed } else { 0 },
                broker_failures: if loop_mode { broker_failures } else { 0 },
                stopped,
                last_claim: None,
                last_result: None,
                last_error: None,
                job: None,
                exit_code: None,
            },
            0,
        ));
    };

    append_progress(&client, &options.broker_url, &options.runner_id, &claim.job)?;
    // Remote capability-parity preflight: validate that this runner can satisfy
    // the claimed job's top-level command (and any required paths) before
    // starting execution, so a missing tool fails before remote dispatch instead
    // of mid-run (#5093). `exec` runs `preflight_runner_capability_plan` against
    // this preflight before handing the command to the runtime.
    let capability_preflight = reverse_worker_capability_preflight(&claim.request);
    // Shared finisher so the exec-error and exec-success paths submit their
    // terminal job result through identical broker plumbing (#5091).
    let finish = |result: RemoteRunnerJobResult| {
        finish_job(
            &client,
            &options.broker_url,
            &options.runner_id,
            &claim.job,
            result,
        )
    };
    let exec_result = exec(
        &options.runner_id,
        RunnerExecOptions {
            cwd: claim.request.cwd.clone(),
            project_id: claim.request.project_id.clone(),
            allow_diagnostic_ssh: false,
            command: claim.request.command.clone(),
            env: claim.request.env.clone(),
            secret_env_names: claim.request.secret_env_names.clone(),
            capture_patch: claim.request.capture_patch,
            raw_exec: false,
            source_snapshot: claim.request.source_snapshot.clone(),
            capability_preflight,
            required_extensions: Vec::new(),
            require_paths: claim.request.require_paths.clone(),
        },
    );
    let (exec_output, exit_code) = match exec_result {
        Ok(result) => result,
        Err(err) => {
            let job = finish(RemoteRunnerJobResult {
                exit_code: 1,
                stdout: None,
                stderr: Some(err.to_string()),
                data: Some(json!({
                    "error": err.to_string(),
                })),
                artifacts: Vec::new(),
                metrics: None,
            })?;
            let exit_code = 1;
            return Ok((
                claimed_output(
                    options,
                    iterations,
                    jobs_claimed,
                    broker_failures,
                    stopped,
                    job,
                    exit_code,
                ),
                exit_code,
            ));
        }
    };
    let job = finish(RemoteRunnerJobResult {
        exit_code,
        stdout: Some(exec_output.stdout),
        stderr: Some(exec_output.stderr),
        data: Some(json!({
            "mode": exec_output.mode,
            "remote_cwd": exec_output.remote_cwd,
        })),
        artifacts: Vec::new(),
        metrics: exec_output.metrics.clone(),
    })?;

    Ok((
        claimed_output(
            options,
            iterations,
            jobs_claimed,
            broker_failures,
            stopped,
            job,
            exit_code,
        ),
        exit_code,
    ))
}

/// Build the remote capability-parity preflight for a claimed reverse-runner
/// job. The claimed command's executable (its first argv element) must be
/// available on this runner before execution starts, mirroring the direct
/// `runner exec` path's preflight contract (#5093).
fn reverse_worker_capability_preflight(
    request: &RemoteRunnerJobRequest,
) -> Option<RunnerCapabilityPreflight> {
    let required_commands: Vec<String> = request
        .command
        .first()
        .filter(|program| !program.trim().is_empty())
        .cloned()
        .into_iter()
        .collect();
    if required_commands.is_empty() {
        return None;
    }
    Some(RunnerCapabilityPreflight {
        command: "runner.work".to_string(),
        required_commands,
        ..Default::default()
    })
}

fn claimed_output(
    options: ReverseRunnerWorkerOptions,
    iterations: u64,
    jobs_claimed: u64,
    broker_failures: u32,
    stopped: bool,
    job: Job,
    exit_code: i32,
) -> ReverseRunnerWorkerOutput {
    let loop_mode = options.loop_mode;
    ReverseRunnerWorkerOutput {
        command: "runner.work",
        runner_id: options.runner_id,
        broker_url: options.broker_url,
        claimed: true,
        loop_mode,
        iterations: if loop_mode { iterations } else { 0 },
        jobs_claimed: if loop_mode { jobs_claimed + 1 } else { 0 },
        broker_failures: if loop_mode { broker_failures } else { 0 },
        stopped,
        last_claim: if loop_mode {
            Some(job.id.to_string())
        } else {
            None
        },
        last_result: if loop_mode { Some(exit_code) } else { None },
        last_error: if loop_mode && exit_code != 0 {
            Some(format!("job exited with code {exit_code}"))
        } else {
            None
        },
        job: Some(job),
        exit_code: Some(exit_code),
    }
}

fn log_worker_event(options: &ReverseRunnerWorkerOptions, event: &str, data: serde_json::Value) {
    eprintln!(
        "{}",
        json!({
            "command": "runner.work",
            "event": event,
            "runner_id": options.runner_id,
            "broker_url": options.broker_url,
            "project_id": options.project_id,
            "data": data,
        })
    );
}

fn claim_job(
    client: &Client,
    options: &ReverseRunnerWorkerOptions,
) -> Result<Option<RemoteRunnerJobClaim>> {
    let data = broker_http::post_json(
        client,
        &options.broker_url,
        "/runner/jobs/claim",
        json!({
            "runner_id": options.runner_id,
            "project_id": options.project_id,
            "lease_ms": options.lease_ms.max(1),
            "concurrency_limit": options.concurrency_limit,
        }),
        "claim reverse runner job",
    )?;
    let claim = data["claim"].clone();
    if claim.is_null() {
        return Ok(None);
    }
    serde_json::from_value(claim).map(Some).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse runner job claim".to_string()),
        )
    })
}

fn append_progress(client: &Client, broker_url: &str, runner_id: &str, job: &Job) -> Result<()> {
    broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/events", job.id),
        json!({
            "runner_id": runner_id,
            "kind": "progress",
            "message": "reverse runner worker started execution",
        }),
        "append reverse runner progress event",
    )?;
    Ok(())
}

fn finish_job(
    client: &Client,
    broker_url: &str,
    runner_id: &str,
    job: &Job,
    result: RemoteRunnerJobResult,
) -> Result<Job> {
    let data = broker_http::post_json(
        client,
        broker_url,
        &format!("/runner/jobs/{}/finish", job.id),
        json!({
            "runner_id": runner_id,
            "result": result,
        }),
        "finish reverse runner job",
    )?;
    serde_json::from_value(data["job"].clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse finished reverse runner job".to_string()),
        )
    })
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};

    use super::*;
    use crate::core::api_jobs::{JobEventKind, JobStatus, JobStore, RemoteRunnerJobRequest};
    use crate::core::server::RunnerPolicy;
    use crate::test_support;
    use serde_json::Value;

    fn worker_options(broker_url: String) -> ReverseRunnerWorkerOptions {
        ReverseRunnerWorkerOptions {
            runner_id: "lab".to_string(),
            broker_url,
            project_id: None,
            lease_ms: 30_000,
            concurrency_limit: None,
            loop_mode: false,
            idle_backoff_ms: 1,
            max_idle_backoff_ms: 10,
            broker_failure_backoff_ms: 1,
            broker_retry_limit: 1,
        }
    }

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
                    metadata: None,
                })
                .expect("submit job");
            let (broker_url, handle) = spawn_mock_broker(store.clone(), 3);

            let (output, exit_code) =
                run_reverse_worker(worker_options(broker_url.clone())).expect("run worker");

            assert_eq!(exit_code, 0);
            assert!(output.claimed);
            let serialized = serde_json::to_value(&output).expect("serialize output");
            assert_eq!(serialized["command"], serde_json::json!("runner.work"));
            assert_eq!(serialized["claimed"], serde_json::json!(true));
            assert!(serialized.get("loop_mode").is_none());
            assert!(serialized.get("iterations").is_none());
            assert!(serialized.get("jobs_claimed").is_none());
            assert!(serialized.get("last_claim").is_none());
            let job = output.job.expect("job");
            assert_eq!(job.status, JobStatus::Succeeded);
            handle.join().expect("mock broker joins");
            let events = store.events(job.id).expect("events");
            assert!(events.iter().any(|event| {
                event.kind == JobEventKind::Result
                    && event.data.as_ref().expect("result data")["stdout"]
                        == serde_json::json!("worker-ok")
            }));
            let result = events
                .iter()
                .find(|event| event.kind == JobEventKind::Result)
                .and_then(|event| event.data.as_ref())
                .expect("result event data");
            assert!(result["metrics"]["duration_ms"].as_u64().is_some());
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
                    metadata: None,
                })
                .expect("submit job");
            let (broker_url, handle) = spawn_mock_broker(store.clone(), 3);

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
                    metadata: None,
                })
                .expect("submit job");
            let (broker_url, handle) = spawn_mock_broker(store.clone(), 4);
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

    fn spawn_mock_broker(
        store: JobStore,
        expected_requests: usize,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept request");
                let request = read_request(&mut stream);
                let response = handle_request(&store, &request);
                write_response(&mut stream, response);
            }
        });
        (format!("http://{addr}"), handle)
    }

    struct MockRequest {
        path: String,
        body: Value,
    }

    fn read_request(stream: &mut std::net::TcpStream) -> MockRequest {
        let mut buffer = Vec::new();
        let mut temp = [0_u8; 1024];
        let header_end = loop {
            let read = stream.read(&mut temp).expect("read request");
            assert_ne!(read, 0, "request closed before headers");
            buffer.extend_from_slice(&temp[..read]);
            if let Some(index) = find_header_end(&buffer) {
                break index;
            }
        };
        let headers = String::from_utf8_lossy(&buffer[..header_end]).to_string();
        let path = headers
            .lines()
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .expect("request path")
            .to_string();
        let content_length = headers
            .lines()
            .find_map(|line| line.strip_prefix("content-length: "))
            .or_else(|| {
                headers
                    .lines()
                    .find_map(|line| line.strip_prefix("Content-Length: "))
            })
            .and_then(|value| value.trim().parse::<usize>().ok())
            .unwrap_or(0);
        let body_start = header_end + 4;
        while buffer.len() < body_start + content_length {
            let read = stream.read(&mut temp).expect("read body");
            assert_ne!(read, 0, "request closed before body");
            buffer.extend_from_slice(&temp[..read]);
        }
        let body = if content_length == 0 {
            Value::Null
        } else {
            serde_json::from_slice(&buffer[body_start..body_start + content_length])
                .expect("request json")
        };
        MockRequest { path, body }
    }

    fn find_header_end(buffer: &[u8]) -> Option<usize> {
        buffer.windows(4).position(|window| window == b"\r\n\r\n")
    }

    fn handle_request(store: &JobStore, request: &MockRequest) -> Value {
        if request.path == "/runner/jobs/claim" {
            let claim = store
                .claim_remote_runner_job("lab", None, 30_000, None)
                .expect("claim job");
            return serde_json::json!({
                "success": true,
                "data": { "body": { "claim": claim } }
            });
        }
        if let Some(job_id) = request
            .path
            .strip_prefix("/runner/jobs/")
            .and_then(|tail| tail.strip_suffix("/events"))
        {
            let job_id = uuid::Uuid::parse_str(job_id).expect("event job id");
            let event = store
                .append_remote_runner_event(
                    job_id,
                    "lab",
                    JobEventKind::Progress,
                    request.body["message"].as_str().map(ToString::to_string),
                    None,
                )
                .expect("append event");
            return serde_json::json!({
                "success": true,
                "data": { "body": { "event": event } }
            });
        }
        if let Some(job_id) = request
            .path
            .strip_prefix("/runner/jobs/")
            .and_then(|tail| tail.strip_suffix("/finish"))
        {
            let job_id = uuid::Uuid::parse_str(job_id).expect("finish job id");
            let result: RemoteRunnerJobResult =
                serde_json::from_value(request.body["result"].clone()).expect("finish result");
            let job = store
                .finish_remote_runner_job(job_id, "lab", result)
                .expect("finish job");
            return serde_json::json!({
                "success": true,
                "data": { "body": { "job": job } }
            });
        }
        serde_json::json!({
            "success": false,
            "error": { "message": "unknown mock path" }
        })
    }

    fn write_response(stream: &mut std::net::TcpStream, body: Value) {
        let body = body.to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream
            .write_all(response.as_bytes())
            .expect("write response");
    }

    fn spawn_failing_broker(expected_requests: usize) -> (String, std::thread::JoinHandle<()>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        let handle = std::thread::spawn(move || {
            for _ in 0..expected_requests {
                let (mut stream, _) = listener.accept().expect("accept request");
                let _ = read_request(&mut stream);
                write_response(
                    &mut stream,
                    serde_json::json!({
                        "success": false,
                        "error": { "message": "broker unavailable" }
                    }),
                );
            }
        });
        (format!("http://{addr}"), handle)
    }
}
