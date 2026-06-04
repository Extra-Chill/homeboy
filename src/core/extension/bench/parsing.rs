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
//!       "metric_groups": {
//!         "phases": {
//!           "resolve_ai_environment_ms": 120.0,
//!           "first_assistant_message_ms": 800.0
//!         }
//!       },
//!       "timeline": [
//!         { "t_ms": 0, "source": "runner", "event": "start" },
//!         { "t_ms": 120, "source": "runner", "event": "ready" }
//!       ],
//!       "span_definitions": [
//!         { "id": "startup", "from": "runner.start", "to": "runner.ready" }
//!       ],
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

use crate::core::error::{Error, Result};
use crate::core::finding::HomeboyFinding;
use crate::core::observation::timeline::{
    reporting_timeline, summarize_spans, ObservationEvent, ObservationSpanDefinition,
    ObservationSpanResult,
};

use super::artifact::BenchArtifact;
use super::artifact_validation;
use super::diagnostic::BenchDiagnostic;
use super::distribution::BenchRunDistribution;
use super::gate::{BenchGate, BenchGateResult};
use super::metric_policy_preset::{expand_metric_policy_presets, BenchMetricPolicyPreset};
use super::phase_events::{
    evaluate_phase_events, BenchPhaseEvent, BenchPhaseFailureClassification, BenchPhaseSummary,
};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_metadata: Option<BenchRunMetadata>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_groups: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<serde_json::Value>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub span_definitions: BTreeMap<String, serde_json::Value>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
    /// Structured lifecycle events emitted by the bench runner.
    ///
    /// Runners can append these as long phases progress so persisted bench
    /// evidence can distinguish dependency setup, runtime startup, workload
    /// execution, artifact collection, or any other runner-defined phase.
    /// Extension-specific details belong in `payload`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_events: Vec<BenchPhaseEvent>,
    /// Derived phase rollup computed from `phase_events`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub phase_summaries: Vec<BenchPhaseSummary>,
    /// Derived classification for the first timeout/failure phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_classification: Option<BenchPhaseFailureClassification>,
    #[serde(
        default,
        deserialize_with = "super::budget_findings::deserialize_budget_findings",
        skip_serializing_if = "Vec::is_empty"
    )]
    pub budget_findings: Vec<HomeboyFinding>,
    pub scenarios: Vec<BenchScenario>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_policies: BTreeMap<String, BenchMetricPolicy>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_policy_presets: BTreeMap<String, BenchMetricPolicyPreset>,
}

/// Homeboy-owned reproducibility metadata for a bench invocation.
///
/// Extension runners are not required to emit this block. Homeboy stamps it
/// after parsing so stored bench artifacts explain what ran without requiring
/// each language runner to duplicate CLI/runtime bookkeeping.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchRunMetadata {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub homeboy_version: Option<String>,
    pub started_at: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub shared_state: Option<String>,
    pub iterations: u64,
    #[serde(flatten)]
    pub execution: BenchRunExecution,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warmup_iterations: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_scenarios: Vec<String>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env_overrides: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub workloads: Vec<BenchWorkloadMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub runner: Option<BenchRunnerMetadata>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Default)]
