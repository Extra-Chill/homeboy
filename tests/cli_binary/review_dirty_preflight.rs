use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn dirty_umbrella_review_rejects_before_dependency_setup_or_stages() {
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

    let launcher = Command::new(homeboy_bin())
        .args(["--placement", "local", "review", "fixture", "--path"])
        .arg(&repository)
        .args(["--changed-only", "--summary"])
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path().join(".config"))
        .env("XDG_DATA_HOME", fixture.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("launch detached review");

    assert_eq!(launcher.status.code(), Some(0), "{launcher:?}");
    let handoff: Value = serde_json::from_slice(&launcher.stdout).expect("handoff JSON");
    let run_id = handoff["run_id"].as_str().expect("handoff run id");

    let watch = Command::new(homeboy_bin())
        .args([
            "runs",
            "watch",
            run_id,
            "--interval",
            "10ms",
            "--timeout",
            "15s",
        ])
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path().join(".config"))
        .env("XDG_DATA_HOME", fixture.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("watch dirty review");

    assert_eq!(watch.status.code(), Some(1), "{watch:?}");
    let watched: Value = serde_json::from_slice(&watch.stdout).expect("watch JSON");
    assert_eq!(watched["data"]["payload"]["status"], "error", "{watched:#}");
    assert_eq!(watched["data"]["payload"]["terminal"], true, "{watched:#}");

    let show = Command::new(homeboy_bin())
        .args(["runs", "show", run_id])
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path().join(".config"))
        .env("XDG_DATA_HOME", fixture.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("show dirty review");
    assert_eq!(show.status.code(), Some(0), "{show:?}");
    let persisted: Value = serde_json::from_slice(&show.stdout).expect("show JSON");
    let metadata = &persisted["data"]["payload"]["run"]["metadata"];
    assert!(
        metadata["operator_projection"]["root_cause"]["message"]
            .as_str()
            .is_some_and(|error| error.contains("Review tests require a clean component checkout")),
        "{metadata:#}"
    );
}

#[test]
fn detached_review_acknowledges_before_launcher_exit_and_watcher_observes_terminal_error() {
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

    let launcher = Command::new(homeboy_bin())
        .args(["--placement", "local", "review", "fixture", "--path"])
        .arg(&repository)
        .args(["--changed-only", "--summary"])
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path().join(".config"))
        .env("XDG_DATA_HOME", fixture.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("launch detached review");
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
    let watch = Command::new(homeboy_bin())
        .args([
            "runs",
            "watch",
            run_id,
            "--interval",
            "10ms",
            "--timeout",
            "15s",
        ])
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path().join(".config"))
        .env("XDG_DATA_HOME", fixture.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("watch detached review");
    assert_eq!(watch.status.code(), Some(1), "{watch:?}");
    let watched: Value = serde_json::from_slice(&watch.stdout).expect("watch JSON");
    assert_eq!(watched["data"]["payload"]["status"], "error", "{watched:#}");
    assert_eq!(watched["data"]["payload"]["terminal"], true, "{watched:#}");

    let show = Command::new(homeboy_bin())
        .args(["runs", "show", run_id])
        .env("HOME", fixture.path())
        .env("XDG_CONFIG_HOME", fixture.path().join(".config"))
        .env("XDG_DATA_HOME", fixture.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("show terminal review");
    assert_eq!(show.status.code(), Some(0), "{show:?}");
    let persisted: Value = serde_json::from_slice(&show.stdout).expect("show JSON");
    assert_eq!(persisted["data"]["payload"]["run"]["status"], "error");
}

fn run_git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
