use homeboy::commands::lint::{run as run_lint, LintArgs};
use homeboy::commands::test::{run as run_test, TestArgs};
use homeboy::commands::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, LintSniffArgs, PositionalComponentArgs, SettingArgs,
};
use homeboy::commands::GlobalArgs;
use std::fs;
use std::path::Path;

fn write_component(root: &Path, scripts: &str) {
    fs::write(
        root.join("homeboy.json"),
        format!(
            r#"{{
  "id": "fixture",
  "scripts": {}
}}"#,
            scripts
        ),
    )
    .expect("homeboy.json should be written");
}

fn write_legacy_self_checks_component(root: &Path) {
    fs::write(
        root.join("homeboy.json"),
        r#"{
  "id": "fixture",
  "self_checks": { "lint": ["sh scripts/lint.sh"] }
}"#,
    )
    .expect("homeboy.json should be written");
}

fn write_script(root: &Path, name: &str, body: &str) {
    let script_dir = root.join("scripts");
    fs::create_dir_all(&script_dir).expect("script dir should be created");
    fs::write(script_dir.join(name), body).expect("script should be written");
}

fn component_args(root: &Path) -> PositionalComponentArgs {
    PositionalComponentArgs {
        component: Some("fixture".to_string()),
        path: Some(root.to_string_lossy().to_string()),
    }
}

fn lint_args(root: &Path) -> LintArgs {
    LintArgs {
        comp: component_args(root),
        extension_override: ExtensionOverrideArgs::default(),
        summary: false,
        file: None,
        glob: None,
        changed_only: false,
        changed_since: None,
        precomputed_changed_files: None,
        lab_changed_files_json: None,
        ci_job: None,
        sniff_filters: LintSniffArgs::default(),
        category: None,
        fix: false,
        force: false,
        setting_args: SettingArgs::default(),
        baseline_args: BaselineArgs::default(),
        json_summary: false,
    }
}

fn test_args(root: &Path) -> TestArgs {
    TestArgs {
        comp: component_args(root),
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
    }
}

fn json_test_args(root: &Path) -> TestArgs {
    let mut args = test_args(root);
    args.json_summary = true;
    args
}

#[test]
fn lint_runs_declared_self_check_without_extensions() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_component(dir.path(), r#"{ "lint": ["sh scripts/lint.sh"] }"#);
    write_script(dir.path(), "lint.sh", "printf 'lint self-check ran\\n'\n");

    let (output, exit_code) =
        run_lint(lint_args(dir.path()), &GlobalArgs {}).expect("lint self-check should run");

    assert_eq!(exit_code, 0);
    assert!(output.passed);
    assert_eq!(output.component, "fixture");
}

#[test]
fn test_runs_declared_self_check_without_extensions() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_component(dir.path(), r#"{ "test": ["sh scripts/test.sh"] }"#);
    write_script(dir.path(), "test.sh", "printf 'test self-check ran\\n'\n");

    let (output, exit_code) =
        run_test(test_args(dir.path()), &GlobalArgs {}).expect("test self-check should run");

    assert_eq!(exit_code, 0);
    assert!(output.passed);
    assert_eq!(output.component, "fixture");
}

#[test]
fn non_zero_self_check_fails_command_and_surfaces_output() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_component(dir.path(), r#"{ "test": ["sh scripts/fail.sh"] }"#);
    write_script(
        dir.path(),
        "fail.sh",
        "printf 'visible failure stdout\\n'\nprintf 'visible failure stderr\\n' >&2\nexit 7\n",
    );

    let (output, exit_code) = run_test(test_args(dir.path()), &GlobalArgs {})
        .expect("test self-check failure should return structured output");

    assert_eq!(exit_code, 7);
    assert!(!output.passed);
    assert_eq!(output.status, "failed");
    let raw = output
        .raw_output
        .expect("failure should include raw output");
    assert!(raw.stdout_tail.contains("visible failure stdout"));
    assert!(raw.stderr_tail.contains("visible failure stderr"));
}

#[test]
fn json_self_check_failure_reports_bounded_large_output_metadata() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_component(dir.path(), r#"{ "test": ["sh scripts/large-fail.sh"] }"#);
    write_script(
        dir.path(),
        "large-fail.sh",
        "perl -e 'print \"stdout-line-\" . (\"x\" x 80) . \"\\n\" for 1..800'\nperl -e 'print STDERR \"stderr-line-\" . (\"x\" x 80) . \"\\n\" for 1..800'\nexit 7\n",
    );

    let (output, exit_code) = run_test(json_test_args(dir.path()), &GlobalArgs {})
        .expect("test self-check failure should return structured output");

    assert_eq!(exit_code, 7);
    assert!(!output.passed);
    let raw = output
        .raw_output
        .expect("failure should include bounded raw output metadata");
    assert!(raw.truncated);
    assert!(raw.stdout_truncated);
    assert!(raw.stderr_truncated);
    assert!(raw.stdout_seen_bytes > raw.stdout_limit_bytes);
    assert!(raw.stderr_seen_bytes > raw.stderr_limit_bytes);
    assert!(raw.stdout_retained_bytes <= raw.stdout_limit_bytes);
    assert!(raw.stderr_retained_bytes <= raw.stderr_limit_bytes);
    assert!(raw.stdout_tail.contains("stdout-line"));
    assert!(raw.stderr_tail.contains("stderr-line"));
}

#[test]
fn missing_extension_and_self_check_keeps_existing_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_component(dir.path(), r#"{}"#);

    let err = match run_lint(lint_args(dir.path()), &GlobalArgs {}) {
        Ok(_) => panic!("lint without extension or self-check should fail"),
        Err(err) => err,
    };

    assert_eq!(err.code.as_str(), "extension.unsupported");
    assert!(
        err.to_string()
            .contains("No extension provider configured for component 'fixture'"),
        "unexpected error: {err}"
    );
}

#[test]
fn legacy_self_checks_are_not_a_script_source() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_legacy_self_checks_component(dir.path());
    write_script(
        dir.path(),
        "lint.sh",
        "printf 'legacy lint should not run\\n'\n",
    );

    let err = match run_lint(lint_args(dir.path()), &GlobalArgs {}) {
        Ok(_) => panic!("legacy self_checks should not satisfy lint"),
        Err(err) => err,
    };

    assert_eq!(err.code.as_str(), "extension.unsupported");
    assert!(
        err.to_string()
            .contains("No extension provider configured for component 'fixture'"),
        "unexpected error: {err}"
    );
}
