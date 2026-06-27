//! Test suite for the runner `execution` module, grouped by concern so each
//! test file stays under the structural item threshold.

use super::extension_parity::required_extensions_for_command;
use super::policy::{validate_runner_policy, RunnerPolicyRequest};
use super::*;
use crate::core::defaults::AgentTaskSecretSource;
use crate::core::error::ErrorCode;
use crate::core::server::{self, RunnerPolicy, RunnerSecretEnvRef, RunnerSettings};

mod exec;
mod handoff;
mod policy;
mod prepare;
mod redaction;
mod secret_source;

pub(super) fn ssh_runner() -> Runner {
    Runner {
        id: "lab".to_string(),
        kind: RunnerKind::Ssh,
        server_id: Some("srv".to_string()),
        workspace_root: Some("/srv/homeboy".to_string()),
        settings: RunnerSettings {
            daemon: true,
            ..Default::default()
        },
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: RunnerPolicy::default(),
    }
}

pub(super) fn local_runner(workspace_root: String) -> Runner {
    Runner {
        id: "local".to_string(),
        kind: RunnerKind::Local,
        server_id: None,
        workspace_root: Some(workspace_root),
        settings: RunnerSettings::default(),
        env: Default::default(),
        secret_env: Default::default(),
        resources: Default::default(),
        policy: RunnerPolicy::default(),
    }
}

pub(super) fn failed_runner_exec_output(stdout: &str, stderr: &str) -> RunnerExecOutput {
    RunnerExecOutput {
        variant: "exec",
        command: "runner.exec",
        runner_id: "lab".to_string(),
        dry_run: false,
        mode: RunnerExecMode::Daemon,
        argv: vec![
            "homeboy".to_string(),
            "extension".to_string(),
            "install".to_string(),
        ],
        remote_cwd: "/srv/homeboy/project".to_string(),
        exit_code: 2,
        stdout: stdout.to_string(),
        stderr: stderr.to_string(),
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
        capture: None,
        runner_result: None,
        handoff: None,
        diagnostics: None,
    }
}

pub(super) fn policy_request(options: &RunnerExecOptions) -> RunnerPolicyRequest<'_> {
    RunnerPolicyRequest {
        project_id: options.project_id.as_deref(),
        command: &options.command,
        capture_patch: options.capture_patch,
        raw_exec: options.raw_exec,
    }
}

struct EnvVarGuard {
    name: &'static str,
    prior: Option<String>,
}

impl EnvVarGuard {
    fn set(name: &'static str, value: &str) -> Self {
        let prior = std::env::var(name).ok();
        std::env::set_var(name, value);
        Self { name, prior }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match &self.prior {
            Some(value) => std::env::set_var(self.name, value),
            None => std::env::remove_var(self.name),
        }
    }
}

pub(super) fn json_file_source(path: &str, field: &str) -> AgentTaskSecretSource {
    AgentTaskSecretSource {
        source: "json-file".to_string(),
        env_var: None,
        path: Some(path.to_string()),
        scope: None,
        name: None,
        field: Some(field.to_string()),
        value: None,
    }
}
