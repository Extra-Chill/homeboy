//! Bench runner JSON output parsing.
//!
//! The extension's bench runner writes a JSON envelope to the path in
//! `$HOMEBOY_BENCH_RESULTS_FILE`. The schema is strict on top-level keys
//! (unknown top-level fields are rejected) but tolerant of unknown
//! scenario-level keys so extensions can emit extra metadata without
//! breaking forward compatibility.
//!
//! # Schema
//!
//! ```json
//! {
//!   "component_id": "string",
//!   "iterations": 10,
//!   "scenarios": [
//!     {
//!       "id": "scenario_slug",
//!       "file": "tests/bench/some-workload.ext",
//!       "iterations": 10,
//!       "metrics": {
//!         "mean_ms": 120.3,
//!         "p50_ms": 118.0,
//!         "p95_ms": 145.0,
//!         "p99_ms": 160.0,
//!         "min_ms": 110.0,
//!         "max_ms": 172.0
//!       },
//!       "memory": { "peak_bytes": 41943040 }
//!     }
//!   ]
//! }
//! ```

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// Full bench run output from an extension script.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchResults {
    pub component_id: String,
    pub iterations: u64,
    pub scenarios: Vec<BenchScenario>,
}

/// One scenario's measurements.
///
/// Scenario-level unknown keys are accepted to keep the contract
/// forward-compatible: a runner can emit extra metadata (tags, warmup
/// counts, environment info) without breaking parsers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchScenario {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub iterations: u64,
    pub metrics: BenchMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchMetrics {
    pub mean_ms: f64,
    pub p50_ms: f64,
    pub p95_ms: f64,
    pub p99_ms: f64,
    pub min_ms: f64,
    pub max_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchMemory {
    pub peak_bytes: u64,
}

/// Read and parse a `$HOMEBOY_BENCH_RESULTS_FILE` written by an extension.
pub fn parse_bench_results_file(path: &Path) -> Result<BenchResults> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to read bench results file {}: {}",
                path.display(),
                e
            ),
            Some("bench.parsing.read".to_string()),
        )
    })?;
    parse_bench_results_str(&content)
}

/// Parse a raw JSON string into a `BenchResults`.
pub fn parse_bench_results_str(raw: &str) -> Result<BenchResults> {
    serde_json::from_str(raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse bench results JSON: {}", e),
            Some("bench.parsing.deserialize".to_string()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const VALID_RESULTS: &str = r#"{
        "component_id": "example",
        "iterations": 10,
        "scenarios": [
            {
                "id": "scenario_one",
                "file": "bench/one.ext",
                "iterations": 10,
                "metrics": {
                    "mean_ms": 120.5,
                    "p50_ms": 118.0,
                    "p95_ms": 145.0,
                    "p99_ms": 160.0,
                    "min_ms": 110.0,
                    "max_ms": 172.5
                },
                "memory": { "peak_bytes": 41943040 }
            }
        ]
    }"#;

    #[test]
    fn parses_valid_results() {
        let parsed = parse_bench_results_str(VALID_RESULTS).unwrap();
        assert_eq!(parsed.component_id, "example");
        assert_eq!(parsed.iterations, 10);
        assert_eq!(parsed.scenarios.len(), 1);
        let scenario = &parsed.scenarios[0];
        assert_eq!(scenario.id, "scenario_one");
        assert_eq!(scenario.file.as_deref(), Some("bench/one.ext"));
        assert_eq!(scenario.metrics.p95_ms, 145.0);
        assert_eq!(scenario.memory.as_ref().unwrap().peak_bytes, 41943040);
    }

    #[test]
    fn rejects_unknown_top_level_keys() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [],
            "unexpected_top_level": true
        }"#;
        let err = parse_bench_results_str(raw).unwrap_err();
        let inner = err
            .details
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            inner.contains("unexpected_top_level") || inner.contains("unknown field"),
            "expected unknown-field error, got details: {}",
            inner
        );
    }

    #[test]
    fn tolerates_unknown_scenario_level_keys() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "scenario_one",
                    "iterations": 10,
                    "metrics": {
                        "mean_ms": 120.5,
                        "p50_ms": 118.0,
                        "p95_ms": 145.0,
                        "p99_ms": 160.0,
                        "min_ms": 110.0,
                        "max_ms": 172.5
                    },
                    "extra_metadata": "tolerated",
                    "tags": ["warmup", "cold"]
                }
            ]
        }"#;
        let parsed = parse_bench_results_str(raw).unwrap();
        assert_eq!(parsed.scenarios.len(), 1);
        assert_eq!(parsed.scenarios[0].id, "scenario_one");
    }

    #[test]
    fn rejects_missing_required_metric() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "scenario_one",
                    "iterations": 10,
                    "metrics": {
                        "mean_ms": 120.5,
                        "p50_ms": 118.0,
                        "p95_ms": 145.0,
                        "p99_ms": 160.0,
                        "min_ms": 110.0
                    }
                }
            ]
        }"#;
        let err = parse_bench_results_str(raw).unwrap_err();
        let inner = err
            .details
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("");
        assert!(
            inner.contains("max_ms") || inner.contains("missing field"),
            "expected missing-field error, got details: {}",
            inner
        );
    }

    #[test]
    fn rejects_malformed_json() {
        let raw = "not json at all";
        assert!(parse_bench_results_str(raw).is_err());
    }
}
