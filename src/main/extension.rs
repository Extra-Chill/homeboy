//! extension — extracted from main.rs.

use super::ExtensionCliCommand;
use super::ExtensionCliInfo;
use clap::{ArgMatches, Command, CommandFactory, FromArgMatches, Parser, Subcommand};
use commands::utils::{args, entity_suggest, response as output, tty};
use commands::GlobalArgs;
use commands::{
    api, audit, auth, build, changelog, changes, cli, component, config, db, deploy, extension,
    file, fleet, git, init, lint, logs, project, refactor, release, server, ssh, status, test,
    transfer, undo, upgrade, validate, version,
};
use homeboy::extension::load_all_extensions;

pub(crate) fn collect_extension_cli_info() -> Vec<ExtensionCliInfo> {
    load_all_extensions()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|m| {
            m.cli.map(|cli| {
                let help = cli.help.unwrap_or_default();
                ExtensionCliInfo {
                    tool: cli.tool,
                    display_name: cli.display_name,
                    extension_name: m.name,
                    project_id_help: help.project_id_help,
                    args_help: help.args_help,
                    examples: help.examples,
                }
            })
        })
        .collect()
}

pub(crate) fn try_parse_extension_cli_command(
    matches: &ArgMatches,
    extension_info: &[ExtensionCliInfo],
) -> Option<ExtensionCliCommand> {
    let (tool, sub_matches) = matches.subcommand()?;

    if !extension_info.iter().any(|m| m.tool == tool) {
        return None;
    }

    let project_id = sub_matches.get_one::<String>("project_id")?.clone();
    let args: Vec<String> = sub_matches
        .get_many::<String>("args")
        .map(|vals| vals.cloned().collect())
        .unwrap_or_default();

    Some(ExtensionCliCommand {
        tool: tool.to_string(),
        project_id,
        args,
    })
}
