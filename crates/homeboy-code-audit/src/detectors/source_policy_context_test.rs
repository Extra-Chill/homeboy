//! Regression coverage for homeboy #11330.
//!
//! Each case below is a verbatim line from the `core-agnostic-source` findings
//! in #11298, resolved at the audited SHA `76c6c50cd`. They are reproduced here
//! rather than paraphrased so the fix is pinned to the evidence that motivated
//! it.

use homeboy_audit_contract::{
    SourcePolicyMatchMode, SourcePolicyRule, SourcePolicyRuleBody, SourcePolicyTerm,
    SourcePolicyTermContext,
};

use super::super::conventions::Language;
use super::super::findings::Finding;
use super::super::fingerprint::FileFingerprint;
use super::run;

fn fp(content: &str) -> FileFingerprint {
    FileFingerprint {
        relative_path: "crates/homeboy-agents/src/thing.rs".to_string(),
        language: Language::Rust,
        content: content.to_string(),
        ..Default::default()
    }
}

/// The real rule shape from `homeboy.json`: token match, case-insensitive,
/// scanning `crates/`.
fn rule(terms: Vec<SourcePolicyTerm>) -> SourcePolicyRule {
    SourcePolicyRule {
        id: "core-agnostic-source".to_string(),
        kind: "core_boundary_leak".to_string(),
        severity: "warning".to_string(),
        convention: "core_boundary_leak:core-agnostic-source".to_string(),
        language: None,
        file_extensions: Vec::new(),
        include_path_contains: vec!["crates/".to_string()],
        exclude_path_contains: Vec::new(),
        allow_line_contains: Vec::new(),
        ignore_line_prefixes: Vec::new(),
        scan_comments: false,
        ignore_after_line_equals: Vec::new(),
        example_path_contains: Vec::new(),
        example_classification: None,
        description: "term `{term}` at line {line}".to_string(),
        suggestion: "move it".to_string(),
        rule: SourcePolicyRuleBody::ForbiddenTerms {
            terms,
            default_match: SourcePolicyMatchMode::Token,
            case_insensitive: true,
        },
    }
}

fn term(value: &str, context: Option<SourcePolicyTermContext>) -> SourcePolicyTerm {
    SourcePolicyTerm {
        value: value.to_string(),
        label: None,
        match_mode: None,
        detect_split: false,
        context,
    }
}

fn findings(content: &str, terms: Vec<SourcePolicyTerm>) -> Vec<Finding> {
    let fp = fp(content);
    run(&[&fp], &[rule(terms)])
}

// ---------------------------------------------------------------- comments

#[test]
fn doc_comment_prose_is_not_a_leak() {
    // reference_docs.rs:290 and :304 in #11298.
    let content = "\
/// Globals are deliberately excluded: they are identical on every node and are
/// Records every visible command node under `path` that has no help text.
fn documented_positionals() {}
";

    assert!(
        findings(content, vec![term("node", None)]).is_empty(),
        "prose about nodes is not a Node.js dependency"
    );
}

#[test]
fn module_comment_command_examples_are_not_leaks() {
    // reference_docs.rs:26 and descriptors.rs:9/:11 in #11298.
    let content = "\
//! HOMEBOY_WRITE_CLI_REFERENCE=1 cargo test -p homeboy-cli --lib
/// rustfmt resolves the module tree by parsing and does not expand
/// `cargo fmt --all` -- fifteen subtrees, 33 files, drifting unformatted.
fn styled() {}
";

    let terms = vec![term("cargo", None), term("rustfmt", None)];
    assert!(
        findings(content, terms).is_empty(),
        "a comment showing how to run a tool does not invoke it"
    );
}

#[test]
fn a_leak_on_a_line_that_also_carries_a_comment_is_still_reported() {
    // Masking comments must not become a way to hide a real leak.
    let content = "fn dispatch() { run(\"composer\"); } // see the composer docs\n";

    let found = findings(content, vec![term("composer", None)]);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn scan_comments_opts_back_in() {
    let content = "/// mentions composer\n";
    let mut rule = rule(vec![term("composer", None)]);
    rule.scan_comments = true;
    let fp = fp(content);

    assert_eq!(run(&[&fp], &[rule]).len(), 1);
}

// ----------------------------------------------------------- term contexts

#[test]
fn string_literal_context_ignores_identifiers_named_like_the_term() {
    // artifacts.rs:148, agent_task_dependency_actions.rs:100/:228, and
    // agent_task_dependency_graph.rs:102/:197 in #11298 — every one of them a
    // graph node, not Node.js.
    let content = "\
fn collect() {
    let mut node = declaration;
    let head = required_node_value(node, \"head\", downstream_id)?;
    let ids = upstream.iter().map(|node| (node.id.clone(), 0usize));
}
";

    let terms = vec![term("node", Some(SourcePolicyTermContext::StringLiteral))];
    assert!(
        findings(content, terms).is_empty(),
        "a local named `node` is not an ecosystem dependency"
    );
}

#[test]
fn string_literal_context_still_reports_the_term_as_data() {
    // agent_task_gate_executor.rs:162 in #11298 — a real hit that must survive.
    let content = "fn gate_argv() -> Vec<String> { vec![\"node\".to_string()] }\n";

    let terms = vec![term("node", Some(SourcePolicyTermContext::StringLiteral))];
    let found = findings(content, terms);

    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].description.contains("node"));
}

#[test]
fn code_context_remains_the_default_and_still_sees_identifiers() {
    // Narrowing is opt-in per term; unscoped terms keep full strength so that
    // a core function named `wordpress_*` is still a finding.
    let content = "fn wordpress_plugin_path() -> String { String::new() }\n";

    let found = findings(content, vec![term("wordpress", None)]);
    assert_eq!(found.len(), 1, "{found:?}");
}

#[test]
fn string_literal_context_reports_the_correct_line() {
    let content = "fn f() {\n    let x = 1;\n    run(\"node\");\n}\n";

    let terms = vec![term("node", Some(SourcePolicyTermContext::StringLiteral))];
    let found = findings(content, terms);

    assert_eq!(found.len(), 1);
    assert!(
        found[0].description.contains("line 3"),
        "masking must preserve line numbers: {}",
        found[0].description
    );
}

// -------------------------------------------------------------- allowlists

#[test]
fn allow_line_contains_reads_the_source_as_written() {
    // The Cargo build-script protocol string is unavoidable in any Rust build
    // script, so it is allowlisted rather than detected.
    let content = "    println!(\"cargo:rerun-if-changed={}\", path.display());\n";

    let mut rule = rule(vec![term("cargo", None)]);
    rule.allow_line_contains = vec!["cargo:rerun-if-changed".to_string()];
    let fp = fp(content);

    assert!(run(&[&fp], &[rule]).is_empty());
}

#[test]
fn the_cargo_directive_is_a_finding_without_the_allowlist() {
    // Pin that the allowlist is what silences it — not the comment mask, which
    // would be the wrong reason and would hide real string-literal leaks.
    let content = "    println!(\"cargo:rerun-if-changed={}\", path.display());\n";

    assert_eq!(findings(content, vec![term("cargo", None)]).len(), 1);
}
