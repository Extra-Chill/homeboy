use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;
use std::time::{Duration, Instant};

use homeboy_core::api_jobs::{JobStatus, JobStore};
use homeboy_core::test_support::{
    bounded_output, HermeticDaemonGuard, HermeticTestContext, TestBinary,
};

#[test]
fn on_demand_daemons_reap_their_supervisor_and_restart_without_accumulation() {
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
        let state: serde_json::Value = serde_json::from_slice(
            &fs::read(context.daemon_dir().join("state.json")).expect("live lease"),
        )
        .unwrap();
        let pid = state["pid"].as_u64().unwrap() as u32;
        let parent = parent_pid(pid);
        wait_for_process_exit(pid);
        if let Some(parent) = parent {
            wait_for_process_exit(parent);
        }
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
    let jobs =
        JobStore::open_without_reconciliation(context.daemon_dir().join("jobs.json")).unwrap();
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

fn parent_pid(pid: u32) -> Option<u32> {
    let output = std::process::Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "ppid="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&output.stdout).trim().parse().ok()
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
    let runner_root = context
        .config_dir()
        .join("daemon-generations/shared-runner/controllers");
    let first_dir = runner_root.join("controller-a/primary");
    let second_dir = runner_root.join("controller-b/primary");

    let mut first_ensure = context.command(TestBinary::HomeboyFixture);
    first_ensure
        .env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, &first_dir)
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
    let first_jobs_path = first_dir.join("jobs.json");
    let first_jobs = JobStore::open_without_reconciliation(&first_jobs_path)
        .expect("open controller A durable jobs store");
    let active_job = first_jobs.create("controller-a-active-job");
    let active_job = first_jobs.start(active_job.id).expect("start durable job");
    assert_eq!(active_job.status, JobStatus::Running);
    let first_jobs_before = fs::read(&first_jobs_path).expect("controller A jobs store exists");

    let mut second_ensure = context.command(TestBinary::HomeboyFixture);
    second_ensure
        .env(homeboy_core::paths::DAEMON_STATE_DIR_ENV, &second_dir)
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
        fs::read(&first_jobs_path).expect("controller A jobs store remains"),
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
