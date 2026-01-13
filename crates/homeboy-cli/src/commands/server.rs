use clap::{Args, Subcommand};
use serde::Serialize;
use std::fs;
use std::process::Command;

use homeboy::config::{create_from_json, slugify_id, AppPaths, ConfigManager, CreateSummary, ServerConfig};
use homeboy::Error;

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerOutput {
    command: String,
    server_id: Option<String>,
    server: Option<ServerConfig>,
    servers: Option<Vec<ServerConfig>>,
    updated: Option<Vec<String>>,
    deleted: Option<Vec<String>>,
    key: Option<ServerKeyOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    import: Option<CreateSummary>,
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
            name,
            host,
            user,
            port,
        } => set(&server_id, name, host, user, port),
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
    let summary = create_from_json::<ServerConfig>(spec, skip_existing)?;
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
    let id = slugify_id(name)?;

    if ConfigManager::load_server(&id).is_ok() {
        return Err(Error::other(format!("Server '{}' already exists", id)));
    }

    let server = ServerConfig {
        id: id.clone(),
        name: name.to_string(),
        host: host.to_string(),
        user: user.to_string(),
        port,
        identity_file: None,
    };

    ConfigManager::save_server(&id, &server)?;

    Ok((
        ServerOutput {
            command: "server.create".to_string(),
            server_id: Some(id),
            server: Some(server),
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
    let server = ConfigManager::load_server(server_id)?;

    Ok((
        ServerOutput {
            command: "server.show".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(server),
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
    name: Option<String>,
    host: Option<String>,
    user: Option<String>,
    port: Option<u16>,
) -> homeboy::Result<(ServerOutput, i32)> {
    let mut server = ConfigManager::load_server(server_id)?;

    let mut changes = Vec::new();

    if let Some(n) = name {
        server.name = n;
        changes.push("name".to_string());
    }
    if let Some(h) = host {
        server.host = h;
        changes.push("host".to_string());
    }
    if let Some(u) = user {
        server.user = u;
        changes.push("user".to_string());
    }
    if let Some(p) = port {
        server.port = p;
        changes.push("port".to_string());
    }

    if changes.is_empty() {
        return Err(Error::other("No changes specified".to_string()));
    }

    ConfigManager::save_server(server_id, &server)?;

    Ok((
        ServerOutput {
            command: "server.set".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(server),
            servers: None,
            updated: Some(changes),
            deleted: None,
            key: None,
            import: None,
        },
        0,
    ))
}

fn delete(server_id: &str, force: bool) -> homeboy::Result<(ServerOutput, i32)> {
    if !force {
        return Err(Error::other("Use --force to confirm deletion".to_string()));
    }

    ConfigManager::load_server(server_id)?;

    let projects = ConfigManager::list_projects()?;
    for project in projects {
        if project.config.server_id.as_deref() == Some(server_id) {
            return Err(Error::other(format!(
                "Server is used by project '{}'. Update or delete the project first.",
                project.id
            )));
        }
    }

    ConfigManager::delete_server(server_id)?;

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
    let servers = ConfigManager::list_servers()?;

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
    ConfigManager::load_server(server_id)?;

    let key_path = AppPaths::key(server_id)?;
    let key_path_str = key_path.to_string_lossy().to_string();

    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("create ssh key directory".to_string()),
            )
        })?;
    }

    let _ = fs::remove_file(&key_path);
    let _ = fs::remove_file(format!("{}.pub", key_path_str));

    let output = Command::new("ssh-keygen")
        .args([
            "-t",
            "rsa",
            "-b",
            "4096",
            "-f",
            &key_path_str,
            "-N",
            "",
            "-C",
            &format!("homeboy-{}", server_id),
        ])
        .output()
        .map_err(|err| Error::internal_io(err.to_string(), Some("run ssh-keygen".to_string())))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::internal_unexpected(format!(
            "ssh-keygen failed: {}",
            stderr
        )));
    }

    let mut server = ConfigManager::load_server(server_id)?;
    server.identity_file = Some(key_path_str.clone());
    ConfigManager::save_server(server_id, &server)?;

    let pub_key_path = format!("{}.pub", key_path_str);
    let public_key = fs::read_to_string(&pub_key_path).map_err(|err| {
        Error::internal_io(
            err.to_string(),
            Some("read generated ssh public key".to_string()),
        )
    })?;

    Ok((
        ServerOutput {
            command: "server.key.generate".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(server),
            servers: None,
            updated: Some(vec!["identity_file".to_string()]),
            deleted: None,
            key: Some(ServerKeyOutput {
                action: "generate".to_string(),
                server_id: server_id.to_string(),
                public_key: Some(public_key.trim().to_string()),
                identity_file: Some(key_path_str),
                imported: None,
            }),
            import: None,
        },
        0,
    ))
}

