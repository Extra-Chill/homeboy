//! Trace baseline adapter: span durations and assertion outcomes (#15840).
//!
//! Trace plugs into the shared baseline / compare / gate engine
//! ([`homeboy_engine_primitives::workload_compare`]). A snapshot is every
//! timed span plus every assertion (status and metric); a span regresses when
//! it slows by more than both the percent threshold and the minimum delta, an
//! assertion when it stops passing or its metric grows.

use serde::{Deserialize, Serialize};

use homeboy_engine_primitives::baseline as generic;
use homeboy_engine_primitives::workload_compare::{pair_by_key, KindAdapter, Verdict};

use super::parsing::{TraceAssertion, TraceAssertionStatus, TraceResults};

pub const DEFAULT_REGRESSION_THRESHOLD_PERCENT: f64 = 5.0;
pub const DEFAULT_REGRESSION_MIN_DELTA_MS: u64 = 50;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceSpanSnapshot {
    pub id: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceAssertionSnapshot {
    pub id: String,
    pub status: TraceAssertionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<f64>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TraceAssertionDelta {
    pub id: String,
    pub baseline_status: TraceAssertionStatus,
    pub current_status: TraceAssertionStatus,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline_metric: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_metric: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metric_delta: Option<f64>,
    pub regression: bool,
    pub improvement: bool,
}

/// Trace snapshot; exactly what `homeboy.json` stores as the baseline metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceBaselineMetadata {
    #[serde(default)]
    pub spans: Vec<TraceSpanSnapshot>,
    #[serde(default)]
    pub assertions: Vec<TraceAssertionSnapshot>,
}

pub type TraceBaseline = generic::Baseline<TraceBaselineMetadata>;

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TraceSpanDelta {
    pub id: String,
    pub baseline_duration_ms: u64,
    pub current_duration_ms: u64,
    pub delta_ms: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta_pct: Option<f64>,
    pub regression: bool,
    pub improvement: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct TraceBaselineComparison {
    pub threshold_percent: f64,
    pub min_delta_ms: u64,
    pub spans: Vec<TraceSpanDelta>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub assertions: Vec<TraceAssertionDelta>,
    pub new_span_ids: Vec<String>,
    pub removed_span_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub new_assertion_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub removed_assertion_ids: Vec<String>,
    pub regression: bool,
    pub has_improvements: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub reasons: Vec<String>,
}

/// Span regression thresholds: a span regresses only when it slows by more
/// than `min_delta_ms` *and* by more than `threshold_percent`.
#[derive(Debug, Clone, Copy)]
pub struct TraceThresholds {
    pub threshold_percent: f64,
    pub min_delta_ms: u64,
}

/// The trace [`KindAdapter`].
pub struct TraceAdapter;

impl KindAdapter for TraceAdapter {
    const KIND: &'static str = "trace";
    type Source = TraceResults;
    type Snapshot = TraceBaselineMetadata;
    type Options = TraceThresholds;
    type Delta = TraceBaselineComparison;

    fn snapshot(results: &TraceResults) -> TraceBaselineMetadata {
        TraceBaselineMetadata {
            spans: results
                .span_results
                .iter()
                .filter_map(|span| {
                    Some(TraceSpanSnapshot {
                        id: span.id.clone(),
                        duration_ms: span.duration_ms?,
                    })
                })
                .collect(),
            assertions: results
                .assertions
                .iter()
                .map(|assertion| TraceAssertionSnapshot {
                    id: assertion.id.clone(),
                    status: assertion.status.clone(),
                    metric: assertion_metric(assertion),
                })
                .collect(),
        }
    }

    fn fingerprints(snapshot: &TraceBaselineMetadata) -> Vec<String> {
        let spans = snapshot
            .spans
            .iter()
            .map(|s| format!("trace-span:{}", s.id));
        let assertions = snapshot
            .assertions
            .iter()
            .map(|a| format!("trace-assertion:{}", a.id));
        spans.chain(assertions).collect()
    }

    fn delta(
        baseline: &TraceBaselineMetadata,
        current: &TraceBaselineMetadata,
        thresholds: &TraceThresholds,
    ) -> TraceBaselineComparison {
        let TraceThresholds {
            threshold_percent,
            min_delta_ms,
        } = *thresholds;
        let mut reasons = Vec::new();

        let span_pairs = pair_by_key(&baseline.spans, &current.spans, |span| span.id.as_str());
        let spans: Vec<TraceSpanDelta> = span_pairs
            .matched
            .into_iter()
            .map(|(prior, span)| {
                let delta_ms = span.duration_ms as i64 - prior.duration_ms as i64;
                let delta_pct = (prior.duration_ms != 0)
                    .then(|| (delta_ms as f64 / prior.duration_ms as f64) * 100.0);
                let regression = delta_ms > min_delta_ms as i64
                    && delta_pct.is_some_and(|pct| pct > threshold_percent);
                if regression {
                    reasons.push(format!(
                        "{} regressed by {}ms ({:.2}%)",
                        span.id,
                        delta_ms,
                        delta_pct.unwrap_or_default()
                    ));
                }
                TraceSpanDelta {
                    id: span.id.clone(),
                    baseline_duration_ms: prior.duration_ms,
                    current_duration_ms: span.duration_ms,
                    delta_ms,
                    delta_pct,
                    regression,
                    improvement: delta_ms < 0,
                }
            })
            .collect();

        let assertion_pairs =
            pair_by_key(&baseline.assertions, &current.assertions, |a| a.id.as_str());
        let assertions: Vec<TraceAssertionDelta> = assertion_pairs
            .matched
            .into_iter()
            .map(|(prior, assertion)| {
                let pass = TraceAssertionStatus::Pass;
                let metric_delta = assertion
                    .metric
                    .zip(prior.metric)
                    .map(|(current, baseline)| current - baseline);
                let status_regression = prior.status == pass && assertion.status != pass;
                let metric_regression = metric_delta.is_some_and(|delta| delta > 0.0);
                if status_regression {
                    reasons.push(format!(
                        "{} assertion regressed from {:?} to {:?}",
                        assertion.id, prior.status, assertion.status
                    ));
                } else if let Some(delta) = metric_delta.filter(|_| metric_regression) {
                    reasons.push(format!(
                        "{} assertion metric regressed by {:.2}",
                        assertion.id, delta
                    ));
                }
                TraceAssertionDelta {
                    id: assertion.id.clone(),
                    baseline_status: prior.status.clone(),
                    current_status: assertion.status.clone(),
                    baseline_metric: prior.metric,
                    current_metric: assertion.metric,
                    metric_delta,
                    regression: status_regression || metric_regression,
                    improvement: (prior.status != pass && assertion.status == pass)
                        || metric_delta.is_some_and(|delta| delta < 0.0),
                }
            })
            .collect();

        TraceBaselineComparison {
            threshold_percent,
            min_delta_ms,
            regression: spans.iter().any(|s| s.regression)
                || assertions.iter().any(|a| a.regression),
            has_improvements: spans.iter().any(|s| s.improvement)
                || assertions.iter().any(|a| a.improvement),
            spans,
            assertions,
            new_span_ids: span_pairs.added,
            removed_span_ids: span_pairs.removed,
            new_assertion_ids: assertion_pairs.added,
            removed_assertion_ids: assertion_pairs.removed,
            reasons,
        }
    }

    fn gate(comparison: &TraceBaselineComparison) -> Verdict {
        let improved_spans = comparison.spans.iter().filter(|s| s.improvement);
        let improved_assertions = comparison.assertions.iter().filter(|a| a.improvement);
        Verdict {
            regressions: comparison.reasons.clone(),
            advisories: Vec::new(),
            improvements: improved_spans
                .map(|s| s.id.clone())
                .chain(improved_assertions.map(|a| a.id.clone()))
                .collect(),
        }
    }
}

fn assertion_metric(assertion: &TraceAssertion) -> Option<f64> {
    let details = assertion.details.as_ref()?;
    match details.get("kind")?.as_str()? {
        "count" | "forbidden-event" => details.get("actual")?.as_f64(),
        "max-concurrent" => details.get("max_observed")?.as_f64(),
        "no-overlap" => details.get("overlap_count")?.as_f64(),
        "ordering" => details.get("violation_count")?.as_f64(),
        "latency-bound" => details
            .get("actual_p95_ms")
            .or_else(|| details.get("actual_p99_ms"))
            .or_else(|| details.get("actual_p50_ms"))?
            .as_f64(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::extension::trace::parsing::{
        TraceAssertion, TraceSpanResult, TraceSpanStatus, TraceStatus,
    };
    use homeboy_engine_primitives::workload_compare::{self as engine, BaselineSlot};
    use std::path::{Path, PathBuf};

    // Storage and the compare entry point now belong to the shared engine.
    // These keep every test below pinned to the same observable behaviour
    // (stored baseline, comparison payload) it covered before.
    fn save_baseline(
        root: &Path,
        component_id: &str,
        results: &TraceResults,
        rig_id: Option<&str>,
    ) -> homeboy_core::Result<PathBuf> {
        engine::save_baseline::<TraceAdapter>(
            &BaselineSlot::new(root, rig_id),
            component_id,
            results,
        )
    }

    fn load_baseline(root: &Path, rig_id: Option<&str>) -> Option<TraceBaseline> {
        engine::load_baseline::<TraceAdapter>(&BaselineSlot::new(root, rig_id))
    }

    fn compare(
        current: &TraceResults,
        baseline: &TraceBaseline,
        threshold_percent: f64,
        min_delta_ms: u64,
    ) -> TraceBaselineComparison {
        TraceAdapter::delta(
            &baseline.metadata,
            &TraceAdapter::snapshot(current),
            &TraceThresholds {
                threshold_percent,
                min_delta_ms,
            },
        )
    }

    fn results(duration_ms: u64) -> TraceResults {
        TraceResults {
            component_id: "studio".to_string(),
            scenario_id: "create-site".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            evidence: None,
            timeline: Vec::new(),
            span_definitions: Vec::new(),
            span_results: vec![TraceSpanResult {
                id: "submit_to_cli".to_string(),
                from: "ui.submit".to_string(),
                to: "cli.start".to_string(),
                status: TraceSpanStatus::Ok,
                duration_ms: Some(duration_ms),
                from_t_ms: Some(0),
                to_t_ms: Some(duration_ms),
                missing: Vec::new(),
                message: None,
            }],
            assertions: Vec::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
            metrics: Default::default(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
            preview: None,
        }
    }

    #[test]
    fn test_compare() {
        let baseline = TraceBaseline {
            created_at: "2026-01-01T00:00:00Z".to_string(),
            context_id: "studio".to_string(),
            item_count: 1,
            known_fingerprints: vec!["submit_to_cli".to_string()],
            metadata: TraceBaselineMetadata {
                spans: vec![TraceSpanSnapshot {
                    id: "submit_to_cli".to_string(),
                    duration_ms: 100,
                }],
                assertions: Vec::new(),
            },
        };

        let comparison = compare(
            &results(180),
            &baseline,
            5.0,
            DEFAULT_REGRESSION_MIN_DELTA_MS,
        );

        assert!(comparison.regression);
        assert_eq!(comparison.spans[0].delta_ms, 80);
        assert_eq!(comparison.spans[0].delta_pct, Some(80.0));
    }

    #[test]
    fn test_compare_requires_min_delta_for_regression() {
        let baseline = TraceBaseline {
            created_at: "2026-01-01T00:00:00Z".to_string(),
            context_id: "studio".to_string(),
            item_count: 1,
            known_fingerprints: vec!["submit_to_cli".to_string()],
            metadata: TraceBaselineMetadata {
                spans: vec![TraceSpanSnapshot {
                    id: "submit_to_cli".to_string(),
                    duration_ms: 9,
                }],
                assertions: Vec::new(),
            },
        };

        let comparison = compare(
            &results(15),
            &baseline,
            5.0,
            DEFAULT_REGRESSION_MIN_DELTA_MS,
        );

        assert!(!comparison.regression);
        assert_eq!(comparison.min_delta_ms, DEFAULT_REGRESSION_MIN_DELTA_MS);
        assert_eq!(comparison.spans[0].delta_ms, 6);
        assert_eq!(comparison.spans[0].delta_pct, Some(66.66666666666666));
        assert!(!comparison.spans[0].regression);
    }

    #[test]
    fn test_save_baseline() {
        let temp = tempfile::tempdir().unwrap();
        save_baseline(temp.path(), "studio", &results(100), None).unwrap();

        let loaded = load_baseline(temp.path(), None).expect("baseline loads");
        assert_eq!(loaded.metadata.spans[0].id, "submit_to_cli");
        assert_eq!(loaded.known_fingerprints[0], "trace-span:submit_to_cli");
    }

    #[test]
    fn test_compare_assertion_metric_regression() {
        let baseline = TraceBaseline {
            created_at: "2026-01-01T00:00:00Z".to_string(),
            context_id: "studio".to_string(),
            item_count: 1,
            known_fingerprints: vec!["trace-assertion:invalid-grant-count".to_string()],
            metadata: TraceBaselineMetadata {
                spans: Vec::new(),
                assertions: vec![TraceAssertionSnapshot {
                    id: "invalid-grant-count".to_string(),
                    status: TraceAssertionStatus::Pass,
                    metric: Some(0.0),
                }],
            },
        };
        let mut current = results(100);
        current.span_results.clear();
        current.assertions.push(TraceAssertion {
            id: "invalid-grant-count".to_string(),
            status: TraceAssertionStatus::Pass,
            message: None,
            details: Some(serde_json::json!({
                "kind": "count",
                "actual": 1,
            })),
        });

        let comparison = compare(&current, &baseline, 5.0, DEFAULT_REGRESSION_MIN_DELTA_MS);

        assert!(comparison.regression);
        assert_eq!(comparison.assertions[0].metric_delta, Some(1.0));
        assert!(comparison.assertions[0].regression);
    }

    #[test]
    fn test_save_baseline_includes_assertions() {
        let temp = tempfile::tempdir().unwrap();
        let mut current = results(100);
        current.span_results.clear();
        current.assertions.push(TraceAssertion {
            id: "no-invalid-grant".to_string(),
            status: TraceAssertionStatus::Pass,
            message: None,
            details: Some(serde_json::json!({
                "kind": "forbidden-event",
                "actual": 0,
            })),
        });

        save_baseline(temp.path(), "studio", &current, None).unwrap();

        let loaded = load_baseline(temp.path(), None).expect("baseline loads");
        assert!(loaded.metadata.spans.is_empty());
        assert_eq!(loaded.metadata.assertions[0].id, "no-invalid-grant");
        assert_eq!(loaded.metadata.assertions[0].metric, Some(0.0));
        assert_eq!(
            loaded.known_fingerprints[0],
            "trace-assertion:no-invalid-grant"
        );
    }

    #[test]
    fn test_save_baseline_updates_assertion_metadata_for_same_fingerprint() {
        let temp = tempfile::tempdir().unwrap();
        let mut current = results(100);
        current.span_results.clear();
        current.assertions.push(TraceAssertion {
            id: "invalid-grant-count".to_string(),
            status: TraceAssertionStatus::Pass,
            message: None,
            details: Some(serde_json::json!({
                "kind": "count",
                "actual": 2,
            })),
        });
        save_baseline(temp.path(), "studio", &current, None).unwrap();

        current.assertions[0].details = Some(serde_json::json!({
            "kind": "count",
            "actual": 0,
        }));
        save_baseline(temp.path(), "studio", &current, None).unwrap();

        let loaded = load_baseline(temp.path(), None).expect("baseline loads");
        assert_eq!(loaded.metadata.assertions[0].metric, Some(0.0));
    }

    #[test]
    fn test_load_baseline() {
        let temp = tempfile::tempdir().unwrap();

        assert!(load_baseline(temp.path(), Some("studio-rig")).is_none());
    }
}
