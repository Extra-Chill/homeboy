use serde_json::Value;
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

#[test]
fn unresolved_backend_preview_binds_stable_replay_lifecycle_without_mutation() {
    let home = tempfile::tempdir().expect("home");
    let output_dir = tempfile::tempdir().expect("output directory");
    let output_path = output_dir.path().join("preview.json");
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
            "--output",
            output_path.to_str().expect("output path"),
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
        serde_json::from_str::<Value>(
            &std::fs::read_to_string(&output_path).expect("preview output")
        )
        .expect("preview output JSON")["data"]["mutates"],
        false,
        "--output must contain the read-only terminal preview"
    );
    assert!(
        !output_dir
            .path()
            .join(".preview.json.homeboy-cook-output.lock")
            .exists(),
        "preview must not create a Cook output lease"
    );
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

#[test]
fn stdin_prompt_preview_declares_replay_requirement() {
    let home = tempfile::tempdir().expect("home");
    let mut command = Command::new(homeboy_bin());
    command
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
            "--verify",
            "cargo test -p homeboy-cli",
            "--prompt",
            "-",
            "--preview",
        ])
        .env("HOME", home.path())
        .env("XDG_CONFIG_HOME", home.path().join(".config"))
        .env("XDG_DATA_HOME", home.path().join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = command.spawn().expect("run Cook preview");
    child
        .stdin
        .take()
        .expect("preview stdin")
        .write_all(b"Fix the preview replay contract.\n")
        .expect("write preview prompt");
    let output = child.wait_with_output().expect("wait for Cook preview");

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let preview: Value = serde_json::from_slice(&output.stdout).expect("preview JSON");
    let replay = preview["data"]["replay_argv"]
        .as_array()
        .expect("preview replay argv");
    assert!(
        replay
            .windows(2)
            .any(|pair| pair[0] == "--prompt" && pair[1] == "-"),
        "stdin source remains explicit in the replay: {replay:?}"
    );
    assert!(
        preview["data"]["replay_requires"]
            .as_array()
            .expect("preview replay requirements")
            .iter()
            .any(|requirement| requirement
                == "replay requires the original non-empty prompt on stdin for `--prompt -`"),
        "stdin replay requirement is explicit: {preview}"
    );
}

#[test]
fn unscoped_preview_uses_the_primary_component_despite_missing_project_shadows() {
    for projects in [
        ["shadow-first", "shadow-second"],
        ["shadow-second", "shadow-first"],
    ] {
        let home = tempfile::tempdir().expect("home");
        let repository = tempfile::tempdir().expect("repository");
        let primary = repository.path().join("primary");
        initialize_git_repository(&primary);

        run_homeboy(
            home.path(),
            [
                "component",
                "create",
                "--local-path",
                primary.to_str().expect("primary path"),
            ],
        );
        run_homeboy(
            home.path(),
            [
                "component",
                "set",
                "primary",
                "--json",
                r#"{"aliases":["primary-alias"],"remote_url":"https://github.com/example/primary-repository.git"}"#,
            ],
        );
        for project in projects {
            let shadow = repository.path().join(project);
            std::fs::create_dir_all(&shadow).expect("create project shadow");
            std::fs::write(shadow.join("homeboy.json"), r#"{"id":"primary"}"#)
                .expect("write project shadow component");
            run_homeboy(home.path(), ["project", "create", project]);
            run_homeboy(
                home.path(),
                [
                    "project",
                    "components",
                    "set",
                    project,
                    "--json",
                    &format!(
                        r#"[{{"id":"primary","local_path":"{}"}}]"#,
                        shadow.display()
                    ),
                ],
            );
            std::fs::remove_file(shadow.join("homeboy.json"))
                .expect("remove project shadow component");
            std::fs::remove_dir(shadow).expect("remove project shadow");
        }

        for repo in ["primary", "primary-alias", "primary-repository"] {
            let output = Command::new(homeboy_bin())
                .args([
                    "agent-task",
                    "cook",
                    "--repo",
                    repo,
                    "--task-url",
                    "https://example.test/issues/14591",
                    "--head",
                    "fix/14591-primary-selection",
                    "--base",
                    "main",
                    "--backend",
                    "fixture",
                    "--prompt",
                    "Preserve the primary component selection.",
                    "--preview",
                    "--no-finalize",
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
                "repo {repo}; stdout: {}\nstderr: {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            let preview: Value = serde_json::from_slice(&output.stdout).expect("preview JSON");
            assert_eq!(
                preview["data"]["resolved"]["repository_identity"]["component_cwd"],
                "."
            );
            assert_eq!(
                preview["data"]["resolved"]["repository_identity"]["component_id"],
                "primary"
            );
        }
    }
}

fn run_homeboy<I, S>(home: &std::path::Path, args: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<std::ffi::OsStr>,
{
    let output = Command::new(homeboy_bin())
        .args(args)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("run Homeboy setup command");
    assert!(
        output.status.success(),
        "stdout: {}\nstderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn initialize_git_repository(path: &std::path::Path) {
    std::fs::create_dir_all(path).expect("create primary repository");
    for args in [
        vec!["init", "--quiet"],
        vec!["config", "user.email", "fixture@example.test"],
        vec!["config", "user.name", "Fixture"],
    ] {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .output()
            .expect("run git setup");
        assert!(output.status.success(), "git setup failed");
    }
    std::fs::write(path.join("README.md"), "fixture\n").expect("write fixture");
    let output = Command::new("git")
        .args(["add", "README.md"])
        .current_dir(path)
        .output()
        .expect("stage fixture");
    assert!(output.status.success(), "stage fixture failed");
    let output = Command::new("git")
        .args(["commit", "--quiet", "-m", "fixture"])
        .current_dir(path)
        .output()
        .expect("commit fixture");
    assert!(output.status.success(), "commit fixture failed");
    let output = Command::new("git")
        .args(["branch", "-M", "main"])
        .current_dir(path)
        .output()
        .expect("name fixture branch");
    assert!(output.status.success(), "name fixture branch failed");
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
