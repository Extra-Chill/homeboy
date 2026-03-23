use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::server::{self, Server};
use homeboy::{EntityCrudOutput, MergeOutput};

use super::{CmdResult, DynamicSetArgs};

/// Entity-specific fields for server commands.
#[derive(Debug, Default, Serialize)]
pub struct ServerExtra {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<ServerKeyOutput>,
}

pub type ServerOutput = EntityCrudOutput<Server, ServerExtra>;

#[derive(Debug, Serialize)]

pub struct ServerKeyOutput {
    action: String,
    server_id: String,
    public_key: Option<String>,
    identity_file: Option<String>,
    imported: Option<String>,
}

#[derive(Args)]
pub struct ServerArgs {
    #[command(subcommand)]
    command: ServerCommand,
}

#[derive(Subcommand)]
enum ServerCommand {
    /// Register a new SSH server
    Create {
        /// JSON input spec for create/update (supports single or bulk)
        #[arg(long)]
        json: Option<String>,

        /// Skip items that already exist (JSON mode only)
        #[arg(long)]
        skip_existing: bool,

        /// Server ID (CLI mode)
        id: Option<String>,
        /// SSH host
        #[arg(long)]
        host: Option<String>,
        /// SSH username
        #[arg(long)]
        user: Option<String>,
        /// SSH port (default: 22)
        #[arg(long)]
        port: Option<u16>,
    },
    /// Display server configuration
    Show {
        /// Server ID
        server_id: String,
    },
    /// Modify server settings
    #[command(visible_aliases = ["edit", "merge"])]
    Set {
        #[command(flatten)]
        args: DynamicSetArgs,
    },
    /// Remove a server configuration
    Delete {
        /// Server ID
        server_id: String,
    },
    /// List all configured servers
    List,
    /// Manage SSH keys
    Key(KeyArgs),
}

#[derive(Args)]
pub struct KeyArgs {
    #[command(subcommand)]
    command: KeyCommand,
}

#[derive(Subcommand)]
enum KeyCommand {
    /// Generate a new SSH key pair and set it for this server
    Generate {
        /// Server ID
        server_id: String,
    },
    /// Display the public SSH key
    Show {
        /// Server ID
        server_id: String,
    },
    /// Import an existing SSH private key and set it for this server
    Import {
        /// Server ID
        server_id: String,
        /// Path to private key file
        private_key_path: String,
    },
    /// Use an existing SSH private key file path for this server
    Use {
        /// Server ID
        server_id: String,
        /// Path to private key file
        private_key_path: String,
    },
    /// Unset the server SSH identity file (use normal SSH resolution)
    Unset {
        /// Server ID
        server_id: String,
    },
}

pub fn run(args: ServerArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<ServerOutput> {
    match args.command {
        ServerCommand::Create {
            json,
            skip_existing,
            id,
            host,
            user,
            port,
        } => {
            let json_spec = if let Some(spec) = json {
                spec
            } else {
                let id = id.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "id",
                        "Missing required argument: id",
                        None,
                        None,
                    )
                })?;

                let host = host.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "host",
                        "Missing required argument: --host",
                        None,
                        None,
                    )
                })?;

                let user = user.ok_or_else(|| {
                    homeboy::Error::validation_invalid_argument(
                        "user",
                        "Missing required argument: --user",
                        None,
                        None,
                    )
                })?;

                let new_server = server::Server {
                    id,
                    aliases: Vec::new(),
                    host,
                    user,
                    port: port.unwrap_or(22),
                    identity_file: None,
                    env: std::collections::HashMap::new(),
                };

                homeboy::config::to_json_string(&new_server)?
            };

            match server::create(&json_spec, skip_existing)? {
                homeboy::CreateOutput::Single(result) => Ok((
                    ServerOutput {
                        command: "server.create".to_string(),
                        id: Some(result.id),
                        entity: Some(result.entity),
                        updated_fields: vec!["created".to_string()],
                        ..Default::default()
                    },
                    0,
                )),
                homeboy::CreateOutput::Bulk(summary) => {
                    let exit_code = summary.exit_code();
                    Ok((
                        ServerOutput {
                            command: "server.create".to_string(),
                            import: Some(summary),
                            ..Default::default()
                        },
                        exit_code,
                    ))
                }
            }
        }
        ServerCommand::Show { server_id } => show(&server_id),
        ServerCommand::Set { args } => set(args),
        ServerCommand::Delete { server_id } => delete(&server_id),
        ServerCommand::List => list(),
        ServerCommand::Key(key_args) => run_key(key_args),
    }
}

fn run_key(args: KeyArgs) -> CmdResult<ServerOutput> {
    match args.command {
        KeyCommand::Generate { server_id } => key_generate(&server_id),
        KeyCommand::Show { server_id } => key_show(&server_id),
        KeyCommand::Import {
            server_id,
            private_key_path,
        } => key_import(&server_id, &private_key_path),
        KeyCommand::Use {
            server_id,
            private_key_path,
        } => key_use(&server_id, &private_key_path),
        KeyCommand::Unset { server_id } => key_unset(&server_id),
    }
}

