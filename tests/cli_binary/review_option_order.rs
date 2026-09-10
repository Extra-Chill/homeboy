use std::path::PathBuf;
use std::process::Command;

#[test]
fn review_test_help_and_execution_accept_the_same_options() {
    let home = tempfile::tempdir().expect("temporary home");
    let sentinel = home.path().join("runtime-initialized");

    let help = Command::new(homeboy_bin())
        .args([
            "review",
            "test",
            "--changed-since=-base",
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
    assert!(
        String::from_utf8_lossy(&help.stdout).contains("--changed-since"),
        "{}",
        String::from_utf8_lossy(&help.stdout)
    );
    assert!(
        !String::from_utf8_lossy(&help.stdout).contains("--changed-only"),
        "review test must not advertise an unsupported option: {}",
        String::from_utf8_lossy(&help.stdout)
    );
    assert!(!sentinel.exists(), "help must not initialize the runtime");

    let runtime = Command::new(homeboy_bin())
        .args([
            "--placement=local",
            "review",
            "test",
            "missing-component",
            "--changed-since=-base",
            "--summary",
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
        !combined.contains("unexpected argument '--changed-since'")
            && !combined.contains("unrecognized subcommand '--changed-since'"),
        "review test options advertised in help must parse before component resolution: {combined}"
    );

    let unsupported = Command::new(homeboy_bin())
        .args(["review", "test", "missing-component", "--changed-only"])
        .env_clear()
        .env("HOME", home.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run unsupported review test option");

    assert!(!unsupported.status.success());
    let unsupported_output = format!(
        "{}\n{}",
        String::from_utf8_lossy(&unsupported.stdout),
        String::from_utf8_lossy(&unsupported.stderr)
    );
    assert!(
        unsupported_output.contains("unexpected argument '--changed-only'"),
        "review test must reject options absent from its help: {unsupported_output}"
    );
}

#[test]
fn review_actions_accept_their_shared_post_action_options_in_help() {
    let home = tempfile::tempdir().expect("temporary home");

    for (action, target, option, expected) in [
        (
            "audit",
            "fixture",
            "--changed-since=main",
            "Audit code conventions",
        ),
        ("lint", "fixture", "--changed-only", "Lint a component"),
        (
            "test",
            "fixture",
            "--changed-since=main",
            "Run tests for a component",
        ),
        (
            "build",
            "fixture",
            "--changed-since=main",
            "Run a local build quality gate",
        ),
    ] {
        let output = Command::new(homeboy_bin())
            .args(["review", action, target, option, "--help"])
            .env_clear()
            .env("HOME", home.path())
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output()
            .expect("run review action help");

        assert!(
            output.status.success(),
            "review {action}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            String::from_utf8_lossy(&output.stdout).contains(expected),
            "review {action}: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}

#[test]
fn startup_help_applies_the_same_review_option_projection() {
    let home = tempfile::tempdir().expect("temporary home");
    let sentinel = home.path().join("runtime-initialized");

    let output = Command::new(homeboy_bin())
        .args(["review", "--changed-only", "test", "fixture", "--help"])
        .env_clear()
        .env("HOME", home.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("HOMEBOY_TEST_RUNTIME_INITIALIZATION_SENTINEL", &sentinel)
        .output()
        .expect("run invalid review test help");

    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("--changed-only is not supported by this review action"),
        "{stderr}"
    );
    assert!(
        !sentinel.exists(),
        "startup help must not initialize the runtime"
    );
}

#[test]
fn review_rejects_conflicting_parent_and_action_values_before_execution() {
    let home = tempfile::tempdir().expect("temporary home");

    for args in [
        [
            "review",
            "--path",
            "parent-path",
            "audit",
            "fixture",
            "--path",
            "child-path",
        ]
        .as_slice(),
        [
            "review",
            "--changed-since",
            "parent-base",
            "test",
            "fixture",
            "--changed-since",
            "child-base",
        ]
        .as_slice(),
        [
            "review",
            "--audit-profile",
            "full",
            "audit",
            "fixture",
            "--audit-profile",
            "pr",
        ]
        .as_slice(),
    ] {
        let output = Command::new(homeboy_bin())
            .args(args)
            .env_clear()
            .env("HOME", home.path())
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output()
            .expect("run conflicting review command");

        assert_eq!(output.status.code(), Some(2));
        let combined = format!(
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            combined.contains("validation.invalid_argument"),
            "{combined}"
        );
        assert!(combined.contains("conflicting"), "{combined}");
        assert!(
            !combined.contains("missing component") && !combined.contains("No files changed"),
            "conflicting scopes must fail before review execution: {combined}"
        );
    }
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
