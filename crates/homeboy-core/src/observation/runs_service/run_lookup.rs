use super::*;

/// Look up a run and surface a stable validation error when it doesn't
/// exist. Used by every observation command that takes a `run_id`.
pub fn require_run(store: &ObservationStore, run_id: &str) -> Result<RunRecord> {
    if let Some(run) = store.get_run(run_id)? {
        return Ok(run);
    }
    if let Ok(Some(run)) = crate::runners::mirror_connected_runner_run(run_id) {
        return Ok(run);
    }
    if let Some(run) = resolve_run_label(store, run_id)? {
        return Ok(run);
    }
    Err(missing_run_error(run_id))
}

fn resolve_run_label(store: &ObservationStore, label: &str) -> Result<Option<RunRecord>> {
    let runs = store.list_runs(RunListFilter {
        limit: Some(1000),
        ..Default::default()
    })?;
    let mut matches = matching_run_labels(&runs, label);
    for report in crate::runners::statuses()
        .unwrap_or_default()
        .into_iter()
        .filter(|report| report.connected)
    {
        let Ok(data) = crate::runners::daemon_api_get(&report.runner_id, "/runs?limit=200") else {
            continue;
        };
        let runs: Vec<RunRecord> =
            serde_json::from_value(data["body"]["runs"].clone()).unwrap_or_default();
        matches.extend(matching_run_labels(&runs, label));
    }
    resolve_run_label_matches(label, matches)
}

fn resolve_run_label_matches(label: &str, matches: Vec<RunRecord>) -> Result<Option<RunRecord>> {
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => Err(ambiguous_run_label_error(label, &matches)),
    }
}

fn ambiguous_run_label_error(label: &str, matches: &[RunRecord]) -> Error {
    let hints = matches.iter().map(disambiguation_hint).collect::<Vec<_>>();
    Error::validation_invalid_argument(
        "run_id",
        format!(
            "run label `{label}` is ambiguous; {} persisted runs match",
            matches.len()
        ),
        Some(label.to_string()),
        Some(hints),
    )
}

fn disambiguation_hint(run: &RunRecord) -> String {
    format!(
        "Use persisted id `{}` (started_at={}, component={}, rig={})",
        run.id,
        run.started_at,
        run.component_id.as_deref().unwrap_or("<none>"),
        run.rig_id.as_deref().unwrap_or("<none>")
    )
}

fn matching_run_labels(runs: &[RunRecord], label: &str) -> Vec<RunRecord> {
    runs.iter()
        .filter(|run| run_matches_label(run, label))
        .cloned()
        .collect()
}

pub(super) fn run_matches_label(run: &RunRecord, label: &str) -> bool {
    if run.id == label {
        return true;
    }
    if let Some(command) = run.command.as_deref() {
        if command_run_id_label(command) == Some(label) {
            return true;
        }
    }
    for pointer in [
        "/requested_run_id",
        "/lab/run_label",
        "/lab/explicit_run_id",
        "/lab/requested_run_id",
        "/lab/mirror_run_id",
        "/proof/provenance/run_id",
        "/caller_run_id",
        "/mirror_run_id",
        "/persisted_run_id",
        "/run_id",
    ] {
        if run.metadata_json.pointer(pointer).and_then(Value::as_str) == Some(label) {
            return true;
        }
    }
    false
}

fn command_run_id_label(command: &str) -> Option<&str> {
    let mut expect_value = false;
    for token in command.split_whitespace() {
        if expect_value {
            return (!token.is_empty()).then_some(token);
        }
        if token == "--run-id" {
            expect_value = true;
        } else if let Some(value) = token.strip_prefix("--run-id=") {
            return (!value.is_empty()).then_some(value);
        }
    }
    None
}

fn missing_run_error(run_id: &str) -> Error {
    Error::validation_invalid_argument(
        "run_id",
        format!("run record not found: {run_id}"),
        Some(run_id.to_string()),
        Some(missing_run_guidance(run_id)),
    )
}

fn missing_run_guidance(run_id: &str) -> Vec<String> {
    let mut hints = Vec::new();
    let connected = crate::runners::statuses()
        .unwrap_or_default()
        .into_iter()
        .filter(|report| report.connected)
        .map(|report| report.runner_id)
        .collect::<Vec<_>>();

    if connected.is_empty() {
        hints.push(
            "No connected runner daemon is available for controller-side lookup; connect the offload runner or inspect it with `homeboy runner exec <runner-id> -- homeboy runs list --limit 100`.".to_string(),
        );
        return hints;
    }

    missing_run_guidance_for_runner_ids(run_id, connected)
}

