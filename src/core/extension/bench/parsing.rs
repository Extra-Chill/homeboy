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

use crate::core::error::{Error, Result};
use crate::core::observation::timeline::{reporting_timeline, summarize_spans};

use super::artifact_validation;
use super::metric_policy_preset::expand_metric_policy_presets;
use super::phase_events::evaluate_phase_events;

pub use super::result_types::{
    BenchMemory, BenchMetricDirection, BenchMetricPhase, BenchMetricPolicy, BenchMetrics,
    BenchProvenance, BenchProvenanceLink, BenchResults, BenchRunExecution, BenchRunMetadata,
    BenchRunSnapshot, BenchRunnerMetadata, BenchScenario, BenchWorkloadMetadata, RegressionTest,
};

/// Derive scenario span results from the shared observation timeline contract.
fn evaluate_spans(results: &mut BenchResults) {
    for scenario in &mut results.scenarios {
        if scenario.span_definitions.is_empty() {
            continue;
        }
        let timeline = reporting_timeline(&scenario.timeline);
        scenario.span_results = summarize_spans(&timeline, &scenario.span_definitions);
    }
}

/// Read and parse a `$HOMEBOY_BENCH_RESULTS_FILE` written by an extension.
pub fn parse_bench_results_file(path: &Path) -> Result<BenchResults> {
    parse_bench_results_file_with_artifact_context(path, None)
}

pub fn parse_bench_results_file_with_artifact_context(
    path: &Path,
    rig_id: Option<&str>,
) -> Result<BenchResults> {
    parse_bench_results_file_with_artifact_context_and_scenarios(path, rig_id, &[])
}

pub fn parse_bench_results_file_with_artifact_context_and_scenarios(
    path: &Path,
    rig_id: Option<&str>,
    scenario_ids: &[String],
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
    parse_bench_results_str_with_artifact_context_and_scenarios(&content, rig_id, scenario_ids)
}

/// Parse a raw JSON string into a `BenchResults`.
pub fn parse_bench_results_str(raw: &str) -> Result<BenchResults> {
    parse_bench_results_str_with_artifact_context(raw, None)
}

fn parse_bench_results_str_with_artifact_context(
    raw: &str,
    rig_id: Option<&str>,
) -> Result<BenchResults> {
    parse_bench_results_str_with_artifact_context_and_scenarios(raw, rig_id, &[])
}

