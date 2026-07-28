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
            response: runner::reconcile_terminal_jobs(runner_id)?,
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
            let result = runner::runner_job_cancel_for_session(&session, job_id);
            runner::close_reconnected_job_log_owner(&session);
            result?
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
    const MAX_RECONNECTS: u8 = 3;
    let mut reconnects = 0;
    let mut recovery_session = None;
    let mut snapshot = match runner_job_log_snapshot(runner_id, job_id) {
        Ok(snapshot) => snapshot,
        Err(_) => {
            reconnects += 1;
            eprintln!("runner log transport lost; reconnecting to the authoritative job generation ({reconnects}/{MAX_RECONNECTS})");
            let session =
                runner::reconnect_job_log_owner(runner_id, job_id).map_err(|reconnect_error| {
                    follow_recovery_error(
                        runner_id,
                        job_id,
                        emitted_sequence,
                        poll_ms,
                        reconnect_error,
                    )
                })?;
            match runner::runner_job_log_snapshot_for_session(&session, job_id) {
                Ok(snapshot) => {
                    recovery_session = Some(session);
                    snapshot
                }
                Err(recovery_error) => {
                    runner::close_reconnected_job_log_owner(&session);
                    return Err(classify_follow_error(
                        runner_id,
                        job_id,
                        emitted_sequence,
                        poll_ms,
                        recovery_error,
                    ));
                }
            }
        }
    };

    emit_new_job_events(&snapshot.events, &mut emitted_sequence);
    let stop = Arc::new(AtomicBool::new(false));
    if follow {
        // The handler only requests a cooperative exit; it never cancels the
        // remote job. The printed cursor is sufficient to resume exactly once.
        homeboy_process::install_shutdown_handler(stop.clone(), "runner job log follow")?;
    }
    while follow && !runner_job_terminal(snapshot.job.status) {
        std::thread::sleep(poll_interval);
        if stop.load(Ordering::SeqCst) {
            eprintln!(
                "follow interrupted; resume with `{}`",
                resume_command(runner_id, job_id, emitted_sequence, poll_ms)
            );
            break;
        }
        let snapshot_result = match recovery_session.as_ref() {
            Some(session) => runner::runner_job_log_snapshot_for_session(session, job_id),
            None => runner_job_log_snapshot(runner_id, job_id),
        };
        snapshot = match snapshot_result {
            Ok(snapshot) => snapshot,
            Err(_error) if reconnects < MAX_RECONNECTS => {
                reconnects += 1;
                eprintln!("runner log transport lost; reconnecting to the authoritative job generation ({reconnects}/{MAX_RECONNECTS})");
                if let Some(session) = recovery_session.take() {
                    runner::close_reconnected_job_log_owner(&session);
                }
                let session = runner::reconnect_job_log_owner(runner_id, job_id).map_err(
                    |reconnect_error| {
                        follow_recovery_error(
                            runner_id,
                            job_id,
                            emitted_sequence,
                            poll_ms,
                            reconnect_error,
                        )
                    },
                )?;
                match runner::runner_job_log_snapshot_for_session(&session, job_id) {
                    Ok(snapshot) => {
                        recovery_session = Some(session);
                        snapshot
                    }
                    Err(recovery_error) => {
                        runner::close_reconnected_job_log_owner(&session);
                        return Err(classify_follow_error(
                            runner_id,
                            job_id,
                            emitted_sequence,
                            poll_ms,
                            recovery_error,
                        ));
                    }
                }
            }
            Err(error) => {
                if let Some(session) = recovery_session.take() {
                    runner::close_reconnected_job_log_owner(&session);
                }
                return Err(follow_recovery_error(
                    runner_id,
                    job_id,
                    emitted_sequence,
                    poll_ms,
                    error,
                ));
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
    // Follow renders each unseen event to stderr immediately. Keep the final
    // JSON envelope event-free so its terminal response cannot replay them.
    let events = if follow {
        Vec::new()
    } else {
        job_events_after_cursor(snapshot.events, cursor.unwrap_or(0))
    };
    let projection = super::log_projection::project_job_log(events, compact, tail_bytes);
    if let Some(session) = recovery_session.take() {
        runner::close_reconnected_job_log_owner(&session);
    }

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

fn job_events_after_cursor(events: Vec<JobEvent>, cursor: u64) -> Vec<JobEvent> {
    let mut cursor = cursor;
    new_job_events(&events, &mut cursor)
        .into_iter()
        .cloned()
        .collect()
}

fn resume_command(runner_id: &str, job_id: &str, cursor: u64, poll_ms: u64) -> String {
    format!("homeboy runner job logs {runner_id} {job_id} --follow --cursor {cursor} --poll-ms {poll_ms}")
}

fn follow_recovery_error(
    runner_id: &str,
    job_id: &str,
    cursor: u64,
    poll_ms: u64,
    error: homeboy::core::Error,
) -> homeboy::core::Error {
    let resume = resume_command(runner_id, job_id, cursor, poll_ms);
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
    poll_ms: u64,
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
    follow_recovery_error(runner_id, job_id, cursor, poll_ms, error)
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
    use homeboy::runner::runners::{RunnerSession, RunnerSessionRole, RunnerTunnelMode};
    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::process::Command;
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

    fn job(status: JobStatus, event_count: usize) -> homeboy::core::api_jobs::Job {
        serde_json::from_value(serde_json::json!({
            "id": Uuid::nil(),
            "operation": "runner.exec",
            "status": status,
            "created_at_ms": 1,
            "updated_at_ms": event_count,
            "event_count": event_count,
        }))
        .expect("test job")
    }

    fn session(url: String, tunnel_pid: Option<u32>) -> RunnerSession {
        RunnerSession {
            runner_id: "lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: RunnerSessionRole::Controller,
            server_id: Some("lab".to_string()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:44000".to_string()),
            local_port: None,
            local_url: Some(url),
            tunnel_pid,
            remote_daemon_pid: Some(4242),
            remote_daemon_lease_id: Some("lease-authoritative".to_string()),
            homeboy_version: "test".to_string(),
            homeboy_build_identity: Some("homeboy test+authoritative".to_string()),
            connected_at: "2026-01-01T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    fn read_request(stream: &mut std::net::TcpStream) -> String {
        let mut request = [0; 4096];
        let length = stream.read(&mut request).expect("read daemon request");
        String::from_utf8(request[..length].to_vec()).expect("daemon request text")
    }

    fn write_daemon_response(stream: &mut std::net::TcpStream, body: serde_json::Value) {
        let body = serde_json::json!({ "success": true, "data": { "body": body } }).to_string();
        stream
            .write_all(
                format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                )
                .as_bytes(),
            )
            .expect("write daemon response");
    }

    fn serve_dropped_daemon_request() -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind dropped daemon");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("dropped daemon address")
        );
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept dropped daemon request");
            assert!(read_request(&mut stream).starts_with("GET /jobs/"));
            // A tunnel loss closes the transport before the daemon response.
        });
        (url, handle)
    }

    fn serve_authoritative_daemon(
        snapshots: Vec<(homeboy::core::api_jobs::Job, Vec<JobEvent>)>,
    ) -> (String, std::thread::JoinHandle<()>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind authoritative daemon");
        let url = format!(
            "http://{}",
            listener.local_addr().expect("authoritative daemon address")
        );
        let handle = std::thread::spawn(move || {
            for (job, events) in snapshots {
                let (mut job_stream, _) = listener.accept().expect("accept job request");
                assert!(read_request(&mut job_stream).starts_with("GET /jobs/"));
                write_daemon_response(&mut job_stream, serde_json::json!({ "job": job }));

                let (mut event_stream, _) = listener.accept().expect("accept event request");
                assert!(read_request(&mut event_stream).starts_with("GET /jobs/"));
                write_daemon_response(&mut event_stream, serde_json::json!({ "events": events }));
            }
        });
        (url, handle)
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
    fn cursor_excludes_replayed_history_from_the_result_payload() {
        let events = vec![event(4), event(2), event(3), event(3)];

        assert_eq!(
            job_events_after_cursor(events, 2)
                .iter()
                .map(|event| event.sequence)
                .collect::<Vec<_>>(),
            vec![3, 4]
        );
    }

    #[test]
    fn exhausted_recovery_preserves_the_requested_poll_interval() {
        let error = homeboy::core::Error::validation_invalid_argument(
            "runner",
            "tunnel closed",
            None,
            None,
        );

        let recovered = follow_recovery_error("lab", "job-42", 9, 250, error);

        assert!(recovered.message.contains("--cursor 9 --poll-ms 250"));
    }

    #[cfg(unix)]
    #[test]
    fn dropped_transport_reconnects_to_authoritative_generation_without_replaying_events() {
        let (dropped_url, dropped_server) = serve_dropped_daemon_request();
        let dropped = session(dropped_url, None);
        assert!(runner::runner_job_log_snapshot_for_session(&dropped, "job-42").is_err());
        dropped_server.join().expect("dropped daemon joins");

        let (authoritative_url, authoritative_server) = serve_authoritative_daemon(vec![
            (job(JobStatus::Queued, 1), vec![event(1)]),
            (job(JobStatus::Running, 2), vec![event(1), event(2)]),
            (
                job(JobStatus::Succeeded, 3),
                vec![event(1), event(2), event(3)],
            ),
        ]);
        let mut tunnel = Command::new("sh")
            .args(["-c", "sleep 60"])
            .spawn()
            .expect("recovery tunnel process");
        let authoritative = session(authoritative_url, Some(tunnel.id()));
        let mut cursor = 0;
        let mut rendered = Vec::new();

        for expected_status in [JobStatus::Queued, JobStatus::Running, JobStatus::Succeeded] {
            let snapshot = runner::runner_job_log_snapshot_for_session(&authoritative, "job-42")
                .expect("authoritative generation snapshot");
            assert_eq!(snapshot.job.status, expected_status);
            rendered.extend(
                new_job_events(&snapshot.events, &mut cursor)
                    .into_iter()
                    .map(|event| event.sequence),
            );
        }

        assert_eq!(rendered, vec![1, 2, 3]);
        assert_eq!(cursor, 3);
        assert!(runner_job_terminal(JobStatus::Succeeded));
        runner::close_reconnected_job_log_owner(&authoritative);
        assert!(!tunnel.wait().expect("recovery tunnel exits").success());
        authoritative_server
            .join()
            .expect("authoritative daemon joins");
    }

    #[test]
    fn missing_job_after_recovery_is_classified_as_retention_or_eviction() {
        let error = homeboy::core::Error::validation_invalid_argument(
            "job_id",
            "daemon request returned HTTP 404",
            None,
            None,
        );
        let classified = classify_follow_error("lab", "job-42", 9, 250, error);

        assert!(classified.message.contains("evicted"));
        assert!(classified.message.contains("cursor 9"));
    }
}
