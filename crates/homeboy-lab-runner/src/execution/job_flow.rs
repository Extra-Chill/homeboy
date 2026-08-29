//! Transport-neutral lifecycle for an accepted runner job.
//!
//! Direct-daemon and reverse-broker transports differ at their HTTP boundaries,
//! but an accepted job has one controller lifecycle: bind, persist or detach,
//! poll, mirror, and assemble the caller-visible result.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use homeboy_core::api_jobs::{Job, JobEvent, RunnerJobLogSnapshot};
use homeboy_core::error::Result;
use homeboy_core::lab_contract::{JobArtifactMetadata, LabRunnerWorkload};
use homeboy_core::redaction::redact_argv;
use homeboy_core::runner_execution_envelope::PathMaterializationPlan;
use homeboy_core::source_snapshot::SourceSnapshot;

use super::*;

pub(super) struct MirroredJobEvidence {
    pub(super) run_id: String,
    pub(super) patch: Option<serde_json::Value>,
    pub(super) artifacts: Vec<JobArtifactMetadata>,
}

pub(super) struct SubmittedRunnerJobFlow<'a> {
    pub(super) runner: &'a Runner,
    pub(super) mode: RunnerExecMode,
    pub(super) transport: &'static str,
    pub(super) runner_job_transport: &'static str,
    pub(super) timeout_label: &'static str,
    pub(super) cwd: String,
    pub(super) command: Vec<String>,
    pub(super) redaction_env: &'a HashMap<String, String>,
    pub(super) secret_env_names: &'a [String],
    pub(super) source_snapshot: SourceSnapshot,
    pub(super) path_materialization_plan: Option<PathMaterializationPlan>,
    pub(super) require_paths: Vec<String>,
    pub(super) lab_runner_workload: Option<LabRunnerWorkload>,
    pub(super) run_id: Option<String>,
    pub(super) run_id_owns_generic_exec: bool,
    pub(super) detach_after_handoff: bool,
    pub(super) mirror_evidence: bool,
    pub(super) print_handoff_output: bool,
    pub(super) handoff_endpoint: Option<&'a str>,
}

#[expect(
    clippy::too_many_arguments,
    reason = "Transport hooks keep HTTP and lease semantics outside the shared lifecycle."
)]
pub(super) fn complete_submitted_runner_job<
    Accepted,
    AfterHandoff,
    Poll,
    Events,
    Mirror,
    AfterEvents,
    Finalize,
