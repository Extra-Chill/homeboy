//! Lab portability contract public surface.

use clap::Command;

/// Declarative help and runner-hint surface for one Lab-capable command family.
/// Capability packages provide this alongside their parsed-match route resolver.
#[derive(Debug, Clone, Copy)]
pub struct LabCommandRouteSupport {
    pub visible_paths: &'static [&'static [&'static str]],
    pub message_label: &'static str,
    pub hint_label: &'static str,
}

const LAB_CLI_ARGUMENT_IDS: &[&str] = &[
    "placement",
    "detach_after_handoff",
    "artifact_root",
    "runner",
    "allow_dirty_lab_workspace",
    "skip_deps_hydration",
    "preserve_workspace_on_failure",
    "runner_env",
    "runner_secret_env",
    "lab_env_json",
    "runner_workspace_root",
];

pub(crate) fn scope_lab_cli_arguments(command: Command) -> Command {
    scope_lab_cli_arguments_with(command, &[])
}

pub(crate) fn scope_lab_cli_arguments_with(
    command: Command,
    composed_support: &[LabCommandRouteSupport],
) -> Command {
    let lab_args = command
        .get_arguments()
        .filter(|arg| LAB_CLI_ARGUMENT_IDS.contains(&arg.get_id().as_str()))
        .cloned()
        .collect::<Vec<_>>();
    let command = LAB_CLI_ARGUMENT_IDS.iter().fold(command, |command, id| {
        command.mut_arg(id, |arg| {
            let arg = arg.hide(true);
            if matches!(*id, "placement" | "runner") {
                arg.conflicts_with(clap::builder::Resettable::<clap::Id>::Reset)
            } else {
                arg
            }
        })
    });
    scope_cook_help(scope_lab_cli_arguments_at_path(
        command,
        &[],
        &lab_args,
        composed_support,
        true,
    ))
}

/// Add scoped Lab arguments only for capability-composed paths. The base CLI
/// has already scoped its static command tree before capabilities are attached.
pub(crate) fn scope_composed_lab_cli_arguments(
    command: Command,
    composed_support: &[LabCommandRouteSupport],
) -> Command {
    let lab_args = command
        .get_arguments()
        .filter(|arg| LAB_CLI_ARGUMENT_IDS.contains(&arg.get_id().as_str()))
        .cloned()
        .collect::<Vec<_>>();
    scope_lab_cli_arguments_at_path(command, &[], &lab_args, composed_support, false)
}

/// Cook has a large control-plane surface, but the routine tracked-task path
/// only needs a small set of flags. Clap renders `HelpShort` for `--help` and
/// `HelpLong` for `--help-full`; keep the latter as the complete reference.
fn scope_cook_help(command: Command) -> Command {
    const COMMON_COOK_ARGUMENTS: &[&str] = &[
        "help",
        "help_full",
        "preview",
        "prompt",
        "goal",
        "repo",
        "task_url",
        "model",
        "to_worktree",
        "cwd",
        "verify",
        "verify_file",
        "base",
        "head",
        "no_finalize",
        "draft_pr",
        "max_attempts",
        "placement",
    ];

    fn visit(command: Command, path: &[String]) -> Command {
        let is_cook = path.iter().map(String::as_str).eq(["agent-task", "cook"]);
        let command = if is_cook {
            let ids = command
                .get_arguments()
                .map(|arg| arg.get_id().to_string())
                .collect::<Vec<_>>();
            ids.into_iter().fold(command, |command, id| {
                if COMMON_COOK_ARGUMENTS.contains(&id.as_str()) {
                    command
                } else {
                    command.mut_arg(id, |arg| arg.hide_short_help(true))
                }
            })
        } else {
            command
        };
        command.mut_subcommands(|subcommand| {
            let mut child_path = path.to_vec();
            child_path.push(subcommand.get_name().to_string());
            visit(subcommand, &child_path)
        })
    }

    visit(command, &[])
}

fn scope_lab_cli_arguments_at_path(
    command: Command,
    path: &[String],
    lab_args: &[clap::Arg],
    composed_support: &[LabCommandRouteSupport],
    include_builtin: bool,
) -> Command {
    // Only re-expose the Lab placement/runner flags on commands that actually
    // support Lab offload. A previous refactor collapsed this to
    // `!path.is_empty()`, which advertised the flags on every subcommand
    // (including non-portable ones like `contract manifest`).
    let visible = lab_cli_arguments_are_visible_for_path(path, composed_support, include_builtin);
    let command = if visible {
        lab_args.iter().fold(command, |command, arg| {
            let already_declared = command
                .get_arguments()
                .any(|existing| existing.get_id() == arg.get_id() && !existing.is_global_set());
            if already_declared {
                command
            } else {
                // Placement is declared on `Cli`, so retain its global
                // propagation when exposing it at a supported subcommand.
                // Otherwise a value after `cook` parses but the root request
                // deserializes its default `auto` value.
                command.arg(arg.clone().hide(false))
            }
        })
    } else if path.iter().map(String::as_str).eq(["cleanup"]) {
        // Cleanup is controller-owned, not Lab-portable. It still honors the
        // shared explicit-local execution primitive for bounded synchronous apply.
        command.arg(
            lab_args
                .iter()
                .find(|arg| arg.get_id() == "placement")
                .expect("global placement argument")
                .clone()
                .global(false)
                .hide(false),
        )
    } else {
        command
    };
    let has_placement = command
        .get_arguments()
        .any(|arg| arg.get_id() == "placement");
    let has_runner = command.get_arguments().any(|arg| arg.get_id() == "runner");
    let command = match (has_placement, has_runner) {
        (true, false) => command.mut_arg("placement", |arg| {
            arg.conflicts_with(clap::builder::Resettable::<clap::Id>::Reset)
        }),
        (false, true) => command.mut_arg("runner", |arg| {
            arg.conflicts_with(clap::builder::Resettable::<clap::Id>::Reset)
        }),
        _ => command,
    };
    // Cook coordinates durable promotion and finalization, so it cannot hand
    // its controller lifecycle to the generic queue. Keep parsing the inherited
    // dispatch flag for a precise validation error, but do not advertise it.
    let command = if matches!(
        path.iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            .as_slice(),
        ["agent-task", "cook"]
    ) {
        command.mut_arg("queue_only", |arg| arg.hide(true))
    } else {
        command
    };
    command.mut_subcommands(|subcommand| {
        let mut subcommand_path = path.to_vec();
        subcommand_path.push(subcommand.get_name().to_string());
        scope_lab_cli_arguments_at_path(
            subcommand,
            &subcommand_path,
            lab_args,
            composed_support,
            include_builtin,
        )
    })
}

/// Command paths that expose the Lab execution placement/runner flags in their
/// `--help`, mirroring the Lab-portable command surface. Kept explicit so the
/// help surface is deterministic and reviewable.
///
/// Declared as DATA rather than a `matches!` arm chain (#11141). This table is
/// a second source of truth for "is this path Lab-portable", parallel to each
/// command's `lab_contract()`, and while it was control flow nothing could
/// enumerate it — so nothing could assert the two agreed, and nothing could
/// catch a path spelling that clap does not expose (the #10313 failure mode,
/// which here would silently hide the placement flags forever). The tests
/// below now assert both.
///
/// The coarse dimension — which top-level commands are Lab-supported at all —
/// is NOT restated here; `lab_cli_arguments_are_visible_for_path` reads it from
/// `COMMAND_SPECS`, so this table only refines a registry fact instead of
/// re-declaring one.
const LAB_VISIBLE_COMMAND_PATHS: &[&[&str]] = &[
    &["agent-task", "cook"],
    &["agent-task", "run-plan"],
    &["agent-task", "run"],
    &["agent-task", "run-next"],
    &["agent-task", "status"],
    &["agent-task", "list"],
    &["agent-task", "active"],
    &["agent-task", "latest"],
    &["agent-task", "logs"],
    &["agent-task", "artifacts"],
    &["agent-task", "evidence"],
    &["agent-task", "review"],
    &["agent-task", "retry"],
    &["agent-task", "promote"],
    &["agent-task", "providers"],
    &["agent-task", "fanout", "submit-batch"],
    &["agent-task", "fanout", "status"],
    &["agent-task", "fanout", "artifacts"],
    // The fanout coordinator is controller-owned, but split placement hands
    // each child attempt to the selected runner, so the placement arguments are
    // load-bearing here and must stay discoverable.
    &["agent-task", "fanout", "cook-batch"],
    &["agent-task", "fanout", "run-plan"],
    &["agent-task", "auth", "status"],
    &["agent-task", "controller", "from-spec"],
    &["agent-task", "controller", "run-from-spec"],
    &["agent-task", "controller", "materialize"],
    &["agent-task", "controller", "resume"],
    &["bench"],
    &["bench", "matrix"],
    &["fuzz"],
    &["fuzz", "run"],
    &["fuzz", "run-campaign"],
    &["fuzz", "list"],
    &["fuzz", "plan"],
    &["fuzz", "doctor"],
    &["review"],
    &["review", "audit"],
    &["review", "lint"],
    &["review", "test"],
    &["trace"],
    &["refactor"],
    &["rig", "check"],
    &["rig", "run"],
    &["runtime", "refresh"],
    &["worktree", "cleanup"],
    &["extension", "update"],
    &["extension", "refresh"],
    &["extension", "dev-run"],
    &["extension", "show"],
    &["tunnel", "preview-consumer", "run"],
    &["tunnel", "service", "expose"],
    &["tunnel", "service", "start"],
];

fn lab_cli_arguments_are_visible_for_path(
    path: &[String],
    composed_support: &[LabCommandRouteSupport],
    include_builtin: bool,
) -> bool {
    let path = path.iter().map(String::as_str).collect::<Vec<_>>();

    // `CommandSpec.lab_supported` already answers "does this command family
    // route to Lab at all". Gate on it so a path table entry can only ever
    // NARROW a registry fact, never invent one: adding a Lab-visible path under
    // a command the registry does not declare Lab-supported now shows nothing
    // and fails the agreement test below, instead of quietly advertising
    // placement flags on a command that cannot honour them.
    if composed_support.iter().any(|support| {
        support
            .visible_paths
            .iter()
            .any(|candidate| candidate == &path.as_slice())
    }) {
        return true;
    }
    if !include_builtin
        || !path
            .first()
            .copied()
            .is_some_and(top_level_command_is_lab_supported)
    {
        return false;
    }

    LAB_VISIBLE_COMMAND_PATHS.contains(&path.as_slice())
}

fn top_level_command_is_lab_supported(name: &str) -> bool {
    crate::command_contract::registered_command(name).is_some_and(|spec| spec.lab_supported)
}

mod support;

#[cfg(test)]
mod tests;

// The lab contract types (workload / handoff / typed identifiers) and the
// lab-runnable command labels now live in the homeboy-lab-contract crate.
// Re-exported here so existing `command_contract::lab::*` (and the top-level
// `command_contract::*`) call sites are unchanged.
pub use homeboy_lab_contract::lab::handoff::*;
pub use homeboy_lab_contract::lab::types::*;
pub use homeboy_lab_contract::lab::workload::*;
pub use support::*;