fn parse_bench_results_str_with_artifact_context_and_scenarios(
    raw: &str,
    rig_id: Option<&str>,
    scenario_ids: &[String],
) -> Result<BenchResults> {
    let mut value: serde_json::Value = serde_json::from_str(raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse bench results JSON: {}", e),
            Some("bench.parsing.deserialize".to_string()),
        )
    })?;
    if let Some(object) = value.as_object_mut() {
        object.remove("schema");
        object.remove("lifecycle");
        object.remove("reset_policy");
        object.remove("warmup_iterations");
        if object
            .get("provenance")
            .is_some_and(|value| !is_bench_provenance_contract(value))
        {
            object.remove("provenance");
        }
    }
    filter_value_scenarios_by_ids(&mut value, scenario_ids);
    normalize_extension_sample_metrics(&mut value);
    normalize_diagnostic_producer_sources(&mut value);
    normalize_inline_artifact_payloads(&mut value);
    let mut parsed: BenchResults = serde_json::from_value(value).map_err(|e| {
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

fn normalize_inline_artifact_payloads(value: &mut serde_json::Value) {
    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for scenario in scenarios {
        normalize_artifact_map(scenario.get_mut("artifacts"));

        let Some(runs) = scenario
            .get_mut("runs")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for run in runs {
            normalize_artifact_map(run.get_mut("artifacts"));
        }
    }
}

fn normalize_artifact_map(value: Option<&mut serde_json::Value>) {
    let Some(artifacts) = value.and_then(serde_json::Value::as_object_mut) else {
        return;
    };

    artifacts.retain(|_, artifact| artifact_has_pointer_field(artifact));
}

fn artifact_has_pointer_field(artifact: &serde_json::Value) -> bool {
    let Some(object) = artifact.as_object() else {
        return false;
    };
    [
        "path",
        "url",
        "public_url",
        "preview_url",
        "viewer_url",
        "local_url",
        "observation_artifact_id",
    ]
    .iter()
    .any(|field| object.contains_key(*field))
}

fn normalize_diagnostic_producer_sources(value: &mut serde_json::Value) {
    normalize_diagnostic_array(value.get_mut("diagnostics"));

    if let Some(run_metadata) = value.get_mut("run_metadata") {
        normalize_diagnostic_array(run_metadata.get_mut("diagnostics"));
    }

    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for scenario in scenarios {
        normalize_diagnostic_array(scenario.get_mut("diagnostics"));
        let Some(runs) = scenario
            .get_mut("runs")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for run in runs {
            normalize_diagnostic_array(run.get_mut("diagnostics"));
        }
    }
}

fn normalize_diagnostic_array(value: Option<&mut serde_json::Value>) {
    let Some(diagnostics) = value.and_then(serde_json::Value::as_array_mut) else {
        return;
    };

    for diagnostic in diagnostics {
        let Some(object) = diagnostic.as_object_mut() else {
            continue;
        };
        let Some(source) = object.remove("source") else {
            continue;
        };
        match source {
            serde_json::Value::String(source) => {
                let metadata_key = if object.contains_key("metadata") {
                    "metadata"
                } else if object.contains_key("details") {
                    "details"
                } else {
                    "metadata"
                };
                let metadata = object
                    .entry(metadata_key.to_string())
                    .or_insert_with(|| serde_json::json!({}));
                if let Some(metadata_object) = metadata.as_object_mut() {
                    metadata_object.insert(
                        "producer_source".to_string(),
                        serde_json::Value::String(source),
                    );
                }
            }
            other => {
                object.insert("source".to_string(), other);
            }
        }
    }
}

fn filter_value_scenarios_by_ids(value: &mut serde_json::Value, scenario_ids: &[String]) {
    if scenario_ids.is_empty() {
        return;
    }

    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    scenarios.retain(|scenario| {
        scenario
            .get("id")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|id| scenario_ids.iter().any(|selected| selected == id))
    });
}

fn normalize_extension_sample_metrics(value: &mut serde_json::Value) {
    let Some(scenarios) = value
        .get_mut("scenarios")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return;
    };

    for scenario in scenarios {
        if let Some(object) = scenario.as_object_mut() {
            if object
                .get("provenance")
                .is_some_and(|value| !is_bench_provenance_contract(value))
            {
                object.remove("provenance");
            }
        }
        let Some(metrics) = scenario
            .get_mut("metrics")
            .and_then(serde_json::Value::as_object_mut)
        else {
            continue;
        };

        let mut normalized = serde_json::Map::new();
        let mut distributions = serde_json::Map::new();
        for (name, metric) in std::mem::take(metrics) {
            if metric.is_number() {
                normalized.insert(name, metric);
                continue;
            }

            let Some(samples) = metric.get("samples").and_then(serde_json::Value::as_object) else {
                normalized.insert(name, metric);
                continue;
            };
            if let Some(mean) = samples.get("mean").and_then(serde_json::Value::as_f64) {
                if let Some(number) = serde_json::Number::from_f64(mean) {
                    normalized.insert(name.clone(), serde_json::Value::Number(number));
                }
            }
            if let Some(values) = samples.get("values").and_then(serde_json::Value::as_array) {
                distributions.insert(name, serde_json::Value::Array(values.clone()));
            }
        }

        if !distributions.is_empty() {
            normalized.insert(
                "distributions".to_string(),
                serde_json::Value::Object(distributions),
            );
        }
        *metrics = normalized;
    }
}

