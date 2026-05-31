use std::time::Duration;

use reqwest::blocking::Client;
use serde::Serialize;
use serde_json::json;

use crate::core::api_jobs::{Job, RemoteRunnerJobClaim, RemoteRunnerJobResult};
use crate::core::error::{Error, Result};

use super::broker_http;
use super::execution::{exec, RunnerExecOptions};

#[derive(Debug, Clone)]
pub struct ReverseRunnerWorkerOptions {
    pub runner_id: String,
    pub broker_url: String,
    pub project_id: Option<String>,
    pub lease_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ReverseRunnerWorkerOutput {
    pub command: &'static str,
    pub runner_id: String,
    pub broker_url: String,
    pub claimed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job: Option<Job>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
}

pub fn run_reverse_worker(
    options: ReverseRunnerWorkerOptions,
) -> Result<(ReverseRunnerWorkerOutput, i32)> {
    if options.runner_id.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "runner_id",
            "reverse runner worker requires a runner id",
            None,
            None,
        ));
    }
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
        return Ok((
            ReverseRunnerWorkerOutput {
                command: "runner.work",
                runner_id: options.runner_id,
                broker_url: options.broker_url,
                claimed: false,
                job: None,
                exit_code: None,
            },
            0,
        ));
    };

    append_progress(&client, &options.broker_url, &options.runner_id, &claim.job)?;
    let (exec_output, exit_code) = exec(
        &options.runner_id,
        RunnerExecOptions {
            cwd: claim.request.cwd.clone(),
            project_id: claim.request.project_id.clone(),
            allow_ssh: false,
            command: claim.request.command.clone(),
            env: claim.request.env.clone(),
            capture_patch: claim.request.capture_patch,
            raw_exec: false,
            source_snapshot: claim.request.source_snapshot.clone(),
            capability_preflight: None,
            required_extensions: Vec::new(),
        },
    )?;
    let job = finish_job(
        &client,
        &options.broker_url,
        &options.runner_id,
        &claim.job,
        RemoteRunnerJobResult {
            exit_code,
            stdout: Some(exec_output.stdout),
            stderr: Some(exec_output.stderr),
            data: Some(json!({
                "mode": exec_output.mode,
                "remote_cwd": exec_output.remote_cwd,
            })),
            artifacts: Vec::new(),
            metrics: exec_output.metrics.clone(),
        },
    )?;

    Ok((
        ReverseRunnerWorkerOutput {
            command: "runner.work",
            runner_id: options.runner_id,
            broker_url: options.broker_url,
            claimed: true,
            job: Some(job),
            exit_code: Some(exit_code),
        },
        exit_code,
    ))
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
                    capture_patch: false,
                    source_snapshot: None,
                    metadata: None,
                })
                .expect("submit job");
            let (broker_url, handle) = spawn_mock_broker(store.clone(), 3);

            let (output, exit_code) = run_reverse_worker(ReverseRunnerWorkerOptions {
                runner_id: "lab".to_string(),
                broker_url: broker_url.clone(),
                project_id: None,
                lease_ms: 30_000,
            })
            .expect("run worker");

            assert_eq!(exit_code, 0);
            assert!(output.claimed);
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
                .claim_remote_runner_job("lab", None, 30_000)
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
}