>(
    flow: SubmittedRunnerJobFlow<'_>,
    mut job: Job,
    mut accepted: Accepted,
    mut after_handoff: AfterHandoff,
    mut poll: Poll,
    mut events: Events,
    mirror: Mirror,
    mut after_events: AfterEvents,
    mut finalize: Finalize,
) -> Result<(RunnerExecOutput, i32)>
where
    Accepted: FnMut(&Job) -> Result<()>,
    AfterHandoff: FnMut(&Job) -> Result<()>,
    Poll: FnMut(&Job) -> Result<Job>,
    Events: FnMut(&Job) -> Result<Vec<JobEvent>>,
    Mirror: FnOnce(&Job, &[JobEvent], &serde_json::Value) -> Result<Option<MirroredJobEvidence>>,
    AfterEvents: FnMut() -> Result<()>,
    Finalize: FnMut(&Job, &[JobArtifactMetadata]) -> Result<()>,
{
    if let Some(run_id) = flow.run_id.as_deref() {
        let binding = if flow.run_id_owns_generic_exec {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_job_identity(
                run_id,
                &flow.runner.id,
                &job.id.to_string(),
                &flow.cwd,
                &flow.command,
            )
            .map(|_| ())
        } else {
            homeboy_agents::agent_task_lifecycle::bind_accepted_lab_runner_job(
                &homeboy_core::lab_contract::RunnerJobIdentity::new(
                    run_id,
                    &flow.runner.id,
                    job.id.to_string(),
                ),
                &flow.cwd,
                &flow.command,
            )
            .map(|_| ())
        };
        binding.map_err(|error| {
            super::accepted_handoff_persistence_error(error, &flow.runner.id, &job.id.to_string())
        })?;
    }
    accepted(&job)?;
    persist_runner_execution_transition(
        &RunnerExecutionRecord::in_flight(
            job.id.to_string(),
            flow.runner.id.clone(),
            flow.transport,
        )
        .with_job_id(job.id.to_string())
        .with_path_materialization_plan(flow.path_materialization_plan.clone())
        .with_orchestration_provenance(orchestration_target_provenance(
            flow.runner,
            None,
            Some(&flow.source_snapshot),
            &[],
        ))
        .with_next_actions(runner_execution_next_actions(
            &flow.runner.id,
            &job.id.to_string(),
        )),
        &flow.cwd,
        &flow.command,
    )?;
    let persisted_run_id = flow
        .mirror_evidence
        .then(|| {
            persist_lab_offload_handoff_run(
                flow.runner,
                &flow.cwd,
                &flow.command,
                &job,
                flow.run_id.as_deref(),
                flow.run_id_owns_generic_exec,
                flow.handoff_endpoint,
            )
        })
        .flatten();
    validate_generic_exec_mirror_run_id(
        flow.run_id_owns_generic_exec,
        flow.run_id.as_deref(),
        persisted_run_id.as_deref(),
    )?;
    after_handoff(&job)?;
    if flow.detach_after_handoff {
        return Ok(detached_handoff_output(
            flow.runner,
            flow.mode,
            flow.cwd,
            flow.command,
            flow.source_snapshot,
            job,
            flow.path_materialization_plan,
            flow.require_paths,
            flow.run_id,
            persisted_run_id,
        ));
    }

    let deadline = Instant::now() + runner_exec_wait_timeout();
    let mut reported_progress_sequence = 0;
    while !job.status.is_terminal() {
        if let Some(status) = flow.run_id.as_deref().and_then(|run_id| {
            observed_agent_task_terminal_job_status(run_id, flow.run_id_owns_generic_exec)
        }) {
            // The agent-task lifecycle owns provider terminality. A stale runner
            // job projection must not hold Cook in dispatch after its aggregate
            // and artifacts are already durable on the controller.
            job.status = status;
            break;
        }
        let job_id = job.id.to_string();
        if Instant::now() >= deadline {
            let events = events(&job)
                .map(|events| {
                    redact_runner_job_events(&events, flow.redaction_env, flow.secret_env_names)
                })
                .unwrap_or_default();
            record_and_report_promotion_progress_frames(
                flow.run_id.as_deref(),
                &job_id,
                &events,
                &mut reported_progress_sequence,
            );
            return Err(daemon_job_wait_timeout(
                flow.runner,
                &flow.cwd,
                &flow.command,
                &job,
                &events,
                flow.timeout_label,
                true,
            ));
        }
        std::thread::sleep(Duration::from_millis(200));
        job = poll(&job)?;
        if let Ok(events) = events(&job) {
            let events =
                redact_runner_job_events(&events, flow.redaction_env, flow.secret_env_names);
            record_and_report_promotion_progress_frames(
                flow.run_id.as_deref(),
                &job_id,
                &events,
                &mut reported_progress_sequence,
            );
        }
    }
    let job_id = job.id.to_string();
    let mut job_events = events(&job).map(|events| {
        redact_runner_job_events(&events, flow.redaction_env, flow.secret_env_names)
    })?;
    record_and_report_promotion_progress_frames(
        flow.run_id.as_deref(),
        &job_id,
        &job_events,
        &mut reported_progress_sequence,
    );
    append_agent_task_lifecycle_workload_event(
        &mut job_events,
        flow.lab_runner_workload.as_ref(),
        &flow.runner.id,
        &job_id,
    )?;
    append_agent_task_dispatch_handoff_workload_event(
        &mut job_events,
        flow.lab_runner_workload.as_ref(),
        &flow.runner.id,
        &job_id,
    );
    let RunnerJobResultFields {
        result,
        stdout,
        stderr,
        metrics,
        capture,
        exit_code,
    } = runner_job_result_fields(
        &job_events,
        job.status,
        flow.redaction_env,
        flow.secret_env_names,
    );
    let terminal_snapshot = RunnerJobLogSnapshot {
        job: job.clone(),
        events: job_events.clone(),
    };
    if flow.run_id_owns_generic_exec {
        if let Some(run_id) = flow.run_id.as_deref() {
            homeboy_agents::agent_task_lifecycle::record_runner_exec_terminal_checkpoint(
                run_id,
                &terminal_snapshot,
            )?;
        }
    }
    after_events()?;
    let mirrored = if flow.mirror_evidence {
        mirror(&job, &job_events, &result).map_err(|error| {
            if flow.run_id_owns_generic_exec {
                if let Some(run_id) = flow.run_id.as_deref() {
                    let _ =
                        homeboy_agents::agent_task_lifecycle::record_runner_exec_projection_failure(
                            run_id,
                            &terminal_snapshot,
                            &error,
                        );
                }
            }
            error
        })?
    } else {
        None
    };
    let patch = mirrored
        .as_ref()
        .and_then(|evidence| evidence.patch.clone());
    let mirror_run_id = mirrored.as_ref().map(|evidence| evidence.run_id.clone());
    validate_generic_exec_mirror_run_id(
        flow.run_id_owns_generic_exec,
        flow.run_id.as_deref(),
        mirror_run_id.as_deref(),
    )?;
    fire_runner_direct_notification(
        flow.run_id.as_deref(),
        &job,
        flow.lab_runner_workload
            .as_ref()
            .and_then(|workload| workload.notification_route.as_ref()),
    );
    let artifacts = mirrored
        .map(|evidence| evidence.artifacts)
        .unwrap_or_default();
    finalize(&job, &artifacts)?;
    let mutation_artifacts = mutation_artifacts_from_job(&job, &result);
    if flow.print_handoff_output {
        print_lab_offload_handoff(
            &flow.runner.id,
            Some(&flow.cwd),
            &job_id,
            mirror_run_id.as_deref(),
            DaemonJobHandoffState::Terminal(job.status),
        );
    }
    let runner_job = RunnerJob::from_job(
        &flow.runner.id,
        flow.runner_job_transport,
        &flow.command,
        Some(flow.cwd.clone()),
        &job,
    );
    let mut runner_result = runner_result(
        Some(&job),
        exit_code,
        &stdout,
        &stderr,
        mirror_run_id.as_deref(),
        mutation_artifacts.clone(),
    );
    runner_result.artifact_refs = artifacts
        .iter()
        .map(crate::session::runner_artifact_ref_from_metadata)
        .collect();
    let provenance_extensions = required_extensions_for_command(
        &flow.command,
        &super::super::workload::merge_lab_runner_workload_required_extensions(
            Vec::new(),
            flow.lab_runner_workload.as_ref(),
        ),
    );
    let handoff = lab_runner_handoff(
        flow.runner,
        flow.transport,
        Some(runner_job.clone()),
        Some(runner_result.clone()),
    );
    let execution_record = runner_execution_record_for_output(
        flow.runner,
        flow.transport,
        exit_code,
        Some(job_id.clone()),
        mirror_run_id.clone(),
        Some(&flow.source_snapshot),
        flow.path_materialization_plan,
        &flow.require_paths,
        &provenance_extensions,
        &artifacts,
        Some(&runner_result),
    );
    persist_runner_execution_transition(&execution_record, &flow.cwd, &flow.command)?;
    let diagnostics = runner_exec_diagnostics(
        flow.runner,
        Some(&flow.source_snapshot),
        &flow.require_paths,
    );
    let mut output = RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: flow.runner.id.clone(),
        dry_run: false,
        mode: flow.mode,
        argv: redact_argv(&flow.command),
        remote_cwd: flow.cwd,
        exit_code,
        stdout,
        stderr,
        source_snapshot: Some(flow.source_snapshot),
        job_id: Some(job_id),
        job: Some(job),
        runner_job: Some(runner_job),
        job_events: Some(job_events),
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
        diagnostics,
    };
    for hint in result
        .get("diagnostic_hints")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
    {
        append_runner_exec_diagnostic_hint(&mut output, Some(hint.to_string()));
    }
    Ok((output, exit_code))
}

