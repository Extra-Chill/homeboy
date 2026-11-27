use super::super::hints::build_autofix_hint;
use super::lint_args;

#[test]
fn autofix_hint_preserves_lint_scope() {
    let mut changed_since = lint_args();
    changed_since.path_override = Some("/tmp/pr checkout".to_string());
    changed_since.changed_since = Some("origin/main".to_string());

    let mut changed_only = lint_args();
    changed_only.file = Some("src/lib.rs".to_string());
    changed_only.changed_only = true;

    for (args, expected) in [
        (
            changed_since,
            "Auto-fix: homeboy review lint demo --path '/tmp/pr checkout' --changed-since origin/main --fix",
        ),
        (
            changed_only,
            "Auto-fix: homeboy review lint demo --file src/lib.rs --changed-only --fix",
        ),
    ] {
        assert_eq!(build_autofix_hint(&args), expected);
    }
}
