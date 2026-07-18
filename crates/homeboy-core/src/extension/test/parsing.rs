use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::extension::test::analyze::{TestAnalysis, TestAnalysisInput, TestFailure};
use crate::extension::test::TestCounts;
use crate::structured_sidecar;
use homeboy_engine_primitives::local_files;
use homeboy_engine_primitives::output_parse::{Aggregate, DeriveRule, ParseRule, ParseSpec};
pub use homeboy_extension_contract::test_parsing::{
    CoverageOutput, TestFailureSummaryItem, TestSummaryOutput, UncoveredFile,
};

#[derive(Debug, Deserialize)]
struct RawTestFailure {
    #[serde(alias = "test_id", default)]
    test_name: Option<String>,
    #[serde(alias = "file", default)]
    test_file: Option<String>,
    #[serde(alias = "failure_type", default)]
    error_type: Option<String>,
    #[serde(default)]
    message: Option<String>,
    #[serde(alias = "source", default)]
    source_file: Option<String>,
    #[serde(alias = "source_line", alias = "line", default)]
    source_line: Option<u32>,
}

impl From<RawTestFailure> for TestFailure {
    fn from(raw: RawTestFailure) -> Self {
        Self {
            test_name: raw.test_name.unwrap_or_else(|| "unknown test".to_string()),
            test_file: raw.test_file.unwrap_or_default(),
            error_type: raw.error_type.unwrap_or_default(),
            message: raw
                .message
                .unwrap_or_else(|| "test failure (no message provided)".to_string()),
            source_file: raw.source_file.unwrap_or_default(),
            source_line: raw.source_line.unwrap_or_default(),
        }
    }
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

pub fn parse_failures_file(path: &std::path::Path) -> Result<Option<TestAnalysisInput>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = local_files::read_file(path, "read test failures file")?;
    let payload: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        crate::Error::internal_json(
            format!("Malformed test failures JSON in {}: {}", path.display(), e),
            Some("test.failures.parse".to_string()),
        )
    })?;
    let mut parsed = parse_failures_payload(payload, path)?;

    if parsed.total == 0 && !parsed.failures.is_empty() {
        parsed.total = parsed.failures.len() as u64;
    }

    if parsed.passed > parsed.total {
        parsed.passed = parsed.total;
    }

    Ok(Some(parsed))
}

fn parse_failures_payload(
    payload: serde_json::Value,
    path: &std::path::Path,
) -> Result<TestAnalysisInput> {
    match payload {
        serde_json::Value::Array(items) => {
            let payload = serde_json::Value::Array(items);
            structured_sidecar::validate_payload("test.failures", &payload)?;
            Ok(TestAnalysisInput {
                failures: parse_failure_items(payload, path)?,
                total: 0,
                passed: 0,
            })
        }
        serde_json::Value::Object(mut object) => {
            let failures = object
                .remove("failures")
                .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
            let failures = match failures {
                serde_json::Value::Array(items) => {
                    let payload = serde_json::Value::Array(items);
                    structured_sidecar::validate_payload("test.failures", &payload)?;
                    parse_failure_items(payload, path)?
                }
                serde_json::Value::Null => Vec::new(),
                other => {
                    return Err(crate::Error::internal_json(
                        format!(
                            "Malformed test failures JSON in {}: failures must be an array, got {}",
                            path.display(),
                            json_type_name(&other)
                        ),
                        Some("test.failures.parse".to_string()),
                    ));
                }
            };
            let total = object
                .get("total")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);
            let passed = object
                .get("passed")
                .and_then(|value| value.as_u64())
                .unwrap_or(0);

            Ok(TestAnalysisInput {
                failures,
                total,
                passed,
            })
        }
        other => Err(crate::Error::internal_json(
            format!(
                "Malformed test failures JSON in {}: expected array or object, got {}",
                path.display(),
                json_type_name(&other)
            ),
            Some("test.failures.parse".to_string()),
        )),
    }
}

fn parse_failure_items(
    payload: serde_json::Value,
    path: &std::path::Path,
) -> Result<Vec<TestFailure>> {
    serde_json::from_value::<Vec<RawTestFailure>>(payload)
        .map(|items| items.into_iter().map(TestFailure::from).collect())
        .map_err(|e| {
            crate::Error::internal_json(
                format!("Malformed test failures JSON in {}: {}", path.display(), e),
                Some("test.failures.parse".to_string()),
            )
        })
}

