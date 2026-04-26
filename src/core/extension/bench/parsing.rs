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
//!         "p95_ms": 145.0,
//!         "status_500_count": 0,
//!         "error_rate": 0.0,
//!         "distributions": {
//!           "agent_loop_ms": [1000.0, 1200.0, 1400.0]
//!         }
//!       },
//!       "memory": { "peak_bytes": 41943040 }
//!     }
//!   ]
//! }
//! ```

use std::collections::BTreeMap;
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
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_policies: BTreeMap<String, BenchMetricPolicy>,
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
    /// Scenario origin. Dispatchers use `in_tree` for component-owned
    /// workloads and `rig` for out-of-tree workloads supplied by a rig spec.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub iterations: u64,
    pub metrics: BenchMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct BenchMetrics {
    #[serde(flatten)]
    pub values: BTreeMap<String, f64>,
    /// Raw per-iteration samples for variance-aware metrics.
    ///
    /// `values` remains the single-point summary contract; distributions
    /// are opt-in data used by variance-aware regression checks.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub distributions: BTreeMap<String, Vec<f64>>,
}

impl BenchMetrics {
    pub fn get(&self, key: &str) -> Option<f64> {
        self.values.get(key).copied()
    }

    pub fn distribution(&self, key: &str) -> Option<&[f64]> {
        self.distributions.get(key).map(Vec::as_slice)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchMetricPolicy {
    pub direction: BenchMetricDirection,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_threshold_percent: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_threshold_absolute: Option<f64>,
    /// True when this metric is expected to vary between iterations.
    ///
    /// Variance-aware metrics must emit a matching `metrics.distributions`
    /// entry so regression checks can compare distribution shape instead
    /// of a single summary point.
    #[serde(default, skip_serializing_if = "is_false")]
    pub variance_aware: bool,
    /// Minimum sample count needed for a meaningful variance-aware run.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_iterations_for_variance: Option<u64>,
    /// Statistical test used for variance-aware regression detection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regression_test: Option<RegressionTest>,
    /// Optional measurement-phase tag.
    ///
    /// Phase is **metadata only**: it does not affect regression math
    /// (cold and warm metrics use the same `direction` /
    /// `regression_threshold_*` fields), but it lets report renderers
    /// group metrics by phase so cold-start numbers don't mix with
    /// steady-state numbers in the same row of a diff table.
    ///
    /// Backwards-compatible: pre-existing JSON without `phase`
    /// deserializes as `None` and round-trips unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<BenchMetricPhase>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum BenchMetricDirection {
    #[serde(rename = "lower_is_better", alias = "lower")]
    LowerIsBetter,
    #[serde(rename = "higher_is_better", alias = "higher")]
    HigherIsBetter,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RegressionTest {
    /// Legacy single-point threshold comparison.
    PointDelta,
    /// Non-parametric rank test, useful when distributions are not Normal.
    MannWhitneyU,
    /// Distribution-shape test sensitive to CDF shifts.
    KolmogorovSmirnov,
}

/// Measurement-phase tag for a metric.
///
/// A bench run can mix one-time setup costs (process spawn, WASM boot,
/// dependency install) with steady-state per-iteration costs. Without
/// this tag every metric ends up in one flat alphabetical list and a
/// 3500ms cold-boot sits next to a 12ms warm request as though they
/// were comparable. Phase tagging lets the report renderer group cold
/// metrics first, warm metrics second, amortized last, so the diff
/// reads as the actually-useful story instead of a flat dump.
///
/// Phase is **opt-in**: pre-existing policies without a `phase` field
/// stay untagged and render identically to today.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum BenchMetricPhase {
    /// One-time setup cost (process spawn, WASM boot, dependency
    /// install). First iteration only; subsequent iterations don't pay
    /// this cost unless the dispatcher restarts the substrate between
    /// iterations.
    Cold,
    /// Steady-state per-iteration cost after warmup. The metric the
    /// user sees on every request after the first.
    Warm,
    /// Synthetic blend, e.g. `(cold + N * warm) / N` for some N.
    /// Useful for "what does the user see on first page-load"
    /// framing where one cold request is amortized over a small
    /// burst of warm follow-ups.
    Amortized,
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
    let parsed: BenchResults = serde_json::from_str(raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse bench results JSON: {}", e),
            Some("bench.parsing.deserialize".to_string()),
        )
    })?;
    validate_variance_policies(&parsed)?;
    Ok(parsed)
}

