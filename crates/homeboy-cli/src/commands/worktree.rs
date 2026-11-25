use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::cleanup::{
    self as artifact_cleanup, ArtifactCleanupOptions, ArtifactCleanupOutput, ArtifactCleanupScope,
    ArtifactCleanupSort,
};
use homeboy::core::worktree::{
    self, CleanupPolicy, TaskWorktreeRecord, TaskWorktreeRegistryQuarantine, WorktreeAdoptOptions,
    WorktreeAdoptOutput, WorktreeCleanupOptions, WorktreeCleanupOutput, WorktreeCreateOptions,
    WorktreeCreateOutput, WorktreeImportOptions, WorktreeImportOutput, WorktreeInventoryOptions,
    WorktreeInventoryOutput, WorktreeListOptions, WorktreeListOutput, WorktreeOwnershipProbe,
    WorktreeQueueCreateOptions, WorktreeQueueCreateOutput, WorktreeReclaimOutput,
    WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeStatusOutput,
};

use super::utils::args::MutationArgs;
use super::utils::response::{CommandActionableMetadata, CommandNextAction, CommandNextActionKind};
use super::CmdResult;
use crate::command_contract::{LabCommandContract, WORKTREE_CLEANUP_LAB_LABEL};

#[derive(Args)]
pub struct WorktreeArgs {
    #[command(subcommand)]
    command: WorktreeCommand,
}

impl WorktreeArgs {
    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        match self.command {
            WorktreeCommand::Cleanup { .. } => Some(LabCommandContract::runner_resident(
                WORKTREE_CLEANUP_LAB_LABEL,
            )),
            _ => None,
        }
    }
}

