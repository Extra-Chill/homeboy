use clap::{ArgMatches, Command, CommandFactory, FromArgMatches};

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
        emit_json_result(json_result, output_file.as_deref(), exit_code);
        return std::process::ExitCode::from(exit_code_to_u8(exit_code));
    }

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    let lab_command = match lab_offload_command(&cli.command) {
        Ok(command) => command,
        Err(err) => {
            emit_json_result(Err(err), output_file.as_deref(), 2);
            return std::process::ExitCode::from(exit_code_to_u8(2));
        }
    };
    match homeboy::core::runner::execute_lab_offload(homeboy::core::runner::LabOffloadRequest {
        command: lab_command,
        normalized_args: &normalized,
        explicit_runner: cli.runner.as_deref(),
        force_hot: cli.force_hot,
        capture_patch: cli.command.lab_offload_mutation_flag().is_some(),
    }) {
        Ok(homeboy::core::runner::LabOffloadOutcome::RunLocal {
            metadata, messages, ..
        }) => {
            if let Some(metadata) = metadata {
                homeboy::core::runner::capture_lab_offload_subprocess_metadata(metadata);
            }
            for message in messages {
                eprintln!("{message}");
            }
        }
        Ok(homeboy::core::runner::LabOffloadOutcome::Offloaded {
            stdout,
            stderr,
            exit_code,
            ..
        }) => {
            if !stderr.is_empty() {
                eprint!("{stderr}");
            }
            if let Some(path) = output_file.as_deref() {
                if let Err(err) = std::fs::write(path, &stdout) {
                    let err = homeboy::core::Error::internal_io(
                        err.to_string(),
                        Some(format!("write {path}")),
                    );
                    emit_json_result(Err(err), output_file.as_deref(), 2);
                    return std::process::ExitCode::from(exit_code_to_u8(2));
                }
            }
            print!("{stdout}");
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
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

fn lab_offload_command(
    command: &Commands,
) -> homeboy::core::Result<Option<homeboy::core::runner::LabOffloadCommand>> {
    let Some(contract) = command.lab_contract() else {
        return Ok(None);
    };
    let required_extensions = if contract.requires_extension_parity {
        lab_required_extensions(command)?
    } else {
        Vec::new()
    };
    Ok(Some(homeboy::core::runner::LabOffloadCommand {
        hot_label: contract.hot_label,
        portable: matches!(
            contract.portability,
            homeboy::cli_surface::LabCommandPortability::Portable
        ),
        unsupported_reason: match contract.portability {
            homeboy::cli_surface::LabCommandPortability::Portable => None,
            homeboy::cli_surface::LabCommandPortability::LocalOnly(reason) => Some(reason),
        },
        workspace_mode_policy: match contract.workspace_mode_policy {
            homeboy::cli_surface::LabWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
                homeboy::core::runner::LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot
            }
        },
        requires_extension_parity: contract.requires_extension_parity,
        required_extensions,
        requires_playwright: contract.extra_required_tools.iter().any(|tool| {
            matches!(
                tool,
                homeboy::cli_surface::LabCommandRequiredTool::Playwright
            )
        }),
    }))
}

fn lab_required_extensions(command: &Commands) -> homeboy::core::Result<Vec<String>> {
    let mut extension_ids = std::collections::BTreeSet::new();

    match command {
        Commands::Audit(args) => extension_ids.extend(args.extension_override.extensions.clone()),
        Commands::Bench(args) => {
            extension_ids.extend(args.extension_override_ids().iter().cloned())
        }
        Commands::Lint(args) => extension_ids.extend(args.extension_override.extensions.clone()),
        Commands::Test(args) => {
            extension_ids.extend(args.extension_override.extensions.clone());
            extension_ids.extend(test_lab_extension_ids(args)?);
        }
        _ => {}
    }

    Ok(extension_ids.into_iter().collect())
}

fn test_lab_extension_ids(
    args: &homeboy::commands::test::TestArgs,
) -> homeboy::core::Result<Vec<String>> {
    let source_context = homeboy::core::engine::execution_context::resolve(
        &homeboy::core::engine::execution_context::ResolveOptions {
            component_id: args.comp.component.clone(),
            path_override: args.comp.path.clone(),
            capability: None,
            settings_overrides: args.setting_args.setting.clone(),
            settings_json_overrides: args.setting_args.setting_json.clone(),
            extension_overrides: args.extension_override.extensions.clone(),
        },
    )?;

    if !args.drift
        && args.ci_job.is_none()
        && source_context
            .component
            .has_script(homeboy::core::extension::ExtensionCapability::Test)
    {
        return Ok(Vec::new());
    }

    let context = homeboy::core::engine::execution_context::resolve(
        &homeboy::core::engine::execution_context::ResolveOptions {
            component_id: args.comp.component.clone(),
            path_override: args.comp.path.clone(),
            capability: Some(homeboy::core::extension::ExtensionCapability::Test),
            settings_overrides: args.setting_args.setting.clone(),
            settings_json_overrides: args.setting_args.setting_json.clone(),
            extension_overrides: args.extension_override.extensions.clone(),
        },
    )?;

    Ok(context.extension_id.into_iter().collect())
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
