//! Compact, signal-only projection for `homeboy runs proof <run-id>`.
//!
//! Verifying a bench/recipe run otherwise forces reading the full evidence or
//! `runs show` payload (tens of KB) to extract a handful of signals. This
//! projection returns only the verdict (status + passed/failed), gate
//! failures, and the run's declared proof/scorecard signal fields flattened to
//! scalar `key:value` pairs. The full JSON stays behind `runs show --json` and
//! `runs evidence`; `runs proof --json` returns this same compact payload.
//!
//! Generic across run kinds: it reuses the shared
//! [`evidence_failure_summary`](homeboy::core::observation::evidence_report::evidence_failure_summary)
//! for the verdict and flattens any declared signal container
//! (`proof`/`scorecard`/`signals`), per-scenario bench metrics, and top-level
//! boolean proof signals — without baking in bench-specific field names.

use std::collections::BTreeMap;

use serde::Serialize;
use serde_json::Value;

use homeboy::core::observation::evidence_report::evidence_failure_summary;
use homeboy::core::observation::{runs_service, ObservationStore, RunRecord};

use super::{reconcile, CmdResult, RunsOutput};

/// Declared proof/scorecard signal containers. Any of these objects/arrays in
/// run metadata are flattened to scalar `key:value` signal leaves.
const SIGNAL_CONTAINERS: &[&str] = &["proof", "scorecard", "signals", "proof_signals"];

/// Generic top-level scalar (non-boolean) signals worth surfacing even when a
/// run does not nest them under a declared container.
const TOP_LEVEL_SCALAR_SIGNALS: &[&str] = &["observation_status", "http_status", "baseline_status"];

#[derive(Serialize)]
pub struct RunsProofOutput {
    pub command: &'static str,
    pub run_id: String,
    pub kind: String,
    pub status: String,
    /// `Some(true)` for a passing run, `Some(false)` for fail/error/stale, and
    /// `None` while the run is still running or otherwise indeterminate.
    pub passed: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gate_failures: Vec<String>,
    /// Flattened scalar proof/scorecard signal fields, deterministic by key.
    pub signals: BTreeMap<String, Value>,
}

pub fn proof(run_id: &str) -> CmdResult<RunsOutput> {
    let store = ObservationStore::open_initialized()?;
    reconcile::reconcile_owned_stale_running_runs(&store, 1000)?;
    runs_service::require_run(&store, run_id)?;
    runs_service::refresh_mirrored_daemon_evidence_best_effort(run_id);
    let run = runs_service::require_run(&store, run_id)?;
    Ok((RunsOutput::Proof(build_proof(&run)), 0))
}

/// Project a loaded run into its compact proof signals. Pure over the run
/// record so non-CLI callers and tests can reuse it without an observation
/// store.
pub fn build_proof(run: &RunRecord) -> RunsProofOutput {
    let failure = evidence_failure_summary(run);
    let passed = if failure.failed {
        Some(false)
    } else if matches!(run.status.as_str(), "pass" | "passed") {
        Some(true)
    } else {
        None
    };

    let mut signals = BTreeMap::new();
    collect_signals(&run.metadata_json, &mut signals);

    RunsProofOutput {
        command: "runs.proof",
        run_id: run.id.clone(),
        kind: run.kind.clone(),
        status: run.status.clone(),
        passed,
        exit_code: failure.exit_code,
        error: failure.error,
        gate_failures: failure.gate_failures,
        signals,
    }
}

fn collect_signals(metadata: &Value, out: &mut BTreeMap<String, Value>) {
    let Some(map) = metadata.as_object() else {
        return;
    };

    // 1. Declared proof/scorecard signal containers.
    for key in SIGNAL_CONTAINERS {
        if let Some(value) = map.get(*key) {
            flatten_scalars(key, value, out);
        }
    }

    // 2. Per-scenario bench scorecard metrics.
    if let Some(scenarios) = map.get("scenario_metrics").and_then(Value::as_array) {
        for scenario in scenarios {
            let id = scenario
                .get("scenario_id")
                .and_then(Value::as_str)
                .unwrap_or("scenario");
            if let Some(passed @ Value::Bool(_)) = scenario.get("passed") {
                out.insert(format!("{id}.passed"), passed.clone());
            }
            if let Some(metrics) = scenario.get("metrics") {
                flatten_scalars(id, metrics, out);
            }
        }
    }

    // 3. Top-level boolean proof signals (e.g. probe `rendered_contains_marker`,
    //    `opfs_resume`) plus known scalar status signals.
    for (key, value) in map {
        if (value.is_boolean() || TOP_LEVEL_SCALAR_SIGNALS.contains(&key.as_str()))
            && is_scalar(value)
        {
            out.insert(key.clone(), value.clone());
        }
    }
}

