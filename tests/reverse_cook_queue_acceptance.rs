use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use homeboy::core::api_jobs::{JobEventKind, JobStatus};
use homeboy_core::test_support::{HermeticTestContext, ReverseBrokerFixture, TestBinary};

/// A live reverse worker republishes its controller session heartbeat while it
/// is connected; the recorded `last_seen_at` is only as old as its last beat.
/// The fixture used to fake that with a single timestamp five minutes in the
/// future, which `reverse_controller_session_is_live` accepts because a
/// negative age fails `Duration::try_from` and falls open. That gave the test a
/// fixed ~390 s liveness window (300 s of future skew plus the 90 s heartbeat
/// TTL) while the test itself runs 430–590 s, so whether it passed depended on
/// where that wall-clock cliff landed. Beat the session honestly instead.
struct ReverseSessionHeartbeat {
    path: PathBuf,
    session: serde_json::Value,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl ReverseSessionHeartbeat {
    fn start(path: &Path, mut session: serde_json::Value) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        session["last_seen_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
        Self::write(path, &session);
        let handle = {
            let path = path.to_path_buf();
            let session = session.clone();
            let stop = stop.clone();
            std::thread::spawn(move || {
                while !stop.load(Ordering::SeqCst) {
                    std::thread::sleep(Duration::from_millis(500));
                    if stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let mut beat = session.clone();
                    beat["last_seen_at"] = serde_json::json!(chrono::Utc::now().to_rfc3339());
                    Self::write(&path, &beat);
                }
            })
        };
        Self {
            path: path.to_path_buf(),
            session,
            stop,
            handle: Some(handle),
        }
    }

    /// Stop beating and record the session a worker that has already exited
    /// leaves behind: a real `last_seen_at` older than the reverse heartbeat
    /// TTL. The controller must still project the terminal result the worker
    /// published to the broker before it exited.
    fn expire(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("reverse session heartbeat thread");
        }
        let mut expired = self.session.clone();
        expired["last_seen_at"] =
            serde_json::json!((chrono::Utc::now() - chrono::Duration::minutes(5)).to_rfc3339());
        Self::write(&self.path, &expired);
    }

    /// Publish through a same-directory rename. A truncating rewrite would let
    /// a controller read observe an empty session file mid-beat and report the
    /// runner disconnected for reasons that have nothing to do with liveness.
    fn write(path: &Path, session: &serde_json::Value) {
        let staged = path.with_extension("beat");
        std::fs::write(&staged, session.to_string()).expect("stage reverse controller session");
        std::fs::rename(&staged, path).expect("publish reverse controller session");
    }
}

impl Drop for ReverseSessionHeartbeat {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// The prepared-source cache is intentionally immutable while a worker uses it.
/// Restore owner write access before the hermetic fixture removes its checkout.
struct WritableTreeOnDrop(PathBuf);

impl Drop for WritableTreeOnDrop {
    fn drop(&mut self) {
        let _ = Command::new("chmod")
            .args(["-R", "u+w"])
            .arg(&self.0)
            .output();
    }
}

/// Wall-clock ledger for the acceptance run.
///
/// This test is the slowest binary in the suite and its deadlines are wall
/// clock, so a bare panic message says nothing about which phase consumed the
/// budget. Record each boundary and render the ledger into every panic so a CI
/// log is sufficient evidence — the machine that reproduces it is not
/// available to whoever reads the failure.
struct PhaseLedger {
    started: Instant,
    previous: Instant,
    phases: Vec<(&'static str, Duration)>,
}

impl PhaseLedger {
    fn new() -> Self {
        let now = Instant::now();
        Self {
            started: now,
            previous: now,
            phases: Vec::new(),
        }
    }

    fn mark(&mut self, phase: &'static str) {
        let now = Instant::now();
        self.phases.push((phase, now.duration_since(self.previous)));
        self.previous = now;
    }

