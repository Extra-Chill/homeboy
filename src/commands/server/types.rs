//! types — extracted from server.rs.

use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::server::{self, Server};
use homeboy::{EntityCrudOutput, MergeOutput};
use super::super::{CmdResult, DynamicSetArgs};


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
pub(crate) enum ServerCommand {
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
pub(crate) enum KeyCommand {
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
