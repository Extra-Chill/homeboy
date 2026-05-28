use clap::{ArgMatches, Command, CommandFactory, FromArgMatches};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use homeboy::cli_surface::Cli;
use homeboy::cli_surface::Commands;
use homeboy::commands::GlobalArgs;

use homeboy::commands;
use homeboy::commands::cli;
use homeboy::commands::utils::{args, entity_suggest, resource_policy, response as output};
use homeboy::core::extension::load_all_extensions;

mod lab_offload_extension_parity;
#[cfg(test)]
mod reverse_lab_offload_tests;

struct ExtensionCliCommand {
    tool: String,
    project_id: String,
    args: Vec<String>,
}

struct ExtensionCliInfo {
    tool: String,
    display_name: String,
    extension_name: String,
    project_id_help: Option<String>,
    args_help: Option<String>,
    examples: Vec<String>,
}

fn collect_extension_cli_info() -> Vec<ExtensionCliInfo> {
    load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            m.cli.map(|cli| {
                let help = cli.help.unwrap_or_default();
                ExtensionCliInfo {
                    tool: cli.tool,
                    display_name: cli.display_name,
                    extension_name: m.name,
                    project_id_help: help.project_id_help,
                    args_help: help.args_help,
                    examples: help.examples,
                }
            })
        })
        .collect()
}

fn build_augmented_command(extension_info: &[ExtensionCliInfo]) -> Command {
    let mut cmd = Cli::command();

    for info in extension_info {
        let project_id_help = info
            .project_id_help
            .clone()
            .unwrap_or_else(|| "Project ID".to_string());
        let args_help = info
            .args_help
            .clone()
            .unwrap_or_else(|| "Command arguments".to_string());

        let mut subcommand = Command::new(info.tool.clone())
            .about(format!(
                "Run {} commands via {}",
                info.display_name, info.extension_name
            ))
            .arg(
                clap::Arg::new("project_id")
                    .help(project_id_help)
                    .required(true)
                    .index(1),
            )
            .arg(
                clap::Arg::new("args")
                    .help(args_help)
                    .index(2)
                    .num_args(0..)
                    .allow_hyphen_values(true),
            )
            .trailing_var_arg(true);

        if !info.examples.is_empty() {
            let examples_text = format!("Examples:\n  {}", info.examples.join("\n  "));
            subcommand = subcommand.after_help(examples_text);
        }

        cmd = cmd.subcommand(subcommand);
    }

    cmd
}

fn try_parse_extension_cli_command(
    matches: &ArgMatches,
    extension_info: &[ExtensionCliInfo],
) -> Option<ExtensionCliCommand> {
    let (tool, sub_matches) = matches.subcommand()?;

    if !extension_info.iter().any(|m| m.tool == tool) {
        return None;
    }

    let project_id = sub_matches.get_one::<String>("project_id")?.clone();
    let args: Vec<String> = sub_matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Some(ExtensionCliCommand {
        tool: tool.to_string(),
        project_id,
        args,
    })
}