#[derive(Subcommand)]
enum WorktreeCommand {
    /// Create a native task worktree
    Create {
        /// Component or repository handle for native creation
        component_id: String,
        /// Branch to create in the task worktree
        #[arg(long)]
        branch: String,
        /// Base ref for the new worktree branch
        #[arg(long = "from")]
        from: Option<String>,
        /// Task or issue URL associated with this worktree
        #[arg(long)]
        task_url: Option<String>,
        /// Agent-task run ID associated with this worktree
        #[arg(long)]
        run_id: Option<String>,
        /// Cleanup policy for lifecycle cleanup
        #[arg(long, value_enum)]
        cleanup_policy: Option<CliCleanupPolicy>,
        /// Require bounded remote freshness proof before allocating the worktree
        #[arg(long)]
        require_handoff_freshness: bool,
    },
    /// Import an existing exact Git worktree into the built-in lifecycle registry
    Import {
        component_id: String,
        handle: String,
        path: String,
        #[arg(long)]
        branch: String,
        #[arg(long)]
        base_ref: String,
        #[arg(long)]
        task_url: Option<String>,
        #[arg(long)]
        owner_run_ref: Option<String>,
        #[arg(long, value_enum)]
        cleanup_policy: CliCleanupPolicy,
        #[arg(long)]
        created_at: Option<String>,
    },
    /// Record a terminal worktree disposition without performing cleanup
    Finalize {
        handle: String,
        #[arg(long)]
        owner_run_ref: String,
        #[arg(long, value_enum)]
        disposition: CliTerminalDisposition,
    },
    /// Adopt an existing local workspace path for @workspace:<handle> refs
    Adopt {
        /// Workspace handle resolved by @workspace:<handle>
        handle: String,
        /// Existing local directory to resolve for this handle
        path: String,
        /// Optional generic kind label recorded as provenance
        #[arg(long)]
        kind: Option<String>,
        /// Optional JSON provenance payload recorded with the adopted path
        #[arg(long)]
        provenance_json: Option<String>,
    },
    /// Create multiple task worktrees one-at-a-time with queue status JSON
    QueueCreate {
        /// Registered component/repo handle, e.g. homeboy
        repo: String,
        /// Branch to create. Repeat for fanout batches.
        #[arg(long = "branch", value_name = "BRANCH", required = true)]
        branches: Vec<String>,
        /// Base ref for each worktree branch
        #[arg(long = "from", default_value = "origin/main")]
        from: String,
        /// Task or issue URL associated with these worktrees
        #[arg(long)]
        task_url: Option<String>,
        /// Short task reference associated with these worktrees, e.g. Extra-Chill/homeboy#5786
        #[arg(long)]
        task_ref: Option<String>,
        /// Print the queue plan/status without creating worktrees
        #[arg(long)]
        dry_run: bool,
        /// Suggested orchestrator wait when queueing is blocked but no retry-after value is available
        #[arg(long, default_value_t = 60)]
        retry_after_seconds: u64,
    },
    /// List native task worktrees in a bounded keyset page
    List {
        /// Maximum manifests to inspect
        #[arg(long, default_value_t = 100)]
        limit: usize,
        /// Opaque continuation returned by the preceding page
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
    },
    /// Report bounded local task-worktree inventory and reconcile only leased terminal snapshots
    Inventory {
        /// Maximum task-worktree manifests to inspect
        #[arg(long, default_value_t = 500)]
        limit: usize,
        /// Start after this task-worktree record ID
        #[arg(long)]
        cursor: Option<String>,
        /// Start after this adopted-workspace handle
        #[arg(long)]
        adopted_cursor: Option<String>,
        /// Conditionally reconcile clean, missing worktrees with terminal authority; preserve or refuse all other records
        #[arg(long)]
        apply: bool,
    },
    /// Inspect a recorded or unrecorded linked worktree and its safety state
    Status {
        /// Task worktree ID or linked worktree root path
        id: String,
    },
    /// Report the session currently holding a managed checkout's write lease
    Holder {
        /// Managed worktree handle or any path inside the checkout
        target: String,
    },
    /// Remove a recorded or unrecorded linked worktree after safety checks
    Remove {
        /// Task worktree ID or linked worktree root path
        id: String,
        /// Allow recorded dirty/unpushed worktree removal; never bypass orphan or hard gates
        #[arg(long)]
        force: bool,
        /// Delete the local task branch after removing the worktree when branch safety allows it.
        #[arg(long)]
        cleanup_branch: bool,
        /// Permit deleting an unmerged task branch. Requires --cleanup-branch.
        #[arg(long, requires = "cleanup_branch")]
        allow_unmerged_branch: bool,
    },
    /// Clean up eligible recorded and unrecorded linked worktrees
    Cleanup {
        // Remove planned worktrees and artifacts after safety checks.
        // Without --apply, only reports the plan; --dry-run names that
        // default explicitly, as on every shared mutation group (#11139).
        #[command(flatten)]
        mutation: MutationArgs,
        /// Allow dirty/unpushed worktree removal; hard gates still apply
        #[arg(long)]
        force: bool,
        /// Also remove declared rebuildable artifacts from the Homeboy checkout that built this binary.
        #[arg(long)]
        cleanup_artifacts: bool,
        /// Delete merged task branches for removed cleanup candidates.
        #[arg(long)]
        cleanup_branches: bool,
        /// Permit deleting unmerged task branches. Requires --cleanup-branches.
        #[arg(long, requires = "cleanup_branches")]
        allow_unmerged_branches: bool,
    },
    /// Reclaim disk space held by completed task-worktree loops
    Reclaim {
        // Default output is a non-mutating reclaim plan naming claimed
        // workspaces, released stale leases, and reclaimable bytes;
        // --apply executes the plan. Same precedence as `worktree cleanup`.
        #[command(flatten)]
        mutation: MutationArgs,
    },
    /// Inspect or explicitly reconcile quarantined malformed task-worktree records
    Quarantine {
        #[command(subcommand)]
        command: WorktreeQuarantineCommand,
    },
}

#[derive(Subcommand)]
enum WorktreeQuarantineCommand {
    /// List quarantined records still protecting Cargo targets
    List,
    /// Mark one quarantined record terminally reconciled while retaining its original evidence
    Clear {
        /// Provenance sidecar reported by cleanup or `worktree quarantine list`
        provenance_path: PathBuf,
        /// Confirms terminal state was independently verified before clearing protection
        #[arg(long)]
        verified_terminal: bool,
    },
}

#[derive(Debug, Clone, ValueEnum)]
enum CliCleanupPolicy {
    RemoveWhenSafe,
    PreserveOnFailure,
}

#[derive(Debug, Clone, ValueEnum)]
enum CliTerminalDisposition {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Interrupted,
}

