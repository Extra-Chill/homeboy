use serde_json::json;

use super::{build_findings_from_native_output, IssueRenderContext};
use crate::code_audit::FindingConfidence;

#[test]
fn test_build_findings_from_native_output() {
    let output = json!({"data": {"passed": true, "lint_findings": []}});
    let rendered =
        build_findings_from_native_output("lint", output, &IssueRenderContext::default()).unwrap();

    assert_eq!(rendered.command, "lint");
    assert!(rendered.groups.is_empty());
}

#[test]
fn test_merge() {
    let mut first = build_findings_from_native_output(
        "lint",
        json!({"data": {"passed": false, "lint_findings": [
            {"id": "lint-1", "category": "A", "message": "one"}
        ]}}),
        &IssueRenderContext::default(),
    )
    .unwrap();
    let second = build_findings_from_native_output(
        "lint",
        json!({"data": {"passed": false, "lint_findings": [
            {"id": "lint-2", "category": "B", "message": "two"}
        ]}}),
        &IssueRenderContext::default(),
    )
    .unwrap();

    first.merge(second);

    assert_eq!(first.command, "lint");
    assert!(first.groups.contains_key("A"));
    assert!(first.groups.contains_key("B"));
}

#[test]
fn renders_audit_output_grouped_by_kind_with_fixability() {
    let output = json!({
        "success": false,
        "data": {
            "command": "audit",
            "passed": false,
            "component_id": "homeboy",
            "source_path": "/tmp/homeboy",
            "findings": [
                {
                    "file": "src/a.rs",
                    "kind": "unreferenced_export",
                    "confidence": "structural",
                    "description": "export is unused",
                    "suggestion": "remove it"
                },
                {
                    "file": "src/b.rs",
                    "kind": "unreferenced_export",
                    "confidence": "structural",
                    "description": "export is unused",
                    "suggestion": "remove it"
                },
                {
                    "file": "src/large.rs",
                    "kind": "god_file",
                    "confidence": "heuristic",
                    "description": "file is large",
                    "suggestion": "split it"
                }
            ],
            "fixability": {
                "by_kind": {
                    "unreferenced_export": {
                        "total": 2,
                        "automated": 1,
                        "manual_only": 1
                    }
                }
            }
        }
    });
    let context = IssueRenderContext {
        run_url: Some("https://github.com/Extra-Chill/homeboy/actions/runs/1".to_string()),
    };

    let rendered = build_findings_from_native_output("audit", output, &context).unwrap();

    assert_eq!(rendered.command, "audit");
    assert_eq!(rendered.groups.len(), 2);
    let group = rendered.groups.get("unreferenced_export").unwrap();
    assert_eq!(group.count, 2);
    assert_eq!(group.label, "unreferenced export");
    assert_eq!(group.confidence, Some(FindingConfidence::Structural));
    assert!(group.body.contains("## unreferenced export"));
    assert!(group
        .body
        .contains("Run: https://github.com/Extra-Chill/homeboy/actions/runs/1"));
    assert!(group.body.contains("### Autofix status"));
    assert!(group.body.contains("- Automated: 1"));
    assert!(group.body.contains("- `src/a.rs` — export is unused"));
}

#[test]
fn renders_lint_output_grouped_by_category() {
    let output = json!({
        "data": {
            "passed": false,
            "status": "failed",
            "exit_code": 1,
            "lint_findings": [
                {"id": "lint-1", "category": "Squiz.Commenting.FunctionComment.Missing", "message": "missing docblock"},
                {"id": "lint-2", "category": "Squiz.Commenting.FunctionComment.Missing", "message": "missing docblock"},
                {"id": "lint-3", "category": "Generic.Files.LineLength.TooLong", "message": "line too long"}
            ]
        }
    });

    let rendered =
        build_findings_from_native_output("lint", output, &IssueRenderContext::default()).unwrap();

    assert_eq!(rendered.command, "lint");
    assert_eq!(rendered.groups.len(), 2);
    let group = rendered
        .groups
        .get("Squiz.Commenting.FunctionComment.Missing")
        .unwrap();
    assert_eq!(group.count, 2);
    assert!(group.body.contains("2 lint finding(s) in this category."));
    assert!(group.body.contains("- `lint-1` — missing docblock"));
}

#[test]
fn renders_lint_aggregate_fallback_when_findings_are_missing() {
    let output = json!({
        "data": {
            "passed": false,
            "status": "failed",
            "exit_code": 2
        }
    });

    let rendered =
        build_findings_from_native_output("lint", output, &IssueRenderContext::default()).unwrap();

    let group = rendered.groups.get("lint_failure").unwrap();
    assert_eq!(group.count, 1);
    assert!(group
        .body
        .contains("Lint failed without structured findings (exit 2)."));
}

#[test]
fn renders_test_analysis_clusters_by_category() {
    let output = json!({
        "data": {
            "passed": false,
            "status": "failed",
            "exit_code": 1,
            "analysis": {
                "clusters": [
                    {
                        "category": "missing_method",
                        "count": 3,
                        "pattern": "undefined method Widget::render",
                        "affected_files": ["tests/widget.rs"],
                        "example_tests": ["widget_renders", "widget_renders_nested"],
                        "suggested_fix": "Add the missing method"
                    }
                ]
            }
        }
    });

    let rendered =
        build_findings_from_native_output("test", output, &IssueRenderContext::default()).unwrap();

    let group = rendered.groups.get("missing_method").unwrap();
    assert_eq!(group.count, 3);
    assert_eq!(group.label, "missing method");
    assert!(group.body.contains("3 test failure(s) in this cluster."));
    assert!(group
        .body
        .contains("**Pattern:** undefined method Widget::render"));
    assert!(group.body.contains("- `tests/widget.rs`"));
    assert!(group.body.contains("- `widget_renders`"));
}

#[test]
fn renders_test_aggregate_fallback_from_counts() {
    let output = json!({
        "data": {
            "passed": false,
            "status": "failed",
            "exit_code": 1,
            "test_counts": {
                "total": 12,
                "passed": 10,
                "failed": 2,
                "skipped": 0
            }
        }
    });

    let rendered =
        build_findings_from_native_output("test", output, &IssueRenderContext::default()).unwrap();

    let group = rendered.groups.get("test_failure").unwrap();
    assert_eq!(group.count, 2);
    assert!(group.body.contains("2 failed test(s) out of 12 total."));
}

#[test]
fn passing_outputs_produce_no_groups() {
    let lint = json!({"data": {"passed": true, "lint_findings": []}});
    let test = json!({"data": {"passed": true, "test_counts": {"total": 1, "passed": 1, "failed": 0, "skipped": 0}}});

    assert!(
        build_findings_from_native_output("lint", lint, &IssueRenderContext::default())
            .unwrap()
            .groups
            .is_empty()
    );
    assert!(
        build_findings_from_native_output("test", test, &IssueRenderContext::default())
            .unwrap()
            .groups
            .is_empty()
    );
}
