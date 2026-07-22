use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use homeboy::core::api_jobs::{JobEventKind, JobStatus};
use homeboy_core::test_support::{HermeticTestContext, ReverseBrokerFixture, TestBinary};

fn output(command: &mut Command) -> std::process::Output {
    let output = command.output().expect("run homeboy fixture command");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
    );
    output
}

fn wait_until<T>(timeout: Duration, mut inspect: impl FnMut() -> Option<T>) -> T {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(value) = inspect() {
            return value;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for fixture state"
        );
        std::thread::sleep(Duration::from_millis(25));
    }
}

fn json_field<'a>(value: &'a serde_json::Value, field: &str) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(entries) => entries
            .get(field)
            .or_else(|| entries.values().find_map(|value| json_field(value, field))),
        serde_json::Value::Array(entries) => {
            entries.iter().find_map(|value| json_field(value, field))
        }
        _ => None,
    }
}

#[cfg(unix)]
#[test]
fn detached_cook_accepts_reverse_capacity_queue_and_worker_completes_once() {
    use std::os::unix::fs::PermissionsExt;

    let _env_guard = homeboy_core::test_support::home_env_guard();
    let context = HermeticTestContext::new();
    std::env::set_var("HOME", context.home());
    std::env::set_var("XDG_CONFIG_HOME", context.root().join(".config"));
    std::env::set_var("XDG_DATA_HOME", context.root().join("data"));
    std::env::set_var("HOMEBOY_ARTIFACT_ROOT", context.artifact_dir());
    std::env::set_var("HOMEBOY_RUNTIME_TMPDIR", context.runtime_dir());
    std::env::set_var("TMPDIR", context.temp_dir());
    std::env::set_var(
        homeboy_core::daemon::DAEMON_BINARY_SHA_OVERRIDE_ENV,
        "0000000000000000000000000000000000000000000000000000000000000000",
    );
    let broker = ReverseBrokerFixture::start("lab");
    let (_checkout_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("cook-source");
    std::fs::write(checkout.join(".gitignore"), "_lab_workspaces/\n")
        .expect("ignore runner workspace materialization");
    homeboy_core::test_support::run_git_fixture_command(&checkout, &["add", ".gitignore"]);
    homeboy_core::test_support::run_git_fixture_command(
        &checkout,
        &["commit", "-m", "ignore runner workspace"],
    );
    let provider = context.root().join("provider.sh");
    std::fs::write(
        &provider,
        "#!/bin/sh\nset -eu\ncat >/dev/null\nprintf '%s\\n' '{\"schema\":\"homeboy/agent-task-outcome/v1\",\"status\":\"succeeded\",\"summary\":\"fixture provider completed\"}'\n",
    )
    .expect("write provider");
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o755))
        .expect("make provider executable");
    let ssh = context.root().join("ssh");
    std::fs::write(
        &ssh,
        "#!/bin/sh\nfor argument do command=$argument; done\ncase \"$command\" in\n  p=*'df -Pk'*) printf '%s\\n' '10485760 5242880' ;;\n  *) exec /bin/sh -c \"$command\" ;;\nesac\n",
    )
    .expect("write capability probe SSH shim");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
        .expect("make capability probe SSH shim executable");
    let path = format!(
        "{}:{}",
        context.root().display(),
        std::env::var("PATH").expect("PATH")
    );
    let daemon_stderr_path = context.root().join("daemon.stderr");
    let mut daemon = context
        .command(TestBinary::HomeboyFixture)
        .env("PATH", &path)
        .env("HOMEBOY_CONTROLLER_ID", "fixture-controller")
        .args(["daemon", "serve", "--addr", "127.0.0.1:0"])
        .stdout(Stdio::null())
        .stderr(Stdio::from(
            std::fs::File::create(&daemon_stderr_path).expect("create daemon stderr"),
        ))
        .spawn()
        .expect("start controller daemon fixture");
    let daemon_status = wait_until(Duration::from_secs(10), || {
        let output = context
            .command(TestBinary::HomeboyFixture)
            .args(["daemon", "status"])
            .output()
            .ok()?;
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).ok()?;
        (output.status.success() && json_field(&status, "running")?.as_bool()? == true)
            .then_some(status)
    });
    let daemon_lease_id = json_field(&daemon_status, "lease_id")
        .and_then(serde_json::Value::as_str)
        .expect("daemon lease id");
    let daemon_address = json_field(&daemon_status, "address")
        .and_then(serde_json::Value::as_str)
        .expect("daemon address");
    let daemon_pid = json_field(&daemon_status, "pid")
        .and_then(serde_json::Value::as_u64)
        .expect("daemon pid");

    output(context.command(TestBinary::HomeboyFixture).args([
        "server",
        "create",
        "lab",
        "--host",
        "reverse-fixture.invalid",
        "--user",
        "fixture",
    ]));
    output(
        context.command(TestBinary::HomeboyFixture).args([
            "runner",
            "enable",
            "lab",
            "--workspace-root",
            checkout.to_str().expect("checkout path"),
            "--concurrency-limit",
            "1",
            "--homeboy-path",
            context
                .binary_path(TestBinary::HomeboyFixture)
                .to_str()
                .expect("homeboy path"),
        ]),
    );

    let session_path = context
        .config_dir()
        .join("runner-sessions/lab/fixture-controller.json");
    std::fs::create_dir_all(session_path.parent().expect("session parent"))
        .expect("create session directory");
    std::fs::write(
        session_path,
        serde_json::json!({
            "runner_id": "lab",
            "mode": "reverse",
            "role": "controller",
            "controller_id": "fixture-controller",
            "broker_url": broker.url(),
            "remote_daemon_address": daemon_address,
            "remote_daemon_pid": daemon_pid,
            "remote_daemon_lease_id": daemon_lease_id,
            "homeboy_version": env!("CARGO_PKG_VERSION"),
            "homeboy_build_identity": null,
            "connected_at": "2026-01-01T00:00:00Z",
            "worker_identity": "fixture-worker",
            "worker_pid": 1,
            "last_seen_at": (chrono::Utc::now() + chrono::Duration::minutes(5)).to_rfc3339()
        })
        .to_string(),
    )
    .expect("write reverse controller session");

    let mut cook_command = context.command(TestBinary::HomeboyFixture);
    cook_command
        .env("PATH", path)
        .env("HOMEBOY_CONTROLLER_ID", "fixture-controller")
        .args([
            "--runner",
            "lab",
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--prompt",
            "Run the deterministic fixture provider.",
            "--backend",
            "fixture",
            "--cwd",
            checkout.to_str().expect("checkout path"),
            "--to-worktree",
            checkout.to_str().expect("checkout path"),
            "--provider-command",
            provider.to_str().expect("provider path"),
            "--verify",
            "true",
            "--max-attempts",
            "1",
            "--no-finalize",
        ]);
    let cook_stdout_path = context.root().join("cook.stdout");
    let cook_stderr_path = context.root().join("cook.stderr");
    let cook_status = cook_command
        .stdout(Stdio::from(
            std::fs::File::create(&cook_stdout_path).expect("create Cook stdout"),
        ))
        .stderr(Stdio::from(
            std::fs::File::create(&cook_stderr_path).expect("create Cook stderr"),
        ))
        .status()
        .expect("run Cook fixture command");
    let cook_stdout = std::fs::read(&cook_stdout_path).expect("read Cook stdout");
    let cook_stderr = std::fs::read(&cook_stderr_path).expect("read Cook stderr");
    assert!(
        cook_status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&cook_stdout),
        String::from_utf8_lossy(&cook_stderr),
    );
    let accepted: serde_json::Value = serde_json::from_slice(&cook_stdout).expect("cook JSON");
    assert!(
        accepted["status"] == "in_flight"
            || accepted.pointer("/data/status") == Some(&serde_json::json!("materializing")),
        "expected accepted durable staging lifecycle: {accepted}"
    );

    // The submitting CLI is gone before the reverse worker exists. The local
    // controller daemon must finish staging and durably enqueue the final job.
    let deadline = Instant::now() + Duration::from_secs(30);
    let queued = loop {
        let jobs = broker.jobs();
        if !jobs.is_empty() {
            break jobs;
        }
        if Instant::now() >= deadline {
            let run_id = accepted["latest_run_id"].as_str().unwrap_or("unknown");
            let status = context
                .command(TestBinary::HomeboyFixture)
                .args(["agent-task", "status", run_id])
                .output()
                .expect("inspect stalled controller parent");
            panic!(
                "controller did not enqueue reverse job\nstatus stdout={}\nstatus stderr={}",
                String::from_utf8_lossy(&status.stdout),
                String::from_utf8_lossy(&status.stderr),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(queued.len(), 1, "detached Cook submits one durable job");
    assert_eq!(queued[0].status, JobStatus::Queued);

    // The worker uses the same broker URL and store as the CLI subprocess.
    let (worker, code) =
        homeboy::runner::run_reverse_worker(homeboy::runner::ReverseRunnerWorkerOptions {
            runner_id: "lab".to_string(),
            broker_url: broker.url().to_string(),
            broker_token: None,
            project_id: None,
            lease_ms: 30_000,
            concurrency_limit: Some(1),
            loop_mode: false,
            idle_backoff_ms: 1,
            max_idle_backoff_ms: 10,
            broker_failure_backoff_ms: 1,
            broker_retry_limit: 1,
        })
        .expect("run reverse worker");
    assert_eq!(
        code,
        0,
        "worker={worker:#?} events={:#?}",
        broker.store.events(queued[0].id).expect("events")
    );
    assert!(worker.claimed);
    let completed = broker
        .store
        .get(queued[0].id)
        .expect("completed broker job");
    assert_eq!(completed.status, JobStatus::Succeeded);
    assert_eq!(
        broker
            .store
            .events(completed.id)
            .expect("broker events")
            .iter()
            .filter(|event| event.kind == JobEventKind::Result)
            .count(),
        1,
    );

    let (_, duplicate_code) =
        homeboy::runner::run_reverse_worker(homeboy::runner::ReverseRunnerWorkerOptions {
            runner_id: "lab".to_string(),
            broker_url: broker.url().to_string(),
            broker_token: None,
            project_id: None,
            lease_ms: 30_000,
            concurrency_limit: Some(1),
            loop_mode: false,
            idle_backoff_ms: 1,
            max_idle_backoff_ms: 10,
            broker_failure_backoff_ms: 1,
            broker_retry_limit: 1,
        })
        .expect("duplicate worker wake");
    assert_eq!(duplicate_code, 0);
    // The controller must project the broker result after the worker exits.
    // `daemon serve` is intentionally un-tokenized, so terminate the test-owned
    // foreground child only after that durable parent lifecycle is terminal.
    let run_id = accepted["latest_run_id"].as_str().expect("accepted run id");
    let deadline = Instant::now() + Duration::from_secs(10);
    let terminal = loop {
        let status = context
            .command(TestBinary::HomeboyFixture)
            .args(["agent-task", "status", run_id])
            .output()
            .expect("read terminal parent status");
        let parsed: serde_json::Value =
            serde_json::from_slice(&status.stdout).expect("parse terminal parent status");
        if matches!(
            parsed
                .pointer("/data/state")
                .and_then(serde_json::Value::as_str),
            Some("succeeded" | "failed" | "cancelled")
        ) {
            break parsed;
        }
        if Instant::now() >= deadline {
            panic!(
                "controller did not project terminal broker result\nstatus={}\ndaemon stderr={}",
                parsed,
                std::fs::read_to_string(&daemon_stderr_path)
                    .unwrap_or_else(|error| format!("<unavailable: {error}>")),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    assert_eq!(
        terminal
            .pointer("/data/state")
            .and_then(serde_json::Value::as_str),
        Some("succeeded"),
        "controller terminal projection: {terminal}"
    );
    daemon.kill().expect("stop test-owned controller daemon");
    daemon.wait().expect("controller daemon fixture exits");
}