fn validate_variance_policies(results: &BenchResults) -> Result<()> {
    for (name, policy) in &results.metric_policies {
        if !policy.variance_aware {
            continue;
        }
        for scenario in &results.scenarios {
            if scenario.metrics.get(name).is_none() {
                continue;
            }
            let Some(samples) = scenario.metrics.distribution(name) else {
                return Err(Error::validation_invalid_argument(
                    "metrics.distributions",
                    format!(
                        "variance-aware metric `{}` in scenario `{}` must emit metrics.distributions.{}",
                        name, scenario.id, name
                    ),
                    None,
                    None,
                ));
            };
            if samples.iter().any(|value| !value.is_finite()) {
                return Err(Error::validation_invalid_argument(
                    "metrics.distributions",
                    format!(
                        "variance-aware metric `{}` in scenario `{}` contains a non-finite sample",
                        name, scenario.id
                    ),
                    None,
                    None,
                ));
            }
            if let Some(min) = policy.min_iterations_for_variance {
                if samples.len() < min as usize {
                    return Err(Error::validation_invalid_argument(
                        "metrics.distributions",
                        format!(
                            "variance-aware metric `{}` in scenario `{}` has {} samples; minimum is {}",
                            name,
                            scenario.id,
                            samples.len(),
                            min
                        ),
                        None,
                        None,
                    ));
                }
            }
        }
    }
    Ok(())
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
        assert_eq!(scenario.metrics.get("p95_ms"), Some(145.0));
        assert_eq!(scenario.memory.as_ref().unwrap().peak_bytes, 41943040);
    }

    #[test]
    fn test_get() {
        let parsed = parse_bench_results_str(VALID_RESULTS).unwrap();
        let metrics = &parsed.scenarios[0].metrics;

        assert_eq!(metrics.get("p95_ms"), Some(145.0));
        assert_eq!(metrics.get("missing"), None);
    }

    #[test]
    fn test_parse_bench_results_str() {
        let parsed = parse_bench_results_str(VALID_RESULTS).unwrap();

        assert_eq!(parsed.component_id, "example");
    }

    #[test]
    fn test_parse_bench_results_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench-results.json");
        std::fs::write(&path, VALID_RESULTS).unwrap();

        let parsed = parse_bench_results_file(&path).unwrap();

        assert_eq!(parsed.scenarios.len(), 1);
    }

    #[test]
    fn parses_arbitrary_numeric_metrics_and_policies() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "metric_policies": {
                "error_rate": {
                    "direction": "lower_is_better",
                    "regression_threshold_absolute": 0.01
                },
                "requests_per_second": {
                    "direction": "higher",
                    "regression_threshold_percent": 5.0
                }
            },
            "scenarios": [
                {
                    "id": "concurrent_http",
                    "iterations": 10,
                    "metrics": {
                        "total_requests": 1200,
                        "status_500_count": 0,
                        "error_rate": 0.0,
                        "requests_per_second": 180.5
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let scenario = &parsed.scenarios[0];

        assert_eq!(scenario.metrics.get("status_500_count"), Some(0.0));
        assert_eq!(scenario.metrics.get("requests_per_second"), Some(180.5));
        assert_eq!(
            parsed.metric_policies["error_rate"].direction,
            BenchMetricDirection::LowerIsBetter
        );
        assert_eq!(
            parsed.metric_policies["requests_per_second"].direction,
            BenchMetricDirection::HigherIsBetter
        );
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
    fn parses_variance_aware_metric_distributions() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 20,
            "metric_policies": {
                "agent_loop_ms": {
                    "direction": "lower_is_better",
                    "variance_aware": true,
                    "min_iterations_for_variance": 3,
                    "regression_test": "mann_whitney_u"
                }
            },
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 20,
                    "metrics": {
                        "agent_loop_ms": 1200.0,
                        "distributions": {
                            "agent_loop_ms": [1000.0, 1200.0, 1400.0]
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let policy = &parsed.metric_policies["agent_loop_ms"];
        assert!(policy.variance_aware);
        assert_eq!(policy.regression_test, Some(RegressionTest::MannWhitneyU));
        assert_eq!(
            parsed.scenarios[0].metrics.distribution("agent_loop_ms"),
            Some(&[1000.0, 1200.0, 1400.0][..])
        );
    }

    #[test]
    fn rejects_variance_aware_metric_without_distribution() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 20,
            "metric_policies": {
                "agent_loop_ms": {
                    "direction": "lower_is_better",
                    "variance_aware": true
                }
            },
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 20,
                    "metrics": { "agent_loop_ms": 1200.0 }
                }
            ]
        }"#;

        assert!(parse_bench_results_str(raw).is_err());
    }

    #[test]
    fn rejects_variance_aware_metric_below_minimum_samples() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 20,
            "metric_policies": {
                "agent_loop_ms": {
                    "direction": "lower_is_better",
                    "variance_aware": true,
                    "min_iterations_for_variance": 5
                }
            },
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 20,
                    "metrics": {
                        "agent_loop_ms": 1200.0,
                        "distributions": { "agent_loop_ms": [1000.0, 1200.0] }
                    }
                }
            ]
        }"#;

        assert!(parse_bench_results_str(raw).is_err());
    }

    #[test]
    fn rejects_non_numeric_metric_values() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "scenario_one",
                    "iterations": 10,
                    "metrics": {
                        "error_rate": "bad"
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
            inner.contains("invalid type") || inner.contains("f64"),
            "expected invalid-metric error, got details: {}",
            inner
        );
    }

    #[test]
    fn rejects_malformed_json() {
        let raw = "not json at all";
        assert!(parse_bench_results_str(raw).is_err());
    }
}
