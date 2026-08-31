use serde_json::Value;
use std::path::PathBuf;
use std::process::Command;

#[test]
fn unresolved_backend_preview_binds_stable_replay_lifecycle_without_mutation() {
    let home = tempfile::tempdir().expect("home");
    let output = Command::new(homeboy_bin())
        .args([
            "agent-task",
            "cook",
            "--repo",
            "homeboy",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/13490",
            "--head",
            "fix/13490-cook-preview-lifecycle-chubes",
            "--base",
            "main",
            "--goal",
            "Fix Cook preview lifecycle",
            "--verify",
            "cargo test -p homeboy-cli",
            "--preview",
        ])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run Cook preview");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let envelope: Value = serde_json::from_slice(&output.stdout).expect("preview JSON");
    assert_eq!(envelope["schema"], "homeboy/command-result/v3");
    assert_eq!(envelope["command"], "agent-task");
    assert_eq!(envelope["operation"], "cook");
    assert_eq!(envelope["success"], true);
    let preview = &envelope["data"];
    assert_eq!(preview["schema"], "homeboy/agent-task-cook-preview/v1");
    assert_eq!(preview["mutates"], false);
    assert_eq!(
        preview["resolved"]["provider"]["backend"]["default_policy"],
        "missing"
    );

    let replay = preview["replay_argv"]
        .as_array()
        .expect("preview replay argv");
    let run_id = replay_flag_value(replay, "--run-id");
    let attempt_run_id = replay_flag_value(replay, "--attempt-run-id");
    assert_eq!(run_id, attempt_run_id);
    assert!(run_id.starts_with("agent-task-"), "{run_id}");

    for choice in preview["resolved"]["provider"]["backend"]["ready_choices"]
        .as_array()
        .expect("ready backend choices")
    {
        let choice_replay = choice["replay_argv"]
            .as_array()
            .expect("choice replay argv");
        assert_eq!(replay_flag_value(choice_replay, "--run-id"), run_id);
        assert_eq!(
            replay_flag_value(choice_replay, "--attempt-run-id"),
            attempt_run_id
        );
    }

    assert_eq!(
        std::fs::read_dir(home.path())
            .expect("read isolated home")
            .count(),
        0,
        "preview must not create Homeboy state"
    );
}

fn replay_flag_value<'a>(argv: &'a [Value], flag: &str) -> &'a str {
    let matches = argv
        .windows(2)
        .filter(|pair| pair[0].as_str() == Some(flag))
        .collect::<Vec<_>>();
    assert_eq!(matches.len(), 1, "{flag} must occur exactly once: {argv:?}");
    matches[0][1].as_str().expect("replay flag value")
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
