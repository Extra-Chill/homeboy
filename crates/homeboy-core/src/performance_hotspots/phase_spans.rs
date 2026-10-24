//! Bounded, read-only phase projection over the canonical local run ledger.
use std::collections::BTreeMap;

use serde::Serialize;

use crate::observation::{runs_service, ObservationStore, RunListFilter};
use crate::Result;
use homeboy_engine_primitives::phase_timing::PhaseStatus;

#[derive(Debug, Clone, Default)]
pub struct PhaseHotspotQuery {
    pub kind: Option<String>,
    pub component: Option<String>,
    pub homeboy_version: Option<String>,
    pub baseline_version: Option<String>,
    pub candidate_version: Option<String>,
    pub last: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhasePercentiles {
    pub samples: usize,
    pub p50_ms: f64,
    pub p95_ms: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseHotspot {
    pub phase: String,
    pub timing: PhasePercentiles,
    pub failed: usize,
    pub skipped: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub baseline: Option<PhasePercentiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub candidate: Option<PhasePercentiles>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p50_delta_ms: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub p95_delta_ms: Option<f64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseHotspots {
    pub run_ids: Vec<String>,
    pub runs_with_spans: usize,
    /// Runs lacking spans are not zero-duration samples.
    pub runs_without_spans: usize,
    pub phases: Vec<PhaseHotspot>,
}

#[derive(Default)]
struct Samples {
    all: Vec<f64>,
    baseline: Vec<f64>,
    candidate: Vec<f64>,
    failed: usize,
    skipped: usize,
}

fn percentiles(mut values: Vec<f64>) -> Option<PhasePercentiles> {
    if values.is_empty() {
        return None;
    }
    values.sort_by(f64::total_cmp);
    // Nearest-rank percentiles, including both endpoints for singleton sets.
    let rank = |fraction: f64| ((values.len() as f64 * fraction).ceil() as usize).saturating_sub(1);
    Some(PhasePercentiles {
        samples: values.len(),
        p50_ms: values[rank(0.50)],
        p95_ms: values[rank(0.95)],
    })
}

/// Scan at most 1000 recent matching rows. This deliberately does not use
/// `require_run` (which can hydrate remote evidence). Kind/component filtering
/// is pushed into the store; version selection uses each span's recorder.
pub fn phase_hotspots(
    store: &ObservationStore,
    query: &PhaseHotspotQuery,
) -> Result<PhaseHotspots> {
    let runs = store.list_runs(RunListFilter {
        kind: query.kind.clone(),
        component_id: query.component.clone(),
        limit: Some(query.last.clamp(1, 1000) as i64),
        ..Default::default()
    })?;
    let runs = runs_service::dedupe_runner_execution_mirrors(runs).canonical;
    let mut groups: BTreeMap<String, Samples> = BTreeMap::new();
    let mut runs_with_spans = 0;
    let run_ids = runs.iter().map(|run| run.id.clone()).collect::<Vec<_>>();
    for run in runs {
        let report = store.phase_spans(&run.id)?;
        let mut has_spans = false;
        for span in report.spans {
            let version = if span.homeboy_version.is_empty() {
                run.homeboy_version.as_deref().unwrap_or_default()
            } else {
                &span.homeboy_version
            };
            if query
                .homeboy_version
                .as_deref()
                .is_some_and(|filter| filter != version)
            {
                continue;
            }
            if chrono::DateTime::parse_from_rfc3339(&span.started_at).is_err() {
                continue;
            }
            has_spans = true;
            let group = groups.entry(span.id).or_default();
            if span.status == PhaseStatus::Skipped {
                group.skipped += 1;
                continue;
            }
            let Some(ms) = span.duration_ms.filter(|ms| ms.is_finite() && *ms >= 0.0) else {
                continue;
            };
            group.all.push(ms);
            if span.status == PhaseStatus::Failed {
                group.failed += 1;
            }
            if query.baseline_version.as_deref() == Some(version) {
                group.baseline.push(ms);
            }
            if query.candidate_version.as_deref() == Some(version) {
                group.candidate.push(ms);
            }
        }
        if has_spans {
            runs_with_spans += 1;
        }
    }
    let mut phases = groups
        .into_iter()
        .filter_map(|(phase, samples)| {
            let timing = percentiles(samples.all)?;
            let baseline = percentiles(samples.baseline);
            let candidate = percentiles(samples.candidate);
            let p50_delta_ms = baseline
                .as_ref()
                .zip(candidate.as_ref())
                .map(|(b, c)| c.p50_ms - b.p50_ms);
            let p95_delta_ms = baseline
                .as_ref()
                .zip(candidate.as_ref())
                .map(|(b, c)| c.p95_ms - b.p95_ms);
            Some(PhaseHotspot {
                phase,
                timing,
                failed: samples.failed,
                skipped: samples.skipped,
                baseline,
                candidate,
                p50_delta_ms,
                p95_delta_ms,
            })
        })
        .collect::<Vec<_>>();
    phases.sort_by(|a, b| {
        b.timing
            .p95_ms
            .total_cmp(&a.timing.p95_ms)
            .then(a.phase.cmp(&b.phase))
    });
    Ok(PhaseHotspots {
        runs_without_spans: run_ids.len() - runs_with_spans,
        run_ids,
        runs_with_spans,
        phases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{NewRunRecord, RunStatus};
    use homeboy_engine_primitives::phase_timing::PhaseTimer;
    use std::time::Duration;

    #[test]
    fn performance_hotspots_scoped_run_fixture_records_failure_and_offset() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("observations.sqlite");
        let store = ObservationStore::open_initialized_at(&path).unwrap();
        let run = store
            .start_run(NewRunRecord::builder("agent-task").build())
            .unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let result: crate::Result<()> = store.time_run_phase(&run.id, "stage.transfer", || {
            Err(crate::Error::internal_unexpected("transfer fixture failed"))
        });
        assert!(result.is_err());
        let spans = store.phase_spans(&run.id).unwrap();
        assert_eq!(spans.spans.len(), 1);
        assert_eq!(spans.spans[0].status, PhaseStatus::Failed);
        assert!(
            chrono::DateTime::parse_from_rfc3339(&spans.spans[0].started_at).unwrap()
                >= chrono::DateTime::parse_from_rfc3339(&run.started_at).unwrap()
        );
        store
            .time_run_phase_with_outcome(&run.id, "gate", || Ok(false), |success| !success)
            .unwrap();
        let summary = phase_hotspots(
            &store,
            &PhaseHotspotQuery {
                last: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert!(summary.phases.iter().all(|phase| phase.failed == 1));
    }

    #[test]
    fn performance_hotspots_real_run_ledger_emission_and_version_comparison() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("observations.sqlite");
        let store = ObservationStore::open_initialized_at(&path).unwrap();
        for (version, duration) in [("before", 100), ("after", 20)] {
            let run = store
                .start_run(
                    NewRunRecord::builder("agent-task")
                        .component_id("engine")
                        .build(),
                )
                .unwrap();
            let mut timer = PhaseTimer::with_version(version);
            timer.record_ok("admit", Duration::from_millis(duration));
            timer.record_ok("stage.transfer", Duration::from_millis(duration / 2));
            timer.record_failed("provider", Duration::from_millis(duration * 2));
            timer.record_skipped("publish");
            let report = timer.into_report();
            store
                .record_phase_spans(&run.id, "attempt-1", &report)
                .unwrap();
            store
                .record_phase_spans(&run.id, "attempt-1", &report)
                .unwrap();
            store.finish_run(&run.id, RunStatus::Pass, None).unwrap();
        }
        drop(store);
        let reader = ObservationStore::open_readonly_at(&path).unwrap();
        let summary = phase_hotspots(
            &reader,
            &PhaseHotspotQuery {
                kind: Some("agent-task".into()),
                component: Some("engine".into()),
                baseline_version: Some("before".into()),
                candidate_version: Some("after".into()),
                last: 10,
                ..Default::default()
            },
        )
        .unwrap();
        assert_eq!(summary.runs_with_spans, 2);
        let admit = summary
            .phases
            .iter()
            .find(|phase| phase.phase == "admit")
            .unwrap();
        assert_eq!(admit.timing.samples, 2);
        assert_eq!(admit.timing.p50_ms, 20.0);
        assert_eq!(admit.timing.p95_ms, 100.0);
        assert_eq!(admit.p95_delta_ms, Some(-80.0));
        assert!(!summary.phases.iter().any(|phase| phase.phase == "publish"));
    }
}
