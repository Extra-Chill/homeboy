use clap::{ArgMatches, Command, CommandFactory, FromArgMatches};
use std::io::IsTerminal;

use crate::cli_surface::{Cli, Commands};
use crate::commands;
use crate::commands::cli;
use crate::commands::output_runtime;
use crate::commands::utils::{args, entity_suggest, resource_policy, response as output};
use crate::commands::GlobalArgs;
use crate::core::extension::load_all_extensions;
use crate::core::upgrade;

pub struct CliRuntime {
    extension_info: Vec<ExtensionCliInfo>,
}

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

impl CliRuntime {
    pub fn new() -> Self {
        Self {
            extension_info: collect_extension_cli_info(),
        }
    }

    pub fn run_from_args(&self, args: Vec<String>) -> std::process::ExitCode {
        if is_top_level_version_request(&args) {
            println!("{}", upgrade::current_build_version());
            return std::process::ExitCode::SUCCESS;
        }

        let normalized = args::normalize(args);
        let matches = self.parse_matches(normalized.clone());
        self.run_matches(matches, normalized)
    }

    fn parse_matches(&self, normalized: Vec<String>) -> ArgMatches {
        match self
            .build_augmented_command()
            .try_get_matches_from(normalized)
        {
            Ok(matches) => matches,
            Err(err) => {
                if let Some(output) = try_augment_clap_error(&err) {
                    eprintln!("{}", output);
                    std::process::exit(2);
                }
                err.exit();
            }
        }
    }

