use clap::{Parser, Subcommand};

fn validate_json_mode(command: &Commands) -> homeboy_core::Result<()> {
    match command {
        Commands::Changelog(_) => Ok(()),
        _ => Err(homeboy_core::Error::validation_invalid_argument(
            "json",
            "--json is not supported for this command yet",
            None,
            None,
        )),
    }
}

mod commands;
mod docs;

use commands::{
    build, changelog, component, config, db, deploy, docs as docs_command, doctor, file, git, logs,
    module, pm2, project, server, ssh, version, wp,
};

const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "homeboy")]
#[command(version = VERSION)]
#[command(about = "CLI tool for development and deployment automation")]
struct Cli {
    #[arg(long, global = true)]
    json: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Manage project configuration
    Project(project::ProjectArgs),
    /// SSH into a project server or configured server
    Ssh(ssh::SshArgs),
    /// Run WP-CLI commands on WordPress projects
    Wp(wp::WpArgs),
    /// Run PM2 commands on Node.js projects
    Pm2(pm2::Pm2Args),
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
    /// Manage global config.json
    Config(config::ConfigArgs),
    /// Execute CLI-compatible modules
    Module(module::ModuleArgs),
    /// Display CLI documentation
    Docs(docs_command::DocsArgs),
    /// Changelog operations
    Changelog(changelog::ChangelogArgs),
    /// Git operations for components
    Git(git::GitArgs),
    /// Version management for components
    Version(version::VersionArgs),
    /// Build a component
    Build(build::BuildArgs),
    /// Diagnose configuration problems
    Doctor(doctor::DoctorArgs),
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    if cli.json.is_some() {
        if let Err(err) = validate_json_mode(&cli.command) {
            homeboy_core::output::print_result::<serde_json::Value>(Err(err));
            return std::process::ExitCode::from(exit_code_to_u8(2));
        }
    }

    let (json_result, exit_code) = match cli.command {
        Commands::Project(args) => homeboy_core::output::map_cmd_result_to_json(project::run(args)),
        Commands::Ssh(args) => homeboy_core::output::map_cmd_result_to_json(ssh::run(args)),
        Commands::Wp(args) => homeboy_core::output::map_cmd_result_to_json(wp::run(args)),
        Commands::Pm2(args) => homeboy_core::output::map_cmd_result_to_json(pm2::run(args)),
        Commands::Server(args) => homeboy_core::output::map_cmd_result_to_json(server::run(args)),
        Commands::Db(args) => homeboy_core::output::map_cmd_result_to_json(db::run(args)),
        Commands::File(args) => homeboy_core::output::map_cmd_result_to_json(file::run(args)),
        Commands::Logs(args) => homeboy_core::output::map_cmd_result_to_json(logs::run(args)),
        Commands::Deploy(args) => homeboy_core::output::map_cmd_result_to_json(deploy::run(args)),
        Commands::Component(args) => {
            homeboy_core::output::map_cmd_result_to_json(component::run(args))
        }
        Commands::Config(args) => homeboy_core::output::map_cmd_result_to_json(config::run(args)),
        Commands::Module(args) => homeboy_core::output::map_cmd_result_to_json(module::run(args)),
        Commands::Docs(args) => {
            homeboy_core::output::map_cmd_result_to_json(docs_command::run(args))
        }
        Commands::Changelog(args) => {
            homeboy_core::output::map_cmd_result_to_json(changelog::run(args))
        }
        Commands::Git(args) => homeboy_core::output::map_cmd_result_to_json(git::run(args)),
        Commands::Version(args) => homeboy_core::output::map_cmd_result_to_json(version::run(args)),
        Commands::Build(args) => homeboy_core::output::map_cmd_result_to_json(build::run(args)),
        Commands::Doctor(args) => homeboy_core::output::map_cmd_result_to_json(doctor::run(args)),
    };

    homeboy_core::output::print_json_result(json_result);

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
