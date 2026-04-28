use homeboy::commands::lint::{run as run_lint, LintArgs};
use homeboy::commands::test::{run as run_test, TestArgs};
use homeboy::commands::utils::args::{
    BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs,
};
use homeboy::commands::GlobalArgs;
use std::fs;
use std::path::Path;

fn write_component(root: &Path, self_checks: &str) {
    fs::write(
        root.join("homeboy.json"),
        format!(
            r#"{{
  "id": "fixture",
  "self_checks": {}
}}"#,
            self_checks
        ),
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
        summary: false,
        file: None,
        glob: None,
        changed_only: false,
        changed_since: None,
        errors_only: false,
        sniffs: None,
        exclude_sniffs: None,
        category: None,
        fix: false,
        setting_args: SettingArgs::default(),
        baseline_args: BaselineArgs::default(),
        _json: HiddenJsonArgs::default(),
    }
}

fn test_args(root: &Path) -> TestArgs {
    TestArgs {
        comp: component_args(root),
        skip_lint: false,
        coverage: false,
        coverage_min: None,
        baseline_args: BaselineArgs::default(),
        analyze: false,
        drift: false,
        write: false,
        since: "HEAD~10".to_string(),
        changed_since: None,
        setting_args: SettingArgs::default(),
        args: Vec::new(),
        _json: HiddenJsonArgs::default(),
        json_summary: false,
    }
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
fn missing_extension_and_self_check_keeps_existing_error() {
    let dir = tempfile::tempdir().expect("temp dir");
    write_component(dir.path(), r#"{}"#);

    let err = match run_lint(lint_args(dir.path()), &GlobalArgs {}) {
        Ok(_) => panic!("lint without extension or self-check should fail"),
        Err(err) => err,
    };

    assert!(
        err.to_string()
            .contains("Component 'fixture' has no extensions configured"),
        "unexpected error: {err}"
    );
}
