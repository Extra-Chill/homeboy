use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

#[test]
fn review_test_output_materializes_runtime_evidence_from_normal_action_run() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    setup_runtime_evidence_fixture(&home, &project);
    let result_path = temp.path().join("review-test.json");

    let test = run_runtime_evidence_test(&home, &project, &result_path, None);

    assert_eq!(
        test.status.code(),
        Some(1),
        "test stderr: {}",
        stderr(&test)
    );
    assert!(result_path.is_file(), "primary command artifact");
    let inventory: Value = serde_json::from_slice(
        &fs::read(temp.path().join("review-test.test-inventory.json")).expect("inventory sidecar"),
    )
    .expect("inventory JSON");
    let outcomes: Value = serde_json::from_slice(
        &fs::read(temp.path().join("review-test.test-outcomes.json")).expect("outcomes sidecar"),
    )
    .expect("outcomes JSON");

    assert_eq!(inventory["command"], "review test");
    assert_eq!(inventory["runner"], "python");
    assert_eq!(
        inventory["tests"],
        serde_json::json!([
            {"id": "python:tests/runtime_evidence.py::test_fails"},
            {"id": "python:tests/runtime_evidence.py::test_passes"}
        ])
    );
    assert_eq!(
        outcomes["failed_test_ids"],
        serde_json::json!(["python:tests/runtime_evidence.py::test_fails"])
    );
    for field in [
        "command",
        "runner",
        "runner_fingerprint",
        "workspace_fingerprint",
        "execution_fingerprint",
        "inventory_fingerprint",
    ] {
        assert_eq!(outcomes[field], inventory[field], "shared {field}");
    }
    assert!(project.join("inventory-producer-ran").is_file());
    assert!(project.join("test-runner-ran").is_file());
}