fn main() -> std::process::ExitCode {
    let extension_info = collect_extension_cli_info();
    let cmd = build_augmented_command(&extension_info);

    let args: Vec<String> = std::env::args().collect();
    let normalized = args::normalize(args);

    let matches = match cmd.try_get_matches_from(normalized.clone()) {
        Ok(m) => m,
        Err(e) => {
            if let Some(output) = try_augment_clap_error(&e) {
                eprintln!("{}", output);
                return std::process::ExitCode::from(2);
            }
            e.exit();
        }
    };

    let global = GlobalArgs {};

    // Extract --output early so it's available for all code paths (including
    // extension CLI commands which exit before Cli::from_arg_matches).
    let mut output_file: Option<String> = matches
        .try_get_one::<std::path::PathBuf>("output")
        .ok()
        .flatten()
        .map(|path| path.to_string_lossy().to_string());

    let artifact_root_override = matches
        .try_get_one::<std::path::PathBuf>("artifact_root")
        .ok()
        .flatten()
        .cloned();
    homeboy::core::set_artifact_root_override(artifact_root_override.clone());

    if let Some(extension_cmd) = try_parse_extension_cli_command(&matches, &extension_info) {
        let cli_args = cli::CliArgs {
            tool: extension_cmd.tool,
            identifier: extension_cmd.project_id,
            args: extension_cmd.args,
        };
        let result = cli::run(cli_args, &global);

        let (json_result, exit_code) = output::map_cmd_result_to_json(result);
        emit_json_result(json_result, output_file.as_deref(), exit_code);
        return std::process::ExitCode::from(exit_code_to_u8(exit_code));
    }

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    match resolve_lab_runner_selection(&cli.command, cli.runner.as_deref(), cli.force_hot) {
        Ok(Some(selection)) => {
            if matches!(selection.source, LabRunnerSelectionSource::Default) {
                eprintln!(
                    "Lab offload: auto-selected default {} runner `{}`.",
                    selection.mode.label(),
                    selection.runner_id
                );
            }

            match prepare_lab_runner_for_offload(&selection) {
                Ok(LabRunnerPreparation::Ready) => {
                    let capture_patch = cli.command.lab_offload_mutation_flag().is_some();
                    return run_lab_offload(
                        &selection.runner_id,
                        selection.source,
                        &cli.command,
                        &normalized,
                        output_file.as_deref(),
                        capture_patch,
                    );
                }
                Ok(LabRunnerPreparation::FallBackLocal { reason }) => {
                    homeboy::core::runner::capture_lab_offload_metadata(
                        homeboy::core::runner::lab_offload_metadata(
                            match selection.source {
                                LabRunnerSelectionSource::Explicit => "explicit",
                                LabRunnerSelectionSource::Default => "automatic",
                            },
                            Some(&selection.runner_id),
                            Some(selection.mode.metadata_value()),
                            "fallback",
                            None,
                            Some(&reason),
                        ),
                    );
                    eprintln!("Lab offload: {reason}; running locally.");
                    // Continue into the normal local command path below.
                }
                Err(err) => {
                    emit_json_result(Err(err), output_file.as_deref(), 2);
                    return std::process::ExitCode::from(exit_code_to_u8(2));
                }
            }
        }
        Ok(None) => {
            if cli.command.supports_lab_runner() {
                homeboy::core::runner::capture_lab_offload_metadata(
                    homeboy::core::runner::lab_offload_metadata(
                        "automatic",
                        None,
                        None,
                        "skipped",
                        None,
                        Some(if cli.force_hot {
                            "force_hot"
                        } else {
                            "no_default_runner"
                        }),
                    ),
                );
            }
        }
        Err(err) => {
            emit_json_result(Err(err), output_file.as_deref(), 2);
            return std::process::ExitCode::from(exit_code_to_u8(2));
        }
    }

    homeboy::core::set_artifact_root_override(cli.artifact_root.clone().or(artifact_root_override));

    if matches!(&cli.command, Commands::Runs(args) if args.is_bundle_export()) {
        output_file = None;
    }

    if let Some(hot_command) = resource_policy::hot_command(&cli.command) {
        if let Ok((resources, _)) = homeboy::commands::doctor::resources::run(
            homeboy::commands::doctor::resources::ResourcesArgs {},
        ) {
            let warning = resource_policy::evaluate(hot_command, &resources);
            if let Some(warning) = warning.as_ref() {
                if !cli.force_hot {
                    eprintln!("{}", warning.message);
                }
            }
            // Persist the preflight resource policy decision so observation
            // runs (bench, lint, test, etc.) can record it in their metadata
            // for later interpretation. This stays generic to Homeboy core.
            resource_policy::capture_context(
                resource_policy::ResourcePolicyContext::from_evaluation(
                    hot_command,
                    &resources,
                    warning.as_ref(),
                    cli.force_hot,
                ),
            );
        }
    }

    // Startup update checks — skip for upgrade (it handles this itself)
    if !matches!(
        &cli.command,
        Commands::Upgrade(_) | Commands::Daemon(_) | Commands::SelfCmd(_)
    ) {
        homeboy::core::upgrade::update_check::run_startup_check();
        homeboy::core::extension::update_check::run_startup_check();
    }

    if matches!(cli.command, Commands::List) {
        let mut cmd = build_augmented_command(&extension_info);
        cmd.print_help().expect("Failed to print help");
        println!();
        return std::process::ExitCode::SUCCESS;
    }

    // Show help for changelog when neither subcommand nor --self is provided
    if let Commands::Changelog(ref args) = cli.command {
        if args.command.is_none() && !args.show_self {
            let cmd = build_augmented_command(&extension_info);
            if let Some(mut changelog_cmd) = cmd.find_subcommand("changelog").cloned() {
                changelog_cmd.print_help().expect("Failed to print help");
                println!();
                return std::process::ExitCode::SUCCESS;
            }
        }
    }

    let exit_code = commands::response::run(cli.command, &global, output_file.as_deref());

    std::process::ExitCode::from(exit_code_to_u8(exit_code))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LabRunnerSelectionSource {
    Explicit,
    Default,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LabRunnerSelection {
    runner_id: String,
    source: LabRunnerSelectionSource,
    mode: homeboy::core::runner::RunnerTunnelMode,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum LabRunnerPreparation {
    Ready,
    FallBackLocal { reason: String },
}

const AUTO_LAB_CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const EXPLICIT_LAB_CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

fn prepare_lab_runner_for_offload(
    selection: &LabRunnerSelection,
) -> homeboy::core::Result<LabRunnerPreparation> {
    let runner = homeboy::core::runner::load(&selection.runner_id)?;
    if runner.kind != homeboy::core::runner::RunnerKind::Ssh {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote direct SSH or reverse-connected runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register a direct SSH runner or configure a reverse-connected runner before using Lab offload.".to_string()
            ]),
        ));
    }

    prepare_lab_runner_for_offload_with(selection, homeboy::core::runner::status, |runner_id| {
        connect_runner_for_offload(runner_id, selection.source)
    })
}

