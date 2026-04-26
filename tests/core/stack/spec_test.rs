//! Spec parsing + path expansion tests for `core::stack::spec`.

use crate::stack::spec::{expand_path, parse_git_ref, GitRef, StackPrEntry, StackSpec};

const STUDIO_FIXTURE: &str = r#"{
    "id": "studio-combined",
    "description": "dev/combined-fixes for Studio",
    "component": "studio",
    "component_path": "~/Developer/studio",
    "base":   { "remote": "origin", "branch": "trunk" },
    "target": { "remote": "fork",   "branch": "dev/combined-fixes" },
    "prs": [
        { "repo": "Automattic/studio", "number": 3057, "note": "native spawn" },
        { "repo": "Automattic/studio", "number": 3095 }
    ]
}"#;

#[test]
fn parses_canonical_studio_fixture() {
    let spec: StackSpec = serde_json::from_str(STUDIO_FIXTURE).expect("parse");
    assert_eq!(spec.id, "studio-combined");
    assert_eq!(spec.component, "studio");
    assert_eq!(spec.base.remote, "origin");
    assert_eq!(spec.base.branch, "trunk");
    assert_eq!(spec.target.remote, "fork");
    // Multi-segment branch survives intact (no remote-side splitting).
    assert_eq!(spec.target.branch, "dev/combined-fixes");
    assert_eq!(spec.prs.len(), 2);
    assert_eq!(spec.prs[0].repo, "Automattic/studio");
    assert_eq!(spec.prs[0].number, 3057);
    assert_eq!(spec.prs[0].note.as_deref(), Some("native spawn"));
    assert!(spec.prs[1].note.is_none());
}

#[test]
fn round_trips_via_serde() {
    let spec: StackSpec = serde_json::from_str(STUDIO_FIXTURE).expect("parse");
    let serialized = serde_json::to_string(&spec).expect("serialize");
    let again: StackSpec = serde_json::from_str(&serialized).expect("re-parse");
    assert_eq!(again.id, spec.id);
    assert_eq!(again.prs.len(), spec.prs.len());
    assert_eq!(again.prs[0].number, spec.prs[0].number);
}

#[test]
fn missing_required_field_errors() {
    // No `base` field — serde must reject it (no default).
    let bad = r#"{
        "component": "studio",
        "component_path": "~/x",
        "target": { "remote": "fork", "branch": "dev" }
    }"#;
    let err = serde_json::from_str::<StackSpec>(bad).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("base"),
        "expected missing-field error mentioning `base`, got: {}",
        msg
    );
}

#[test]
fn empty_id_filled_from_loader_not_serde() {
    // Spec on disk may omit `id` (filename is the source of truth) — serde
    // honors `#[serde(default)]` and yields an empty id.
    let no_id = r#"{
        "component": "studio",
        "component_path": "~/Developer/studio",
        "base":   { "remote": "origin", "branch": "trunk" },
        "target": { "remote": "fork",   "branch": "dev" }
    }"#;
    let spec: StackSpec = serde_json::from_str(no_id).expect("parse");
    assert_eq!(spec.id, "");
    // `load()` is what fills it from the filename; serde doesn't.
}

#[test]
fn empty_prs_array_is_valid() {
    let no_prs = r#"{
        "component": "studio",
        "component_path": "~/x",
        "base":   { "remote": "o", "branch": "trunk" },
        "target": { "remote": "f", "branch": "dev" }
    }"#;
    let spec: StackSpec = serde_json::from_str(no_prs).expect("parse");
    assert_eq!(spec.prs.len(), 0);
}

#[test]
fn parse_git_ref_splits_on_first_slash_only() {
    let r = parse_git_ref("origin/trunk", "base").expect("parse");
    assert_eq!(r.remote, "origin");
    assert_eq!(r.branch, "trunk");

    // Multi-segment branch keeps the rest intact.
    let r = parse_git_ref("fork/dev/combined-fixes", "target").expect("parse");
    assert_eq!(r.remote, "fork");
    assert_eq!(r.branch, "dev/combined-fixes");
}

#[test]
fn parse_git_ref_rejects_no_slash() {
    let err = parse_git_ref("trunk", "base").unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("remote") && msg.contains("branch"),
        "expected slash-format error, got: {}",
        msg
    );
}

#[test]
fn parse_git_ref_rejects_empty_components() {
    assert!(parse_git_ref("/trunk", "base").is_err());
    assert!(parse_git_ref("origin/", "base").is_err());
    assert!(parse_git_ref("", "base").is_err());
}

#[test]
fn git_ref_display_round_trip_format() {
    let r = GitRef {
        remote: "origin".into(),
        branch: "trunk".into(),
    };
    assert_eq!(r.display(), "origin/trunk");

    let r = GitRef {
        remote: "fork".into(),
        branch: "dev/combined-fixes".into(),
    };
    assert_eq!(r.display(), "fork/dev/combined-fixes");
}

#[test]
fn expand_path_expands_tilde() {
    // Use $HOME instead of asserting on a literal so the test is portable.
    let home = std::env::var("HOME").unwrap_or_default();
    let expanded = expand_path("~/Developer/foo");
    assert!(
        expanded.starts_with(&home),
        "expected tilde to expand to $HOME, got: {}",
        expanded
    );
    assert!(expanded.ends_with("/Developer/foo"));
}

#[test]
fn expand_path_substitutes_env_vars() {
    // Use a uniquely-named env var to avoid collision with parallel tests.
    std::env::set_var("HOMEBOY_STACK_TEST_VAR", "magic-value");
    let out = expand_path("/prefix/${env.HOMEBOY_STACK_TEST_VAR}/suffix");
    assert_eq!(out, "/prefix/magic-value/suffix");
    std::env::remove_var("HOMEBOY_STACK_TEST_VAR");
}

#[test]
fn expand_path_unknown_token_left_literal() {
    let out = expand_path("/prefix/${unknown.thing}/suffix");
    // Unknown tokens stay verbatim so the resulting path errors loudly
    // when used (not silently empty).
    assert_eq!(out, "/prefix/${unknown.thing}/suffix");
}

#[test]
fn expand_path_unset_env_var_becomes_empty() {
    std::env::remove_var("HOMEBOY_STACK_TEST_NEVER_SET_XYZ");
    let out = expand_path("/a/${env.HOMEBOY_STACK_TEST_NEVER_SET_XYZ}/b");
    assert_eq!(out, "/a//b");
}

#[test]
fn pr_entry_serializes_without_optional_fields() {
    let entry = StackPrEntry {
        repo: "Automattic/studio".into(),
        number: 1,
        note: None,
    };
    let json = serde_json::to_string(&entry).unwrap();
    assert!(!json.contains("note"));
    assert!(json.contains("\"number\":1"));
}
