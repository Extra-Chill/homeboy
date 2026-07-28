use clap::{Args, Subcommand};
use homeboy::cli_surface::current_command_surface_doctor_report;
use homeboy::core::build_identity;
use homeboy::core::cleanup;
use homeboy::core::engine;
use homeboy::core::{api_jobs::JobStore, paths};
use homeboy::runner::runners::{self as runner, Runner, RunnerKind, RunnerStatusReport};
use homeboy_upgrade::self_status::{self, ControllerRuntimeInput, RunnerRuntimeInput};
use serde_json::Value;

use crate::commands::{docs, resources, CmdResult};

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
    /// Display CLI documentation
    Docs(docs::DocsArgs),
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
    /// Defaults to the configured `retention.runtime_tmp_days`.
    #[arg(long)]
    pub older_than_days: Option<u64>,
    /// Only include entries whose directory/file name starts with this prefix.
    #[arg(long)]
    pub prefix: Option<String>,
    /// Maximum temp entries to inspect in one invocation.
    /// Defaults to the configured `retention.limit`.
    #[arg(long)]
    pub limit: Option<i64>,
    /// Maximum aggregate bytes retained for failed runtime run evidence.
    /// Defaults to the configured `retention.runtime_run_max_bytes`.
    #[arg(long)]
    pub run_max_bytes: Option<u64>,
    /// Maximum failed runtime run directories retained.
    /// Defaults to the configured `retention.runtime_run_max_count`.
    #[arg(long)]
    pub run_max_count: Option<usize>,
    /// Continue bounded runtime-run inspection from a prior next_cursor.
    #[arg(long)]
    pub cursor: Option<String>,
}

pub fn run(args: SelfArgs) -> CmdResult<Value> {
    match args.command {
        SelfCommand::Status(_) => {
            let status = self_status::collect_status_read_only();
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
                let daemon_memory_owners = paths::daemon_jobs_file()
                    .and_then(JobStore::retained_owner_report_at_path)
                    .unwrap_or_else(|error| serde_json::json!({
                        "error": error.to_string(),
                        "guidance": "Inspect the daemon job store before restarting the controller."
                    }));
                object.insert("daemon_memory_owners".to_string(), daemon_memory_owners);
            }
            Ok((json, exit_code))
        }
        SelfCommand::CleanupRuntimeTmp(args) => {
            // Resolved by the shared policy so this specialist and `homeboy
            // cleanup --include runtime-tmp` cannot honor different windows
            // (#10316). The clap defaults used to be literal `7`/`1000`/`1
            // GiB`/`100`, so a widened `retention.runtime_tmp_days` was honored
            // by the aggregate and silently ignored here.
            let policy = cleanup::resolve_cleanup_policy(cleanup::CleanupPolicyOverrides {
                limit: args.limit,
                runtime_tmp_days: args.older_than_days,
                runtime_run_max_bytes: args.run_max_bytes,
                runtime_run_max_count: args.run_max_count,
                ..cleanup::CleanupPolicyOverrides::default()
            })?;
            let output = engine::temp::cleanup_runtime_tmp_bounded(
                engine::temp::RuntimeTempCleanupOptions {
                    apply: args.apply,
                    older_than_days: policy.runtime_tmp_days,
                    prefix: args.prefix.as_deref(),
                    limit: policy.scan_limit(),
                    run_max_bytes: policy.runtime_run_max_bytes,
                    run_max_count: policy.runtime_run_max_count,
                    cursor: args.cursor.as_deref(),
                },
            )?;
            let mut json = serde_json::to_value(output)
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            if let Value::Object(ref mut object) = json {
                object.insert(
                    "retention".to_string(),
                    serde_json::to_value(policy)
                        .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?,
                );
            }
            Ok((json, 0))
        }
        SelfCommand::Docs(args) => {
            let (output, exit_code) = docs::run(args)?;
            let json = serde_json::to_value(output)
                .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
            Ok((json, exit_code))
        }
    }
}

pub fn is_docs_markdown(args: &SelfArgs) -> bool {
    matches!(&args.command, SelfCommand::Docs(docs_args) if !docs::is_json_mode(docs_args))
}

pub fn run_docs_markdown(args: SelfArgs) -> CmdResult<String> {
    match args.command {
        SelfCommand::Docs(docs_args) => docs::run_markdown(docs_args),
        _ => Err(homeboy::core::Error::validation_invalid_argument(
            "output_mode",
            "Only `homeboy self docs` supports markdown output under `self`",
            None,
            None,
        )),
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

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::SelfCleanupRuntimeTmpArgs;

    #[derive(Parser)]
    struct CleanupRuntimeTmpCli {
        #[command(flatten)]
        args: SelfCleanupRuntimeTmpArgs,
    }

    #[test]
    fn runtime_tmp_budgets_default_to_the_configured_policy() {
        // Regression for #10316: these were `default_value_t` literals (7 days,
        // 1000 entries, 1 GiB, 100 runs) that shadowed
        // `retention.runtime_tmp_days` and friends. An operator who widened the
        // configured window still had this command delete at 7 days while
        // `homeboy cleanup --include runtime-tmp` honored the configuration.
        let cli = CleanupRuntimeTmpCli::parse_from(["cleanup-runtime-tmp"]);
        assert_eq!(cli.args.older_than_days, None);
        assert_eq!(cli.args.limit, None);
        assert_eq!(cli.args.run_max_bytes, None);
        assert_eq!(cli.args.run_max_count, None);
        assert_eq!(cli.args.prefix, None);
        assert!(!cli.args.apply);
    }

    #[test]
    fn runtime_tmp_budgets_still_accept_explicit_overrides() {
        let cli = CleanupRuntimeTmpCli::parse_from([
            "cleanup-runtime-tmp",
            "--older-than-days",
            "2",
            "--limit",
            "9",
            "--run-max-bytes",
            "1024",
            "--run-max-count",
            "3",
        ]);
        assert_eq!(cli.args.older_than_days, Some(2));
        assert_eq!(cli.args.limit, Some(9));
        assert_eq!(cli.args.run_max_bytes, Some(1024));
        assert_eq!(cli.args.run_max_count, Some(3));
    }
}
