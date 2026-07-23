use super::*;

/// Look up a run and surface a stable validation error when it doesn't
/// exist. Used by every observation command that takes a `run_id`.
pub fn require_run(store: &ObservationStore, run_id: &str) -> Result<RunRecord> {
    if let Some(run) = store.get_run(run_id)? {
        return Ok(run);
    }
    if let Ok(Some(run)) =
        runner_evidence::with_runner_evidence(|p| p.mirror_connected_runner_run(run_id))
    {
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
    let connected_runners: Vec<RunnerConnectionInfo> = runner_evidence::with_runner_evidence(|p| {
        p.statuses().into_iter().filter(|r| r.connected).collect()
    });
    for report in connected_runners {
        let Ok(data) = runner_evidence::with_runner_evidence(|p| {
            p.daemon_api_get(&report.runner_id, "/runs?limit=200")
        }) else {
            continue;
        };
        let runs: Vec<RunRecord> =
            serde_json::from_value(data["body"]["runs"].clone()).unwrap_or_default();
        matches.extend(matching_run_labels(&runs, label));
    }
    resolve_run_label_matches(label, matches)
}

fn resolve_run_label_matches(label: &str, matches: Vec<RunRecord>) -> Result<Option<RunRecord>> {
    let matches = canonicalize_lab_run_label_matches(matches);
    match matches.len() {
        0 => Ok(None),
        1 => Ok(matches.into_iter().next()),
        _ => Err(ambiguous_run_label_error(label, &matches)),
    }
}

/// Collapse a caller and its Lab mirrors only when their durable job lineage
/// identifies one execution and exactly one caller record remains. Thin wrapper
/// over [`dedupe_runner_execution_mirrors`] that discards the hidden-mirror
/// count (label resolution only needs the canonical set).
fn canonicalize_lab_run_label_matches(matches: Vec<RunRecord>) -> Vec<RunRecord> {
    dedupe_runner_execution_mirrors(matches).canonical
}

/// Durable Lab job lineage `(runner_id, job_id)` identifying the single logical
/// execution behind a run record and its mirrors. A controller caller record
/// and its `runner-exec` / `runner_execution` mirrors all resolve to the same
/// lineage, so this is the canonical key for collapsing mirror rows into one
/// logical execution (`run_lookup` disambiguation; `runs list` dedup #9629).
/// Returns `None` for local runs with no Lab lineage.
pub fn lab_run_lineage(run: &RunRecord) -> Option<(String, String)> {
    runner_evidence::with_runner_evidence(|provider| provider.mirrored_runner_job_identity(run))
        .or_else(|| {
            let lab = run.metadata_json.get("lab")?;
            let runner_id = lab
                .pointer("/runner/id")
                .or_else(|| lab.get("runner_id"))
                .and_then(Value::as_str)?;
            let job_id = lab
                .pointer("/remote_job/id")
                .or_else(|| lab.get("remote_job_id"))
                .and_then(Value::as_str)?;
            Some((runner_id.to_string(), job_id.to_string()))
        })
}

/// Outcome of collapsing a run list to one canonical row per logical execution.
pub struct DedupedRunList {
    /// One canonical row per logical execution: the caller record when a single
    /// non-`runner-exec` caller shares the lineage, otherwise every related row
    /// (ambiguous lineage is preserved rather than silently dropped).
    pub canonical: Vec<RunRecord>,
    /// Count of mirror rows hidden by collapsing. `canonical.len() +
    /// hidden_mirrors == input.len()`.
    pub hidden_mirrors: usize,
}

/// Collapse runner-execution mirrors into one canonical row per logical Lab
/// execution, keyed by durable job lineage (#9629). Input order is preserved
/// for canonical rows. Runs without Lab lineage (local runs) always pass
/// through unchanged. When multiple non-`runner-exec` callers share one
/// lineage the lineage is ambiguous, so all related rows are kept — matching
/// the conservative behavior of `canonicalize_lab_run_label_matches`.
pub fn dedupe_runner_execution_mirrors(runs: Vec<RunRecord>) -> DedupedRunList {
    let input_len = runs.len();
    let mut canonical: Vec<RunRecord> = Vec::new();
    for (index, run) in runs.iter().enumerate() {
        let Some(lineage) = lab_run_lineage(run) else {
            canonical.push(run.clone());
            continue;
        };
        // Emit each lineage group once, at its first occurrence.
        if runs[..index]
            .iter()
            .any(|candidate| lab_run_lineage(candidate).as_ref() == Some(&lineage))
        {
            continue;
        }
        let related = runs
            .iter()
            .filter(|candidate| lab_run_lineage(candidate).as_ref() == Some(&lineage))
            .collect::<Vec<_>>();
        let callers = related
            .iter()
            .filter(|candidate| candidate.kind != "runner-exec")
            .collect::<Vec<_>>();
        if callers.len() == 1 {
            canonical.push((*callers[0]).clone());
        } else {
            // Ambiguous or caller-less lineage: keep every related row.
            canonical.extend(related.into_iter().cloned());
        }
    }
    let hidden_mirrors = input_len.saturating_sub(canonical.len());
    DedupedRunList {
        canonical,
        hidden_mirrors,
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

/// Extract the `--run-id <value>` (or `--run-id=<value>`) argument from a
/// persisted command string, if present. This is the caller-supplied logical
/// run label that ties a controller run to its runner-side mirrors.
pub fn command_run_id_label(command: &str) -> Option<&str> {
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
    let connected = runner_evidence::with_runner_evidence(|p| {
        p.statuses()
            .into_iter()
            .filter(|report| report.connected)
            .map(|report| report.runner_id)
            .collect::<Vec<_>>()
    });

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
        // Prefer the controller-side, generation-owner-routed retrieval. It
        // reads runner-owned run/artifact provenance without rotating the shared
        // tunnel, so it works even while a stale admission daemon is draining
        // (Extra-Chill/homeboy#9420).
        hints.push(format!(
            "Resolve runner-owned artifacts non-destructively from the controller: `homeboy runs artifacts {run_id} --runner {runner_id}` (routes to the generation that retains the run without rotating the shared tunnel)."
        ));
        hints.push(format!(
            "Check runner `{runner_id}` from the controller: `homeboy runs list --runner {runner_id} --limit 100`."
        ));
        hints.push(format!(
            "Inspect run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs show {run_id}`."
        ));
        hints.push(format!(
            "List artifacts for run `{run_id}` directly on runner `{runner_id}`: `homeboy runner exec {runner_id} -- homeboy runs artifacts {run_id}`."
        ));
        // A retained artifact must be readable even while the admission daemon is
        // stale. --read-only-artifact routes the exec to the retaining
        // generation instead of the mutable current admission session
        // (Extra-Chill/homeboy#9420).
        hints.push(format!(
            "If the admission daemon is stale, read retained evidence without a refresh: `homeboy runner exec {runner_id} --read-only-artifact -- homeboy runs artifacts {run_id}`."
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
    if let Err(err) =
        runner_evidence::with_runner_evidence(|p| p.refresh_mirrored_daemon_evidence(run_id))
    {
        eprintln!(
            "Warning: could not refresh mirrored Lab runner evidence for `{run_id}`: {}",
            err.message
        );
    }
}

/// Refresh one selected mirrored run. A daemon 404 means the persisted mirror
/// can no longer be observed, so preserve that terminal diagnostic locally
/// instead of emitting a generic refresh warning.
pub fn refresh_selected_mirrored_daemon_evidence_best_effort(
    store: &ObservationStore,
    run: &RunRecord,
) {
    if let Some(err) = refresh_selected_mirrored_daemon_evidence(store, run) {
        eprintln!(
            "Warning: could not refresh mirrored Lab runner evidence for `{}`: {}",
            run.id, err.message
        );
    }
}

pub(crate) fn refresh_selected_mirrored_daemon_evidence(
    store: &ObservationStore,
    run: &RunRecord,
) -> Option<Error> {
    let Some((runner_id, job_id)) =
        runner_evidence::with_runner_evidence(|p| p.mirrored_runner_job_identity(run))
    else {
        return None;
    };

    match runner_evidence::with_runner_evidence(|p| p.refresh_mirrored_daemon_evidence(&run.id)) {
        Ok(_) => None,
        Err(err) if err.details.get("http_status").and_then(Value::as_u64) == Some(404) => {
            let mut metadata = run.metadata_json.clone();
            if !metadata.is_object() {
                metadata = serde_json::json!({ "homeboy_original_metadata": metadata });
            }
            if let Some(object) = metadata.as_object_mut() {
                object.insert(
                    "runner_terminal_evidence".to_string(),
                    serde_json::json!({
                        "runner_id": runner_id,
                        "job_id": job_id,
                        "status": "not_found",
                        "lifecycle_state": "stale",
                        "stale_reason": "daemon_job_not_found",
                        "retryable": false,
                        "diagnostic": {
                            "code": err.code.as_str(),
                            "message": err.message,
                            "details": err.details,
                        },
                    }),
                );
            }
            let _ = store.finish_run(&run.id, RunStatus::Stale, Some(metadata));
            None
        }
        Err(err) => Some(err),
    }
}

/// Best-effort refresh of all locally-running Lab runner mirror records.
///
/// A controller can exit, disconnect, or time out while the runner daemon keeps
/// executing the job. Refreshing before list/reconcile reads lets the local
/// observation store converge on the daemon's terminal state without requiring
/// operators to know and run `runs show <mirror-run-id>` first.
pub fn refresh_running_mirrored_daemon_evidence_best_effort(store: &ObservationStore) {
    let statuses = runner_evidence::with_runner_evidence(|p| p.statuses());
    for report in statuses {
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
        if runner_evidence::with_runner_evidence(|p| p.mirrored_runner_job_identity(&run)).is_some()
        {
            refresh_mirrored_daemon_evidence_best_effort(&run.id);
        }
    }
}

fn finish_stale_runner_child_run(store: &ObservationStore, job: &StaleRunnerJobInfo) {
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
mod guidance_tests {
    use super::*;

    #[test]
    fn runner_owned_guidance_offers_non_destructive_controller_and_read_only_retrieval() {
        let hints = missing_run_guidance_for_runner_ids("run-42", vec!["homeboy-lab".to_string()]);

        // The controller-side, generation-owner-routed retrieval is offered
        // first: it resolves runner-owned artifacts without rotating the tunnel
        // (Extra-Chill/homeboy#9420).
        assert!(hints
            .iter()
            .any(|hint| hint.contains("homeboy runs artifacts run-42 --runner homeboy-lab")));
        // A stale admission daemon must not block reading retained evidence:
        // --read-only-artifact routes to the retaining generation.
        assert!(hints.iter().any(|hint| {
            hint.contains("--read-only-artifact")
                && hint.contains("homeboy runner exec homeboy-lab")
        }));
    }
}

#[cfg(test)]
mod stale_runner_tests {
    use super::*;
    use crate::observation::{NewRunRecord, ObservationStore};

    #[test]
    fn stale_runner_child_finalizes_matching_running_observation() {
        crate::test_support::with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let started = store
                .start_run(NewRunRecord::builder("bench").build())
                .expect("run");
            let run_id = started.id;
            let job = StaleRunnerJobInfo {
                durable_run_id: Some(run_id.clone()),
                runner_id: "homeboy-lab".to_string(),
                job_id: "orphaned-child-run-run-1".to_string(),
                status: "failed".to_string(),
                lifecycle_state: Some("stale".to_string()),
                stale_reason: Some("child_run_running_without_active_runner_job".to_string()),
                retryable: Some(true),
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

#[cfg(test)]
mod dedup_tests {
    use super::*;

    fn run(id: &str, kind: &str, metadata: Value) -> RunRecord {
        RunRecord {
            id: id.to_string(),
            kind: kind.to_string(),
            component_id: None,
            started_at: "2026-07-22T00:00:00Z".to_string(),
            finished_at: None,
            status: "success".to_string(),
            command: None,
            cwd: None,
            homeboy_version: None,
            git_sha: None,
            rig_id: None,
            metadata_json: metadata,
        }
    }

    fn lab_lineage(runner: &str, job: &str) -> Value {
        serde_json::json!({ "lab": { "runner_id": runner, "remote_job_id": job } })
    }

    #[test]
    fn collapses_mirrors_to_single_caller_and_counts_hidden() {
        // #9629: one command shows as a caller row plus runner-exec mirrors that
        // share the same durable job lineage. The default projection keeps the
        // single caller and reports the collapsed mirrors.
        let runs = vec![
            run("caller-1", "bench", lab_lineage("homeboy-lab", "job-7")),
            run(
                "mirror-1",
                "runner-exec",
                lab_lineage("homeboy-lab", "job-7"),
            ),
            run(
                "mirror-2",
                "runner-exec",
                lab_lineage("homeboy-lab", "job-7"),
            ),
        ];
        let deduped = dedupe_runner_execution_mirrors(runs);
        assert_eq!(deduped.canonical.len(), 1);
        assert_eq!(deduped.canonical[0].id, "caller-1");
        assert_eq!(deduped.hidden_mirrors, 2);
    }

    #[test]
    fn distinct_lineages_are_independent_and_local_runs_pass_through() {
        let runs = vec![
            run("caller-a", "bench", lab_lineage("homeboy-lab", "job-1")),
            run(
                "mirror-a",
                "runner-exec",
                lab_lineage("homeboy-lab", "job-1"),
            ),
            run("caller-b", "bench", lab_lineage("homeboy-lab", "job-2")),
            run("local", "trace", serde_json::json!({})),
        ];
        let deduped = dedupe_runner_execution_mirrors(runs);
        let ids: Vec<&str> = deduped.canonical.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(ids, vec!["caller-a", "caller-b", "local"]);
        assert_eq!(deduped.hidden_mirrors, 1);
    }

    #[test]
    fn ambiguous_lineage_preserves_all_related_rows() {
        // Two callers sharing one lineage is ambiguous; keep both rather than
        // silently dropping one (mirrors the label-resolution safety behavior).
        let runs = vec![
            run("caller-1", "bench", lab_lineage("homeboy-lab", "job-9")),
            run("caller-2", "bench", lab_lineage("homeboy-lab", "job-9")),
            run(
                "mirror-1",
                "runner-exec",
                lab_lineage("homeboy-lab", "job-9"),
            ),
        ];
        let deduped = dedupe_runner_execution_mirrors(runs);
        assert_eq!(deduped.canonical.len(), 3);
        assert_eq!(deduped.hidden_mirrors, 0);
    }

    #[test]
    fn lineage_reads_runner_and_job_from_lab_metadata() {
        let record = run("r", "bench", lab_lineage("homeboy-lab", "job-42"));
        assert_eq!(
            lab_run_lineage(&record),
            Some(("homeboy-lab".to_string(), "job-42".to_string()))
        );
        let local = run("l", "trace", serde_json::json!({}));
        assert_eq!(lab_run_lineage(&local), None);
    }

    #[test]
    fn command_run_id_label_extracts_run_id_argument() {
        assert_eq!(
            command_run_id_label("homeboy bench sample --run-id cook-42 --head"),
            Some("cook-42")
        );
        assert_eq!(
            command_run_id_label("homeboy bench sample --run-id=cook-99"),
            Some("cook-99")
        );
        assert_eq!(command_run_id_label("homeboy bench sample"), None);
    }
}
