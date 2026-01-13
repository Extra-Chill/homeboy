use clap::{Args, Subcommand};
use serde::Serialize;

use homeboy::Error;
use homeboy::server::{self, Server};
use homeboy::project;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerOutput {
    command: String,
    server_id: Option<String>,
    server: Option<Server>,
    servers: Option<Vec<Server>>,
    updated: Option<Vec<String>>,
    deleted: Option<Vec<String>>,
    key: Option<ServerKeyOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    import: Option<server::CreateSummary>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
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

        /// Server display name (CLI mode)
        name: Option<String>,
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
    Set {
        /// Server ID
        server_id: String,
        /// Merge JSON object into config (any field, supports @file and - for stdin)
        #[arg(long, value_name = "JSON")]
        merge: Option<String>,
        /// Server display name
        #[arg(long)]
        name: Option<String>,
        /// SSH host
        #[arg(long)]
        host: Option<String>,
        /// SSH username
        #[arg(long)]
        user: Option<String>,
        /// SSH port
        #[arg(long)]
        port: Option<u16>,
    },
    /// Remove a server configuration
    Delete {
        /// Server ID
        server_id: String,
        /// Confirm deletion
        #[arg(long)]
        force: bool,
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

pub fn run(
    args: ServerArgs,
    _global: &crate::commands::GlobalArgs,
) -> homeboy::Result<(ServerOutput, i32)> {
    match args.command {
        ServerCommand::Create {
            json,
            skip_existing,
            name,
            host,
            user,
            port,
        } => {
            if let Some(spec) = json {
                return create_json(&spec, skip_existing);
            }

            let name = name.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "name",
                    "Missing required argument: name (or use --json)",
                    None,
                    None,
                )
            })?;
            let host = host.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "host",
                    "Missing required argument: --host (or use --json)",
                    None,
                    None,
                )
            })?;
            let user = user.ok_or_else(|| {
                homeboy::Error::validation_invalid_argument(
                    "user",
                    "Missing required argument: --user (or use --json)",
                    None,
                    None,
                )
            })?;

            create(&name, &host, &user, port.unwrap_or(22))
        }
        ServerCommand::Show { server_id } => show(&server_id),
        ServerCommand::Set {
            server_id,
            merge,
            name,
            host,
            user,
            port,
        } => set(&server_id, merge, name, host, user, port),
        ServerCommand::Delete { server_id, force } => delete(&server_id, force),
        ServerCommand::List => list(),
        ServerCommand::Key(key_args) => run_key(key_args),
    }
}

