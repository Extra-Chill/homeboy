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

/// Resource behavior is independent from a command's Lab portability contract.
/// Bounded local record reads remain available for runner recovery; provider
/// execution, gates, and artifact hydration retain their existing admission.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum AgentTaskResourceBehavior {
    BoundedMetadataRead,
    AdmittedWorkload,
    LocalControl,
}

/// Classify every `agent-task` subcommand before consulting its portability
/// contract. `None` means this is not an agent-task command.
pub(super) fn agent_task_resource_behavior(
    command: &Commands,
) -> Option<AgentTaskResourceBehavior> {
    let Commands::AgentTask(args) = command else {
        return None;
    };

    Some(match &args.command {
        agent_task::AgentTaskCommand::Doctor(_)
        | agent_task::AgentTaskCommand::Submit(_)
        | agent_task::AgentTaskCommand::RuntimeRecover(_)
        | agent_task::AgentTaskCommand::RuntimeValidate(_)
        | agent_task::AgentTaskCommand::Cancel(_)
        | agent_task::AgentTaskCommand::Prompts(_)
        | agent_task::AgentTaskCommand::Contract(_)
        | agent_task::AgentTaskCommand::CompileLoop(_)
        | agent_task::AgentTaskCommand::Auth(_) => AgentTaskResourceBehavior::LocalControl,
        agent_task::AgentTaskCommand::Cook(cook) if cook.dispatch.core.queue_only => {
            AgentTaskResourceBehavior::LocalControl
        }
        agent_task::AgentTaskCommand::Cook(_)
        | agent_task::AgentTaskCommand::CookContinue(_)
        | agent_task::AgentTaskCommand::RunPlan(_)
        | agent_task::AgentTaskCommand::Run(_)
        | agent_task::AgentTaskCommand::RunNext
        | agent_task::AgentTaskCommand::Evidence(_)
        | agent_task::AgentTaskCommand::Diagnose(_)
        | agent_task::AgentTaskCommand::ReplayProviderBoundary(_)
        | agent_task::AgentTaskCommand::Resume(_)
        | agent_task::AgentTaskCommand::Review(_)
        | agent_task::AgentTaskCommand::Promote(_)
        | agent_task::AgentTaskCommand::Adopt(_)
        | agent_task::AgentTaskCommand::PromotionProvider(_)
        | agent_task::AgentTaskCommand::FinalizePr(_)
        | agent_task::AgentTaskCommand::GateFeedback(_)
        | agent_task::AgentTaskCommand::Providers(_)
        | agent_task::AgentTaskCommand::Tool(_) => AgentTaskResourceBehavior::AdmittedWorkload,
        agent_task::AgentTaskCommand::Retry(retry) if retry.run => {
            AgentTaskResourceBehavior::AdmittedWorkload
        }
        agent_task::AgentTaskCommand::Retry(_) => AgentTaskResourceBehavior::LocalControl,
        agent_task::AgentTaskCommand::Status(_)
        | agent_task::AgentTaskCommand::List(_)
        | agent_task::AgentTaskCommand::Latest(_)
        | agent_task::AgentTaskCommand::Logs(_)
        | agent_task::AgentTaskCommand::Artifacts(_) => {
            AgentTaskResourceBehavior::BoundedMetadataRead
        }
        agent_task::AgentTaskCommand::Active(active) if !active.reconcile => {
            AgentTaskResourceBehavior::BoundedMetadataRead
        }
        agent_task::AgentTaskCommand::Active(_)
        | agent_task::AgentTaskCommand::Reconcile(_)
        | agent_task::AgentTaskCommand::ReconcileRecords(_) => {
            AgentTaskResourceBehavior::AdmittedWorkload
        }
        agent_task::AgentTaskCommand::Fanout(fanout) => match &fanout.command {
            agent_task::AgentTaskFanoutCommand::Status(_)
            | agent_task::AgentTaskFanoutCommand::Artifacts(_) => {
                AgentTaskResourceBehavior::BoundedMetadataRead
            }
            agent_task::AgentTaskFanoutCommand::Resume(_)
            | agent_task::AgentTaskFanoutCommand::RunPlan(_) => {
                AgentTaskResourceBehavior::AdmittedWorkload
            }
            agent_task::AgentTaskFanoutCommand::CookBatch(_)
            | agent_task::AgentTaskFanoutCommand::Plan(_)
            | agent_task::AgentTaskFanoutCommand::Submit(_)
            | agent_task::AgentTaskFanoutCommand::SubmitBatch(_) => {
                AgentTaskResourceBehavior::LocalControl
            }
        },
        agent_task::AgentTaskCommand::Loop(loop_args) => match &loop_args.command {
            agent_task::AgentTaskLoopCommand::Status(_) => {
                AgentTaskResourceBehavior::BoundedMetadataRead
            }
            agent_task::AgentTaskLoopCommand::Define(args) if args.resume => {
                AgentTaskResourceBehavior::AdmittedWorkload
            }
            agent_task::AgentTaskLoopCommand::Resume(_) => {
                AgentTaskResourceBehavior::AdmittedWorkload
            }
            agent_task::AgentTaskLoopCommand::Define(_)
            | agent_task::AgentTaskLoopCommand::Stop(_) => AgentTaskResourceBehavior::LocalControl,
        },
        agent_task::AgentTaskCommand::Controller(controller) => match &controller.command {
            agent_task::AgentTaskControllerCommand::Status(_)
            | agent_task::AgentTaskControllerCommand::Diagnose(_)
            | agent_task::AgentTaskControllerCommand::List => {
                AgentTaskResourceBehavior::BoundedMetadataRead
            }
            agent_task::AgentTaskControllerCommand::FromSpec(args) if args.resume => {
                AgentTaskResourceBehavior::AdmittedWorkload
            }
            agent_task::AgentTaskControllerCommand::RunFromSpec(_)
            | agent_task::AgentTaskControllerCommand::Materialize(_)
            | agent_task::AgentTaskControllerCommand::RunNext(_)
            | agent_task::AgentTaskControllerCommand::Run(_)
            | agent_task::AgentTaskControllerCommand::Resume(_)
            | agent_task::AgentTaskControllerCommand::Proof(_) => {
                AgentTaskResourceBehavior::AdmittedWorkload
            }
            agent_task::AgentTaskControllerCommand::Init(_)
            | agent_task::AgentTaskControllerCommand::FromSpec(_)
            | agent_task::AgentTaskControllerCommand::ValidateProof(_)
            | agent_task::AgentTaskControllerCommand::Plan(_)
            | agent_task::AgentTaskControllerCommand::Events(_)
            | agent_task::AgentTaskControllerCommand::ApplyEvent(_)
            | agent_task::AgentTaskControllerCommand::MarkHumanReady(_) => {
                AgentTaskResourceBehavior::LocalControl
            }
        },
    })
}

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

pub(super) fn is_bounded_agent_task_metadata_read(command: &Commands) -> bool {
    agent_task_resource_behavior(command) == Some(AgentTaskResourceBehavior::BoundedMetadataRead)
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
