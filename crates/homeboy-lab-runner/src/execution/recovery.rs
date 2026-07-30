//! Recovery of generic runner-exec evidence after a controller interruption.

use super::*;

const STARTUP_RUNNER_EXEC_RECOVERY_LIMIT: i64 = 100;

/// Scan durable generic runner-exec runs, query their exact accepted runner-job
/// binding, then retain declared evidence before terminal projection. This runs
/// before command dispatch so a new daemon operation cannot evict a completed
/// job while its controller checkpoint is still pending.
pub fn reconcile_terminal_runner_exec_runs() -> Result<usize> {
    let store = ObservationStore::open_initialized()?;
    let mut reconciled = 0;
    for run in store.list_runs(RunListFilter {
        kind: Some("runner_execution".to_string()),
        status: Some(RunStatus::Running.as_str().to_string()),
        limit: Some(STARTUP_RUNNER_EXEC_RECOVERY_LIMIT),
        ..RunListFilter::default()
    })? {
        if run.metadata_json.get("kind").and_then(Value::as_str) != Some("runner_exec") {
            continue;
        }
        let (Some(runner_id), Some(job_id), Some(cwd)) = (
            run.metadata_json.get("runner_id").and_then(Value::as_str),
            run.metadata_json
                .get("runner_job_id")
                .and_then(Value::as_str),
            run.cwd.as_deref(),
        ) else {
            continue;
        };
        let snapshot = match crate::evidence::runner_job_log_snapshot(runner_id, job_id) {
            Ok(snapshot) => snapshot,
            Err(error) => {
                record_evicted_evidence_loss(&store, &run, &error)?;
                continue;
            }
        };
        if !snapshot.job.status.is_terminal() {
            continue;
        }
        homeboy_agents::agent_task_lifecycle::record_runner_exec_terminal_checkpoint(
            &run.id, &snapshot,
        )?;
        let declarations = run
            .metadata_json
            .get("runner_exec_artifact_declarations")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let strings = |key: &str| -> Vec<String> {
            declarations
                .get(key)
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(str::to_string)
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        };
        let output = recovered_output(&run, &snapshot, cwd);
        let mut artifacts = Vec::new();
        for declaration in strings("artifacts") {
            if homeboy_agents::agent_task_lifecycle::runner_exec_declaration_is_promoted(
                &run,
                "artifact",
                &declaration,
            ) {
                continue;
            }
            let promoted = promote_runner_exec_artifacts(
                &run.id,
                &output,
                std::slice::from_ref(&declaration),
            )?;
            homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion(
                &run.id,
                "artifact",
                &declaration,
                &promoted,
            )?;
            artifacts.extend(promoted);
        }
        let mut directories = Vec::new();
        for declaration in strings("artifact_dirs") {
            if homeboy_agents::agent_task_lifecycle::runner_exec_declaration_is_promoted(
                &run,
                "artifact_dir",
                &declaration,
            ) {
                continue;
            }
            let promoted = promote_runner_exec_artifact_dirs(
                &run.id,
                &output,
                std::slice::from_ref(&declaration),
            )?;
            homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion(
                &run.id,
                "artifact_dir",
                &declaration,
                &promoted,
            )?;
            directories.extend(promoted);
        }
        let mut summaries = Vec::new();
        for declaration in strings("summaries") {
            if homeboy_agents::agent_task_lifecycle::runner_exec_declaration_is_promoted(
                &run,
                "summary",
                &declaration,
            ) {
                continue;
            }
            let promoted = promote_runner_exec_summaries(
                &run.id,
                &output,
                std::slice::from_ref(&declaration),
            )?;
            homeboy_agents::agent_task_lifecycle::record_runner_exec_declaration_promotion(
                &run.id,
                "summary",
                &declaration,
                &promoted,
            )?;
            summaries.extend(promoted);
        }
        let retained = artifacts
            .iter()
            .chain(directories.iter())
            .chain(summaries.iter())
            .cloned()
            .collect::<Vec<_>>();
        homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_refs(&run.id, &retained)?;
        homeboy_agents::agent_task_lifecycle::project_terminal_runner_result(&run.id, &snapshot)?;
        reconciled += 1;
    }
    Ok(reconciled)
}

