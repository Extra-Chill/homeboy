use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Command;

use crate::commands::test::{run as run_test, TestArgs};
use crate::commands::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
};
use crate::commands::GlobalArgs;
use homeboy_core::component::{Component, ComponentScriptsConfig, ScopedExtensionConfig};
use homeboy_core::engine::run_dir::RunDir;
use homeboy_core::test_support::with_isolated_home;
use homeboy_extension::component_script::{
    run_component_scripts, run_component_scripts_with_env, run_component_scripts_with_run_dir,
    source_path,
};
use homeboy_extension::ExtensionCapability;

fn component_script_args(root: &Path) -> PositionalComponentArgs {
    PositionalComponentArgs {
        component: Some("fixture".to_string()),
        path: Some(root.to_string_lossy().to_string()),
    }
}

fn test_command_args(root: &Path) -> TestArgs {
    TestArgs {
        comp: component_script_args(root),
        extension_override: ExtensionOverrideArgs::default(),
        skip_lint: false,
        coverage: false,
        coverage_min: None,
        baseline_args: BaselineArgs::default(),
        analyze: false,
        drift: false,
        write: false,
        since: "HEAD~10".to_string(),
        changed_since: None,
        precomputed_changed_files: None,
        lab_changed_files_json: None,
        ci_job: None,
        setting_args: SettingArgs::default(),
        args: Vec::new(),
        json_summary: false,
        restore_checkout: false,
    }
}

fn write_component_script(root: &Path, name: &str, body: &str) {
    let script_dir = root.join("scripts");
    fs::create_dir_all(&script_dir).expect("script dir should be created");
    fs::write(script_dir.join(name), body).expect("script should be written");
}

