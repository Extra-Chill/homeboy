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

use commands::{
    api, auth, build, changelog, cli, component, context, db, deploy, file, git, init, logs,
    module, project, server, ssh, version,
};
use homeboy::module::load_all_modules;

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "homeboy")]
#[command(version = VERSION)]
#[command(about = "CLI tool for development and deployment automation")]
struct Cli {
    /// Dry-run: show what would happen without writing.
    #[arg(long, global = true)]
    dry_run: bool,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage project configuration
    Project(project::ProjectArgs),
    /// SSH into a project server or configured server
    Ssh(ssh::SshArgs),
    /// Manage SSH server configurations
    Server(server::ServerArgs),
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
    /// Authenticate with a project's API
    Auth(auth::AuthArgs),
    /// Make API requests to a project
    Api(api::ApiArgs),
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
        Commands::Docs(args) if !args.list => ResponseMode::Raw(RawOutputMode::Markdown),
        Commands::Init(_) => ResponseMode::Raw(RawOutputMode::Markdown),
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

    let sub_matches = sub_matches;
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
    let matches = cmd.get_matches();

    let global = GlobalArgs {
        dry_run: matches.get_flag("dry_run"),
    };

    if let Some(module_cmd) = try_parse_module_cli_command(&matches, &module_info) {
        let result = cli::run(
            &module_cmd.tool,
            &module_cmd.project_id,
            module_cmd.args,
            &global,
        );

        let (json_result, exit_code) = homeboy::output::map_cmd_result_to_json(
            result.map(|(data, exit_code)| (data, vec![], exit_code)),
        );
        homeboy::output::print_json_result(json_result);
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
            if !homeboy::tty::require_tty_for_interactive() {
                let err = homeboy::Error::validation_invalid_argument(
                    "tty",
                    "This command requires an interactive TTY",
                    None,
                    None,
                );
                homeboy::output::print_result::<serde_json::Value>(Err(err));
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

    if let ResponseMode::Raw(RawOutputMode::Markdown) = mode {
        let markdown_result = commands::run_markdown(cli.command, &global);

        match markdown_result {
            Ok((content, exit_code)) => {
                print!("{}", content);
                return std::process::ExitCode::from(exit_code_to_u8(exit_code));
            }
            Err(err) => {
                homeboy::output::print_result::<serde_json::Value>(Err(err));
                return std::process::ExitCode::from(exit_code_to_u8(1));
            }
        }
    }

    let (json_result, exit_code) = commands::run_json(cli.command, &global);

    match mode {
        ResponseMode::Json => homeboy::output::print_json_result(json_result),
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