    fn run_matches(&self, matches: ArgMatches, normalized: Vec<String>) -> std::process::ExitCode {
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
        crate::core::set_artifact_root_override(artifact_root_override.clone());

        if let Some(extension_cmd) = self.try_parse_extension_cli_command(&matches) {
            if let Some(path) = output_file.as_deref() {
                if let Some(err) = output_runtime::validate_output_file_path(path) {
                    output_runtime::emit_json_result(Err(err), None, 2);
                    return std::process::ExitCode::from(exit_code_to_u8(2));
                }
            }

            let cli_args = cli::CliArgs {
                tool: extension_cmd.tool,
                identifier: extension_cmd.project_id,
                args: extension_cmd.args,
            };
            let result = cli::run(cli_args, &global);

            let (json_result, exit_code) = output::map_cmd_result_to_json(result);
            output_runtime::emit_json_result(json_result, output_file.as_deref(), exit_code);
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        let mut cli = match Cli::from_arg_matches(&matches) {
            Ok(cli) => cli,
            Err(err) => err.exit(),
        };
        normalize_runs_list_runner(&mut cli, &normalized);

        if matches!(&cli.command, Commands::Runs(args) if args.is_bundle_export()) {
            output_file = None;
        }

        if cli.command.consumes_output_file_as_command_arg() {
            // This command owns `--output/-o`; it is not the global JSON envelope.
            output_file = None;
        } else if let Some(path) = output_file.as_deref() {
            if let Some(err) = output_runtime::validate_output_file_path(path) {
                output_runtime::emit_json_result(Err(err), None, 2);
                return std::process::ExitCode::from(exit_code_to_u8(2));
            }
        }

        match crate::commands::route::route_after_parse(&cli, &normalized, output_file.as_deref()) {
            Ok(None) => {}
            Ok(Some(exit_code)) => {
                return std::process::ExitCode::from(exit_code_to_u8(exit_code));
            }
            Err(err) => {
                output_runtime::emit_json_result(Err(err), output_file.as_deref(), 2);
                return std::process::ExitCode::from(exit_code_to_u8(2));
            }
        }

        crate::core::set_artifact_root_override(
            cli.artifact_root.clone().or(artifact_root_override),
        );

        if let Some(exit_code) = preflight_hot_command(&cli, output_file.as_deref()) {
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        run_startup_update_checks(&cli.command);

        if matches!(cli.command, Commands::List) {
            let mut cmd = self.build_augmented_command();
            cmd.print_help().expect("Failed to print help");
            println!();
            return std::process::ExitCode::SUCCESS;
        }

        // Show help for changelog when neither subcommand nor --self is provided.
        if let Commands::Changelog(ref args) = cli.command {
            if args.command.is_none() && !args.show_self {
                let cmd = self.build_augmented_command();
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

    fn build_augmented_command(&self) -> Command {
        build_augmented_command(&self.extension_info)
    }

    fn try_parse_extension_cli_command(&self, matches: &ArgMatches) -> Option<ExtensionCliCommand> {
        try_parse_extension_cli_command(matches, &self.extension_info)
    }
}

fn is_top_level_version_request(args: &[String]) -> bool {
    matches!(args, [_, flag] if flag == "--version" || flag == "-V")
}

impl Default for CliRuntime {
    fn default() -> Self {
        Self::new()
    }
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

fn preflight_hot_command(cli: &Cli, output_file: Option<&str>) -> Option<i32> {
    if let Some(hot_command) = resource_policy::hot_command(&cli.command) {
        if let Ok((resources, _)) = crate::commands::doctor::resources::run(
            crate::commands::doctor::resources::ResourcesArgs {},
        ) {
            let warning = resource_policy::evaluate(hot_command, &resources);
            let runner_hosted = resource_policy::is_runner_hosted_exec();
            if runner_hosted {
                resource_policy::clear_runner_hosted_exec();
            }
            if let Some(warning) = warning.as_ref() {
                if !cli.force_hot && !runner_hosted {
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
                    if runner_hosted {
                        None
                    } else {
                        warning.as_ref()
                    },
                    cli.force_hot,
                ),
            );
            if let Some(warning) = warning.as_ref() {
                if let Some(err) = resource_policy::non_interactive_preflight_error(
                    warning,
                    cli.force_hot,
                    is_interactive_shell(),
                ) {
                    output_runtime::emit_json_result(Err(err), output_file, 2);
                    return Some(2);
                }
            }
        }
    }

    None
}

fn run_startup_update_checks(command: &Commands) {
    // Startup update checks — skip for upgrade (it handles this itself).
    if !matches!(
        command,
        Commands::Upgrade(_) | Commands::Daemon(_) | Commands::SelfCmd(_)
    ) {
        crate::core::upgrade::update_check::run_startup_check();
        crate::core::extension::update_check::run_startup_check();
    }
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

fn is_interactive_shell() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn normalize_runs_list_runner(cli: &mut Cli, normalized_args: &[String]) {
    if is_runs_list_runner_option(normalized_args) {
        if let Commands::Runs(args) = &mut cli.command {
            cli.runner = args.absorb_global_runner_for_list(cli.runner.take());
        }
    }
}

fn is_runs_list_runner_option(args: &[String]) -> bool {
    let Some(runs_index) = args.iter().position(|arg| arg == "runs") else {
        return false;
    };
    let Some(list_index) = args.iter().position(|arg| arg == "list") else {
        return false;
    };

    list_index > runs_index
        && args.iter().enumerate().any(|(index, arg)| {
            index > list_index && (arg == "--runner" || arg.starts_with("--runner="))
        })
}

/// Attempt to augment a clap error with entity suggestions.
/// Returns Some(augmented_message) if the unrecognized string matches a known entity.
fn try_augment_clap_error(e: &clap::Error) -> Option<String> {
    use clap::error::ErrorKind;

    // Only handle InvalidSubcommand errors.
    if e.kind() != ErrorKind::InvalidSubcommand {
        return None;
    }

    // Extract unrecognized subcommand and parent command from error.
    let unrecognized = extract_unrecognized_from_error(e)?;
    let parent_command = extract_parent_command_from_error(e)?;

    // Check if it matches a known entity.
    let entity_match = entity_suggest::find_entity_match(&unrecognized)?;

    // Generate hints.
    let hints =
        entity_suggest::generate_entity_hints(&entity_match, &parent_command, &unrecognized);

    // Build augmented output.
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

    // clap 4.x provides context via e.context().
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::InvalidSubcommand) {
            return Some(value.to_string());
        }
    }

    // Fallback: parse from error message.
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

    // clap 4.x: look for Usage context which contains "homeboy <command> ...".
    for (kind, value) in e.context() {
        if matches!(kind, ContextKind::Usage) {
            let usage = value.to_string();
            // Format: "Usage: homeboy <command> [OPTIONS] ...".
            if let Some(rest) = usage.strip_prefix("Usage: homeboy ") {
                // Get first word after "homeboy ".
                if let Some(cmd) = rest.split_whitespace().next() {
                    // Skip if it's a placeholder like "[OPTIONS]" or "<COMMAND>".
                    if !cmd.starts_with('[') && !cmd.starts_with('<') {
                        return Some(cmd.to_string());
                    }
                }
            }
        }
    }

    // Fallback: parse from error message which includes usage.
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
    use super::*;
    use clap::Parser;
    use std::sync::{Mutex, MutexGuard, OnceLock};

    struct EnvGuard {
        name: &'static str,
        previous: Option<String>,
        _guard: MutexGuard<'static, ()>,
    }

    impl EnvGuard {
        fn remove(name: &'static str) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|err| err.into_inner());
            let previous = std::env::var(name).ok();
            std::env::remove_var(name);
            Self {
                name,
                previous,
                _guard: guard,
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            if let Some(value) = &self.previous {
                std::env::set_var(self.name, value);
            } else {
                std::env::remove_var(self.name);
            }
        }
    }

    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    #[test]
    fn output_format_names_are_rejected_as_global_output_paths() {
        let err = output_runtime::validate_output_file_path("json")
            .expect("format-like path should be rejected");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("--output json"));
    }

    #[test]
    fn normal_output_file_paths_are_allowed() {
        assert!(output_runtime::validate_output_file_path("./homeboy-output.json").is_none());
    }

    #[test]
    fn runs_list_runner_after_subcommand_is_not_treated_as_global_runner() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let mut cli = Cli::parse_from([
            "homeboy",
            "runs",
            "list",
            "--runner",
            "homeboy-lab",
            "--status",
            "running",
        ]);

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));

        normalize_runs_list_runner(
            &mut cli,
            &[
                "homeboy".into(),
                "runs".into(),
                "list".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "--status".into(),
                "running".into(),
            ],
        );

        assert_eq!(cli.runner, None);
        let Commands::Runs(args) = &cli.command else {
            panic!("expected runs command");
        };
        assert_eq!(args.list_runner(), Some("homeboy-lab"));
    }

    #[test]
    fn global_runner_for_runs_show_is_preserved_for_guidance_error() {
        let _env = EnvGuard::remove(crate::core::observation::LAB_OFFLOAD_METADATA_ENV);
        let mut cli = Cli::parse_from([
            "homeboy",
            "--runner",
            "homeboy-lab",
            "runs",
            "show",
            "run-123",
        ]);

        normalize_runs_list_runner(
            &mut cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "show".into(),
                "run-123".into(),
            ],
        );

        assert_eq!(cli.runner.as_deref(), Some("homeboy-lab"));
        let err = crate::commands::route::route_after_parse(
            &cli,
            &[
                "homeboy".into(),
                "--runner".into(),
                "homeboy-lab".into(),
                "runs".into(),
                "show".into(),
                "run-123".into(),
            ],
            None,
        )
        .expect_err("runs show still rejects global runner");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("without --runner"));
    }
}