impl From<CliTerminalDisposition>
    for homeboy::core::worktree_provider::WorktreeTerminalDisposition
{
    fn from(value: CliTerminalDisposition) -> Self {
        match value {
            CliTerminalDisposition::Succeeded => Self::Succeeded,
            CliTerminalDisposition::Failed => Self::Failed,
            CliTerminalDisposition::Cancelled => Self::Cancelled,
            CliTerminalDisposition::TimedOut => Self::TimedOut,
            CliTerminalDisposition::Interrupted => Self::Interrupted,
        }
    }
}

impl From<CliCleanupPolicy> for CleanupPolicy {
    fn from(value: CliCleanupPolicy) -> Self {
        match value {
            CliCleanupPolicy::RemoveWhenSafe => CleanupPolicy::RemoveWhenSafe,
            CliCleanupPolicy::PreserveOnFailure => CleanupPolicy::PreserveOnFailure,
        }
    }
}

#[derive(Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum WorktreeOutput {
    Create(WorktreeCreateOutput),
    Import(WorktreeImportOutput),
    Finalize(TaskWorktreeRecord),
    Adopt(WorktreeAdoptOutput),
    QueueCreate(WorktreeQueueCreateOutput),
    List(WorktreeListOutput),
    Inventory(WorktreeInventoryOutput),
    Status(WorktreeStatusOutput),
    Holder(WorktreeOwnershipProbe),
    Remove(WorktreeRemoveOutput),
    Cleanup(WorktreeCleanupCommandOutput),
    Reclaim(WorktreeReclaimOutput),
    QuarantineList {
        quarantines: Vec<TaskWorktreeRegistryQuarantine>,
    },
    QuarantineClear(TaskWorktreeRegistryQuarantine),
}

#[derive(Serialize)]
pub struct WorktreeCleanupCommandOutput {
    pub worktrees: WorktreeCleanupOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_cleanup: Option<ArtifactCleanupOutput>,
    #[serde(rename = "_homeboy_actionable")]
    pub actionable: CommandActionableMetadata,
}

/// Session/run identity recorded as reaping provenance by CLI reapers.
fn cli_reaper_identity() -> String {
    format!("cli-pid:{}", std::process::id())
}

/// Reconciliation authority for native task worktrees. Records whose owning
/// run never crossed a runner boundary reconcile under a controller-only claim;
/// every other record is preserved with its own typed reason.
#[derive(Default)]
pub(crate) struct AgentTaskAuthority(
    std::sync::Mutex<
        std::collections::HashMap<
            String,
            homeboy::agents::agent_task_lifecycle::CompositeWorkspaceClaim,
        >,
    >,
);
impl worktree::WorktreeReconciliationAuthority for AgentTaskAuthority {
    fn local_refusal(
        &self,
        record: &worktree::TaskWorktreeRecord,
    ) -> Option<worktree::WorktreeAuthorityRefusal> {
        // At mutation time this adapter holds the record's reconciliation
        // claim; its own acquisition intent is not a foreign claim.
        let held = record
            .effective_workspace_identity()
            .ok()
            .and_then(|workspace| {
                self.0.lock().ok().and_then(|claims| {
                    claims
                        .values()
                        .find(|claim| claim.workspace == workspace)
                        .cloned()
                })
            });
        match held {
            Some(claim) => {
                homeboy::agents::agent_task_lifecycle::local_terminal_workspace_authority_under_claim(
                    record, &claim,
                )
            }
            None => homeboy::agents::agent_task_lifecycle::local_terminal_workspace_authority(record),
        }
        .err()
    }

