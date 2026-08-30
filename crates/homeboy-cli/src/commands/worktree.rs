use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};
use serde::Serialize;

use homeboy::core::cleanup::{
    self as artifact_cleanup, ArtifactCleanupOptions, ArtifactCleanupOutput, ArtifactCleanupSort,
};
use homeboy::core::worktree::{
    self, CleanupPolicy, TaskWorktreeRegistryQuarantine, WorktreeAdoptOptions, WorktreeAdoptOutput,
    WorktreeCleanupOutput, WorktreeCreateOptions, WorktreeCreateOutput, WorktreeImportOptions,
    WorktreeImportOutput, WorktreeInventoryOptions, WorktreeInventoryOutput, WorktreeListOutput,
    WorktreeOwnershipProbe, WorktreeQueueCreateOptions, WorktreeQueueCreateOutput,
    WorktreeRemoveOptions, WorktreeRemoveOutput, WorktreeStatusOutput,
};
use homeboy::core::worktree_provider::{
    self, ConfiguredWorktreeCleanupOutput as WorktreeProviderCleanupOutput,
    ConfiguredWorktreeCreateEvidence, WorktreeCleanupRequest, WorktreeCleanupScope,
    WorktreeFinalization, WorktreeFinalizationLookup, WorktreeProviderCreateOutput,
    WorktreeProviderIdentity, WorktreeProviderSafety, WorktreeProviderWorkspace,
    WorktreeProvisionLifecycle, WorktreeStatusEvidence, WorktreeTerminalDisposition,
};

use crate::command_contract::{LabCommandContract, WORKTREE_CLEANUP_LAB_LABEL};