fn connect_runner_for_offload(
    runner_id: &str,
    source: LabRunnerSelectionSource,
) -> homeboy::core::Result<(homeboy::core::runner::RunnerConnectReport, i32)> {
    let timeout = match source {
        LabRunnerSelectionSource::Explicit => EXPLICIT_LAB_CONNECT_TIMEOUT,
        LabRunnerSelectionSource::Default => AUTO_LAB_CONNECT_TIMEOUT,
    };
    let (stdout, stderr, exit_code, timed_out) = run_runner_connect_command(runner_id, timeout)?;
    let status = homeboy::core::runner::status(runner_id)?;

    if status.connected {
        if let Some(session) = status.session {
            return Ok((
                homeboy::core::runner::RunnerConnectReport {
                    runner_id: runner_id.to_string(),
                    mode: Some(session.mode),
                    role: Some(session.role),
                    connected: true,
                    recorded: None,
                    local_url: session.local_url,
                    broker_url: session.broker_url,
                    controller_id: session.controller_id,
                    remote_daemon_address: session.remote_daemon_address,
                    tunnel_pid: session.tunnel_pid,
                    remote_daemon_pid: session.remote_daemon_pid,
                    homeboy_version: Some(session.homeboy_version),
                    session_path: Some(status.session_path),
                    failure_kind: None,
                    failure_message: None,
                },
                0,
            ));
        }
    }

    let reason = if timed_out {
        format!("runner connect timed out after {}s", timeout.as_secs())
    } else {
        let detail = if stderr.trim().is_empty() {
            stdout.trim()
        } else {
            stderr.trim()
        };
        if detail.is_empty() {
            format!("runner connect exited with code {exit_code}")
        } else {
            format!("runner connect exited with code {exit_code}: {detail}")
        }
    };

    Ok((
        homeboy::core::runner::RunnerConnectReport {
            runner_id: runner_id.to_string(),
            mode: None,
            role: None,
            connected: false,
            recorded: None,
            local_url: None,
            broker_url: None,
            controller_id: None,
            remote_daemon_address: None,
            tunnel_pid: None,
            remote_daemon_pid: None,
            homeboy_version: None,
            session_path: Some(status.session_path),
            failure_kind: Some(homeboy::core::runner::RunnerFailureKind::SshFailure),
            failure_message: Some(reason),
        },
        exit_code,
    ))
}

fn run_runner_connect_command(
    runner_id: &str,
    timeout: Duration,
) -> homeboy::core::Result<(String, String, i32, bool)> {
    let exe = std::env::current_exe().map_err(|err| {
        homeboy::core::Error::internal_io(
            err.to_string(),
            Some("resolve homeboy executable".into()),
        )
    })?;
    let mut child = std::process::Command::new(exe)
        .args(["runner", "connect", runner_id])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            homeboy::core::Error::internal_io(err.to_string(), Some("start runner connect".into()))
        })?;
    let deadline = std::time::Instant::now() + timeout;

    loop {
        if let Some(status) = child.try_wait().map_err(|err| {
            homeboy::core::Error::internal_io(err.to_string(), Some("wait runner connect".into()))
        })? {
            let mut stdout = String::new();
            if let Some(mut pipe) = child.stdout.take() {
                let _ = pipe.read_to_string(&mut stdout);
            }
            let mut stderr = String::new();
            if let Some(mut pipe) = child.stderr.take() {
                let _ = pipe.read_to_string(&mut stderr);
            }
            return Ok((stdout, stderr, status.code().unwrap_or(-1), false));
        }

        if std::time::Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Ok((String::new(), String::new(), 124, true));
        }

        std::thread::sleep(Duration::from_millis(50));
    }
}

fn prepare_lab_runner_for_offload_with(
    selection: &LabRunnerSelection,
    status_fn: impl Fn(&str) -> homeboy::core::Result<homeboy::core::runner::RunnerStatusReport>,
    connect_fn: impl Fn(
        &str,
    ) -> homeboy::core::Result<(homeboy::core::runner::RunnerConnectReport, i32)>,
) -> homeboy::core::Result<LabRunnerPreparation> {
    let status = status_fn(&selection.runner_id)?;
    if status.connected {
        eprintln!(
            "Lab offload: runner `{}` is connected via {} mode.",
            selection.runner_id,
            status_tunnel_mode(&status).label()
        );
        return Ok(LabRunnerPreparation::Ready);
    }

    if status_tunnel_mode(&status) == homeboy::core::runner::RunnerTunnelMode::Reverse {
        let reason = format!(
            "reverse-connected runner `{}` is not currently connected",
            selection.runner_id
        );
        return match selection.source {
            LabRunnerSelectionSource::Default => Ok(LabRunnerPreparation::FallBackLocal { reason }),
            LabRunnerSelectionSource::Explicit => {
                Err(homeboy::core::Error::validation_invalid_argument(
                    "runner",
                    format!("Lab offload requires reverse runner `{}` to have an active reverse session", selection.runner_id),
                    Some(selection.runner_id.clone()),
                    Some(vec![
                        "Start the reverse runner session on the Lab machine before using --runner.".to_string(),
                        "Use --force-hot to run the command locally instead of offloading.".to_string(),
                    ]),
                ))
            }
        };
    }

    eprintln!(
        "Lab offload: direct SSH runner `{}` is not connected; attempting connection.",
        selection.runner_id
    );
    let (report, _) = connect_fn(&selection.runner_id)?;
    if report.connected {
        return Ok(LabRunnerPreparation::Ready);
    }

    let reason = report
        .failure_message
        .unwrap_or_else(|| "runner connection did not become ready".to_string());

    match selection.source {
        LabRunnerSelectionSource::Default => Ok(LabRunnerPreparation::FallBackLocal { reason }),
        LabRunnerSelectionSource::Explicit => {
            Err(homeboy::core::Error::validation_invalid_argument(
                "runner",
                format!(
                    "Lab offload could not connect runner `{}` before execution: {reason}",
                    selection.runner_id
                ),
                Some(selection.runner_id.clone()),
                Some(vec![
                    format!(
                        "Run `homeboy runner connect {}` for full diagnostics.",
                        selection.runner_id
                    ),
                    "Use --force-hot to run the command locally instead of offloading.".to_string(),
                ]),
            ))
        }
    }
}

