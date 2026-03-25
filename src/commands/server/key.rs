//! key — extracted from server.rs.

use super::super::{CmdResult, DynamicSetArgs};
use super::ServerExtra;
use super::ServerKeyOutput;
use super::ServerOutput;
use clap::{Args, Subcommand};
use homeboy::server::{self, Server};
use homeboy::{EntityCrudOutput, MergeOutput};
use serde::Serialize;

pub(crate) fn key_generate(server_id: &str) -> CmdResult<ServerOutput> {
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

pub(crate) fn key_use(server_id: &str, private_key_path: &str) -> CmdResult<ServerOutput> {
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

pub(crate) fn key_unset(server_id: &str) -> CmdResult<ServerOutput> {
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

pub(crate) fn key_import(server_id: &str, private_key_path: &str) -> CmdResult<ServerOutput> {
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
