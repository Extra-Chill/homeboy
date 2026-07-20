use super::*;
use crate::conventions::Language;

fn fp(path: &str, content: &str) -> FileFingerprint {
    FileFingerprint {
        relative_path: path.to_string(),
        language: Language::Rust,
        content: content.to_string(),
        ..Default::default()
    }
}

#[test]
fn flags_literal_duplicating_a_constant_defined_elsewhere() {
    let def = fp(
        "src/sync.rs",
        r#"const WORKSPACE_METADATA_FILE: &str = ".homeboy/runner-workspace.json";"#,
    );
    let user = fp(
        "src/harvest.rs",
        "fn go() {\n    workspace.join(\".homeboy/runner-workspace.json\");\n}\n",
    );
    let findings = detect_constant_bypass_literals(&[&def, &user]);
    assert_eq!(findings.len(), 1, "one bypass in harvest.rs");
    assert_eq!(findings[0].kind, AuditFinding::ConstantBypassLiteral);
    assert_eq!(findings[0].file, "src/harvest.rs");
    assert!(findings[0].description.contains("WORKSPACE_METADATA_FILE"));
    assert!(findings[0].description.contains("src/sync.rs"));
}

#[test]
fn does_not_flag_the_constant_definition_itself() {
    let def = fp(
        "src/schema.rs",
        r#"pub const AGENT_TASK_AGGREGATE_SCHEMA: &str = "homeboy/agent-task-aggregate/v1";"#,
    );
    // Only the definition exists; nobody bypasses it.
    assert!(detect_constant_bypass_literals(&[&def]).is_empty());
}

#[test]
fn ignores_short_values_below_the_length_floor() {
    let def = fp("src/a.rs", r#"const MODE: &str = "snapshot";"#);
    let user = fp("src/b.rs", "fn f() { let m = \"snapshot\"; }");
    assert!(
        detect_constant_bypass_literals(&[&def, &user]).is_empty(),
        "short idiomatic values must not be flagged"
    );
}

#[test]
fn ignores_test_files_as_both_source_and_site() {
    let def = fp(
        "src/schema.rs",
        r#"const LONG_SCHEMA_KEY: &str = "homeboy/some-schema/v1";"#,
    );
    let test_site = fp(
        "src/foo/tests.rs",
        "fn t() { assert_eq!(x, \"homeboy/some-schema/v1\"); }",
    );
    assert!(
        detect_constant_bypass_literals(&[&def, &test_site]).is_empty(),
        "a literal inside a test file must not be flagged"
    );
}

#[test]
fn re_export_const_line_is_not_flagged() {
    let def = fp(
        "src/a.rs",
        r#"const CANONICAL_METADATA_KEY: &str = "controller_runtime_key";"#,
    );
    // A second const with the same value on its own decl line is not a bypass.
    let reexport = fp(
        "src/b.rs",
        r#"const ALIAS_METADATA_KEY: &str = "controller_runtime_key";"#,
    );
    assert!(
        detect_constant_bypass_literals(&[&def, &reexport]).is_empty(),
        "a constant declaration line is not a bypass site"
    );
}
