use super::super::workflow::{run_main_lint_workflow, run_self_check_lint_workflow};
use super::{component, lint_args};
use crate::core::component::{Component, ComponentScriptsConfig};
use crate::core::engine::run_dir::RunDir;

#[test]
fn test_run_self_check_lint_workflow() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::fs::write(dir.path().join("lint.sh"), "printf lint-ok\n")
        .expect("script should be written");

    let mut component = Component::new(
        "fixture".to_string(),
        dir.path().to_string_lossy().to_string(),
        "".to_string(),
        None,
    );
    component.scripts = Some(ComponentScriptsConfig {
        lint: vec!["sh lint.sh".to_string()],
        test: Vec::new(),
        build: Vec::new(),
        bench: Vec::new(),
        fuzz: Vec::new(),
        trace: Vec::new(),
        deps: Vec::new(),
    });

    let result = run_self_check_lint_workflow(&component, dir.path(), "fixture".to_string(), false)
        .expect("lint self-check should run");

    assert_eq!(result.status, "passed");
    assert_eq!(result.exit_code, 0);
    assert_eq!(result.component, "fixture");
}

#[test]
fn test_run_main_lint_workflow() {
    let dir = tempfile::tempdir().expect("temp dir");
    std::process::Command::new("git")
        .args(["init"])
        .current_dir(dir.path())
        .output()
        .expect("git init should run");
    let run_dir = RunDir::create().expect("run dir");
    let mut args = lint_args();
    args.changed_only = true;

    let result = run_main_lint_workflow(
        &component(&dir.path().to_string_lossy()),
        dir.path(),
        args,
        &run_dir,
    )
    .expect("unchanged git repo should skip lint runner");

    assert_eq!(result.status, "passed");
    assert_eq!(result.exit_code, 0);
    assert!(result.findings.is_none());
}

#[test]
fn lint_config_deserializes_changed_file_routes() {
    let config: crate::core::extension::LintConfig = serde_json::from_str(
        r#"{
                "extension_script": "scripts/lint.sh",
                "changed_file_routes": [
                    { "extensions": ["php"], "step": "phpcs,phpstan" },
                    { "globs": ["assets/**/*.css"], "step": "stylelint" }
                ]
            }"#,
    )
    .expect("parse lint config");

    assert_eq!(config.changed_file_routes.len(), 2);
    assert_eq!(config.changed_file_routes[0].extensions, vec!["php"]);
    assert_eq!(config.changed_file_routes[0].step, "phpcs,phpstan");
    assert_eq!(config.changed_file_routes[1].globs, vec!["assets/**/*.css"]);
    assert_eq!(config.changed_file_routes[1].step, "stylelint");
}
