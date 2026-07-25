use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::Duration;

use homeboy::core::api_jobs::{JobEvent, JobStatus};
use homeboy::runner::runners::{self as runner, runner_job_log_snapshot};

use super::super::CmdResult;
use super::cli::RunnerJobCommand;
use super::types::{RunnerBrokerJobOutput, RunnerJobOutput};

pub(super) enum RunnerJobCommandOutput {
    Daemon(RunnerJobOutput),
    Broker(RunnerBrokerJobOutput),
}

pub(super) fn job(command: RunnerJobCommand) -> CmdResult<RunnerJobCommandOutput> {
    match command {
        RunnerJobCommand::Logs {
            runner_id,
            job_id,
            follow,
            poll_ms,
            cursor,
            compact,
            tail_kb,
        } => map_daemon_job(job_logs(
            &runner_id, &job_id, follow, poll_ms, cursor, compact, tail_kb,
        )),
        RunnerJobCommand::Cancel { runner_id, job_id } => {
            map_daemon_job(job_cancel(&runner_id, &job_id))
        }
        RunnerJobCommand::Reconcile { runner_id } => job_reconcile(&runner_id),
        RunnerJobCommand::Artifacts {
            runner_id,
            job_id,
            artifact_id,
        } => job_artifacts(&runner_id, &job_id, &artifact_id),
    }
}

fn map_daemon_job(result: CmdResult<RunnerJobOutput>) -> CmdResult<RunnerJobCommandOutput> {
    result.map(|(output, exit_code)| (RunnerJobCommandOutput::Daemon(output), exit_code))
}

fn job_reconcile(runner_id: &str) -> CmdResult<RunnerJobCommandOutput> {
    Ok((
        RunnerJobCommandOutput::Broker(RunnerBrokerJobOutput {
            variant: "job_reconcile",
            command: "runner.job.reconcile",
            runner_id: runner_id.to_string(),
            job_id: None,
            artifact_id: None,
            response: runner::reverse_broker_reconcile(runner_id)?,
        }),
        0,
    ))
}

fn job_artifacts(
    runner_id: &str,
    job_id: &str,
    artifact_id: &str,
) -> CmdResult<RunnerJobCommandOutput> {
    Ok((
        RunnerJobCommandOutput::Broker(RunnerBrokerJobOutput {
            variant: "job_artifacts",
            command: "runner.job.artifacts",
            runner_id: runner_id.to_string(),
            job_id: Some(job_id.to_string()),
            artifact_id: Some(artifact_id.to_string()),
            response: runner::reverse_broker_artifact(runner_id, job_id, artifact_id)?,
        }),
        0,
    ))
}

fn job_cancel(runner_id: &str, job_id: &str) -> CmdResult<RunnerJobOutput> {
    let (job, events) = match runner::runner_job_cancel(runner_id, job_id) {
        Ok(result) => result,
        Err(_) => {
            let session = runner::reconnect_job_log_owner(runner_id, job_id)?;
            runner::runner_job_cancel_for_session(&session, job_id)?
        }
    };
    let next_cursor = events.iter().map(|event| event.sequence).max().unwrap_or(0);
    let runner_job = homeboy::runner::runners::RunnerJob::from_job(
        runner_id,
        "runner.job.cancel",
        &[],
        None,
        &job,
    );

    Ok((
        RunnerJobOutput {
            variant: "job_cancel",
            command: "runner.job.cancel",
            runner_id: runner_id.to_string(),
            job_id: job_id.to_string(),
            follow: false,
            compact: false,
            job,
            runner_job,
            events,
            exit_code: None,
            orchestration_provenance: None,
            stdout: None,
            stderr: None,
            next_cursor,
            resume_command: None,
        },
        0,
    ))
}

