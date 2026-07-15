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
    // Only re-expose the Lab placement/runner flags on commands that actually
    // support Lab offload. A previous refactor collapsed this to
    // `!path.is_empty()`, which advertised the flags on every subcommand
    // (including non-portable ones like `contract manifest`).
    let visible = lab_cli_arguments_are_visible_for_path(path);
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

/// Command paths that expose the Lab execution placement/runner flags in their
/// `--help`, mirroring the Lab-portable command surface. Kept explicit so the
/// help surface is deterministic and reviewable.
fn lab_cli_arguments_are_visible_for_path(path: &[String]) -> bool {
    matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["agent-task", "cook"]
            | ["agent-task", "run-plan"]
            | ["agent-task", "run"]
            | ["agent-task", "run-next"]
            | ["agent-task", "status"]
            | ["agent-task", "list"]
            | ["agent-task", "active"]
            | ["agent-task", "latest"]
            | ["agent-task", "logs"]
            | ["agent-task", "artifacts"]
            | ["agent-task", "evidence"]
            | ["agent-task", "review"]
            | ["agent-task", "retry"]
            | ["agent-task", "promote"]
            | ["agent-task", "providers"]
            | ["agent-task", "fanout", "submit-batch"]
            | ["agent-task", "fanout", "status"]
            | ["agent-task", "fanout", "artifacts"]
            | ["agent-task", "auth", "status"]
            | ["agent-task", "controller", "from-spec"]
            | ["agent-task", "controller", "run-from-spec"]
            | ["agent-task", "controller", "materialize"]
            | ["agent-task", "controller", "resume"]
            | ["bench"]
            | ["bench", "matrix"]
            | ["fuzz"]
            | ["fuzz", "run"]
            | ["fuzz", "run-campaign"]
            | ["fuzz", "list"]
            | ["fuzz", "plan"]
            | ["fuzz", "doctor"]
            | ["review"]
            | ["review", "audit"]
            | ["review", "lint"]
            | ["review", "test"]
            | ["trace"]
            | ["refactor"]
            | ["rig", "check"]
            | ["rig", "run"]
            | ["runtime", "refresh"]
            | ["worktree", "cleanup"]
            | ["extension", "update"]
            | ["extension", "refresh"]
            | ["extension", "dev-run"]
            | ["extension", "show"]
            | ["tunnel", "preview-consumer", "run"]
            | ["tunnel", "service", "expose"]
            | ["tunnel", "service", "start"]
    )
}

mod support;

// The lab contract types (workload / handoff / typed identifiers) and the
// lab-runnable command labels now live in the homeboy-lab-contract crate.
// Re-exported here so existing `command_contract::lab::*` (and the top-level
// `command_contract::*`) call sites are unchanged.
pub use homeboy_lab_contract::lab::handoff::*;
pub use homeboy_lab_contract::lab::labels::*;
pub use homeboy_lab_contract::lab::types::*;
pub use homeboy_lab_contract::lab::workload::*;
pub use support::*;
