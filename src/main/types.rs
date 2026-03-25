//! types — extracted from main.rs.

use clap::{ArgMatches, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use commands::utils::{args, entity_suggest, response as output, tty};
use commands::GlobalArgs;
use commands::{
    api, audit, auth, build, changelog, changes, cli, component, config, db, deploy, extension,
    file, fleet, git, init, lint, logs, project, refactor, release, server, ssh, status, test,
    transfer, undo, upgrade, validate, version,
};

#[derive(Debug, Clone, Copy)]
pub(crate) enum ResponseMode {
    Json,
    Raw(RawOutputMode),
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum RawOutputMode {
    InteractivePassthrough,
    Markdown,
    PlainText,
}

pub(crate) const VERSION: &str = env!("CARGO_PKG_VERSION");

#[derive(Parser)]
#[command(name = "homeboy")]
#[command(version = VERSION)]
#[command(about = "CLI tool for development and deployment automation")]
pub(crate) struct Cli {
    /// Write structured JSON output to a file (in addition to stdout).
    /// The file contains only the JSON envelope — no log text, no timestamps.
    #[arg(long, global = true, value_name = "PATH")]
    output: Option<String>,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
pub(crate) enum Commands {
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
    /// Execute CLI-compatible extensions
    #[command(visible_alias = "extensions")]
    Extension(extension::ExtensionArgs),
    /// Deprecated alias for `status --full`
    #[command(hide = true)]
    Init(init::InitArgs),
    /// Actionable component status overview
    Status(status::StatusArgs),
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
    /// Validate that code compiles/parses (runs extension scripts.validate)
    Validate(validate::ValidateArgs),
    /// Show changes since last version tag
    Changes(changes::ChangesArgs),
    /// Plan release workflows
    Release(release::ReleaseArgs),
    /// Audit code conventions and detect architectural drift
    Audit(audit::AuditArgs),
    /// Structural refactoring (rename terms across codebase)
    Refactor(refactor::RefactorArgs),
    /// Undo the last write operation (audit fix, refactor, etc.)
    Undo(undo::UndoArgs),
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

pub(crate) struct ExtensionCliCommand {
    tool: String,
    project_id: String,
    args: Vec<String>,
}

pub(crate) struct ExtensionCliInfo {
    tool: String,
    display_name: String,
    extension_name: String,
    project_id_help: Option<String>,
    args_help: Option<String>,
    examples: Vec<String>,
}
