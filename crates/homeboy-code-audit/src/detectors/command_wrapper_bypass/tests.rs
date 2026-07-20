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
fn flags_raw_argvector_that_bypasses_a_thin_wrapper() {
    let helper = fp(
        "src/git/primitives_query.rs",
        "pub fn head_sha(git_root: &Path) -> Option<String> {\n    output_optional(git_root, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    let user = fp(
        "src/finalization/backend.rs",
        "fn go(path: &str) {\n    let head = git_output(path, &[\"rev-parse\", \"HEAD\"])?;\n}\n",
    );
    let findings = detect_command_wrapper_bypass(&[&helper, &user]);
    assert_eq!(findings.len(), 1, "one bypass in backend.rs");
    assert_eq!(findings[0].kind, AuditFinding::CommandWrapperBypass);
    assert_eq!(findings[0].file, "src/finalization/backend.rs");
    assert!(findings[0].description.contains("head_sha"));
    assert!(findings[0].description.contains("rev-parse HEAD"));
}

#[test]
fn does_not_flag_the_wrapper_definition_itself() {
    let helper = fp(
        "src/git/primitives_query.rs",
        "pub fn head_sha(git_root: &Path) -> Option<String> {\n    output_optional(git_root, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    assert!(
        detect_command_wrapper_bypass(&[&helper]).is_empty(),
        "the wrapper's own arg-vector is not a bypass"
    );
}

#[test]
fn ignores_single_element_generic_commands() {
    let helper = fp(
        "src/git/x.rs",
        "pub fn init_repo(p: &Path) -> Result<()> {\n    run(p, &[\"init\"])\n}\n",
    );
    let user = fp("src/y.rs", "fn f(p: &str) { run(p, &[\"init\"]); }");
    assert!(
        detect_command_wrapper_bypass(&[&helper, &user]).is_empty(),
        "single-arg commands are too generic to attribute to one wrapper"
    );
}

#[test]
fn ignores_test_files() {
    let helper = fp(
        "src/git/x.rs",
        "pub fn head_sha(p: &Path) -> Option<String> {\n    output_optional(p, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    let test_site = fp(
        "src/foo/tests.rs",
        "fn t(p: &str) { let h = git(p, &[\"rev-parse\", \"HEAD\"]); }",
    );
    assert!(
        detect_command_wrapper_bypass(&[&helper, &test_site]).is_empty(),
        "raw arg-vectors inside test files are not flagged"
    );
}

#[test]
fn does_not_treat_a_dynamic_arg_wrapper_as_canonical() {
    // `rev_parse(ref)` passes a NON-literal element, so it is not a fixed
    // canonical command and must not seed the wrapper map.
    let dynamic = fp(
        "src/git/x.rs",
        "pub fn rev_parse(root: &Path, git_ref: &str) -> Option<String> {\n    output_optional(root, &[\"rev-parse\", git_ref])\n}\n",
    );
    let user = fp(
        "src/y.rs",
        "fn f(p: &str) { let x = git(p, &[\"rev-parse\", \"HEAD\"]); }",
    );
    assert!(
        detect_command_wrapper_bypass(&[&dynamic, &user]).is_empty(),
        "a wrapper taking a dynamic arg is not a fixed-command canonical wrapper"
    );
}

#[test]
fn ignores_argvectors_inside_inline_cfg_test_blocks() {
    let helper = fp(
        "src/git/x.rs",
        "pub fn head_sha(p: &Path) -> Option<String> {\n    output_optional(p, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    // A production file whose ONLY raw arg-vector lives in its inline
    // `#[cfg(test)] mod tests { … }` block — a fixture reading a freshly-built
    // repo, not production command drift.
    let production_with_inline_tests = fp(
        "src/agent/materialization.rs",
        "fn prod(p: &str) {\n    do_something(p);\n}\n\n#[cfg(test)]\nmod tests {\n    fn setup(p: &str) {\n        let head = git(p, &[\"rev-parse\", \"HEAD\"]);\n    }\n}\n",
    );
    assert!(
        detect_command_wrapper_bypass(&[&helper, &production_with_inline_tests]).is_empty(),
        "raw arg-vectors inside inline #[cfg(test)] blocks are test fixtures, not bypasses"
    );
}

#[test]
fn still_flags_production_argvector_alongside_an_inline_test_block() {
    let helper = fp(
        "src/git/x.rs",
        "pub fn head_sha(p: &Path) -> Option<String> {\n    output_optional(p, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    // Same file has a REAL production bypass plus a test-only arg-vector; only
    // the production site is flagged, proving the cfg(test) skip is scoped to
    // the test region and doesn't suppress legitimate findings.
    let mixed = fp(
        "src/agent/materialization.rs",
        "fn prod(p: &str) {\n    let head = git(p, &[\"rev-parse\", \"HEAD\"]);\n}\n\n#[cfg(test)]\nmod tests {\n    fn setup(p: &str) {\n        let h = git(p, &[\"rev-parse\", \"HEAD\"]);\n    }\n}\n",
    );
    let findings = detect_command_wrapper_bypass(&[&helper, &mixed]);
    assert_eq!(
        findings.len(),
        1,
        "the production bypass is flagged, the inline-test one is not"
    );
    assert_eq!(findings[0].file, "src/agent/materialization.rs");
}