fn job_logs(
    runner_id: &str,
    job_id: &str,
    follow: bool,
    poll_ms: u64,
    cursor: Option<u64>,
    compact: bool,
    tail_kb: Option<usize>,
) -> CmdResult<RunnerJobOutput> {
    let poll_interval = Duration::from_millis(poll_ms.max(100));
    let mut emitted_sequence = cursor.unwrap_or(0);
    let mut snapshot = runner_job_log_snapshot(runner_id, job_id)?;

    emit_new_job_events(&snapshot.events, &mut emitted_sequence);
    let stop = Arc::new(AtomicBool::new(false));
    if follow {
        // The handler only requests a cooperative exit; it never cancels the
        // remote job. The printed cursor is sufficient to resume exactly once.
        homeboy_process::install_shutdown_handler(stop.clone(), "runner job log follow")?;
    }
    let mut reconnects = 0;
    const MAX_RECONNECTS: u8 = 3;
    while follow && !runner_job_terminal(snapshot.job.status) {
        std::thread::sleep(poll_interval);
        if stop.load(Ordering::SeqCst) {
            eprintln!(
                "follow interrupted; resume with `{}`",
                resume_command(runner_id, job_id, emitted_sequence, poll_ms)
            );
            break;
        }
        snapshot = match runner_job_log_snapshot(runner_id, job_id) {
            Ok(snapshot) => snapshot,
            Err(_error) if reconnects < MAX_RECONNECTS => {
                reconnects += 1;
                eprintln!("runner log transport lost; reconnecting to the authoritative job generation ({reconnects}/{MAX_RECONNECTS})");
                let session = runner::reconnect_job_log_owner(runner_id, job_id).map_err(
                    |reconnect_error| {
                        follow_recovery_error(runner_id, job_id, emitted_sequence, reconnect_error)
                    },
                )?;
                match runner::runner_job_log_snapshot_for_session(&session, job_id) {
                    Ok(snapshot) => snapshot,
                    Err(recovery_error) => {
                        return Err(classify_follow_error(
                            runner_id,
                            job_id,
                            emitted_sequence,
                            recovery_error,
                        ))
                    }
                }
            }
            Err(error) => {
                return Err(follow_recovery_error(
                    runner_id,
                    job_id,
                    emitted_sequence,
                    error,
                ))
            }
        };
        emit_new_job_events(&snapshot.events, &mut emitted_sequence);
    }
    let runner_job = homeboy::runner::runners::RunnerJob::from_job(
        runner_id,
        "runner.job.logs",
        &[],
        None,
        &snapshot.job,
    );

    let tail_bytes = tail_kb.map(|kb| kb.saturating_mul(1024));
    let projection = super::log_projection::project_job_log(snapshot.events, compact, tail_bytes);

    Ok((
        RunnerJobOutput {
            variant: "job_logs",
            command: "runner.job.logs",
            runner_id: runner_id.to_string(),
            job_id: job_id.to_string(),
            follow,
            compact,
            job: snapshot.job,
            runner_job,
            events: projection.events,
            exit_code: projection.exit_code,
            orchestration_provenance: projection.orchestration_provenance,
            stdout: projection.stdout,
            stderr: projection.stderr,
            next_cursor: emitted_sequence,
            resume_command: Some(resume_command(runner_id, job_id, emitted_sequence, poll_ms)),
        },
        0,
    ))
}

fn emit_new_job_events(events: &[JobEvent], emitted_sequence: &mut u64) {
    for event in new_job_events(events, emitted_sequence) {
        eprintln!("{}", format_job_event(event));
    }
}

/// Return unseen events in sequence order and advance the durable follow cursor.
/// Replayed snapshots may overlap, duplicate, or arrive out of order after a
/// tunnel recovery; sequence is the daemon's monotonic event identity.
fn new_job_events<'a>(events: &'a [JobEvent], emitted_sequence: &mut u64) -> Vec<&'a JobEvent> {
    let mut ordered = events.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|event| event.sequence);
    let mut new_events = Vec::new();
    for event in ordered {
        if event.sequence <= *emitted_sequence {
            continue;
        }
        *emitted_sequence = event.sequence;
        new_events.push(event);
    }
    new_events
}

