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
fn does_not_flag_a_sibling_thin_wrapper_with_a_different_return_contract() {
    // `head_sha` returns Option (best-effort read); `get_head_commit` is a
    // separate canonical thin wrapper around the SAME arg-vector that returns
    // Result (error-propagating, for callers that must not silently swallow a
    // failed rev-parse). Both are legitimate sibling helpers — callers pick the
    // contract they need. `get_head_commit` is NOT a raw bypass of `head_sha`;
    // flagging it as one and telling callers to "call head_sha instead" would
    // downgrade error handling. A thin wrapper delegating an arg-vector is a
    // canonical definition, not drift.
    let option_helper = fp(
        "src/git/primitives_query.rs",
        "pub fn head_sha(git_root: &Path) -> Option<String> {\n    output_optional(git_root, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    let result_helper = fp(
        "src/git/operations_tags.rs",
        "pub fn get_head_commit(path: &str) -> Result<String> {\n    run_in(path, \"git\", &[\"rev-parse\", \"HEAD\"], \"get HEAD commit\")\n}\n",
    );
    let findings = detect_command_wrapper_bypass(&[&option_helper, &result_helper]);
    assert!(
        findings.is_empty(),
        "a sibling thin wrapper around the same arg-vector is a canonical helper, not a bypass; got: {findings:?}"
    );
}

#[test]
fn still_flags_a_raw_call_in_a_non_wrapper_function_even_beside_sibling_wrappers() {
    // Guard against over-suppression: the sibling-wrapper skip must be scoped to
    // genuine thin wrappers. A raw arg-vector inside a function that does real
    // work (not a one-call delegation) is still a bypass.
    let helper = fp(
        "src/git/primitives_query.rs",
        "pub fn head_sha(git_root: &Path) -> Option<String> {\n    output_optional(git_root, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    let user = fp(
        "src/finalization/backend.rs",
        "fn go(path: &str) {\n    let before = do_setup(path);\n    let head = git_output(path, &[\"rev-parse\", \"HEAD\"])?;\n    finalize(before, head);\n}\n",
    );
    let findings = detect_command_wrapper_bypass(&[&helper, &user]);
    assert_eq!(
        findings.len(),
        1,
        "raw call in a working function is still a bypass"
    );
    assert_eq!(findings[0].file, "src/finalization/backend.rs");
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

#[test]
fn does_not_flag_cross_crate_call_of_a_private_helper() {
    // A private (`pub(crate)`) thin wrapper in one crate cannot be called from
    // another crate, so a raw arg-vector there is not a reachable bypass.
    let helper = fp(
        "crates/homeboy-lab-runner/src/homeboy_refresh.rs",
        "pub(crate) fn git_dirty(p: &Path) -> bool {\n    run_git_output(p, &[\"status\", \"--porcelain\"], \"x\")\n}\n",
    );
    let caller = fp(
        "crates/homeboy-agents/src/promotion.rs",
        "fn f(p: &str) {\n    let s = git_output(p, &[\"status\", \"--porcelain\"]);\n}\n",
    );
    assert!(
        detect_command_wrapper_bypass(&[&helper, &caller]).is_empty(),
        "a private helper in another crate is unreachable — not a bypass"
    );
}

#[test]
fn flags_cross_crate_call_of_a_public_helper() {
    // A `pub` helper IS reachable across a crate boundary (e.g. homeboy-core is
    // a dependency of homeboy-agents), so the raw arg-vector is a real bypass.
    let helper = fp(
        "crates/homeboy-core/src/git/primitives_query.rs",
        "pub fn head_sha(p: &Path) -> Option<String> {\n    output_optional(p, &[\"rev-parse\", \"HEAD\"])\n}\n",
    );
    let caller = fp(
        "crates/homeboy-agents/src/promotion.rs",
        "fn f(p: &str) {\n    let h = git_output(p, &[\"rev-parse\", \"HEAD\"]);\n}\n",
    );
    let findings = detect_command_wrapper_bypass(&[&helper, &caller]);
    assert_eq!(findings.len(), 1, "public cross-crate helper is reachable");
    assert!(findings[0].description.contains("head_sha"));
}

#[test]
fn flags_same_crate_call_of_a_private_helper() {
    // Within the SAME crate, a private helper is reachable, so a raw arg-vector
    // duplicating it is still a bypass.
    let helper = fp(
        "crates/homeboy-agents/src/git_util.rs",
        "fn git_dirty(p: &Path) -> bool {\n    run_git_output(p, &[\"status\", \"--porcelain\"], \"x\")\n}\n",
    );
    let caller = fp(
        "crates/homeboy-agents/src/promotion.rs",
        "fn f(p: &str) {\n    let s = git_output(p, &[\"status\", \"--porcelain\"]);\n}\n",
    );
    let findings = detect_command_wrapper_bypass(&[&helper, &caller]);
    assert_eq!(
        findings.len(),
        1,
        "same-crate private helper is reachable — still a bypass"
    );
}
