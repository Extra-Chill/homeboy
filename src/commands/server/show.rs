//! show — extracted from server.rs.

use homeboy::server::{self, Server};
use super::super::{CmdResult, DynamicSetArgs};
use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::{EntityCrudOutput, MergeOutput};
use super::ServerExtra;
use super::ServerKeyOutput;
use super::ServerOutput;


pub(crate) fn show(server_id: &str) -> CmdResult<ServerOutput> {
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

pub(crate) fn key_show(server_id: &str) -> CmdResult<ServerOutput> {
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
