//! Trace span baseline and ratchet support.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::engine::baseline::{self as generic, BaselineConfig};
use crate::core::error::Result;

use super::parsing::{TraceAssertion, TraceAssertionStatus, TraceResults};

const BASELINE_KEY: &str = "trace";
pub const DEFAULT_REGRESSION_THRESHOLD_PERCENT: f64 = 5.0;
pub const DEFAULT_REGRESSION_MIN_DELTA_MS: u64 = 50;

fn baseline_key_for(rig_id: Option<&str>) -> String {
    rig_id.map_or_else(|| BASELINE_KEY.to_string(), |id| format!("trace.rig.{id}"))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceSpanSnapshot {
    pub id: String,
    pub duration_ms: u64,
}

impl generic::Fingerprintable for TraceSpanSnapshot {
    fn fingerprint(&self) -> String {
        format!("trace-span:{}", self.id)
    }

    fn description(&self) -> String {
        format!("{}ms", self.duration_ms)
    }

    fn context_label(&self) -> String {
        format!("trace span {}", self.id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceAssertionSnapshot {
    pub id: String,
    pub status: TraceAssertionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub metric: Option<f64>,
}

impl generic::Fingerprintable for TraceAssertionSnapshot {
    fn fingerprint(&self) -> String {
        format!("trace-assertion:{}", self.id)
    }

    fn description(&self) -> String {
        match self.metric {
            Some(metric) => format!("{:?} ({metric:.2})", self.status),
            None => format!("{:?}", self.status),
        }
    }

    fn context_label(&self) -> String {
        format!("trace assertion {}", self.id)
    }
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

pub fn save_baseline(
    source_path: &Path,
    component_id: &str,
    results: &TraceResults,
    rig_id: Option<&str>,
) -> Result<std::path::PathBuf> {
    let snapshots = snapshots_from_results(results);
    let assertion_snapshots = assertion_snapshots_from_results(results);
    let mut items = Vec::with_capacity(snapshots.len() + assertion_snapshots.len());
    items.extend(snapshots.iter().map(TraceBaselineItem::Span));
    items.extend(assertion_snapshots.iter().map(TraceBaselineItem::Assertion));
    let metadata = TraceBaselineMetadata {
        spans: snapshots.clone(),
        assertions: assertion_snapshots.clone(),
    };
    let key = baseline_key_for(rig_id);
    let config = BaselineConfig::new(source_path, key);
    generic::save(&config, component_id, &items, metadata)
}

pub fn load_baseline(source_path: &Path, rig_id: Option<&str>) -> Option<TraceBaseline> {
    let key = baseline_key_for(rig_id);
    let config = BaselineConfig::new(source_path, key);
    generic::load::<TraceBaselineMetadata>(&config).unwrap_or_default()
}

pub fn compare(
    current: &TraceResults,
    baseline: &TraceBaseline,
    threshold_percent: f64,
    min_delta_ms: u64,
) -> TraceBaselineComparison {
    let current_snapshots = snapshots_from_results(current);
    let baseline_by_id: HashMap<&str, &TraceSpanSnapshot> = baseline
        .metadata
        .spans
        .iter()
        .map(|span| (span.id.as_str(), span))
        .collect();
    let current_by_id: HashMap<&str, &TraceSpanSnapshot> = current_snapshots
        .iter()
        .map(|span| (span.id.as_str(), span))
        .collect();
    let current_assertion_snapshots = assertion_snapshots_from_results(current);
    let baseline_assertions_by_id: HashMap<&str, &TraceAssertionSnapshot> = baseline
        .metadata
        .assertions
        .iter()
        .map(|assertion| (assertion.id.as_str(), assertion))
        .collect();
    let current_assertions_by_id: HashMap<&str, &TraceAssertionSnapshot> =
        current_assertion_snapshots
            .iter()
            .map(|assertion| (assertion.id.as_str(), assertion))
            .collect();

    let mut spans = Vec::new();
    let mut assertions = Vec::new();
    let mut new_span_ids = Vec::new();
    let mut new_assertion_ids = Vec::new();
    let mut reasons = Vec::new();
    let mut regression = false;
    let mut has_improvements = false;

    for current_span in &current_snapshots {
        let Some(prior) = baseline_by_id.get(current_span.id.as_str()) else {
            new_span_ids.push(current_span.id.clone());
            continue;
        };
        let delta_ms = current_span.duration_ms as i64 - prior.duration_ms as i64;
        let delta_pct = if prior.duration_ms == 0 {
            None
        } else {
            Some((delta_ms as f64 / prior.duration_ms as f64) * 100.0)
        };
        let span_regression =
            delta_ms > min_delta_ms as i64 && delta_pct.is_some_and(|pct| pct > threshold_percent);
        let span_improvement = delta_ms < 0;
        if span_regression {
            regression = true;
            reasons.push(format!(
                "{} regressed by {}ms ({:.2}%)",
                current_span.id,
                delta_ms,
                delta_pct.unwrap_or_default()
            ));
        }
        if span_improvement {
            has_improvements = true;
        }
        spans.push(TraceSpanDelta {
            id: current_span.id.clone(),
            baseline_duration_ms: prior.duration_ms,
            current_duration_ms: current_span.duration_ms,
            delta_ms,
            delta_pct,
            regression: span_regression,
            improvement: span_improvement,
        });
    }

    for current_assertion in &current_assertion_snapshots {
        let Some(prior) = baseline_assertions_by_id.get(current_assertion.id.as_str()) else {
            new_assertion_ids.push(current_assertion.id.clone());
            continue;
        };
        let metric_delta = current_assertion
            .metric
            .zip(prior.metric)
            .map(|(current, baseline)| current - baseline);
        let status_regression = prior.status == TraceAssertionStatus::Pass
            && current_assertion.status != TraceAssertionStatus::Pass;
        let metric_regression = metric_delta.is_some_and(|delta| delta > 0.0);
        let assertion_regression = status_regression || metric_regression;
        let assertion_improvement = (prior.status != TraceAssertionStatus::Pass
            && current_assertion.status == TraceAssertionStatus::Pass)
            || metric_delta.is_some_and(|delta| delta < 0.0);
        if assertion_regression {
            regression = true;
            if status_regression {
                reasons.push(format!(
                    "{} assertion regressed from {:?} to {:?}",
                    current_assertion.id, prior.status, current_assertion.status
                ));
            } else if let Some(delta) = metric_delta {
                reasons.push(format!(
                    "{} assertion metric regressed by {:.2}",
                    current_assertion.id, delta
                ));
            }
        }
        if assertion_improvement {
            has_improvements = true;
        }
        assertions.push(TraceAssertionDelta {
            id: current_assertion.id.clone(),
            baseline_status: prior.status.clone(),
            current_status: current_assertion.status.clone(),
            baseline_metric: prior.metric,
            current_metric: current_assertion.metric,
            metric_delta,
            regression: assertion_regression,
            improvement: assertion_improvement,
        });
    }

    let removed_span_ids = baseline
        .metadata
        .spans
        .iter()
        .filter(|span| !current_by_id.contains_key(span.id.as_str()))
        .map(|span| span.id.clone())
        .collect();
    let removed_assertion_ids = baseline
        .metadata
        .assertions
        .iter()
        .filter(|assertion| !current_assertions_by_id.contains_key(assertion.id.as_str()))
        .map(|assertion| assertion.id.clone())
        .collect();

    TraceBaselineComparison {
        threshold_percent,
        min_delta_ms,
        spans,
        assertions,
        new_span_ids,
        removed_span_ids,
        new_assertion_ids,
        removed_assertion_ids,
        regression,
        has_improvements,
        reasons,
    }
}

fn snapshots_from_results(results: &TraceResults) -> Vec<TraceSpanSnapshot> {
    results
        .span_results
        .iter()
        .filter_map(|span| {
            span.duration_ms.map(|duration_ms| TraceSpanSnapshot {
                id: span.id.clone(),
                duration_ms,
            })
        })
        .collect()
}

fn assertion_snapshots_from_results(results: &TraceResults) -> Vec<TraceAssertionSnapshot> {
    results
        .assertions
        .iter()
        .map(|assertion| TraceAssertionSnapshot {
            id: assertion.id.clone(),
            status: assertion.status.clone(),
            metric: assertion_metric(assertion),
        })
        .collect()
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

enum TraceBaselineItem<'a> {
    Span(&'a TraceSpanSnapshot),
    Assertion(&'a TraceAssertionSnapshot),
}

impl generic::Fingerprintable for TraceBaselineItem<'_> {
    fn fingerprint(&self) -> String {
        match self {
            TraceBaselineItem::Span(span) => span.fingerprint(),
            TraceBaselineItem::Assertion(assertion) => assertion.fingerprint(),
        }
    }

    fn description(&self) -> String {
        match self {
            TraceBaselineItem::Span(span) => span.description(),
            TraceBaselineItem::Assertion(assertion) => assertion.description(),
        }
    }

    fn context_label(&self) -> String {
        match self {
            TraceBaselineItem::Span(span) => span.context_label(),
            TraceBaselineItem::Assertion(assertion) => assertion.context_label(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::trace::parsing::{
        TraceAssertion, TraceSpanResult, TraceSpanStatus, TraceStatus,
    };

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
