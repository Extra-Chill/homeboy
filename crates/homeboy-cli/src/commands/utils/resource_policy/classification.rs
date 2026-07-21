//! Command-classification predicates that decide whether a command is subject
//! to resource-policy admission at all.
//!
//! Extracted from the resource-policy module root to keep it under the god-file
//! line threshold (#9279). These are pure matches over the parsed command tree;
//! `hot_command` composes them to short-circuit commands that are controller-
//! local coordination, planning-only, read-only, or lightweight registry
//! management, and to recognize the Lab-offloadable fanout coordinator.

use crate::cli_surface::Commands;
use crate::commands::agent_task;

/// The `cook-batch` fanout coordinator is controller-owned in every mode: it
/// compiles the plan (default), previews it (`--dry-run`), or runs the batch
/// coordinator locally (`--run-plan`). In none of these modes may the
/// coordinator command itself be offloaded to Lab as a single job — the
/// coordinator owns worktree creation, the durable batch record, and child
/// dispatch. Only the child cooks it generates are Lab-eligible.
///
/// Previously this guard only matched `run_plan: true`, so a default (neither
/// `--dry-run` nor `--run-plan`) coordinator invocation fell through to being
/// treated as a portable, offloadable hot command. That allowed the whole
/// coordinator to be dispatched to Lab, where it timed out before creating its
/// local batch record/worktrees and stranded the run (#8025).
pub(super) fn is_controller_owned_fanout_coordination(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                command: agent_task::AgentTaskFanoutCommand::CookBatch(_),
            }),
        })
    )
}

/// The `fanout run-plan` coordinator executes a materialized batch-cook plan:
/// it owns the durable batch record, worktrees, promotion, gates, and
/// finalization, but dispatches each independent child provider attempt to a
/// selected Lab runner (`run_split_placement_fanout`). Unlike `cook-batch`
/// (which is unconditionally controller-local planning/coordination and is
/// filtered out earlier so it never refuses), `run-plan` previously fell
/// through to the generic local-only contract and refused on a warm/hot
/// controller — stranding a validated batch that a ready Lab runner could
/// serve (#9375). Recognize it here so its provider attempts can be admitted
/// under warm-runner coordination.
pub(super) fn is_lab_offloadable_fanout_coordinator(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                command: agent_task::AgentTaskFanoutCommand::RunPlan(_),
            }),
        })
    )
}

pub(super) fn is_plan_only_command(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                command: agent_task::AgentTaskFanoutCommand::CookBatch(
                    agent_task::AgentTaskFanoutCookBatchArgs { dry_run: true, .. },
                ),
            }),
        })
    )
}

pub(super) fn is_read_only_agent_task(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Status(_)
                | agent_task::AgentTaskCommand::Logs(_)
                | agent_task::AgentTaskCommand::Artifacts(_)
                | agent_task::AgentTaskCommand::Review(_),
        })
    )
}

/// Local registry/source-state management (`rig install|update|sync|sources`)
/// is lightweight controller-local bookkeeping, not a resource-intensive
/// workload. These commands carry a `LocalOnly` Lab contract only to *explain*
/// their controller-local boundary when an operator requests unsupported Lab
/// placement. `hot_command` otherwise converts every command with a Lab
/// contract — including every explanatory `LocalOnly` one — into a
/// `HotCommand`, which put rig source management behind warm/hot resource-policy
/// refusal and forced callers to bypass setup with `--skip-install --skip-sync`
/// (#9428). Resource policy must gate only genuinely resource-intensive
/// commands (e.g. `rig up`, `rig check`), so exempt these here while their
/// portability diagnostics stay intact.
pub(super) fn is_local_registry_management(command: &Commands) -> bool {
    matches!(command, Commands::Rig(args) if args.is_runner_source_management_command())
}