fn recovered_output(
    run: &homeboy_core::observation::RunRecord,
    snapshot: &homeboy_core::api_jobs::RunnerJobLogSnapshot,
    cwd: &str,
) -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: snapshot.job.target_runner_id.clone().unwrap_or_default(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: run
            .metadata_json
            .get("remote_command")
            .and_then(Value::as_array)
            .map(|values| {
                values
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default(),
        remote_cwd: cwd.to_string(),
        exit_code: if snapshot.job.status == JobStatus::Succeeded {
            0
        } else {
            1
        },
        stdout: String::new(),
        stderr: String::new(),
        source_snapshot: None,
        job: Some(snapshot.job.clone()),
        runner_job: None,
        job_id: Some(snapshot.job.id.to_string()),
        job_events: Some(snapshot.events.clone()),
        mirror_run_id: None,
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: None,
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}

fn record_evicted_evidence_loss(
    store: &ObservationStore,
    run: &homeboy_core::observation::RunRecord,
    error: &Error,
) -> Result<()> {
    let checkpointed = run
        .metadata_json
        .pointer("/runner_terminal_projection/state")
        .and_then(Value::as_str)
        == Some("terminal_checkpointed");
    if error.details.get("http_status").and_then(Value::as_u64) != Some(404) {
        return Ok(());
    }
    let has_declarations = run
        .metadata_json
        .get("runner_exec_artifact_declarations")
        .and_then(Value::as_object)
        .is_some_and(|declarations| {
            declarations
                .values()
                .any(|value| value.as_array().is_some_and(|values| !values.is_empty()))
        });
    if !checkpointed && !has_declarations {
        return Ok(());
    }
    let mut metadata = run.metadata_json.clone();
    metadata["runner_terminal_projection"] = json!({
        "state": "unrecoverable_evidence_loss",
        "classification": if checkpointed { "daemon_evicted_missing_declared_artifacts" } else { "daemon_evicted_before_terminal_projection" },
        "runner_id": run.metadata_json["runner_id"],
        "runner_job_id": run.metadata_json["runner_job_id"],
    });
    store.finish_run(&run.id, RunStatus::Fail, Some(metadata))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::test_support::with_isolated_home;
    use homeboy_core::{Error, ErrorCode};

    #[test]
    fn startup_recovery_does_not_materialize_unrelated_active_payloads() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            let unrelated = store
                .start_run(NewRunRecord::builder("unrelated").build())
                .expect("unrelated run");
            drop(store);

            let path = homeboy_core::observation::store::database_path().expect("database path");
            let connection = rusqlite::Connection::open(path).expect("open database");
            connection
                .execute("DROP INDEX idx_runs_metadata_retry_of", [])
                .expect("drop metadata expression index");
            connection
                .execute(
                    "UPDATE runs SET metadata_json = 'invalid-json' WHERE id = ?1",
                    [&unrelated.id],
                )
                .expect("corrupt unrelated payload");
            drop(connection);

            assert_eq!(
                reconcile_terminal_runner_exec_runs().expect("bounded recovery"),
                0
            );
        });
    }

    #[test]
    fn startup_recovery_bounds_runner_exec_candidates() {
        with_isolated_home(|_| {
            let store = ObservationStore::open_initialized().expect("store");
            drop(store);
            let path = homeboy_core::observation::store::database_path().expect("database path");
            let connection = rusqlite::Connection::open(path).expect("open database");
            connection
                .execute("DROP INDEX idx_runs_metadata_retry_of", [])
                .expect("drop metadata expression index");
            for index in 0..STARTUP_RUNNER_EXEC_RECOVERY_LIMIT {
                connection
                    .execute(
                        "INSERT INTO runs(id, kind, started_at, status, metadata_json) VALUES (?1, 'runner_execution', ?2, 'running', '{\"kind\":\"runner_exec\"}')",
                        rusqlite::params![
                            format!("candidate-{index}"),
                            format!("2026-07-30T{:02}:{:02}:00Z", 18 + index / 60, index % 60)
                        ],
                    )
                    .expect("insert candidate");
            }
            connection
                .execute(
                    "INSERT INTO runs(id, kind, started_at, status, metadata_json) VALUES ('outside-budget', 'runner_execution', '2026-01-01T00:00:00Z', 'running', 'invalid-json')",
                    [],
                )
                .expect("insert outside-budget candidate");
            drop(connection);

            assert_eq!(
                reconcile_terminal_runner_exec_runs().expect("bounded recovery"),
                0
            );
        });
    }

    #[test]
    fn daemon_eviction_preserves_literal_declaration_paths_in_loss_detection() {
        with_isolated_home(|_| {
            let run_id = "evicted-escaped-declaration";
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                run_id,
                "runner",
                "job",
                "/workspace",
                &["true".to_string()],
            )
            .expect("run");
            homeboy_agents::agent_task_lifecycle::record_runner_exec_artifact_declarations(
                run_id,
                &["artifacts/result.json".to_string(), "a~b/c".to_string()],
                &[],
                &[],
            )
            .expect("declarations");
            let store = ObservationStore::open_initialized().expect("store");
            let run = store.get_run(run_id).expect("read").expect("run");
            let error = Error {
                code: ErrorCode::ValidationInvalidArgument,
                message: "daemon job evicted".to_string(),
                details: json!({ "http_status": 404 }),
                hints: Vec::new(),
                retryable: None,
            };
            record_evicted_evidence_loss(&store, &run, &error).expect("eviction recorded");
            let terminal = store.get_run(run_id).expect("read").expect("terminal run");
            assert_eq!(terminal.status, "fail");
            assert_eq!(
                terminal.metadata_json["runner_terminal_projection"]["classification"],
                "daemon_evicted_before_terminal_projection"
            );
        });
    }
}
