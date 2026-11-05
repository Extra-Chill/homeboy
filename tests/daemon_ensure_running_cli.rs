use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use homeboy_core::api_jobs::{DaemonScope, JobStatus, JobStore};
use homeboy_core::test_support::{
    bounded_output, HermeticDaemonGuard, HermeticTestContext, TestBinary,
};

#[test]
fn on_demand_daemons_exit_idle_and_restart_without_accumulation() {
    let context = HermeticTestContext::new();
    let _daemon = HermeticDaemonGuard::new(&context, TestBinary::HomeboyFixture);
    for _ in 0..3 {
        let mut ensure = context.command(TestBinary::HomeboyFixture);
        ensure
            // Exercise production detachment. The bounded subprocess helper
            // otherwise reaps the daemon along with the successful launcher.
            .env_remove("HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP")
            .env("HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS", "2")
            .args(["daemon", "ensure-running"]);
        let output = bounded_output(ensure);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(
                started["data"]["state_path"]
                    .as_str()
                    .expect("selected generation path"),
            )
            .expect("live lease"),
        )
        .unwrap();
        let pid = state["pid"].as_u64().unwrap() as u32;
        wait_for_process_exit(pid);
        assert!(
            processes_holding_token(state["startup_token"].as_str().unwrap()).is_empty(),
            "an idle daemon leaves no process behind"
        );
        let mut status = context.command(TestBinary::HomeboyFixture);
        status.args(["daemon", "status"]);
        let output = bounded_output(status);
        let status: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(
            status["data"]["running"], false,
            "idle shutdown is observable"
        );
    }
}

#[test]
fn idle_shutdown_preserves_active_durable_jobs_then_reaps_after_completion() {
    let context = HermeticTestContext::new();
    let _daemon = HermeticDaemonGuard::new(&context, TestBinary::HomeboyFixture);
    let mut ensure = context.command(TestBinary::HomeboyFixture);
    ensure
        .env_remove("HOMEBOY_TEST_KEEP_DAEMON_IN_PROCESS_GROUP")
        .env("HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS", "3")
        .args(["daemon", "ensure-running"]);
    assert!(bounded_output(ensure).status.success());
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(context.daemon_dir().join("state.json")).unwrap())
            .unwrap();
    let pid = state["pid"].as_u64().unwrap() as u32;
    let jobs = JobStore::open_without_reconciliation(
        context
            .data_dir()
            .join(homeboy_core::paths::OBSERVATION_DB_FILENAME),
    )
    .unwrap()
    // Idle shutdown counts only its own scope's work (#16104).
    .with_daemon_scope(DaemonScope::from_state_dir(&context.daemon_dir()));
    let job = jobs.create("idle-lifetime-active-regression");
    jobs.start(job.id).unwrap();
    thread::sleep(Duration::from_secs(5));
    let alive_with_active_work = homeboy_core::process::pid_is_running(pid);
    let active_status = jobs.get(job.id).unwrap().status;
    jobs.cancel(job.id, "regression fixture finished").unwrap();
    assert!(
        alive_with_active_work,
        "active durable work keeps its owner alive"
    );
    assert_eq!(active_status, JobStatus::Running);
    wait_for_process_exit(pid);
    assert_eq!(jobs.get(job.id).unwrap().status, JobStatus::Cancelled);
}

/// Every process whose argv carries this exact startup token, by `ps`.
fn processes_holding_token(token: &str) -> Vec<(u32, String)> {
    let output = std::process::Command::new("ps")
        .args(["-axww", "-o", "pid=,command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let (pid, command) = line.trim().split_once(char::is_whitespace)?;
            command
                .split_whitespace()
                .collect::<Vec<_>>()
                .windows(2)
                .any(|pair| pair == ["--startup-token", token])
                .then(|| Some((pid.parse().ok()?, command.trim().to_string())))
                .flatten()
        })
        .collect()
}

/// #17412: the launched process is the daemon. Its PID is the lease PID, and
/// no second process (a `daemon supervise` wrapper) holds its startup token.
#[test]
fn the_launched_daemon_is_one_process_whose_pid_is_the_lease_pid() {
    let context = HermeticTestContext::new();
    let _daemon = HermeticDaemonGuard::new(&context, TestBinary::HomeboyFixture);
    let mut ensure = context.command(TestBinary::HomeboyFixture);
    ensure.args(["daemon", "ensure-running"]);
    let output = bounded_output(ensure);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let started: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let state: serde_json::Value =
        serde_json::from_slice(&fs::read(context.daemon_dir().join("state.json")).unwrap())
            .unwrap();
    let pid = state["pid"].as_u64().unwrap() as u32;
    assert_eq!(started["data"]["pid"].as_u64(), Some(u64::from(pid)));

    let holders = processes_holding_token(state["startup_token"].as_str().unwrap());
    assert_eq!(
        holders.iter().map(|(pid, _)| *pid).collect::<Vec<_>>(),
        vec![pid],
        "exactly the lease PID holds the startup token: {holders:?}"
    );
    let (_, command) = &holders[0];
    assert!(command.contains("daemon serve"), "{command}");
    assert!(!command.contains("daemon supervise"), "{command}");
}

