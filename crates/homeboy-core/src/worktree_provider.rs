use crate::error::{Error, Result};
use crate::worktree;
use std::path::{Path, PathBuf};

/// Ownership recorded in Homeboy's native workspace registry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOwnership {
    pub handle: String,
    pub path: String,
    pub kind: WorktreeWorkspaceKind,
    pub branch: Option<String>,
    pub task_url: Option<String>,
    pub provenance: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeWorkspaceKind {
    TaskWorktree,
    AdoptedWorkspace,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeSafety {
    pub dirty: bool,
    pub unpushed: bool,
    pub primary: bool,
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeWorkspace {
    pub ownership: WorktreeOwnership,
    pub repository: Option<String>,
    pub owner_run_ref: Option<String>,
    pub created_at: Option<String>,
    pub terminal_disposition: Option<String>,
    pub safety: WorktreeSafety,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMutationTarget {
    pub handle: String,
    pub path: PathBuf,
    pub source_kind: Option<String>,
    pub branch: Option<String>,
    pub task_url: Option<String>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct WorktreeMutationContext<'a> {
    pub safety_baseline: Option<&'a serde_json::Value>,
}

#[derive(Debug, Clone)]
pub struct WorktreeCleanupRequest {
    pub apply: bool,
    pub force: bool,
    pub cleanup_branches: bool,
    pub allow_unmerged_branches: bool,
}

impl Default for WorktreeCleanupRequest {
    fn default() -> Self {
        Self {
            apply: false,
            force: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProvisionIntent {
    pub handle: String,
    pub repo: String,
    pub base: String,
    pub head: String,
    pub task_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProvisionLifecycle {
    pub purpose: String,
    pub owner_run_ref: String,
    pub cleanup_policy: WorktreeCleanupPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeCleanupPolicy {
    RemoveOnSuccess,
    PreserveOnFailure,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeTerminalDisposition {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    Interrupted,
}

impl WorktreeTerminalDisposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }

    fn owner_outcome(self) -> &'static str {
        match self {
            Self::Succeeded => "success",
            Self::Failed | Self::Cancelled | Self::TimedOut | Self::Interrupted => "failure",
        }
    }

    fn lifecycle_state(self) -> &'static str {
        match self {
            Self::Succeeded => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::TimedOut => "timed_out",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct WorktreeFinalization {
    pub handle: String,
    pub disposition: WorktreeTerminalDisposition,
    pub owner_outcome: String,
    pub lifecycle_state: String,
    pub inspection_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProvisionDestination {
    pub ownership: WorktreeOwnership,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProvisionLookup {
    Admitted(WorktreeProvisionDestination),
    NotFound,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProvisionPlan {
    Admitted(WorktreeProvisionDestination),
    Planned(WorktreeProvisionDestination),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProvisionAction {
    Admitted,
    Ensured,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProvision {
    pub destination: WorktreeProvisionDestination,
    pub action: WorktreeProvisionAction,
    pub idempotency_key: String,
}

/// Native worktree registry operations.
pub struct NativeWorktreeRegistry;

impl NativeWorktreeRegistry {
    pub fn resolve(&self, handle: &str) -> Result<Option<WorktreeOwnership>> {
        let Some(record) = worktree::resolve_workspace_ref_if_present(handle)? else {
            return Ok(None);
        };
        if record.handle() != handle {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "native worktree registry record `{}` does not match requested handle `{handle}`",
                    record.handle()
                ),
                Some(handle.to_string()),
                None,
            ));
        }
        if record.state() == &worktree::TaskWorktreeState::Removed {
            return Ok(None);
        }
        let path = PathBuf::from(record.path());
        let (kind, branch, task_url, provenance) = match &record {
            worktree::WorkspaceRefRecord::Task(record) => {
                let safety = worktree::safety_report_for_provider(record)?;
                if safety.worktree_missing || !safety.safe {
                    let mut reasons = safety.reasons;
                    if safety.worktree_missing {
                        reasons.push("worktree directory is missing".to_string());
                    }
                    return Err(Error::validation_invalid_argument(
                        "to_worktree",
                        format!("native worktree `{handle}` is not safe for use"),
                        Some(handle.to_string()),
                        Some(reasons),
                    ));
                }
                (
                    WorktreeWorkspaceKind::TaskWorktree,
                    Some(record.branch.clone()),
                    record.task_url.clone(),
                    None,
                )
            }
            worktree::WorkspaceRefRecord::Adopted(_) if !path.is_dir() => {
                return Err(Error::validation_invalid_argument(
                    "to_worktree",
                    format!("adopted workspace `{handle}` points at a missing directory"),
                    Some(path.display().to_string()),
                    None,
                ));
            }
            worktree::WorkspaceRefRecord::Adopted(_) => (
                WorktreeWorkspaceKind::AdoptedWorkspace,
                None,
                None,
                record.provenance().cloned(),
            ),
        };
        Ok(Some(WorktreeOwnership {
            handle: record.handle().to_string(),
            path: path.display().to_string(),
            kind,
            branch,
            task_url,
            provenance,
        }))
    }

    pub fn list(&self) -> Result<Vec<WorktreeWorkspace>> {
        worktree::list_workspace_refs()?
            .into_iter()
            .filter(|record| record.state() == &worktree::TaskWorktreeState::Active)
            .map(|record| {
                let (
                    kind,
                    branch,
                    task_url,
                    repository,
                    owner_run_ref,
                    terminal_disposition,
                    safety,
                ) = match &record {
                    worktree::WorkspaceRefRecord::Task(record) => {
                        let safety = worktree::safety_report_for_provider(record)?;
                        (
                            WorktreeWorkspaceKind::TaskWorktree,
                            Some(record.branch.clone()),
                            record.task_url.clone(),
                            Some(record.component_id.clone()),
                            record.run_id.clone(),
                            record.terminal_disposition.clone(),
                            WorktreeSafety {
                                dirty: safety.dirty,
                                unpushed: safety.unpushed_commits > 0,
                                primary: safety.primary_checkout,
                                missing: safety.worktree_missing,
                            },
                        )
                    }
                    worktree::WorkspaceRefRecord::Adopted(record) => (
                        WorktreeWorkspaceKind::AdoptedWorkspace,
                        None,
                        None,
                        None,
                        None,
                        None,
                        WorktreeSafety {
                            dirty: false,
                            unpushed: false,
                            primary: false,
                            missing: !Path::new(&record.path).is_dir(),
                        },
                    ),
                };
                Ok(WorktreeWorkspace {
                    ownership: WorktreeOwnership {
                        handle: record.handle().to_string(),
                        path: record.path().to_string(),
                        kind,
                        branch,
                        task_url,
                        provenance: record.provenance().cloned(),
                    },
                    repository,
                    owner_run_ref,
                    created_at: Some(match record {
                        worktree::WorkspaceRefRecord::Task(record) => record.created_at,
                        worktree::WorkspaceRefRecord::Adopted(record) => record.created_at,
                    }),
                    terminal_disposition,
                    safety,
                })
            })
            .collect()
    }

    pub fn resolve_mutation(
        &self,
        reference: &str,
        _context: WorktreeMutationContext<'_>,
    ) -> Result<Option<WorktreeMutationTarget>> {
        let Some(record) = worktree::resolve_workspace_ref_if_present(reference)? else {
            return Ok(None);
        };
        if record.handle() != reference {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "native workspace registry record `{}` does not match requested handle `{reference}`",
                    record.handle()
                ),
                Some(reference.to_string()),
                None,
            ));
        }
        if record.state() != &worktree::TaskWorktreeState::Active {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "Homeboy workspace '{}' is no longer active",
                    record.handle()
                ),
                Some(reference.to_string()),
                None,
            ));
        }
        let path = PathBuf::from(record.path());
        if !path.is_dir() {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "Homeboy workspace '{}' points at a missing directory {}; recreate or remove the stale record",
                    record.handle(),
                    path.display()
                ),
                Some(reference.to_string()),
                None,
            ));
        }
        Ok(Some(WorktreeMutationTarget {
            handle: record.handle().to_string(),
            path,
            source_kind: Some(record.source_kind().to_string()),
            branch: match &record {
                worktree::WorkspaceRefRecord::Task(record) => Some(record.branch.clone()),
                worktree::WorkspaceRefRecord::Adopted(_) => None,
            },
            task_url: match &record {
                worktree::WorkspaceRefRecord::Task(record) => record.task_url.clone(),
                worktree::WorkspaceRefRecord::Adopted(_) => None,
            },
        }))
    }

    pub fn plan(&self, intent: &WorktreeProvisionIntent) -> Result<WorktreeProvisionPlan> {
        let intent = native_provision_intent(intent);
        if let Some(ownership) = self.resolve(&intent.handle)? {
            return Ok(WorktreeProvisionPlan::Admitted(
                WorktreeProvisionDestination { ownership },
            ));
        }
        Ok(WorktreeProvisionPlan::Planned(
            WorktreeProvisionDestination {
                ownership: WorktreeOwnership {
                    handle: intent.handle.clone(),
                    path: worktree::planned_create_path(&intent.repo, &intent.head, &intent.base)?,
                    kind: WorktreeWorkspaceKind::TaskWorktree,
                    branch: Some(intent.head.clone()),
                    task_url: intent.task_url.clone(),
                    provenance: None,
                },
            },
        ))
    }

    pub fn ensure(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvision> {
        let intent = native_provision_intent(intent);
        if let Some(ownership) = self.resolve(&intent.handle)? {
            return Ok(WorktreeProvision {
                destination: WorktreeProvisionDestination { ownership },
                action: WorktreeProvisionAction::Admitted,
                idempotency_key: worktree_provision_idempotency_key(&intent),
            });
        }
        let created = worktree::create(worktree::WorktreeCreateOptions {
            component_id: intent.repo.clone(),
            branch: intent.head.clone(),
            from: Some(intent.base.clone()),
            task_url: intent.task_url.clone(),
            run_id: Some(lifecycle.owner_run_ref.clone()),
            cleanup_policy: Some(match lifecycle.cleanup_policy {
                WorktreeCleanupPolicy::RemoveOnSuccess => worktree::CleanupPolicy::RemoveWhenSafe,
                WorktreeCleanupPolicy::PreserveOnFailure => {
                    worktree::CleanupPolicy::PreserveOnFailure
                }
            }),
        })?;
        Ok(WorktreeProvision {
            destination: WorktreeProvisionDestination {
                ownership: WorktreeOwnership {
                    handle: created.record.id,
                    path: created.record.worktree_path,
                    kind: WorktreeWorkspaceKind::TaskWorktree,
                    branch: Some(created.record.branch),
                    task_url: created.record.task_url,
                    provenance: None,
                },
            },
            action: WorktreeProvisionAction::Ensured,
            idempotency_key: worktree_provision_idempotency_key(&intent),
        })
    }

    pub fn finalize(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: WorktreeTerminalDisposition,
    ) -> Result<Option<WorktreeFinalization>> {
        let Some(workspace) = worktree::resolve_workspace_ref_if_present(handle)? else {
            return Ok(None);
        };
        let worktree::WorkspaceRefRecord::Task(record) = workspace else {
            return Ok(None);
        };
        if record.id != handle {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!(
                    "native worktree registry record `{}` does not match requested handle `{handle}`",
                    record.id
                ),
                Some(handle.to_string()),
                None,
            ));
        }
        let record =
            worktree::finalize_provider_lifecycle(handle, &lifecycle.owner_run_ref, disposition)?;
        Ok(Some(WorktreeFinalization {
            handle: record.id,
            disposition,
            owner_outcome: disposition.owner_outcome().to_string(),
            lifecycle_state: disposition.lifecycle_state().to_string(),
            inspection_path: record.worktree_path,
        }))
    }
}

