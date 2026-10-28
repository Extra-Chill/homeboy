use super::*;
use crate::observation::{NewRunRecord, ObservationStore, RunStatus};
use homeboy_engine_primitives::phase_timing::PhaseTimer;
use serde_json::json;
use std::time::Duration;

fn cohort(store: &ObservationStore, version: &str, n: usize, ms: u64, failures: usize) {
    for i in 0..n {
        let run = store.start_run(NewRunRecord::builder("agent-task")
            .component_id("engine").homeboy_version(version)
            .metadata(json!({"agent_task_run":{"metadata":{"cook_controller_failure":{"message":"transport disconnected"}}}}))
            .build()).unwrap();
        let mut timer = PhaseTimer::with_version(version);
        timer.record_ok("cook", Duration::from_millis(ms));
        store
            .record_phase_spans(&run.id, "attempt", &timer.into_report())
            .unwrap();
        store
            .finish_run(
                &run.id,
                if i < failures {
                    RunStatus::Fail
                } else {
                    RunStatus::Pass
                },
                None,
            )
            .unwrap();
    }
}
fn query() -> PhaseHotspotQuery {
    PhaseHotspotQuery {
        kind: Some("agent-task".into()),
        component: Some("engine".into()),
        baseline_version: Some("0.1.0".into()),
        candidate_version: Some("0.2.0".into()),
        last: 1000,
        ..Default::default()
    }
}
fn store() -> (tempfile::TempDir, ObservationStore) {
    let dir = tempfile::tempdir().unwrap();
    let store = ObservationStore::open_initialized_at(dir.path().join("runs.sqlite")).unwrap();
    (dir, store)
}
#[test]
fn clearly_slower_synthetic_store_has_evidence() {
    let (dir, writer) = store();
    cohort(&writer, "0.1.0", 10, 20_000, 0);
    cohort(&writer, "0.2.0", 10, 30_000, 0);
    drop(writer);
    let reader = ObservationStore::open_readonly_at(dir.path().join("runs.sqlite")).unwrap();
    let comparison = phase_hotspots(&reader, &query())
        .unwrap()
        .comparison
        .unwrap();
    assert_eq!(comparison.verdict, Verdict::Regressed);
    assert_eq!(comparison.regressions.len(), 2);
    let evidence = &comparison.regressions[0];
    assert_eq!(evidence.metric, "phase.cook.p50_ms");
    assert_eq!(
        (evidence.baseline, evidence.candidate),
        (20_000.0, 30_000.0)
    );
    assert_eq!(
        (evidence.baseline_samples, evidence.candidate_samples),
        (10, 10)
    );
}
#[test]
fn workload_noise_and_absolute_floor_are_clean() {
    for (baseline, candidate) in [(100_000, 120_000), (1000, 4000), (20_000, 19_000)] {
        let (_dir, store) = store();
        cohort(&store, "0.1.0", 10, baseline, 0);
        cohort(&store, "0.2.0", 10, candidate, 0);
        let comparison = phase_hotspots(&store, &query())
            .unwrap()
            .comparison
            .unwrap();
        assert_eq!(comparison.verdict, Verdict::Clean);
        assert!(comparison.regressions.is_empty());
    }
}
#[test]
fn small_or_missing_cohorts_are_insufficient() {
    let (_dir, store) = store();
    cohort(&store, "0.1.0", 9, 20_000, 0);
    cohort(&store, "0.2.0", 9, 90_000, 9);
    assert_eq!(
        phase_hotspots(&store, &query())
            .unwrap()
            .comparison
            .unwrap()
            .verdict,
        Verdict::InsufficientSamples
    );
    let mut missing = query();
    missing.baseline_version = Some("absent".into());
    assert_eq!(
        phase_hotspots(&store, &missing)
            .unwrap()
            .comparison
            .unwrap()
            .verdict,
        Verdict::InsufficientSamples
    );
}
#[test]
fn reliability_rate_rise_at_boundary_is_detected() {
    let (_dir, store) = store();
    cohort(&store, "0.1.0", 20, 20_000, 1);
    cohort(&store, "0.2.0", 20, 20_000, 2);
    let comparison = phase_hotspots(&store, &query())
        .unwrap()
        .comparison
        .unwrap();
    assert_eq!(comparison.verdict, Verdict::Regressed);
    assert_eq!(comparison.regressions.len(), 1);
    assert_eq!(
        comparison.regressions[0].metric,
        "infrastructure_failure_rate"
    );
    assert_eq!(comparison.regressions[0].baseline_samples, 20);
}
#[test]
fn current_previous_choose_newest_eligible_scoped_version() {
    let (_dir, store) = store();
    cohort(&store, "0.2.0", 10, 20_000, 0);
    cohort(&store, "0.3.0", 9, 20_000, 0);
    cohort(&store, "0.1.0", 10, 20_000, 0);
    cohort(
        &store,
        homeboy_product_identity::product_version(),
        10,
        30_000,
        0,
    );
    let mut q = query();
    q.baseline_version = Some("previous".into());
    q.candidate_version = Some("current".into());
    let comparison = phase_hotspots(&store, &q).unwrap().comparison.unwrap();
    assert_eq!(comparison.baseline_version.as_deref(), Some("0.2.0"));
    assert_eq!(
        comparison.candidate_version.as_deref(),
        Some(homeboy_product_identity::product_version())
    );
    assert_eq!(comparison.verdict, Verdict::Regressed);
    q.component = Some("other-component".into());
    let comparison = phase_hotspots(&store, &q).unwrap().comparison.unwrap();
    assert_eq!(comparison.baseline_version, None);
    assert_eq!(comparison.verdict, Verdict::InsufficientSamples);
}
#[test]
fn malformed_phase_rows_are_logged_and_skipped_readonly() {
    let (dir, writer) = store();
    cohort(&writer, "0.1.0", 10, 20_000, 0);
    cohort(&writer, "0.2.0", 11, 30_000, 0);
    drop(writer);
    let path = dir.path().join("runs.sqlite");
    let connection = rusqlite::Connection::open(&path).unwrap();
    connection.execute(
        "UPDATE control_plane_event_appends SET durable_event_json = json_set(durable_event_json, '$.data.spans', 'malformed') WHERE run_id = (SELECT id FROM runs WHERE homeboy_version = '0.2.0' LIMIT 1)",
        [],
    ).unwrap();
    drop(connection);
    let reader = ObservationStore::open_readonly_at(&path).unwrap();
    let comparison = phase_hotspots(&reader, &query())
        .unwrap()
        .comparison
        .unwrap();
    assert_eq!(comparison.verdict, Verdict::Regressed);
    assert_eq!(comparison.regressions[0].candidate_samples, 10);
    let mut q = query();
    q.baseline_version = Some("previous".into());
    assert_eq!(
        phase_hotspots(&reader, &q)
            .unwrap()
            .comparison
            .unwrap()
            .baseline_version
            .as_deref(),
        Some("0.1.0")
    );
}

