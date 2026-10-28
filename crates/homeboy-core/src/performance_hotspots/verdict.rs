//! Conservative regression policy for read-only version cohorts.
use super::{PhaseHotspotQuery, PhaseHotspots};
use crate::observation::{ObservationStore, RunListFilter};
use serde::Serialize;

#[derive(Debug, Clone, PartialEq)]
pub struct RegressionThresholds {
    pub phase_relative: f64,
    pub phase_floor_ms: f64,
    pub phase_samples: usize,
    pub failure_rate: f64,
    pub missions: usize,
}
impl Default for RegressionThresholds {
    fn default() -> Self {
        Self {
            phase_relative: 0.25,
            phase_floor_ms: 5000.0,
            phase_samples: 10,
            failure_rate: 0.05,
            missions: 20,
        }
    }
}
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Regressed,
    Clean,
    InsufficientSamples,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Regression {
    pub metric: String,
    pub baseline: f64,
    pub candidate: f64,
    pub baseline_samples: usize,
    pub candidate_samples: usize,
}
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RegressionComparison {
    pub baseline_version: Option<String>,
    pub candidate_version: Option<String>,
    pub verdict: Verdict,
    pub regressions: Vec<Regression>,
}

pub(super) fn evaluate(
    report: &PhaseHotspots,
    query: &PhaseHotspotQuery,
    t: &RegressionThresholds,
) -> RegressionComparison {
    let mut evidence = Vec::new();
    let mut enough = false;
    for phase in &report.phases {
        let Some((b, c)) = phase.baseline.as_ref().zip(phase.candidate.as_ref()) else {
            continue;
        };
        if b.samples < t.phase_samples || c.samples < t.phase_samples {
            continue;
        }
        enough = true;
        for (metric, baseline, candidate) in [
            ("p50_ms", b.p50_ms, c.p50_ms),
            ("p95_ms", b.p95_ms, c.p95_ms),
        ] {
            let delta = candidate - baseline;
            if delta > 0.0 && delta >= t.phase_floor_ms && delta >= baseline * t.phase_relative {
                evidence.push(Regression {
                    metric: format!("phase.{}.{}", phase.phase, metric),
                    baseline,
                    candidate,
                    baseline_samples: b.samples,
                    candidate_samples: c.samples,
                });
            }
        }
    }
    let b = report
        .reliability
        .versions
        .iter()
        .find(|v| Some(&v.homeboy_version) == query.baseline_version.as_ref());
    let c = report
        .reliability
        .versions
        .iter()
        .find(|v| Some(&v.homeboy_version) == query.candidate_version.as_ref());
    if let Some((b, c)) = b
        .zip(c)
        .filter(|(b, c)| b.runs >= t.missions && c.runs >= t.missions)
    {
        enough = true;
        for (metric, baseline, candidate, threshold) in [
            (
                "infrastructure_failure_rate",
                b.infrastructure_failure_rate,
                c.infrastructure_failure_rate,
                t.failure_rate,
            ),
            // A rate, not a count: cohorts differ in size.
            (
                "unclassified_failure_rate",
                b.unclassified_failures as f64 / b.runs as f64,
                c.unclassified_failures as f64 / c.runs as f64,
                t.failure_rate,
            ),
        ] {
            let delta = candidate - baseline;
            if delta > 0.0 && delta + f64::EPSILON >= threshold {
                evidence.push(Regression {
                    metric: metric.into(),
                    baseline,
                    candidate,
                    baseline_samples: b.runs,
                    candidate_samples: c.runs,
                });
            }
        }
    }
    RegressionComparison {
        baseline_version: query.baseline_version.clone(),
        candidate_version: query.candidate_version.clone(),
        verdict: if !evidence.is_empty() {
            Verdict::Regressed
        } else if enough {
            Verdict::Clean
        } else {
            Verdict::InsufficientSamples
        },
        regressions: evidence,
    }
}

pub fn compare_phase_hotspots(
    store: &ObservationStore,
    query: &PhaseHotspotQuery,
    thresholds: &RegressionThresholds,
) -> crate::Result<PhaseHotspots> {
    let mut query = query.clone();
    if query.candidate_version.as_deref() == Some("current") {
        query.candidate_version = Some(homeboy_product_identity::product_version().into());
    }
    if query.baseline_version.as_deref() == Some("previous") {
        query.baseline_version = None;
        let runs = store.list_runs_best_effort(RunListFilter {
            kind: query.kind.clone(),
            component_id: query.component.clone(),
            limit: Some(query.last.clamp(1, 1000) as i64),
            ..Default::default()
        })?;
        let candidate = query
            .candidate_version
            .as_deref()
            .and_then(|v| semver::Version::parse(v).ok());
        let mut versions = Vec::new();
        for run in runs {
            let mut recorded = vec![run.homeboy_version.unwrap_or_default()];
            match store.phase_spans(&run.id) {
                Ok(report) => recorded.extend(report.spans.into_iter().map(|s| s.homeboy_version)),
                Err(error) => {
                    crate::log_status!("runs", "skipping malformed phase row {}: {}", run.id, error)
                }
            }
            for version in recorded {
                // A baseline precedes the candidate when both are semantic versions.
                let newer = semver::Version::parse(&version)
                    .ok()
                    .zip(candidate.clone())
                    .is_some_and(|(version, candidate)| version >= candidate);
                if !version.is_empty()
                    && !newer
                    && Some(&version) != query.candidate_version.as_ref()
                    && !versions.contains(&version)
                {
                    versions.push(version);
                }
            }
        }
        versions.sort_by(
            |a, b| match (semver::Version::parse(a), semver::Version::parse(b)) {
                (Ok(a), Ok(b)) => b.cmp(&a),
                (Ok(_), Err(_)) => std::cmp::Ordering::Less,
                (Err(_), Ok(_)) => std::cmp::Ordering::Greater,
                (Err(_), Err(_)) => std::cmp::Ordering::Equal,
            },
        );
        for version in versions {
            query.baseline_version = Some(version);
            let report = super::phase_spans::phase_hotspots_raw(store, &query)?;
            if report.phases.iter().any(|p| {
                p.baseline
                    .as_ref()
                    .is_some_and(|b| b.samples >= thresholds.phase_samples)
            }) || report.reliability.versions.iter().any(|v| {
                Some(&v.homeboy_version) == query.baseline_version.as_ref()
                    && v.runs >= thresholds.missions
            }) {
                break;
            }
            query.baseline_version = None;
        }
    }
    let mut report = super::phase_spans::phase_hotspots_raw(store, &query)?;
    if query.candidate_version.is_some() {
        report.comparison = Some(evaluate(&report, &query, thresholds));
    }
    Ok(report)
}
