use super::*;

#[test]
fn lint_overlap_is_advisory_and_preserves_commands() {
    let gates = VerifyGateOptions {
        verify: vec![
            "cargo check --workspace --all-targets --locked".into(),
            "HOMEBOY_LINT_BASE=abc homeboy --placement local review lint homeboy --path .".into(),
        ],
        ..Default::default()
    };
    let before = gates.verify.clone();
    assert!(gates.lint_overlap_warning().is_some());
    assert_eq!(gates.verify, before);
}

#[test]
fn lint_overlap_does_not_expose_private_commands_or_match_substrings() {
    let mut gates = VerifyGateOptions {
        verify: vec!["cargo fmt --all -- --check".into()],
        private_verify: vec!["homeboy review lint private-component".into()],
        ..Default::default()
    };
    assert!(gates.lint_overlap_warning().is_none());
    gates.verify.push("echo review-lint".into());
    assert!(gates.lint_overlap_warning().is_none());
    gates.verify.push("homeboy review lint homeboy".into());
    assert!(gates.lint_overlap_warning().is_some());
}
