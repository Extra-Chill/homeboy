//! Unit tests for the Lab offload module, split by concern.

pub(super) use super::super::super::lab_capabilities::lab_runner_capability_contract;
pub(super) use super::super::super::lab_env::build_lab_offload_env;
pub(super) use super::super::super::lab_plan::base_lab_plan;
pub(super) use super::super::super::lab_selection::{
    fail_if_no_default_runner_accepts_jobs_with, resolve_lab_runner_selection_from_placement,
};
pub(super) use super::super::super::lab_workspaces::{
    workspace_mapping_entry, LAB_WORKSPACE_MAPPING_SCHEMA,
};
pub(super) use super::*;
pub(super) use crate::engine::command::{CaptureMetadata, CommandCaptureMetadata};
pub(super) use crate::observation::{LAB_OFFLOAD_METADATA_ENV, SOURCE_SNAPSHOT_METADATA_ENV};
pub(super) use crate::plan::PlanKind;
pub(super) use crate::runner::verify_lab_workspace_from_env;
pub(super) use crate::runner::{
    RunnerActiveJobSource, RunnerActiveJobState, RunnerAvailability, RunnerExecMode,
    RunnerExecOutput, RunnerRequiredTool, RunnerSession, RunnerSessionState,
    RunnerStaleDaemonWarning, RunnerTunnelMode, RunnerWorkspaceMaterializationPlan,
    RunnerWorkspaceSyncOutput,
};
pub(super) use crate::runner_execution_envelope::{
    PathMaterializationEntry, PathMaterializationPlan, PATH_MATERIALIZATION_MODE_SNAPSHOT,
    PATH_MATERIALIZATION_OWNER_LAB_EXECUTION_CONTEXT, PATH_MATERIALIZATION_STATUS_MATERIALIZED,
};

mod capability_metadata;
mod durable_fallbacks;
mod exec_errors;
mod selection;
mod workspace_sync;

pub(super) fn portable_lab_command(label: &'static str) -> LabOffloadCommand {
    LabOffloadCommand {
        command: crate::lab_contract::LabCommandContract::portable(label, None, true, &[]),
        required_extensions: Vec::new(),
        required_capabilities: Vec::new(),
        workload: None,
    }
}

pub(super) fn local_only_lab_command(reason: &'static str) -> LabOffloadCommand {
    LabOffloadCommand {
        command: crate::lab_contract::LabCommandContract::local_only("rig up", reason),
        required_extensions: Vec::new(),
        required_capabilities: Vec::new(),
        workload: None,
    }
}

pub(super) fn release_gate_lab_command(label: &'static str) -> LabOffloadCommand {
    let mut command = portable_lab_command(label);
    command.routing_policy.release_gate = true;
    command
}

pub(super) fn reverse_status(runner_id: &str) -> RunnerStatusReport {
    RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: true,
        state: RunnerSessionState::Connected,
        session: Some(RunnerSession {
            runner_id: runner_id.to_string(),
            mode: RunnerTunnelMode::Reverse,
            role: super::super::super::RunnerSessionRole::Controller,
            server_id: None,
            controller_id: Some("controller".to_string()),
            broker_url: Some("http://127.0.0.1:9876".to_string()),
            remote_daemon_address: None,
            local_port: None,
            local_url: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            remote_daemon_lease_id: None,
            homeboy_version: "homeboy 0.0.0".to_string(),
            homeboy_build_identity: Some("homeboy 0.0.0+test".to_string()),
            connected_at: "2026-06-03T00:00:00Z".to_string(),
            worker_identity: Some("worker-1".to_string()),
            worker_pid: Some(1234),
            last_seen_at: Some(chrono::Utc::now().to_rfc3339()),
            leaseless_recovery_evidence: None,
        }),
        stale_daemon: None,
        daemon_freshness: None,
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::Available,
        active_job_source: Some(RunnerActiveJobSource::ReverseBroker),
        active_job_error: None,
        session_path: "/tmp/lab.json".to_string(),
    }
}

pub(super) fn stale_reverse_status(runner_id: &str) -> RunnerStatusReport {
    let mut status = reverse_status(runner_id);
    status.stale_daemon = Some(RunnerStaleDaemonWarning::new(
        runner_id,
        "homeboy 0.228.0".to_string(),
        "homeboy 0.229.11".to_string(),
        Some("homeboy 0.228.0+old".to_string()),
        Some("homeboy 0.229.11+new".to_string()),
    ));
    status
}

pub(super) fn truncated_runner_exec_output() -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "execution",
        command: "runner.exec",
        runner_id: "homeboy-lab".to_string(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: vec!["homeboy".to_string(), "status".to_string()],
        remote_cwd: "/tmp/homeboy-workspace".to_string(),
        exit_code: 0,
        stdout: "retained stdout tail".to_string(),
        stderr: String::new(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: Some("job-123".to_string()),
        job_events: None,
        mirror_run_id: None,
        patch: None,
        mutation_artifacts: None,
        artifacts: Vec::new(),
        promoted_outputs: Vec::new(),
        structured_summaries: Vec::new(),
        metrics: None,
        capture: Some(crate::engine::command::CommandCaptureMetadata {
            stdout: crate::engine::command::CaptureMetadata {
                bytes_seen: 5 * 1024 * 1024,
                bytes_retained: 4 * 1024 * 1024,
                byte_limit: 4 * 1024 * 1024,
                truncated: true,
            },
            stderr: crate::engine::command::CaptureMetadata {
                bytes_seen: 0,
                bytes_retained: 0,
                byte_limit: 4 * 1024 * 1024,
                truncated: false,
            },
        }),
        execution_record: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}
