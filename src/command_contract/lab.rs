//! Lab portability contract public surface.

use clap::Command;

const LAB_CLI_ARGUMENT_IDS: &[&str] = &[
    "placement",
    "detach_after_handoff",
    "artifact_root",
    "runner",
    "allow_dirty_lab_workspace",
    "skip_deps_hydration",
    "runner_env",
    "lab_env_json",
    "runner_workspace_root",
];

pub fn scope_lab_cli_arguments(command: Command) -> Command {
    let lab_args = command
        .get_arguments()
        .filter(|arg| LAB_CLI_ARGUMENT_IDS.contains(&arg.get_id().as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let command = LAB_CLI_ARGUMENT_IDS.iter().fold(command, |command, id| {
        command.mut_arg(id, |arg| arg.hide(true))
    });
    scope_lab_cli_arguments_at_path(command, &[], &lab_args)
}

fn scope_lab_cli_arguments_at_path(
    command: Command,
    path: &[String],
    lab_args: &[clap::Arg],
) -> Command {
    let visible = !path.is_empty();
    let command = if visible {
        lab_args.iter().fold(command, |command, arg| {
            command.arg(arg.clone().global(false).hide(false))
        })
    } else {
        command
    };
    command.mut_subcommands(|subcommand| {
        let mut subcommand_path = path.to_vec();
        subcommand_path.push(subcommand.get_name().to_string());
        scope_lab_cli_arguments_at_path(subcommand, &subcommand_path, lab_args)
    })
}

mod handoff;
mod placement;
mod support;
#[cfg(test)]
mod tests;
mod types;
mod workload;

pub use handoff::*;
pub use support::*;
pub use types::*;
pub use workload::*;

#[cfg(test)]
pub(crate) use placement::{
    AGENT_TASK_COOK_COORDINATOR_CONTROLLER_REASON, AGENT_TASK_FANOUT_COORDINATOR_CONTROLLER_REASON,
};