fn run_key(args: KeyArgs) -> homeboy::Result<(ServerOutput, i32)> {
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

fn create_json(spec: &str, skip_existing: bool) -> homeboy::Result<(ServerOutput, i32)> {
    let summary = server::create_from_json(spec, skip_existing)?;
    let exit_code = if summary.errors > 0 { 1 } else { 0 };

    Ok((
        ServerOutput {
            command: "server.create".to_string(),
            server_id: None,
            server: None,
            servers: None,
            updated: None,
            deleted: None,
            key: None,
            import: Some(summary),
        },
        exit_code,
    ))
}

fn create(
    name: &str,
    host: &str,
    user: &str,
    port: u16,
) -> homeboy::Result<(ServerOutput, i32)> {
    let result = server::create_from_cli(
        Some(name.to_string()),
        Some(host.to_string()),
        Some(user.to_string()),
        Some(port),
    )?;

    Ok((
        ServerOutput {
            command: "server.create".to_string(),
            server_id: Some(result.id),
            server: Some(result.server),
            servers: None,
            updated: Some(vec!["created".to_string()]),
            deleted: None,
            key: None,
            import: None,
        },
        0,
    ))
}

fn show(server_id: &str) -> homeboy::Result<(ServerOutput, i32)> {
    let svr = server::load(server_id)?;

    Ok((
        ServerOutput {
            command: "server.show".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(svr),
            servers: None,
            updated: None,
            deleted: None,
            key: None,
            import: None,
        },
        0,
    ))
}

fn set(
    server_id: &str,
    merge: Option<String>,
    name: Option<String>,
    host: Option<String>,
    user: Option<String>,
    port: Option<u16>,
) -> homeboy::Result<(ServerOutput, i32)> {
    // Handle --merge via public API (mutually exclusive with individual flags)
    if let Some(spec) = merge {
        let result = server::merge_from_json(server_id, &spec)?;
        let server = server::load(server_id)?;
        return Ok((
            ServerOutput {
                command: "server.set".to_string(),
                server_id: Some(server_id.to_string()),
                server: Some(server),
                servers: None,
                updated: Some(result.updated_fields),
                deleted: None,
                key: None,
                import: None,
            },
            0,
        ));
    }

    if name.is_none() && host.is_none() && user.is_none() && port.is_none() {
        return Err(Error::validation_invalid_argument(
            "fields",
            "No changes specified",
            None,
            None,
        ));
    }

    let result = server::update(server_id, name, host, user, port)?;

    Ok((
        ServerOutput {
            command: "server.set".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(result.server),
            servers: None,
            updated: Some(result.updated_fields),
            deleted: None,
            key: None,
            import: None,
        },
        0,
    ))
}

fn delete(server_id: &str, force: bool) -> homeboy::Result<(ServerOutput, i32)> {
    if !force {
        return Err(Error::validation_invalid_argument(
            "force",
            "Use --force to confirm deletion",
            None,
            None,
        ));
    }

    server::load(server_id)?;

    let projects = project::list()?;
    for proj in projects {
        if proj.config.server_id.as_deref() == Some(server_id) {
            return Err(Error::validation_invalid_argument(
                "server",
                format!(
                    "Server is used by project '{}'. Update or delete the project first.",
                    proj.id
                ),
                Some(server_id.to_string()),
                Some(vec![proj.id.clone()]),
            ));
        }
    }

    server::delete(server_id)?;

    Ok((
        ServerOutput {
            command: "server.delete".to_string(),
            server_id: Some(server_id.to_string()),
            server: None,
            servers: None,
            updated: None,
            deleted: Some(vec![server_id.to_string()]),
            key: None,
            import: None,
        },
        0,
    ))
}

fn list() -> homeboy::Result<(ServerOutput, i32)> {
    let servers = server::list()?;

    Ok((
        ServerOutput {
            command: "server.list".to_string(),
            server_id: None,
            server: None,
            servers: Some(servers),
            updated: None,
            deleted: None,
            key: None,
            import: None,
        },
        0,
    ))
}

fn key_generate(server_id: &str) -> homeboy::Result<(ServerOutput, i32)> {
    let result = server::generate_key(server_id)?;

    Ok((
        ServerOutput {
            command: "server.key.generate".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(result.server),
            servers: None,
            updated: Some(vec!["identity_file".to_string()]),
            deleted: None,
            key: Some(ServerKeyOutput {
                action: "generate".to_string(),
                server_id: server_id.to_string(),
                public_key: Some(result.public_key),
                identity_file: Some(result.identity_file),
                imported: None,
            }),
            import: None,
        },
        0,
    ))
}

fn key_show(server_id: &str) -> homeboy::Result<(ServerOutput, i32)> {
    let public_key = server::get_public_key(server_id)?;

    Ok((
        ServerOutput {
            command: "server.key.show".to_string(),
            server_id: Some(server_id.to_string()),
            server: None,
            servers: None,
            updated: None,
            deleted: None,
            key: Some(ServerKeyOutput {
                action: "show".to_string(),
                server_id: server_id.to_string(),
                public_key: Some(public_key),
                identity_file: None,
                imported: None,
            }),
            import: None,
        },
        0,
    ))
}

fn key_use(server_id: &str, private_key_path: &str) -> homeboy::Result<(ServerOutput, i32)> {
    let server = server::use_key(server_id, private_key_path)?;
    let identity_file = server.identity_file.clone();

    Ok((
        ServerOutput {
            command: "server.key.use".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(server),
            servers: None,
            updated: Some(vec!["identity_file".to_string()]),
            deleted: None,
            key: Some(ServerKeyOutput {
                action: "use".to_string(),
                server_id: server_id.to_string(),
                public_key: None,
                identity_file,
                imported: None,
            }),
            import: None,
        },
        0,
    ))
}

fn key_unset(server_id: &str) -> homeboy::Result<(ServerOutput, i32)> {
    let server = server::unset_key(server_id)?;

    Ok((
        ServerOutput {
            command: "server.key.unset".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(server),
            servers: None,
            updated: Some(vec!["identity_file".to_string()]),
            deleted: None,
            key: Some(ServerKeyOutput {
                action: "unset".to_string(),
                server_id: server_id.to_string(),
                public_key: None,
                identity_file: None,
                imported: None,
            }),
            import: None,
        },
        0,
    ))
}

fn key_import(
    server_id: &str,
    private_key_path: &str,
) -> homeboy::Result<(ServerOutput, i32)> {
    let result = server::import_key(server_id, private_key_path)?;

    Ok((
        ServerOutput {
            command: "server.key.import".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(result.server),
            servers: None,
            updated: Some(vec!["identity_file".to_string()]),
            deleted: None,
            key: Some(ServerKeyOutput {
                action: "import".to_string(),
                server_id: server_id.to_string(),
                public_key: Some(result.public_key),
                identity_file: Some(result.identity_file),
                imported: Some(result.imported_from),
            }),
            import: None,
        },
        0,
    ))
}