fn resolve_lab_runner_selection(
    command: &Commands,
    explicit_runner: Option<&str>,
    force_hot: bool,
) -> homeboy::core::Result<Option<LabRunnerSelection>> {
    let default_runner = if explicit_runner.is_none() && !force_hot && command.supports_lab_runner()
    {
        homeboy::core::runner::resolve_default_lab_runner()?
    } else {
        None
    };

    resolve_lab_runner_selection_from_default(command, explicit_runner, force_hot, default_runner)
}

fn resolve_lab_runner_selection_from_default(
    command: &Commands,
    explicit_runner: Option<&str>,
    force_hot: bool,
    default_runner: Option<String>,
) -> homeboy::core::Result<Option<LabRunnerSelection>> {
    if let Some(runner_id) = explicit_runner {
        if !command.supports_lab_runner() {
            let reason = command.lab_runner_unsupported_reason();
            let message = reason.map_or_else(
                || "--runner is only supported for hot Lab-offload commands: lint, test, audit, bench, trace, and refactor source runs".to_string(),
                |reason| format!("--runner is unavailable for this hot command. {reason}"),
            );
            return Err(homeboy::core::Error::validation_invalid_argument(
                "runner",
                message,
                Some(runner_id.to_string()),
                Some(vec!["Current Lab offload support: audit, bench run, full lint, full test, trace, and refactor source runs.".to_string()]),
            ));
        }

        return Ok(Some(LabRunnerSelection {
            runner_id: runner_id.to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: runner_status_tunnel_mode(runner_id),
        }));
    }

    if force_hot || !command.supports_lab_runner() {
        return Ok(None);
    }

    default_runner
        .map(|runner_id| {
            Ok(LabRunnerSelection {
                mode: runner_status_tunnel_mode(&runner_id),
                runner_id,
                source: LabRunnerSelectionSource::Default,
            })
        })
        .transpose()
}

fn runner_status_tunnel_mode(runner_id: &str) -> homeboy::core::runner::RunnerTunnelMode {
    homeboy::core::runner::status(runner_id).map_or(
        homeboy::core::runner::RunnerTunnelMode::DirectSsh,
        |status| status_tunnel_mode(&status),
    )
}

fn status_tunnel_mode(
    status: &homeboy::core::runner::RunnerStatusReport,
) -> homeboy::core::runner::RunnerTunnelMode {
    status.session.as_ref().map_or(
        homeboy::core::runner::RunnerTunnelMode::DirectSsh,
        |session| session.mode.clone(),
    )
}

fn exit_code_to_u8(code: i32) -> u8 {
    if code <= 0 {
        0
    } else if code >= 255 {
        255
    } else {
        code as u8
    }
}

fn emit_json_result(
    result: homeboy::core::Result<serde_json::Value>,
    output_file: Option<&str>,
    exit_code: i32,
) {
    if let Some(path) = output_file {
        output::write_json_to_file(&result, path, exit_code);
    }
    output::print_json_result(result, exit_code).ok();
}

fn run_lab_offload(
    runner_id: &str,
    source: LabRunnerSelectionSource,
    command: &Commands,
    normalized_args: &[String],
    output_file: Option<&str>,
    capture_patch: bool,
) -> std::process::ExitCode {
    match run_lab_offload_inner(
        runner_id,
        source,
        command,
        normalized_args,
        output_file,
        capture_patch,
    ) {
        Ok(exit_code) => std::process::ExitCode::from(exit_code_to_u8(exit_code)),
        Err(err) => {
            let (json_result, exit_code) =
                output::map_cmd_result_to_json::<serde_json::Value>(Err(err));
            emit_json_result(json_result, output_file, exit_code);
            std::process::ExitCode::from(exit_code_to_u8(exit_code))
        }
    }
}