fn show(server_id: &str) -> CmdResult<ServerOutput> {
    let svr = server::load(server_id)
        .or_else(|original_error| server::find_by_host(server_id).ok_or(original_error))?;

    Ok((
        ServerOutput {
            command: "server.show".to_string(),
            id: Some(svr.id.clone()),
            entity: Some(svr),
            ..Default::default()
        },
        0,
    ))
}

fn set(args: DynamicSetArgs) -> CmdResult<ServerOutput> {
    let merged = super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "spec",
            "Provide JSON spec, --json flag, --base64 flag, or --key value flags",
            None,
            None,
        )
    })?;
    let (json_string, replace_fields) = super::finalize_set_spec(&merged, &args.replace)?;

    match server::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        MergeOutput::Single(result) => {
            let svr = server::load(&result.id)?;
            Ok((
                ServerOutput {
                    command: "server.set".to_string(),
                    id: Some(result.id),
                    entity: Some(svr),
                    updated_fields: result.updated_fields,
                    ..Default::default()
                },
                0,
            ))
        }
        MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                ServerOutput {
                    command: "server.set".to_string(),
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

fn delete(server_id: &str) -> CmdResult<ServerOutput> {
    server::delete_safe(server_id)?;

    Ok((
        ServerOutput {
            command: "server.delete".to_string(),
            id: Some(server_id.to_string()),
            deleted: vec![server_id.to_string()],
            ..Default::default()
        },
        0,
    ))
}

fn list() -> CmdResult<ServerOutput> {
    let servers = server::list()?;

    Ok((
        ServerOutput {
            command: "server.list".to_string(),
            entities: servers,
            ..Default::default()
        },
        0,
    ))
}

fn key_generate(server_id: &str) -> CmdResult<ServerOutput> {
    let result = server::generate_key(server_id)?;

    Ok((
        ServerOutput {
            command: "server.key.generate".to_string(),
            id: Some(server_id.to_string()),
            entity: Some(result.server),
            updated_fields: vec!["identity_file".to_string()],
            extra: ServerExtra {
                key: Some(ServerKeyOutput {
                    action: "generate".to_string(),
                    server_id: server_id.to_string(),
                    public_key: Some(result.public_key),
                    identity_file: Some(result.identity_file),
                    imported: None,
                }),
            },
            ..Default::default()
        },
        0,
    ))
}

fn key_show(server_id: &str) -> CmdResult<ServerOutput> {
    let public_key = server::get_public_key(server_id)?;

    Ok((
        ServerOutput {
            command: "server.key.show".to_string(),
            id: Some(server_id.to_string()),
            extra: ServerExtra {
                key: Some(ServerKeyOutput {
                    action: "show".to_string(),
                    server_id: server_id.to_string(),
                    public_key: Some(public_key),
                    identity_file: None,
                    imported: None,
                }),
            },
            ..Default::default()
        },
        0,
    ))
}

fn key_use(server_id: &str, private_key_path: &str) -> CmdResult<ServerOutput> {
    let server = server::use_key(server_id, private_key_path)?;
    let identity_file = server.identity_file.clone();

    Ok((
        ServerOutput {
            command: "server.key.use".to_string(),
            id: Some(server_id.to_string()),
            entity: Some(server),
            updated_fields: vec!["identity_file".to_string()],
            extra: ServerExtra {
                key: Some(ServerKeyOutput {
                    action: "use".to_string(),
                    server_id: server_id.to_string(),
                    public_key: None,
                    identity_file,
                    imported: None,
                }),
            },
            ..Default::default()
        },
        0,
    ))
}

fn key_unset(server_id: &str) -> CmdResult<ServerOutput> {
    let server = server::unset_key(server_id)?;

    Ok((
        ServerOutput {
            command: "server.key.unset".to_string(),
            id: Some(server_id.to_string()),
            entity: Some(server),
            updated_fields: vec!["identity_file".to_string()],
            extra: ServerExtra {
                key: Some(ServerKeyOutput {
                    action: "unset".to_string(),
                    server_id: server_id.to_string(),
                    public_key: None,
                    identity_file: None,
                    imported: None,
                }),
            },
            ..Default::default()
        },
        0,
    ))
}

fn key_import(server_id: &str, private_key_path: &str) -> CmdResult<ServerOutput> {
    let result = server::import_key(server_id, private_key_path)?;

    Ok((
        ServerOutput {
            command: "server.key.import".to_string(),
            id: Some(server_id.to_string()),
            entity: Some(result.server),
            updated_fields: vec!["identity_file".to_string()],
            extra: ServerExtra {
                key: Some(ServerKeyOutput {
                    action: "import".to_string(),
                    server_id: server_id.to_string(),
                    public_key: Some(result.public_key),
                    identity_file: Some(result.identity_file),
                    imported: Some(result.imported_from),
                }),
            },
            ..Default::default()
        },
        0,
    ))
}