fn json_type_name(value: &serde_json::Value) -> &'static str {
    match value {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "boolean",
        serde_json::Value::Number(_) => "number",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

pub fn parse_test_results_file(path: &std::path::Path) -> Result<Option<TestCounts>> {
    parse_test_results_file_with_spec(path, None)
}

pub fn parse_test_results_file_with_spec(
    path: &std::path::Path,
    _spec: Option<&ParseSpec>,
) -> Result<Option<TestCounts>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = local_files::read_file(path, "read test results file")?;
    let data: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        crate::Error::internal_json(
            format!("Malformed test results JSON in {}: {}", path.display(), e),
            Some("test.results.parse".to_string()),
        )
    })?;
    structured_sidecar::validate_payload("test.results", &data)?;

    let has_flat_counts = ["total", "passed", "failed", "errors", "skipped"]
        .iter()
        .any(|key| data.get(key).is_some());
    if !has_flat_counts {
        return Ok(None);
    }

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
    let errors = data
        .get("errors")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);
    let skipped = data
        .get("skipped")
        .and_then(|value| value.as_u64())
        .unwrap_or(0);

    // `TestCounts` has no separate errors field; fold runner errors into
    // `failed` so status/baseline decisions do not treat errors as passing.
    Ok(Some(TestCounts::new(
        total,
        passed,
        failed + errors,
        skipped,
    )))
}

pub fn parse_test_results_text(text: &str) -> Option<TestCounts> {
    parse_test_results_text_with_spec(text, &default_test_result_parse_spec())
}

pub fn parse_test_results_text_with_spec(text: &str, spec: &ParseSpec) -> Option<TestCounts> {
    for adapter in &spec.adapters {
        if let Some(counts) = parse_test_results_text_with_adapter(text, adapter) {
            return Some(counts);
        }
    }

    let parsed = spec.parse(text);
    let total = parsed.get("total").copied().unwrap_or(0.0).max(0.0) as u64;
    let passed = parsed.get("passed").copied().unwrap_or(0.0).max(0.0) as u64;
    let failed = parsed.get("failed").copied().unwrap_or(0.0).max(0.0) as u64;
    let errors = parsed.get("errors").copied().unwrap_or(0.0).max(0.0) as u64;
    let skipped = parsed.get("skipped").copied().unwrap_or(0.0).max(0.0) as u64;
    // `TestCounts` has no separate errors field; fold runner errors into
    // `failed` so status/baseline decisions do not treat errors as passing.
    if total > 0 {
        return Some(TestCounts::new(total, passed, failed + errors, skipped));
    }

    parse_key_value_test_summary(text)
}

/// Parse terminal summaries emitted by runners that report aggregate counts as
/// `passed=<n> failed=<n>` rather than a framework-specific result line.
fn parse_key_value_test_summary(text: &str) -> Option<TestCounts> {
    let summary =
        regex::Regex::new(r"(?m)^\S.*?\bpassed=(\d+)\s+failed=(\d+)(?:\s+skipped=(\d+))?\s*$")
            .expect("key-value test summary regex is valid");
    let captures = summary.captures_iter(text).last()?;
    let passed = captures.get(1)?.as_str().parse().ok()?;
    let failed = captures.get(2)?.as_str().parse().ok()?;
    let skipped = captures
        .get(3)
        .map(|capture| capture.as_str().parse())
        .transpose()
        .ok()?
        .unwrap_or(0);

    Some(TestCounts::new(
        passed + failed + skipped,
        passed,
        failed,
        skipped,
    ))
}

fn parse_test_results_text_with_adapter(text: &str, adapter: &str) -> Option<TestCounts> {
    match adapter {
        "phpunit-testdox" => parse_phpunit_testdox_text(text),
        _ => None,
    }
}

fn parse_phpunit_testdox_text(text: &str) -> Option<TestCounts> {
    let passed = text.lines().filter(|line| line.starts_with(" ✔")).count() as u64;
    let failed = text.lines().filter(|line| line.starts_with(" ✘")).count() as u64;

    if passed == 0 && failed == 0 {
        return None;
    }

    Some(TestCounts::new(passed + failed, passed, failed, 0))
}

