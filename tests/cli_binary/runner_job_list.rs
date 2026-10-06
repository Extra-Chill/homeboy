use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use homeboy_core::test_support::{HermeticTestContext, TestBinary};
use serde_json::{json, Value};

const RUNNER_ID: &str = "homeboy-lab";
const CONTROLLER_ID: &str = "job-list-cli-fixture";

#[test]
fn runner_job_list_real_binary_renders_filters_json_and_output_file() {
    let context = HermeticTestContext::new();
    let fixture_owner_pid = std::process::id();
    let tunnel_identity = homeboy_core::process::process_start_identity(fixture_owner_pid)
        .expect("inspect fixture process")
        .expect("fixture process identity");
    let endpoint = serve_runner_inventory();
    write_runner_state(
        &context,
        &endpoint.url,
        endpoint.port,
        fixture_owner_pid,
        tunnel_identity,
    );

    let default = run_cli(&context, &[]);
    assert_command_success(&default, "default job list");
    let default_table = stdout_text(&default);
    assert!(
        default_table.contains("Live: 2  Retained: 1\n"),
        "{default_table}"
    );
    eprintln!("successful compact job-list CLI output:\n{default_table}");
    assert!(default_table.contains("job-running"), "{default_table}");
    assert!(default_table.contains("job-queued"), "{default_table}");
    assert!(!default_table.contains("job-terminal"), "{default_table}");
    assert!(!default_table.contains("job-retained"), "{default_table}");

    let active = run_cli(&context, &["--active"]);
    assert_command_success(&active, "active job list");
    assert_contains_only_ids(&stdout_text(&active), &["job-running"]);

    let queued = run_cli(&context, &["--queued"]);
    assert_command_success(&queued, "queued job list");
    assert_contains_only_ids(&stdout_text(&queued), &["job-queued"]);

    let terminal = run_cli(&context, &["--terminal"]);
    assert_command_success(&terminal, "terminal job list");
    assert_contains_only_ids(&stdout_text(&terminal), &["job-terminal"]);

    let retained = run_cli(&context, &["--retained"]);
    assert_command_success(&retained, "retained job list");
    let retained_table = stdout_text(&retained);
    for id in ["job-running", "job-queued", "job-terminal", "job-retained"] {
        assert!(
            retained_table.contains(id),
            "{id} absent from {retained_table}"
        );
    }
    assert!(retained_table.contains("Live: 2  Retained: 1\n"));

    let generation = run_cli(&context, &["--retained", "--generation", "lease-retained"]);
    assert_command_success(&generation, "generation-filtered retained job list");
    assert_contains_only_ids(&stdout_text(&generation), &["job-retained"]);

    let correlation = run_cli(&context, &["--retained", "--correlation", "retained"]);
    assert_command_success(&correlation, "correlation-filtered retained job list");
    assert_contains_only_ids(&stdout_text(&correlation), &["job-retained"]);

    let json_output = run_cli(&context, &["--json"]);
    assert_command_success(&json_output, "JSON job list");
    let json_envelope: Value = serde_json::from_slice(&json_output.stdout).expect("JSON envelope");
    assert_eq!(json_envelope["success"], true);
    assert_eq!(json_envelope["data"]["command"], "runner.job.list");
    assert_eq!(json_envelope["data"]["live_daemon_job_count"], 2);
    assert_eq!(
        json_envelope["data"]["retained_durable_projection_count"],
        1
    );
    assert_eq!(
        json_envelope["data"]["jobs"]
            .as_array()
            .expect("JSON jobs")
            .len(),
        2
    );

    let output_path = context.root().join("job-list-output.json");
    let output_file_command = context
        .command(TestBinary::HomeboyFixture)
        .args(["runner", "job", "list", RUNNER_ID, "--output"])
        .arg(&output_path)
        .env("HOMEBOY_CONTROLLER_ID", CONTROLLER_ID)
        .output()
        .expect("run job list with output file");
    assert_command_success(&output_file_command, "job list output-file invocation");
    assert!(String::from_utf8_lossy(&output_file_command.stdout).contains("Live: 2  Retained: 1"));
    let output_file: Value = serde_json::from_slice(
        &std::fs::read(&output_path).expect("structured output file is written"),
    )
    .expect("output file JSON envelope");
    assert_eq!(output_file["success"], true);
    assert_eq!(output_file["data"]["live_daemon_job_count"], 2);
    assert_eq!(output_file["data"]["retained_durable_projection_count"], 1);
}