fn init_git_repo(root: &Path) {
    for args in [
        vec!["init", "-q"],
        vec!["config", "user.email", "tests@example.com"],
        vec!["config", "user.name", "Tests"],
        vec!["add", "."],
        vec!["commit", "-q", "-m", "fixture"],
    ] {
        let output = Command::new("git")
            .args(&args)
            .current_dir(root)
            .output()
            .expect("git fixture command should start");
        assert!(
            output.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

fn script_component(root: &Path, command: &str) -> Component {
    let mut component = Component::new(
        "fixture".to_string(),
        root.to_string_lossy().to_string(),
        String::new(),
        None,
    );
    component.scripts = Some(ComponentScriptsConfig {
        test: vec![command.to_string()],
        ..Default::default()
    });
    component
}

#[test]
fn test_run_component_scripts() {
    let dir = tempfile::tempdir().expect("temp dir");
    let component = script_component(dir.path(), "printf ok > marker");

    let output = run_component_scripts(&component, ExtensionCapability::Test, dir.path(), false)
        .expect("component script should run");

    assert!(output.success);
    assert_eq!(output.exit_code, 0);
    assert_eq!(fs::read_to_string(dir.path().join("marker")).unwrap(), "ok");
}

#[test]
fn test_run_component_scripts_with_env() {
    let dir = tempfile::tempdir().expect("temp dir");
    let component = script_component(dir.path(), "printf \"$EXTRA_VALUE\" > marker");

    let output = run_component_scripts_with_env(
        &component,
        ExtensionCapability::Test,
        dir.path(),
        false,
        &[("EXTRA_VALUE".to_string(), "ok".to_string())],
        &[],
    )
    .expect("component script should run with env");

    assert!(output.success);
    assert_eq!(fs::read_to_string(dir.path().join("marker")).unwrap(), "ok");
}

#[test]
fn component_config_env_is_available_to_component_scripts_and_extra_env_wins() {
    let dir = tempfile::tempdir().expect("temp dir");
    let mut component = script_component(
        dir.path(),
        "printf '%s:%s' \"$SHARED_CACHE_DIR\" \"$OVERRIDE_ME\" > marker",
    );
    component.env.insert(
        "SHARED_CACHE_DIR".to_string(),
        "/tmp/homeboy-shared-cache".to_string(),
    );
    component
        .env
        .insert("OVERRIDE_ME".to_string(), "configured".to_string());

    let output = run_component_scripts_with_env(
        &component,
        ExtensionCapability::Test,
        dir.path(),
        false,
        &[("OVERRIDE_ME".to_string(), "runtime".to_string())],
        &[],
    )
    .expect("component script should run with configured env");

    assert!(output.success);
    assert_eq!(
        fs::read_to_string(dir.path().join("marker")).unwrap(),
        "/tmp/homeboy-shared-cache:runtime"
    );
}

#[test]
fn component_config_env_is_visible_to_env_providers() {
    with_isolated_home(|_| {
        let dir = tempfile::tempdir().expect("temp dir");
        let extension_dir = homeboy_core::paths::extensions()
            .expect("extensions path")
            .join("fixture-provider-env");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("fixture-provider-env.json"),
            r#"{
                "name": "Fixture Provider Env",
                "version": "1.0.0",
                "env_provider": { "script": "env.sh" }
            }"#,
        )
        .expect("extension manifest");
        fs::write(
            extension_dir.join("env.sh"),
            "#!/bin/sh\nprintf '{\"PROVIDER_SAW\":\"%s\"}' \"$SHARED_CACHE_DIR\"\n",
        )
        .expect("extension env script");
        let mut permissions = fs::metadata(extension_dir.join("env.sh"))
            .expect("extension env metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(extension_dir.join("env.sh"), permissions)
            .expect("extension env executable");

        let mut component = script_component(dir.path(), "printf '%s' \"$PROVIDER_SAW\" > marker");
        component.env.insert(
            "SHARED_CACHE_DIR".to_string(),
            "/tmp/homeboy-provider-cache".to_string(),
        );
        component.extensions = Some(HashMap::from([(
            "fixture-provider-env".to_string(),
            ScopedExtensionConfig::default(),
        )]));

        let output = run_component_scripts_with_env(
            &component,
            ExtensionCapability::Test,
            dir.path(),
            false,
            &[],
            &[],
        )
        .expect("component script should run");

        assert!(output.success);
        assert_eq!(
            fs::read_to_string(dir.path().join("marker")).unwrap(),
            "/tmp/homeboy-provider-cache"
        );
    });
}

#[test]
fn component_scripts_include_linked_extension_env_provider_output() {
    with_isolated_home(|_| {
        let dir = tempfile::tempdir().expect("temp dir");
        let extension_dir = homeboy_core::paths::extensions()
            .expect("extensions path")
            .join("fixture-env");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("fixture-env.json"),
            r#"{
                "name": "Fixture Env",
                "version": "1.0.0",
                "env_provider": { "script": "env.sh" }
            }"#,
        )
        .expect("extension manifest");
        fs::write(
            extension_dir.join("env.sh"),
            "#!/bin/sh\nprintf '{\"FIXTURE_ENV\":\"%s\"}' \"$EXTRA_VALUE\"\n",
        )
        .expect("extension env script");
        let mut permissions = fs::metadata(extension_dir.join("env.sh"))
            .expect("extension env metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(extension_dir.join("env.sh"), permissions)
            .expect("extension env executable");

        let mut component = script_component(dir.path(), "printf '%s' \"$FIXTURE_ENV\" > marker");
        component.extensions = Some(HashMap::from([(
            "fixture-env".to_string(),
            ScopedExtensionConfig::default(),
        )]));

        let output = run_component_scripts_with_env(
            &component,
            ExtensionCapability::Test,
            dir.path(),
            false,
            &[("EXTRA_VALUE".to_string(), "provided".to_string())],
            &[],
        )
        .expect("component script should run");

        assert!(output.success);
        assert_eq!(
            fs::read_to_string(dir.path().join("marker")).unwrap(),
            "provided"
        );
    });
}

#[test]
fn test_run_component_scripts_with_run_dir() {
    with_isolated_home(|_| {
        let dir = tempfile::tempdir().expect("temp dir");
        let run_dir = RunDir::create().expect("run dir");
        let component = script_component(
            dir.path(),
            "test -n \"$HOMEBOY_RUN_DIR\" && printf ok > marker",
        );

        let output = run_component_scripts_with_run_dir(
            &component,
            ExtensionCapability::Test,
            dir.path(),
            &run_dir,
            false,
            &[],
            &[],
        )
        .expect("component script should run with run dir");

        assert!(output.success);
        assert_eq!(fs::read_to_string(dir.path().join("marker")).unwrap(), "ok");
        run_dir.cleanup();
    });
}

#[test]
fn test_source_path() {
    let component = Component::new(
        "fixture".to_string(),
        "/component/path".to_string(),
        String::new(),
        None,
    );

    assert_eq!(source_path(&component, None), Path::new("/component/path"));
    assert_eq!(
        source_path(&component, Some("/override")),
        Path::new("/override")
    );
}

#[test]
fn command_dispatch_runs_component_script_before_extension_resolution() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(
        dir.path().join("homeboy.json"),
        r#"{
  "id": "fixture",
  "scripts": { "test": ["sh scripts/test.sh"] }
}"#,
    )
    .expect("homeboy.json should be written");
    write_component_script(
        dir.path(),
        "test.sh",
        "printf 'component script ran\n' > component-script-marker\n",
    );

    let (output, exit_code) =
        run_test(test_command_args(dir.path()), &GlobalArgs {}).expect("test script should run");

    assert_eq!(exit_code, 0);
    assert!(output.passed);
    assert!(dir.path().join("component-script-marker").exists());
}