fn observed_agent_task_terminal_job_status(
    run_id: &str,
    run_id_owns_generic_exec: bool,
) -> Option<JobStatus> {
    if run_id_owns_generic_exec {
        return None;
    }
    let state = homeboy_agents::agent_task_lifecycle::persisted_status(run_id)
        .ok()?
        .state;
    agent_task_terminal_job_status(state)
}

fn agent_task_terminal_job_status(
    state: homeboy_agents::agent_task_lifecycle::AgentTaskRunState,
) -> Option<JobStatus> {
    use homeboy_agents::agent_task_lifecycle::AgentTaskRunState;
    match state {
        AgentTaskRunState::Succeeded
        | AgentTaskRunState::CandidateRecoverable
        | AgentTaskRunState::PartialRecoverable => Some(JobStatus::Succeeded),
        AgentTaskRunState::PartialFailure | AgentTaskRunState::Failed => Some(JobStatus::Failed),
        AgentTaskRunState::Cancelled => Some(JobStatus::Cancelled),
        AgentTaskRunState::Queued | AgentTaskRunState::Running => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_agents::agent_task_lifecycle::AgentTaskRunState;

    #[test]
    fn agent_task_terminal_state_bounds_stale_runner_job_polling() {
        for state in [
            AgentTaskRunState::Succeeded,
            AgentTaskRunState::CandidateRecoverable,
            AgentTaskRunState::PartialRecoverable,
        ] {
            assert_eq!(
                agent_task_terminal_job_status(state),
                Some(JobStatus::Succeeded)
            );
        }
        for state in [AgentTaskRunState::PartialFailure, AgentTaskRunState::Failed] {
            assert_eq!(
                agent_task_terminal_job_status(state),
                Some(JobStatus::Failed)
            );
        }
        assert_eq!(
            agent_task_terminal_job_status(AgentTaskRunState::Cancelled),
            Some(JobStatus::Cancelled)
        );
        for state in [AgentTaskRunState::Queued, AgentTaskRunState::Running] {
            assert_eq!(agent_task_terminal_job_status(state), None);
        }
    }
}
