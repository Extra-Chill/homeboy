//! Corpus invariants for the audit engine.
//!
//! Wired into `engine.rs` via `#[cfg(test)] #[path = ...] mod engine_corpus_test`.
//!
//! WHY THIS EXISTS (#10557): the post-merge `full-audit-gate` invoked the raw
//! `homeboy` binary instead of routing through `homeboy-action`. That skipped
//! the action's `Install extension` step, so `extension_provided_file_extensions()`
//! returned nothing, `CodebaseSnapshot` matched nothing, and the audit reported:
//!
//! ```json
//! { "files_scanned": 0, "files_skipped": 1817, "findings": [], "passed": true }
//! ```
//!
//! Green in 7 seconds on a repository with 1800 source files. The workflow test
//! that was supposed to guard the gate asserted the COMMAND STRING
//! (`review audit homeboy --profile=full`) and the absence of
//! `continue-on-error`. Both held. Neither could see that the command scanned
//! nothing.
//!
//! An instrument that reports success while measuring nothing is worse than no
//! instrument, so the invariant is enforced here — in the engine, for every
//! consumer of `review audit` — rather than in one workflow's YAML:
//!
//!   **an audit that found no files to audit has not passed; it has not run.**
//!
//! These tests assert the EFFECT (a corpus existed, or the run failed loudly),
//! which is the thing the old workflow test could not express.

use std::fs;

/// The invariant, stated so it holds in BOTH environments — with a
/// fingerprinting extension installed and without one.
///
/// * extension available  -> `Ok` with a non-empty corpus.
/// * no extension         -> `Err` naming the empty corpus.
///
/// What must never happen is the third outcome that shipped: `Ok` with
/// `files_scanned == 0` on a tree that demonstrably contains source files.
#[test]
fn audit_never_reports_success_with_an_empty_corpus() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    fs::write(
        root.join("alpha.rs"),
        "pub fn run() {}\npub fn helper() {}\n",
    )
    .unwrap();
    fs::write(
        root.join("beta.rs"),
        "pub fn run() {}\npub fn helper() {}\n",
    )
    .unwrap();

    match crate::audit_path_with_id("empty-corpus-guard", &root.to_string_lossy()) {
        Ok(result) => assert!(
            result.summary.files_scanned > 0,
            "audit returned success having scanned {} of 2 source files. \
             An audit with an empty corpus has not passed — it has not run (#10557). \
             summary = {:#?}",
            result.summary.files_scanned,
            result.summary
        ),
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains("audit scanned 0 files"),
                "an empty corpus must fail with the corpus diagnostic, not an unrelated error: {message}"
            );
        }
    }
}

/// The corpus error must be actionable: it has to say what went wrong AND how a
/// CI job gets a corpus back. The whole defect was a gate whose failure mode was
/// invisible; a diagnostic that does not name the cause reproduces it.
#[test]
fn empty_corpus_error_names_the_missing_extension_and_the_fix() {
    let dir = tempfile::tempdir().expect("temp dir");
    let root = dir.path();
    fs::write(root.join("only.rs"), "pub fn run() {}\n").unwrap();

    let Err(error) = crate::audit_path_with_id("empty-corpus-message", &root.to_string_lossy())
    else {
        // A fingerprinting extension is installed, so there is no empty corpus
        // to diagnose here. The invariant itself is covered above.
        return;
    };

    let message = error.to_string();
    for expected in [
        "audit scanned 0 files",
        "provides.file_extensions",
        "homeboy-action",
    ] {
        assert!(
            message.contains(expected),
            "corpus diagnostic must mention `{expected}`: {message}"
        );
    }
}

/// A tree with genuinely nothing to audit is a real zero, not a broken
/// instrument. Docs-only components must keep passing.
#[test]
fn a_tree_with_no_source_files_is_still_a_clean_pass() {
    let dir = tempfile::tempdir().expect("temp dir");
    fs::write(dir.path().join("README.md"), "# docs only\n").unwrap();

    let result = crate::audit_path_with_id("docs-only", &dir.path().to_string_lossy())
        .expect("a component with no source files audits cleanly");

    assert_eq!(result.summary.files_scanned, 0);
    assert!(result.findings.is_empty());
}

/// #10558: the two convention-discovery filters must not apply to the
/// source-policy corpus.
///
/// `is_extension_provided_file` is the honest "an extension claims this file
/// type" predicate; index-file exclusion is a separate, convention-only
/// decision. Keeping them separate is what stops `mod.rs`/`lib.rs`/`main.rs`
/// from silently leaving the policy corpus again.
#[test]
fn index_files_are_extension_provided_even_though_conventions_skip_them() {
    use std::path::Path;

    let extensions = vec!["rs".to_string()];

    for index in ["src/mod.rs", "src/lib.rs", "src/main.rs"] {
        let path = Path::new(index);
        assert!(
            crate::walker::is_extension_provided_file(path, &extensions),
            "{index} is a claimed source file and must be reachable by source policies (#10558)"
        );
        assert!(
            crate::walker::is_index_file(path),
            "{index} must stay excluded from convention sibling detection"
        );
    }

    let peer = Path::new("src/thing.rs");
    assert!(crate::walker::is_extension_provided_file(peer, &extensions));
    assert!(!crate::walker::is_index_file(peer));

    // Unclaimed file types are still out of scope for both.
    assert!(!crate::walker::is_extension_provided_file(
        Path::new("src/thing.py"),
        &extensions
    ));
}
