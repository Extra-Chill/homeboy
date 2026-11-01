use super::*;
use crate::{
    Runner, RunnerActiveJobState, RunnerExecMode, RunnerExecOutput, RunnerKind, RunnerSessionState,
    RunnerStatusReport,
};
use homeboy_core::server::RunnerSettings;
use homeboy_core::Result;
use homeboy_upgrade::upgrade::ExtensionUpgradeEntry;
use std::collections::HashMap;

pub(super) fn extension_update(extension_id: &str, source_revision: &str) -> ExtensionUpgradeEntry {
    ExtensionUpgradeEntry {
        extension_id: extension_id.to_string(),
        old_version: "1.0.0".to_string(),
        new_version: "1.0.0".to_string(),
        linked: true,
        source_path: Some(format!(
            "/Users/user/Developer/homeboy-extensions/{extension_id}"
        )),
        git_root: Some("/Users/user/Developer/homeboy-extensions".to_string()),
        source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
        source_revision: Some(source_revision.to_string()),
        source_update: Default::default(),
    }
}

pub(super) fn runner_status(runner_id: &str) -> Result<RunnerStatusReport> {
    Ok(RunnerStatusReport {
        runner_id: runner_id.to_string(),
        connected: false,
        state: RunnerSessionState::Disconnected,
        session: None,
        stale_daemon: None,
        configured_job_binary_build_identity: None,
        daemon_freshness: Some(homeboy_core::daemon::DaemonFreshnessReport {
            fresh: true,
            stale_reason_code: None,
            restartable: false,
            lease_id: Some("lease-test".to_string()),
            pid: Some(42),
            recovery_evidence: Some(homeboy_core::daemon::DaemonRecoveryEvidence::Recoverable),
            ownership_evidence: None,
            adoption_command: None,
            binary_hash: None,
            daemon_version: None,
            daemon_build_identity: None,
            runtime_paths: None,
            active_jobs: 0,
            termination_evidence: None,
            repair_plan: Vec::new(),
        }),
        active_jobs: Vec::new(),
        active_runner_jobs: Vec::new(),
        active_job_count: 0,
        stale_runner_jobs: Vec::new(),
        stale_runner_job_count: 0,
        active_job_state: RunnerActiveJobState::NotQueried,
        active_job_source: None,
        active_job_error: None,
        active_job_recovery_evidence: None,
        session_path: "/tmp/homeboy-runner-session.json".to_string(),
    })
}

pub(super) fn ssh_runner(id: &str, homeboy_path: Option<&str>) -> Runner {
    Runner {
        id: id.to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some(format!("{id}-server")),
        workspace_root: Some("/home/user/workspace".to_string()),
        settings: RunnerSettings {
            homeboy_path: homeboy_path.map(str::to_string),
            ..Default::default()
        },
        env: HashMap::new(),
        secret_env: HashMap::new(),
        resources: HashMap::new(),
        policy: Default::default(),
    }
}

pub(super) fn exec_output(
    runner_id: &str,
    argv: Vec<String>,
    stdout: &str,
    stderr: &str,
    exit_code: i32,
) -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: runner_id.to_string(),
        dry_run: false,
        mode: RunnerExecMode::DiagnosticSsh,
        argv,
        remote_cwd: "/home/user/workspace".to_string(),
        exit_code,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
        source_snapshot: None,
        job: None,
        runner_job: None,
        job_id: None,
        job_events: None,
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

mod part_c;
