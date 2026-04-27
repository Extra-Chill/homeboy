use serde::Serialize;

use crate::engine::local_files;
use crate::engine::output_parse::{Aggregate, DeriveRule, ParseRule, ParseSpec};
use crate::extension::test::analyze::{TestAnalysis, TestAnalysisInput};
use crate::extension::test::TestCounts;

#[derive(Debug, Clone, Serialize)]
pub struct CoverageOutput {
    pub lines_pct: f64,
    pub lines_total: u64,
    pub lines_covered: u64,
    pub methods_pct: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered_files: Vec<UncoveredFile>,
}

#[derive(Debug, Clone, Serialize)]
pub struct UncoveredFile {
    pub file: String,
    pub line_pct: f64,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestFailureSummaryItem {
    pub test_name: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct TestSummaryOutput {
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub skipped: u64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub failures: Vec<TestFailureSummaryItem>,
    pub exit_code: i32,
}

pub fn build_test_summary(
    test_counts: Option<&TestCounts>,
    analysis: Option<&TestAnalysis>,
    exit_code: i32,
) -> TestSummaryOutput {
    let (total, passed, failed, skipped) = if let Some(counts) = test_counts {
        (counts.total, counts.passed, counts.failed, counts.skipped)
    } else {
        let total = analysis.map(|analysis| analysis.total_tests).unwrap_or(0);
        let passed = analysis.map(|analysis| analysis.total_passed).unwrap_or(0);
        let failed = analysis
            .map(|analysis| analysis.total_failures as u64)
            .unwrap_or(0);
        let skipped = total.saturating_sub(passed + failed);
        (total, passed, failed, skipped)
    };

    let failures = analysis
        .map(|analysis| {
            analysis
                .clusters
                .iter()
                .flat_map(|cluster| {
                    cluster
                        .example_tests
                        .iter()
                        .map(|name| TestFailureSummaryItem {
                            test_name: name.clone(),
                            message: cluster.pattern.clone(),
                            file: cluster.affected_files.first().cloned(),
                            line: None,
                        })
                })
                .take(20)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    TestSummaryOutput {
        total,
        passed,
        failed,
        skipped,
        failures,
        exit_code,
    }
}

pub fn parse_failures_file(path: &std::path::Path) -> Option<TestAnalysisInput> {
    let content = local_files::read_file(path, "read test failures file").ok()?;
    let mut parsed: TestAnalysisInput = serde_json::from_str(&content).ok()?;

    if parsed.total == 0 && !parsed.failures.is_empty() {
        parsed.total = parsed.failures.len() as u64;
    }

    if parsed.passed > parsed.total {
        parsed.passed = parsed.total;
    }

    Some(parsed)
}

pub fn parse_test_results_file(path: &std::path::Path) -> Option<TestCounts> {
    let content = local_files::read_file(path, "read test results file").ok()?;
    let data: serde_json::Value = serde_json::from_str(&content).ok()?;

    let total = data
        .get("total")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let passed = data
        .get("passed")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let failed = data
        .get("failed")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let skipped = data
        .get("skipped")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);

    Some(TestCounts::new(total, passed, failed, skipped))
}

pub fn parse_test_results_text(text: &str) -> Option<TestCounts> {
    parse_test_results_text_with_spec(text, &default_test_result_parse_spec())
}

pub fn parse_test_results_text_with_spec(text: &str, spec: &ParseSpec) -> Option<TestCounts> {
    let parsed = spec.parse(text);
    let total = parsed.get("total").copied().unwrap_or(0.0).max(0.0) as u64;
    if total == 0 {
        return None;
    }
    let passed = parsed.get("passed").copied().unwrap_or(0.0).max(0.0) as u64;
    let failed = parsed.get("failed").copied().unwrap_or(0.0).max(0.0) as u64;
    let skipped = parsed.get("skipped").copied().unwrap_or(0.0).max(0.0) as u64;
    Some(TestCounts::new(total, passed, failed, skipped))
}

fn default_test_result_parse_spec() -> ParseSpec {
    ParseSpec {
        rules: vec![
            ParseRule {
                pattern: r"Tests:\s*(\d+)".to_string(),
                field: "total".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
            ParseRule {
                pattern: r"Failures:\s*(\d+)".to_string(),
                field: "failed".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
            ParseRule {
                pattern: r"Errors:\s*(\d+)".to_string(),
                field: "errors".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
            ParseRule {
                pattern: r"Skipped:\s*(\d+)".to_string(),
                field: "skipped".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
            ParseRule {
                pattern: r"OK\s*\((\d+) tests".to_string(),
                field: "total".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
        ],
        defaults: std::collections::HashMap::from([
            ("failed".to_string(), 0.0),
            ("errors".to_string(), 0.0),
            ("skipped".to_string(), 0.0),
        ]),
        derive: vec![DeriveRule {
            field: "passed".to_string(),
            expr: "total - failed - errors - skipped".to_string(),
        }],
    }
}

pub fn parse_coverage_file(path: &std::path::Path) -> std::result::Result<CoverageOutput, ()> {
    let content = local_files::read_file(path, "read coverage file").map_err(|_| ())?;
    let data: serde_json::Value = serde_json::from_str(&content).map_err(|_| ())?;

    let totals = data.get("totals").ok_or(())?;
    let lines = totals.get("lines").ok_or(())?;
    let methods = totals.get("methods").ok_or(())?;

    let lines_pct = lines
        .get("pct")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);
    let lines_total = lines
        .get("total")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let lines_covered = lines
        .get("covered")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let methods_pct = methods
        .get("pct")
        .and_then(|value| value.as_f64())
        .unwrap_or(0.0);

    let uncovered_files = data
        .get("files")
        .and_then(|files| files.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|file| {
                    let pct = file.get("line_pct").and_then(|value| value.as_f64())?;
                    if pct < 50.0 {
                        Some(UncoveredFile {
                            file: file
                                .get("file")
                                .and_then(|value| value.as_str())
                                .unwrap_or("?")
                                .to_string(),
                            line_pct: pct,
                        })
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(CoverageOutput {
        lines_pct,
        lines_total,
        lines_covered,
        methods_pct,
        uncovered_files,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manifest_parse_spec_can_sum_cargo_result_lines() {
        let spec: ParseSpec = serde_json::from_value(serde_json::json!({
            "rules": [
                { "pattern": "test result:.*?(\\d+) passed", "field": "passed", "aggregate": "sum" },
                { "pattern": "test result:.*?(\\d+) failed", "field": "failed", "aggregate": "sum" },
                { "pattern": "test result:.*?(\\d+) ignored", "field": "skipped", "aggregate": "sum" }
            ],
            "defaults": { "passed": 0, "failed": 0, "skipped": 0 },
            "derive": [
                { "field": "total", "expr": "passed + failed + skipped" }
            ]
        }))
        .expect("parse spec should deserialize from manifest-shaped JSON");

        let counts = parse_test_results_text_with_spec(
            "test result: ok. 2 passed; 0 failed; 1 ignored\n\
             test result: FAILED. 3 passed; 1 failed; 0 ignored",
            &spec,
        )
        .expect("cargo-style output should parse");

        assert_eq!(counts.total, 7);
        assert_eq!(counts.passed, 5);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.skipped, 1);
    }

    #[test]
    fn default_parse_spec_preserves_phpunit_summary_fallback() {
        let counts = parse_test_results_text("Tests: 12, Assertions: 20, Failures: 2, Skipped: 3")
            .expect("default PHPUnit-ish fallback should still parse");

        assert_eq!(counts.total, 12);
        assert_eq!(counts.passed, 7);
        assert_eq!(counts.failed, 2);
        assert_eq!(counts.skipped, 3);
    }
}