pub(crate) fn missing_run_guidance_for_runner_ids(
    run_id: &str,
    runner_ids: Vec<String>,
) -> Vec<String> {
    let mut hints = Vec::new();
    for runner_id in runner_ids {
        hints.push(format!(
            "Check runner `{runner_id}` from the controller: `homeboy runs list --runner {runner_id} --limit 100`."
        ));
        hints.push(format!(
            "Inspect run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs show {run_id}`."
        ));
        hints.push(format!(
            "List artifacts for run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs artifacts {run_id}`."
        ));
        hints.push(format!(
            "Export run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs export --run {run_id} --output <dir>`."
        ));
    }
    hints
}

/// Best-effort refresh of mirrored Lab runner evidence for a run.
///
/// The previous CLI helper printed a warning to stderr and swallowed the
/// error. Callers that want richer logging can use
/// [`refresh_mirrored_daemon_evidence`] directly. This helper preserves the
/// historical CLI behavior so the `runs show` / `runs artifacts` commands
/// keep emitting the same stderr text on failures.
pub fn refresh_mirrored_daemon_evidence_best_effort(run_id: &str) {
    if let Err(err) = crate::runners::refresh_mirrored_daemon_evidence(run_id) {
        eprintln!(
            "Warning: could not refresh mirrored Lab runner evidence for `{run_id}`: {}",
            err.message
        );
    }
}

/// Best-effort refresh of all locally-running Lab runner mirror records.
///
/// A controller can exit, disconnect, or time out while the runner daemon keeps
/// executing the job. Refreshing before list/reconcile reads lets the local
/// observation store converge on the daemon's terminal state without requiring
/// operators to know and run `runs show <mirror-run-id>` first.
pub fn refresh_running_mirrored_daemon_evidence_best_effort(store: &ObservationStore) {
    for report in crate::runners::statuses().unwrap_or_default() {
        for job in report.stale_runner_jobs {
            finish_stale_runner_child_run(store, &job);
        }
    }

    let runs = store
        .list_runs(RunListFilter {
            status: Some(RunStatus::Running.as_str().to_string()),
            limit: Some(1000),
            ..Default::default()
        })
        .unwrap_or_default();

    for run in runs {
        if crate::runners::mirrored_runner_job_identity(&run).is_some() {
            refresh_mirrored_daemon_evidence_best_effort(&run.id);
        }
    }
}

fn finish_stale_runner_child_run(store: &ObservationStore, job: &crate::runners::RunnerJob) {
    let Some(run_id) = job.durable_run_id.as_deref() else {
        return;
    };
    let Ok(Some(run)) = store.get_run(run_id) else {
        return;
    };
    if run.status != RunStatus::Running.as_str() {
        return;
    }
    let mut metadata = run.metadata_json;
    if !metadata.is_object() {
        metadata = serde_json::json!({ "homeboy_original_metadata": metadata });
    }
    if let Some(object) = metadata.as_object_mut() {
        object.insert(
            "runner_terminal_evidence".to_string(),
            serde_json::json!({
                "runner_id": job.runner_id,
                "job_id": job.job_id,
                "status": job.status,
                "lifecycle_state": job.lifecycle_state,
                "stale_reason": job.stale_reason,
                "retryable": job.retryable,
                "reconciled_at": chrono::Utc::now().to_rfc3339(),
            }),
        );
    }
    let _ = store.finish_run(run_id, RunStatus::Stale, Some(metadata));
}

#[cfg(test)]
mod stale_runner_tests {
    use super::*;
    use crate::api_jobs::{JobClaimMetadata, JobStatus};
    use crate::observation::{NewRunRecord, ObservationStore};
    use crate::runners::{RunnerJob, RunnerLifecycleOwner};

    #[test]
    fn stale_runner_child_finalizes_matching_running_observation() {
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let started = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let run_id = started.id;
            let job = RunnerJob {
                runner_id: "homeboy-lab".to_string(),
                job_id: "orphaned-child-run-run-1".to_string(),
                operation: "child-run".to_string(),
                status: JobStatus::Failed,
                command: "homeboy bench".to_string(),
                cwd: None,
                source: "runner-observation".to_string(),
                lifecycle_owner: RunnerLifecycleOwner::Controller,
                lifecycle: None,
                started_at_ms: None,
                updated_at_ms: None,
                elapsed_ms: None,
                heartbeat_age_ms: None,
                claim: JobClaimMetadata::default(),
                claim_expires_in_ms: None,
                durable_run_id: Some(run_id.clone()),
                stale_reason: Some("child_run_running_without_active_runner_job".to_string()),
                lifecycle_state: Some("stale".to_string()),
                retryable: Some(true),
                artifact_refs: Vec::new(),
            };

            finish_stale_runner_child_run(&store, &job);

            let run = store.get_run(&run_id).expect("read").expect("run");
            assert_eq!(run.status, "stale");
            assert!(run.finished_at.is_some());
            assert_eq!(
                run.metadata_json["runner_terminal_evidence"]["stale_reason"],
                "child_run_running_without_active_runner_job"
            );
        });
    }
}