    fn acquire(
        &self,
        record: &worktree::TaskWorktreeRecord,
        deadline: std::time::Instant,
    ) -> homeboy::core::Result<worktree::WorktreeLivenessAuthority> {
        const CLAIM_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(20);
        let deadline = deadline.min(std::time::Instant::now() + CLAIM_TIMEOUT);
        if std::time::Instant::now() >= deadline {
            return Ok(worktree::WorktreeLivenessAuthority::Incomplete {
                reason: "worktree inventory apply authority deadline exhausted".to_string(),
            });
        }
        let proof =
            match homeboy::agents::agent_task_lifecycle::local_terminal_workspace_authority(record)
            {
                Ok(proof) => proof,
                Err(refusal) => {
                    return Ok(worktree::WorktreeLivenessAuthority::Incomplete {
                        reason: refusal.detail,
                    })
                }
            };
        let workspace = proof.workspace.clone();
        // Persist before acquiring the fence; the mutation re-checks it exactly.
        homeboy::core::worktree::persist_terminal_workspace_authority(
            &record.id,
            record.lifecycle_revision,
            *proof,
        )?;
        match homeboy::agents::agent_task_lifecycle::acquire_composite_workspace_claim_until(
            workspace,
            record.lifecycle_revision,
            homeboy::agents::agent_task_lifecycle::CompositeClaimAuthorities::ControllerOnly,
            deadline,
        ) {
            Ok(composite) => {
                let claim = composite.local.clone();
                self.0
                    .lock()
                    .map_err(|_| {
                        homeboy::core::Error::internal_unexpected(
                            "workspace claim adapter lock poisoned",
                        )
                    })?
                    .insert(claim.token.clone(), composite);
                Ok(worktree::WorktreeLivenessAuthority::Terminal {
                    claim,
                    provenance: "controller-local terminal run and workspace reconciliation claim"
                        .to_string(),
                })
            }
            Err(failure) => Ok(worktree::WorktreeLivenessAuthority::Incomplete {
                reason: format!(
                    "workspace reconciliation authority refused acquisition: {}{}",
                    failure.primary.message,
                    failure
                        .primary
                        .details
                        .get("workspace_claim_composite_cleanup")
                        .and_then(|status| status.get("status"))
                        .and_then(serde_json::Value::as_str)
                        .map(|status| format!("; {status}"))
                        .unwrap_or_default()
                ),
            }),
        }
    }

    fn validate(
        &self,
        _: &worktree::TaskWorktreeRecord,
        claim: &homeboy::core::workspace_claim::WorkspaceClaim,
        deadline: std::time::Instant,
    ) -> homeboy::core::Result<bool> {
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        let claims = self.0.lock().map_err(|_| {
            homeboy::core::Error::internal_unexpected("workspace claim adapter lock poisoned")
        })?;
        claims
            .get(&claim.token)
            .map(|claim| {
                homeboy::agents::agent_task_lifecycle::validate_composite_workspace_claim_until(
                    claim, deadline,
                )
            })
            .transpose()
            .map(|valid| valid.unwrap_or(false))
    }

    fn ready_to_commit(&self, claim: &homeboy::core::workspace_claim::WorkspaceClaim) -> bool {
        self.0
            .lock()
            .ok()
            .and_then(|claims| claims.get(&claim.token).cloned())
            .is_some_and(|composite| {
                composite.local == *claim
                    && homeboy::agents::agent_task_lifecycle::composite_workspace_claim_ready_to_commit(&composite)
            })
    }

    fn requires_terminal_workspace_authority_proof(&self) -> bool {
        true
    }

    fn release(
        &self,
        claim: &homeboy::core::workspace_claim::WorkspaceClaim,
        _deadline: std::time::Instant,
    ) -> homeboy::core::Result<()> {
        let mut claims = self.0.lock().map_err(|_| {
            homeboy::core::Error::internal_unexpected("workspace claim adapter lock poisoned")
        })?;
        let Some(mut composite) = claims.remove(&claim.token) else {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "workspace_claim",
                "workspace composite claim token is unavailable for release",
                Some(claim.token.clone()),
                None,
            ));
        };
        match homeboy::agents::agent_task_lifecycle::release_composite_workspace_claim_until(
            &mut composite,
            _deadline,
        )? {
            homeboy::agents::agent_task_lifecycle::CompositeWorkspaceClaimRelease::Released => {
                Ok(())
            }
            homeboy::agents::agent_task_lifecycle::CompositeWorkspaceClaimRelease::Partial {
                failures,
            } => {
                let error = homeboy::core::Error::validation_invalid_argument(
                    "workspace_claim_composite",
                    "workspace composite release partially failed",
                    None,
                    Some(failures),
                );
                let status = homeboy::agents::agent_task_lifecycle::persist_and_retry_composite_workspace_cleanup_until(composite, &error, _deadline);
                Err(homeboy::core::Error::validation_invalid_argument(
                    "workspace_claim_composite",
                    format!(
                        "workspace composite release incomplete: {}",
                        status.public_summary()
                    ),
                    None,
                    None,
                ))
            }
        }
    }
}