fn default_test_result_parse_spec() -> ParseSpec {
    ParseSpec {
        extension_script: None,
        adapters: vec!["phpunit-testdox".to_string()],
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
                field: "skipped_raw".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
            ParseRule {
                pattern: r"Incomplete:\s*(\d+)".to_string(),
                field: "incomplete".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
            ParseRule {
                pattern: r"Risky:\s*(\d+)".to_string(),
                field: "risky".to_string(),
                group: 1,
                aggregate: Aggregate::Last,
            },
            ParseRule {
                pattern: r"Warnings:\s*(\d+)".to_string(),
                field: "warnings".to_string(),
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
            ("skipped_raw".to_string(), 0.0),
            ("incomplete".to_string(), 0.0),
            ("risky".to_string(), 0.0),
            ("warnings".to_string(), 0.0),
        ]),
        derive: vec![
            DeriveRule {
                field: "skipped".to_string(),
                expr: "skipped_raw + incomplete + risky + warnings".to_string(),
            },
            DeriveRule {
                field: "passed".to_string(),
                expr: "total - failed - errors - skipped".to_string(),
            },
        ],
    }
}

pub fn parse_coverage_file(path: &std::path::Path) -> Result<Option<CoverageOutput>> {
    if !path.exists() {
        return Ok(None);
    }

    let content = local_files::read_file(path, "read coverage file")?;
    let data: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
        crate::Error::internal_json(
            format!("Malformed test coverage JSON in {}: {}", path.display(), e),
            Some("test.coverage.parse".to_string()),
        )
    })?;

    let Some(totals) = data.get("totals") else {
        return Ok(None);
    };
    let Some(lines) = totals.get("lines") else {
        return Ok(None);
    };
    let Some(methods) = totals.get("methods") else {
        return Ok(None);
    };

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

    Ok(Some(CoverageOutput {
        lines_pct,
        lines_total,
        lines_covered,
        methods_pct,
        uncovered_files,
    }))
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

    #[test]
    fn default_parse_spec_folds_errors_into_failed_count() {
        let counts = parse_test_results_text(
            "Tests: 12, Assertions: 20, Failures: 2, Errors: 1, Skipped: 3",
        )
        .expect("default PHPUnit-ish fallback should parse errors");

        assert_eq!(counts.total, 12);
        assert_eq!(counts.passed, 6);
        assert_eq!(counts.failed, 3);
        assert_eq!(counts.skipped, 3);
    }

    #[test]
    fn default_parse_spec_counts_phpunit_non_passing_summary_buckets_as_skipped() {
        let counts = parse_test_results_text(
            "Tests: 20, Assertions: 40, Errors: 1, Failures: 2, Warnings: 3, Skipped: 4, Incomplete: 5, Risky: 1",
        )
        .expect("default PHPUnit-ish fallback should parse all summary buckets");

        assert_eq!(counts.total, 20);
        assert_eq!(counts.passed, 4);
        assert_eq!(counts.failed, 3);
        assert_eq!(counts.skipped, 13);
    }

    #[test]
    fn default_parse_spec_reads_terminal_key_value_summary() {
        let counts = parse_test_results_text(
            "HOST_SMOKE_SUMMARY:passed=21 failed=15\n\
             Real-WordPress smoke tests failed (15 of 36):\n\
               - tests/wiki/upsert-delegation-smoke.php (exit 1)",
        )
        .expect("key-value terminal summary should parse");

        assert_eq!(counts.total, 36);
        assert_eq!(counts.passed, 21);
        assert_eq!(counts.failed, 15);
        assert_eq!(counts.skipped, 0);
    }

    #[test]
    fn core_adapter_parses_phpunit_testdox_fallback() {
        let spec: ParseSpec = serde_json::from_value(serde_json::json!({
            "adapters": ["phpunit-testdox"]
        }))
        .expect("parse spec should deserialize adapters");

        let counts = parse_test_results_text_with_spec(
            "Example Test\n ✔ passes one assertion\n ✘ fails one assertion\n ✔ passes another assertion",
            &spec,
        )
        .expect("TestDox adapter should parse glyph output");

        assert_eq!(counts.total, 3);
        assert_eq!(counts.passed, 2);
        assert_eq!(counts.failed, 1);
        assert_eq!(counts.skipped, 0);
    }

    #[test]
    fn result_file_parser_reads_flat_count_json() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let results_file = temp_dir.path().join("test-results.json");
        let payload = r#"{
            "total": 5,
            "passed": 2,
            "failed": 1,
            "skipped": 2
        }"#;
        std::fs::write(&results_file, payload).expect("write test results");

        let file_counts =
            parse_test_results_file(&results_file).expect("flat test-results JSON should parse");
        let file_counts = file_counts.expect("flat counts should be present");

        assert_eq!(file_counts.total, 5);
        assert_eq!(file_counts.passed, 2);
        assert_eq!(file_counts.failed, 1);
        assert_eq!(file_counts.skipped, 2);
    }

    #[test]
    fn result_file_parser_ignores_schema_json_without_flat_counts() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let results_file = temp_dir.path().join("test-results.json");
        std::fs::write(
            &results_file,
            r#"{
                "schema": "custom-provider/test-results/v1",
                "summary": { "total": 5, "passed": 2, "failed": 1, "skipped": 2 }
            }"#,
        )
        .expect("write test results");

        let counts = parse_test_results_file(&results_file);
        let counts = counts.expect("schema JSON should parse");

        assert!(counts.is_none());
    }

    #[test]
    fn failures_file_parser_accepts_empty_object_payload() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let failures_file = temp_dir.path().join("test-failures.json");
        std::fs::write(
            &failures_file,
            r#"{
                "failures": [],
                "total": 10,
                "passed": 10
            }"#,
        )
        .expect("write test failures");

        let parsed = parse_failures_file(&failures_file)
            .expect("object-form test failures should parse")
            .expect("test failures payload should be present");

        assert!(parsed.failures.is_empty());
        assert_eq!(parsed.total, 10);
        assert_eq!(parsed.passed, 10);
    }

    #[test]
    fn failures_file_parser_accepts_runtime_helper_array_payload() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let failures_file = temp_dir.path().join("test-failures.json");
        std::fs::write(
            &failures_file,
            r#"[
                {
                    "test_id": "suite::case",
                    "file": "tests/suite.rs",
                    "line": 42,
                    "failure_type": "assertion",
                    "message": "failed assertion"
                }
            ]"#,
        )
        .expect("write test failures");

        let parsed = parse_failures_file(&failures_file)
            .expect("array-form test failures should parse")
            .expect("test failures payload should be present");

        assert_eq!(parsed.total, 1);
        assert_eq!(parsed.passed, 0);
        assert_eq!(parsed.failures.len(), 1);
        assert_eq!(parsed.failures[0].test_name, "suite::case");
        assert_eq!(parsed.failures[0].test_file, "tests/suite.rs");
        assert_eq!(parsed.failures[0].source_line, 42);
        assert_eq!(parsed.failures[0].error_type, "assertion");
        assert_eq!(parsed.failures[0].message, "failed assertion");
    }

    #[test]
    fn failures_file_parser_preserves_records_with_nullable_or_missing_fields() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let failures_file = temp_dir.path().join("test-failures.json");
        std::fs::write(
            &failures_file,
            r#"{
                "failures": [
                    {
                        "test_id": "suite::case",
                        "file": null,
                        "failure_type": null,
                        "message": null,
                        "source": null,
                        "line": null
                    },
                    { "message": "assertion failed" }
                ],
                "total": null,
                "passed": null
            }"#,
        )
        .expect("write test failures");

        let parsed = parse_failures_file(&failures_file)
            .expect("nullable test failures should parse")
            .expect("test failures payload should be present");

        assert_eq!(parsed.total, 2);
        assert_eq!(parsed.passed, 0);
        assert_eq!(parsed.failures[0].test_name, "suite::case");
        assert_eq!(
            parsed.failures[0].message,
            "test failure (no message provided)"
        );
        assert_eq!(parsed.failures[1].test_name, "unknown test");
        assert_eq!(parsed.failures[1].message, "assertion failed");
    }

    #[test]
    fn failures_file_parser_accepts_null_or_missing_failure_collections() {
        let temp_dir = tempfile::tempdir().expect("temp dir");
        let failures_file = temp_dir.path().join("test-failures.json");
        std::fs::write(&failures_file, r#"{ "failures": null }"#).expect("write null failures");

        let parsed = parse_failures_file(&failures_file)
            .expect("null failures should parse")
            .expect("test failures payload should be present");

        assert!(parsed.failures.is_empty());
    }
}
