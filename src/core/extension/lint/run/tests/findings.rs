use super::super::exit_code::normalize_empty_finding_exit_code;
use super::super::findings::{
    build_lint_producer_summaries, build_lint_summary, filter_findings_to_scoped_files,
    filter_lint_findings, mark_zero_finding_producers_passed, parse_lint_producer_summaries_file,
};
use super::super::types::ScopedLintRun;
use super::{lint_args, lint_finding};
use crate::core::engine::run_dir;
use crate::core::finding::HomeboyFinding;
use std::path::Path;

#[test]
fn lint_summary_counts_categories_and_caps_top_findings() {
    let findings = (0..25)
        .map(|index| {
            let category = if index % 2 == 0 {
                "style"
            } else {
                "correctness"
            };
            HomeboyFinding::builder("lint", "message")
                .category(category)
                .fingerprint(format!("src/file-{index}.rs::rule"))
                .build()
        })
        .collect::<Vec<_>>();

    let producers = build_lint_producer_summaries(
        &findings,
        Path::new("lint-findings.json"),
        Path::new("lint-producers.json"),
        Vec::new(),
        false,
        1,
        None,
    );
    let summary = build_lint_summary(&findings, &producers, 1);

    assert_eq!(summary.total_findings, 25);
    assert_eq!(summary.categories.get("style"), Some(&13));
    assert_eq!(summary.categories.get("correctness"), Some(&12));
    assert_eq!(summary.top_findings.len(), 20);
    assert_eq!(summary.producer_summaries[0].finding_count, 25);
    assert_eq!(summary.exit_code, 1);
}

#[test]
fn producer_summary_sidecar_represents_zero_finding_tools() {
    let dir = tempfile::tempdir().expect("temp dir");
    let producers_file = dir.path().join(run_dir::files::LINT_PRODUCERS);
    std::fs::write(
        &producers_file,
        r#"[
                {"tool":"phpcs","status":"passed","finding_count":0,"step":"phpcs"},
                {"tool":"phpstan","status":"passed","finding_count":0,"step":"phpstan"}
            ]"#,
    )
    .expect("producer summaries should be written");

    let declared = parse_lint_producer_summaries_file(&producers_file).expect("producers parse");
    let summaries = build_lint_producer_summaries(
        &[],
        &dir.path().join(run_dir::files::LINT_FINDINGS),
        &producers_file,
        declared,
        true,
        0,
        None,
    );

    assert_eq!(summaries.len(), 2);
    assert_eq!(summaries[0].tool, "phpcs");
    assert_eq!(summaries[0].finding_count, 0);
    assert_eq!(summaries[0].status, "passed");
    let producers_path = producers_file.to_string_lossy().to_string();
    assert_eq!(
        summaries[0].source.as_ref().unwrap().path.as_deref(),
        Some(producers_path.as_str())
    );
}

#[test]
fn filtered_scoped_findings_normalize_synthetic_zero_finding_producer() {
    let mut summaries = build_lint_producer_summaries(
        &[],
        Path::new("lint-findings.json"),
        Path::new("lint-producers.json"),
        Vec::new(),
        false,
        1,
        None,
    );

    assert_eq!(summaries[0].status, "error");

    mark_zero_finding_producers_passed(&mut summaries);

    assert_eq!(summaries[0].finding_count, 0);
    assert_eq!(summaries[0].status, "passed");
    assert_eq!(
        normalize_empty_finding_exit_code(1, false, &[], &summaries),
        0
    );
}

#[test]
fn filter_lint_findings_keeps_requested_category_only() {
    let mut args = lint_args();
    args.category = Some("security".to_string());
    let findings = vec![
        lint_finding(
            "a",
            "security",
            "WordPress.Security.ValidatedSanitizedInput",
        ),
        lint_finding("b", "database", "WordPress.DB.PreparedSQL"),
        lint_finding("c", "eslint", "react-hooks/rules-of-hooks"),
    ];

    let filtered = filter_lint_findings(findings, &args);

    assert_eq!(filtered.len(), 1);
    assert_eq!(filtered[0].fingerprint.as_deref(), Some("a"));
}

#[test]
fn filter_lint_findings_honors_include_and_exclude_sniffs() {
    let mut args = lint_args();
    args.sniff_filters.sniffs = Some(
        "WordPress.Security.ValidatedSanitizedInput,Generic.WhiteSpace.ScopeIndent".to_string(),
    );
    args.sniff_filters.exclude_sniffs = Some("Generic.WhiteSpace.ScopeIndent".to_string());
    let findings = vec![
        lint_finding(
            "inc/a.php::WordPress.Security.ValidatedSanitizedInput",
            "security",
            "WordPress.Security.ValidatedSanitizedInput",
        ),
        lint_finding(
            "inc/b.php::Generic.WhiteSpace.ScopeIndent",
            "whitespace",
            "Generic.WhiteSpace.ScopeIndent",
        ),
        lint_finding(
            "inc/c.php::WordPress.DB.PreparedSQL",
            "database",
            "WordPress.DB.PreparedSQL",
        ),
    ];

    let filtered = filter_lint_findings(findings, &args);

    assert_eq!(filtered.len(), 1);
    assert_eq!(
        filtered[0].fingerprint.as_deref(),
        Some("inc/a.php::WordPress.Security.ValidatedSanitizedInput")
    );
}

#[test]
fn scoped_lint_filters_findings_to_changed_files() {
    let runs = vec![ScopedLintRun {
        glob: "/repo/src/changed.rs".to_string(),
        step: None,
        changed_files: vec!["src/changed.rs".to_string()],
    }];
    let mut changed = lint_finding("changed", "correctness", "clippy");
    changed.location.file = Some("src/changed.rs".to_string());
    let mut unrelated = lint_finding("unrelated", "correctness", "clippy");
    unrelated.location.file = Some("src/unrelated.rs".to_string());

    let filtered = filter_findings_to_scoped_files(vec![changed.clone(), unrelated], Some(&runs));

    assert_eq!(filtered, vec![changed]);
}