pub fn run(args: WorktreeArgs) -> CmdResult<WorktreeOutput> {
    let exit_code = 0;
    let output = match args.command {
        WorktreeCommand::Create {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy,
            require_handoff_freshness,
        } => WorktreeOutput::Create(worktree::create(WorktreeCreateOptions {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy: cleanup_policy.map(Into::into),
            require_handoff_freshness,
            source_path: None,
        })?),
        WorktreeCommand::Import {
            component_id,
            handle,
            path,
            branch,
            base_ref,
            task_url,
            owner_run_ref,
            cleanup_policy,
            created_at,
        } => WorktreeOutput::Import(worktree::import(WorktreeImportOptions {
            component_id,
            handle,
            path,
            branch,
            base_ref,
            task_url,
            owner_run_ref,
            cleanup_policy: cleanup_policy.into(),
            created_at,
        })?),
        WorktreeCommand::Finalize {
            handle,
            owner_run_ref,
            disposition,
        } => WorktreeOutput::Finalize(worktree::finalize_provider_lifecycle(
            &handle,
            &owner_run_ref,
            disposition.into(),
        )?),
        WorktreeCommand::Adopt {
            handle,
            path,
            kind,
            provenance_json,
        } => {
            let provenance = provenance_json
                .map(|value| serde_json::from_str(&value))
                .transpose()
                .map_err(|err| {
                    homeboy::core::Error::validation_invalid_json(
                        err,
                        Some("provenance_json".to_string()),
                        None,
                    )
                })?;
            WorktreeOutput::Adopt(worktree::adopt(WorktreeAdoptOptions {
                handle,
                path,
                kind,
                provenance,
            })?)
        }
        WorktreeCommand::QueueCreate {
            repo,
            branches,
            from,
            task_url,
            task_ref,
            dry_run,
            retry_after_seconds,
        } => WorktreeOutput::QueueCreate(worktree::queue_create(WorktreeQueueCreateOptions {
            repo,
            requests: branches
                .into_iter()
                .map(|branch| worktree::WorktreeQueueCreateRequest {
                    branch,
                    task_url: task_url.clone(),
                    task_ref: task_ref.clone(),
                    run_id: None,
                    provider_lifecycle: None,
                })
                .collect(),
            from,
            dry_run,
            retry_after_seconds,
        })?),
        WorktreeCommand::List { limit, cursor } => {
            WorktreeOutput::List(worktree::list_page(WorktreeListOptions { limit, cursor })?)
        }
        WorktreeCommand::Inventory {
            limit,
            cursor,
            adopted_cursor,
            apply,
        } => WorktreeOutput::Inventory(worktree::inventory(
            WorktreeInventoryOptions {
                limit,
                cursor,
                adopted_cursor,
                apply,
                apply_deadline: None,
            },
            &AgentTaskAuthority::default(),
        )?),
        WorktreeCommand::Status { id } => WorktreeOutput::Status(worktree::status(&id)?),
        WorktreeCommand::Holder { target } => {
            let path = PathBuf::from(&target);
            let path = if path.exists() {
                path
            } else {
                PathBuf::from(worktree::resolve(&target)?.worktree_path)
            };
            WorktreeOutput::Holder(worktree::ownership_probe(&path)?.ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "worktree",
                    "path is not inside a managed task worktree",
                    Some(target),
                    None,
                )
            })?)
        }
        WorktreeCommand::Remove {
            id,
            force,
            cleanup_branch,
            allow_unmerged_branch,
        } => WorktreeOutput::Remove(worktree::remove(WorktreeRemoveOptions {
            id,
            force,
            cleanup_branch,
            allow_unmerged_branch,
            reason: Some("worktree remove".to_string()),
            reaper: Some(cli_reaper_identity()),
        })?),
        WorktreeCommand::Cleanup {
            mutation,
            force,
            cleanup_artifacts,
            cleanup_branches,
            allow_unmerged_branches,
        } => {
            let apply = mutation.is_apply();
            let worktrees = worktree::cleanup(WorktreeCleanupOptions {
                force,
                dry_run: !apply,
                cleanup_branches,
                allow_unmerged_branches,
                reaper: Some(cli_reaper_identity()),
            })?;
            let artifact_cleanup = if cleanup_artifacts {
                Some(artifact_cleanup::cleanup_artifacts(
                    ArtifactCleanupOptions {
                        path: None,
                        scope: ArtifactCleanupScope::ExactCheckout,
                        apply,
                        self_artifacts: true,
                        temp_roots: Vec::new(),
                        sort: ArtifactCleanupSort::Discovery,
                        limit: None,
                        cursor: None,
                        merged_only: false,
                        min_age_days: None,
                        include_active_worktrees: false,
                        max_scan_duration: None,
                    },
                )?)
            } else {
                None
            };
            let actionable = worktree_cleanup_actionable(
                &worktrees,
                cleanup_branches,
                inventory_apply_reconciles_skipped(&worktrees),
            );
            WorktreeOutput::Cleanup(WorktreeCleanupCommandOutput {
                worktrees,
                artifact_cleanup,
                actionable,
            })
        }
        WorktreeCommand::Reclaim { mutation } => WorktreeOutput::Reclaim(worktree::reclaim(
            homeboy::core::worktree::WorktreeReclaimOptions {
                dry_run: !mutation.is_apply(),
                limit: 500,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
        )?),
        WorktreeCommand::Quarantine { command } => match command {
            WorktreeQuarantineCommand::List => WorktreeOutput::QuarantineList {
                quarantines: worktree::list_task_worktree_registry_quarantines()?,
            },
            WorktreeQuarantineCommand::Clear {
                provenance_path,
                verified_terminal,
            } => {
                WorktreeOutput::QuarantineClear(worktree::clear_task_worktree_registry_quarantine(
                    &provenance_path,
                    verified_terminal,
                )?)
            }
        },
    };
    Ok((output, exit_code))
}

