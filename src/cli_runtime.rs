use clap::{ArgMatches, Command, CommandFactory, FromArgMatches};
use std::io::IsTerminal;
use std::sync::OnceLock;

use crate::cli_surface::{
    command_safety_manifest_from_dynamic, command_surface_from, Cli, Commands,
    DynamicCommandDescriptor,
};
use crate::commands;
use crate::commands::cli;
use crate::commands::output_runtime;
use crate::commands::utils::{args, entity_suggest, resource_policy, response as output};
use crate::commands::GlobalArgs;
use crate::core::extension::{list_summaries, load_all_extensions};
use crate::core::upgrade;

pub struct CliRuntime {
    extension_discovery: OnceLock<ExtensionCliDiscovery>,
}

struct ExtensionCliCommand {
    tool: String,
    project_id: String,
    args: Vec<String>,
}

struct ExtensionCliInfo {
    tool: String,
    descriptor: DynamicCommandDescriptor,
    project_id_help: Option<String>,
    args_help: Option<String>,
    examples: Vec<String>,
}

#[derive(Debug, Clone, Default)]
struct ExtensionCliHealth {
    load_error: Option<String>,
    broken_link_ids: Vec<String>,
}

struct ExtensionCliDiscovery {
    info: Vec<ExtensionCliInfo>,
    health: ExtensionCliHealth,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StartupFastPath {
    Help,
    Version,
}

pub fn run_startup_fast_path(args: &[String]) -> Option<std::process::ExitCode> {
    match startup_fast_path(args)? {
        StartupFastPath::Help => {
            let mut cmd = Cli::command();
            cmd.print_help().expect("Failed to print help");
            println!();
        }
        StartupFastPath::Version => println!("{}", upgrade::current_build_version()),
    }

    Some(std::process::ExitCode::SUCCESS)
}

impl CliRuntime {
    pub fn new() -> Self {
        Self {
            extension_discovery: OnceLock::new(),
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
        match Cli::command().try_get_matches_from(normalized.clone()) {
            Ok(matches) => matches,
            Err(static_err) => match self
                .build_augmented_command()
                .try_get_matches_from(normalized)
            {
                Ok(matches) => matches,
                Err(err) => {
                    if let Some(output) =
                        try_augment_clap_error(&err, &self.extension_discovery().health)
                    {
                        eprintln!("{}", output);
                        std::process::exit(2);
                    }

                    if let Some(output) =
                        try_augment_clap_error(&static_err, &self.extension_discovery().health)
                    {
                        eprintln!("{}", output);
                        std::process::exit(2);
                    }

                    err.exit();
                }
            },
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

        if let Some(exit_code) = run_raw_agent_tool_dispatch(&cli.command) {
            return std::process::ExitCode::from(exit_code_to_u8(exit_code));
        }

        run_startup_update_checks(&cli.command);

        if let Commands::List { json } = &cli.command {
            if *json {
                self.print_command_safety_manifest_json();
            } else {
                let mut cmd = self.build_augmented_command();
                cmd.print_help().expect("Failed to print help");
                println!();
            }
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
        let discovery = self.extension_discovery();
        build_augmented_command(&discovery.info, &discovery.health)
    }

    fn try_parse_extension_cli_command(&self, matches: &ArgMatches) -> Option<ExtensionCliCommand> {
        let (tool, _) = matches.subcommand()?;
        if is_builtin_subcommand(tool) {
            return None;
        }

        try_parse_extension_cli_command(matches, &self.extension_discovery().info)
    }

    fn extension_discovery(&self) -> &ExtensionCliDiscovery {
        self.extension_discovery
            .get_or_init(collect_extension_cli_info)
    }

    fn print_command_safety_manifest_json(&self) {
        let discovery = self.extension_discovery();
        let dynamic_commands = discovery
            .info
            .iter()
            .map(|info| info.descriptor.clone())
            .collect::<Vec<_>>();
        let surface = command_surface_from(self.build_augmented_command());
        let manifest = command_safety_manifest_from_dynamic(surface, &dynamic_commands);
        let json =
            serde_json::to_string_pretty(&manifest).expect("command safety manifest serializes");
        println!("{json}");
    }
}

fn run_raw_agent_tool_dispatch(command: &Commands) -> Option<i32> {
    let Commands::AgentTask(args) = command else {
        return None;
    };
    let crate::commands::agent_task::AgentTaskCommand::Tool(tool_args) = &args.command else {
        return None;
    };
    match &tool_args.command {
        crate::commands::agent_task::tool::AgentTaskToolCommand::Dispatch(_args) => {
            Some(crate::commands::agent_task::tool::dispatch_raw(
                crate::commands::agent_task::tool::AgentTaskToolDispatchArgs {},
            ))
        }
    }
}

fn is_top_level_version_request(args: &[String]) -> bool {
    matches!(args, [_, flag] if flag == "--version" || flag == "-V")
}

fn startup_fast_path(args: &[String]) -> Option<StartupFastPath> {
    match args {
        [_, flag] if flag == "--help" || flag == "-h" => Some(StartupFastPath::Help),
        [_, flag] if flag == "--version" || flag == "-V" => Some(StartupFastPath::Version),
        _ => None,
    }
}

impl Default for CliRuntime {
    fn default() -> Self {
        Self::new()
    }
}

fn collect_extension_cli_info() -> ExtensionCliDiscovery {
    let summaries = list_summaries(None);
    let mut broken_link_ids: Vec<String> = summaries
        .iter()
        .filter(|summary| summary.error.as_deref() == Some("target_missing"))
        .map(|summary| summary.id.clone())
        .collect();
    broken_link_ids.sort();

    let (extensions, load_error) = match load_all_extensions() {
        Ok(extensions) => (extensions, None),
        Err(error) => (Vec::new(), Some(error.message)),
    };

    let info = extensions
        .into_iter()
        .filter_map(|m| {
            m.cli.map(|cli| {
                let help = cli.help.unwrap_or_default();
                let about = format!("Run {} commands via {}", cli.display_name, m.name);
                ExtensionCliInfo {
                    descriptor: DynamicCommandDescriptor::extension_command(
                        cli.tool.clone(),
                        about,
                    ),
                    tool: cli.tool,
                    project_id_help: help.project_id_help,
                    args_help: help.args_help,
                    examples: help.examples,
                }
            })
        })
        .collect();

    ExtensionCliDiscovery {
        info,
        health: ExtensionCliHealth {
            load_error,
            broken_link_ids,
        },
    }
}

fn build_augmented_command(
    extension_info: &[ExtensionCliInfo],
    extension_health: &ExtensionCliHealth,
) -> Command {
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

        let mut subcommand = Command::new(info.descriptor.name.clone())
            .about(info.descriptor.about.clone())
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

    if let Some(after_help) = extension_after_help(extension_info, extension_health) {
        cmd = cmd.after_help(after_help);
    }

    cmd
}

fn extension_after_help(
    extension_info: &[ExtensionCliInfo],
    extension_health: &ExtensionCliHealth,
) -> Option<String> {
    let mut lines = Vec::new();

    if !extension_info.is_empty() {
        let commands = extension_info
            .iter()
            .map(|info| info.tool.as_str())
            .collect::<Vec<_>>()
            .join(", ");
        lines.push(format!("Extension-provided commands: {commands}"));
    }

    if let Some(error) = &extension_health.load_error {
        lines.push(format!(
            "Extension discovery warning: {error}. Run `homeboy extension list` for details."
        ));
    }

    if !extension_health.broken_link_ids.is_empty() {
        lines.push(format!(
            "Extension health warning: {} broken extension link(s): {}. Run `homeboy extension list` for details or `homeboy extension relink <id> <path>` to repair.",
            extension_health.broken_link_ids.len(),
            extension_health.broken_link_ids.join(", ")
        ));
    }

    if lines.is_empty() {
        None
    } else {
        Some(lines.join("\n"))
    }
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

fn is_builtin_subcommand(name: &str) -> bool {
    crate::command_contract::registered_command(name).is_some()
}

fn preflight_hot_command(cli: &Cli, output_file: Option<&str>) -> Option<i32> {
    if let Some(hot_command) = resource_policy::hot_command(&cli.command) {
        if let Ok((resources, _)) = crate::commands::doctor::resources::run_preflight() {
            let default_lab_runner = if hot_command.lab_offload_supported {
                crate::core::runner::resolve_default_lab_runner()
                    .ok()
                    .flatten()
            } else {
                None
            };
            let warning = resource_policy::evaluate_with_runner_hint(
                hot_command,
                &resources,
                default_lab_runner.as_deref(),
            );
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
                    cli.force_hot || runner_hosted,
                    is_interactive_shell(),
                    resource_policy::local_hot_rerun_command(
                        hot_command,
                        &std::env::args().collect::<Vec<_>>(),
                    ),
                    default_lab_runner.as_deref(),
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
fn try_augment_clap_error(
    e: &clap::Error,
    extension_health: &ExtensionCliHealth,
) -> Option<String> {
    // Extract unrecognized subcommand and parent command from error.
    let unrecognized = extract_unrecognized_from_error(e)?;
    let parent_command = extract_parent_command_from_error(e)?;

    let mut hints = entity_suggest::find_entity_match(&unrecognized)
        .map(|entity_match| {
            entity_suggest::generate_entity_hints(&entity_match, &parent_command, &unrecognized)
        })
        .unwrap_or_default();

    append_extension_health_hints(&mut hints, extension_health);

    if hints.is_empty() {
        return None;
    }

    // Build augmented output.
    let mut output = format!("error: unrecognized subcommand '{}'\n\n", unrecognized);
    for hint in hints {
        output.push_str(&format!("hint: {}\n", hint));
    }
    if parent_command.is_empty() {
        output.push_str("\nFor more information, try 'homeboy --help'");
    } else {
        output.push_str(&format!(
            "\nFor more information, try 'homeboy {} --help'",
            parent_command
        ));
    }

    Some(output)
}

fn append_extension_health_hints(hints: &mut Vec<String>, extension_health: &ExtensionCliHealth) {
    if extension_health.load_error.is_some() || !extension_health.broken_link_ids.is_empty() {
        hints.push(
            "extension-provided commands may be unavailable; run `homeboy extension list` to inspect extension health".to_string(),
        );
    }

    if !extension_health.broken_link_ids.is_empty() {
        hints.push(format!(
            "broken extension link(s): {}; repair with `homeboy extension relink <id> <path>`",
            extension_health.broken_link_ids.join(", ")
        ));
    }
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

    // Fallback: parse from error message. Clap wording varies between
    // contexts and versions.
    let msg = e.to_string();
    for marker in ["unrecognized subcommand '", "subcommand '"] {
        if let Some(start) = msg.find(marker) {
            let rest = &msg[start + marker.len()..];
            if let Some(end) = rest.find('\'') {
                return Some(rest[..end].to_string());
            }
        }
    }
    for marker in ["unrecognized subcommand `", "subcommand `"] {
        if let Some(start) = msg.find(marker) {
            let rest = &msg[start + marker.len()..];
            if let Some(end) = rest.find('`') {
                return Some(rest[..end].to_string());
            }
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

    Some(String::new())
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

    fn sample_extension_info(tool: &str) -> ExtensionCliInfo {
        ExtensionCliInfo {
            tool: tool.to_string(),
            descriptor: DynamicCommandDescriptor::extension_command(
                tool.to_string(),
                "Run Sample CLI commands via Sample Extension".to_string(),
            ),
            project_id_help: None,
            args_help: None,
            examples: Vec::new(),
        }
    }

    fn write_cli_extension(home: &std::path::Path, id: &str, tool: &str) {
        let extension_dir = home.join(".config/homeboy/extensions").join(id);
        std::fs::create_dir_all(&extension_dir).expect("extension dir");
        std::fs::write(
            extension_dir.join(format!("{id}.json")),
            serde_json::json!({
                "name": "WordPress Extension",
                "version": "0.0.0",
                "cli": {
                    "tool": tool,
                    "display_name": "WordPress CLI",
                    "command_template": "{{cliPath}} {{args}}"
                }
            })
            .to_string(),
        )
        .expect("extension manifest");
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
    fn startup_fast_path_only_matches_root_help_and_version_flags() {
        let args = |values: &[&str]| {
            values
                .iter()
                .map(|value| value.to_string())
                .collect::<Vec<_>>()
        };

        assert_eq!(
            startup_fast_path(&args(&["homeboy", "--help"])),
            Some(StartupFastPath::Help)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "-h"])),
            Some(StartupFastPath::Help)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "--version"])),
            Some(StartupFastPath::Version)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "-V"])),
            Some(StartupFastPath::Version)
        );
        assert_eq!(
            startup_fast_path(&args(&["homeboy", "status", "--help"])),
            None
        );
        assert_eq!(startup_fast_path(&args(&["homeboy", "wp", "--help"])), None);
    }

    #[test]
    fn root_help_lists_extension_provided_commands() {
        let mut command = build_augmented_command(
            &[sample_extension_info("wp")],
            &ExtensionCliHealth::default(),
        );

        let help = command.render_help().to_string();

        assert!(help.contains("Extension-provided commands: wp"));
    }

    #[cfg(unix)]
    #[test]
    fn root_help_warns_about_broken_extension_links_without_paths() {
        let health = ExtensionCliHealth {
            load_error: None,
            broken_link_ids: vec!["wordpress".to_string()],
        };
        let mut command = build_augmented_command(&[], &health);

        let help = command.render_help().to_string();

        assert!(help.contains("Extension health warning: 1 broken extension link(s): wordpress"));
        assert!(help.contains("homeboy extension list"));
        assert!(help.contains("homeboy extension relink <id> <path>"));
        assert!(!help.contains("/missing-wordpress"));
    }

    #[cfg(unix)]
    #[test]
    fn invalid_dynamic_command_points_to_extension_health_when_links_are_broken() {
        let command = build_augmented_command(&[], &ExtensionCliHealth::default());
        let err = command
            .try_get_matches_from(["homeboy", "wp"])
            .expect_err("wp should not parse without extension command metadata");
        let health = ExtensionCliHealth {
            load_error: None,
            broken_link_ids: vec!["wordpress".to_string()],
        };

        let output = try_augment_clap_error(&err, &health).expect("extension health hint");

        assert!(output.contains("extension-provided commands may be unavailable"));
        assert!(output.contains("broken extension link(s): wordpress"));
        assert!(output.contains("homeboy extension list"));
    }

    #[cfg(unix)]
    #[test]
    fn extension_discovery_reports_dynamic_commands_and_broken_links() {
        crate::test_support::with_isolated_home(|home| {
            write_cli_extension(home.path(), "wordpress", "wp");
            let extensions_dir = home.path().join(".config/homeboy/extensions");
            let link = extensions_dir.join("nodejs");
            let target = extensions_dir.join("missing-nodejs");
            std::os::unix::fs::symlink(&target, &link).unwrap();

            let discovery = collect_extension_cli_info();

            assert_eq!(discovery.info.len(), 1);
            assert_eq!(discovery.info[0].tool, "wp");
            assert_eq!(discovery.health.broken_link_ids, vec!["nodejs"]);
        });
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