fn is_bench_provenance_contract(value: &serde_json::Value) -> bool {
    value
        .as_object()
        .is_some_and(|object| object.contains_key("links") || object.contains_key("labels"))
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
    fn parses_results_with_top_level_schema_marker() {
        let raw = r#"{
            "schema": "homeboy/bench-results/v1",
            "component_id": "example",
            "iterations": 1,
            "scenarios": [
                {
                    "id": "scenario_one",
                    "iterations": 1,
                    "metrics": { "p95_ms": 145.0 }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        assert_eq!(parsed.component_id, "example");
        assert_eq!(parsed.scenarios.len(), 1);
    }

    #[test]
    fn parses_extension_bench_result_markers_and_sample_metrics() {
        let raw = r#"{
            "schema": "extension/bench-results/v1",
            "component_id": "woocommerce",
            "iterations": 1,
            "warmup_iterations": 0,
            "lifecycle": { "phases": [], "diagnostics": [] },
            "provenance": { "command": "wordpress.bench" },
            "reset_policy": { "betweenIterations": "none", "betweenScenarios": "none" },
            "scenarios": [
                {
                    "id": "checkout-shipping-cache",
                    "iterations": 1,
                    "provenance": { "workload_file": "tests/bench/checkout-shipping-cache.php" },
                    "metrics": {
                        "actual_package_count": {
                            "unit": "count",
                            "samples": {
                                "mean": 8,
                                "values": [8]
                            }
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let metrics = &parsed.scenarios[0].metrics;
        assert_eq!(metrics.get("actual_package_count"), Some(8.0));
        assert_eq!(
            metrics.distribution("actual_package_count"),
            Some(&[8.0][..])
        );
    }

    #[test]
    fn parses_extension_diagnostics_with_producer_source() {
        let raw = r#"{
            "component_id": "woocommerce",
            "iterations": 1,
            "diagnostics": [
                {
                    "code": "wordpress.bench.stdout_noise",
                    "message": "captured non-JSON stdout",
                    "severity": "warning",
                    "source": "wordpress.bench/stdout",
                    "details": { "line_count": 3 }
                }
            ],
            "scenarios": [
                {
                    "id": "checkout-gateway-compatibility-matrix",
                    "iterations": 1,
                    "metrics": { "success_rate": 0.0 }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();
        let diagnostic = &parsed.diagnostics[0];

        assert_eq!(diagnostic.class, "wordpress.bench.stdout_noise");
        assert_eq!(diagnostic.severity.as_deref(), Some("warning"));
        assert_eq!(diagnostic.source, None);
        assert_eq!(diagnostic.metadata["line_count"], 3);
        assert_eq!(
            diagnostic.metadata["producer_source"].as_str(),
            Some("wordpress.bench/stdout")
        );
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
    fn parses_top_level_and_scenario_provenance() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "provenance": {
                "labels": ["source: zendesk", "privacy: internal"],
                "links": [
                    {
                        "url": "https://automattic.zendesk.com/agent/tickets/9426116",
                        "label": "Zendesk ticket 9426116",
                        "source": "zendesk",
                        "privacy": "internal"
                    }
                ]
            },
            "scenarios": [
                {
                    "id": "checkout_latency",
                    "iterations": 1,
                    "metrics": { "p95_ms": 250.0 },
                    "provenance": {
                        "labels": ["scenario: shortcode checkout place-order latency"],
                        "links": [
                            {
                                "url": "https://wordpress.org/support/topic/checkout-is-very-slow/",
                                "label": "WordPress.org support thread",
                                "source": "wordpress.org"
                            }
                        ]
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();

        assert_eq!(parsed.provenance.labels[0], "source: zendesk");
        assert_eq!(
            parsed.provenance.links[0].source.as_deref(),
            Some("zendesk")
        );
        assert_eq!(
            parsed.scenarios[0].provenance.links[0].url,
            "https://wordpress.org/support/topic/checkout-is-very-slow/"
        );
        let serialized = serde_json::to_string(&parsed).unwrap();
        assert!(serialized.contains("automattic.zendesk.com"));
        assert!(serialized.contains("wordpress.org/support"));
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
    fn selected_parse_ignores_unselected_duplicate_scenario_ids() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "scenarios": [
                {
                    "id": "target",
                    "file": "tests/bench/target.php",
                    "iterations": 1,
                    "metrics": { "p95_ms": 5.0 }
                },
                {
                    "id": "unrelated-duplicate",
                    "file": "tests/bench/first.php",
                    "iterations": 1,
                    "metrics": { "p95_ms": 10.0 }
                },
                {
                    "id": "unrelated-duplicate",
                    "file": "tests/bench/second.php",
                    "iterations": 1,
                    "metrics": { "p95_ms": 20.0 }
                }
            ]
        }"#;

        let selected = vec!["target".to_string()];
        let parsed =
            parse_bench_results_str_with_artifact_context_and_scenarios(raw, None, &selected)
                .unwrap();

        assert_eq!(parsed.scenarios.len(), 1);
        assert_eq!(parsed.scenarios[0].id, "target");
    }

    #[test]
    fn selected_parse_still_rejects_selected_duplicate_scenario_ids() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "scenarios": [
                {
                    "id": "target",
                    "file": "tests/bench/target-one.php",
                    "iterations": 1,
                    "metrics": { "p95_ms": 5.0 }
                },
                {
                    "id": "target",
                    "file": "tests/bench/target-two.php",
                    "iterations": 1,
                    "metrics": { "p95_ms": 10.0 }
                }
            ]
        }"#;

        let selected = vec!["target".to_string()];
        let err = parse_bench_results_str_with_artifact_context_and_scenarios(raw, None, &selected)
            .unwrap_err();
        let problem = err
            .details
            .get("problem")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        assert!(problem.contains("duplicate bench scenario id `target`"));
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
    fn accepts_inline_benchmark_artifacts_without_paths() {
        let raw = r#"{
            "component_id": "example",
            "iterations": 1,
            "scenarios": [
                {
                    "id": "query-profile",
                    "iterations": 1,
                    "metrics": { "duration_ms": 12.0 },
                    "artifacts": {
                        "query-profile": {
                            "schema": "example/query-profile/v1",
                            "summary": { "query_count": 1 },
                            "cases": [ { "case_id": "sample-a", "samples": [] } ]
                        },
                        "diagnostics": {
                            "path": "bench/diagnostics.json",
                            "kind": "json"
                        }
                    }
                }
            ]
        }"#;

        let parsed = parse_bench_results_str(raw).unwrap();

        assert!(!parsed.scenarios[0].artifacts.contains_key("query-profile"));
        assert!(parsed.scenarios[0].artifacts.contains_key("diagnostics"));
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
