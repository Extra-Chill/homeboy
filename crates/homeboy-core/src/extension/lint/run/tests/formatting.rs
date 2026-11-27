use super::super::formatting::{extract_formatting_findings, self_check_output_is_harness_failure};

#[test]
fn formatting_findings_use_distinct_declared_format_files() {
    let dir = tempfile::tempdir().unwrap();
    let finding = |category, file: String| {
        homeboy_finding::HomeboyFinding::builder("fixture", "message")
            .category(category)
            .file(file)
            .build()
    };
    let findings = extract_formatting_findings(
        &[
            finding(
                "format",
                dir.path().join("src/main.js").to_string_lossy().into(),
            ),
            finding("format", "./src/main.js".into()),
            finding("lint", "other.js".into()),
        ],
        dir.path(),
    )
    .unwrap();
    assert_eq!(findings.files, vec!["src/main.js"]);
    assert!(extract_formatting_findings(&[], dir.path()).is_none());
}

#[test]
fn self_check_output_is_harness_failure_classifies_exit_and_markers() {
    // (exit, stdout, stderr, is_harness_failure)
    let cases = [
        // Missing runner-steps.sh harness (#4586).
        (
            1,
            "",
            "sh: runner-steps.sh: No such file or directory",
            true,
        ),
        // Exit codes >= 2 indicate tooling/internal errors.
        (2, "boom", "", true),
        (127, "", "", true),
        // Ecosystem-agnostic infra marker at exit 1.
        (1, "lint runner infrastructure failure", "", true),
        // Genuine lint failure must hard-block the release.
        (1, "FOUND 2 ERRORS AFFECTING 2 LINES", "", false),
        (0, "", "", false),
    ];
    for (exit, stdout, stderr, expected) in cases {
        assert_eq!(
            self_check_output_is_harness_failure(exit, stdout, stderr),
            expected,
            "exit={exit} stdout={stdout:?} stderr={stderr:?}"
        );
    }
}
