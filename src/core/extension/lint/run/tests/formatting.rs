use super::super::formatting::{extract_formatting_findings, self_check_output_is_harness_failure};

#[test]
fn formatting_findings_parse_diff_paths_and_summary() {
    let dir = tempfile::tempdir().expect("temp dir");
    let stdout = format!(
        "FMT SUMMARY: 2 files need formatting\nDiff in {} at line 12:\nDiff in src/lib.rs:1:\n",
        dir.path().join("src/main.rs").display()
    );

    let findings = extract_formatting_findings(&stdout, "", dir.path())
        .expect("formatting findings should parse");

    assert_eq!(findings.files, vec!["src/lib.rs", "src/main.rs"]);
    assert_eq!(
        findings.summary.as_deref(),
        Some("FMT SUMMARY: 2 files need formatting")
    );
    assert_eq!(findings.suggested_command, "cargo fmt");
}

#[test]
fn self_check_output_is_harness_failure_detects_missing_runner_steps() {
    // The runner-steps.sh missing-harness environmental issue (#4586).
    assert!(self_check_output_is_harness_failure(
        1,
        "",
        "sh: runner-steps.sh: No such file or directory"
    ));
}

#[test]
fn self_check_output_is_harness_failure_detects_high_exit_code() {
    // Exit codes >= 2 conventionally indicate tooling/internal errors.
    assert!(self_check_output_is_harness_failure(2, "boom", ""));
    assert!(self_check_output_is_harness_failure(7, "", ""));
    assert!(self_check_output_is_harness_failure(127, "", ""));
}

#[test]
fn self_check_output_is_harness_failure_detects_generic_infra_marker() {
    // Core only recognizes ecosystem-agnostic infra markers at exit 1.
    // Ecosystem-specific signatures are detected by the owning extension.
    assert!(self_check_output_is_harness_failure(
        1,
        "lint runner infrastructure failure",
        ""
    ));
}

#[test]
fn self_check_output_is_harness_failure_rejects_plain_lint_failure() {
    // Exit 1 with no infra markers is a genuine lint failure, not a harness
    // failure — it must hard-block the release.
    assert!(!self_check_output_is_harness_failure(
        1,
        "FOUND 2 ERRORS AFFECTING 2 LINES",
        ""
    ));
}

#[test]
fn self_check_output_is_harness_failure_accepts_clean_pass() {
    assert!(!self_check_output_is_harness_failure(0, "", ""));
}