pub(crate) const INVENTORY_APPLY_COMMAND: &str = "homeboy worktree inventory --apply";

/// Whether `worktree inventory --apply` would reconcile at least one record
/// cleanup skipped as a missing active worktree. Stops at the first one.
pub(crate) fn inventory_apply_reconciles_skipped(output: &WorktreeCleanupOutput) -> bool {
    let authority = AgentTaskAuthority::default();
    output
        .skipped
        .iter()
        .filter(|skipped| {
            skipped
                .safety
                .as_ref()
                .is_some_and(|safety| safety.worktree_missing)
        })
        .any(|skipped| worktree::inventory_apply_would_reconcile(&skipped.record, &authority))
}

fn worktree_cleanup_actionable(
    output: &WorktreeCleanupOutput,
    cleanup_branches: bool,
    inventory_apply_reconciles: bool,
) -> CommandActionableMetadata {
    let mut actionable = CommandActionableMetadata::default();
    if inventory_apply_reconciles {
        actionable.next_actions.push(
            CommandNextAction::new("reconcile task worktrees", INVENTORY_APPLY_COMMAND)
                .with_kind(CommandNextActionKind::Repair),
        );
    }
    if output.dry_run || output.counts.candidates > output.counts.removed {
        let mut command = "homeboy worktree cleanup".to_string();
        if cleanup_branches {
            command.push_str(" --cleanup-branches");
        }
        command.push_str(" --apply");
        actionable
            .next_actions
            .push(CommandNextAction::new("task worktree cleanup", command));
    }
    actionable
}

#[cfg(test)]
mod tests {
    use homeboy::core::worktree::{
        CleanupPolicy, TaskWorktreeRecord, TaskWorktreeState, WorktreeCleanupCounts,
        WorktreeCleanupOutput, WorktreeCreateAction, WorktreeCreateEvidence, WorktreeCreateOutput,
        WorktreeCreateReconciliation, WorktreeImportOutput,
    };

    use super::{worktree_cleanup_actionable, WorktreeOutput};

    fn record() -> TaskWorktreeRecord {
        TaskWorktreeRecord {
            id: "fixture@branch".to_string(),
            component_id: "fixture".to_string(),
            source_checkout: "/tmp/source".to_string(),
            worktree_path: "/tmp/fixture@branch".to_string(),
            branch: "branch".to_string(),
            base_ref: "HEAD".to_string(),
            workspace_identity: Some(
                homeboy::core::worktree::WorkspaceIdentity::new(
                    "task-worktree",
                    "fixture/fixture@branch",
                )
                .expect("workspace identity"),
            ),
            task_url: None,
            run_id: None,
            cleanup_policy: CleanupPolicy::RemoveWhenSafe,
            terminal_disposition: None,
            branch_cleanup_intent: Default::default(),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            state: TaskWorktreeState::Active,
            lifecycle_revision: 0,
            terminal_workspace_authority: None,
        }
    }

