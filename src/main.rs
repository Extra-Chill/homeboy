use clap::{ArgMatches, Command, CommandFactory, FromArgMatches};
use std::path::Path;

use homeboy::cli_surface::Cli;
use homeboy::cli_surface::Commands;
use homeboy::commands::GlobalArgs;

use homeboy::commands;
use homeboy::commands::cli;
use homeboy::commands::utils::{args, entity_suggest, resource_policy, response as output};
use homeboy::core::extension::load_all_extensions;

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
        if let Some(ref path) = output_file {
            output::write_json_to_file(&json_result, path, exit_code);
        }
        output::print_json_result(json_result, exit_code).ok();
        return std::process::ExitCode::from(exit_code_to_u8(exit_code));
    }

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    if let Some(runner_id) = cli.runner.as_deref() {
        if !cli.command.supports_lab_runner() {
            let err = homeboy::core::Error::validation_invalid_argument(
                "runner",
                "--runner is only supported for hot Lab-offload commands: lint, test, audit, bench, and trace",
                Some(runner_id.to_string()),
                None,
            );
            output::print_result::<serde_json::Value>(Err(err)).ok();
            return std::process::ExitCode::from(exit_code_to_u8(2));
        }
        let capture_patch = cli.command.lab_offload_mutation_flag().is_some();
        return run_lab_offload(
            runner_id,
            &normalized,
            output_file.as_deref(),
            capture_patch,
        );
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

fn exit_code_to_u8(code: i32) -> u8 {
    if code <= 0 {
        0
    } else if code >= 255 {
        255
    } else {
        code as u8
    }
}

fn run_lab_offload(
    runner_id: &str,
    normalized_args: &[String],
    output_file: Option<&str>,
    capture_patch: bool,
) -> std::process::ExitCode {
    match run_lab_offload_inner(runner_id, normalized_args, output_file, capture_patch) {
        Ok(exit_code) => std::process::ExitCode::from(exit_code_to_u8(exit_code)),
        Err(err) => {
            output::print_result::<serde_json::Value>(Err(err)).ok();
            std::process::ExitCode::from(exit_code_to_u8(1))
        }
    }
}

fn run_lab_offload_inner(
    runner_id: &str,
    normalized_args: &[String],
    output_file: Option<&str>,
    capture_patch: bool,
) -> homeboy::core::Result<i32> {
    let runner = homeboy::core::runner::load(runner_id)?;
    if runner.kind != homeboy::core::runner::RunnerKind::Ssh {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a remote SSH runner; local runners would execute on this machine",
            Some(runner.id),
            Some(vec![
                "Register an SSH runner and run `homeboy runner connect <runner-id>` first."
                    .to_string(),
            ]),
        ));
    }

    let status = homeboy::core::runner::status(runner_id)?;
    if !status.connected {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "runner",
            "Lab offload requires a connected runner daemon",
            Some(runner_id.to_string()),
            Some(vec![format!(
                "Run `homeboy runner connect {runner_id}` before using --runner."
            )]),
        ));
    }

    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "workspace_root",
            "Lab offload requires runner.workspace_root so the local checkout can be mapped remotely",
            Some(runner.id.clone()),
            Some(vec![
                "This Wave 3 adapter assumes workspace sync/provenance has placed the same checkout basename under runner.workspace_root.".to_string(),
            ]),
        )
    })?;
    let remote_cwd = remote_cwd_for_current_checkout(workspace_root)?;
    let source_snapshot = homeboy::core::source_snapshot::SourceSnapshot::collect_local(
        runner_id,
        &std::env::current_dir().map_err(|err| {
            homeboy::core::Error::internal_io(err.to_string(), Some("read cwd".to_string()))
        })?,
        Some(&remote_cwd),
        "lab_offload",
    );
    let homeboy_path = runner.homeboy_path.as_deref().unwrap_or("homeboy");
    let mut command = vec![homeboy_path.to_string()];
    command.extend(strip_runner_flag(normalized_args).into_iter().skip(1));

    eprintln!(
        "Lab offload: running `{}` on runner `{}` in `{}`.",
        command.join(" "),
        runner_id,
        remote_cwd
    );
    let (exec_output, exit_code) = homeboy::core::runner::exec(
        runner_id,
        homeboy::core::runner::RunnerExecOptions {
            cwd: Some(remote_cwd),
            allow_ssh: false,
            command,
            capture_patch,
            source_snapshot: Some(source_snapshot),
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

fn remote_cwd_for_current_checkout(workspace_root: &str) -> homeboy::core::Result<String> {
    let cwd = std::env::current_dir().map_err(|err| {
        homeboy::core::Error::internal_io(err.to_string(), Some("read cwd".to_string()))
    })?;
    let basename = cwd
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "cwd",
                "current checkout path has no basename for Lab workspace mapping",
                Some(cwd.display().to_string()),
                None,
            )
        })?;
    Ok(Path::new(workspace_root)
        .join(basename)
        .to_string_lossy()
        .to_string())
}

fn strip_runner_flag(args: &[String]) -> Vec<String> {
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        stripped.push(arg.clone());
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
    use super::strip_runner_flag;

    #[test]
    fn strips_runner_flag_forms_before_remote_exec() {
        let args = vec![
            "homeboy".to_string(),
            "lint".to_string(),
            "--runner".to_string(),
            "lab-a".to_string(),
            "--json-summary".to_string(),
            "--runner=lab-b".to_string(),
        ];

        assert_eq!(
            strip_runner_flag(&args),
            vec![
                "homeboy".to_string(),
                "lint".to_string(),
                "--json-summary".to_string()
            ]
        );
    }
}
