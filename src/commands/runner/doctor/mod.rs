use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use homeboy::core::agent_tasks::provider::{
    AgentTaskProviderRunnerReadiness, AgentTaskProviderRunnerSource,
};
use homeboy::core::runners::{
    self as runner, Runner, RunnerKind, RunnerToolRegistry, RunnerToolSpec, RunnerTunnelMode,
};
use homeboy::core::server::{self, Server, SshClient};
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
    pub scope: RunnerDoctorScope,
    pub repair: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RunnerDoctorScope {
    #[default]
    General,
    LabOffload,
}

pub fn run(runner_id: &str) -> CmdResult<RunnerDoctorOutput> {
    run_with_options(runner_id, RunnerDoctorOptions::default())
}

pub fn run_with_options(
    runner_id: &str,
    options: RunnerDoctorOptions,
) -> CmdResult<RunnerDoctorOutput> {
    let target = target::resolve(runner_id)?;
    let mut report = match &target {
        target::RunnerTarget::Local { id, runner } => local::report(id, runner.as_ref(), &options),
        target::RunnerTarget::Ssh {
            id,
            runner,
            server,
            client,
        } => remote::report(id, runner, server, client, &options),
    };

    if options.repair {
        repair::apply(&target, &options, &mut report);
    }

    report.status = checks::overall_status(&report.checks);
    Ok((report, 0))
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