fn run_cli(context: &HermeticTestContext, extra_args: &[&str]) -> std::process::Output {
    context
        .command(TestBinary::HomeboyFixture)
        .args(["runner", "job", "list", RUNNER_ID])
        .args(extra_args)
        .env("HOMEBOY_CONTROLLER_ID", CONTROLLER_ID)
        .output()
        .expect("run real Homeboy binary")
}

fn assert_command_success(output: &std::process::Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} exited {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn stdout_text(output: &std::process::Output) -> String {
    String::from_utf8(output.stdout.clone()).expect("CLI stdout UTF-8")
}

fn assert_contains_only_ids(table: &str, expected_ids: &[&str]) {
    for id in ["job-running", "job-queued", "job-terminal", "job-retained"] {
        assert_eq!(table.contains(id), expected_ids.contains(&id), "{table}");
    }
}

struct RunnerInventoryServer {
    url: String,
    port: u16,
    stop: Arc<AtomicBool>,
    handle: Option<thread::JoinHandle<()>>,
}

impl RunnerInventoryServer {
    fn stop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = std::net::TcpStream::connect(self.url.trim_start_matches("http://"));
        if let Some(handle) = self.handle.take() {
            handle.join().expect("runner inventory server exits");
        }
    }
}

impl Drop for RunnerInventoryServer {
    fn drop(&mut self) {
        self.stop();
    }
}

fn serve_runner_inventory() -> RunnerInventoryServer {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind fixture runner endpoint");
    listener
        .set_nonblocking(true)
        .expect("make fixture endpoint nonblocking");
    let url = format!(
        "http://{}",
        listener.local_addr().expect("listener address")
    );
    let port = listener.local_addr().expect("listener address").port();
    let stop = Arc::new(AtomicBool::new(false));
    let thread_stop = Arc::clone(&stop);
    let handle = thread::spawn(move || {
        while !thread_stop.load(Ordering::SeqCst) {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    stream
                        .set_read_timeout(Some(Duration::from_secs(2)))
                        .expect("set fixture request timeout");
                    let request = read_http_request(&mut stream);
                    if request.is_empty() {
                        continue;
                    }
                    let body = if request.starts_with("GET /health ") {
                        json!({
                            "success": true,
                            "data": {
                                "pid": 4242,
                                "freshness": {
                                    "fresh": true,
                                    "restartable": true,
                                    "lease_id": "lease-live",
                                    "pid": 4242,
                                    "active_jobs": 2
                                }
                            }
                        })
                        .to_string()
                    } else {
                        assert!(request.starts_with("GET /runner/describe "), "{request}");
                        json!({"success":true,"data":{"body":runner_observation()}}).to_string()
                    };
                    write!(
                        stream,
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    )
                    .expect("write runner observation response");
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => panic!("accept fixture runner request: {error}"),
            }
        }
    });
    RunnerInventoryServer {
        url,
        port,
        stop,
        handle: Some(handle),
    }
}

fn read_http_request(stream: &mut std::net::TcpStream) -> String {
    let mut bytes = Vec::new();
    let mut chunk = [0; 1024];
    loop {
        let count = stream.read(&mut chunk).expect("read fixture request");
        if count == 0 {
            return String::new();
        }
        bytes.extend_from_slice(&chunk[..count]);
        if let Some(end) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            return String::from_utf8(bytes[..end].to_vec()).expect("request headers UTF-8");
        }
    }
}

