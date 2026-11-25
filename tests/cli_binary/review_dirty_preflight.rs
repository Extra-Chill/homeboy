use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

/// A detached local review acknowledges before its launcher exits; a fresh
/// watcher then observes the dirty-checkout rejection as a terminal error, and
/// the persisted run names the root cause.
#[test]
fn dirty_detached_review_acknowledges_then_terminalizes_with_its_root_cause() {
    let fixture = tempfile::tempdir().expect("fixture");
    let repository = fixture.path().join("repository");
    std::fs::create_dir_all(&repository).expect("repository");
    run_git(&repository, &["init", "-q", "--initial-branch", "main"]);
    run_git(
        &repository,
        &["config", "user.email", "homeboy@example.test"],
    );
    run_git(&repository, &["config", "user.name", "Homeboy Test"]);
    std::fs::write(repository.join("tracked.txt"), "initial\n").expect("tracked file");
    run_git(&repository, &["add", "tracked.txt"]);
    run_git(&repository, &["commit", "-q", "-m", "fixture"]);
    std::fs::write(repository.join("tracked.txt"), "dirty\n").expect("dirty file");

    let launcher = homeboy(fixture.path(), |command| {
        command
            .args(["--placement", "local", "review", "fixture", "--path"])
            .arg(&repository)
            .args(["--changed-only", "--summary"]);
    });
    assert_eq!(
        launcher.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&launcher.stdout),
        String::from_utf8_lossy(&launcher.stderr)
    );
    let handoff: Value = serde_json::from_slice(&launcher.stdout).expect("handoff JSON");
    assert_eq!(handoff["schema"], "homeboy/review-local-handoff/v1");
    let run_id = handoff["run_id"].as_str().expect("handoff run id");

    // The launcher has exited. Starting a new real CLI process immediately
    // exercises the watcher against the detached child's persisted ownership.
    let watch = homeboy(fixture.path(), |command| {
        command.args([
            "runs",
            "watch",
            run_id,
            "--interval",
            "10ms",
            "--timeout",
            "15s",
        ]);
    });
    assert_eq!(watch.status.code(), Some(1), "{watch:?}");
    let watched: Value = serde_json::from_slice(&watch.stdout).expect("watch JSON");
    assert_eq!(watched["data"]["payload"]["status"], "error", "{watched:#}");
    assert_eq!(watched["data"]["payload"]["terminal"], true, "{watched:#}");

    let show = homeboy(fixture.path(), |command| {
        command.args(["runs", "show", run_id]);
    });
    assert_eq!(show.status.code(), Some(0), "{show:?}");
    let persisted: Value = serde_json::from_slice(&show.stdout).expect("show JSON");
    let run = &persisted["data"]["payload"]["run"];
    assert_eq!(run["status"], "error", "{run:#}");
    assert!(
        run["metadata"]["operator_projection"]["root_cause"]["message"]
            .as_str()
            .is_some_and(|error| error.contains("Review tests require a clean component checkout")),
        "{run:#}"
    );
}

fn homeboy(home: &Path, configure: impl FnOnce(&mut Command)) -> Output {
    let mut command = Command::new(homeboy_bin());
    configure(&mut command);
    command
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run homeboy")
}

use homeboy_test_support::git::run as run_git;

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
