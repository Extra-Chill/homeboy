use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::cleanup::{
    self as artifact_cleanup, ArtifactCleanupOptions, ArtifactCleanupOutput, ArtifactCleanupSort,
};
use homeboy::core::worktree::{
    self, CleanupPolicy, TaskWorktreeRegistryQuarantine, WorktreeAdoptOptions, WorktreeAdoptOutput,
    WorktreeCleanupOptions, WorktreeCleanupOutput, WorktreeCreateOptions, WorktreeCreateOutput,
    WorktreeInventoryOptions, WorktreeInventoryOutput, WorktreeListOutput,
    WorktreeQueueCreateOptions, WorktreeQueueCreateOutput, WorktreeRemoveOptions,
    WorktreeRemoveOutput, WorktreeStatusOutput,
};

use crate::command_contract::{LabCommandContract, WORKTREE_CLEANUP_LAB_LABEL};

use super::utils::args::MutationArgs;
use super::CmdResult;

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
    /// Create a task worktree from a registered component checkout
    Create {
        /// Component ID to use as the source checkout
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
    /// List persisted task worktrees
    List,
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
    /// Inspect one task worktree and its safety gates
    Status {
        /// Task worktree ID, e.g. component@branch-slug
        id: String,
    },
    /// Remove one task worktree after safety checks
    Remove {
        /// Task worktree ID, e.g. component@branch-slug
        id: String,
        /// Allow dirty/unpushed worktree removal; hard gates still apply
        #[arg(long)]
        force: bool,
        /// Delete the local task branch after removing the worktree when branch safety allows it.
        #[arg(long)]
        cleanup_branch: bool,
        /// Permit deleting an unmerged task branch. Requires --cleanup-branch.
        #[arg(long, requires = "cleanup_branch")]
        allow_unmerged_branch: bool,
    },
    /// Remove cleanup-eligible task worktrees after safety checks
    Cleanup {
        // Remove planned worktrees and artifacts after safety checks.
        // Without --apply, only reports the plan; --dry-run names that
        // default explicitly. This pair is the precedent the shared
        // plan-default mutation group was modeled on (#11139).
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
    Adopt(WorktreeAdoptOutput),
    QueueCreate(WorktreeQueueCreateOutput),
    List(WorktreeListOutput),
    Inventory(WorktreeInventoryOutput),
    Status(WorktreeStatusOutput),
    Remove(WorktreeRemoveOutput),
    Cleanup(WorktreeCleanupCommandOutput),
    QuarantineList(Vec<TaskWorktreeRegistryQuarantine>),
    QuarantineClear(TaskWorktreeRegistryQuarantine),
}

#[derive(Serialize)]
pub struct WorktreeCleanupCommandOutput {
    pub worktrees: WorktreeCleanupOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_cleanup: Option<ArtifactCleanupOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated_flag: Option<&'static str>,
}

pub fn run(args: WorktreeArgs) -> CmdResult<WorktreeOutput> {
    struct AgentTaskAuthority(
        std::sync::Mutex<
            std::collections::HashMap<
                String,
                homeboy::agents::agent_task_lifecycle::CompositeWorkspaceClaim,
            >,
        >,
    );
    impl worktree::WorktreeReconciliationAuthority for AgentTaskAuthority {
        fn acquire(
            &self,
            record: &worktree::TaskWorktreeRecord,
        ) -> homeboy::core::Result<worktree::WorktreeLivenessAuthority> {
            let workspace = match record.effective_workspace_identity() {
                Ok(workspace) => workspace,
                Err(error) => {
                    return Ok(worktree::WorktreeLivenessAuthority::Incomplete {
                        reason: error.message,
                    })
                }
            };
            let proof = match homeboy::agents::agent_task_lifecycle::resolve_terminal_workspace_authority(record)? {
                homeboy::agents::agent_task_lifecycle::TerminalWorkspaceAuthorityResolution::Proven(proof) => proof,
                homeboy::agents::agent_task_lifecycle::TerminalWorkspaceAuthorityResolution::Refused { reason, .. } => {
                    return Ok(worktree::WorktreeLivenessAuthority::Incomplete { reason });
                }
            };
            // Persist before acquiring the fence. A no-run-id record can only
            // ever reach this point through a previously exact cached proof.
            homeboy::core::worktree::persist_terminal_workspace_authority(
                &record.id,
                record.lifecycle_revision,
                *proof,
            )?;
            match homeboy::agents::agent_task_lifecycle::acquire_composite_workspace_claim(
                workspace,
                record.lifecycle_revision,
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
                        provenance: "complete local/direct/reverse workspace reconciliation claim"
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
        ) -> homeboy::core::Result<bool> {
            let claims = self.0.lock().map_err(|_| {
                homeboy::core::Error::internal_unexpected("workspace claim adapter lock poisoned")
            })?;
            claims
                .get(&claim.token)
                .map(homeboy::agents::agent_task_lifecycle::validate_composite_workspace_claim)
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
            match homeboy::agents::agent_task_lifecycle::release_composite_workspace_claim(&mut composite)? {
                homeboy::agents::agent_task_lifecycle::CompositeWorkspaceClaimRelease::Released => {
                    Ok(())
                }
                homeboy::agents::agent_task_lifecycle::CompositeWorkspaceClaimRelease::Partial { failures } => {
                    let error = homeboy::core::Error::validation_invalid_argument(
                        "workspace_claim_composite",
                        "workspace composite release partially failed",
                        None,
                        Some(failures),
                    );
                    let status = homeboy::agents::agent_task_lifecycle::persist_and_retry_composite_workspace_cleanup(composite, &error);
                    Err(homeboy::core::Error::validation_invalid_argument(
                        "workspace_claim_composite",
                        format!("workspace composite release incomplete: {}", status.public_summary()),
                        None,
                        None,
                    ))
                }
            }
        }
    }
    let output = match args.command {
        WorktreeCommand::Create {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy,
        } => WorktreeOutput::Create(worktree::create(WorktreeCreateOptions {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy: cleanup_policy.map(Into::into),
        })?),
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
            branches,
            from,
            task_url,
            task_ref,
            dry_run,
            retry_after_seconds,
        })?),
        WorktreeCommand::List => WorktreeOutput::List(worktree::list()?),
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
            },
            &AgentTaskAuthority(std::sync::Mutex::new(std::collections::HashMap::new())),
        )?),
        WorktreeCommand::Status { id } => WorktreeOutput::Status(worktree::status(&id)?),
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
        })?),
        WorktreeCommand::Cleanup {
            mutation,
            force,
            cleanup_artifacts,
            cleanup_branches,
            allow_unmerged_branches,
        } => {
            let apply = mutation.is_apply();
            let deprecated_dry_run = mutation.dry_run;
            let dry_run = cleanup_is_dry_run(apply);
            let worktrees = worktree::cleanup(WorktreeCleanupOptions {
                force,
                dry_run,
                cleanup_branches,
                allow_unmerged_branches,
            })?;
            let artifact_cleanup = if cleanup_artifacts {
                Some(artifact_cleanup::cleanup_artifacts(
                    ArtifactCleanupOptions {
                        path: None,
                        apply,
                        self_artifacts: true,
                        temp_roots: Vec::new(),
                        sort: ArtifactCleanupSort::Discovery,
                        limit: None,
                        merged_only: false,
                        min_age_days: None,
                        include_active_worktrees: false,
                    },
                )?)
            } else {
                None
            };
            WorktreeOutput::Cleanup(WorktreeCleanupCommandOutput {
                worktrees,
                artifact_cleanup,
                deprecated_flag: deprecated_dry_run.then_some("--dry-run"),
            })
        }
        WorktreeCommand::Quarantine { command } => match command {
            WorktreeQuarantineCommand::List => {
                WorktreeOutput::QuarantineList(worktree::list_task_worktree_registry_quarantines()?)
            }
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
    Ok((output, 0))
}