#[test]
fn policy_overrides_control_the_verdict() {
    let (_dir, store) = store();
    cohort(&store, "0.1.0", 10, 20_000, 0);
    cohort(&store, "0.2.0", 10, 30_000, 0);
    let policy = RegressionThresholds {
        phase_relative: 1.0,
        ..Default::default()
    };
    assert_eq!(
        compare_phase_hotspots(&store, &query(), &policy)
            .unwrap()
            .comparison
            .unwrap()
            .verdict,
        Verdict::Clean
    );
}
#[test]
fn unclassified_rate_increase_is_evidence_with_enough_missions() {
    let (_dir, store) = store();
    cohort(&store, "0.1.0", 20, 20_000, 0);
    cohort(&store, "0.2.0", 19, 20_000, 0);
    let run = store
        .start_run(
            NewRunRecord::builder("agent-task")
                .component_id("engine")
                .homeboy_version("0.2.0")
                .build(),
        )
        .unwrap();
    store.finish_run(&run.id, RunStatus::Fail, None).unwrap();
    let comparison = phase_hotspots(&store, &query())
        .unwrap()
        .comparison
        .unwrap();
    assert_eq!(comparison.verdict, Verdict::Regressed);
    assert_eq!(
        comparison.regressions[0].metric,
        "unclassified_failure_rate"
    );
}
#[test]
fn previous_never_selects_a_version_newer_than_the_candidate() {
    let (_dir, store) = store();
    cohort(&store, "0.1.0", 10, 20_000, 0);
    cohort(&store, "0.2.0", 10, 30_000, 0);
    cohort(&store, "0.3.0", 10, 20_000, 0);
    let mut q = query();
    q.baseline_version = Some("previous".into());
    let comparison = phase_hotspots(&store, &q).unwrap().comparison.unwrap();
    assert_eq!(comparison.baseline_version.as_deref(), Some("0.1.0"));
    assert_eq!(comparison.verdict, Verdict::Regressed);
}