    fn create(reconciliation: Option<WorktreeCreateReconciliation>) -> WorktreeOutput {
        WorktreeOutput::Create(WorktreeCreateOutput {
            record: record(),
            reconciliation,
            handoff_freshness: None,
        })
    }

    /// JSON consumers read each payload's fields beside the outer `action`
    /// tag, and only see `reconciliation` when an existing worktree was restored.
    #[test]
    fn worktree_output_flattens_each_payload_beside_its_action_tag() {
        let evidence = WorktreeCreateEvidence {
            task_worktree_id: "fixture@branch".to_string(),
            component_id: "fixture".to_string(),
            source_checkout: "/tmp/source".to_string(),
            worktree_path: "/tmp/fixture@branch".to_string(),
            branch: "branch".to_string(),
            workspace_identity: record().workspace_identity.unwrap(),
            git_registration: "registered".to_string(),
        };
        let mut finalized = record();
        finalized.terminal_disposition = Some("succeeded".to_string());
        let cases = [
            (
                create(None),
                "create",
                "reconciliation",
                serde_json::Value::Null,
            ),
            (
                create(Some(WorktreeCreateReconciliation {
                    action: WorktreeCreateAction::Restored,
                    previous: evidence.clone(),
                    current: evidence,
                })),
                "create",
                "reconciliation",
                serde_json::json!("restored"),
            ),
            (
                WorktreeOutput::Import(WorktreeImportOutput {
                    record: record(),
                    imported: true,
                }),
                "import",
                "imported",
                serde_json::json!(true),
            ),
            (
                WorktreeOutput::Finalize(finalized),
                "finalize",
                "terminal_disposition",
                serde_json::json!("succeeded"),
            ),
            (
                WorktreeOutput::QuarantineList {
                    quarantines: Vec::new(),
                },
                "quarantine_list",
                "quarantines",
                serde_json::json!([]),
            ),
        ];
        for (output, action, field, expected) in cases {
            let value = serde_json::to_value(output).expect("serialize worktree output");
            assert_eq!(value["action"], action);
            let actual = match &value[field] {
                serde_json::Value::Object(reconciliation) => reconciliation["action"].clone(),
                other => other.clone(),
            };
            assert_eq!(actual, expected, "{action}.{field}");
        }
    }

    #[test]
    fn worktree_cleanup_next_actions_follow_plan_state() {
        let output = |dry_run, candidates, removed| WorktreeCleanupOutput {
            dry_run,
            counts: WorktreeCleanupCounts {
                candidates,
                removed,
                ..Default::default()
            },
            candidates: Vec::new(),
            removed: Vec::new(),
            skipped: Vec::new(),
            next_cursor: None,
            continuation: None,
        };
        let cases: [(WorktreeCleanupOutput, bool, bool, &[&str]); 5] = [
            // Bounded reconciliation is offered before cleanup.
            (
                output(true, 1, 0),
                true,
                true,
                &[
                    "homeboy worktree inventory --apply",
                    "homeboy worktree cleanup --cleanup-branches --apply",
                ],
            ),
            // Blockers that inventory cannot reconcile never point at it.
            (
                output(true, 1, 0),
                true,
                false,
                &["homeboy worktree cleanup --cleanup-branches --apply"],
            ),
            // The preview preserves the branch cleanup request.
            (
                output(true, 1, 0),
                false,
                false,
                &["homeboy worktree cleanup --apply"],
            ),
            // Apply that removed some candidates still offers the rest.
            (
                output(false, 2, 1),
                false,
                false,
                &["homeboy worktree cleanup --apply"],
            ),
            // Apply that removed every candidate has nothing left to suggest.
            (output(false, 1, 1), true, false, &[]),
        ];
        for (output, cleanup_branches, reconciles, expected) in cases {
            let commands = worktree_cleanup_actionable(&output, cleanup_branches, reconciles)
                .next_actions
                .into_iter()
                .map(|action| action.command)
                .collect::<Vec<_>>();
            assert_eq!(commands, expected);
        }
    }
}
