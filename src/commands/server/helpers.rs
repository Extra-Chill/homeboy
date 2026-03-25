//! helpers — extracted from server.rs.

use super::super::{CmdResult, DynamicSetArgs};
use super::key_generate;
use super::key_import;
use super::key_show;
use super::key_unset;
use super::key_use;
use super::show;
use super::KeyArgs;
use super::KeyCommand;
use super::ServerArgs;
use super::ServerCommand;
use super::ServerOutput;
use clap::{Args, Subcommand};
use homeboy::server::{self, Server};
use homeboy::{EntityCrudOutput, MergeOutput};
use serde::Serialize;

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

pub(crate) fn run_key(args: KeyArgs) -> CmdResult<ServerOutput> {
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

pub(crate) fn set(args: DynamicSetArgs) -> CmdResult<ServerOutput> {
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

pub(crate) fn delete(server_id: &str) -> CmdResult<ServerOutput> {
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

pub(crate) fn list() -> CmdResult<ServerOutput> {
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
