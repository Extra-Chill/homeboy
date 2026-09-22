use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn release_version_show_has_a_bounded_wall_clock_budget() {
    let started = Instant::now();
    let output = Command::new(env!("CARGO_BIN_EXE_homeboy"))
        .args(["release", "version", "show"])
        .env("HOMEBOY_RELEASE_DEADLINE_SECS", "2")
        .output()
        .expect("run release version show");

    assert!(
        started.elapsed() < Duration::from_secs(8),
        "release version show exceeded the regression-test budget: {:?}",
        started.elapsed()
    );
    assert_eq!(output.status.code(), Some(124));
    let envelope: serde_json::Value =
        serde_json::from_slice(&output.stdout).expect("timeout must emit JSON envelope");
    assert_eq!(envelope["schema"], "homeboy/command-result/v3");
    assert_eq!(envelope["success"], false);
    assert_eq!(envelope["exit_code"], 124);
    assert!(envelope["diagnostics"]["details"]["error"]
        .as_str()
        .expect("timeout cause")
        .contains("version show: resolving component"));
}
