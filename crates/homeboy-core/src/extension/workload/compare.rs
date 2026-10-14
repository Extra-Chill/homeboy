//! The one generic compare/gate adapter for every extension-declared kind.
//!
//! Driven entirely by the kind's declared `compare` section: metrics with a
//! direction and an optional percent threshold, and the findings key. A kind
//! adds compare semantics by declaring metrics, not by adding an adapter.

use std::collections::{BTreeMap, BTreeSet};

use homeboy_engine_primitives::workload_compare::{KindAdapter, Verdict};
use homeboy_extension_contract::workload_result::finding_key;
use homeboy_extension_contract::{
    WorkloadCompareConfig, WorkloadFindingsKey, WorkloadMetricDirection, WorkloadResult,
    WorkloadStatus,
};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// The generic adapter. Its static `KIND` is only a placeholder: callers
/// store baselines through `BaselineSlot::with_kind(&baseline_key(kind))`.
pub struct WorkloadAdapter;

/// The comparable projection of one envelope; also the stored baseline.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkloadSnapshot {
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    pub status: WorkloadStatus,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    /// Finding key -> severity (empty when the finding has none).
    #[serde(default)]
    pub findings: BTreeMap<String, String>,
    /// Case id -> status.
    #[serde(default)]
    pub cases: BTreeMap<String, WorkloadStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MetricChange {
    Regressed,
    Improved,
    Unchanged,
    /// Candidate reports the metric, baseline does not.
    Added,
    /// Baseline reports the metric, candidate does not.
    Missing,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MetricDelta {
    pub name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub delta: Option<f64>,
    /// Change in percent of the baseline; absent when the baseline is zero.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub percent: Option<f64>,
    /// Declared direction; absent for undeclared (report-only) metrics.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub direction: Option<WorkloadMetricDirection>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub regression_threshold: Option<f64>,
    pub change: MetricChange,
    /// Whether this metric is declared and so participates in the gate.
    pub gated: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FindingsDelta {
    pub new: Vec<String>,
    pub resolved: Vec<String>,
    pub persisting: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CasesDelta {
    pub added: Vec<String>,
    pub removed: Vec<String>,
    pub newly_failing: Vec<String>,
    pub newly_passing: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkloadDelta {
    pub kind: String,
    pub baseline_status: WorkloadStatus,
    pub candidate_status: WorkloadStatus,
    pub metrics: Vec<MetricDelta>,
    pub findings: FindingsDelta,
    pub cases: CasesDelta,
}

pub fn snapshot_with_key(result: &WorkloadResult, key: WorkloadFindingsKey) -> WorkloadSnapshot {
    WorkloadSnapshot {
        kind: result.kind.clone(),
        workload_id: result.workload_id.clone(),
        status: result.status,
        metrics: result.metrics.clone(),
        findings: result
            .findings
            .iter()
            .map(|finding| {
                (
                    finding_key(finding, key),
                    finding.severity.clone().unwrap_or_default(),
                )
            })
            .collect(),
        cases: result
            .cases
            .iter()
            .map(|case| (case.case_id.clone(), case.status))
            .collect(),
    }
}

fn metric_change(
    direction: WorkloadMetricDirection,
    threshold: Option<f64>,
    baseline: f64,
    candidate: f64,
) -> MetricChange {
    let delta = candidate - baseline;
    if delta == 0.0 {
        return MetricChange::Unchanged;
    }
    if let Some(threshold) = threshold {
        let beyond = if baseline == 0.0 {
            true
        } else {
            (delta / baseline.abs()).abs() * 100.0 > threshold
        };
        if !beyond {
            return MetricChange::Unchanged;
        }
    }
    let worse = match direction {
        WorkloadMetricDirection::LowerIsBetter => delta > 0.0,
        WorkloadMetricDirection::HigherIsBetter => delta < 0.0,
    };
    if worse {
        MetricChange::Regressed
    } else {
        MetricChange::Improved
    }
}

fn metric_deltas(
    baseline: &WorkloadSnapshot,
    candidate: &WorkloadSnapshot,
    config: &WorkloadCompareConfig,
) -> Vec<MetricDelta> {
    let declared = config
        .metrics
        .iter()
        .map(|metric| (metric.name.as_str(), metric))
        .collect::<BTreeMap<_, _>>();
    let names = baseline
        .metrics
        .keys()
        .chain(candidate.metrics.keys())
        .map(String::as_str)
        .chain(declared.keys().copied())
        .collect::<BTreeSet<_>>();
    names
        .into_iter()
        .filter_map(|name| {
            let policy = declared.get(name).copied();
            let before = baseline.metrics.get(name).copied();
            let after = candidate.metrics.get(name).copied();
            let change = match (before, after) {
                (None, None) => return None,
                (None, Some(_)) => MetricChange::Added,
                (Some(_), None) => MetricChange::Missing,
                (Some(before), Some(after)) => match policy {
                    Some(policy) => {
                        metric_change(policy.direction, policy.regression_threshold, before, after)
                    }
                    // Undeclared metrics are reported, never judged.
                    None => MetricChange::Unchanged,
                },
            };
            let delta = before.zip(after).map(|(before, after)| after - before);
            Some(MetricDelta {
                name: name.to_string(),
                baseline: before,
                candidate: after,
                delta,
                percent: before
                    .zip(delta)
                    .filter(|(before, _)| *before != 0.0)
                    .map(|(before, delta)| delta / before.abs() * 100.0),
                direction: policy.map(|policy| policy.direction),
                regression_threshold: policy.and_then(|policy| policy.regression_threshold),
                change,
                gated: policy.is_some(),
            })
        })
        .collect()
}

impl KindAdapter for WorkloadAdapter {
    const KIND: &'static str = "workload";
    type Source = WorkloadResult;
    type Snapshot = WorkloadSnapshot;
    type Options = WorkloadCompareConfig;
    type Delta = WorkloadDelta;

    /// `fingerprint` is the only findings key, so the snapshot does not need
    /// the options to key findings.
    fn snapshot(source: &WorkloadResult) -> WorkloadSnapshot {
        snapshot_with_key(source, WorkloadFindingsKey::Fingerprint)
    }

    fn delta(
        baseline: &WorkloadSnapshot,
        candidate: &WorkloadSnapshot,
        config: &WorkloadCompareConfig,
    ) -> WorkloadDelta {
        let mut findings = FindingsDelta::default();
        for key in candidate.findings.keys() {
            if baseline.findings.contains_key(key) {
                findings.persisting += 1;
            } else {
                findings.new.push(key.clone());
            }
        }
        findings.resolved = baseline
            .findings
            .keys()
            .filter(|key| !candidate.findings.contains_key(*key))
            .cloned()
            .collect();

        let mut cases = CasesDelta::default();
        for (case_id, status) in &candidate.cases {
            match baseline.cases.get(case_id) {
                None => cases.added.push(case_id.clone()),
                Some(prior) if prior.is_pass() && !status.is_pass() => {
                    cases.newly_failing.push(case_id.clone())
                }
                Some(prior) if !prior.is_pass() && status.is_pass() => {
                    cases.newly_passing.push(case_id.clone())
                }
                Some(_) => {}
            }
        }
        cases.removed = baseline
            .cases
            .keys()
            .filter(|case_id| !candidate.cases.contains_key(*case_id))
            .cloned()
            .collect();

        WorkloadDelta {
            kind: candidate.kind.clone(),
            baseline_status: baseline.status,
            candidate_status: candidate.status,
            metrics: metric_deltas(baseline, candidate, config),
            findings,
            cases,
        }
    }

    /// - regressions (block): a declared metric regressed or went missing, a
    ///   new finding, or the run status went from passing to not passing;
    /// - advisories: cases that newly fail (their cause is reported as a
    ///   finding when the kind considers it one);
    /// - improvements (ratchet): a declared metric improved, a finding was
    ///   resolved, or the status recovered.
    fn gate(delta: &WorkloadDelta) -> Verdict {
        let mut verdict = Verdict::default();
        for metric in delta.metrics.iter().filter(|metric| metric.gated) {
            match metric.change {
                MetricChange::Regressed => verdict
                    .regressions
                    .push(format!("metric `{}` regressed", metric.name)),
                MetricChange::Missing => verdict
                    .regressions
                    .push(format!("metric `{}` is no longer reported", metric.name)),
                MetricChange::Improved => verdict
                    .improvements
                    .push(format!("metric `{}` improved", metric.name)),
                MetricChange::Unchanged | MetricChange::Added => {}
            }
        }
        verdict.regressions.extend(
            delta
                .findings
                .new
                .iter()
                .map(|key| format!("new finding `{key}`")),
        );
        verdict.improvements.extend(
            delta
                .findings
                .resolved
                .iter()
                .map(|key| format!("resolved finding `{key}`")),
        );
        if delta.baseline_status.is_pass() && !delta.candidate_status.is_pass() {
            verdict.regressions.push(format!(
                "status went from {} to {}",
                delta.baseline_status.as_str(),
                delta.candidate_status.as_str()
            ));
        } else if !delta.baseline_status.is_pass() && delta.candidate_status.is_pass() {
            verdict.improvements.push(format!(
                "status recovered from {} to {}",
                delta.baseline_status.as_str(),
                delta.candidate_status.as_str()
            ));
        }
        verdict.advisories.extend(
            delta
                .cases
                .newly_failing
                .iter()
                .map(|case_id| format!("case `{case_id}` newly fails")),
        );
        verdict
    }

    fn fingerprints(snapshot: &WorkloadSnapshot) -> Vec<String> {
        snapshot.findings.keys().cloned().collect()
    }

    fn comparable(baseline: &WorkloadSnapshot, candidate: &WorkloadSnapshot) -> Result<()> {
        if baseline.kind != candidate.kind {
            return Err(Error::validation_invalid_argument(
                "kind",
                format!(
                    "cannot compare a `{}` result with a `{}` result",
                    baseline.kind, candidate.kind
                ),
                None,
                None,
            ));
        }
        if let (Some(before), Some(after)) = (&baseline.workload_id, &candidate.workload_id) {
            if before != after {
                return Err(Error::validation_invalid_argument(
                    "workload_id",
                    format!("cannot compare workload `{before}` with workload `{after}`"),
                    None,
                    None,
                ));
            }
        }
        Ok(())
    }
}

/// The JSON a compare or baseline gate reports.
#[derive(Debug, Clone, Serialize)]
pub struct WorkloadComparison {
    pub status: &'static str,
    pub regressions: Vec<String>,
    pub advisories: Vec<String>,
    pub improvements: Vec<String>,
    pub delta: WorkloadDelta,
}

impl WorkloadComparison {
    pub fn new(delta: WorkloadDelta) -> Self {
        let verdict = WorkloadAdapter::gate(&delta);
        Self {
            status: verdict.status(),
            regressions: verdict.regressions,
            advisories: verdict.advisories,
            improvements: verdict.improvements,
            delta,
        }
    }

    pub fn regressed(&self) -> bool {
        !self.regressions.is_empty()
    }
}
