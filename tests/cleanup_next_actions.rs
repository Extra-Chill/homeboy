use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

#[test]
#[cfg(unix)]
fn aggregate_repo_artifact_next_action_runs_outside_a_checkout() {
    let fixture = tempfile::tempdir().expect("fixture");
    let repository = fixture.path().join("repository");
    let invocation_dir = fixture.path().join("operator-cwd");
    let components_dir = fixture.path().join(".config/homeboy/components");
    let tools = fixture.path().join("tools");
    std::fs::create_dir_all(repository.join("target/debug")).expect("target directory");
    std::fs::create_dir_all(&invocation_dir).expect("operator directory");
    std::fs::create_dir_all(&components_dir).expect("components directory");
    std::fs::create_dir_all(&tools).expect("tools directory");
    write_executable(&tools.join("ps"), "#!/bin/sh\nexit 0\n");
    let path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );
    std::fs::write(repository.join(".gitignore"), "target/\n").expect("target ignore rule");
    std::fs::write(repository.join("target/debug/app"), "artifact").expect("target artifact");
    run_git(&repository, &["init", "-b", "main"]);
    run_git(&repository, &["add", ".gitignore"]);
    run_git(
        &repository,
        &[
            "-c",
            "user.name=Homeboy Test",
            "-c",
            "user.email=homeboy@example.test",
            "commit",
            "-m",
            "initial",
        ],
    );
    std::fs::write(
        components_dir.join("fixture.json"),
        serde_json::to_vec(&serde_json::json!({
            "local_path": repository,
            "remote_path": "fixture"
        }))
        .expect("component JSON"),
    )
    .expect("component registration");

    let inventory = run_cleanup_with_path(
        fixture.path(),
        &invocation_dir,
        &["--include", "repo-artifacts"],
        &path,
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
    let applied = run_cleanup_with_path(fixture.path(), &invocation_dir, &args, &path);
    assert_eq!(applied["success"], true, "{applied:#}");
    let job_id = applied
        .pointer("/data/job_id")
        .and_then(Value::as_str)
        .expect("cleanup job ID");
    let completed = wait_for_cleanup_job(fixture.path(), &invocation_dir, job_id, &path);
    assert_eq!(completed["data"]["status"], "succeeded", "{completed:#}");
    let artifact_removed = !repository.join("target").exists();
    let stopped = run_homeboy(fixture.path(), &invocation_dir, &["daemon", "stop"], &path);
    assert_eq!(stopped["success"], true, "{stopped:#}");
    assert!(artifact_removed);
}

#[test]
fn aggregate_repo_artifact_next_action_excludes_unignored_work() {
    let fixture = tempfile::tempdir().expect("fixture");
    let repository = fixture.path().join("repository");
    let invocation_dir = fixture.path().join("operator-cwd");
    let components_dir = fixture.path().join(".config/homeboy/components");
    std::fs::create_dir_all(repository.join("target/debug")).expect("target directory");
    std::fs::create_dir_all(&invocation_dir).expect("operator directory");
    std::fs::create_dir_all(&components_dir).expect("components directory");
    std::fs::write(repository.join("target/debug/notes.txt"), "operator work")
        .expect("unignored work");
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
    assert!(
        inventory
            .get("next_actions")
            .is_none_or(|actions| actions.as_array().is_some_and(Vec::is_empty)),
        "{inventory:#}"
    );
    assert!(inventory["data"]["categories"][0]["output"]
        .as_array()
        .expect("repo artifact diagnostics")[0]["output"]["skipped"]
        .as_array()
        .expect("skipped artifacts")
        .iter()
        .any(|row| row["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("untracked work"))));
    assert!(repository.join("target/debug/notes.txt").exists());
}

#[test]
#[cfg(unix)]
fn aggregate_runner_binary_cache_next_action_applies_owned_candidate() {
    let fixture = tempfile::tempdir().expect("fixture");
    let invocation_dir = fixture.path().join("operator-cwd");
    let slot = invocation_dir.join("_homeboy_binaries/homeboy-old");
    let binary = slot.join("target/release/homeboy");
    let tools = fixture.path().join("tools");
    std::fs::create_dir_all(&invocation_dir).expect("operator directory");
    std::fs::create_dir_all(binary.parent().expect("binary parent")).expect("slot directory");
    std::fs::create_dir_all(&tools).expect("tools directory");
    std::fs::write(&binary, "binary").expect("cached binary");
    write_executable(&tools.join("lsof"), "#!/bin/sh\nexit 1\n");
    let touch = Command::new("touch")
        .args(["-t", "202001010000", slot.to_str().expect("slot path")])
        .output()
        .expect("age cache slot");
    assert!(
        touch.status.success(),
        "{}",
        String::from_utf8_lossy(&touch.stderr)
    );
    let path = format!(
        "{}:{}",
        tools.display(),
        std::env::var("PATH").unwrap_or_default()
    );

    let inventory = run_cleanup_with_path(
        fixture.path(),
        &invocation_dir,
        &["--include", "runner-binary-caches"],
        &path,
    );
    assert_eq!(inventory["success"], true, "{inventory:#}");
    assert_eq!(inventory["data"]["candidate_count"], 1);
    assert!(slot.exists());
    let next_command = inventory["next_actions"][0]["command"]
        .as_str()
        .expect("runner cache next action");
    assert_eq!(next_command, "homeboy runner cache-prune local --apply");

    let args: Vec<_> = next_command.split_whitespace().skip(1).collect();
    let applied = run_homeboy(fixture.path(), &invocation_dir, &args, &path);
    assert_eq!(applied["success"], true, "{applied:#}");
    assert!(!slot.exists());
}

fn run_cleanup(home: &Path, cwd: &Path, args: &[&str]) -> Value {
    run_cleanup_with_path(home, cwd, args, &std::env::var("PATH").unwrap_or_default())
}

fn run_cleanup_with_path(home: &Path, cwd: &Path, args: &[&str], path: &str) -> Value {
    let mut command = vec!["cleanup"];
    command.extend_from_slice(args);
    run_homeboy(home, cwd, &command, path)
}

fn run_homeboy(home: &Path, cwd: &Path, args: &[&str], path: &str) -> Value {
    let output = Command::new(homeboy_bin())
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .env("PATH", path)
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

fn wait_for_cleanup_job(home: &Path, cwd: &Path, job_id: &str, path: &str) -> Value {
    let mut latest = Value::Null;
    for _ in 0..100 {
        latest = run_cleanup_with_path(home, cwd, &["status", job_id], path);
        if latest
            .pointer("/data/status")
            .and_then(Value::as_str)
            .is_some_and(|status| matches!(status, "succeeded" | "failed" | "cancelled"))
        {
            return latest;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    panic!("cleanup job did not finish: {latest:#}");
}

#[cfg(unix)]
fn write_executable(path: &Path, contents: &str) {
    use std::os::unix::fs::PermissionsExt;

    std::fs::write(path, contents).expect("write executable");
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
        .expect("set executable permissions");
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
