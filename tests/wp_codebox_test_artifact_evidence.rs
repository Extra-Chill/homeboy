use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

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
