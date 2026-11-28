#![cfg(target_os = "linux")]

use std::fs;
use std::path::Path;
use std::process::Command;

use serde_json::{json, Value};

#[test]
fn extension_setup_timeout_survives_action_export() {
    if Command::new("jq").arg("--version").output().is_err() {
        eprintln!("Skipping Action integration test: jq is unavailable");
        return;
    }
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let action = Path::new(env!("CARGO_MANIFEST_DIR")).join("action");
    let bin = Path::new(env!("CARGO_BIN_EXE_homeboy"));
    let extension = root.join("fixture-extension");
    fs::create_dir(&extension).unwrap();
    fs::write(
        extension.join("fixture-extension.json"),
        json!({
            "name": "fixture-extension", "version": "1.0.0",
            "executable": { "runtime": {
                "setup_command": "sh ./block.sh --token 'inline fixture credential'",
                "setup_timeout_seconds": 1
            }}
        })
        .to_string(),
    )
    .unwrap();
    fs::write(
        extension.join("block.sh"),
        r#"#!/bin/sh
set -eu
jq -e '.status == "running"' "$HOMEBOY_EXTENSION_SETUP_RESULT_FILE" >/dev/null
printf 'stdout beginning\n%5000s\nstdout tail\n' ''
printf 'stderr beginning\n%5000s\nstderr tail %s\n' '' "$FIXTURE_API_TOKEN" >&2
trap '' TERM
sleep 30 &
printf '%s\n' "$!" > "$RUNNER_TEMP/child.pid"
wait
"#,
    )
    .unwrap();
    let github_env = root.join("github-env");
    let path = std::env::join_paths(std::iter::once(bin.parent().unwrap().to_path_buf()).chain(
        std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default()),
    ))
    .unwrap();
    let action_shell = |script: &str| {
        let mut cmd = Command::new("bash");
        cmd.arg(action.join("scripts").join(script))
            .current_dir(root)
            .env("PATH", &path)
            .env("HOME", root)
            .env("HOMEBOY_CONFIG_ROOT", root.join("config"))
            .env("HOMEBOY_DATA_DIR", root.join("data"))
            .env("HOMEBOY_DIFFERENTIAL_GATING", "false")
            .env("HOMEBOY_RUN_CANCELLED", "false")
            .env("HOMEBOY_OBSERVATION_WINDOW", "24h")
            .env("EXTENSION_REF", "")
            .env("COMPONENT_DIR", root)
            .env("RUNNER_TEMP", root)
            .env("GITHUB_ACTION_PATH", &action)
            .env("GITHUB_WORKSPACE", root)
            .env("GITHUB_ENV", &github_env)
            .env("HOMEBOY_ACTION_SETUP_TIMEOUT_SECONDS", "20")
            .env_remove("HOMEBOY_CI_RESULTS_DIR")
            .env_remove("HOMEBOY_EXTENSION_SETUP_TIMEOUT_SECONDS");
        cmd
    };
    let install = action_shell("core/run-setup-with-timeout.sh")
        .args(["install Homeboy extension", "--", "bash"])
        .arg(action.join("scripts/setup/install-extension.sh"))
        .env("EXTENSION_INPUT", "fixture-extension")
        .env("EXTENSION_SOURCE", &extension)
        .env("FIXTURE_API_TOKEN", "fixture-output-credential")
        .output()
        .unwrap();
    assert!(!install.status.success());
    let read_json =
        |path: &Path| serde_json::from_slice::<Value>(&fs::read(path).unwrap()).unwrap();
    let sidecar = read_json(&root.join("homeboy-ci-results/setup.json"));
    assert_eq!(sidecar["owner"], "fixture-extension", "{sidecar}");
    assert_eq!(sidecar["status"], "timeout");
    let setup = &sidecar["extension_setup"];
    assert_eq!(setup["invocation"]["phase"], "extension_runtime_setup");
    assert_eq!(setup["invocation"]["executable"], "sh");
    assert_eq!(setup["invocation"]["arguments"][0], "-c");
    assert!(setup["invocation"]["arguments"][1]
        .as_str()
        .unwrap()
        .contains("block.sh"));
    assert!(!setup.to_string().contains("inline fixture credential"));
    assert!(!setup.to_string().contains("fixture-output-credential"));
    assert_eq!(setup["invocation"]["budget_ms"], 1000);
    assert!(setup["elapsed_ms"].as_u64().unwrap() >= 1000);
    assert_eq!(setup["classification"], "transient_infrastructure_timeout");
    assert_eq!(setup["retryable"], true);
    assert_eq!(setup["termination"]["signals"]["term_delivered"], true);
    assert_eq!(setup["termination"]["signals"]["kill_delivered"], true);
    assert_eq!(setup["termination"]["no_child_left_running"], true);
    assert!(!root.join("config/extensions/fixture-extension").exists());
    let child = fs::read_to_string(root.join("child.pid"))
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    assert!(!homeboy_engine_primitives::command::process_is_running(
        child
    ));
    let replay = sidecar["replay_command"].as_str().unwrap();
    assert_eq!(
        replay,
        format!(
            "homeboy extension install --id fixture-extension -- {}",
            extension.display()
        )
    );
    for stream in ["stdout", "stderr"] {
        let tail = setup[format!("{stream}_tail")].as_str().unwrap();
        assert!(tail.contains(&format!("{stream} tail")));
        assert!(tail.len() <= 4096 && !tail.contains("beginning"));
    }

    let final_results = root.join("github-output");
    let select = action_shell("core/select-final-results.sh")
        .env(
            "HOMEBOY_SETUP_RESULT_FILE",
            root.join("homeboy-ci-results/setup.json"),
        )
        .env("COMMANDS", "review test")
        .env("FIRST_RESULTS", "")
        .env("GITHUB_OUTPUT", &final_results)
        .output()
        .unwrap();
    assert!(select.status.success());
    assert!(fs::read_to_string(final_results)
        .unwrap()
        .contains(r#""review test":"not_run""#));

    let export = action_shell("core/export-observations.sh")
        .output()
        .unwrap();
    assert!(
        export.status.success(),
        "{}",
        String::from_utf8_lossy(&export.stderr)
    );
    let bundle = root.join("homeboy-observations");
    assert!(
        read_json(&bundle.join("manifest.json"))["run_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    let runs = read_json(&bundle.join("runs.json"));
    let run = runs
        .as_array()
        .unwrap()
        .iter()
        .find(|run| run["id"] == setup["run_id"])
        .unwrap();
    assert_eq!(run["status"], "error");
    assert_eq!(run["metadata_json"]["setup"], *setup);
    let artifacts = read_json(&bundle.join("artifacts.json"));
    for stream in ["stdout", "stderr"] {
        let artifact = artifacts
            .as_array()
            .unwrap()
            .iter()
            .find(|artifact| artifact["id"] == setup["artifacts"][stream])
            .unwrap();
        let file = fs::read_dir(bundle.join("artifact-bytes"))
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .find(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(artifact["id"].as_str().unwrap())
            })
            .unwrap();
        let log = fs::read_to_string(file).unwrap();
        assert!(log.contains(&format!("{stream} beginning")));
        assert!(log.contains(&format!("{stream} tail")));
        assert!(!log.contains("fixture-output-credential"));
    }
}