    fn render(&self) -> String {
        let mut rendered = String::from("phase timings (seconds):");
        for (phase, elapsed) in &self.phases {
            rendered.push_str(&format!("\n  {phase}: {:.2}", elapsed.as_secs_f64()));
        }
        rendered.push_str(&format!(
            "\n  TOTAL: {:.2}",
            self.started.elapsed().as_secs_f64()
        ));
        rendered
    }
}

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
fn explicit_lab_route_persists_the_verified_lab_outcome_through_detached_cook_lifecycle() {
    use std::os::unix::fs::PermissionsExt;

    let mut ledger = PhaseLedger::new();
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
    std::env::set_var(
        "HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE",
        context.binary_path(TestBinary::HomeboyFixture),
    );
    std::env::set_var(
        "HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY",
        homeboy_core::build_identity::current().display,
    );
    let runtime_identity = std::env::var("HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY")
        .expect("fixture controller runtime identity");
    // The daemon, submitting CLI, and detached worker all pin the same
    // immutable controller binary under this context's data root.
    std::env::set_var("HOMEBOY_TEST_CONTROLLER_RUNTIME_USE_ENV", "1");
    ledger.mark("hermetic_context");
    let broker = ReverseBrokerFixture::start("lab");
    let (_checkout_guard, checkout) =
        homeboy_core::test_support::shared_committed_git_repo_fixture("cook-source");
    let _checkout_permissions = WritableTreeOnDrop(checkout.clone());
    std::fs::write(checkout.join(".gitignore"), "_lab_workspaces/\n")
        .expect("ignore runner workspace materialization");
    homeboy_core::test_support::run_git_fixture_command(&checkout, &["add", ".gitignore"]);
    homeboy_core::test_support::run_git_fixture_command(
        &checkout,
        &["commit", "-m", "ignore runner workspace"],
    );
    homeboy::core::component::inventory::write_standalone_registration(
        &homeboy::core::component::Component::new(
            "cook-source".to_string(),
            checkout.display().to_string(),
            String::new(),
            None,
        ),
    )
    .expect("register Cook source component");
    let task_worktree = context.root().join("cook-task");
    homeboy_core::test_support::run_git_fixture_command(
        &checkout,
        &[
            "worktree",
            "add",
            "-b",
            "cook-task",
            task_worktree.to_str().expect("task worktree path"),
        ],
    );
    ledger.mark("git_fixtures");
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
        "#!/bin/sh\nif [ \"${1:-}\" = -G ]; then\n  printf '%s\\n' 'hostname reverse-fixture.invalid' 'port 22' 'proxycommand fixture-proxy'\n  exit 0\nfi\nfor argument do command=$argument; done\ncase \"$command\" in\n  *'self identity'*) identity=\"${HOMEBOY_TEST_CONTROLLER_RUNTIME_IDENTITY:?}\"; version=\"${identity#homeboy }\"; version=\"${version%%+*}\"; printf '{\"version\":\"%s\",\"display\":\"%s\"}\\n' \"$version\" \"$identity\" ;;\n  *'daemon status'*) printf '%s\\n' '{\"success\":true,\"data\":{\"running\":false,\"fresh\":true,\"reachable\":true,\"active_jobs\":0}}' ;;\n  *'df -Pk'*) printf '%s\\n' 'fixture-device 5242880 1048576' ;;\n  *) exec /bin/sh -c \"$command\" ;;\nesac\n",
    )
    .expect("write capability probe SSH shim");
    std::fs::set_permissions(&ssh, std::fs::Permissions::from_mode(0o755))
        .expect("make capability probe SSH shim executable");
    let setsid = context.root().join("setsid");
    std::fs::write(
        &setsid,
        "#!/usr/bin/env perl\nuse POSIX qw(setsid);\nsetsid() or die \"setsid: $!\\n\";\nexec @ARGV or die \"exec: $!\\n\";\n",
    )
    .expect("write setsid shim");
    std::fs::set_permissions(&setsid, std::fs::Permissions::from_mode(0o755))
        .expect("make setsid shim executable");
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
    ledger.mark("daemon_ready");

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