/// Recursively record scalar (bool/number/string) leaves under a dotted key
/// path. Objects and arrays recurse; `null` is dropped so the projection only
/// carries decided signals.
fn flatten_scalars(prefix: &str, value: &Value, out: &mut BTreeMap<String, Value>) {
    match value {
        Value::Object(map) => {
            for (key, child) in map {
                flatten_scalars(&format!("{prefix}.{key}"), child, out);
            }
        }
        Value::Array(items) => {
            for (idx, child) in items.iter().enumerate() {
                flatten_scalars(&format!("{prefix}[{idx}]"), child, out);
            }
        }
        Value::Null => {}
        scalar => {
            out.insert(prefix.to_string(), scalar.clone());
        }
    }
}

fn is_scalar(value: &Value) -> bool {
    matches!(value, Value::Bool(_) | Value::Number(_) | Value::String(_))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run_with(status: &str, metadata: Value) -> RunRecord {
        RunRecord {
            id: "run-proof-1".to_string(),
            kind: "bench".to_string(),
            component_id: Some("homeboy".to_string()),
            started_at: "2026-06-29T00:00:00Z".to_string(),
            finished_at: Some("2026-06-29T00:01:00Z".to_string()),
            status: status.to_string(),
            command: Some("homeboy bench homeboy".to_string()),
            cwd: Some("/tmp/homeboy-fixture".to_string()),
            homeboy_version: Some("test-version".to_string()),
            git_sha: Some("abc123".to_string()),
            rig_id: None,
            metadata_json: metadata,
        }
    }

    #[test]
    fn build_proof_flattens_declared_and_bench_signals() {
        let run = run_with(
            "fail",
            json!({
                "exit_code": 2,
                "error": "gate exceeded",
                "gate_failures": ["p95_ms exceeded", "rss_mb exceeded"],
                "observation_status": "failed",
                "baseline_status": null,
                // Declared probe-style proof signals (booleans + nested scalar).
                "proof": {
                    "rendered_contains_marker": true,
                    "opfs_resume": false,
                    "http_status": 200,
                    "nested": { "value": "ok" }
                },
                // Top-level boolean proof signal not nested in a container.
                "marker_present": true,
                // Bench scorecard metrics.
                "scenario_metrics": [{
                    "scenario_id": "cold",
                    "passed": false,
                    "metrics": { "p95_ms": 42.0, "rss_mb": 5496.3 }
                }]
            }),
        );

        let proof = build_proof(&run);

        assert_eq!(proof.command, "runs.proof");
        assert_eq!(proof.status, "fail");
        assert_eq!(proof.passed, Some(false));
        assert_eq!(proof.exit_code, Some(2));
        assert_eq!(proof.error.as_deref(), Some("gate exceeded"));
        assert_eq!(proof.gate_failures, vec!["p95_ms exceeded", "rss_mb exceeded"]);

        // Declared container flattened to dotted scalar leaves.
        assert_eq!(proof.signals.get("proof.rendered_contains_marker"), Some(&json!(true)));
        assert_eq!(proof.signals.get("proof.opfs_resume"), Some(&json!(false)));
        assert_eq!(proof.signals.get("proof.http_status"), Some(&json!(200)));
        assert_eq!(proof.signals.get("proof.nested.value"), Some(&json!("ok")));
        // Top-level boolean proof signal and known scalar status signal.
        assert_eq!(proof.signals.get("marker_present"), Some(&json!(true)));
        assert_eq!(proof.signals.get("observation_status"), Some(&json!("failed")));
        // Per-scenario bench metrics + passed flag.
        assert_eq!(proof.signals.get("cold.passed"), Some(&json!(false)));
        assert_eq!(proof.signals.get("cold.p95_ms"), Some(&json!(42.0)));
        assert_eq!(proof.signals.get("cold.rss_mb"), Some(&json!(5496.3)));

        // Null signal is dropped; non-signal scalars (exit_code/error) are not
        // duplicated into the signal map.
        assert!(!proof.signals.contains_key("baseline_status"));
        assert!(!proof.signals.contains_key("exit_code"));
        assert!(!proof.signals.contains_key("error"));
    }

    #[test]
    fn build_proof_marks_running_run_pending_with_no_signals() {
        let run = run_with("running", json!({ "phase": "warmup" }));
        let proof = build_proof(&run);
        assert_eq!(proof.passed, None);
        assert!(proof.gate_failures.is_empty());
        assert!(proof.signals.is_empty());
    }

    #[test]
    fn build_proof_marks_pass() {
        let run = run_with("pass", json!({ "scenario_metrics": [] }));
        let proof = build_proof(&run);
        assert_eq!(proof.passed, Some(true));
    }
}
