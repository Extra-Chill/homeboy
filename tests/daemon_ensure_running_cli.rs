use std::fs;

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