#[serde(deny_unknown_fields)]
pub struct BenchRunExecution {
    pub runs: u64,
    pub concurrency: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchWorkloadMetadata {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BenchRunnerMetadata {
    pub extension: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
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
    /// Optional grouped numeric metrics for secondary metric families.
    ///
    /// Flat `metrics` remains the primary backwards-compatible contract.
    /// Runners can opt into grouped metrics when a scenario naturally emits
    /// related values (for example phase timings or tool-call stats) without
    /// flattening those groups in the source JSON.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_groups: BTreeMap<String, BTreeMap<String, f64>>,
    /// Optional scenario timeline for causal evidence.
    ///
    /// Bench scenarios use the shared observation timeline contract so
    /// runners can emit phase/span evidence without flattening everything
    /// into metrics. Homeboy derives `span_results` from this timeline when
    /// `span_definitions` are present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<ObservationEvent>,
    /// Optional span definitions over `source.event` timeline keys.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_definitions: Vec<ObservationSpanDefinition>,
    /// Computed span outcomes, populated by Homeboy after parsing.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_results: Vec<ObservationSpanResult>,
    /// Scenario-level semantic gates. Unlike metric policies, gates are
    /// correctness checks: any failure invalidates the scenario even if
    /// timing metrics improved.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<BenchGate>,
    /// Computed gate outcomes, populated by Homeboy after metrics are
    /// parsed and aggregated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_results: Vec<BenchGateResult>,
    /// Scenario-level categorical metadata emitted by benchmark workloads.
    ///
    /// Numeric values belong in `metrics`; this freeform object is for
    /// labels and feature flags that are useful when aggregating persisted
    /// observation runs, such as design fingerprints or workload choices.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metadata: BTreeMap<String, serde_json::Value>,
    /// Scenario pass/fail status after semantic gates are evaluated.
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub passed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
    /// Optional artifact pointers produced by the scenario.
    ///
    /// Homeboy preserves paths/URLs and metadata but does not upload, retain,
    /// or diff artifact contents. Consumers can correlate artifacts by
    /// scenario, rig, and run without scraping logs or side-channel files.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, BenchArtifact>,
    /// Workload-emitted diagnostics for this scenario.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
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

/// Derive scenario span results from the shared observation timeline contract.
pub fn evaluate_spans(results: &mut BenchResults) {
    for scenario in &mut results.scenarios {
        if scenario.span_definitions.is_empty() {
            continue;
        }
        let timeline = reporting_timeline(&scenario.timeline);
        scenario.span_results = summarize_spans(&timeline, &scenario.span_definitions);
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BenchRunSnapshot {
    pub metrics: BenchMetrics,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub metric_groups: BTreeMap<String, BTreeMap<String, f64>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub timeline: Vec<ObservationEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_definitions: Vec<ObservationSpanDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_results: Vec<ObservationSpanResult>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<BenchMemory>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub artifacts: BTreeMap<String, BenchArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<BenchDiagnostic>,
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
    parse_bench_results_file_with_artifact_context(path, None)
}

pub fn parse_bench_results_file_with_artifact_context(
    path: &Path,
    rig_id: Option<&str>,
) -> Result<BenchResults> {
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
    parse_bench_results_str_with_artifact_context(&content, rig_id)
}

/// Parse a raw JSON string into a `BenchResults`.
pub fn parse_bench_results_str(raw: &str) -> Result<BenchResults> {
    parse_bench_results_str_with_artifact_context(raw, None)
}

fn parse_bench_results_str_with_artifact_context(
    raw: &str,
    rig_id: Option<&str>,
) -> Result<BenchResults> {
    let mut parsed: BenchResults = serde_json::from_str(raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse bench results JSON: {}", e),
            Some("bench.parsing.deserialize".to_string()),
        )
    })?;
    validate_unique_scenario_ids(&parsed)?;
    expand_metric_policy_presets(&mut parsed)?;
    validate_variance_policies(&parsed)?;
    evaluate_phase_events(&mut parsed);
    evaluate_spans(&mut parsed);
    artifact_validation::validate_artifact_paths(&parsed, rig_id)?;
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
    use super::super::gate::{evaluate_gates, BenchGateOp};
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
        assert!(scenario.metadata.is_empty());
        assert!(scenario.artifacts.is_empty());
    }

    #[test]
    fn parses_scenario_metadata() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "scenarios": [
                {
                    "id": "site_build",
                    "iterations": 1,
                    "metrics": { "success_rate": 1.0 },
                    "metadata": {
                        "design": {
                            "dominant_font_family": "Space Grotesk",
                            "motifs": ["terminal_window", "glow_overlay"]
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let metadata = &parsed.scenarios[0].metadata;
        assert_eq!(
            metadata["design"]["dominant_font_family"].as_str(),
            Some("Space Grotesk")
        );
        assert_eq!(
            metadata["design"]["motifs"][0].as_str(),
            Some("terminal_window")
        );
    }

    #[test]
    fn parses_runner_level_phase_evidence() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "metadata": {
                "runner": { "phase_status": "captured" }
            },
            "metric_groups": {
                "runner_phases_ms": { "setup": 42.0 }
            },
            "timeline": [
                { "t_ms": 0, "source": "runner", "event": "start" },
                { "t_ms": 42, "source": "runner", "event": "setup" }
            ],
            "span_definitions": {
                "setup": { "from": "runner.start", "to": "runner.setup" }
            },
            "scenarios": [
                {
                    "id": "example-scenario",
                    "iterations": 1,
                    "metrics": { "p95_ms": 42.0 }
                }
            ]
        }"#;
        let parsed = parse_bench_results_str(raw).unwrap();
        assert_eq!(
            parsed.metadata["runner"]["phase_status"].as_str(),
            Some("captured")
        );
        assert_eq!(
            parsed.metric_groups["runner_phases_ms"].get("setup"),
            Some(&42.0)
        );
        assert_eq!(parsed.timeline.len(), 2);
        assert!(parsed.span_definitions.contains_key("setup"));
    }

    #[test]
    fn derives_phase_summaries_and_timeout_classification() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "phase_events": [
                { "phase": "dependency_preparation", "status": "started", "t_ms": 0 },
                { "phase": "dependency_preparation", "status": "heartbeat", "t_ms": 1000, "message": "installing" },
                {
                    "phase": "dependency_preparation",
                    "status": "timeout",
                    "t_ms": 2000,
                    "message": "dependency install exceeded budget",
                    "diagnostics": { "budget_ms": 2000 },
                    "payload": { "operation": "dependency_install" }
                }
            ],
            "scenarios": [
                {
                    "id": "example-scenario",
                    "iterations": 1,
                    "metrics": { "p95_ms": 42.0 }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();

        assert_eq!(parsed.phase_events.len(), 3);
        assert_eq!(parsed.phase_summaries.len(), 1);
        let summary = &parsed.phase_summaries[0];
        assert_eq!(summary.phase, "dependency_preparation");
        assert_eq!(summary.status, "timeout");
        assert_eq!(summary.first_t_ms, Some(0));
        assert_eq!(summary.last_t_ms, Some(2000));
        assert_eq!(summary.duration_ms, Some(2000));
        assert_eq!(summary.heartbeat_count, 1);
        assert_eq!(summary.diagnostic_count, 1);

        let classification = parsed.failure_classification.expect("classification");
        assert_eq!(classification.kind, "timeout");
        assert_eq!(classification.phase, "dependency_preparation");
        assert_eq!(
            classification.message.as_deref(),
            Some("dependency install exceeded budget")
        );
    }

    #[test]
    fn derives_scenario_span_results_from_timeline() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 1,
                    "metrics": { "success_rate": 1.0 },
                    "timeline": [
                        { "t_ms": 10, "source": "runner", "event": "start" },
                        { "t_ms": 45, "source": "runner", "event": "ready" }
                    ],
                    "span_definitions": [
                        { "id": "startup", "from": "runner.start", "to": "runner.ready" }
                    ]
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let scenario = &parsed.scenarios[0];

        assert_eq!(scenario.timeline.len(), 2);
        assert_eq!(scenario.span_definitions.len(), 1);
        assert_eq!(scenario.span_results.len(), 1);
        assert_eq!(
            scenario.span_results[0].status,
            crate::core::observation::timeline::ObservationSpanStatus::Ok
        );
        assert_eq!(scenario.span_results[0].duration_ms, Some(35));
    }

    #[test]
    fn omits_empty_timeline_and_spans_on_serialize() {
        let parsed = parse_bench_results_str(VALID_RESULTS).unwrap();
        let raw = serde_json::to_string(&parsed.scenarios[0]).unwrap();

        assert!(!raw.contains("timeline"));
        assert!(!raw.contains("span_definitions"));
        assert!(!raw.contains("span_results"));
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
                        },
                        "frontend": {
                            "type": "url",
                            "kind": "frontend_url",
                            "url": "https://example.test/",
                            "label": "Frontend"
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let artifacts = &parsed.scenarios[0].artifacts;

        assert_eq!(artifacts.len(), 3);
        assert_eq!(
            artifacts["transcript"].path.as_deref(),
            Some("artifacts/agent-loop/transcript.json")
        );
        assert_eq!(artifacts["transcript"].kind.as_deref(), Some("json"));
        assert_eq!(
            artifacts["transcript"].label.as_deref(),
            Some("Agent transcript")
        );
        assert_eq!(
            artifacts["final_output"].path.as_deref(),
            Some("artifacts/agent-loop/final.md")
        );
        assert_eq!(artifacts["final_output"].kind, None);
        assert_eq!(artifacts["frontend"].artifact_type.as_deref(), Some("url"));
        assert_eq!(artifacts["frontend"].kind.as_deref(), Some("frontend_url"));
        assert_eq!(
            artifacts["frontend"].url.as_deref(),
            Some("https://example.test/")
        );

        let serialized = serde_json::to_string(&parsed).unwrap();
        assert!(serialized.contains("\"artifacts\""));
        assert!(serialized.contains("artifacts/agent-loop/transcript.json"));
        assert!(serialized.contains("https://example.test/"));
    }

    #[test]
    fn rejects_empty_scenario_artifact_path_with_contract_guidance() {
        let err = parse_bench_results_str(
            r#"{
                "component_id": "studio",
                "iterations": 1,
                "scenarios": [
                    {
                        "id": "site_build",
                        "file": "bench/site-build.bench.mjs",
                        "iterations": 1,
                        "metrics": { "success_rate": 1.0 },
                        "artifacts": {
                            "visual_comparison_dir": { "path": "" }
                        }
                    }
                ]
            }"#,
        )
        .expect_err("empty artifact path should fail validation");

        let message = err.to_string();
        assert!(message.contains("component id `studio`"));
        assert!(message.contains("workload id `bench/site-build.bench.mjs`"));
        assert!(message.contains("scenario id `site_build`"));
        assert!(message.contains("phase `scenario`"));
        assert!(message.contains("artifact key `visual_comparison_dir`"));
        assert!(message.contains("Omit optional artifacts"));
        assert!(message.contains("real diagnostics file/directory"));
    }

    #[test]
    fn rejects_empty_measured_iteration_artifact_path_with_iteration_context() {
        let err = parse_bench_results_str(
            r#"{
                "component_id": "studio",
                "iterations": 2,
                "scenarios": [
                    {
                        "id": "site_build",
                        "file": "bench/site-build.bench.mjs",
                        "iterations": 2,
                        "metrics": { "success_rate": 1.0 },
                        "runs": [
                            { "metrics": { "success_rate": 1.0 } },
                            {
                                "metrics": { "success_rate": 1.0 },
                                "artifacts": {
                                    "visual_comparison_dir": { "path": "   " }
                                }
                            }
                        ]
                    }
                ]
            }"#,
        )
        .expect_err("empty measured iteration artifact path should fail validation");

        let message = err.to_string();
        assert!(message.contains("component id `studio`"));
        assert!(message.contains("workload id `bench/site-build.bench.mjs`"));
        assert!(message.contains("scenario id `site_build`"));
        assert!(message.contains("phase `iteration`"));
        assert!(message.contains("iteration 2"));
        assert!(message.contains("artifact key `visual_comparison_dir`"));
        assert!(message.contains("Omit optional artifacts"));
    }

    #[test]
    fn empty_artifact_path_diagnostic_includes_rig_when_available() {
        let err = parse_bench_results_str_with_artifact_context(
            r#"{
                "component_id": "studio",
                "iterations": 1,
                "scenarios": [
                    {
                        "id": "site_build",
                        "iterations": 1,
                        "metrics": { "success_rate": 1.0 },
                        "artifacts": {
                            "visual_comparison_dir": { "path": "" }
                        }
                    }
                ]
            }"#,
            Some("studio-bfb"),
        )
        .expect_err("empty artifact path should include rig context");

        assert!(err.to_string().contains("rig id `studio-bfb`"));
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
    fn test_parse_bench_results_file_with_artifact_context() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("bench-results.json");
        std::fs::write(
            &path,
            r#"{
                "component_id": "studio",
                "iterations": 1,
                "scenarios": [
                    {
                        "id": "site_build",
                        "iterations": 1,
                        "metrics": { "success_rate": 1.0 },
                        "artifacts": {
                            "visual_comparison_dir": { "path": "" }
                        }
                    }
                ]
            }"#,
        )
        .unwrap();