#[test]
fn command_dispatch_falls_back_to_extension_when_component_script_is_absent() {
    with_isolated_home(|home| {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("homeboy.json"),
            r#"{
  "id": "fixture",
  "extensions": { "fixture-extension": {} }
}"#,
        )
        .expect("homeboy.json should be written");

        let extension_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join("fixture-extension");
        fs::create_dir_all(&extension_dir).expect("extension dir should be created");
        fs::write(
            extension_dir.join("fixture-extension.json"),
            r#"{
  "name": "Fixture extension",
  "version": "1.0.0",
  "test": { "extension_script": "test.sh" }
}"#,
        )
        .expect("extension manifest should be written");
        let extension_script = extension_dir.join("test.sh");
        fs::write(
            &extension_script,
            "#!/bin/sh\nprintf 'extension ran\n' > \"$HOMEBOY_COMPONENT_PATH/extension-marker\"\nprintf 'Tests: 1\\nFailures: 0\\n'\n",
        )
        .expect("extension script should be written");
        let mut perms = fs::metadata(&extension_script)
            .expect("extension script metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&extension_script, perms)
            .expect("extension script should be executable");

        let (output, exit_code) = run_test(test_command_args(dir.path()), &GlobalArgs {})
            .expect("extension test should run");

        assert_eq!(exit_code, 0);
        assert!(output.passed);
        assert!(dir.path().join("extension-marker").exists());
    });
}

#[test]
fn changed_wordpress_php_smoke_test_executes_with_generic_result_adapter() {
    with_isolated_home(|home| {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("homeboy.json"),
            r#"{
  "id": "fixture",
  "extensions": { "wordpress": {} }
}"#,
        )
        .expect("homeboy.json should be written");

        let extension_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join("wordpress");
        fs::create_dir_all(&extension_dir).expect("extension dir should be created");
        fs::write(
            extension_dir.join("wordpress.json"),
            r#"{
  "name": "WordPress fixture",
  "version": "1.0.0",
  "test": {
    "extension_script": "test.sh",
    "drift": {
      "source_dirs": ["src"],
      "test_dirs": ["tests"],
      "file_extensions": ["php"],
      "inline_tests": false
    }
  }
}"#,
        )
        .expect("extension manifest should be written");
        let extension_script = extension_dir.join("test.sh");
        fs::write(
            &extension_script,
            "#!/bin/sh\nprintf '%s' \"$HOMEBOY_CHANGED_TEST_FILES\" | grep -q 'tests/patterns/patterns-ability-smoke.php'\nprintf 'wordpress smoke passed=1 failed=0\\n'\n",
        )
        .expect("extension script should be written");
        let mut perms = fs::metadata(&extension_script)
            .expect("extension script metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&extension_script, perms)
            .expect("extension script should be executable");
        init_git_repo(dir.path());

        let mut args = test_command_args(dir.path());
        args.changed_since = Some("HEAD".to_string());
        args.precomputed_changed_files = Some(vec![
            "src/patterns.php".to_string(),
            "tests/patterns/patterns-ability-smoke.php".to_string(),
        ]);
        let (output, exit_code) =
            run_test(args, &GlobalArgs {}).expect("extension test should run");

        assert_eq!(exit_code, 0);
        assert!(output.passed);
        assert_eq!(output.status, "passed");
        assert_eq!(output.test_counts.expect("test counts").passed, 1);
        assert_eq!(
            output.test_scope.expect("changed scope").selected_files,
            vec!["tests/patterns/patterns-ability-smoke.php"]
        );
    });
}

