use clap::{Args, Subcommand};
use serde::Serialize;
use std::path::PathBuf;

use homeboy::core::ci_profile::{self, CiInventory};
use homeboy::core::engine::execution_context::{self, ResolveOptions};

use super::utils::args::{ExtensionOverrideArgs, HiddenJsonArgs, PositionalComponentArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct CiArgs {
    #[command(subcommand)]
    pub command: CiCommand,
}

#[derive(Subcommand)]
pub enum CiCommand {
    /// List declared CI profiles and shallow discovered CI surfaces.
    List(CiListArgs),
}

#[derive(Args)]
pub struct CiListArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    #[command(flatten)]
    pub _json: HiddenJsonArgs,
}

#[derive(Debug, Serialize)]
pub struct CiListOutput {
    pub command: &'static str,
    pub component_id: String,
    pub source_path: PathBuf,
    pub inventory: CiInventory,
}

pub fn run(args: CiArgs, global: &GlobalArgs) -> CmdResult<CiListOutput> {
    match args.command {
        CiCommand::List(args) => run_list(args, global),
    }
}

fn run_list(args: CiListArgs, _global: &GlobalArgs) -> CmdResult<CiListOutput> {
    let ctx = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: None,
        settings_overrides: Vec::new(),
        settings_json_overrides: Vec::new(),
        extension_overrides: args.extension_override.extensions.clone(),
    })?;
    let extension_ids = ctx
        .component
        .extensions
        .as_ref()
        .map(|extensions| {
            let mut ids: Vec<String> = extensions.keys().cloned().collect();
            ids.sort();
            ids
        })
        .unwrap_or_default();
    let extension_id = ci_profile::select_extension_id(&extension_ids)?;
    let inventory = ci_profile::list_for_extension(&ctx.source_path, &extension_id)?;

    Ok((
        CiListOutput {
            command: "ci.list",
            component_id: ctx.component_id,
            source_path: ctx.source_path,
            inventory,
        },
        0,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[test]
    fn parses_ci_list_path_and_extension() {
        let cli = crate::cli_surface::Cli::try_parse_from([
            "homeboy",
            "ci",
            "list",
            "--path",
            "/tmp/repo",
            "--extension",
            "nodejs",
        ])
        .expect("parse cli");

        let crate::cli_surface::Commands::Ci(args) = cli.command else {
            panic!("expected ci command");
        };
        let CiCommand::List(args) = args.command;

        assert_eq!(args.comp.path.as_deref(), Some("/tmp/repo"));
        assert_eq!(args.extension_override.extensions, vec!["nodejs"]);
    }
}