#[test]
fn review_test_runtime_evidence_rejects_missing_malformed_and_countless_red() {
    for case in ["missing", "malformed", "countless"] {
        let temp = tempfile::tempdir().expect("temp dir");
        let home = temp.path().join("home");
        let project = temp.path().join("project");
        setup_runtime_evidence_fixture(&home, &project);
        fs::write(project.join("runtime-evidence-case"), case).expect("case selector");
        let result_path = temp.path().join(format!("{case}.json"));

        let test = run_runtime_evidence_test(&home, &project, &result_path, None);

        assert_eq!(test.status.code(), Some(1), "{case}: {}", stderr(&test));
        let inventory: Value = serde_json::from_slice(
            &fs::read(temp.path().join(format!("{case}.test-inventory.json"))).unwrap(),
        )
        .unwrap();
        let outcomes: Value = serde_json::from_slice(
            &fs::read(temp.path().join(format!("{case}.test-outcomes.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(inventory["invalid_evidence"]["type"], "invalid_evidence");
        assert_eq!(outcomes["invalid_evidence"], inventory["invalid_evidence"]);
    }
}

#[test]
fn review_test_runtime_evidence_accepts_live_provenance_shard_fixture() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    setup_runtime_evidence_fixture(&home, &project);
    let manifest = temp.path().join("shard.json");
    let runner_version = Command::new("python3")
        .arg("--version")
        .output()
        .expect("python version");
    let runner_version = String::from_utf8(runner_version.stdout)
        .expect("version UTF-8")
        .trim()
        .to_string();
    let runner_fingerprint = homeboy_engine_primitives::content_hash::sha256_hex(
        format!("python\0{runner_version}").as_bytes(),
    );
    let workspace_fingerprint = runtime_fixture_workspace_fingerprint(&project);
    fs::write(
        &manifest,
        serde_json::to_vec(&serde_json::json!({
            "schema": "homeboy/test-shard-manifest/v1",
            "id": "python-shard-1",
            "runner": "python",
            "runner_fingerprint": runner_fingerprint,
            "workspace_fingerprint": workspace_fingerprint,
            "inventory_fingerprint": "c".repeat(64),
            "tests": [
                "python:tests/runtime_evidence.py::test_passes",
                "python:tests/runtime_evidence.py::test_fails"
            ]
        }))
        .unwrap(),
    )
    .expect("shard manifest");
    let result_path = temp.path().join("shard-result.json");

    let test = run_runtime_evidence_test(&home, &project, &result_path, Some(&manifest));

    assert_eq!(test.status.code(), Some(1), "{}", stderr(&test));
    let outcomes: Value = serde_json::from_slice(
        &fs::read(temp.path().join("shard-result.test-outcomes.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        outcomes["failed_test_ids"],
        serde_json::json!(["python:tests/runtime_evidence.py::test_fails"])
    );
}

#[test]
fn failing_provider_test_artifacts_remain_retrievable_after_scratch_cleanup() {
    let temp = tempfile::tempdir().expect("temp dir");
    let home = temp.path().join("home");
    let project = temp.path().join("project");
    let extension = home.join(".config/homeboy/extensions/wp-codebox-fixture");
    fs::create_dir_all(&project).expect("project dir");
    fs::create_dir_all(&extension).expect("extension dir");
    fs::write(
        project.join("homeboy.json"),
        r#"{"id":"wp-codebox-fixture"}"#,
    )
    .expect("project manifest");
    fs::write(
        extension.join("wp-codebox-fixture.json"),
        r#"{
  "name": "WP Codebox artifact fixture",
  "version": "1.0.0",
  "test": { "extension_script": "test.sh" }
}"#,
    )
    .expect("extension manifest");
    write_executable(
        &extension.join("test.sh"),
        r##"#!/bin/sh
mkdir -p "$HOMEBOY_RUN_DIR/files"
printf '%s' '{"total":3,"passed":2,"failed":1,"results":[{"status":"failed","name":"WP_Codebox_Test::test_database_connection","message":"Expected wpdb connection to succeed.","file":"tests/WP_Codebox_Test.php","line":73}]}' > "$HOMEBOY_RUN_DIR/files/test-results.json"
printf '%s\n' 'PHPUnit 10.0' '' 'There was 1 failure:' '' '1) WP_Codebox_Test::test_database_connection' 'Expected wpdb connection to succeed.' '' 'tests/WP_Codebox_Test.php:73' '' 'FAILURES!' 'Tests: 3, Assertions: 4, Failures: 1' > "$HOMEBOY_RUN_DIR/files/phpunit-output.log"
printf '%s\n' 'artifact://files/test-results.json' 'artifact://files/phpunit-output.log'
exit 1
"##,
    );

    let test = run_homeboy(
        &home,
        [
            "--placement",
            "local",
            "review",
            "test",
            "--path",
            project.to_str().expect("project path"),
            "--extension",
            "wp-codebox-fixture",
            "--skip-lint",
            "--json-summary",
        ],
    );
    assert_eq!(
        test.status.code(),
        Some(1),
        "test stderr: {}",
        stderr(&test)
    );
    let test_json = json_output(&test, "test");
    let payload = &test_json["data"];
    assert_eq!(payload["status"], "failed");
    assert_eq!(payload["test_counts"]["total"], 3);
    assert_eq!(payload["test_counts"]["passed"], 2);
    assert_eq!(payload["test_counts"]["failed"], 1);
    assert_eq!(
        payload["summary"]["failures"][0]["test_name"],
        "WP_Codebox_Test::test_database_connection"
    );
    assert_eq!(
        payload["summary"]["failures"][0]["message"],
        "Expected wpdb connection to succeed."
    );
    let run_id = test_json["run"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("test run id missing from {test_json}"));
    assert!(
        !test_json.to_string().contains("artifact://"),
        "serialized test output must expose durable evidence references only: {test_json}"
    );

    let listed = run_homeboy(&home, ["runs", "artifacts", run_id]);
    assert!(
        listed.status.success(),
        "artifacts stderr: {}",
        stderr(&listed)
    );
    let listed_json = json_output(&listed, "runs artifacts");
    let artifacts = listed_json["data"]["payload"]["artifacts"]
        .as_array()
        .expect("artifact rows");
    let results = artifact_by_locator(artifacts, "artifact://files/test-results.json");
    let phpunit = artifact_by_locator(artifacts, "artifact://files/phpunit-output.log");
    assert_eq!(results["type"], "file");
    assert_eq!(phpunit["type"], "file");
    let phase_artifacts = payload["extension_phase_timings"][0]["artifacts"]
        .as_array()
        .expect("serialized phase artifacts");
    assert_eq!(phase_artifacts.len(), 2);
    assert!(phase_artifacts.iter().all(|artifact| {
        artifact["artifact_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty())
            && artifact["ref"]
                .as_str()
                .is_some_and(|reference| reference.starts_with("homeboy://run/"))
            && artifact.get("source_locator").is_none()
    }));

    let results_output = temp.path().join("retrieved-test-results.json");
    let fetched_results = artifact_get(&home, run_id, results, &results_output);
    assert_eq!(
        fs::read(&results_output).expect("retrieved results bytes"),
        br#"{"total":3,"passed":2,"failed":1,"results":[{"status":"failed","name":"WP_Codebox_Test::test_database_connection","message":"Expected wpdb connection to succeed.","file":"tests/WP_Codebox_Test.php","line":73}]}"#
    );
    assert_eq!(
        fetched_results["data"]["payload"]["size_bytes"],
        fs::read(&results_output)
            .expect("retrieved results byte count")
            .len()
    );

    let phpunit_output = temp.path().join("retrieved-phpunit-output.log");
    let fetched_phpunit = artifact_get(&home, run_id, phpunit, &phpunit_output);
    let phpunit_bytes = fs::read_to_string(&phpunit_output).expect("retrieved phpunit bytes");
    assert!(phpunit_bytes.contains("WP_Codebox_Test::test_database_connection"));
    assert!(phpunit_bytes.contains("Tests: 3, Assertions: 4, Failures: 1"));
    assert_eq!(
        fetched_phpunit["data"]["payload"]["size_bytes"],
        phpunit_bytes.len()
    );
}

fn setup_runtime_evidence_fixture(home: &Path, project: &Path) {
    let extension = home.join(".config/homeboy/extensions/runtime-evidence-fixture");
    fs::create_dir_all(project.join("tests")).expect("project dir");
    fs::create_dir_all(&extension).expect("extension dir");
    fs::write(
        project.join("homeboy.json"),
        r#"{"id":"runtime-evidence-fixture"}"#,
    )
    .expect("project manifest");
    fs::write(project.join("fixture.root"), "root\n").expect("root marker");
    fs::write(project.join("tests/runtime_evidence.fixture"), "suite\n").expect("suite fixture");
    fs::write(
        extension.join("runtime-evidence-fixture.json"),
        r#"{
  "name": "Runtime evidence fixture",
  "version": "1.0.0",
  "test": {
    "extension_script": "test.py",
    "inventory": {
      "root_markers": ["fixture.root"],
      "fingerprint_names": ["fixture.root"],
      "fingerprint_extensions": ["fixture"],
      "runners": [{"id":"python","version_command":["python3","--version"]}]
    }
  }
}"#,
    )
    .expect("extension manifest");
    write_executable(
        &extension.join("test.py"),
        r##"#!/usr/bin/env python3
import hashlib
import json
import os
import subprocess
from pathlib import Path

root = Path(os.environ["HOMEBOY_COMPONENT_PATH"]).resolve()
ids = [
    "python:tests/runtime_evidence.py::test_passes",
    "python:tests/runtime_evidence.py::test_fails",
]
if os.environ.get("HOMEBOY_TEST_INVENTORY_ONLY") == "1":
    files = sorted(path for path in root.rglob("*") if path.is_file() and (path.name == "fixture.root" or path.suffix == ".fixture"))
    workspace = hashlib.sha256("".join(f"{path.relative_to(root)}\0{path.read_text()}\0" for path in files).encode()).hexdigest()
    version = subprocess.check_output(["python3", "--version"], cwd=root, text=True).strip()
    runner = hashlib.sha256(f"python\0{version}".encode()).hexdigest()
    tests = [{
        "id": test_id,
        "package": "runtime-evidence-fixture",
        "target": "tests/runtime_evidence.py",
        "target_kind": "test",
        "name": test_id.rsplit("::", 1)[1],
        "expected_outcome": "executed",
    } for test_id in ids]
    inventory = {
        "schema": "homeboy/test-inventory/v1",
        "runner": "python",
        "runner_fingerprint": runner,
        "workspace_fingerprint": workspace,
        "tests": tests,
    }
    inventory["inventory_fingerprint"] = hashlib.sha256(json.dumps(inventory, sort_keys=True, separators=(",", ":")).encode()).hexdigest()
    Path(os.environ["HOMEBOY_TEST_INVENTORY_FILE"]).write_text(json.dumps(inventory))
    (root / "inventory-producer-ran").touch()
    raise SystemExit(0)

(root / "test-runner-ran").touch()
case_path = root / "runtime-evidence-case"
case = case_path.read_text().strip() if case_path.exists() else "complete"
if case != "countless":
    Path(os.environ["HOMEBOY_TEST_RESULTS_FILE"]).write_text(json.dumps({"total": 2, "passed": 1, "failed": 1}))
if case == "malformed":
    Path(os.environ["HOMEBOY_TEST_FAILURES_FILE"]).write_text("{")
elif case != "missing":
    Path(os.environ["HOMEBOY_TEST_FAILURES_FILE"]).write_text(json.dumps([{"test_name": ids[1], "message": "failed"}]))
raise SystemExit(1)
"##,
    );
}

fn run_runtime_evidence_test(
    home: &Path,
    project: &Path,
    result_path: &Path,
    shard_manifest: Option<&Path>,
) -> Output {
    let mut command = Command::new(homeboy_bin());
    command
        .args([
            "--output",
            result_path.to_str().expect("result path"),
            "--placement",
            "local",
            "review",
            "test",
            "--path",
            project.to_str().expect("project path"),
            "--extension",
            "runtime-evidence-fixture",
            "--skip-lint",
            "--json-summary",
        ])
        .env("HOME", home)
        .env_remove("XDG_DATA_HOME")
        .env_remove("HOMEBOY_TEST_INVENTORY_FILE")
        .env_remove("HOMEBOY_TEST_SHARD_MANIFEST")
        .env("HOMEBOY_NO_UPDATE_CHECK", "1");
    if let Some(manifest) = shard_manifest {
        command.env("HOMEBOY_TEST_SHARD_MANIFEST", manifest);
    }
    command.output().expect("homeboy review test")
}

fn runtime_fixture_workspace_fingerprint(project: &Path) -> String {
    let mut files = vec![
        PathBuf::from("fixture.root"),
        PathBuf::from("tests/runtime_evidence.fixture"),
    ];
    files.sort();
    let mut content = String::new();
    for relative in files {
        content.push_str(relative.to_str().unwrap());
        content.push('\0');
        content.push_str(&fs::read_to_string(project.join(&relative)).unwrap());
        content.push('\0');
    }
    homeboy_engine_primitives::content_hash::sha256_hex(content.as_bytes())
}

fn artifact_get(home: &Path, run_id: &str, artifact: &Value, output: &Path) -> Value {
    let artifact_id = artifact["id"].as_str().expect("artifact id");
    let fetched = run_homeboy(
        home,
        [
            "runs",
            "artifact",
            "get",
            run_id,
            artifact_id,
            "--output",
            output.to_str().expect("output path"),
        ],
    );
    assert!(
        fetched.status.success(),
        "artifact get stderr: {}",
        stderr(&fetched)
    );
    json_output(&fetched, "runs artifact get")
}

fn artifact_by_locator<'a>(artifacts: &'a [Value], locator: &str) -> &'a Value {
    artifacts
        .iter()
        .find(|artifact| artifact["metadata_json"]["locator"] == locator)
        .unwrap_or_else(|| panic!("missing indexed artifact {locator}: {artifacts:?}"))
}

fn run_homeboy<const N: usize>(home: &Path, args: [&str; N]) -> Output {
    Command::new(homeboy_bin())
        .args(args)
        .env("HOME", home)
        .env_remove("XDG_DATA_HOME")
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .expect("homeboy command")
}

fn write_executable(path: &Path, body: &str) {
    fs::write(path, body).expect("extension script");
    let mut permissions = fs::metadata(path).expect("script metadata").permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions).expect("script executable");
}

fn json_output(output: &Output, command: &str) -> Value {
    serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "{command} JSON: {error}; stdout: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    })
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("CARGO_BIN_EXE_homeboy"))
}