#[test]
fn successful_selected_test_without_result_evidence_fails_closed() {
    with_isolated_home(|home| {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("homeboy.json"),
            r#"{
  "id": "fixture",
  "extensions": { "generic": {} }
}"#,
        )
        .expect("homeboy.json should be written");

        let extension_dir = home
            .path()
            .join(".config")
            .join("homeboy")
            .join("extensions")
            .join("generic");
        fs::create_dir_all(&extension_dir).expect("extension dir should be created");
        fs::write(
            extension_dir.join("generic.json"),
            r#"{
  "name": "Generic fixture",
  "version": "1.0.0",
  "test": { "extension_script": "test.sh" }
}"#,
        )
        .expect("extension manifest should be written");
        let extension_script = extension_dir.join("test.sh");
        fs::write(
            &extension_script,
            "#!/bin/sh\nprintf 'runner completed without a result adapter\\n'\n",
        )
        .expect("extension script should be written");
        let mut perms = fs::metadata(&extension_script)
            .expect("extension script metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&extension_script, perms)
            .expect("extension script should be executable");

        let mut args = test_command_args(dir.path());
        args.changed_since = Some("origin/main".to_string());
        args.precomputed_changed_files = Some(vec!["tests/generic-smoke.php".to_string()]);
        let (output, exit_code) =
            run_test(args, &GlobalArgs {}).expect("extension test should run");

        assert_eq!(exit_code, 1);
        assert!(!output.passed);
        assert_eq!(output.status, "failed");
        assert!(output.test_counts.is_none());
        assert!(output
            .hints
            .expect("execution evidence hint")
            .iter()
            .any(|hint| hint.contains("verifiable test results")));
    });
}

#[test]
fn selected_filter_that_skips_every_test_fails_closed() {
    with_isolated_home(|_home| {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("homeboy.json"),
            r#"{"id":"fixture","extensions":{"generic":{}}}"#,
        )
        .expect("homeboy.json");
        let extension_dir = homeboy_core::paths::extensions()
            .expect("extensions path")
            .join("generic");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(
            extension_dir.join("generic.json"),
            r#"{"name":"Generic","version":"1.0.0","test":{"extension_script":"test.sh"}}"#,
        )
        .expect("extension manifest");
        let script = extension_dir.join("test.sh");
        fs::write(&script, "#!/bin/sh\nprintf 'filter selected no runnable tests passed=0 failed=0 skipped=1\\n'\n")
            .expect("extension script");
        let mut permissions = fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("script executable");

        let (output, exit_code) =
            run_test(test_command_args(dir.path()), &GlobalArgs {}).expect("test run");
        assert_eq!(exit_code, 1);
        assert_eq!(output.test_counts.expect("counts").skipped, 1);
        assert_eq!(output.status, "failed");
    });
}

#[test]
fn extension_policy_with_evidence_allows_a_neutral_no_test_scope() {
    with_isolated_home(|_home| {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("homeboy.json"),
            r#"{"id":"fixture","extensions":{"docs":{}}}"#,
        )
        .expect("homeboy.json");
        let extension_dir = homeboy_core::paths::extensions()
            .expect("extensions path")
            .join("docs");
        fs::create_dir_all(&extension_dir).expect("extension dir");
        fs::write(extension_dir.join("docs.json"), r#"{"name":"Docs","version":"1.0.0","test":{"extension_script":"test.sh","no_tests_applicable":{"evidence_markers":["NO TESTS APPLICABLE","scope=docs-only"]}}}"#)
            .expect("extension manifest");
        let script = extension_dir.join("test.sh");
        fs::write(
            &script,
            "#!/bin/sh\nprintf 'NO TESTS APPLICABLE\\nscope=docs-only\\n'\n",
        )
        .expect("extension script");
        let mut permissions = fs::metadata(&script)
            .expect("script metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&script, permissions).expect("script executable");

        let (output, exit_code) =
            run_test(test_command_args(dir.path()), &GlobalArgs {}).expect("test run");
        assert_eq!(exit_code, 0);
        assert!(output.passed);
        assert_eq!(output.status, "skipped");
        assert_eq!(
            output.phase.expect("phase").status,
            homeboy_extension::PhaseStatus::Skipped
        );
    });
}
