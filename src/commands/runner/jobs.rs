use std::time::Duration;

use homeboy::core::api_jobs::{JobEvent, JobStatus};
use homeboy::core::runners::{self as runner, runner_job_log_snapshot};

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
            compact,
            tail_kb,
        } => job_logs(&runner_id, &job_id, follow, poll_ms, compact, tail_kb).map_job_daemon(),
        RunnerJobCommand::Cancel { runner_id, job_id } => {
            job_cancel(&runner_id, &job_id).map_job_daemon()
        }
        RunnerJobCommand::Reconcile { runner_id } => job_reconcile(&runner_id),
        RunnerJobCommand::Artifacts {
            runner_id,
            job_id,
            artifact_id,
        } => job_artifacts(&runner_id, &job_id, &artifact_id),
    }
}

trait RunnerJobCommandResultExt {
    fn map_job_daemon(self) -> CmdResult<RunnerJobCommandOutput>;
}

impl RunnerJobCommandResultExt for CmdResult<RunnerJobOutput> {
    fn map_job_daemon(self) -> CmdResult<RunnerJobCommandOutput> {
        self.map(|(output, exit_code)| (RunnerJobCommandOutput::Daemon(output), exit_code))
    }
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
    let (job, events) = homeboy::core::runners::runner_job_cancel(runner_id, job_id)?;
    let runner_job = homeboy::core::runners::RunnerJob::from_job(
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
            stdout: None,
            stderr: None,
        },
        0,
    ))
}

fn job_logs(
    runner_id: &str,
    job_id: &str,
    follow: bool,
    poll_ms: u64,
    compact: bool,
    tail_kb: Option<usize>,
) -> CmdResult<RunnerJobOutput> {
    let poll_interval = Duration::from_millis(poll_ms.max(100));
    let mut emitted_sequence = 0;
    let mut snapshot = runner_job_log_snapshot(runner_id, job_id)?;

    emit_new_job_events(&snapshot.events, &mut emitted_sequence);
    while follow && !runner_job_terminal(snapshot.job.status) {
        std::thread::sleep(poll_interval);
        snapshot = runner_job_log_snapshot(runner_id, job_id)?;
        emit_new_job_events(&snapshot.events, &mut emitted_sequence);
    }
    let runner_job = homeboy::core::runners::RunnerJob::from_job(
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
            stdout: projection.stdout,
            stderr: projection.stderr,
        },
        0,
    ))
}

fn emit_new_job_events(events: &[JobEvent], emitted_sequence: &mut u64) {
    for event in events {
        if event.sequence <= *emitted_sequence {
            continue;
        }
        eprintln!("{}", format_job_event(event));
        *emitted_sequence = event.sequence;
    }
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
