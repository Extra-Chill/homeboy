use clap::{Parser, Subcommand};

mod commands;
mod docs;

use commands::{
    build, changelog, component, db, deploy, docs as docs_command, file, git, logs, module, pin,
    pm2, project, projects, server, ssh, version, wp,
};

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
    /// Display the changelog
    Changelog,
    /// Git operations for components
    Git(git::GitArgs),
    /// Version management for components
    Version(version::VersionArgs),
    /// Build a component
    Build(build::BuildArgs),
}

fn main() -> std::process::ExitCode {
    let cli = Cli::parse();

    let (result, exit_code) = match cli.command {
        Commands::Projects(args) => {
            let result = projects::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Project(args) => {
            let result = project::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Ssh(args) => {
            let result = ssh::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Wp(args) => {
            let result = wp::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Pm2(args) => {
            let result = pm2::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Server(args) => {
            let result = server::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Db(args) => {
            let result = db::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::File(args) => {
            let result = file::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Logs(args) => {
            let result = logs::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Deploy(args) => {
            let result = deploy::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Component(args) => {
            let result = component::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Pin(args) => {
            let result = pin::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Module(args) => {
            let result = module::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Docs(args) => {
            let result = docs_command::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Changelog => {
            let result = changelog::run();
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Git(args) => {
            let result = git::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Version(args) => {
            let result = version::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
        Commands::Build(args) => {
            let result = build::run(args);
            let exit_code = extract_exit_code(&result);
            match result.map(|(data, _)| data) {
                Ok(data) => match serde_json::to_value(data) {
                    Ok(value) => (Ok(value), exit_code),
                    Err(err) => (
                        Err(homeboy_core::Error::Other(format!(
                            "Failed to serialize output: {}",
                            err
                        ))),
                        1,
                    ),
                },
                Err(err) => (Err(err), exit_code),
            }
        }
    };

    homeboy_core::output::print_result(result);

    std::process::ExitCode::from(exit_code_to_u8(exit_code))
}

fn extract_exit_code<T>(result: &homeboy_core::Result<(T, i32)>) -> i32 {
    match result {
        Ok((_, code)) => *code,
        Err(_) => 1,
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
