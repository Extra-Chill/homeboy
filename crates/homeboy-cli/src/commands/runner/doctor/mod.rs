use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::agents::agent_tasks::provider::{
    AgentTaskExecutorProvider, AgentTaskProviderRunnerReadiness, AgentTaskProviderRunnerSource,
};
use homeboy::core::engine::shell;
use homeboy::core::server::{self, Server, SshClient};
use homeboy::runner::runners::{
    self as runner, daemon_repair_codes, Runner, RunnerKind, RunnerSession, RunnerToolRegistry,
    RunnerToolSpec, RunnerTunnelMode,
};
use serde::Serialize;

use crate::commands::CmdResult;

mod checks;
mod common;
mod extension_parity;
mod local;
mod probes;
mod remote;
mod repair;
mod target;
mod types;

pub use types::{RunnerDoctorOutput, RunnerDoctorStatus};

#[derive(Debug, Default)]
pub struct RunnerDoctorOptions {
    pub path: Option<String>,
    pub extensions: Vec<String>,
    pub required_tools: Vec<String>,
    pub agent_backend: Option<String>,
    pub agent_selector: Option<String>,
    pub scope: RunnerDoctorScope,
    pub repair: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunnerDoctorScope {
    #[default]
    General,
    LabOffload,
    SecretEnv,
}

pub fn run(runner_id: &str) -> CmdResult<RunnerDoctorOutput> {
    run_with_options(runner_id, RunnerDoctorOptions::default())
}

pub fn run_with_options(
    runner_id: &str,
    mut options: RunnerDoctorOptions,
) -> CmdResult<RunnerDoctorOutput> {
    options.scope = repair_scope(options.scope, options.repair);
    let target = target::resolve(runner_id)?;
    let mut report = match &target {
        target::RunnerTarget::Local { id, runner } => {
            // The local probe's artifact root is the only filesystem root this
            // command reads, so it is resolved once here at the entry point
            // rather than inside the probe. The SSH branch resolves its root on
            // the remote host and deliberately does not take this one.
            let artifact_root = crate::core::paths::artifact_root().ok();
            local::report(id, runner.as_ref(), &options, artifact_root.as_deref())
        }
        target::RunnerTarget::Ssh {
            id,
            runner,
            server,
            client,
        } => remote::report(id, runner, server, client, &options),
    };

    let migration = runner::secret_env_migration_plan(runner_id)?;
    report.secret_env_migration = (!migration.is_empty()).then_some(migration);

    if options.repair {
        repair::apply(&target, &options, &mut report);
    }

    // Doctor probes the runner directly. Its observation supersedes any
    // process-local capability answer, so the next execution preflight probes
    // the exact command environment rather than replaying stale remediation.
    runner::observe_runner_capabilities(runner_id);
    if options.scope == RunnerDoctorScope::LabOffload {
        let catalog = homeboy::agents::agent_tasks::provider::AgentTaskProviderCatalog::discover();
        let eligible_provider_ids = probes::eligible_provider_ids(
            catalog.providers(),
            options.agent_backend.as_deref(),
            options.agent_selector.as_deref(),
        );
        let (status, provider_readiness) =
            checks::lab_offload_status(&report.checks, &eligible_provider_ids);
        report.status = status;
        report.provider_readiness = Some(provider_readiness);
    } else {
        report.status = checks::overall_status(&report.checks);
    }
    let exit_code = report.status.operational_exit_code();
    Ok((report, exit_code))
}

/// A bare `--repair` is the Lab daemon recovery request emitted by runner
/// recovery guidance. Resolve it before probing so that command both diagnoses
/// and applies the repair it advertises.
pub(super) fn repair_scope(scope: RunnerDoctorScope, repair: bool) -> RunnerDoctorScope {
    if repair && scope == RunnerDoctorScope::General {
        RunnerDoctorScope::LabOffload
    } else {
        scope
    }
}

fn runner_summary(
    target_type: &'static str,
    runner: Option<&Runner>,
    server: Option<&Server>,
) -> types::RunnerTargetSummary {
    types::RunnerTargetSummary {
        target_type,
        registry: runner.map(|runner| types::RunnerRegistrySummary {
            id: runner.id.clone(),
            kind: runner.kind.clone(),
        }),
        server: server.map(|server| types::RunnerServerSummary {
            id: server.id.clone(),
            host: server.host.clone(),
            user: server.user.clone(),
            port: server.port,
            is_localhost: matches!(server.host.as_str(), "localhost" | "127.0.0.1" | "::1"),
        }),
    }
}

fn normalized_extension_ids(extension_ids: &[String]) -> Vec<String> {
    let mut ids = extension_ids
        .iter()
        .map(|id| id.trim())
        .filter(|id| !id.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn normalized_required_tools(commands: &[String]) -> Vec<String> {
    let mut tools = commands
        .iter()
        .map(|command| command.trim())
        .filter(|command| !command.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();
    tools.sort();
    tools.dedup();
    tools
}

#[cfg(test)]
mod tests;