fn native_provision_intent(intent: &WorktreeProvisionIntent) -> WorktreeProvisionIntent {
    let mut intent = intent.clone();
    intent.handle = worktree::handle_for_branch(&intent.repo, &intent.head);
    intent
}

pub fn resolve_worktree_ownership(handle: &str) -> Result<WorktreeOwnership> {
    NativeWorktreeRegistry.resolve(handle)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!("native worktree `{handle}` is not registered"),
            Some(handle.to_string()),
            None,
        )
    })
}

pub fn resolve_worktree_ownership_if_present(handle: &str) -> Result<Option<WorktreeOwnership>> {
    NativeWorktreeRegistry.resolve(handle)
}

pub fn list_worktree_inventory() -> Result<Vec<WorktreeWorkspace>> {
    NativeWorktreeRegistry.list()
}

pub fn resolve_worktree_mutation_target(
    reference: &str,
    context: WorktreeMutationContext<'_>,
) -> Result<WorktreeMutationTarget> {
    NativeWorktreeRegistry
        .resolve_mutation(reference, context)?
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "to_worktree",
                format!("native worktree `{reference}` is not registered"),
                Some(reference.to_string()),
                None,
            )
        })
}

pub fn cleanup_worktrees(
    request: &WorktreeCleanupRequest,
) -> Result<worktree::WorktreeCleanupOutput> {
    worktree::cleanup(worktree::WorktreeCleanupOptions {
        force: request.force,
        dry_run: !request.apply,
        cleanup_branches: request.cleanup_branches,
        allow_unmerged_branches: request.allow_unmerged_branches,
    })
}

