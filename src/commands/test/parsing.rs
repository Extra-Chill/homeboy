use serde::Serialize;

use homeboy::test_analyze::TestAnalysisInput;
use homeboy::test_baseline::TestCounts;
use homeboy::utils::io;

#[derive(Serialize)]
pub struct CoverageOutput {
    pub lines_pct: f64,
    pub lines_total: u64,
    pub lines_covered: u64,
    pub methods_pct: f64,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub uncovered_files: Vec<UncoveredFile>,
}

#[derive(Serialize)]
pub struct UncoveredFile {
    pub file: String,
    pub line_pct: f64,
}

/// Parse the test failures JSON file written by the extension test runner.
pub fn parse_failures_file(path: &std::path::Path) -> Option<TestAnalysisInput> {
    let content = io::read_file(path, "read test failures file").ok()?;
    let mut parsed: TestAnalysisInput = serde_json::from_str(&content).ok()?;

    // Backfill aggregate counters from failure list when extension output omits
    // totals (legacy parser shape). This keeps --analyze metadata accurate.
    if parsed.total == 0 && !parsed.failures.is_empty() {
        parsed.total = parsed.failures.len() as u64;
    }

    if parsed.passed > parsed.total {
        parsed.passed = parsed.total;
    }

    Some(parsed)
}

/// Parse the test results JSON file written by the extension test runner.
pub fn parse_test_results_file(path: &std::path::Path) -> Option<TestCounts> {
    let content = io::read_file(path, "read test results file").ok()?;
    let data: serde_json::Value = serde_json::from_str(&content).ok()?;

    let total = data.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let passed = data.get("passed").and_then(|v| v.as_u64()).unwrap_or(0);
    let failed = data.get("failed").and_then(|v| v.as_u64()).unwrap_or(0);
    let skipped = data.get("skipped").and_then(|v| v.as_u64()).unwrap_or(0);

    Some(TestCounts::new(total, passed, failed, skipped))
}

/// Parse the coverage JSON file written by the extension test runner.
pub fn parse_coverage_file(path: &std::path::Path) -> std::result::Result<CoverageOutput, ()> {
    let content = io::read_file(path, "read coverage file").map_err(|_| ())?;
    let data: serde_json::Value = serde_json::from_str(&content).map_err(|_| ())?;

    let totals = data.get("totals").ok_or(())?;
    let lines = totals.get("lines").ok_or(())?;
    let methods = totals.get("methods").ok_or(())?;

    let lines_pct = lines.get("pct").and_then(|v| v.as_f64()).unwrap_or(0.0);
    let lines_total = lines.get("total").and_then(|v| v.as_u64()).unwrap_or(0);
    let lines_covered = lines.get("covered").and_then(|v| v.as_u64()).unwrap_or(0);
    let methods_pct = methods.get("pct").and_then(|v| v.as_f64()).unwrap_or(0.0);

    // Collect files below 50% coverage as "uncovered"
    let uncovered_files = data
        .get("files")
        .and_then(|f| f.as_array())
        .map(|files| {
            files
                .iter()
                .filter_map(|f| {
                    let pct = f.get("line_pct").and_then(|v| v.as_f64())?;
                    if pct < 50.0 {
                        Some(UncoveredFile {
                            file: f
                                .get("file")
                                .and_then(|v| v.as_str())
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
    fn parse_failures_file_backfills_totals_when_missing() {
        let tmp = std::env::temp_dir().join("homeboy-test-failures-backfill.json");
        let _ = std::fs::remove_file(&tmp);

        let payload = r#"{
            "failures": [
                {
                    "test_name": "Suite::test_one",
                    "test_file": "tests/suite_test.php",
                    "error_type": "Error",
                    "message": "Call to undefined method Foo::bar()"
                },
                {
                    "test_name": "Suite::test_two",
                    "test_file": "tests/suite_test.php",
                    "error_type": "Error",
                    "message": "Call to undefined method Foo::bar()"
                }
            ]
        }"#;

        std::fs::write(&tmp, payload).unwrap();
        let parsed = parse_failures_file(&tmp).expect("should parse failures file");

        assert_eq!(parsed.failures.len(), 2);
        assert_eq!(parsed.total, 2);
        assert_eq!(parsed.passed, 0);

        let _ = std::fs::remove_file(&tmp);
    }

    #[test]
    fn parse_failures_file_clamps_invalid_passed_count() {
        let tmp = std::env::temp_dir().join("homeboy-test-failures-clamp.json");
        let _ = std::fs::remove_file(&tmp);

        let payload = r#"{
            "failures": [
                {
                    "test_name": "Suite::test_one",
                    "test_file": "tests/suite_test.php",
                    "error_type": "Error",
                    "message": "Call to undefined method Foo::bar()"
                }
            ],
            "total": 3,
            "passed": 9
        }"#;

        std::fs::write(&tmp, payload).unwrap();
        let parsed = parse_failures_file(&tmp).expect("should parse failures file");

        assert_eq!(parsed.total, 3);
        assert_eq!(parsed.passed, 3);

        let _ = std::fs::remove_file(&tmp);
    }
}