fn run_lab_offload_inner(
    runner_id: &str,
    source: LabRunnerSelectionSource,
    command_kind: &Commands,
    normalized_args: &[String],
    output_file: Option<&str>,
    capture_patch: bool,
) -> homeboy::core::Result<i32> {
    let runner = homeboy::core::runner::load(runner_id)?;
    let status = homeboy::core::runner::status(runner_id)?;
    if runner.kind != homeboy::core::runner::RunnerKind::Ssh {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote direct SSH or reverse-connected runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register a direct SSH runner or configure a reverse-connected runner first."
                    .to_string(),
            ]),
        ));
    }

    if !status.connected {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            format!(
                "Lab offload requires a connected {} runner daemon",
                status_tunnel_mode(&status).label()
            ),
            Some(runner_id.to_string()),
            Some(vec![format!(
                "Connect runner `{runner_id}` before using --runner."
            )]),
        ));
    }

    runner.workspace_root.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "workspace_root",
            "Lab offload requires runner.workspace_root so the local checkout can be mapped remotely",
            Some(runner.id.clone()),
            Some(vec![
                "This Wave 3 adapter assumes workspace sync/provenance has placed the same checkout basename under runner.workspace_root.".to_string(),
            ]),
        )
    })?;
    let source_path = lab_offload_source_path(normalized_args)?;
    let capability_preflight = lab_runner_capability_preflight(command_kind, &source_path);
    let capability_plan_preflight = lab_offload_extension_parity::after_capability_plan();
    let sync_mode =
        if homeboy::core::runner::lab_offload_changed_since_ref(normalized_args).is_some() {
            homeboy::core::runner::RunnerWorkspaceSyncMode::Git
        } else {
            homeboy::core::runner::RunnerWorkspaceSyncMode::Snapshot
        };
    let changed_since_preflight = if sync_mode
        == homeboy::core::runner::RunnerWorkspaceSyncMode::Git
    {
        homeboy::core::runner::prepare_git_lab_offload_changed_since(normalized_args, &source_path)?
    } else {
        homeboy::core::runner::preflight_lab_offload_changed_since(normalized_args, sync_mode)?
    };
    let synced = homeboy::core::runner::sync_workspace(
        runner_id,
        homeboy::core::runner::RunnerWorkspaceSyncOptions {
            path: source_path.display().to_string(),
            mode: sync_mode,
            changed_since_base: changed_since_preflight.resolved_base.clone(),
        },
    )?
    .0;
    let remote_cwd = synced.remote_path;
    let source_snapshot = homeboy::core::source_snapshot::SourceSnapshot::collect_local(
        runner_id,
        Path::new(&synced.local_path),
        Some(&remote_cwd),
        "lab_offload",
    );
    let homeboy_path = runner.settings.homeboy_path.as_deref().unwrap_or("homeboy");
    lab_offload_extension_parity::preflight(
        command_kind,
        runner_id,
        homeboy_path,
        &remote_cwd,
        capability_plan_preflight,
    )?;

    let mut command = vec![homeboy_path.to_string()];
    command.extend(
        rewrite_lab_offload_args(&changed_since_preflight.args, &remote_cwd)
            .into_iter()
            .skip(1),
    );

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        command.join(" "),
        runner_id,
        remote_cwd
    );
    let lab_metadata = homeboy::core::runner::lab_offload_metadata(
        match source {
            LabRunnerSelectionSource::Explicit => "explicit",
            LabRunnerSelectionSource::Default => "automatic",
        },
        Some(runner_id),
        Some(status_tunnel_mode(&status).metadata_value()),
        "offloaded",
        Some(&remote_cwd),
        None,
    );
    let mut env = std::collections::HashMap::new();
    env.insert(
        homeboy::core::observation::LAB_OFFLOAD_METADATA_ENV.to_string(),
        serde_json::to_string(&lab_metadata).unwrap_or_default(),
    );
    let (exec_output, exit_code) = homeboy::core::runner::exec(
        runner_id,
        homeboy::core::runner::RunnerExecOptions {
            cwd: Some(remote_cwd),
            project_id: None,
            allow_ssh: false,
            command,
            env,
            capture_patch,
            raw_exec: false,
            source_snapshot: Some(source_snapshot),
            capability_preflight,
        },
    )?;

    if !exec_output.stderr.is_empty() {
        eprint!("{}", exec_output.stderr);
    }
    if let Some(path) = output_file {
        std::fs::write(path, &exec_output.stdout).map_err(|err| {
            homeboy::core::Error::internal_io(err.to_string(), Some(format!("write {path}")))
        })?;
    }
    print!("{}", exec_output.stdout);
    Ok(exit_code)
}

fn lab_runner_capability_preflight(
    command: &Commands,
    source_path: &Path,
) -> Option<homeboy::core::runner::RunnerCapabilityPreflight> {
    let plan = resource_policy::lab_runner_capability_plan(command, source_path)?;
    Some(homeboy::core::runner::RunnerCapabilityPreflight {
        command: plan.command.to_string(),
        required_tools: plan
            .required_tools
            .into_iter()
            .map(lab_runner_required_tool)
            .collect(),
        required_components: Vec::new(),
        required_env: Vec::new(),
    })
}

fn lab_runner_required_tool(
    tool: resource_policy::LabRunnerTool,
) -> homeboy::core::runner::RunnerRequiredTool {
    match tool {
        resource_policy::LabRunnerTool::Git => homeboy::core::runner::RunnerRequiredTool::Git,
        resource_policy::LabRunnerTool::Node => homeboy::core::runner::RunnerRequiredTool::Node,
        resource_policy::LabRunnerTool::Npm => homeboy::core::runner::RunnerRequiredTool::Npm,
        resource_policy::LabRunnerTool::Pnpm => homeboy::core::runner::RunnerRequiredTool::Pnpm,
        resource_policy::LabRunnerTool::Php => homeboy::core::runner::RunnerRequiredTool::Php,
        resource_policy::LabRunnerTool::Composer => {
            homeboy::core::runner::RunnerRequiredTool::Composer
        }
        resource_policy::LabRunnerTool::Docker => homeboy::core::runner::RunnerRequiredTool::Docker,
        resource_policy::LabRunnerTool::Playwright => {
            homeboy::core::runner::RunnerRequiredTool::Playwright
        }
    }
}

