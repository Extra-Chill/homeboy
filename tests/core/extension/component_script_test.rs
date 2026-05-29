use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::commands::test::{run as run_test, TestArgs};
use crate::commands::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
};
use crate::commands::GlobalArgs;
use crate::core::cargo_target::CARGO_TARGET_DIR_ENV;
use crate::core::component::{Component, ComponentScriptsConfig};
use crate::core::engine::run_dir::RunDir;
use crate::core::extension::component_script::{
    run_component_scripts, run_component_scripts_with_env, run_component_scripts_with_run_dir,
    source_path,
};
use crate::core::extension::ExtensionCapability;
use crate::test_support::with_isolated_home;

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
        ci_job: None,
        setting_args: SettingArgs::default(),
        args: Vec::new(),
        json_summary: false,
    }
}

fn test_command_args_with_setting(root: &Path, key: &str, value: &str) -> TestArgs {
    let mut args = test_command_args(root);
    args.setting_args.setting = vec![(key.to_string(), value.to_string())];
    args
}

fn write_component_script(root: &Path, name: &str, body: &str) {
    let script_dir = root.join("scripts");
    fs::create_dir_all(&script_dir).expect("script dir should be created");
    fs::write(script_dir.join(name), body).expect("script should be written");
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

fn rust_script_component(root: &Path, command: &str) -> Component {
    let mut component = script_component(root, command);
    component.remote_url = Some("https://github.com/Extra-Chill/homeboy.git".to_string());
    component
}

fn without_process_cargo_target_dir<R>(body: impl FnOnce() -> R) -> R {
    let prior = std::env::var(CARGO_TARGET_DIR_ENV).ok();
    std::env::remove_var(CARGO_TARGET_DIR_ENV);
    let result = body();
    match prior {
        Some(value) => std::env::set_var(CARGO_TARGET_DIR_ENV, value),
        None => std::env::remove_var(CARGO_TARGET_DIR_ENV),
    }
    result
}

fn with_process_cargo_target_dir<R>(value: &Path, body: impl FnOnce() -> R) -> R {
    let prior = std::env::var(CARGO_TARGET_DIR_ENV).ok();
    std::env::set_var(CARGO_TARGET_DIR_ENV, value);
    let result = body();
    match prior {
        Some(value) => std::env::set_var(CARGO_TARGET_DIR_ENV, value),
        None => std::env::remove_var(CARGO_TARGET_DIR_ENV),
    }
    result
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
fn rust_component_scripts_default_to_shared_cargo_target_dir() {
    with_isolated_home(|home| {
        without_process_cargo_target_dir(|| {
            let primary = tempfile::tempdir().expect("primary temp dir");
            let worktree = tempfile::tempdir().expect("worktree temp dir");
            fs::write(
                primary.path().join("Cargo.toml"),
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
            )
            .expect("primary Cargo.toml");
            fs::write(
                worktree.path().join("Cargo.toml"),
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
            )
            .expect("worktree Cargo.toml");

            let primary_component = rust_script_component(
                primary.path(),
                "printf '%s' \"$CARGO_TARGET_DIR\" > cargo-target",
            );
            let worktree_component = rust_script_component(
                worktree.path(),
                "printf '%s' \"$CARGO_TARGET_DIR\" > cargo-target",
            );

            let primary_output = run_component_scripts(
                &primary_component,
                ExtensionCapability::Test,
                primary.path(),
                false,
            )
            .expect("primary component script should run");
            let worktree_output = run_component_scripts(
                &worktree_component,
                ExtensionCapability::Test,
                worktree.path(),
                false,
            )
            .expect("worktree component script should run");

            assert!(primary_output.success);
            assert!(worktree_output.success);
            let primary_target = fs::read_to_string(primary.path().join("cargo-target")).unwrap();
            let worktree_target = fs::read_to_string(worktree.path().join("cargo-target")).unwrap();

            assert_eq!(primary_target, worktree_target);
            assert!(
                primary_target.starts_with(
                    &home
                        .path()
                        .join(".local/share/homeboy/cargo-targets")
                        .to_string_lossy()
                        .to_string()
                ),
                "target dir should live under Homeboy data dir: {primary_target}"
            );
            assert!(
                primary_target.contains("fixture-"),
                "target dir should include component identity label: {primary_target}"
            );
        });
    });
}

#[test]
fn rust_component_scripts_respect_explicit_cargo_target_dir() {
    with_isolated_home(|_| {
        without_process_cargo_target_dir(|| {
            let dir = tempfile::tempdir().expect("temp dir");
            fs::write(
                dir.path().join("Cargo.toml"),
                "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
            )
            .expect("Cargo.toml");
            let explicit = dir.path().join("custom-target");
            let component = rust_script_component(
                dir.path(),
                "printf '%s' \"$CARGO_TARGET_DIR\" > cargo-target",
            );

            let output = run_component_scripts_with_env(
                &component,
                ExtensionCapability::Test,
                dir.path(),
                false,
                &[(
                    CARGO_TARGET_DIR_ENV.to_string(),
                    explicit.to_string_lossy().to_string(),
                )],
                &[],
            )
            .expect("component script should run");

            assert!(output.success);
            assert_eq!(
                fs::read_to_string(dir.path().join("cargo-target")).unwrap(),
                explicit.to_string_lossy()
            );
        });
    });
}

#[test]
fn rust_component_scripts_respect_process_cargo_target_dir() {
    with_isolated_home(|_| {
        let dir = tempfile::tempdir().expect("temp dir");
        fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"fixture\"\nversion = \"0.1.0\"\n",
        )
        .expect("Cargo.toml");
        let explicit = dir.path().join("process-target");
        with_process_cargo_target_dir(&explicit, || {
            let component = rust_script_component(
                dir.path(),
                "printf '%s' \"$CARGO_TARGET_DIR\" > cargo-target",
            );

            let output =
                run_component_scripts(&component, ExtensionCapability::Test, dir.path(), false)
                    .expect("component script should run");

            assert!(output.success);
            assert_eq!(
                fs::read_to_string(dir.path().join("cargo-target")).unwrap(),
                explicit.to_string_lossy()
            );
        });
    });
}

#[test]
fn test_run_component_scripts_with_run_dir() {
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
            "#!/bin/sh\nprintf 'extension ran\n' > \"$HOMEBOY_COMPONENT_PATH/extension-marker\"\n",
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
fn wordpress_phpunit_no_discovery_is_neutral_skipped() {
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
  "name": "WordPress",
  "version": "1.0.0",
  "test": { "extension_script": "test.sh" }
}"#,
        )
        .expect("extension manifest should be written");
        let extension_script = extension_dir.join("test.sh");
        fs::write(
            &extension_script,
            "#!/bin/sh\nprintf 'Plugin activation/install passed\\nNO PHPUNIT TEST FILES DISCOVERED\\nSkipping PHPUnit tests: no files matched the WordPress runner discovery contract.\\n'\n",
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
        assert_eq!(output.status, "skipped");
        assert_eq!(output.test_counts.expect("test counts").total, 0);
        let phase = output.phase.expect("phase report");
        assert_eq!(phase.status, crate::core::extension::PhaseStatus::Skipped);
        assert_eq!(
            phase.summary,
            "activation/install passed; PHPUnit discovery found zero tests; no PHPUnit assertions ran"
        );
    });
}

#[test]
fn wordpress_phpunit_no_discovery_can_be_required_as_failure() {
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
  "name": "WordPress",
  "version": "1.0.0",
  "test": { "extension_script": "test.sh" }
}"#,
        )
        .expect("extension manifest should be written");
        let extension_script = extension_dir.join("test.sh");
        fs::write(
            &extension_script,
            "#!/bin/sh\nprintf 'NO PHPUNIT TEST FILES DISCOVERED\\n'\n",
        )
        .expect("extension script should be written");
        let mut perms = fs::metadata(&extension_script)
            .expect("extension script metadata")
            .permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&extension_script, perms)
            .expect("extension script should be executable");

        let (output, exit_code) = run_test(
            test_command_args_with_setting(dir.path(), "require_phpunit_tests", "true"),
            &GlobalArgs {},
        )
        .expect("extension test should run");

        assert_eq!(exit_code, 1);
        assert!(!output.passed);
        assert_eq!(output.status, "failed");
        assert_eq!(output.test_counts.expect("test counts").total, 0);
        let failure = output.failure.expect("failure report");
        assert_eq!(
            failure.category,
            crate::core::extension::PhaseFailureCategory::Findings
        );
    });
}
