use clap::{Args, Subcommand};
use homeboy::cli_surface::current_command_surface_doctor_report;
use homeboy::core::build_identity;
use homeboy::core::engine;
use homeboy::core::runners::{self as runner, Runner, RunnerKind, RunnerStatusReport};
use homeboy::core::self_status::{self, ControllerRuntimeInput, RunnerRuntimeInput};
use serde_json::Value;

use crate::commands::{resources, CmdResult, GlobalArgs};

#[derive(Args)]
pub struct SelfArgs {
    #[command(subcommand)]
    pub command: SelfCommand,
}

#[derive(Subcommand)]
pub enum SelfCommand {
    /// Report active binary, version, and nearby install/update signals
    Status(SelfStatusArgs),
    /// Report the active binary build identity without external probes
    Identity(SelfIdentityArgs),
    /// Report one authoritative binary/runtime view across the controller and
    /// every configured runner, including version drift signals and host
    /// resource pressure (machine load, hot Homeboy-adjacent processes, rig
    /// leases)
    Doctor(SelfDoctorArgs),
    /// Plan or delete orphaned Homeboy runtime temp entries
    CleanupRuntimeTmp(SelfCleanupRuntimeTmpArgs),
}

#[derive(Args)]
pub struct SelfStatusArgs {}

#[derive(Args)]
pub struct SelfIdentityArgs {}

#[derive(Args)]
pub struct SelfDoctorArgs {}

#[derive(Args)]
pub struct SelfCleanupRuntimeTmpArgs {
    /// Delete planned temp entries. Without this flag, only reports the plan.
    #[arg(long)]
    pub apply: bool,
    /// Only include entries older than this many days.
    #[arg(long, default_value_t = 7)]
    pub older_than_days: u64,
    /// Only include entries whose directory/file name starts with this prefix.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Maximum temp entries to inspect in one invocation.
    #[arg(long, default_value_t = 1000)]
    pub limit: usize,
}

pub fn run(args: SelfArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    match args.command {
        SelfCommand::Status(_) => {
            let status = self_status::collect_status();
            let json = serde_json::to_value(status)
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
        SelfCommand::Identity(_) => {
            let json = serde_json::to_value(build_identity::current())
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
        SelfCommand::Doctor(_) => {
            let view = self_status::build_runtime_view(
                collect_controller_input(),
                collect_runner_inputs(),
            );
            let command_surface = current_command_surface_doctor_report();
            // Host resource pressure is diagnostic context, not a drift signal,
            // so it is reported alongside the runtime/command-surface view
            // without affecting the agreement exit code.
            let (host_resources, _) = resources::run(resources::ResourcesArgs {})?;
            let exit_code = if view.agrees && command_surface.agrees {
                0
            } else {
                1
            };
            let mut json = serde_json::to_value(view)
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            if let Value::Object(ref mut object) = json {
                object.insert("agrees".to_string(), Value::Bool(exit_code == 0));
                if let Some(Value::Array(notes)) = object.get_mut("drift_notes") {
                    notes.extend(
                        command_surface
                            .drift_notes
                            .iter()
                            .cloned()
                            .map(Value::String),
                    );
                }
                object.insert(
                    "command_surface".to_string(),
                    serde_json::to_value(command_surface)
                        .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?,
                );
                object.insert(
                    "resources".to_string(),
                    serde_json::to_value(host_resources)
                        .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?,
                );
            }
            Ok((json, exit_code))
        }
        SelfCommand::CleanupRuntimeTmp(args) => {
            let output = engine::temp::cleanup_runtime_tmp(
                args.apply,
                args.older_than_days,
                args.prefix.as_deref(),
                args.limit,
            )?;
            let json = serde_json::to_value(output)
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            Ok((json, 0))
        }
    }
}

/// Collect the controller-side authoritative binary facts. Reuses the existing
/// `self status` collector for install-method detection and source-checkout
/// probing so the runtime view and `self status` never disagree about the
/// active binary.
fn collect_controller_input() -> ControllerRuntimeInput {
    let status = self_status::collect_status();
    let binary_path = if status.active_binary == "unknown" {
        None
    } else {
        Some(status.active_binary)
    };
    ControllerRuntimeInput {
        binary_path,
        version: status.active_version,
        build_identity: status.active_build_identity,
        install_method: status.install_method,
        source_checkout: status.source_checkout,
    }
}

/// Collect one runtime input per configured runner. Pairs each runner's
/// configured executable path with the identity of its active daemon session
/// (when connected) so the assembler can flag version skew and stale daemons in
/// a single view. Probes are best-effort: a runner that fails to load or has no
/// session simply reports `connected: false`.
fn collect_runner_inputs() -> Vec<RunnerRuntimeInput> {
    let Ok(runners) = runner::list() else {
        return Vec::new();
    };
    let statuses = runner::statuses().unwrap_or_default();

    runners
        .into_iter()
        .map(|runner| {
            let status = statuses.iter().find(|report| report.runner_id == runner.id);
            runner_runtime_input(runner, status)
        })
        .collect()
}

fn runner_runtime_input(runner: Runner, status: Option<&RunnerStatusReport>) -> RunnerRuntimeInput {
    let connected = status.map(|report| report.connected).unwrap_or(false);
    let session = status
        .filter(|report| report.connected)
        .and_then(|report| report.session.as_ref());
    let daemon_version = session.map(|session| session.homeboy_version.clone());
    let daemon_build_identity = session.and_then(|session| session.homeboy_build_identity.clone());
    let daemon_drift = status
        .map(|report| report.stale_daemon.is_some())
        .unwrap_or(false);

    RunnerRuntimeInput {
        runner_id: runner.id,
        kind: runner_kind_label(&runner.kind).to_string(),
        server_id: runner.server_id,
        configured_binary_path: runner.settings.homeboy_path,
        connected,
        daemon_version,
        daemon_build_identity,
        daemon_drift,
    }
}

fn runner_kind_label(kind: &RunnerKind) -> &'static str {
    match kind {
        RunnerKind::Local => "local",
        RunnerKind::Ssh => "ssh",
    }
}