use super::utils::args::MutationArgs;
use super::utils::response::{CommandActionableMetadata, CommandNextAction, CommandNextActionKind};
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
    /// Create a task worktree through the configured or built-in provider
    Create {
        /// Component or repository handle for provider creation
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
    /// List worktrees owned by configured and built-in providers
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
    /// Inspect a provider-owned worktree and its safety state
    Status {
        /// Task worktree ID, e.g. component@branch-slug
        id: String,
    },
    /// Report the session currently holding a managed checkout's write lease
    Holder {
        /// Managed worktree handle or any path inside the checkout
        target: String,
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
    /// Clean up eligible configured and built-in provider worktrees
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

#[derive(Debug, Clone, ValueEnum)]
enum CliTerminalDisposition {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Interrupted,
}

impl From<CliTerminalDisposition> for WorktreeTerminalDisposition {
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
    Create(WorktreeCreateCommandOutput),
    Import(WorktreeImportOutput),
    Finalize(WorktreeFinalization),
    Adopt(WorktreeAdoptOutput),
    QueueCreate(WorktreeQueueCreateOutput),
    List(WorktreeListCommandOutput),
    Inventory(WorktreeInventoryOutput),
    Status(WorktreeStatusCommandOutput),
    Holder(WorktreeOwnershipProbe),
    Remove(WorktreeRemoveOutput),
    Cleanup(WorktreeCleanupCommandOutput),
    QuarantineList(Vec<TaskWorktreeRegistryQuarantine>),
    QuarantineClear(TaskWorktreeRegistryQuarantine),
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum WorktreeCreateCommandOutput {
    Native(WorktreeCreateOutput),
    Configured(ConfiguredWorktreeCreateEvidence),
}

impl From<WorktreeProviderCreateOutput> for WorktreeCreateCommandOutput {
    fn from(output: WorktreeProviderCreateOutput) -> Self {
        match output {
            WorktreeProviderCreateOutput::Native(output) => Self::Native(output),
            WorktreeProviderCreateOutput::Configured(output) => Self::Configured(output.into()),
        }
    }
}

#[derive(Serialize)]
pub struct WorktreeListCommandOutput {
    #[serde(flatten)]
    pub native: WorktreeListOutput,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub provider_worktrees: Vec<ProviderWorktreeOutput>,
}

#[derive(Serialize)]
#[serde(untagged)]
pub enum WorktreeStatusCommandOutput {
    Native(WorktreeStatusOutput),
    Provider {
        provider_worktree: ProviderWorktreeOutput,
    },
}

#[derive(Debug, Serialize)]
pub struct ProviderWorktreeOutput {
    pub provider: String,
    pub handle: String,
    pub path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub repository: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub owner_run_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_disposition: Option<String>,
    pub safety: ProviderWorktreeSafetyOutput,
}

#[derive(Debug, Serialize)]
pub struct ProviderWorktreeSafetyOutput {
    pub dirty: bool,
    pub unpushed: bool,
    pub primary: bool,
    pub missing: bool,
}

impl From<WorktreeProviderWorkspace> for ProviderWorktreeOutput {
    fn from(workspace: WorktreeProviderWorkspace) -> Self {
        let provider = match workspace.ownership.provider {
            WorktreeProviderIdentity::Native => "native".to_string(),
            WorktreeProviderIdentity::Configured(provider) => provider,
        };
        Self {
            provider,
            handle: workspace.ownership.handle,
            path: workspace.ownership.path,
            branch: workspace.ownership.branch,
            task_url: workspace.ownership.task_url,
            repository: workspace.repository,
            owner_run_ref: workspace.owner_run_ref,
            created_at: workspace.created_at,
            terminal_disposition: workspace.terminal_disposition,
            safety: workspace.safety.into(),
        }
    }
}

impl From<WorktreeProviderSafety> for ProviderWorktreeSafetyOutput {
    fn from(safety: WorktreeProviderSafety) -> Self {
        Self {
            dirty: safety.dirty,
            unpushed: safety.unpushed,
            primary: safety.primary,
            missing: safety.missing,
        }
    }
}

#[derive(Serialize)]
pub struct WorktreeCleanupCommandOutput {
    pub worktrees: WorktreeCleanupOutput,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_worktrees: Option<WorktreeProviderCleanupOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_cleanup: Option<ArtifactCleanupOutput>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deprecated_flag: Option<&'static str>,
    #[serde(rename = "_homeboy_actionable")]
    pub actionable: CommandActionableMetadata,
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
    let mut exit_code = 0;
    let output = match args.command {
        WorktreeCommand::Create {
            component_id,
            branch,
            from,
            task_url,
            run_id,
            cleanup_policy,
        } => WorktreeOutput::Create(
            worktree_provider::create_worktree(WorktreeCreateOptions {
                component_id,
                branch,
                from,
                task_url,
                run_id,
                cleanup_policy: cleanup_policy.map(Into::into),
            })?
            .into(),
        ),
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
        } => {
            let lifecycle = WorktreeProvisionLifecycle {
                purpose: "operator_terminal_finalization".to_string(),
                owner_run_ref,
                cleanup_policy:
                    homeboy::core::worktree_provider::WorktreeCleanupPolicy::PreserveOnFailure,
            };
            match worktree_provider::finalize_worktree_from_config(
                &handle,
                &lifecycle,
                disposition.into(),
                &homeboy::core::defaults::load_config(),
            )? {
                WorktreeFinalizationLookup::Finalized(output) => WorktreeOutput::Finalize(output),
                WorktreeFinalizationLookup::Unsupported => {
                    return Err(homeboy::core::Error::validation_invalid_argument(
                        "handle",
                        "selected worktree provider does not support terminal finalization",
                        Some(handle),
                        None,
                    ));
                }
                WorktreeFinalizationLookup::NotFound => {
                    return Err(homeboy::core::Error::validation_invalid_argument(
                        "handle",
                        "worktree handle was not found",
                        Some(handle),
                        None,
                    ));
                }
            }
        }
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
        WorktreeCommand::List => {
            let report = worktree_provider::list_worktrees()?;
            let provider_worktrees = report
                .provider_worktrees
                .into_iter()
                .map(ProviderWorktreeOutput::from)
                .collect();
            WorktreeOutput::List(WorktreeListCommandOutput {
                native: report.native,
                provider_worktrees,
            })
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
            },
            &AgentTaskAuthority(std::sync::Mutex::new(std::collections::HashMap::new())),
        )?),
        WorktreeCommand::Status { id } => {
            let status = match worktree_provider::worktree_status(&id)? {
                WorktreeStatusEvidence::Native(status) => {
                    WorktreeStatusCommandOutput::Native(status)
                }
                WorktreeStatusEvidence::Provider(workspace) => {
                    WorktreeStatusCommandOutput::Provider {
                        provider_worktree: workspace.into(),
                    }
                }
            };
            WorktreeOutput::Status(status)
        }
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
            let cleanup = worktree_provider::cleanup_worktrees_from_config(
                &WorktreeCleanupRequest {
                    scope: WorktreeCleanupScope::All,
                    providers: Vec::new(),
                    all_configured_providers: true,
                    apply,
                    force,
                    cleanup_branches,
                    allow_unmerged_branches,
                    timeout: None,
                    provider_run_id: None,
                    provider_plan_id: None,
                },
                &homeboy::core::defaults::load_config(),
            )?;
            let worktrees = cleanup
                .native
                .expect("all-provider cleanup includes the built-in provider");
            let provider_worktrees = cleanup
                .configured
                .expect("all-provider cleanup includes configured providers");
            exit_code = (provider_worktrees.failure_count > 0) as i32;
            let provider_worktrees =
                (provider_worktrees.provider_count > 0).then_some(provider_worktrees);
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
                        max_scan_duration: None,
                    },
                )?)
            } else {
                None
            };
            let actionable = worktree_cleanup_actionable(&worktrees, cleanup_branches);
            WorktreeOutput::Cleanup(WorktreeCleanupCommandOutput {
                worktrees,
                provider_worktrees,
                artifact_cleanup,
                deprecated_flag: deprecated_dry_run.then_some("--dry-run"),
                actionable,
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
    Ok((output, exit_code))
}

#[cfg(test)]
fn cleanup_is_dry_run(apply: bool) -> bool {
    !apply
}

fn worktree_cleanup_actionable(
    output: &WorktreeCleanupOutput,
    cleanup_branches: bool,
) -> CommandActionableMetadata {
    let mut actionable = CommandActionableMetadata::default();
    if output.counts.reconciliation_blockers > 0 {
        actionable.next_actions.push(
            CommandNextAction::new(
                "reconcile task worktrees",
                "homeboy worktree inventory --apply",
            )
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
    use clap::{error::ErrorKind, Parser};
    use homeboy::core::worktree::{
        CleanupPolicy, TaskWorktreeRecord, TaskWorktreeState, WorktreeCreateAction,
        WorktreeCreateEvidence, WorktreeCreateOutput, WorktreeCreateReconciliation,
        WorktreeImportOutput, WorktreeListOutput,
    };

    use crate::cli_surface::{Cli, Commands};
    use homeboy::core::worktree_provider::{
        WorktreeOwnership, WorktreeProviderIdentity, WorktreeProvisionDestination,
    };

    use super::{
        cleanup_is_dry_run, ProviderWorktreeOutput, ProviderWorktreeSafetyOutput, WorktreeCommand,
        WorktreeCreateCommandOutput, WorktreeListCommandOutput, WorktreeOutput,
        WorktreeStatusCommandOutput,
    };

    fn create_output(reconciliation: Option<WorktreeCreateReconciliation>) -> WorktreeCreateOutput {
        let identity = homeboy::core::worktree::WorkspaceIdentity::new(
            "task-worktree",
            "fixture/fixture@branch",
        )
        .expect("workspace identity");
        WorktreeCreateOutput {
            record: TaskWorktreeRecord {
                id: "fixture@branch".to_string(),
                component_id: "fixture".to_string(),
                source_checkout: "/tmp/source".to_string(),
                worktree_path: "/tmp/fixture@branch".to_string(),
                branch: "branch".to_string(),
                base_ref: "HEAD".to_string(),
                workspace_identity: Some(identity),
                task_url: None,
                run_id: None,
                cleanup_policy: CleanupPolicy::RemoveWhenSafe,
                terminal_disposition: None,
                branch_cleanup_intent: Default::default(),
                created_at: "2026-01-01T00:00:00Z".to_string(),
                state: TaskWorktreeState::Active,
                lifecycle_revision: 0,
                terminal_workspace_authority: None,
            },
            reconciliation,
        }
    }

    #[test]
    fn worktree_create_serialization_preserves_outer_action_and_adds_restore_detail() {
        let created = serde_json::to_value(WorktreeOutput::Create(
            WorktreeCreateCommandOutput::Native(create_output(None)),
        ))
        .expect("serialize created output");
        let existing = serde_json::to_value(WorktreeOutput::Create(
            WorktreeCreateCommandOutput::Native(create_output(None)),
        ))
        .expect("serialize existing output");
        let evidence = WorktreeCreateEvidence {
            task_worktree_id: "fixture@branch".to_string(),
            component_id: "fixture".to_string(),
            source_checkout: "/tmp/source".to_string(),
            worktree_path: "/tmp/fixture@branch".to_string(),
            branch: "branch".to_string(),
            workspace_identity: homeboy::core::worktree::WorkspaceIdentity::new(
                "task-worktree",
                "fixture/fixture@branch",
            )
            .expect("workspace identity"),
            git_registration: "registered".to_string(),
        };
        let restored =
            serde_json::to_value(WorktreeOutput::Create(WorktreeCreateCommandOutput::Native(
                create_output(Some(WorktreeCreateReconciliation {
                    action: WorktreeCreateAction::Restored,
                    previous: evidence.clone(),
                    current: evidence,
                })),
            )))
            .expect("serialize restored output");

        assert_eq!(created["action"], "create");
        assert_eq!(existing["action"], "create");
        assert!(created.get("reconciliation").is_none());
        assert!(existing.get("reconciliation").is_none());
        assert_eq!(restored["action"], "create");
        assert_eq!(restored["reconciliation"]["action"], "restored");
    }

    #[test]
    fn worktree_import_and_finalize_are_public_typed_cli_commands() {
        let cli = Cli::parse_from([
            "homeboy",
            "worktree",
            "import",
            "fixture",
            "fixture@branch",
            "/tmp/fixture@branch",
            "--branch",
            "branch",
            "--base-ref",
            "main",
            "--owner-run-ref",
            "run-1",
            "--cleanup-policy",
            "preserve-on-failure",
            "--created-at",
            "2026-01-02T03:04:05Z",
        ]);
        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Import {
            owner_run_ref,
            created_at,
            ..
        } = args.command
        else {
            panic!("expected import command");
        };
        assert_eq!(owner_run_ref.as_deref(), Some("run-1"));
        assert_eq!(created_at.as_deref(), Some("2026-01-02T03:04:05Z"));

        let record = create_output(None).record;
        let imported = serde_json::to_value(WorktreeOutput::Import(WorktreeImportOutput {
            record,
            imported: true,
        }))
        .expect("serialize import");
        let finalized = serde_json::to_value(WorktreeOutput::Finalize(
            homeboy::core::worktree_provider::WorktreeFinalization {
                provider_id: "builtin".to_string(),
                handle: "fixture@branch".to_string(),
                disposition:
                    homeboy::core::worktree_provider::WorktreeTerminalDisposition::Succeeded,
                owner_outcome: "success".to_string(),
                lifecycle_state: "completed".to_string(),
                inspection_path: "/tmp/fixture@branch".to_string(),
            },
        ))
        .expect("serialize finalization");

        assert_eq!(imported["action"], "import");
        assert_eq!(imported["imported"], true);
        assert_eq!(finalized["action"], "finalize");
        assert_eq!(finalized["disposition"], "succeeded");
    }

    #[test]
    fn configured_worktree_create_serialization_exposes_provider_evidence() {
        let output =
            serde_json::to_value(WorktreeOutput::Create(WorktreeCreateCommandOutput::from(
                homeboy::core::worktree_provider::WorktreeProviderCreateOutput::Configured(
                    homeboy::core::worktree_provider::WorktreeProvision {
                        destination: WorktreeProvisionDestination {
                            ownership: WorktreeOwnership {
                                provider: WorktreeProviderIdentity::Configured(
                                    "fixture-provider".to_string(),
                                ),
                                handle: "fixture@branch".to_string(),
                                path: "/tmp/fixture@branch".to_string(),
                                kind: homeboy::core::worktree_provider::WorktreeWorkspaceKind::Configured,
                                branch: Some("branch".to_string()),
                                task_url: Some("https://example.test/1".to_string()),
                                provenance: None,
                            },
                            exact_identity: None,
                        },
                        action: homeboy::core::worktree_provider::WorktreeProvisionAction::Ensured,
                        idempotency_key: "fixture-key".to_string(),
                    },
                ),
            )))
            .expect("serialize configured create output");

        assert_eq!(output["action"], "create");
        assert_eq!(output["provider"], "fixture-provider");
        assert_eq!(output["provision_action"], "ensured");
        assert_eq!(output["idempotency_key"], "fixture-key");
        assert!(output.get("record").is_none());
    }

    #[test]
    fn worktree_list_preserves_native_schema_when_no_configured_provider_is_present() {
        let output = serde_json::to_value(WorktreeOutput::List(WorktreeListCommandOutput {
            native: WorktreeListOutput {
                worktrees: Vec::new(),
            },
            provider_worktrees: Vec::new(),
        }))
        .expect("serialize worktree list");

        assert_eq!(output["action"], "list");
        assert_eq!(output["worktrees"], serde_json::json!([]));
        assert!(output.get("provider_worktrees").is_none());
    }

    #[test]
    fn configured_provider_status_reports_unsafe_state_without_changing_native_fields() {
        let provider_worktree = ProviderWorktreeOutput {
            provider: "fixture-provider".to_string(),
            handle: "fixture@unsafe".to_string(),
            path: "/tmp/fixture@unsafe".to_string(),
            branch: Some("unsafe".to_string()),
            task_url: None,
            repository: None,
            owner_run_ref: None,
            created_at: None,
            terminal_disposition: None,
            safety: ProviderWorktreeSafetyOutput {
                dirty: true,
                unpushed: true,
                primary: false,
                missing: false,
            },
        };
        let output = serde_json::to_value(WorktreeOutput::Status(
            WorktreeStatusCommandOutput::Provider { provider_worktree },
        ))
        .expect("serialize provider worktree status");

        assert_eq!(output["action"], "status");
        assert_eq!(output["provider_worktree"]["provider"], "fixture-provider");
        assert_eq!(output["provider_worktree"]["safety"]["dirty"], true);
        assert!(output.get("record").is_none());
    }

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
    fn worktree_holder_accepts_a_checkout_path() {
        let cli = Cli::parse_from(["homeboy", "worktree", "holder", "/tmp/managed-checkout"]);
        let Commands::Worktree(args) = cli.command else {
            panic!("expected worktree command");
        };
        let WorktreeCommand::Holder { target } = args.command else {
            panic!("expected holder command");
        };
        assert_eq!(target, "/tmp/managed-checkout");
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

    #[test]
    fn worktree_cleanup_prioritizes_bounded_reconciliation_before_cleanup() {
        let output = homeboy::core::worktree::WorktreeCleanupOutput {
            dry_run: true,
            counts: homeboy::core::worktree::WorktreeCleanupCounts {
                candidates: 1,
                skipped: 1,
                reconciliation_blockers: 1,
                ..Default::default()
            },
            candidates: Vec::new(),
            removed: Vec::new(),
            skipped: Vec::new(),
        };

        let actions = super::worktree_cleanup_actionable(&output, true);
        assert_eq!(
            actions.next_actions[0].command,
            "homeboy worktree inventory --apply"
        );
        assert_eq!(
            actions.next_actions[1].command,
            "homeboy worktree cleanup --cleanup-branches --apply"
        );
    }

    #[test]
    fn worktree_cleanup_preview_preserves_branch_cleanup_request() {
        let output = homeboy::core::worktree::WorktreeCleanupOutput {
            dry_run: true,
            counts: homeboy::core::worktree::WorktreeCleanupCounts {
                candidates: 1,
                ..Default::default()
            },
            candidates: Vec::new(),
            removed: Vec::new(),
            skipped: Vec::new(),
        };

        let without_branches = super::worktree_cleanup_actionable(&output, false);
        assert_eq!(
            without_branches.next_actions[0].command,
            "homeboy worktree cleanup --apply"
        );
        let with_branches = super::worktree_cleanup_actionable(&output, true);
        assert_eq!(
            with_branches.next_actions[0].command,
            "homeboy worktree cleanup --cleanup-branches --apply"
        );
    }

    #[test]
    fn worktree_cleanup_apply_omits_action_after_removing_all_candidates() {
        let output = homeboy::core::worktree::WorktreeCleanupOutput {
            dry_run: false,
            counts: homeboy::core::worktree::WorktreeCleanupCounts {
                candidates: 1,
                removed: 1,
                ..Default::default()
            },
            candidates: Vec::new(),
            removed: Vec::new(),
            skipped: Vec::new(),
        };

        assert!(super::worktree_cleanup_actionable(&output, true)
            .next_actions
            .is_empty());
    }
}
