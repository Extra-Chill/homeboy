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
//!       "default_iterations": 10,
//!       "tags": ["cold", "lifecycle"],
//!       "iterations": 10,
//!       "metrics": {
//!         "p95_ms": 145.0,
//!         "status_500_count": 0,
//!         "error_rate": 0.0,
//!         "distributions": {
//!           "agent_loop_ms": [1000.0, 1200.0, 1400.0]
//!         }
//!       },
//!       "memory": { "peak_bytes": 41943040 },
//!       "artifacts": {
//!         "transcript": {
//!           "path": "bench-artifacts/scenario/transcript.json",
//!           "kind": "json",
//!           "label": "Agent transcript"
//!         }
//!       }
//!     }
//!   ]
//! }
//! ```

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

use super::artifact::BenchArtifact;
use super::distribution::BenchRunDistribution;

fn default_true() -> bool {
    true
}

fn is_true(value: &bool) -> bool {
    *value
}

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
    /// Declared default iteration count. List-only discovery uses this to
    /// expose runner defaults without executing the workload.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_iterations: Option<u64>,
    /// Freeform scenario labels supplied by extension runners.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    pub iterations: u64,
    pub metrics: BenchMetrics,
    /// Scenario-level semantic gates. Unlike metric policies, gates are
    /// correctness checks: any failure invalidates the scenario even if
    /// timing metrics improved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<BenchGate>,
    /// Computed gate outcomes, populated by Homeboy after metrics are
    /// parsed and aggregated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<BenchGateResult>,
    /// Scenario pass/fail status after semantic gates are evaluated.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
    /// Optional local artifact pointers produced by the scenario.
    ///
    /// Homeboy preserves paths and metadata but does not upload, retain, or
    /// diff artifact contents. Consumers can correlate artifacts by scenario,
    /// rig, and run without scraping logs or side-channel files.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, BenchArtifact>,
    /// Per-run raw metric snapshots when `homeboy bench --runs N` is used.
    /// Omitted for the default `--runs 1` path so existing envelopes keep
    /// their exact shape.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs: Option<Vec<BenchRunSnapshot>>,
    /// Cross-run distribution stats keyed by metric name. Omitted for the
    /// default `--runs 1` path.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runs_summary: Option<BTreeMap<String, BenchRunDistribution>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchGate {
    pub metric: String,
    pub op: BenchGateOp,
    pub value: f64,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum BenchGateOp {
    Eq,
    Gte,
    Lte,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchGateResult {
    pub metric: String,
    pub op: BenchGateOp,
    pub expected: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub actual: Option<f64>,
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl BenchGate {
    fn evaluate(&self, scenario_id: &str, metrics: &BenchMetrics) -> BenchGateResult {
        let actual = metrics.get(&self.metric);
        let passed = actual
            .map(|value| match self.op {
                BenchGateOp::Eq => value == self.value,
                BenchGateOp::Gte => value >= self.value,
                BenchGateOp::Lte => value <= self.value,
            })
            .unwrap_or(false);
        let reason = if passed {
            None
        } else {
            Some(match actual {
                Some(value) => format!(
                    "scenario `{}` gate failed: {} {} {} (actual {})",
                    scenario_id,
                    self.metric,
                    self.op.as_str(),
                    self.value,
                    value
                ),
                None => format!(
                    "scenario `{}` gate failed: metric `{}` is missing",
                    scenario_id, self.metric
                ),
            })
        };

        BenchGateResult {
            metric: self.metric.clone(),
            op: self.op,
            expected: self.value,
            actual,
            passed,
            reason,
        }
    }
}

impl BenchGateOp {
    fn as_str(self) -> &'static str {
        match self {
            BenchGateOp::Eq => "eq",
            BenchGateOp::Gte => "gte",
            BenchGateOp::Lte => "lte",
        }
    }
}

/// Evaluate semantic gates in place and return every failure reason.
pub fn evaluate_gates(results: &mut BenchResults) -> Vec<String> {
    let mut failures = Vec::new();
    for scenario in &mut results.scenarios {
        scenario.gate_results = scenario
            .gates
            .iter()
            .map(|gate| gate.evaluate(&scenario.id, &scenario.metrics))
            .collect();
        scenario.passed = scenario.gate_results.iter().all(|result| result.passed);
        failures.extend(
            scenario
                .gate_results
                .iter()
                .filter_map(|result| result.reason.clone()),
        );
    }
    failures
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchRunSnapshot {
    pub metrics: BenchMetrics,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, BenchArtifact>,
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
    validate_unique_scenario_ids(&parsed)?;
    validate_variance_policies(&parsed)?;
    Ok(parsed)
}

fn validate_unique_scenario_ids(results: &BenchResults) -> Result<()> {
    let mut seen: BTreeMap<&str, Option<&str>> = BTreeMap::new();

    for scenario in &results.scenarios {
        if let Some(first_file) = seen.insert(&scenario.id, scenario.file.as_deref()) {
            let first = first_file.unwrap_or("<unknown>");
            let second = scenario.file.as_deref().unwrap_or("<unknown>");
            return Err(Error::validation_invalid_argument(
                "scenarios.id",
                format!(
                    "duplicate bench scenario id `{}` from `{}` and `{}`; scenario ids must be unique, so dispatchers should derive ids from workload paths relative to the bench root or fail discovery before emitting results",
                    scenario.id, first, second
                ),
                Some(scenario.id.clone()),
                Some(vec![first.to_string(), second.to_string()]),
            ));
        }
    }

    Ok(())
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
                "default_iterations": 10,
                "tags": ["cold", "cli"],
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
        assert_eq!(scenario.default_iterations, Some(10));
        assert_eq!(scenario.tags, vec!["cold", "cli"]);
        assert_eq!(scenario.metrics.get("p95_ms"), Some(145.0));
        assert_eq!(scenario.memory.as_ref().unwrap().peak_bytes, 41943040);
        assert!(scenario.artifacts.is_empty());
    }

    #[test]
    fn parses_scenario_artifacts() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 1,
                    "metrics": { "success_rate": 1.0 },
                    "artifacts": {
                        "transcript": {
                            "path": "artifacts/agent-loop/transcript.json",
                            "kind": "json",
                            "label": "Agent transcript"
                        },
                        "final_output": {
                            "path": "artifacts/agent-loop/final.md"
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let artifacts = &parsed.scenarios[0].artifacts;

        assert_eq!(artifacts.len(), 2);
        assert_eq!(
            artifacts["transcript"].path,
            "artifacts/agent-loop/transcript.json"
        );
        assert_eq!(artifacts["transcript"].kind.as_deref(), Some("json"));
        assert_eq!(
            artifacts["transcript"].label.as_deref(),
            Some("Agent transcript")
        );
        assert_eq!(
            artifacts["final_output"].path,
            "artifacts/agent-loop/final.md"
        );
        assert_eq!(artifacts["final_output"].kind, None);

        let serialized = serde_json::to_string(&parsed).unwrap();
        assert!(serialized.contains("\"artifacts\""));
        assert!(serialized.contains("artifacts/agent-loop/transcript.json"));
    }

    #[test]
    fn omits_empty_scenario_artifacts() {
        let parsed = parse_bench_results_str(VALID_RESULTS).unwrap();
        let raw = serde_json::to_string(&parsed.scenarios[0]).unwrap();

        assert!(!raw.contains("artifacts"));
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
    fn semantic_gate_pass_leaves_scenario_passed() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 10,
                    "metrics": {
                        "assistant_message_count": 2,
                        "identifies_studio_rate": 1.0
                    },
                    "gates": [
                        { "metric": "assistant_message_count", "op": "gte", "value": 1 },
                        { "metric": "identifies_studio_rate", "op": "eq", "value": 1.0 }
                    ]
                }
            ]
        }"#;

        let mut parsed = parse_bench_results_str(raw).unwrap();
        let failures = evaluate_gates(&mut parsed);
        let scenario = &parsed.scenarios[0];

        assert!(failures.is_empty());
        assert!(scenario.passed);
        assert_eq!(scenario.gate_results.len(), 2);
        assert!(scenario.gate_results.iter().all(|result| result.passed));
    }

    #[test]
    fn semantic_gate_failure_marks_scenario_failed() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 10,
                    "metrics": {
                        "assistant_message_count": 0,
                        "p95_ms": 80.0
                    },
                    "gates": [
                        { "metric": "assistant_message_count", "op": "gte", "value": 1 }
                    ]
                }
            ]
        }"#;

        let mut parsed = parse_bench_results_str(raw).unwrap();
        let failures = evaluate_gates(&mut parsed);
        let scenario = &parsed.scenarios[0];

        assert!(!scenario.passed);
        assert_eq!(failures.len(), 1);
        assert!(failures[0].contains("assistant_message_count gte 1"));
        assert_eq!(scenario.gate_results[0].actual, Some(0.0));
    }

    #[test]
    fn timing_improvement_does_not_override_semantic_gate_failure() {
        let baseline = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                { "id": "agent_loop", "iterations": 10, "metrics": { "p95_ms": 100.0 } }
            ]
        }"#;
        let current = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 10,
                    "metrics": { "p95_ms": 50.0, "assistant_message_count": 0 },
                    "gates": [
                        { "metric": "assistant_message_count", "op": "gte", "value": 1 }
                    ]
                }
            ]
        }"#;

        let baseline = parse_bench_results_str(baseline).unwrap();
        let mut current = parse_bench_results_str(current).unwrap();
        let failures = evaluate_gates(&mut current);

        assert!(
            current.scenarios[0].metrics.get("p95_ms").unwrap()
                < baseline.scenarios[0].metrics.get("p95_ms").unwrap()
        );
        assert_eq!(failures.len(), 1);
        assert!(!current.scenarios[0].passed);
    }

    #[test]
    fn semantic_gate_failure_serializes_details() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 10,
                    "metrics": { "identifies_studio_rate": 0.0 },
                    "gates": [
                        { "metric": "identifies_studio_rate", "op": "gte", "value": 1.0 }
                    ]
                }
            ]
        }"#;

        let mut parsed = parse_bench_results_str(raw).unwrap();
        let failures = evaluate_gates(&mut parsed);
        let value = serde_json::to_value(&parsed).unwrap();
        let scenario = &value["scenarios"][0];

        assert_eq!(failures.len(), 1);
        assert_eq!(scenario["passed"], serde_json::Value::Bool(false));
        assert_eq!(
            scenario["gate_results"][0]["metric"],
            "identifies_studio_rate"
        );
        assert_eq!(scenario["gate_results"][0]["op"], "gte");
        assert_eq!(scenario["gate_results"][0]["expected"], 1.0);
        assert_eq!(scenario["gate_results"][0]["actual"], 0.0);
        assert_eq!(scenario["gate_results"][0]["passed"], false);
        assert!(scenario["gate_results"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("identifies_studio_rate gte 1"));
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
    fn rejects_duplicate_scenario_ids_from_same_basename_subdirs() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "heavy",
                    "file": "tests/bench/reads/heavy.php",
                    "iterations": 10,
                    "metrics": { "p95_ms": 10.0 }
                },
                {
                    "id": "heavy",
                    "file": "tests/bench/writes/heavy.php",
                    "iterations": 10,
                    "metrics": { "p95_ms": 20.0 }
                }
            ]
        }"#;

        let err = parse_bench_results_str(raw).unwrap_err();
        let problem = err
            .details
            .get("problem")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        assert!(
            problem.contains("duplicate bench scenario id `heavy`"),
            "expected duplicate-id problem, got: {}",
            problem
        );
        assert!(problem.contains("tests/bench/reads/heavy.php"));
        assert!(problem.contains("tests/bench/writes/heavy.php"));
        assert!(problem.contains("workload paths relative to the bench root"));
        assert_eq!(
            err.details.get("id").and_then(|v| v.as_str()),
            Some("heavy")
        );
    }

    #[test]
    fn accepts_relative_path_scenario_ids_for_same_basename_subdirs() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "reads-heavy",
                    "file": "tests/bench/reads/heavy.php",
                    "iterations": 10,
                    "metrics": { "p95_ms": 10.0 }
                },
                {
                    "id": "writes-heavy",
                    "file": "tests/bench/writes/heavy.php",
                    "iterations": 10,
                    "metrics": { "p95_ms": 20.0 }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();

        assert_eq!(parsed.scenarios.len(), 2);
        assert_eq!(parsed.scenarios[0].id, "reads-heavy");
        assert_eq!(parsed.scenarios[1].id, "writes-heavy");
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
