use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::command_contract::RunnerWorkload;
use crate::core::api_jobs::{Job, JobStatus, RemoteRunnerJobRequest, RunnerJobLifecycleMetadata};
use crate::core::error::{Error, Result};
use crate::core::redaction::redact_argv;
use crate::core::source_snapshot::SourceSnapshot;
use reqwest::blocking::Client;

use super::super::broker_http;
use super::super::evidence::mirror_reverse_broker_evidence;
use super::super::{Runner, RunnerJob};

#[allow(unused_imports)]
use super::*;

#[allow(clippy::too_many_arguments)]
pub(super) fn exec_via_reverse_broker(
    runner: &Runner,
    broker_url: &str,
    cwd: String,
    project_id: Option<String>,
    command: Vec<String>,
    env: HashMap<String, String>,
    secret_env_names: Vec<String>,
    capture_patch: bool,
    source_snapshot_override: Option<SourceSnapshot>,
    require_paths: Vec<String>,
    runner_workload: Option<RunnerWorkload>,
    run_id: Option<String>,
    detach_after_handoff: bool,
) -> Result<(RunnerExecOutput, i32)> {
    let client = Client::builder()
        .timeout(Duration::from_secs(10))
        .build()
        .map_err(|err| Error::internal_unexpected(format!("build broker HTTP client: {err}")))?;
    let source_snapshot = source_snapshot_override.unwrap_or_else(|| {
        SourceSnapshot::existing_remote(&runner.id, &cwd, runner.workspace_root.as_deref())
    });
    let redaction_env = env.clone();
    let redaction_secret_env_names = secret_env_names.clone();
    let request = RemoteRunnerJobRequest {
        runner_id: runner.id.clone(),
        project_id,
        operation: "runner.exec".to_string(),
        command: command.clone(),
        cwd: Some(cwd.clone()),
        env,
        secret_env_names,
        capture_patch,
        source_snapshot: Some(source_snapshot.clone()),
        runner_workload: runner_workload.clone(),
        metadata: Some(runner_exec_request_metadata(
            run_id.as_deref(),
            "reverse_broker",
        )),
        lifecycle: Some(RunnerJobLifecycleMetadata {
            source: Some("reverse-broker".to_string()),
            kind: Some("runner.exec".to_string()),
            durable_run_id: run_id.clone(),
            active_child_count: None,
            active_cell_count: None,
        }),
        require_paths: require_paths.clone(),
    };
    let broker_token = super::super::broker_auth::broker_token_from_env();
    let data = broker_http::post_json(
        &client,
        broker_url,
        "/runner/jobs",
        serde_json::to_value(&request).map_err(|err| {
            Error::internal_json(
                err.to_string(),
                Some("serialize reverse runner job request".to_string()),
            )
        })?,
        "submit reverse runner job",
        broker_token.as_deref(),
    )?;
    let job_value = data
        .get("job")
        .ok_or_else(|| Error::internal_unexpected("reverse broker submit returned no job"))?;
    let mut job: Job = serde_json::from_value(job_value.clone()).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse reverse broker job".to_string()),
        )
    })?;
    let persisted_run_id =
        persist_lab_offload_handoff_run(runner, &cwd, &command, &job, run_id.as_deref());
    if detach_after_handoff {
        return Ok(detached_handoff_output(
            runner,
            RunnerExecMode::ReverseBroker,
            cwd,
            command,
            source_snapshot,
            job,
            require_paths,
            persisted_run_id,
        ));
    }

    let deadline = Instant::now() + runner_exec_wait_timeout();
    while !matches!(
        job.status,
        JobStatus::Succeeded | JobStatus::Failed | JobStatus::Cancelled
    ) {
        if Instant::now() >= deadline {
            let events = fetch_daemon_events(&client, broker_url, &job.id.to_string())
                .map(|events| {
                    redact_runner_job_events(&events, &redaction_env, &redaction_secret_env_names)
                })
                .unwrap_or_default();
            return Err(daemon_job_wait_timeout(
                runner,
                &cwd,
                &command,
                &job,
                &events,
                "reverse runner job",
                true,
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
        let job_id = job.id.to_string();
        job = fetch_daemon_job_resilient(&client, broker_url, &job_id)
            .map_err(|err| daemon_job_context_error(&runner.id, &job_id, err))?;
    }
    let events = redact_runner_job_events(
        &fetch_daemon_events(&client, broker_url, &job.id.to_string())?,
        &redaction_env,
        &redaction_secret_env_names,
    );

    let RunnerJobResultFields {
        result,
        stdout,
        stderr,
        metrics,
        capture,
        exit_code,
    } = runner_job_result_fields(
        &events,
        job.status,
        &redaction_env,
        &redaction_secret_env_names,
    );

    let mirror = mirror_reverse_broker_evidence(
        runner,
        broker_url,
        &cwd,
        &command,
        &job,
        &events,
        &result,
        run_id.as_deref(),
    )?;
    let patch = mirror.as_ref().and_then(|evidence| evidence.patch.clone());
    let mirror_run_id = mirror.as_ref().map(|evidence| evidence.run.id.clone());
    let artifacts = job.artifacts.clone();
    let mutation_artifacts = mutation_artifacts_from_job(&job, &result);

    print_lab_offload_handoff(
        &runner.id,
        Some(&cwd),
        &job.id.to_string(),
        mirror_run_id.as_deref(),
        DaemonJobHandoffState::Terminal(job.status),
    );

    let runner_job = RunnerJob::from_job(&runner.id, "broker", &command, Some(cwd.clone()), &job);
    let runner_result = runner_result(
        Some(&job),
        exit_code,
        &stdout,
        &stderr,
        mirror_run_id.as_deref(),
        mutation_artifacts.clone(),
    );
    let handoff = runner_handoff(
        runner,
        "reverse_broker",
        Some(runner_job.clone()),
        Some(runner_result.clone()),
    );
    let execution_record = runner_execution_record_for_output(
        runner,
        "reverse_broker",
        exit_code,
        Some(job.id.to_string()),
        mirror_run_id.clone(),
        &artifacts,
        Some(&runner_result),
    );

    Ok((
        RunnerExecOutput {
            variant: "exec",
            command: "runner.exec",
            runner_id: runner.id.clone(),
            dry_run: false,
            mode: RunnerExecMode::ReverseBroker,
            argv: redact_argv(&command),
            remote_cwd: cwd,
            exit_code,
            stdout,
            stderr,
            source_snapshot: Some(source_snapshot.clone()),
            job_id: Some(job.id.to_string()),
            job: Some(job),
            runner_job: Some(runner_job),
            job_events: Some(events),
            mirror_run_id,
            patch,
            mutation_artifacts,
            artifacts,
            promoted_outputs: Vec::new(),
            structured_summaries: Vec::new(),
            metrics,
            capture,
            execution_record: Some(execution_record),
            runner_result: Some(runner_result),
            handoff: Some(handoff),
            diagnostics: runner_exec_diagnostics(runner, Some(&source_snapshot), &require_paths),
        },
        exit_code,
    ))
}
