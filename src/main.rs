use clap::{ArgMatches, Command, CommandFactory, FromArgMatches, Parser, Subcommand};

use commands::GlobalArgs;

#[derive(Debug, Clone, Copy)]
enum ResponseMode {
    Json,
    Raw(RawOutputMode),
}

#[derive(Debug, Clone, Copy)]
enum RawOutputMode {
    InteractivePassthrough,
    Markdown,
    PlainText,
}

mod commands;
mod docs;
mod output;
mod tty;

use commands::{
    api, auth, build, changelog, changes, cli, component, config, db, deploy, file, fleet, git, init,
    lint, logs, module, project, release, server, ssh, test, transfer, upgrade, version,
};
use homeboy::module::load_all_modules;
use homeboy::utils::args;
use homeboy::utils::entity_suggest::{find_entity_match, generate_entity_hints};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "homeboy")]
#[command(version = VERSION)]
#[command(about = "CLI tool for development and deployment automation")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage project configuration
    #[command(visible_alias = "projects")]
    Project(project::ProjectArgs),
    /// SSH into a project server or configured server
    Ssh(ssh::SshArgs),
    /// Manage SSH server configurations
    #[command(visible_alias = "servers")]
    Server(server::ServerArgs),
    /// Run tests for a component
    Test(test::TestArgs),
    /// Lint a component
    Lint(lint::LintArgs),
    /// Database operations
    Db(db::DbArgs),
    /// Remote file operations
    File(file::FileArgs),
    /// Manage fleets (groups of projects)
    #[command(visible_alias = "fleets")]
    Fleet(fleet::FleetArgs),
    /// Remote log viewing
    Logs(logs::LogsArgs),
    /// Transfer files between servers
    Transfer(transfer::TransferArgs),
    /// Deploy components to remote server
    Deploy(deploy::DeployArgs),
    /// Manage standalone component configurations
    #[command(visible_alias = "components")]
    Component(component::ComponentArgs),
    /// Manage global Homeboy configuration
    Config(config::ConfigArgs),
    /// Execute CLI-compatible modules
    #[command(visible_alias = "modules")]
    Module(module::ModuleArgs),
    /// Get repo context and status (read-only, creates no state)
    #[command(visible_alias = "status")]
    Init(init::InitArgs),
    /// Display CLI documentation
    Docs(crate::commands::docs::DocsArgs),
    /// Changelog operations
    Changelog(changelog::ChangelogArgs),
    /// Git operations for components
    Git(git::GitArgs),
    /// Version management for components
    Version(version::VersionArgs),
    /// Build a component
    Build(build::BuildArgs),
    /// Show changes since last version tag
    Changes(changes::ChangesArgs),
    /// Plan release workflows
    Release(release::ReleaseArgs),
    /// Authenticate with a project's API
    Auth(auth::AuthArgs),
    /// Make API requests to a project
    Api(api::ApiArgs),
    /// Upgrade Homeboy to the latest version
    Upgrade(upgrade::UpgradeArgs),
    /// Alias for upgrade
    #[command(hide = true)]
    Update(upgrade::UpgradeArgs),
    /// List available commands (alias for --help)
    List,
}

fn response_mode(command: &Commands) -> ResponseMode {
    match command {
        Commands::Ssh(args) if args.subcommand.is_none() && args.command.is_none() => {
            ResponseMode::Raw(RawOutputMode::InteractivePassthrough)
        }
        Commands::Logs(args) if logs::is_interactive(args) => {
            ResponseMode::Raw(RawOutputMode::InteractivePassthrough)
        }
        Commands::File(args) if file::is_raw_read(args) => {
            ResponseMode::Raw(RawOutputMode::PlainText)
        }
        Commands::Docs(args) if crate::commands::docs::is_json_mode(args) => ResponseMode::Json,
        Commands::Docs(_) => ResponseMode::Raw(RawOutputMode::Markdown),
        Commands::Changelog(args) if changelog::is_show_markdown(args) => {
            ResponseMode::Raw(RawOutputMode::Markdown)
        }
        Commands::List => ResponseMode::Raw(RawOutputMode::Markdown),
        _ => ResponseMode::Json,
    }
}

struct ModuleCliCommand {
    tool: String,
    project_id: String,
    args: Vec<String>,
}

struct ModuleCliInfo {
    tool: String,
    display_name: String,
    module_name: String,
    project_id_help: Option<String>,
    args_help: Option<String>,
    examples: Vec<String>,
}

fn collect_module_cli_info() -> Vec<ModuleCliInfo> {
    load_all_modules()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            m.cli.map(|cli| {
                let help = cli.help.unwrap_or_default();
                ModuleCliInfo {
                    tool: cli.tool,
                    display_name: cli.display_name,
                    module_name: m.name,
                    project_id_help: help.project_id_help,
                    args_help: help.args_help,
                    examples: help.examples,
                }
            })
        })
        .collect()
}