fn lab_offload_source_path(args: &[String]) -> homeboy::core::Result<PathBuf> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" {
            let value = iter.next().ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "path",
                    "--path requires a value before Lab offload can sync the workspace",
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
    }

    std::env::current_dir().map_err(|err| {
        homeboy::core::Error::internal_io(err.to_string(), Some("read cwd".to_string()))
    })
}

fn rewrite_lab_offload_args(args: &[String], remote_path: &str) -> Vec<String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    let has_force_hot = args.iter().any(|arg| arg == "--force-hot");
    while let Some(arg) = iter.next() {
        if passthrough {
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--path" {
            stripped.push(arg.clone());
            let _ = iter.next();
            stripped.push(remote_path.to_string());
            continue;
        }
        if arg.starts_with("--path=") {
            stripped.push(format!("--path={remote_path}"));
            continue;
        }
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        if arg == "--output" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--output=") {
            continue;
        }
        stripped.push(arg.clone());
    }
    if !has_force_hot {
        stripped.insert(1, "--force-hot".to_string());
    }
    stripped
}

/// Attempt to augment a clap error with entity suggestions.
/// Returns Some(augmented_message) if the unrecognized string matches a known entity.
fn try_augment_clap_error(e: &clap::Error) -> Option<String> {
    use clap::error::ErrorKind;

    // Only handle InvalidSubcommand errors
    if e.kind() != ErrorKind::InvalidSubcommand {
        return None;
    }

    // Extract unrecognized subcommand and parent command from error
    let unrecognized = extract_unrecognized_from_error(e)?;
    let parent_command = extract_parent_command_from_error(e)?;

    // Check if it matches a known entity
    let entity_match = entity_suggest::find_entity_match(&unrecognized)?;

    // Generate hints
    let hints =
        entity_suggest::generate_entity_hints(&entity_match, &parent_command, &unrecognized);

    // Build augmented output
    let mut output = format!("error: unrecognized subcommand '{}'\n\n", unrecognized);
    for hint in hints {
        output.push_str(&format!("hint: {}\n", hint));
    }
    output.push_str(&format!(
        "\nFor more information, try 'homeboy {} --help'",
        parent_command
    ));

    Some(output)
}

/// Extract the unrecognized subcommand string from a clap error.
fn extract_unrecognized_from_error(e: &clap::Error) -> Option<String> {
    use clap::error::ContextKind;

    // clap 4.x provides context via e.context()
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::InvalidSubcommand) {
            return Some(value.to_string());
        }
    }

    // Fallback: parse from error message
    // Format: "error: unrecognized subcommand 'xyz'"
    let msg = e.to_string();
    if let Some(start) = msg.find("unrecognized subcommand '") {
        let rest = &msg[start + 25..];
        if let Some(end) = rest.find('\'') {
            return Some(rest[..end].to_string());
        }
    }

    None
}