/// #17412: with no supervisor capturing its output, a daemon that dies during
/// startup is diagnosed from the tail of `<state-dir>/daemon.log`.
#[test]
fn a_startup_failure_surfaces_the_daemon_log_tail() {
    let context = HermeticTestContext::new();
    let _daemon = HermeticDaemonGuard::new(&context, TestBinary::HomeboyFixture);
    // Holding the port makes `serve` fail to bind after it was launched.
    let occupied = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = occupied.local_addr().unwrap().to_string();
    let mut ensure = context.command(TestBinary::HomeboyFixture);
    ensure.args(["daemon", "ensure-running", "--addr", &addr]);
    let output = bounded_output(ensure);
    drop(occupied);

    assert!(
        !output.status.success(),
        "a daemon that cannot bind never starts"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    let envelope: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("error envelope ({error}): {stdout}"));
    let tail = find_key(&envelope, "daemon_log")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_else(|| panic!("startup failure carries the daemon.log tail: {stdout}"));
    assert!(
        tail.to_lowercase().contains("address already in use")
            || tail.to_lowercase().contains("addrinuse"),
        "the tail names the bind failure: {tail}"
    );
    let log = fs::read_to_string(context.daemon_dir().join("daemon.log")).unwrap();
    assert!(
        !log.is_empty(),
        "the daemon's own output lands in daemon.log"
    );
}

fn find_key<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => map
            .get(key)
            .filter(|found| !found.is_null())
            .or_else(|| map.values().find_map(|child| find_key(child, key))),
        serde_json::Value::Array(items) => items.iter().find_map(|child| find_key(child, key)),
        _ => None,
    }
}

fn wait_for_process_exit(pid: u32) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while homeboy_core::process::pid_is_running(pid) && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(100));
    }
    assert!(
        !homeboy_core::process::pid_is_running(pid),
        "daemon lifecycle must reap pid {pid}"
    );
}

#[test]
fn ensure_running_observes_the_isolated_daemon_startup_lease() {
    let context = HermeticTestContext::new();
    let mut ensure = context.command(TestBinary::HomeboyFixture);
    ensure.args(["daemon", "ensure-running"]);
    let ensure_output = bounded_output(ensure);

    let state = fs::read_to_string(context.daemon_dir().join("state.json"));

    let mut stop = context.command(TestBinary::HomeboyFixture);
    stop.args(["daemon", "stop"]);
    let stop_output = bounded_output(stop);

    assert!(
        ensure_output.status.success(),
        "daemon ensure-running failed: stdout={} stderr={}",
        String::from_utf8_lossy(&ensure_output.stdout),
        String::from_utf8_lossy(&ensure_output.stderr),
    );
    let state = state.expect("ensure-running must publish daemon state before returning");
    let state: serde_json::Value = serde_json::from_str(&state).expect("daemon state is JSON");
    assert!(
        state["startup_token"]
            .as_str()
            .is_some_and(|token| !token.is_empty()),
        "daemon state must retain the isolated startup token: {state}"
    );
    assert!(
        stop_output.status.success(),
        "daemon stop failed: stdout={} stderr={}",
        String::from_utf8_lossy(&stop_output.stdout),
        String::from_utf8_lossy(&stop_output.stderr),
    );
}

