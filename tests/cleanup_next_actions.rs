use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
fn aggregate_repo_artifact_next_action_runs_outside_a_checkout() {
    let fixture = tempfile::tempdir().expect("fixture");
    let repository = fixture.path().join("repository");
    let invocation_dir = fixture.path().join("operator-cwd");
    let components_dir = fixture.path().join(".config/homeboy/components");
    std::fs::create_dir_all(repository.join("target/debug")).expect("target directory");
    std::fs::create_dir_all(&invocation_dir).expect("operator directory");
    std::fs::create_dir_all(&components_dir).expect("components directory");
    std::fs::write(repository.join("target/debug/app"), "artifact").expect("target artifact");
    run_git(&repository, &["init", "-b", "main"]);
    std::fs::write(
        components_dir.join("fixture.json"),
        serde_json::to_vec(&serde_json::json!({
            "local_path": repository,
            "remote_path": "fixture"
        }))
        .expect("component JSON"),
    )
    .expect("component registration");

    let inventory = run_cleanup(
        fixture.path(),
        &invocation_dir,
        &["--include", "repo-artifacts"],
    );
    assert_eq!(inventory["success"], true, "{inventory:#}");
    assert_eq!(
        inventory["data"]["categories"][0]["specialist_command"],
        "homeboy cleanup artifacts"
    );
    assert_eq!(
        inventory["data"]["categories"][0]["canonical_cleanup_command"],
        "homeboy cleanup --include repo-artifacts"
    );

    let next_command = inventory["next_actions"][0]["command"]
        .as_str()
        .expect("repo artifact next action");
    assert_eq!(
        next_command,
        "homeboy cleanup --include repo-artifacts --apply"
    );

    let args: Vec<_> = next_command.split_whitespace().skip(2).collect();
    let applied = run_cleanup(fixture.path(), &invocation_dir, &args);
    assert_eq!(applied["success"], true, "{applied:#}");
    assert!(!repository.join("target").exists());
}

fn run_cleanup(home: &Path, cwd: &Path, args: &[&str]) -> Value {
    let output = Command::new(homeboy_bin())
        .arg("cleanup")
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run cleanup");
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "cleanup output was not JSON ({error}); status={:?}; stderr={}",
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        )
    })
}

fn run_git(path: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
