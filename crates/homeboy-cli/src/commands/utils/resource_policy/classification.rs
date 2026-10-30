//! Command-classification predicates that decide whether a command is subject
//! to resource-policy admission at all.
//!
//! Extracted from the resource-policy module root to keep it under the god-file
//! line threshold (#9279). These are pure matches over the parsed command tree;
//! `hot_command` composes them to short-circuit commands that are controller-
//! local coordination, planning-only, read-only, or lightweight registry
//! management, and to recognize the Lab-offloadable fanout coordinator.
//!
//! ## Why this is not a `CommandPathSafetySpec` lookup
//!
//! #11141 proposed folding this table into the command registry as a
//! per-resolved-path `resource_behavior`, alongside the Lab help-visibility
//! table (converted) and `command_capability` (see that module's note).
//!
//! It does not fit. `CommandPathSafetySpec` keys on a command PATH; six rows
//! here key on parsed ARGUMENT VALUES at a single path, and flip classification
//! across the admission boundary when they do:
//!
//! - `cook --queue-only` — `LocalControl`, otherwise `AdmittedWorkload`
//! - `retry --run` — `AdmittedWorkload`, otherwise `LocalControl`
//! - `active --reconcile` — `AdmittedWorkload`, otherwise `BoundedMetadataRead`
//! - `reconcile --apply` — `AdmittedWorkload`, otherwise `BoundedMetadataRead`
//! - `recover --preflight` — `BoundedMetadataRead`, otherwise `AdmittedWorkload`
//! - `fanout cook-batch --dry-run` — `is_plan_only_command` below
//!
//! `path_safety` resolves one entry per path, so each of these pairs would
//! collapse to a single behavior. Choosing either side is a real incident:
//! collapsing to `AdmittedWorkload` puts controller-local bookkeeping behind
//! warm/hot refusal (the #9428 failure), and collapsing to `LocalControl`
//! refuses a validated batch a ready runner could serve (the #9375 failure).
//!
//! The Lab help-visibility table WAS convertible because help visibility is
//! genuinely per-path. This one is per-invocation. Folding it into the registry
//! needs the registry to gain an argument-predicate axis first.

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
        agent_task::AgentTaskCommand::ExecutionStep { .. }
        | agent_task::AgentTaskCommand::Doctor(_)
        | agent_task::AgentTaskCommand::RuntimeValidate(_)
        | agent_task::AgentTaskCommand::Cancel(_)
        | agent_task::AgentTaskCommand::PlacementUpdate(_)
        | agent_task::AgentTaskCommand::Quarantine(_)
        | agent_task::AgentTaskCommand::Rearm(_)
        | agent_task::AgentTaskCommand::Prompts(_)
        | agent_task::AgentTaskCommand::Contract(_)
        | agent_task::AgentTaskCommand::Auth(_)
        | agent_task::AgentTaskCommand::Accept(_) => AgentTaskResourceBehavior::LocalControl,
        agent_task::AgentTaskCommand::Cook(cook) if cook.dispatch.core.queue_only => {
            AgentTaskResourceBehavior::LocalControl
        }
        agent_task::AgentTaskCommand::CookFeedback(args) if args.status => {
            AgentTaskResourceBehavior::BoundedMetadataRead
        }
        agent_task::AgentTaskCommand::CookFeedback(_) => AgentTaskResourceBehavior::LocalControl,
        agent_task::AgentTaskCommand::Cook(_)
        | agent_task::AgentTaskCommand::RunPlan(_)
        | agent_task::AgentTaskCommand::Run(_)
        | agent_task::AgentTaskCommand::ReplayProviderBoundary(_)
        | agent_task::AgentTaskCommand::Resume(_)
        | agent_task::AgentTaskCommand::Promote(_)
        | agent_task::AgentTaskCommand::Adopt(_)
        | agent_task::AgentTaskCommand::PromotionProvider(_)
        | agent_task::AgentTaskCommand::GateExecute { .. }
        | agent_task::AgentTaskCommand::TerminalContinuationWorker { .. }
        | agent_task::AgentTaskCommand::FinalizePr(_)
        | agent_task::AgentTaskCommand::Tool(_) => AgentTaskResourceBehavior::AdmittedWorkload,
        agent_task::AgentTaskCommand::Retry(retry) if retry.run => {
            AgentTaskResourceBehavior::AdmittedWorkload
        }
        agent_task::AgentTaskCommand::Retry(_) => AgentTaskResourceBehavior::LocalControl,
        // `recover --preflight` only reads durable Cook state to name its step
        // and validate continuation or publication admission read-only. Every
        // step it performs (continuation, `retry --run`, `reconcile --apply`,
        // replacement gates, publication) is an admitted workload, like the
        // handlers it calls.
        agent_task::AgentTaskCommand::Recover(recover) if recover.preflight => {
            AgentTaskResourceBehavior::BoundedMetadataRead
        }
        agent_task::AgentTaskCommand::Recover(_) => AgentTaskResourceBehavior::AdmittedWorkload,
        agent_task::AgentTaskCommand::Status(_)
        | agent_task::AgentTaskCommand::ValidatePlan(_)
        | agent_task::AgentTaskCommand::List(_)
        | agent_task::AgentTaskCommand::ActiveScope(_)
        | agent_task::AgentTaskCommand::Logs(_)
        | agent_task::AgentTaskCommand::Artifacts(_)
        | agent_task::AgentTaskCommand::Evidence(_)
        | agent_task::AgentTaskCommand::Diagnose(_)
        | agent_task::AgentTaskCommand::GateFeedback(_)
        // Provider discovery reads controller-local extension and runtime
        // manifests under bounded probes. Classifying it as an admitted
        // workload put it behind resource admission, which captured a
        // warm-controller resource context and let Lab routing promote an
        // unscoped diagnostic read onto a runner — silently changing the scope
        // the answer describes (#9763).
        | agent_task::AgentTaskCommand::Providers(_)
        // Capacity is the same class of controller-local provider read:
        // bounded readiness-mode probes over the configured rotation, never
        // provider inference (#15024).
        | agent_task::AgentTaskCommand::Capacity(_)
        | agent_task::AgentTaskCommand::Review(_) => AgentTaskResourceBehavior::BoundedMetadataRead,
        agent_task::AgentTaskCommand::RetainedArtifacts(retained) => match &retained.command {
            // Discovery reads only the local immutable workspace receipt.
            agent_task::RetainedArtifactsCommand::Discover { .. } => {
                AgentTaskResourceBehavior::BoundedMetadataRead
            }
            // Attach may transfer a selected runner-side artifact and persists it.
            agent_task::RetainedArtifactsCommand::Attach { .. } => {
                AgentTaskResourceBehavior::AdmittedWorkload
            }
        },
        agent_task::AgentTaskCommand::Active(active) if !active.reconcile => {
            AgentTaskResourceBehavior::BoundedMetadataRead
        }
        agent_task::AgentTaskCommand::Reconcile(reconcile) if !reconcile.apply => {
            AgentTaskResourceBehavior::BoundedMetadataRead
        }
        agent_task::AgentTaskCommand::Active(_) | agent_task::AgentTaskCommand::Reconcile(_) => {
            AgentTaskResourceBehavior::AdmittedWorkload
        }
        // Cook-batch admits a workflow; batch-prepare plans it. Their
        // children are ordinary Cooks with their own admission.
        agent_task::AgentTaskCommand::Fanout(_) | agent_task::AgentTaskCommand::BatchPrepare(_) => {
            AgentTaskResourceBehavior::LocalControl
        }
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
/// A cook-batch and its `batch-prepare` step are controller-local planning:
/// the batch only admits a workflow, and its children are ordinary Cooks with
/// their own admission.
pub(super) fn is_controller_owned_fanout_coordination(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Fanout(_)
                | agent_task::AgentTaskCommand::BatchPrepare(_),
        })
    )
}

/// Executable retries use the generic Lab route, which first materializes the
/// persisted controller plan and then hands that replay to the selected runner.
pub(super) fn is_executable_agent_task_retry(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Retry(retry),
        }) if retry.run
    )
}

pub(super) fn is_plan_only_command(command: &Commands) -> bool {
    matches!(
        command,
        Commands::AgentTask(agent_task::AgentTaskArgs {
            command: agent_task::AgentTaskCommand::Fanout(agent_task::AgentTaskFanoutArgs {
                command: agent_task::AgentTaskFanoutCommand::CookBatch(args),
            }),
        }) if args.preview
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
