use std::fs;
use std::sync::{Arc, Barrier};
use std::thread;

use homeboy_core::test_support::{bounded_output, HermeticTestContext, TestBinary};

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