    ledger.mark("server_and_runner_configured");
    let session_path = context
        .config_dir()
        .join("runner-sessions/lab/fixture-controller.json");
    std::fs::create_dir_all(session_path.parent().expect("session parent"))
        .expect("create session directory");
    // Beat the session for as long as the fixture worker is "connected", the
    // way `homeboy runner work` does, instead of pinning one future timestamp.
    let mut session_heartbeat = ReverseSessionHeartbeat::start(
        &session_path,
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
            "homeboy_build_identity": runtime_identity,
            "connected_at": "2026-01-01T00:00:00Z",
            "worker_identity": "fixture-worker",
            "worker_pid": 1,
        }),
    );

    let mut cook_command = context.command(TestBinary::HomeboyFixture);
    cook_command
        .env("PATH", &path)
        .env("HOMEBOY_CONTROLLER_ID", "fixture-controller")
        .args([
            "--detach-after-handoff",
            "agent-task",
            "cook",
            "--placement",
            "lab",
            "--repo",
            "cook-source",
            "--prompt",
            "Run the deterministic fixture provider.",
            "--backend",
            "fixture",
            "--cwd",
            task_worktree.to_str().expect("task worktree path"),
            "--to-worktree",
            task_worktree.to_str().expect("task worktree path"),
            "--provider-command",
            provider.to_str().expect("provider path"),
            "--verify",
            "true",
            "--max-attempts",
            "1",
            "--no-finalize",
            "--full",
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
    ledger.mark("detached_cook_cli");
    let cook_stdout = std::fs::read(&cook_stdout_path).expect("read Cook stdout");
    let cook_stderr = std::fs::read(&cook_stderr_path).expect("read Cook stderr");
    if !cook_status.success() {
        // The summary Cook view now carries `failure_context` (#11113), but it
        // deliberately withholds the provider and gate evidence behind
        // `--full`/`diagnose`. A failing Cook is exactly when the controller's
        // own record and the daemon it delegated to are worth reading, so
        // hydrate both before panicking.
        let reported_run_id = serde_json::from_slice::<serde_json::Value>(&cook_stdout)
            .ok()
            .and_then(|report| {
                report["cook_id"]
                    .as_str()
                    .or_else(|| report["latest_run_id"].as_str())
                    .map(str::to_string)
            });
        let full_status = reported_run_id.map(|run_id| {
            let status = context
                .command(TestBinary::HomeboyFixture)
                .env("PATH", &path)
                .args(["agent-task", "status", &run_id, "--full"])
                .output()
                .expect("inspect failed Cook run");
            format!(
                "stdout={}\nstderr={}",
                String::from_utf8_lossy(&status.stdout),
                String::from_utf8_lossy(&status.stderr),
            )
        });
        panic!(
            "detached Cook CLI failed\n{}\ncook stdout={}\ncook stderr={}\nagent-task status --full: {}\ndaemon stderr={}",
            ledger.render(),
            String::from_utf8_lossy(&cook_stdout),
            String::from_utf8_lossy(&cook_stderr),
            full_status.as_deref().unwrap_or("<no run id reported>"),
            std::fs::read_to_string(&daemon_stderr_path)
                .unwrap_or_else(|error| format!("<unavailable: {error}>")),
        );
    }
    let accepted: serde_json::Value = serde_json::from_slice(&cook_stdout).expect("cook JSON");
    assert_eq!(
        accepted["schema"],
        "homeboy/unmaterialized-cook-admission-result/v1"
    );
    assert!(matches!(
        accepted["status"].as_str(),
        Some("pending_resource_admission")
    ));
    assert!(matches!(
        accepted["admission_state"].as_str(),
        Some("queued" | "blocked_runner_unavailable")
    ));
    assert_eq!(accepted["materialized"], false);
    assert!(accepted["commands"]["status"].is_string());
    assert!(accepted["commands"]["cancel"].is_string());

    // The submitting CLI is gone before the reverse worker exists. The local
    // controller daemon must finish staging and durably enqueue the final job.
    let deadline = Instant::now() + Duration::from_secs(120);
    let queued = loop {
        let jobs = broker.jobs();
        if !jobs.is_empty() {
            break jobs;
        }
        if Instant::now() >= deadline {
            let run_id = accepted["run_id"].as_str().unwrap_or("unknown");
            let status = context
                .command(TestBinary::HomeboyFixture)
                .env("PATH", &path)
                .args(["agent-task", "status", run_id, "--full"])
                .output()
                .expect("inspect stalled controller parent");
            let status_json = serde_json::from_slice::<serde_json::Value>(&status.stdout).ok();
            let worker_log = status_json
                .as_ref()
                .and_then(|value| {
                    value.pointer(
                        "/data/metadata/unmaterialized_cook_admission/replay_receipt/worker_log",
                    )
                })
                .and_then(serde_json::Value::as_str)
                .and_then(|path| std::fs::read_to_string(path).ok())
                .unwrap_or_else(|| "<unavailable>".to_string());
            panic!(
                "controller did not enqueue reverse job\n{}\nstatus stdout={}\nstatus stderr={}\nworker stderr={}\ndaemon stderr={}",
                ledger.render(),
                String::from_utf8_lossy(&status.stdout),
                String::from_utf8_lossy(&status.stderr),
                worker_log,
                std::fs::read_to_string(&daemon_stderr_path)
                    .unwrap_or_else(|error| format!("<unavailable: {error}>")),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    ledger.mark("controller_enqueued_reverse_job");
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
    ledger.mark("reverse_worker_first_wave");
    assert!(worker.claimed);
    let completed = broker
        .store
        .get(queued[0].id)
        .expect("completed broker job");
    assert_eq!(completed.status, JobStatus::Succeeded);
    let private_at_files = checkout.join(".homeboy/lab-at-files");
    let retained_private_files = match std::fs::read_dir(&private_at_files) {
        Ok(entries) => entries
            .filter_map(std::result::Result::ok)
            .filter_map(|entry| entry.file_name().into_string().ok())
            .filter(|name| {
                name.starts_with("private-sha256-") || name.starts_with(".homeboy-verified-")
            })
            .collect::<Vec<_>>(),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
        Err(error) => panic!("read runner @file directory: {error}"),
    };
    assert!(
        retained_private_files.is_empty(),
        "worker removes private plan source and verified snapshot: {retained_private_files:?}"
    );
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
    let terminal_result = broker
        .store
        .events(completed.id)
        .expect("broker events")
        .into_iter()
        .find(|event| event.kind == JobEventKind::Result)
        .and_then(|event| event.data)
        .expect("broker terminal result event");
    assert!(
        terminal_result.get("exit_code").is_some(),
        "broker terminal result preserves the typed payload: {terminal_result}"
    );
    let broker_events: serde_json::Value =
        reqwest::blocking::get(format!("{}/jobs/{}/events", broker.url(), completed.id))
            .expect("fetch broker events over HTTP")
            .json()
            .expect("parse broker events response");
    let broker_terminal_result = broker_events
        .pointer("/data/body/events")
        .and_then(serde_json::Value::as_array)
        .and_then(|events| {
            events.iter().rev().find_map(|event| {
                (event["kind"] == serde_json::json!("result")).then(|| event["data"].clone())
            })
        })
        .expect("broker HTTP response retains terminal result");
    serde_json::from_value::<homeboy::core::api_jobs::RemoteRunnerJobResult>(
        broker_terminal_result.clone(),
    )
    .unwrap_or_else(|error| {
        panic!("broker HTTP terminal result must retain its typed contract: {error}\nresult={broker_terminal_result}")
    });

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
    ledger.mark("reverse_worker_duplicate_wave");
    assert_eq!(duplicate_code, 0);
    // Both worker waves have returned, so the reverse worker is gone and its
    // controller-session heartbeat stops. That is the steady state of a
    // detached Cook, not an edge case: the worker exits the moment it publishes
    // its terminal result. Record it explicitly so terminal projection is
    // proven against a genuinely expired session instead of racing one.
    session_heartbeat.expire();
    // The controller must project the broker result after the worker exits.
    // Read the record directly: `agent-task status` deliberately uses the
    // caller_opted_out probe policy, so invoking it here could not prove the
    // daemon's own controller-job provider registration.
    let cook_id = accepted["run_id"].as_str().expect("accepted Cook id");
    let parent =
        homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("open controller lifecycle store")
            .read_record(cook_id)
            .expect("read redirected admission parent");
    let run_id = parent.metadata["detached_cook_handoff"]["attempt_run_id"]
        .as_str()
        .expect("admission parent redirects to the materialized attempt");
    // Bound this on observations as well as wall clock. Requiring a minimum
    // number of durable reads keeps the assertion about controller progress,
    // rather than a single slow observation, while leaving failure bounded.
    const MINIMUM_TERMINAL_OBSERVATIONS: u32 = 8;
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut observations = 0u32;
    let terminal = loop {
        observations += 1;
        let record = homeboy::agents::agent_task_lifecycle::AgentTaskLifecycleStore::from_current_environment()
            .expect("open controller lifecycle store")
            .read_record(run_id)
            .expect("read controller parent record");
        if record.state.is_terminal() {
            break record;
        }
        if observations >= MINIMUM_TERMINAL_OBSERVATIONS && Instant::now() >= deadline {
            panic!(
                "controller did not project terminal broker result after {observations} observations\n{}\nrecord={record:#?}\ndaemon stderr={}",
                ledger.render(),
                std::fs::read_to_string(&daemon_stderr_path)
                    .unwrap_or_else(|error| format!("<unavailable: {error}>")),
            );
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    ledger.mark("controller_terminal_projection");
    assert_eq!(
        terminal.state,
        homeboy::agents::agent_task_lifecycle::AgentTaskRunState::Succeeded,
        "controller terminal projection: {terminal:#?}\n{}",
        ledger.render(),
    );
    let durable_record = terminal;
    let decision_id = durable_record.metadata["execution_placement_decision"]["decision_id"]
        .as_str()
        .expect("Lab placement decision persisted on the terminal run");
    assert_eq!(
        durable_record.metadata["execution_placement_decision"]["requested"],
        serde_json::json!("Lab"),
        "the durable decision must retain explicit Lab placement: {durable_record:#?}"
    );
    assert_eq!(
        durable_record.metadata["execution_placement_decision"]["runner"]["runner_id"],
        serde_json::json!("lab"),
        "the ready Lab runner must be selected for explicit Lab placement: {durable_record:#?}"
    );
    assert_eq!(
        durable_record.metadata["execution_placement_outcome"]["decision_id"],
        serde_json::json!(decision_id),
        "the completed provider outcome must bind to the persisted decision: {durable_record:#?}"
    );
    assert_eq!(
        durable_record.metadata["execution_placement_outcome"]["effective"],
        serde_json::json!("lab"),
        "the deterministic reverse worker must report a Lab outcome: {durable_record:#?}"
    );
    daemon.kill().expect("stop test-owned controller daemon");
    daemon.wait().expect("controller daemon fixture exits");
    // Slow-test findings key off this binary's duration (#10655). Publish the
    // phase ledger unconditionally so a passing run still explains where its
    // budget went; libtest hides it unless the test fails or `--nocapture` is
    // set, and `--nocapture` is exactly how this gets investigated.
    println!("{}", ledger.render());
}