fn resume_command(runner_id: &str, job_id: &str, cursor: u64, poll_ms: u64) -> String {
    format!("homeboy runner job logs {runner_id} {job_id} --follow --cursor {cursor} --poll-ms {poll_ms}")
}

fn follow_recovery_error(
    runner_id: &str,
    job_id: &str,
    cursor: u64,
    error: homeboy::core::Error,
) -> homeboy::core::Error {
    let resume = resume_command(runner_id, job_id, cursor, 1000);
    homeboy::core::Error::validation_invalid_argument(
        "runner",
        format!(
            "runner log follow recovery was exhausted: {}. Resume with `{resume}`",
            error.message
        ),
        Some(runner_id.to_string()),
        None,
    )
}

fn classify_follow_error(
    runner_id: &str,
    job_id: &str,
    cursor: u64,
    error: homeboy::core::Error,
) -> homeboy::core::Error {
    let message = error.message.to_ascii_lowercase();
    if message.contains("404") || message.contains("not found") {
        return homeboy::core::Error::validation_invalid_argument(
            "job_id",
            format!("runner job `{job_id}` is absent after authoritative generation recovery; its retained log history was evicted or the job ID is invalid. Resume evidence is unavailable after cursor {cursor}"),
            Some(job_id.to_string()),
            None,
        );
    }
    follow_recovery_error(runner_id, job_id, cursor, error)
}

pub(super) fn format_job_event(event: &JobEvent) -> String {
    let kind = format!("{:?}", event.kind).to_ascii_lowercase();
    let message = event.message.as_deref().unwrap_or("");
    let data = event
        .data
        .as_ref()
        .map(|data| serde_json::to_string(data).unwrap_or_else(|_| "null".to_string()))
        .unwrap_or_default();
    match (message.is_empty(), data.is_empty()) {
        (true, true) => format!("#{:04} {}", event.sequence, kind),
        (false, true) => format!("#{:04} {} {}", event.sequence, kind, message),
        (true, false) => format!("#{:04} {} {}", event.sequence, kind, data),
        (false, false) => format!("#{:04} {} {} {}", event.sequence, kind, message, data),
    }
}

fn runner_job_terminal(status: JobStatus) -> bool {
    matches!(
        status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::api_jobs::JobEventKind;
    use uuid::Uuid;

    fn event(sequence: u64) -> JobEvent {
        JobEvent {
            sequence,
            job_id: Uuid::nil(),
            kind: JobEventKind::Progress,
            timestamp_ms: sequence,
            message: None,
            data: None,
        }
    }

    #[test]
    fn reconnect_replay_is_exactly_once_despite_duplicate_out_of_order_events() {
        let mut cursor = 2;
        let replay = vec![event(4), event(2), event(3), event(3)];

        let emitted = new_job_events(&replay, &mut cursor);

        assert_eq!(
            emitted
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
        assert_eq!(cursor, 4);
    }

    #[test]
    fn terminal_event_observed_after_outage_stops_following() {
        assert!(runner_job_terminal(JobStatus::Succeeded));
        assert!(runner_job_terminal(JobStatus::Failed));
        assert!(runner_job_terminal(JobStatus::Cancelled));
        assert!(!runner_job_terminal(JobStatus::Running));
    }

    #[test]
    fn cursor_resume_command_preserves_runner_job_and_polling_contract() {
        assert_eq!(
            resume_command("lab", "job-42", 99, 250),
            "homeboy runner job logs lab job-42 --follow --cursor 99 --poll-ms 250"
        );
    }

    #[test]
    fn missing_job_after_recovery_is_classified_as_retention_or_eviction() {
        let error = homeboy::core::Error::validation_invalid_argument(
            "job_id",
            "daemon request returned HTTP 404",
            None,
            None,
        );
        let classified = classify_follow_error("lab", "job-42", 9, error);

        assert!(classified.message.contains("evicted"));
        assert!(classified.message.contains("cursor 9"));
    }
}
