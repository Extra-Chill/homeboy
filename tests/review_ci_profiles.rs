#![cfg(unix)]

use homeboy_test_support::git;
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use std::process::{Command, Output};

#[test]
fn review_ci_runs_wordpress_jobs_in_the_selected_checkout() {
    let temp = tempfile::tempdir().unwrap();
    let home = temp.path().join("home");
    let checkout = temp.path().join("checkout with spaces");
    let extension = home.join(".config/homeboy/extensions/wordpress");
    fs::create_dir_all(&checkout).unwrap();
    fs::create_dir_all(&extension).unwrap();
    fs::write(checkout.join("homeboy.json"), r#"{"id":"fixture"}"#).unwrap();
    fs::write(checkout.join("fixture.php"), "<?php\n").unwrap();
    git::init(&checkout);
    git::commit_all(&checkout, "fixture");
    let wordpress: Value =
        serde_json::from_str(include_str!("../extensions/wordpress/wordpress.json")).unwrap();
    let mut manifest = json!({
        "name": "WordPress CI fixture",
        "version": "1.0.0",
        "ci": wordpress["ci"],
        "lint": { "extension_script": "lint.sh" },
        "test": { "extension_script": "test.sh" },
        "build": { "command_template": "bash {{script}}", "extension_script": "build.sh" }
    });
    manifest["ci"]["jobs"]["wp-codebox-phpunit"]["args"] = json!(["argument with spaces"]);
    fs::write(extension.join("wordpress.json"), manifest.to_string()).unwrap();
    write_script(
        &extension.join("lint.sh"),
        "set -e\ntest -f fixture.php\ntest \"$CI\" = 1\nprintf lint-ran >&2\n",
    );
    write_script(
        &extension.join("test.sh"),
        r#"set -e
 test -f fixture.php
 test "$CI" = 1
 test "$#" = 1
 test "$1" = 'argument with spaces'
 printf test-ran >&2
 printf '%s' '{"total":1,"passed":1,"failed":0,"results":[{"status":"passed","name":"Fixture::test_passes","file":"fixture.php"}]}' > "$HOMEBOY_TEST_RESULTS_FILE"
"#,
    );
    write_script(
        &extension.join("build.sh"),
        "set -e\ntest -f fixture.php\ntest \"$CI\" = 1\nprintf build-ran >&2\n",
    );

    for (selection, expected) in [
        (
            ["--job", "wp-codebox-build-smoke"],
            vec![("build", "local-partial")],
        ),
        (
            ["--profile", "pr"],
            vec![("lint", "local-equivalent"), ("test", "local-partial")],
        ),
    ] {
        let output = run_ci(&home, &checkout, &selection);
        let result: Value = serde_json::from_slice(&output.stdout)
            .unwrap_or_else(|error| panic!("CI did not emit a result: {error}; {output:?}"));
        assert!(output.status.success(), "{result:#}");
        let data = &result["data"];
        assert_eq!(data["success"], true, "{result:#}");
        let jobs = data["jobs"].as_array().unwrap();
        assert_eq!(jobs.len(), expected.len());
        for (job, (command, fidelity)) in jobs.iter().zip(expected) {
            assert_eq!(job["command"], command);
            assert_eq!(job["ci_context"]["fidelity"], fidelity);
            if selection[0] == "--profile" {
                assert_eq!(job["ci_context"]["profile"], selection[1]);
            }
            assert!(
                job.to_string().contains(&format!("{command}-ran")),
                "{job:#}"
            );
        }
    }

    let output = run_ci(&home, &checkout, &["--job", "wp-codebox-bench-offloaded"]);
    assert!(!output.status.success());
    let diagnostics = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        diagnostics.contains("wp-codebox-bench-offloaded"),
        "{diagnostics}"
    );
    assert!(diagnostics.contains("remote-only"), "{diagnostics}");
}

fn run_ci(home: &Path, checkout: &Path, selection: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_homeboy"))
        .args(["--placement", "local", "review", "ci", "run", "--path"])
        .arg(checkout)
        .args(["--extension", "wordpress"])
        .args(selection)
        .env("HOME", home)
        .env("XDG_CONFIG_HOME", home.join(".config"))
        .env("XDG_DATA_HOME", home.join(".local/share"))
        .env("HOMEBOY_NO_UPDATE_CHECK", "1")
        .output()
        .unwrap()
}

fn write_script(path: &Path, script: &str) {
    use std::os::unix::fs::PermissionsExt;
    fs::write(path, format!("#!/bin/sh\n{script}")).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}