pub fn plan_worktree_provision(intent: &WorktreeProvisionIntent) -> Result<WorktreeProvisionPlan> {
    NativeWorktreeRegistry.plan(intent)
}

pub fn ensure_worktree_provision(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
) -> Result<WorktreeProvision> {
    NativeWorktreeRegistry.ensure(intent, lifecycle)
}

/// Native provisioning does not consult configuration. This transition helper
/// keeps internal callers compiling while they drop their obsolete config input.
pub fn ensure_worktree_provision_from_config(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
    _selected_provider: Option<()>,
    _config: &crate::defaults::HomeboyConfig,
) -> Result<WorktreeProvision> {
    ensure_worktree_provision(intent, lifecycle)
}

pub fn finalize_worktree(
    handle: &str,
    lifecycle: &WorktreeProvisionLifecycle,
    disposition: WorktreeTerminalDisposition,
) -> Result<Option<WorktreeFinalization>> {
    NativeWorktreeRegistry.finalize(handle, lifecycle, disposition)
}

pub fn worktree_provision_idempotency_key(intent: &WorktreeProvisionIntent) -> String {
    homeboy_engine_primitives::content_hash::nul_separated_digest([
        intent.repo.as_str(),
        intent.base.as_str(),
        intent.head.as_str(),
        intent.task_url.as_deref().unwrap_or_default(),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_registry_omits_removed_records() {
        crate::test_support::with_isolated_home(|home| {
            worktree::record_removed_for_test("fixture@removed", &home.path().join("removed"));

            assert!(NativeWorktreeRegistry
                .resolve("fixture@removed")
                .expect("removed lookup")
                .is_none());
        });
    }

    #[test]
    fn native_registry_rejects_colliding_manifest_identity() {
        crate::test_support::with_isolated_home(|home| {
            worktree::record_removed_for_test("fixture@a/b", &home.path().join("worktree"));

            let error = NativeWorktreeRegistry
                .resolve("fixture@a?b")
                .expect_err("colliding handle must not resolve another manifest");
            assert!(error.message.contains("does not match requested handle"));
        });
    }
}
