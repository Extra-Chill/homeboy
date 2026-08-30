use super::super::hints::build_autofix_hint;
use super::lint_args;

#[test]
fn autofix_hint_preserves_changed_since_scope() {
    let mut args = lint_args();
    args.path_override = Some("/tmp/pr checkout".to_string());
    args.changed_since = Some("origin/main".to_string());

    let hint = build_autofix_hint(&args);

    assert_eq!(
        hint,
        "Auto-fix: homeboy refactor demo --path '/tmp/pr checkout' --changed-since origin/main --from lint --write"
    );
}

#[test]
fn autofix_hint_preserves_changed_only_and_file_scope() {
    let mut args = lint_args();
    args.file = Some("src/lib.rs".to_string());
    args.changed_only = true;

    let hint = build_autofix_hint(&args);

    assert_eq!(hint, "Auto-fix: homeboy refactor demo --from lint --write");
}
