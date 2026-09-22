use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Child, Output};
use std::thread;
use std::time::{Duration, Instant};

use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};
use serde_json::{json, Value};

#[test]
fn real_daemon_loop_restart_resumes_one_admitted_revolution() {
    let context = HermeticTestContext::new();
    let root = context.root().to_path_buf();
    let spec = root.join("loop.json");
    std::fs::write(
        &spec,
        r#"{
          "schema": "homeboy/controller-spec/v1",
          "controller_id": "real-loop-restart",
          "phase": "repair",
          "actions": [{
            "action": "spawn_task",
            "dedupe_key": "restart-provider",
            "request": {
              "mode": "dispatch",
              "dispatch": { "backend": "restart-fixture", "prompt": "run" }
            }
          }]
        }"#,
    )
    .expect("loop spec");

    let invocation_marker = root.join("provider-invocations");
    let admission_marker = root.join("dispatch-admitted");
    let provider = json!({
        "id": "restart-fixture",
        "backend": "restart-fixture",
        "command_argv": [
            "sh", "-c",
            format!(
                "printf x >> {}; printf '%s' '{{\"schema\":\"homeboy/agent-task-outcome/v1\",\"task_id\":\"provider\",\"status\":\"succeeded\"}}'",
                invocation_marker.display()
            )
        ],
        "capabilities": ["structured_outcome"]
    });
    let port = free_port();
    let mut daemon = DaemonGuard::new(start_daemon(
        &context,
        port,
        Some(admission_marker.as_path()),
    ));
    wait_for("daemon HTTP socket", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });

    let defined = run_cli(
        &context,
        [
            "agent-task",
            "loop",
            "define",
            &format!("@{}", spec.to_str().expect("spec path")),
            "--on",
        ],
    );
    assert_success(&defined, "define loop");

    let action = json!({
        "schema": "homeboy/control-plane-action-request/v1",
        "effect_id": "loop-restart-effect",
        "action": "resume",
        "idempotency_key": "loop-restart-effect",
        "actor": "test",
        "parameters": {
            "schema": "homeboy/agent-task-loop-resume-parameters/v1",
            "data": {
                "dispatch_defaults": {
                    "backend": "restart-fixture",
                    "provider_catalog": { "providers": [provider] }
                }
            }
        },
        "confirmed": true
    });
    let response = post_json(
        port,
        "/v1/control-plane/runs/real-loop-restart/actions",
        action,
    );
    assert_eq!(
        response["data"]["body"]["resource"]["outcome"], "succeeded",
        "{response}"
    );
    let job_id = response["data"]["body"]["resource"]["result"]["data"]["aggregate"]["work"]
        ["job_id"]
        .as_str()
        .expect("admitted WorkJob")
        .to_string();
    wait_for("dispatch admission marker", || admission_marker.is_file());
    assert!(
        daemon_alive(daemon.as_mut()),
        "fixture daemon died before fault boundary"
    );

    // Only the fixture daemon is terminated. The persisted controller, work
    // job, provider marker, and candidate binary are left untouched.
    daemon.terminate();

    let mut restarted = DaemonGuard::new(start_daemon(&context, port, None));
    wait_for("restarted daemon HTTP socket", || {
        TcpStream::connect(("127.0.0.1", port)).is_ok()
    });
    let deadline = Instant::now() + Duration::from_secs(15);
    let mut last_job = Value::Null;
    while Instant::now() < deadline {
        last_job = get_json(port, &format!("/jobs/{job_id}"));
        if last_job.to_string().contains("\"status\":\"succeeded\"")
            || last_job.to_string().contains("\"status\":\"failed\"")
        {
            break;
        }
        thread::sleep(Duration::from_millis(25));
    }
    assert!(
        last_job.to_string().contains("\"status\":\"succeeded\"")
            || last_job.to_string().contains("\"status\":\"failed\""),
        "reconciled WorkJob: {last_job}"
    );
    let status = run_cli(
        &context,
        ["agent-task", "loop", "status", "real-loop-restart"],
    );
    assert_success(&status, "reconcile loop after daemon restart");
    let status_json = stdout_json(&status)["data"].clone();
    assert_eq!(status_json["runtime"]["revolutions"], 1);
    assert_eq!(
        status_json["status"]["controller"]["loop_id"],
        "real-loop-restart"
    );
    assert_eq!(status_json["status"]["controller"]["state"], "failed");
    assert_eq!(status_json["work"]["status"], "succeeded");
    assert_eq!(
        std::fs::read(&invocation_marker)
            .expect("provider marker")
            .len(),
        1
    );
    assert_eq!(count_work_jobs(&context), 1, "one durable WorkJob");
    assert!(
        daemon_alive(restarted.as_mut()),
        "restarted daemon exited unexpectedly"
    );
    restarted.terminate();
}

struct DaemonGuard(Option<Child>);

impl DaemonGuard {
    fn new(child: Child) -> Self {
        Self(Some(child))
    }

    fn as_mut(&mut self) -> &mut Child {
        self.0.as_mut().expect("daemon child")
    }

    fn terminate(&mut self) {
        if let Some(mut child) = self.0.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        self.terminate();
    }
}

fn start_daemon(
    context: &HermeticTestContext,
    port: u16,
    admission_marker: Option<&Path>,
) -> Child {
    let mut command = context.command(TestBinary::HomeboyFixture);
    command.args([
        "daemon",
        "serve",
        "--addr",
        &format!("127.0.0.1:{port}"),
        "--startup-token",
        "loop-restart-fixture",
        "--state-dir",
    ]);
    command.arg(context.daemon_dir());
    if let Some(marker) = admission_marker {
        command.env("HOMEBOY_TEST_LOOP_DISPATCH_ADMITTED", marker);
    } else {
        command.env_remove("HOMEBOY_TEST_LOOP_DISPATCH_ADMITTED");
    }
    command.spawn().expect("start candidate daemon")
}

fn run_cli<const N: usize>(context: &HermeticTestContext, args: [&str; N]) -> Output {
    let mut command = context.command(TestBinary::HomeboyFixture);
    command.args(args);
    bounded_output(command)
}

fn post_json(port: u16, path: &str, body: Value) -> Value {
    let client = reqwest::blocking::Client::new();
    client
        .post(format!("http://127.0.0.1:{port}{path}"))
        .json(&body)
        .send()
        .expect("HTTP action")
        .json()
        .expect("HTTP JSON response")
}

fn get_json(port: u16, path: &str) -> Value {
    reqwest::blocking::Client::new()
        .get(format!("http://127.0.0.1:{port}{path}"))
        .send()
        .expect("HTTP read")
        .json()
        .expect("HTTP JSON read response")
}

fn count_work_jobs(context: &HermeticTestContext) -> usize {
    let jobs =
        std::fs::read_to_string(context.daemon_dir().join("jobs.json")).expect("durable jobs");
    let value: Value = serde_json::from_str(&jobs).expect("jobs JSON");
    value["jobs"].as_array().expect("jobs array").len()
}

fn stdout_json(output: &Output) -> Value {
    serde_json::from_slice(&output.stdout).expect("CLI JSON")
}

fn assert_success(output: &Output, label: &str) {
    assert!(
        output.status.success(),
        "{label} failed: stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn daemon_alive(child: &mut Child) -> bool {
    child.try_wait().expect("inspect daemon").is_none()
}

fn wait_for(label: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if condition() {
            return;
        }
        thread::sleep(Duration::from_millis(25));
    }
    panic!("timed out waiting for {label}");
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0")
        .expect("free port")
        .local_addr()
        .expect("local address")
        .port()
}
