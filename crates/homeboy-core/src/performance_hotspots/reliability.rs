//! Read-only mission reliability, using run versions (not span recorder versions).
use std::collections::{BTreeMap, BTreeSet};

use homeboy_engine_primitives::phase_timing::PhaseStatus;
use serde::Serialize;
use serde_json::Value;

use super::PhaseHotspotQuery;
use crate::observation::{ObservationStore, RunRecord};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Reliability {
    pub versions: Vec<VersionReliability>,
    pub comparison: Option<ReliabilityComparison>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct VersionReliability {
    pub homeboy_version: String,
    pub runs: usize,
    pub passes: usize,
    pub task_failures: usize,
    pub infrastructure_failures: usize,
    pub unclassified_failures: usize,
    /// Infrastructure failures / all missions started, including unfinished missions.
    pub infrastructure_failure_rate: f64,
    /// Missing reasons are defects, even when a phase is available.
    pub defects: usize,
    pub top_failing_phases: Vec<FailingPhase>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FailingPhase {
    pub phase: String,
    pub component: String,
    pub classification: String,
    pub failures: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ReliabilityComparison {
    pub baseline_version: String,
    pub candidate_version: String,
    /// Candidate minus baseline; rates are fractions, not percentages.
    pub infrastructure_failure_rate_delta: f64,
    pub task_failures_delta: i64,
    pub infrastructure_failures_delta: i64,
    pub unclassified_failures_delta: i64,
}

/// One ordered ownership mapping. Setup/supervision is Homeboy's responsibility,
/// even around gates/providers; candidate rejection and agent execution are task
/// failures. Absent diagnostics always win over phase inference. Unknown reasons
/// stay unclassified rather than silently blaming the candidate.
fn classify(diagnostic: Option<&str>, phase: &str) -> &'static str {
    let Some(reason) = diagnostic.filter(|s| !s.trim().is_empty()) else {
        return "unclassified";
    };
    let reason = reason.to_ascii_lowercase();
    if reason.contains("no failure diagnostic was recorded") {
        return "unclassified";
    }
    let owner = phase.to_ascii_lowercase();
    if reason.contains("deterministic gate")
        && !["setup", "supervision", "transport"]
            .iter()
            .any(|key| owner.contains(key) || reason.contains(key))
    {
        return "task";
    }
    if [
        "staging",
        "stage",
        "handoff",
        "seal",
        "transport",
        "gate_setup",
        "gate.setup",
        "gate_supervision",
        "gate.supervision",
        "promotion_setup",
        "promote",
        "publish",
        "controller_failure",
        "source_materialization",
    ]
    .iter()
    .any(|key| owner.contains(key))
        || reason.starts_with("/cook_controller_failure:")
        || reason.starts_with("/pre_execution_failure:")
        || reason.starts_with("/lab_staging_controller_failure:")
    {
        return "infrastructure";
    }
    if reason.contains("deterministic gate")
        || reason.contains("gate_failed")
        || ["provider", "agent", "execution", "gate"]
            .iter()
            .any(|key| phase == *key || phase.starts_with(&format!("{key}.")))
        || reason.contains("timeout: provider")
        || reason.contains("outcome.")
    {
        return "task";
    }
    "unclassified"
}

/// These are the durable fields consumed by agent-task status: delivery blocker,
/// controller/pre-execution failure, then the first failed outcome diagnostic.
fn diagnostic(run: &RunRecord) -> Option<String> {
    let record = &run.metadata_json["agent_task_run"];
    let metadata = &record["metadata"];
    for path in [
        "/cook_delivery/delivery/blocker",
        "/cook_controller_failure",
        "/pre_execution_failure",
        "/lab_staging_controller_failure",
    ] {
        if let Some(value) = metadata.pointer(path) {
            if let Some(message) = value["message"]
                .as_str()
                .or_else(|| value["detail"].as_str())
            {
                if !message.trim().is_empty() {
                    return Some(format!(
                        "{}: {message}",
                        value["code"].as_str().unwrap_or(path)
                    ));
                }
            }
        }
    }
    if let Some(message) = record
        .pointer("/candidate_adoption/terminal_error")
        .and_then(Value::as_str)
    {
        return Some(message.to_string());
    }
    let outcomes = run
        .metadata_json
        .pointer("/agent_task_aggregate/outcomes")?
        .as_array()?;
    for outcome in outcomes.iter().filter(|o| {
        matches!(
            o["status"].as_str(),
            Some("failed" | "timeout" | "candidate_recoverable")
        )
    }) {
        if let Some(items) = outcome["diagnostics"].as_array() {
            for item in items {
                if let Some(message) = item["message"].as_str().filter(|s| !s.trim().is_empty()) {
                    return Some(format!(
                        "outcome.{}: {message}",
                        item["class"].as_str().unwrap_or("unknown")
                    ));
                }
            }
        }
    }
    None
}

pub(super) fn reliability(
    store: &ObservationStore,
    runs: &[RunRecord],
    query: &PhaseHotspotQuery,
) -> Reliability {
    let mut versions = BTreeMap::<String, VersionReliability>::new();
    let mut seen = BTreeSet::new();
    // list_runs is newest first: one selected attempt per mission, not one
    // failure for every retry or detached handoff row.
    for run in runs {
        let projected = (|| -> crate::Result<_> {
            let mission = store
                .get_run_mission(&run.id)?
                .unwrap_or_else(|| run.id.clone());
            let spans = store.phase_spans(&run.id)?;
            Ok((mission, spans))
        })();
        let (mission, spans) = match projected {
            Ok(value) => value,
            Err(error) => {
                crate::log_status!("runs", "skipping reliability row {}: {}", run.id, error);
                continue;
            }
        };
        if !seen.insert(mission) {
            continue;
        }
        let version = run.homeboy_version.as_deref().unwrap_or("unknown");
        if query
            .homeboy_version
            .as_deref()
            .is_some_and(|v| v != version)
        {
            continue;
        }
        let group = versions
            .entry(version.to_string())
            .or_insert_with(|| VersionReliability {
                homeboy_version: version.to_string(),
                ..Default::default()
            });
        group.runs += 1;
        if run.status == "pass" {
            group.passes += 1;
            continue;
        }
        if !matches!(run.status.as_str(), "fail" | "failed" | "error" | "timeout") {
            continue;
        }
        let phase = spans
            .spans
            .iter()
            .filter(|s| {
                s.status == PhaseStatus::Failed
                    || s.duration_ms.is_none() && s.status != PhaseStatus::Skipped
            })
            .max_by_key(|s| &s.started_at)
            .map(|s| s.id.as_str())
            .or_else(|| {
                run.metadata_json
                    .pointer("/agent_task_run/metadata/cook_progress/phase")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                run.metadata_json
                    .pointer("/agent_task_run/metadata/control_plane/phase")
                    .and_then(Value::as_str)
            })
            .or_else(|| {
                run.metadata_json
                    .pointer("/agent_task_run/metadata/phase")
                    .and_then(Value::as_str)
            })
            .unwrap_or("unknown");
        let reason = diagnostic(run);
        let class = classify(reason.as_deref(), phase);
        match class {
            "task" => group.task_failures += 1,
            "infrastructure" => group.infrastructure_failures += 1,
            _ => group.unclassified_failures += 1,
        }
        let component = run
            .component_id
            .as_deref()
            .unwrap_or_else(|| phase.split(['.', '_']).next().unwrap_or("unknown"));
        if let Some(entry) = group
            .top_failing_phases
            .iter_mut()
            .find(|p| p.phase == phase && p.component == component && p.classification == class)
        {
            entry.failures += 1;
        } else {
            group.top_failing_phases.push(FailingPhase {
                phase: phase.into(),
                component: component.into(),
                classification: class.into(),
                failures: 1,
            });
        }
    }
    for group in versions.values_mut() {
        group.infrastructure_failure_rate =
            group.infrastructure_failures as f64 / group.runs as f64;
        group.defects = group.infrastructure_failures + group.unclassified_failures;
        group.top_failing_phases.sort_by(|a, b| {
            b.failures
                .cmp(&a.failures)
                .then(a.phase.cmp(&b.phase))
                .then(a.component.cmp(&b.component))
                .then(a.classification.cmp(&b.classification))
        });
        group.top_failing_phases.truncate(10);
    }
    let comparison = query
        .baseline_version
        .as_ref()
        .zip(query.candidate_version.as_ref())
        .and_then(|(b, c)| {
            let baseline = versions.get(b)?;
            let candidate = versions.get(c)?;
            Some(ReliabilityComparison {
                baseline_version: b.clone(),
                candidate_version: c.clone(),
                infrastructure_failure_rate_delta: candidate.infrastructure_failure_rate
                    - baseline.infrastructure_failure_rate,
                task_failures_delta: candidate.task_failures as i64 - baseline.task_failures as i64,
                infrastructure_failures_delta: candidate.infrastructure_failures as i64
                    - baseline.infrastructure_failures as i64,
                unclassified_failures_delta: candidate.unclassified_failures as i64
                    - baseline.unclassified_failures as i64,
            })
        });
    Reliability {
        versions: versions.into_values().collect(),
        comparison,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::observation::{NewRunRecord, RunStatus};
    use crate::performance_hotspots::phase_hotspots;
    use homeboy_engine_primitives::phase_timing::PhaseTimer;
    use serde_json::json;
    use std::time::Duration;

    #[test]
    fn performance_hotspots_mixed_mission_reliability() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("runs.sqlite")).unwrap();
        for (version, status, phase, metadata) in [
            (
                "before",
                RunStatus::Fail,
                "promote",
                json!({"cook_delivery":{"delivery":{"blocker":{"code":"gate_failed", "message":"deterministic gate `gate-1` failed"}}}}),
            ),
            (
                "before",
                RunStatus::Fail,
                "stage.transfer",
                json!({"cook_controller_failure":{"message":"transport disconnected"}}),
            ),
            (
                "before",
                RunStatus::Fail,
                "controller_runtime_seal",
                json!({}),
            ),
            ("before", RunStatus::Pass, "publish", json!({})),
            ("after", RunStatus::Pass, "publish", json!({})),
        ] {
            let run = store
                .start_run(
                    NewRunRecord::builder("agent-task")
                        .component_id("engine")
                        .homeboy_version(version)
                        .metadata(json!({"agent_task_run":{"metadata":metadata}}))
                        .build(),
                )
                .unwrap();
            let mut timer = PhaseTimer::with_version("different-recorder-version");
            timer.record_failed(phase, Duration::from_millis(10));
            store
                .record_phase_spans(&run.id, "attempt", &timer.into_report())
                .unwrap();
            store.finish_run(&run.id, status, None).unwrap();
        }
        let summary = phase_hotspots(
            &store,
            &PhaseHotspotQuery {
                kind: Some("agent-task".into()),
                baseline_version: Some("before".into()),
                candidate_version: Some("after".into()),
                last: 200,
                ..Default::default()
            },
        )
        .unwrap()
        .reliability;
        let before = summary
            .versions
            .iter()
            .find(|v| v.homeboy_version == "before")
            .unwrap();
        assert_eq!(
            (
                before.runs,
                before.passes,
                before.task_failures,
                before.infrastructure_failures,
                before.unclassified_failures
            ),
            (4, 1, 1, 1, 1)
        );
        assert_eq!(before.infrastructure_failure_rate, 0.25);
        assert_eq!(before.defects, 2);
        assert!(before
            .top_failing_phases
            .iter()
            .any(|p| p.phase == "stage.transfer"
                && p.component == "engine"
                && p.classification == "infrastructure"));
        let after = summary
            .versions
            .iter()
            .find(|v| v.homeboy_version == "after")
            .unwrap();
        assert_eq!(
            (after.runs, after.passes, after.infrastructure_failure_rate),
            (1, 1, 0.0)
        );
        assert_eq!(
            summary
                .comparison
                .unwrap()
                .infrastructure_failure_rate_delta,
            -0.25
        );
    }

    #[test]
    fn performance_hotspots_mission_deduplication_filters_and_malformed_rows() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("runs.sqlite");
        let store = ObservationStore::open_initialized_at(&path).unwrap();
        let mission = "reliability-mission";
        let first = store
            .start_run_in_mission(
                NewRunRecord::builder("agent-task")
                    .homeboy_version("old")
                    .build(),
                mission,
            )
            .unwrap();
        store.finish_run(&first.id, RunStatus::Fail, None).unwrap();
        std::thread::sleep(Duration::from_millis(2));
        let latest = store
            .start_run_in_mission(
                NewRunRecord::builder("agent-task")
                    .homeboy_version("new")
                    .build(),
                mission,
            )
            .unwrap();
        store.finish_run(&latest.id, RunStatus::Pass, None).unwrap();
        let malformed = store
            .start_run(NewRunRecord::builder("agent-task").build())
            .unwrap();
        let connection = rusqlite::Connection::open(&path).unwrap();
        connection
            .execute(
                "UPDATE runs SET status = x'ff' WHERE id = ?1",
                [&malformed.id],
            )
            .unwrap();
        drop(connection);
        drop(store);
        let reader = ObservationStore::open_readonly_at(&path).unwrap();
        let query = PhaseHotspotQuery {
            last: 200,
            ..Default::default()
        };
        let summary = phase_hotspots(&reader, &query).unwrap();
        assert_eq!(summary.reliability.versions.len(), 1);
        assert_eq!(summary.reliability.versions[0].homeboy_version, "new");
        assert_eq!(summary.reliability.versions[0].runs, 1);
        assert_eq!(summary.reliability.versions[0].passes, 1);
        let filtered = phase_hotspots(
            &reader,
            &PhaseHotspotQuery {
                homeboy_version: Some("old".into()),
                ..query
            },
        )
        .unwrap();
        assert!(filtered.reliability.versions.is_empty());
    }

    #[test]
    fn performance_hotspots_failure_ownership_mapping() {
        for phase in [
            "stage",
            "handoff",
            "seal",
            "transport",
            "gate.setup",
            "gate.supervision",
            "promotion_setup",
        ] {
            assert_eq!(classify(Some("operation failed"), phase), "infrastructure");
            assert_eq!(classify(None, phase), "unclassified");
        }
        assert_eq!(
            classify(Some("deterministic gate `gate-1` failed"), "promote"),
            "task"
        );
        assert_eq!(
            classify(Some("deterministic gate setup failed"), "gate.setup"),
            "infrastructure"
        );
        assert_eq!(
            classify(Some("timeout: provider exceeded deadline"), "provider"),
            "task"
        );
        assert_eq!(classify(Some("no failure diagnostic was recorded for `failed` during `controller_runtime_seal`"), "seal"), "unclassified");
    }
}