fn runner_observation() -> Value {
    let active_runner_jobs = vec![
        runner_job("job-running", "running", "cargo test"),
        runner_job("job-queued", "queued", "cargo fmt"),
    ];
    let stale_runner_jobs = vec![runner_job("job-terminal", "succeeded", "cargo check")];
    json!({
        "schema": "homeboy/runner-service-observation/v1",
        "lease_id": "lease-live",
        "active_runner_job_count": active_runner_jobs.len(),
        "active_runner_jobs": active_runner_jobs,
        "stale_runner_jobs": stale_runner_jobs,
        "freshness": {
            "fresh": true,
            "restartable": true,
            "lease_id": "lease-live",
            "pid": 4242,
            "active_jobs": 2
        }
    })
}

fn runner_job(job_id: &str, status: &str, command: &str) -> Value {
    json!({
        "runner_id": RUNNER_ID,
        "job_id": job_id,
        "operation": "runner.exec",
        "source": "daemon",
        "kind": "runner.exec",
        "status": status,
        "command": command,
        "started_at_ms": 1,
        "updated_at_ms": 2,
        "elapsed_ms": 1,
        "heartbeat_age_ms": 0,
        "claim_id": format!("claim-{job_id}"),
        "claimed_by_runner_id": RUNNER_ID,
        "claimed_at_ms": 1,
        "durable_run_id": format!("run-{job_id}")
    })
}

fn write_runner_state(
    context: &HermeticTestContext,
    url: &str,
    port: u16,
    tunnel_pid: u32,
    tunnel_identity: homeboy_core::process::ProcessStartIdentity,
) {
    let config = json!({"kind":"local"});
    std::fs::write(
        context.runner_dir().join(format!("{RUNNER_ID}.json")),
        config.to_string(),
    )
    .expect("write runner config");

    let identity = match tunnel_identity {
        homeboy_core::process::ProcessStartIdentity::Linux { starttime_ticks } => {
            json!({"platform":"linux","starttime_ticks":starttime_ticks})
        }
        homeboy_core::process::ProcessStartIdentity::Macos {
            start_seconds,
            start_microseconds,
        } => json!({
            "platform":"macos",
            "start_seconds":start_seconds,
            "start_microseconds":start_microseconds
        }),
    };
    let current_session = session(url, port, tunnel_pid, identity.clone(), "lease-live");
    let session_path = context
        .config_dir()
        .join(format!("runner-sessions/{RUNNER_ID}/{CONTROLLER_ID}.json"));
    std::fs::create_dir_all(session_path.parent().expect("session parent"))
        .expect("create session directory");
    std::fs::write(&session_path, current_session.to_string()).expect("write runner session");

    let retained_session = session(url, port, tunnel_pid, identity, "lease-retained");
    let generations = json!({
        "runner_id": RUNNER_ID,
        "admission_owner": "lease-live",
        "generations": {
            "lease-live": {
                "endpoint": current_session,
                "active_jobs": 2,
                "observed_active_jobs": 2,
                "drain_state": "admitting"
            },
            "lease-retained": {
                "endpoint": retained_session,
                "active_jobs": 0,
                "observed_active_jobs": 0,
                "drain_state": "draining"
            }
        },
        "job_owners": { "job-retained": "lease-retained" }
    });
    std::fs::write(
        session_path.with_file_name("generations.json"),
        generations.to_string(),
    )
    .expect("write retained generation ownership fixture");
}

fn session(url: &str, port: u16, tunnel_pid: u32, identity: Value, lease_id: &str) -> Value {
    json!({
        "runner_id": RUNNER_ID,
        "mode": "direct_ssh",
        "role": "controller",
        "server_id": null,
        "controller_id": CONTROLLER_ID,
        "broker_url": null,
        "remote_daemon_address": "127.0.0.1:44000",
        "local_port": port,
        "local_url": url,
        "tunnel_pid": tunnel_pid,
        "tunnel_process_start_identity": identity,
        "remote_daemon_pid": 4242,
        "remote_daemon_lease_id": lease_id,
        "homeboy_version": env!("CARGO_PKG_VERSION"),
        "homeboy_build_identity": "homeboy fixture+test",
        "connected_at": "2026-01-01T00:00:00Z"
    })
}