/// Extract the parent command from a clap error's usage string.
fn extract_parent_command_from_error(e: &clap::Error) -> Option<String> {
    use clap::error::ContextKind;

    // clap 4.x: look for Usage context which contains "homeboy <command> ..."
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::Usage) {
            let usage = value.to_string();
            // Format: "Usage: homeboy <command> [OPTIONS] ..."
            if let Some(rest) = usage.strip_prefix("Usage: homeboy ") {
                // Get first word after "homeboy "
                if let Some(cmd) = rest.split_whitespace().next() {
                    // Skip if it's a placeholder like "[OPTIONS]" or "<COMMAND>"
                    if !cmd.starts_with('[') && !cmd.starts_with('<') {
                        return Some(cmd.to_string());
                    }
                }
            }
        }
    }

    // Fallback: parse from error message which includes usage
    let msg = e.to_string();
    if let Some(start) = msg.find("Usage: homeboy ") {
        let rest = &msg[start + 15..];
        if let Some(cmd) = rest.split_whitespace().next() {
            if !cmd.starts_with('[') && !cmd.starts_with('<') {
                return Some(cmd.to_string());
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::{
        lab_offload_source_path, prepare_lab_runner_for_offload_with,
        resolve_lab_runner_selection_from_default, rewrite_lab_offload_args, LabRunnerPreparation,
        LabRunnerSelection, LabRunnerSelectionSource,
    };
    use clap::Parser;
    use homeboy::cli_surface::Commands;
    use homeboy::commands::test::TestArgs;
    use homeboy::commands::utils::args::{
        BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs,
    };
    fn test_args_for_path(path: &std::path::Path) -> TestArgs {
        TestArgs {
            comp: PositionalComponentArgs {
                component: None,
                path: Some(path.to_string_lossy().to_string()),
            },
            extension_override: ExtensionOverrideArgs::default(),
            skip_lint: false,
            coverage: false,
            coverage_min: None,
            baseline_args: BaselineArgs::default(),
            analyze: false,
            drift: false,
            write: false,
            since: "HEAD~10".to_string(),
            changed_since: None,
            ci_job: None,
            setting_args: SettingArgs::default(),
            args: Vec::new(),
            json_summary: false,
        }
    }

    #[test]
    fn rewrites_lab_offload_path_and_strips_runner_flag_before_remote_exec() {
        let args = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--runner".to_string(),
            "lab-a".to_string(),
            "--json-summary".to_string(),
            "--runner=lab-b".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "lint".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
                "--json-summary".to_string()
            ]
        );
    }

    #[test]
    fn rewrites_equals_path_before_remote_exec() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path=/Users/chubes/Developer/project".to_string(),
            "--runner=lab".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "test".to_string(),
                "--path=/home/chubes/Developer/project".to_string()
            ]
        );
    }

    #[test]
    fn strips_local_output_path_before_remote_exec() {
        let args = vec![
            "homeboy".to_string(),
            "audit".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--runner".to_string(),
            "lab".to_string(),
            "--json-summary".to_string(),
            "--output".to_string(),
            "/var/folders/local/homeboy-audit.json".to_string(),
            "--output=/tmp/other-local-output.json".to_string(),
        ];

        let rewritten = rewrite_lab_offload_args(&args, "/home/chubes/Developer/project");

        assert_eq!(
            rewritten,
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "audit".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
                "--json-summary".to_string(),
            ]
        );
        assert!(!rewritten.iter().any(|arg| arg.contains("/var/folders")));
        assert!(!rewritten
            .iter()
            .any(|arg| arg.contains("/tmp/other-local-output.json")));
    }

    #[test]
    fn leaves_passthrough_path_args_untouched() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
            "--".to_string(),
            "--path".to_string(),
            "test-fixture".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "test".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
                "--".to_string(),
                "--path".to_string(),
                "test-fixture".to_string()
            ]
        );
    }

    #[test]
    fn detects_lab_offload_source_path_from_path_flag() {
        let args = vec![
            "homeboy".to_string(),
            "test".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
        ];

        assert_eq!(
            lab_offload_source_path(&args).expect("path"),
            std::path::PathBuf::from("/Users/chubes/Developer/project")
        );
    }

    #[test]
    fn rewrite_lab_offload_args_does_not_duplicate_force_hot() {
        let args = vec![
            "homeboy".to_string(),
            "--force-hot".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "audit".to_string(),
            "--path".to_string(),
            "/Users/chubes/Developer/project".to_string(),
        ];

        assert_eq!(
            rewrite_lab_offload_args(&args, "/home/chubes/Developer/project"),
            vec![
                "homeboy".to_string(),
                "--force-hot".to_string(),
                "refactor".to_string(),
                "--from".to_string(),
                "audit".to_string(),
                "--path".to_string(),
                "/home/chubes/Developer/project".to_string(),
            ]
        );
    }

    #[test]
    fn lab_runner_selection_keeps_explicit_runner_precedence() {
        let command = Commands::Test(test_args_for_path(std::path::Path::new("/tmp/project")));
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            Some("lab-explicit"),
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .expect("explicit runner selected");

        assert_eq!(selection.runner_id, "lab-explicit");
        assert_eq!(selection.source, LabRunnerSelectionSource::Explicit);
    }

    #[test]
    fn lab_runner_selection_uses_default_for_supported_commands() {
        let command = Commands::Test(test_args_for_path(std::path::Path::new("/tmp/project")));
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            None,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .expect("default runner selected");

        assert_eq!(selection.runner_id, "lab-default");
        assert_eq!(selection.source, LabRunnerSelectionSource::Default);
    }

    #[test]
    fn lab_runner_selection_uses_default_for_hot_refactor_sources() {
        let command = homeboy::cli_surface::Cli::try_parse_from([
            "homeboy",
            "refactor",
            "--from",
            "audit",
            "--path",
            "/tmp/project",
        ])
        .expect("parse")
        .command;
        let selection = resolve_lab_runner_selection_from_default(
            &command,
            None,
            false,
            Some("lab-default".to_string()),
        )
        .expect("selection")
        .expect("default runner selected");

        assert_eq!(selection.runner_id, "lab-default");
        assert_eq!(selection.source, LabRunnerSelectionSource::Default);
    }

    #[test]
    fn lab_runner_selection_runs_locally_without_default_runner() {
        let command = Commands::Test(test_args_for_path(std::path::Path::new("/tmp/project")));

        assert!(
            resolve_lab_runner_selection_from_default(&command, None, false, None)
                .expect("selection")
                .is_none()
        );
    }

    #[test]
    fn lab_runner_selection_force_hot_is_local_escape_hatch() {
        let command = Commands::Test(test_args_for_path(std::path::Path::new("/tmp/project")));

        assert!(resolve_lab_runner_selection_from_default(
            &command,
            None,
            true,
            Some("lab-default".to_string())
        )
        .expect("selection")
        .is_none());
    }

    #[test]
    fn lab_runner_selection_rejects_explicit_runner_on_unsupported_command() {
        let err = resolve_lab_runner_selection_from_default(
            &Commands::List,
            Some("lab-explicit"),
            false,
            Some("lab-default".to_string()),
        )
        .expect_err("unsupported command rejects explicit runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
    }

    #[test]
    fn lab_runner_selection_explains_hot_commands_that_stay_local() {
        let err = resolve_lab_runner_selection_from_default(
            &homeboy::cli_surface::Cli::try_parse_from(["homeboy", "rig", "up", "studio"])
                .expect("parse")
                .command,
            Some("lab-explicit"),
            false,
            Some("lab-default".to_string()),
        )
        .expect_err("rig up rejects explicit runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("single-workspace Lab snapshot"));
    }

    #[test]
    fn lab_runner_preparation_uses_already_connected_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: homeboy::core::runner::RunnerTunnelMode::DirectSsh,
        };

        let prepared = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(homeboy::core::runner::RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: true,
                    state: homeboy::core::runner::RunnerSessionState::Connected,
                    session: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |_| panic!("connected runner should not reconnect"),
        )
        .expect("prepared");

        assert_eq!(prepared, LabRunnerPreparation::Ready);
    }

    #[test]
    fn lab_runner_preparation_connects_disconnected_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: homeboy::core::runner::RunnerTunnelMode::DirectSsh,
        };

        let prepared = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(homeboy::core::runner::RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: false,
                    state: homeboy::core::runner::RunnerSessionState::Disconnected,
                    session: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |runner_id| {
                Ok((
                    homeboy::core::runner::RunnerConnectReport {
                        runner_id: runner_id.to_string(),
                        mode: Some(homeboy::core::runner::RunnerTunnelMode::DirectSsh),
                        role: Some(homeboy::core::runner::RunnerSessionRole::Controller),
                        connected: true,
                        recorded: None,
                        local_url: Some("http://127.0.0.1:1234".to_string()),
                        broker_url: None,
                        controller_id: None,
                        remote_daemon_address: Some("127.0.0.1:5678".to_string()),
                        tunnel_pid: None,
                        remote_daemon_pid: Some(42),
                        homeboy_version: Some("homeboy 0.0.0".to_string()),
                        session_path: Some("/tmp/lab.json".to_string()),
                        failure_kind: None,
                        failure_message: None,
                    },
                    0,
                ))
            },
        )
        .expect("prepared");

        assert_eq!(prepared, LabRunnerPreparation::Ready);
    }

    #[test]
    fn lab_runner_preparation_falls_back_for_unreachable_default_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Default,
            mode: homeboy::core::runner::RunnerTunnelMode::DirectSsh,
        };

        let prepared = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(homeboy::core::runner::RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: false,
                    state: homeboy::core::runner::RunnerSessionState::Disconnected,
                    session: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |runner_id| {
                Ok((
                    homeboy::core::runner::RunnerConnectReport {
                        runner_id: runner_id.to_string(),
                        mode: None,
                        role: None,
                        connected: false,
                        recorded: None,
                        local_url: None,
                        broker_url: None,
                        controller_id: None,
                        remote_daemon_address: None,
                        tunnel_pid: None,
                        remote_daemon_pid: None,
                        homeboy_version: None,
                        session_path: Some("/tmp/lab.json".to_string()),
                        failure_kind: Some(homeboy::core::runner::RunnerFailureKind::SshFailure),
                        failure_message: Some("SSH connectivity check failed".to_string()),
                    },
                    20,
                ))
            },
        )
        .expect("prepared");

        assert_eq!(
            prepared,
            LabRunnerPreparation::FallBackLocal {
                reason: "SSH connectivity check failed".to_string()
            }
        );
    }

    #[test]
    fn lab_runner_preparation_errors_for_unreachable_explicit_runner() {
        let selection = LabRunnerSelection {
            runner_id: "lab".to_string(),
            source: LabRunnerSelectionSource::Explicit,
            mode: homeboy::core::runner::RunnerTunnelMode::DirectSsh,
        };

        let err = prepare_lab_runner_for_offload_with(
            &selection,
            |runner_id| {
                Ok(homeboy::core::runner::RunnerStatusReport {
                    runner_id: runner_id.to_string(),
                    connected: false,
                    state: homeboy::core::runner::RunnerSessionState::Disconnected,
                    session: None,
                    session_path: "/tmp/lab.json".to_string(),
                })
            },
            |runner_id| {
                Ok((
                    homeboy::core::runner::RunnerConnectReport {
                        runner_id: runner_id.to_string(),
                        mode: None,
                        role: None,
                        connected: false,
                        recorded: None,
                        local_url: None,
                        broker_url: None,
                        controller_id: None,
                        remote_daemon_address: None,
                        tunnel_pid: None,
                        remote_daemon_pid: None,
                        homeboy_version: None,
                        session_path: Some("/tmp/lab.json".to_string()),
                        failure_kind: Some(homeboy::core::runner::RunnerFailureKind::SshFailure),
                        failure_message: Some("SSH connectivity check failed".to_string()),
                    },
                    20,
                ))
            },
        )
        .expect_err("explicit runner should error");

        assert!(err.message.contains("could not connect runner"));
    }
}