fn key_show(server_id: &str) -> homeboy::Result<(ServerOutput, i32)> {
    ConfigManager::load_server(server_id)?;

    let key_path = AppPaths::key(server_id)?;
    let pub_key_path = format!("{}.pub", key_path.to_string_lossy());

    let public_key = fs::read_to_string(&pub_key_path).map_err(|err| {
        if err.kind() == std::io::ErrorKind::NotFound {
            Error::ssh_identity_file_not_found(server_id.to_string(), pub_key_path.clone())
        } else {
            Error::internal_io(err.to_string(), Some("read ssh public key".to_string()))
        }
    })?;

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
                public_key: Some(public_key.trim().to_string()),
                identity_file: None,
                imported: None,
            }),
            import: None,
        },
        0,
    ))
}

fn key_use(server_id: &str, private_key_path: &str) -> homeboy::Result<(ServerOutput, i32)> {
    let mut server = ConfigManager::load_server(server_id)?;

    let expanded_path = shellexpand::tilde(private_key_path).to_string();

    if !std::path::Path::new(&expanded_path).exists() {
        return Err(Error::ssh_identity_file_not_found(
            server_id.to_string(),
            expanded_path,
        ));
    }

    server.identity_file = Some(expanded_path.clone());
    ConfigManager::save_server(server_id, &server)?;

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
                identity_file: Some(expanded_path),
                imported: None,
            }),
            import: None,
        },
        0,
    ))
}

fn key_unset(server_id: &str) -> homeboy::Result<(ServerOutput, i32)> {
    let mut server = ConfigManager::load_server(server_id)?;

    server.identity_file = None;
    ConfigManager::save_server(server_id, &server)?;

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
    ConfigManager::load_server(server_id)?;

    let expanded_path = shellexpand::tilde(private_key_path).to_string();

    let private_key = fs::read_to_string(&expanded_path).map_err(|err| {
        Error::internal_io(err.to_string(), Some("read ssh private key".to_string()))
    })?;

    if !private_key.contains("-----BEGIN") || !private_key.contains("PRIVATE KEY-----") {
        return Err(Error::validation_invalid_argument(
            "privateKeyPath",
            "File doesn't appear to be a valid SSH private key",
            Some(server_id.to_string()),
            Some(vec![expanded_path.clone()]),
        ));
    }

    let output = Command::new("ssh-keygen")
        .args(["-y", "-f", &expanded_path])
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some("run ssh-keygen -y".to_string()))
        })?;

    if !output.status.success() {
        return Err(Error::internal_unexpected(
            "Failed to derive public key from private key".to_string(),
        ));
    }

    let public_key = String::from_utf8_lossy(&output.stdout).trim().to_string();

    let key_path = AppPaths::key(server_id)?;
    let key_path_str = key_path.to_string_lossy().to_string();

    if let Some(parent) = key_path.parent() {
        fs::create_dir_all(parent).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("create ssh key directory".to_string()),
            )
        })?;
    }

    fs::write(&key_path, &private_key).map_err(|err| {
        Error::internal_io(err.to_string(), Some("write ssh private key".to_string()))
    })?;

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some("set ssh private key permissions".to_string()),
            )
        })?;
    }

    fs::write(format!("{}.pub", key_path_str), &public_key).map_err(|err| {
        Error::internal_io(err.to_string(), Some("write ssh public key".to_string()))
    })?;

    let mut server = ConfigManager::load_server(server_id)?;
    server.identity_file = Some(key_path_str.clone());
    ConfigManager::save_server(server_id, &server)?;

    Ok((
        ServerOutput {
            command: "server.key.import".to_string(),
            server_id: Some(server_id.to_string()),
            server: Some(server),
            servers: None,
            updated: Some(vec!["identity_file".to_string()]),
            deleted: None,
            key: Some(ServerKeyOutput {
                action: "import".to_string(),
                server_id: server_id.to_string(),
                public_key: Some(public_key),
                identity_file: Some(key_path_str),
                imported: Some(expanded_path),
            }),
            import: None,
        },
        0,
    ))
}