fn build_augmented_command(module_info: &[ModuleCliInfo]) -> Command {
    let mut cmd = Cli::command();

    for info in module_info {
        let tool_name: &'static str = Box::leak(info.tool.clone().into_boxed_str());

        let project_id_help = info
            .project_id_help
            .clone()
            .unwrap_or_else(|| "Project ID".to_string());
        let project_id_help: &'static str = Box::leak(project_id_help.into_boxed_str());

        let args_help = info
            .args_help
            .clone()
            .unwrap_or_else(|| "Command arguments".to_string());
        let args_help: &'static str = Box::leak(args_help.into_boxed_str());

        let mut subcommand = Command::new(tool_name)
            .about(format!(
                "Run {} commands via {}",
                info.display_name, info.module_name
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
            let examples_text: &'static str = Box::leak(examples_text.into_boxed_str());
            subcommand = subcommand.after_help(examples_text);
        }

        cmd = cmd.subcommand(subcommand);
    }

    cmd
}

fn try_parse_module_cli_command(
    matches: &ArgMatches,
    module_info: &[ModuleCliInfo],
) -> Option<ModuleCliCommand> {
    let (tool, sub_matches) = matches.subcommand()?;

    if !module_info.iter().any(|m| m.tool == tool) {
        return None;
    }

    let project_id = sub_matches.get_one::<String>("project_id")?.clone();
    let args: Vec<String> = sub_matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Some(ModuleCliCommand {
        tool: tool.to_string(),
        project_id,
        args,
    })
}

fn main() -> std::process::ExitCode {
    let module_info = collect_module_cli_info();
    let cmd = build_augmented_command(&module_info);

    let args: Vec<String> = std::env::args().collect();
    let normalized = args::normalize(args);

    let matches = match cmd.try_get_matches_from(normalized) {
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

    if let Some(module_cmd) = try_parse_module_cli_command(&matches, &module_info) {
        let result = cli::run(
            &module_cmd.tool,
            &module_cmd.project_id,
            module_cmd.args,
            &global,
        );

        let (json_result, exit_code) = output::map_cmd_result_to_json(result);
        output::print_json_result(json_result).ok();
        return std::process::ExitCode::from(exit_code_to_u8(exit_code));
    }

    let cli = match Cli::from_arg_matches(&matches) {
        Ok(cli) => cli,
        Err(e) => e.exit(),
    };

    let mode = response_mode(&cli.command);

    match mode {
        ResponseMode::Json => {}
        ResponseMode::Raw(RawOutputMode::InteractivePassthrough) => {
            if !tty::require_tty_for_interactive() {
                let err = homeboy::Error::validation_invalid_argument(
                    "tty",
                    "This command requires an interactive TTY",
                    None,
                    None,
                );
                output::print_result::<serde_json::Value>(Err(err)).ok();
                return std::process::ExitCode::from(exit_code_to_u8(2));
            }
        }
        ResponseMode::Raw(RawOutputMode::Markdown) => {}
        ResponseMode::Raw(RawOutputMode::PlainText) => {}
    }

    if matches!(cli.command, Commands::List) {
        let mut cmd = build_augmented_command(&module_info);
        cmd.print_help().expect("Failed to print help");
        println!();
        return std::process::ExitCode::SUCCESS;
    }

    // Show help for changelog when neither subcommand nor --self is provided
    if let Commands::Changelog(ref args) = cli.command {
        if args.command.is_none() && !args.show_self {
            let cmd = build_augmented_command(&module_info);
            if let Some(mut changelog_cmd) = cmd.find_subcommand("changelog").cloned() {
                changelog_cmd.print_help().expect("Failed to print help");
                println!();
                return std::process::ExitCode::SUCCESS;
            }
        }
    }

    if let ResponseMode::Raw(RawOutputMode::Markdown) = mode {
        let markdown_result = commands::run_markdown(cli.command, &global);

        match markdown_result {
            Ok((content, exit_code)) => {
                print!("{}", content);
                return std::process::ExitCode::from(exit_code_to_u8(exit_code));
            }
            Err(err) => {
                output::print_result::<serde_json::Value>(Err(err)).ok();
                return std::process::ExitCode::from(exit_code_to_u8(1));
            }
        }
    }

    if let ResponseMode::Raw(RawOutputMode::PlainText) = mode {
        if let crate::Commands::File(args) = cli.command {
            let result = file::run(args, &global);
            match result {
                Ok((file::FileCommandOutput::Raw(content), exit_code)) => {
                    print!("{}", content);
                    return std::process::ExitCode::from(exit_code_to_u8(exit_code));
                }
                Ok(_) => {
                    let err = homeboy::Error::other("Unexpected output type for raw mode");
                    output::print_result::<serde_json::Value>(Err(err)).ok();
                    return std::process::ExitCode::from(exit_code_to_u8(1));
                }
                Err(err) => {
                    output::print_result::<serde_json::Value>(Err(err)).ok();
                    return std::process::ExitCode::from(exit_code_to_u8(1));
                }
            }
        }
    }

    let (json_result, exit_code) = commands::run_json(cli.command, &global);

    match mode {
        ResponseMode::Json => {
            output::print_json_result(json_result).ok();
        }
        ResponseMode::Raw(RawOutputMode::InteractivePassthrough) => {}
        ResponseMode::Raw(RawOutputMode::Markdown) => {}
        ResponseMode::Raw(RawOutputMode::PlainText) => {}
    }

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
    let entity_match = find_entity_match(&unrecognized)?;

    // Generate hints
    let hints = generate_entity_hints(&entity_match, &parent_command, &unrecognized);

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
