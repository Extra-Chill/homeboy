use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn cook_rejects_empty_whitespace_padded_prompt_stdin_before_destination_or_provider_preflight() {
    let output = Command::new(homeboy_bin())
        .args([
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--prompt",
            " \t-\n ",
            "--backend",
            "fixture",
            "--no-finalize",
        ])
        .stdin(Stdio::piped())
        .env("HOME", tempfile::tempdir().expect("home").path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run homeboy");

    assert!(!output.status.success(), "{output:?}");
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stdout.contains("agent-task cook --prompt - received empty stdin")
            || stderr.contains("agent-task cook --prompt - received empty stdin"),
        "stdout: {stdout}\nstderr: {stderr}"
    );
    assert!(
        !stdout.contains("provider") && !stderr.contains("provider"),
        "empty stdin must fail before provider preflight\nstdout: {stdout}\nstderr: {stderr}"
    );
}

#[test]
fn cook_snapshots_multiline_whitespace_padded_stdin_once_in_the_durable_recipe() {
    let home = tempfile::tempdir().expect("home");
    let config_dir = home.path().join(".config/homeboy");
    std::fs::create_dir_all(&config_dir).expect("create fixture config directory");
    std::fs::write(
        config_dir.join("homeboy.json"),
        r#"{"retention":{"reconstructable_artifact_reserve_bytes":0}}"#,
    )
    .expect("disable host-capacity admission for fixture worktree");
    let extension_dir = config_dir.join("extensions/fixture");
    std::fs::create_dir_all(&extension_dir).expect("create fixture extension directory");
    std::fs::write(
        extension_dir.join("fixture.json"),
        r#"{
  "name": "Fixture provider",
  "version": "1.0.0",
  "agent_runtimes": [{
    "id": "fixture/v1",
    "agent_task_executors": [{
      "schema": "homeboy/agent-task-executor-provider/v1",
      "id": "fixture.default",
      "backend": "fixture",
      "invocation": {"argv": ["sh", "-c", "exit 1"]},
      "request_schema": "homeboy/agent-task-request/v1",
      "outcome_schema": "homeboy/agent-task-outcome/v1"
    }]
  }]
}"#,
    )
    .expect("write fixture executor manifest");
    let source = tempfile::tempdir().expect("source checkout");
    git(source.path(), &["init", "-b", "main"]);
    git(
        source.path(),
        &["config", "user.email", "test@example.invalid"],
    );
    git(source.path(), &["config", "user.name", "Homeboy test"]);
    std::fs::write(source.path().join("README.md"), "fixture\n").expect("write fixture");
    git(source.path(), &["add", "README.md"]);
    git(source.path(), &["commit", "-m", "fixture"]);
    let target = source.path().join("cook-target");
    git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "fix/stdin-snapshot",
            target.to_str().expect("target path"),
            "HEAD",
        ],
    );

    let prompt = "# Preserve bytes\n\nUse `cargo test` and $HOME exactly.\n";
    let mut child = Command::new(homeboy_bin())
        .args([
            "--placement",
            "local",
            "agent-task",
            "cook",
            "--run-id",
            "stdin-snapshot",
            "--prompt",
            " \t-\n ",
            "--backend",
            "fixture",
            "--repo",
            "stdin-fixture",
            "--cwd",
            target.to_str().expect("target path"),
            "--to-worktree",
            target.to_str().expect("target path"),
            "--no-finalize",
            "--no-progress",
        ])
        .stdin(Stdio::piped())
        .env("HOME", home.path())
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .spawn()
        .expect("run homeboy");
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(prompt.as_bytes())
        .expect("write prompt");
    let output = child.wait_with_output().expect("wait for homeboy");
    // The fixture provider fails after durable recipe creation. This test owns
    // the prompt bytes persisted before provider execution, not promotion.
    assert!(!output.status.success(), "{output:?}");

    let recipe = home
        .path()
        .join(".local/share/homeboy/agent-task-cooks/stdin-snapshot/recipe.json");
    let recipe: serde_json::Value =
        serde_json::from_slice(&std::fs::read(recipe).expect("recipe")).expect("recipe JSON");
    assert_eq!(
        recipe["attempts"][0]["plan"]["tasks"][0]["instructions"],
        prompt
    );
    assert_eq!(
        recipe["attempts"][0]["plan"]["tasks"][0]["metadata"]["prompt_source"],
        " \t-\n "
    );
    assert_eq!(
        recipe["attempts"][0]["plan"]["metadata"]["prompt_input_v1"]["source"],
        "stdin"
    );
    assert_eq!(
        recipe["attempts"][0]["plan"]["metadata"]["prompt_input_v1"]["sha256"],
        format!(
            "sha256:{}",
            homeboy_engine_primitives::content_hash::sha256_hex(prompt.as_bytes())
        )
    );
}

fn git(directory: &std::path::Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(directory)
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?}");
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
