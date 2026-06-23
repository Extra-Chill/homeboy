use super::super::scoping::{build_changed_lint_runs, build_changed_lint_runs_with_routes};
use super::super::types::ScopedLintRun;
use super::{component, split_lint_routes};
use crate::core::component::Component;
use crate::core::extension::LintChangedFileRoute;

#[test]
fn manifest_changed_php_files_route_to_php_steps_only() {
    let component = component("/repo");
    let runs = build_changed_lint_runs_with_routes(
        &component,
        &["sample-plugin.php".to_string(), "inc/Foo.php".to_string()],
        &split_lint_routes(),
    );

    assert_eq!(
        runs,
        vec![ScopedLintRun {
            glob: "{/repo/sample-plugin.php,/repo/inc/Foo.php}".to_string(),
            step: Some("phpcs,phpstan".to_string()),
            changed_files: vec!["sample-plugin.php".to_string(), "inc/Foo.php".to_string()],
        }]
    );
}

#[test]
fn manifest_changed_markdown_files_do_not_route_to_eslint() {
    let component = component("/repo");
    let runs = build_changed_lint_runs_with_routes(
        &component,
        &[
            "docs/core-system/agent-bundles.md".to_string(),
            "README.md".to_string(),
        ],
        &split_lint_routes(),
    );

    assert!(runs.is_empty());
}

#[test]
fn manifest_changed_mixed_php_and_js_files_split_by_runner() {
    let component = component("/repo");
    let runs = build_changed_lint_runs_with_routes(
        &component,
        &[
            "inc/Foo.php".to_string(),
            "docs/notes.md".to_string(),
            "assets/app.js".to_string(),
            "assets/view.tsx".to_string(),
        ],
        &split_lint_routes(),
    );

    assert_eq!(
        runs,
        vec![
            ScopedLintRun {
                glob: "/repo/inc/Foo.php".to_string(),
                step: Some("phpcs,phpstan".to_string()),
                changed_files: vec!["inc/Foo.php".to_string()],
            },
            ScopedLintRun {
                glob: "{/repo/assets/app.js,/repo/assets/view.tsx}".to_string(),
                step: Some("eslint".to_string()),
                changed_files: vec!["assets/app.js".to_string(), "assets/view.tsx".to_string()],
            },
        ]
    );
}

#[test]
fn manifest_changed_files_can_route_by_glob() {
    let component = component("/repo");
    let routes = vec![LintChangedFileRoute {
        extensions: Vec::new(),
        globs: vec!["assets/**/*.css".to_string()],
        step: "stylelint".to_string(),
    }];
    let runs = build_changed_lint_runs_with_routes(
        &component,
        &["assets/css/admin.css".to_string(), "README.md".to_string()],
        &routes,
    );

    assert_eq!(
        runs,
        vec![ScopedLintRun {
            glob: "/repo/assets/css/admin.css".to_string(),
            step: Some("stylelint".to_string()),
            changed_files: vec!["assets/css/admin.css".to_string()],
        }]
    );
}

#[test]
fn non_wordpress_changed_files_keep_existing_single_runner_scope() {
    let component = Component::new(
        "fixture".to_string(),
        "/repo".to_string(),
        "".to_string(),
        None,
    );
    let runs = build_changed_lint_runs(
        &component,
        &["src/main.rs".to_string(), "README.md".to_string()],
    );

    assert_eq!(
        runs,
        vec![ScopedLintRun {
            glob: "{/repo/src/main.rs,/repo/README.md}".to_string(),
            step: None,
            changed_files: vec!["src/main.rs".to_string(), "README.md".to_string()],
        }]
    );
}
