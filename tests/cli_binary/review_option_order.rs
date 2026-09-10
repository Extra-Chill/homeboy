use std::path::PathBuf;
use std::process::Command;

#[test]
fn review_test_accepts_shared_options_after_the_action_in_help_and_runtime_paths() {
    let home = tempfile::tempdir().expect("temporary home");
    let sentinel = home.path().join("runtime-initialized");

    let help = Command::new(homeboy_bin())
        .args([
            "review",
            "test",
            "--changed-only",
            "--placement=local",
            "--summary",
            "--help",
        ])
        .env_clear()
        .env("HOME", home.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_TEST_RUNTIME_INITIALIZATION_SENTINEL", &sentinel)
        .output()
        .expect("run review test help");

    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("Run tests for a component"),
        "{}",
        String::from_utf8_lossy(&help.stdout)
    );
    assert!(!sentinel.exists(), "help must not initialize the runtime");

    let runtime = Command::new(homeboy_bin())
        .args([
            "--placement=local",
            "review",
            "test",
            "missing-component",
            "--changed-only",
        ])
        .env_clear()
        .env("HOME", home.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run review test");

    assert!(
        !runtime.status.success(),
        "missing component must fail at runtime"
    );
    let combined = format!(
        "{}\n{}",
        String::from_utf8_lossy(&runtime.stdout),
        String::from_utf8_lossy(&runtime.stderr)
    );
    assert!(
        !combined.contains("unexpected argument '--changed-only'")
            && !combined.contains("unrecognized subcommand '--changed-only'"),
        "review test flags must parse before component resolution: {combined}"
    );
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