fn cleanup_is_dry_run(apply: bool) -> bool {
    !apply
}

#[cfg(test)]
mod tests {
    use clap::{error::ErrorKind, Parser};

    use crate::cli_surface::{Cli, Commands};

    use super::{cleanup_is_dry_run, WorktreeCommand};

    #[test]
    fn worktree_cleanup_bare_is_a_plan() {
        let cli = Cli::parse_from(["homeboy", "worktree", "cleanup"]);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Cleanup {
            mutation,
            cleanup_artifacts,
            ..
        } = args.command
        else {
            panic!("expected worktree cleanup command");
        };

        assert!(!mutation.apply);
        assert!(!mutation.dry_run);
        assert!(cleanup_is_dry_run(mutation.is_apply()));
        assert!(!cleanup_artifacts);
    }

    #[test]
    fn worktree_cleanup_apply_enables_worktree_and_artifact_mutation() {
        let cli = Cli::parse_from([
            "homeboy",
            "worktree",
            "cleanup",
            "--apply",
            "--cleanup-artifacts",
        ]);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Cleanup {
            mutation,
            cleanup_artifacts,
            ..
        } = args.command
        else {
            panic!("expected worktree cleanup command");
        };

        assert!(mutation.apply);
        assert!(!cleanup_is_dry_run(mutation.is_apply()));
        assert!(cleanup_artifacts);
    }

    #[test]
    fn worktree_cleanup_dry_run_is_a_legacy_plan_only_alias() {
        let cli = Cli::parse_from(["homeboy", "worktree", "cleanup", "--dry-run"]);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Cleanup { mutation, .. } = args.command else {
            panic!("expected worktree cleanup command");
        };

        assert!(!mutation.apply);
        assert!(mutation.dry_run);
        assert!(cleanup_is_dry_run(mutation.is_apply()));
    }

    #[test]
    fn worktree_cleanup_dry_run_marks_the_deprecated_flag() {
        let cli = Cli::parse_from(["homeboy", "worktree", "cleanup", "--dry-run"]);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Cleanup { mutation, .. } = args.command else {
            panic!("expected worktree cleanup command");
        };

        assert_eq!(mutation.dry_run.then_some("--dry-run"), Some("--dry-run"));
    }

    #[test]
    fn worktree_inventory_defaults_to_a_bounded_preview() {
        let cli = Cli::parse_from(["homeboy", "worktree", "inventory"]);

        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Inventory {
            limit,
            cursor,
            adopted_cursor,
            apply,
        } = args.command
        else {
            panic!("expected worktree inventory command");
        };

        assert_eq!(limit, 500);
        assert!(cursor.is_none());
        assert!(adopted_cursor.is_none());
        assert!(!apply);
    }

    #[test]
    fn worktree_cleanup_rejects_conflicting_apply_and_dry_run() {
        let error =
            match Cli::try_parse_from(["homeboy", "worktree", "cleanup", "--apply", "--dry-run"]) {
                Ok(_) => panic!("conflicting cleanup modes must be rejected"),
                Err(error) => error,
            };

        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }
}
