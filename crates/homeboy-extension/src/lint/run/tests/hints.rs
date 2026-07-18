use super::super::hints::build_autofix_hint;
use super::lint_args;

#[test]
fn autofix_hint_preserves_changed_since_scope() {
    let mut args = lint_args();
    args.path_override = Some("/tmp/pr checkout".to_string());
    args.changed_since = Some("origin/main".to_string());

    let hint = build_autofix_hint(&args);

    assert!(hint
        .contains("homeboy lint demo --path '/tmp/pr checkout' --changed-since origin/main --fix"));
    assert!(hint.contains(
        "homeboy refactor demo --path '/tmp/pr checkout' --changed-since origin/main --from lint --write"
    ));
}

#[test]
fn autofix_hint_preserves_changed_only_and_file_scope() {
    let mut args = lint_args();
    args.file = Some("src/lib.rs".to_string());
    args.changed_only = true;

    let hint = build_autofix_hint(&args);

    assert!(hint.contains("homeboy lint demo --file src/lib.rs --changed-only --fix"));
    assert!(!hint.contains("homeboy refactor"));
}
