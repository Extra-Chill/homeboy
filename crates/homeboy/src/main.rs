use clap::{Parser, Subcommand};

mod commands;
mod docs;

use commands::{projects, project, ssh, wp, pm2, server, db, file, logs, deploy, component, pin, module, docs as docs_command, git, version, build};

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
    /// List all configured projects
    Projects(projects::ProjectsArgs),
    /// Manage project configuration
    Project(project::ProjectArgs),
    /// SSH into project server
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
    /// Manage pinned files and logs
    Pin(pin::PinArgs),
    /// Execute CLI-compatible modules
    Module(module::ModuleArgs),
    /// Display CLI documentation
    Docs(docs_command::DocsArgs),
    /// Git operations for components
    Git(git::GitArgs),
    /// Version management for components
    Version(version::VersionArgs),
    /// Build a component
    Build(build::BuildArgs),
}

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Commands::Projects(args) => projects::run(args),
        Commands::Project(args) => project::run(args),
        Commands::Ssh(args) => ssh::run(args),
        Commands::Wp(args) => wp::run(args),
        Commands::Pm2(args) => pm2::run(args),
        Commands::Server(args) => server::run(args),
        Commands::Db(args) => db::run(args),
        Commands::File(args) => file::run(args),
        Commands::Logs(args) => logs::run(args),
        Commands::Deploy(args) => deploy::run(args),
        Commands::Component(args) => component::run(args),
        Commands::Pin(args) => pin::run(args),
        Commands::Module(args) => module::run(args),
        Commands::Docs(args) => docs_command::run(args),
        Commands::Git(args) => git::run(args),
        Commands::Version(args) => version::run(args),
        Commands::Build(args) => build::run(args),
    }
}