#[test]
fn ensure_running_in_second_controller_namespace_ignores_live_first_daemon() {
    let context = HermeticTestContext::new();
    let runner_root = context.config_dir().join("runner-daemons/shared-runner");
    let first_dir = runner_root.join("controller-a");
    let second_dir = runner_root.join("controller-b");

    let mut first_ensure = context.command(TestBinary::HomeboyFixture);
    first_ensure
        .env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, &first_dir)
        .env(
            homeboy_core::paths::HOMEBOY_DATA_DIR_ENV,
            first_dir.join("data"),
        )
        .args(["daemon", "ensure-running"]);
    let first_output = bounded_output(first_ensure);
    assert!(
        first_output.status.success(),
        "controller A ensure-running failed: {}",
        String::from_utf8_lossy(&first_output.stderr),
    );
    let first_state_path = first_dir.join("state.json");
    let first_state_bytes = fs::read(&first_state_path).expect("controller A lease exists");
    let first_state: serde_json::Value =
        serde_json::from_slice(&first_state_bytes).expect("controller A lease is JSON");
    let first_jobs_path = first_dir
        .join("data")
        .join(homeboy_core::paths::OBSERVATION_DB_FILENAME);
    let first_jobs = JobStore::open_without_reconciliation(&first_jobs_path)
        .expect("open controller A durable jobs store");
    let active_job = first_jobs.create("controller-a-active-job");
    let active_job = first_jobs.start(active_job.id).expect("start durable job");
    assert_eq!(active_job.status, JobStatus::Running);
    let first_jobs_before = serde_json::to_value((
        first_jobs.list(),
        first_jobs
            .events(active_job.id)
            .expect("controller A job events"),
    ))
    .expect("controller A jobs JSON");

    let mut second_ensure = context.command(TestBinary::HomeboyFixture);
    second_ensure
        .env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, &second_dir)
        .env(
            homeboy_core::paths::HOMEBOY_DATA_DIR_ENV,
            second_dir.join("data"),
        )
        .args(["daemon", "ensure-running"]);
    let second_output = bounded_output(second_ensure);
    assert!(
        second_output.status.success(),
        "controller B must start in its own namespace despite A's live daemon: stdout={} stderr={}",
        String::from_utf8_lossy(&second_output.stdout),
        String::from_utf8_lossy(&second_output.stderr),
    );

    let mut first_status = context.command(TestBinary::HomeboyFixture);
    first_status
        .env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, &first_dir)
        .env(
            homeboy_core::paths::HOMEBOY_DATA_DIR_ENV,
            first_dir.join("data"),
        )
        .args(["daemon", "status"]);
    let first_status_output = bounded_output(first_status);
    assert!(
        first_status_output.status.success(),
        "controller A status failed after B connected: {}",
        String::from_utf8_lossy(&first_status_output.stderr),
    );
    let second_state: serde_json::Value = serde_json::from_slice(
        &fs::read(second_dir.join("state.json")).expect("controller B lease exists"),
    )
    .expect("controller B lease is JSON");
    assert_ne!(first_state["pid"], second_state["pid"]);
    assert_ne!(first_state["lease_id"], second_state["lease_id"]);
    assert_eq!(
        fs::read(&first_state_path).expect("controller A lease remains"),
        first_state_bytes,
    );
    assert_eq!(
        {
            let reopened = JobStore::open_without_reconciliation(&first_jobs_path)
                .expect("reopen controller A jobs store");
            serde_json::to_value((
                reopened.list(),
                reopened
                    .events(active_job.id)
                    .expect("controller A job events"),
            ))
            .expect("controller A jobs JSON")
        },
        first_jobs_before,
        "controller B startup must not reconcile or rewrite controller A's jobs"
    );
    assert_eq!(
        JobStore::open_without_reconciliation(&first_jobs_path)
            .expect("reopen controller A jobs store")
            .get(active_job.id)
            .expect("controller A active job remains")
            .status,
        JobStatus::Running
    );

    for state_dir in [&second_dir, &first_dir] {
        let mut stop = context.command(TestBinary::HomeboyFixture);
        stop.env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, state_dir)
            .env(
                homeboy_core::paths::HOMEBOY_DATA_DIR_ENV,
                state_dir.join("data"),
            )
            .args(["daemon", "stop"]);
        let output = bounded_output(stop);
        assert!(
            output.status.success(),
            "isolated daemon stop failed: {}",
            String::from_utf8_lossy(&output.stderr),
        );
    }
}

#[test]
fn concurrent_hermetic_daemons_use_independent_lifecycle_namespaces() {
    let barrier = Arc::new(Barrier::new(3));
    let first = start_and_stop_hermetic_daemon(Arc::clone(&barrier));
    let second = start_and_stop_hermetic_daemon(Arc::clone(&barrier));
    barrier.wait();

    let first = first.join().expect("first daemon fixture thread");
    let second = second.join().expect("second daemon fixture thread");
    assert_daemon_lifecycle_completed(&first);
    assert_daemon_lifecycle_completed(&second);
    assert_ne!(first.0["pid"], second.0["pid"]);
    assert_ne!(first.1, second.1);
}

fn start_and_stop_hermetic_daemon(
    barrier: Arc<Barrier>,
) -> thread::JoinHandle<(
    serde_json::Value,
    std::path::PathBuf,
    std::process::Output,
    std::process::Output,
)> {
    thread::spawn(move || {
        let context = HermeticTestContext::new();
        barrier.wait();
        let mut ensure = context.command(TestBinary::HomeboyFixture);
        ensure.args(["daemon", "ensure-running"]);
        let ensure_output = bounded_output(ensure);
        let state_path = context.daemon_dir().join("state.json");
        let state = fs::read_to_string(&state_path)
            .ok()
            .and_then(|state| serde_json::from_str(&state).ok())
            .unwrap_or(serde_json::Value::Null);
        let mut stop = context.command(TestBinary::HomeboyFixture);
        stop.args(["daemon", "stop"]);
        let stop_output = bounded_output(stop);
        (state, state_path, ensure_output, stop_output)
    })
}

fn assert_daemon_lifecycle_completed(
    result: &(
        serde_json::Value,
        std::path::PathBuf,
        std::process::Output,
        std::process::Output,
    ),
) {
    assert!(
        result.2.status.success(),
        "ensure-running failed: {}",
        String::from_utf8_lossy(&result.2.stderr)
    );
    assert!(
        result.3.status.success(),
        "stop failed: {}",
        String::from_utf8_lossy(&result.3.stderr)
    );
    assert!(result.0["startup_token"]
        .as_str()
        .is_some_and(|token| !token.is_empty()));
    assert_eq!(result.0["state_path"], result.1.to_string_lossy().as_ref());
}