        let err = parse_bench_results_file_with_artifact_context(&path, Some("studio-bfb"))
            .expect_err("empty artifact path should include rig context");

        assert!(err.to_string().contains("rig id `studio-bfb`"));
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
    fn parses_and_serializes_grouped_numeric_metrics() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "scenarios": [
                {
                    "id": "agent_loop",
                    "iterations": 10,
                    "metrics": {
                        "elapsed_ms": 1400.0
                    },
                    "metric_groups": {
                        "phases": {
                            "resolve_ai_environment_ms": 120.0,
                            "first_assistant_message_ms": 800.0
                        },
                        "tools": {
                            "max_tool_duration_ms": 250.0
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let scenario = &parsed.scenarios[0];

        assert_eq!(scenario.metrics.get("elapsed_ms"), Some(1400.0));
        assert_eq!(
            scenario.metric_groups["phases"].get("resolve_ai_environment_ms"),
            Some(&120.0)
        );
        assert_eq!(
            scenario.metric_groups["phases"].get("first_assistant_message_ms"),
            Some(&800.0)
        );
        assert_eq!(
            scenario.metric_groups["tools"].get("max_tool_duration_ms"),
            Some(&250.0)
        );

        let serialized = serde_json::to_string(&parsed).unwrap();
        assert!(
            serialized.contains("\"metric_groups\""),
            "metric_groups must round-trip in JSON output: {}",
            serialized
        );
        assert!(serialized.contains("\"phases\""), "got: {}", serialized);
        assert!(
            serialized.contains("\"first_assistant_message_ms\":800.0"),
            "got: {}",
            serialized
        );
    }

    #[test]
    fn flat_only_metrics_omit_metric_groups_on_serialize() {
        let parsed = parse_bench_results_str(VALID_RESULTS).unwrap();
        assert!(parsed.scenarios[0].metric_groups.is_empty());

        let raw = serde_json::to_string(&parsed.scenarios[0]).unwrap();
        assert!(
            !raw.contains("metric_groups"),
            "flat-only scenarios should keep legacy JSON shape: {}",
            raw
        );
    }

    #[test]
    fn test_evaluate_gates() {
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
        assert_eq!(parsed.budget_findings[0].metadata["passed"], false);
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
    fn latency_metric_policy_preset_expands_to_metric_policy() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "metric_policy_presets": {
                "agent_loop_ms": {
                    "preset": "latency_regression",
                    "regression_threshold_percent": 7.5,
                    "phase": "warm"
                }
            },
            "scenarios": [
                {
                    "id": "agent-loop",
                    "iterations": 10,
                    "metrics": { "agent_loop_ms": 1200.0 }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let policy = parsed.metric_policies.get("agent_loop_ms").unwrap();

        assert_eq!(policy.direction, BenchMetricDirection::LowerIsBetter);
        assert_eq!(policy.regression_threshold_percent, Some(7.5));
        assert_eq!(policy.phase, Some(BenchMetricPhase::Warm));
    }

    #[test]
    fn memory_metric_policy_preset_uses_memory_threshold_default() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "metric_policy_presets": {
                "peak_rss_bytes": { "preset": "memory_regression" }
            },
            "scenarios": [
                {
                    "id": "audit-self",
                    "iterations": 10,
                    "metrics": { "peak_rss_bytes": 41943040.0 }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let policy = parsed.metric_policies.get("peak_rss_bytes").unwrap();

        assert_eq!(policy.direction, BenchMetricDirection::LowerIsBetter);
        assert_eq!(policy.regression_threshold_percent, Some(10.0));
    }

    #[test]
    fn absolute_budget_preset_expands_to_gate_and_budget_finding() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 10,
            "metric_policy_presets": {
                "peak_rss_bytes": { "preset": "absolute_budget", "max": 1000 }
            },
            "scenarios": [
                {
                    "id": "audit-self",
                    "iterations": 10,
                    "metrics": { "peak_rss_bytes": 2000.0 }
                }
            ]
        }"#;

        let mut parsed = parse_bench_results_str(raw).unwrap();
        let failures = evaluate_gates(&mut parsed);

        assert_eq!(parsed.scenarios[0].gates.len(), 1);
        assert_eq!(parsed.scenarios[0].gates[0].op, BenchGateOp::Lte);
        assert!(!failures.is_empty());
        assert_eq!(
            parsed.budget_findings[0].rule.as_deref(),
            Some("bench.gate.peak_rss_bytes")
        );
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
