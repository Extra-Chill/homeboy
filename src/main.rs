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
}

mod commands;
mod docs;
mod output;
mod tty;

use commands::{
    api, auth, build, changelog, changes, cli, component, config, context, db, deploy, file, git,
    init, logs, module, project, release, server, ssh, test, upgrade, version,
};
use homeboy::module::load_all_modules;

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
    Server(server::ServerArgs),
    /// Run tests for a component
    Test(test::TestArgs),
    /// Database operations
    Db(db::DbArgs),
    /// Remote file operations
    File(file::FileArgs),
    /// Remote log viewing
    Logs(logs::LogsArgs),
    /// Deploy components to remote server
    Deploy(deploy::DeployArgs),
    /// Manage standalone component configurations
    Component(component::ComponentArgs),
    /// Manage global Homeboy configuration
    Config(config::ConfigArgs),
    /// Show context for current working directory
    Context(context::ContextArgs),
    /// Execute CLI-compatible modules
    Module(module::ModuleArgs),
    /// Initialize a repo for use with Homeboy
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
}

fn collect_module_cli_info() -> Vec<ModuleCliInfo> {
    load_all_modules()
        .into_iter()
        .filter_map(|m| {
            m.cli.map(|cli| ModuleCliInfo {
                tool: cli.tool,
                display_name: cli.display_name,
                module_name: m.name,
            })
        })
        .collect()
}

fn build_augmented_command(module_info: &[ModuleCliInfo]) -> Command {
    let mut cmd = Cli::command();

    for info in module_info {
        let tool_name: &'static str = Box::leak(info.tool.clone().into_boxed_str());
        cmd = cmd.subcommand(
            Command::new(tool_name)
                .about(format!(
                    "Run {} commands via {}",
                    info.display_name, info.module_name
                ))
                .arg(
                    clap::Arg::new("project_id")
                        .help("Project ID")
                        .required(true)
                        .index(1),
                )
                .arg(
                    clap::Arg::new("args")
                        .help("Command arguments")
                        .index(2)
                        .num_args(0..)
                        .allow_hyphen_values(true),
                )
                .trailing_var_arg(true),
        );
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

/// Normalize version bump arguments to support --patch/--minor/--major syntax.
/// Converts `version bump <component> --patch` to `version bump <component> -- patch`.
/// The bump type must appear last (after `--`) so other flags like `--dry-run` are parsed correctly.
fn normalize_version_bump_args(args: Vec<String>) -> Vec<String> {
    let is_version_bump = args.len() >= 3
        && args.get(1).map(|s| s == "version").unwrap_or(false)
        && args.get(2).map(|s| s == "bump").unwrap_or(false);

    if !is_version_bump {
        return args;
    }

    let bump_flags = ["--patch", "--minor", "--major"];
    let mut result = Vec::new();
    let mut found_bump_type: Option<String> = None;

    for arg in args {
        if bump_flags.contains(&arg.as_str()) && found_bump_type.is_none() {
            found_bump_type = Some(arg.trim_start_matches('-').to_string());
        } else {
            result.push(arg);
        }
    }

    if let Some(bump_type) = found_bump_type {
        result.push("--".to_string());
        result.push(bump_type);
    }

    result
}

fn main() -> std::process::ExitCode {
    let module_info = collect_module_cli_info();
    let cmd = build_augmented_command(&module_info);

    let args: Vec<String> = std::env::args().collect();
    let normalized = normalize_version_bump_args(args);
    let matches = cmd.get_matches_from(normalized);

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
        Err(e) => {
            e.exit();
        }
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

    let (json_result, exit_code) = commands::run_json(cli.command, &global);

    match mode {
        ResponseMode::Json => {
            output::print_json_result(json_result).ok();
        }
        ResponseMode::Raw(RawOutputMode::InteractivePassthrough) => {}
        ResponseMode::Raw(RawOutputMode::Markdown) => {}
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
