use crate::defaults::{self, HomeboyConfig};
use crate::error::{Error, Result};
use crate::{worktree, worktree_providers};
use std::path::{Path, PathBuf};

/// Canonical read-only ownership returned by every worktree provider.
///
/// Lifecycle mutation remains capability-segregated: native registry
/// reconciliation and command-provider finalization have different authority
/// models and are not implied by read-only ownership.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProviderIdentity {
    Native,
    Configured(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeOwnership {
    pub provider: WorktreeProviderIdentity,
    pub handle: String,
    pub path: String,
    pub branch: String,
    pub task_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProviderLookup {
    Found(WorktreeOwnership),
    NotFound,
}

/// Provider-neutral read-only ownership contract.
///
/// `NotFound` means the provider authoritatively does not own the handle.
/// Corrupt state, malformed responses, timeouts, and unsafe workspaces are
/// errors and must never be treated as permission to fall through.
pub trait WorktreeProvider {
    fn resolve(&self, handle: &str) -> Result<WorktreeProviderLookup>;
}

/// Canonical local target admitted for a provider-owned mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMutationTarget {
    pub provider: WorktreeProviderIdentity,
    pub handle: String,
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeMutationLookup {
    Found(WorktreeMutationTarget),
    NotFound,
}

/// Mutable safety exceptions supplied by the lifecycle that owns the mutation.
#[derive(Debug, Clone, Copy, Default)]
pub struct WorktreeMutationContext<'a> {
    pub safety_baseline: Option<&'a serde_json::Value>,
    pub trusted_unpushed_destination: Option<&'a worktree_providers::TrustedUnpushedWorktree>,
}

/// Optional lifecycle capability for resolving and revalidating a local
/// mutation target. Implementations retain authority over identity and safety.
pub trait WorktreeMutationProvider {
    fn resolve_for_mutation(
        &self,
        reference: &str,
        context: WorktreeMutationContext<'_>,
    ) -> Result<WorktreeMutationLookup>;
}

/// Exact creation request shared by native and configured worktree providers.
pub type WorktreeProvisionIntent = worktree_providers::WorktreeProviderCreateIntent;
/// Lifecycle ownership bound before a provisioning mutation is allowed.
pub type WorktreeProvisionLifecycle = worktree_providers::WorktreeProviderLifecycleIntent;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProvisionDestination {
    pub ownership: WorktreeOwnership,
    /// Configured providers may issue an opaque exact identity. Native identity
    /// remains in the task-worktree registry and is not projected into this slot.
    pub exact_identity: Option<worktree_providers::WorktreeProviderExactIdentity>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeProvisionLookup {
    Admitted(WorktreeProvisionDestination),
    NotFound,
}

impl WorktreeProvisionLookup {
    pub fn into_admitted(self, handle: &str) -> Result<WorktreeProvisionDestination> {
        match self {
            Self::Admitted(destination) => Ok(destination),
            Self::NotFound => Err(Error::validation_invalid_argument(
                "to_worktree",
                format!("worktree handle `{handle}` is no longer admitted after provisioning"),
                Some(handle.to_string()),
                None,
            )),
        }
    }
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

/// Optional capability for admitting, planning, and ensuring a destination.
/// Planning is read-only. Callers must durably bind lifecycle ownership before
/// invoking `ensure`, and must re-admit its postcondition before use.
pub trait WorktreeProvisionProvider {
    fn admit(
        &self,
        handle: &str,
        selected_provider: Option<&WorktreeProviderIdentity>,
    ) -> Result<WorktreeProvisionLookup>;

    fn plan(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvisionPlan>;

    fn ensure(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvision>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeFinalizationLookup {
    Finalized(worktree_providers::WorktreeProviderFinalization),
    Unsupported,
    NotFound,
}

/// Optional terminal lifecycle capability. Finalization is idempotent and only
/// records cleanup disposition; deletion remains a separately authorized step.
pub trait WorktreeFinalizationProvider {
    fn finalize(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: worktree_providers::WorktreeProviderTerminalDisposition,
    ) -> Result<WorktreeFinalizationLookup>;
}

/// Built-in provider for Homeboy's standalone task-worktree registry.
///
/// This intentionally excludes adopted workspace refs because the first
/// migrated consumer historically admitted task worktrees only.
pub struct NativeWorktreeProvider;

impl WorktreeProvider for NativeWorktreeProvider {
    fn resolve(&self, handle: &str) -> Result<WorktreeProviderLookup> {
        let Some(record) = worktree::resolve_if_present(handle)? else {
            return Ok(WorktreeProviderLookup::NotFound);
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
        if record.state == worktree::TaskWorktreeState::Removed {
            return Ok(WorktreeProviderLookup::NotFound);
        }
        if record.branch.trim().is_empty() {
            return Err(Error::validation_invalid_argument(
                "to_worktree",
                format!("native worktree `{handle}` has no branch"),
                Some(handle.to_string()),
                None,
            ));
        }
        let safety = worktree::safety_report_for_provider(&record)?;
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
        Ok(WorktreeProviderLookup::Found(WorktreeOwnership {
            provider: WorktreeProviderIdentity::Native,
            handle: record.id,
            path: record.worktree_path,
            branch: record.branch,
            task_url: record.task_url,
        }))
    }
}

impl WorktreeMutationProvider for NativeWorktreeProvider {
    fn resolve_for_mutation(
        &self,
        reference: &str,
        _context: WorktreeMutationContext<'_>,
    ) -> Result<WorktreeMutationLookup> {
        let Some(record) = worktree::resolve_workspace_ref_if_present(reference)? else {
            return Ok(WorktreeMutationLookup::NotFound);
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
        Ok(WorktreeMutationLookup::Found(WorktreeMutationTarget {
            provider: WorktreeProviderIdentity::Native,
            handle: record.handle().to_string(),
            path,
        }))
    }
}

impl WorktreeProvisionProvider for NativeWorktreeProvider {
    fn admit(
        &self,
        handle: &str,
        selected_provider: Option<&WorktreeProviderIdentity>,
    ) -> Result<WorktreeProvisionLookup> {
        if selected_provider.is_some_and(|provider| provider != &WorktreeProviderIdentity::Native) {
            return Ok(WorktreeProvisionLookup::NotFound);
        }
        Ok(match self.resolve(handle)? {
            WorktreeProviderLookup::Found(ownership) => {
                WorktreeProvisionLookup::Admitted(WorktreeProvisionDestination {
                    ownership,
                    exact_identity: None,
                })
            }
            WorktreeProviderLookup::NotFound => WorktreeProvisionLookup::NotFound,
        })
    }

    fn plan(
        &self,
        intent: &WorktreeProvisionIntent,
        _lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvisionPlan> {
        if let WorktreeProvisionLookup::Admitted(destination) = self.admit(&intent.handle, None)? {
            return Ok(WorktreeProvisionPlan::Admitted(destination));
        }
        validate_native_provision_handle(intent)?;
        Ok(WorktreeProvisionPlan::Planned(
            WorktreeProvisionDestination {
                ownership: WorktreeOwnership {
                    provider: WorktreeProviderIdentity::Native,
                    handle: intent.handle.clone(),
                    path: worktree::planned_create_path(&intent.repo, &intent.head, &intent.base)?,
                    branch: intent.head.clone(),
                    task_url: Some(intent.task_url.clone()),
                },
                exact_identity: None,
            },
        ))
    }

    fn ensure(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvision> {
        if let WorktreeProvisionLookup::Admitted(destination) = self.admit(&intent.handle, None)? {
            return Ok(WorktreeProvision {
                destination,
                action: WorktreeProvisionAction::Admitted,
                idempotency_key: worktree_providers::worktree_provider_idempotency_key(intent),
            });
        }
        validate_native_provision_handle(intent)?;
        let created = worktree::create(worktree::WorktreeCreateOptions {
            component_id: intent.repo.clone(),
            branch: intent.head.clone(),
            from: Some(intent.base.clone()),
            task_url: Some(intent.task_url.clone()),
            run_id: Some(lifecycle.owner_run_ref.clone()),
            cleanup_policy: Some(match lifecycle.cleanup_policy {
                worktree_providers::WorktreeProviderCleanupPolicy::RemoveOnSuccess => {
                    worktree::CleanupPolicy::RemoveWhenSafe
                }
                worktree_providers::WorktreeProviderCleanupPolicy::PreserveOnFailure => {
                    worktree::CleanupPolicy::PreserveOnFailure
                }
            }),
        })?;
        Ok(WorktreeProvision {
            destination: WorktreeProvisionDestination {
                ownership: WorktreeOwnership {
                    provider: WorktreeProviderIdentity::Native,
                    handle: created.record.id,
                    path: created.record.worktree_path,
                    branch: created.record.branch,
                    task_url: created.record.task_url,
                },
                exact_identity: None,
            },
            action: WorktreeProvisionAction::Ensured,
            idempotency_key: worktree_providers::worktree_provider_idempotency_key(intent),
        })
    }
}

impl WorktreeFinalizationProvider for NativeWorktreeProvider {
    fn finalize(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: worktree_providers::WorktreeProviderTerminalDisposition,
    ) -> Result<WorktreeFinalizationLookup> {
        let Some(record) = worktree::resolve_if_present(handle)? else {
            return Ok(WorktreeFinalizationLookup::NotFound);
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
        Ok(WorktreeFinalizationLookup::Finalized(
            worktree_providers::WorktreeProviderFinalization {
                provider_id: "native".to_string(),
                handle: record.id,
                disposition,
                owner_outcome: disposition.owner_outcome().to_string(),
                lifecycle_state: disposition.lifecycle_state().to_string(),
                inspection_path: record.worktree_path,
            },
        ))
    }
}

fn validate_native_provision_handle(intent: &WorktreeProvisionIntent) -> Result<()> {
    let expected = worktree::handle_for_branch(&intent.repo, &intent.head);
    if expected == intent.handle {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "native worktree creation for branch `{}` resolves to handle `{expected}`, not `{}`",
            intent.head, intent.handle
        ),
        Some(intent.handle.clone()),
        None,
    ))
}

/// Adapter for configured command-backed worktree providers.
pub struct CommandWorktreeProvider<'a> {
    config: &'a HomeboyConfig,
}

impl<'a> CommandWorktreeProvider<'a> {
    pub fn new(config: &'a HomeboyConfig) -> Self {
        Self { config }
    }
}

impl WorktreeProvider for CommandWorktreeProvider<'_> {
    fn resolve(&self, handle: &str) -> Result<WorktreeProviderLookup> {
        match worktree_providers::resolve_worktree_provider_from_config(handle, self.config) {
            Ok(resolution) => Ok(WorktreeProviderLookup::Found(WorktreeOwnership {
                provider: WorktreeProviderIdentity::Configured(resolution.provider_id),
                handle: resolution.worktree.handle,
                path: resolution.worktree.path,
                branch: resolution.worktree.branch,
                task_url: resolution.worktree.task_url,
            })),
            Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => {
                Ok(WorktreeProviderLookup::NotFound)
            }
            Err(error) => Err(error),
        }
    }
}

impl WorktreeMutationProvider for CommandWorktreeProvider<'_> {
    fn resolve_for_mutation(
        &self,
        reference: &str,
        context: WorktreeMutationContext<'_>,
    ) -> Result<WorktreeMutationLookup> {
        let resolution = if Path::new(reference).is_dir() {
            worktree_providers::resolve_apply_enabled_worktree_provider_path_from_config(
                Path::new(reference),
                self.config,
                context.safety_baseline,
                context.trusted_unpushed_destination,
            )?
        } else {
            match worktree_providers::resolve_apply_enabled_worktree_provider_with_trusted_unpushed_destination_from_config(
                    reference,
                    self.config,
                    context.safety_baseline,
                    context.trusted_unpushed_destination,
                ) {
                Ok(resolution) => Some(resolution),
                Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => None,
                Err(error) => return Err(error),
            }
        };
        Ok(match resolution {
            Some(resolution) => WorktreeMutationLookup::Found(WorktreeMutationTarget {
                provider: WorktreeProviderIdentity::Configured(resolution.provider_id),
                handle: resolution.worktree.handle,
                path: PathBuf::from(resolution.worktree.path),
            }),
            None => WorktreeMutationLookup::NotFound,
        })
    }
}

impl WorktreeProvisionProvider for CommandWorktreeProvider<'_> {
    fn admit(
        &self,
        handle: &str,
        selected_provider: Option<&WorktreeProviderIdentity>,
    ) -> Result<WorktreeProvisionLookup> {
        let identity = match selected_provider {
            Some(WorktreeProviderIdentity::Native) => return Ok(WorktreeProvisionLookup::NotFound),
            Some(WorktreeProviderIdentity::Configured(provider_id)) => {
                worktree_providers::resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
                    handle,
                    provider_id,
                    self.config,
                )
            }
            None => {
                worktree_providers::resolve_apply_enabled_worktree_provider_identity_from_config(
                    handle,
                    self.config,
                )
            }
        };
        match identity {
            Ok(identity) => Ok(WorktreeProvisionLookup::Admitted(
                WorktreeProvisionDestination {
                    ownership: WorktreeOwnership {
                        provider: WorktreeProviderIdentity::Configured(
                            identity.provider_id.clone(),
                        ),
                        handle: identity.handle.clone(),
                        path: identity.path.clone(),
                        branch: identity.branch.clone(),
                        task_url: None,
                    },
                    exact_identity: Some(identity),
                },
            )),
            Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => {
                Ok(WorktreeProvisionLookup::NotFound)
            }
            Err(error) => Err(error),
        }
    }

    fn plan(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvisionPlan> {
        if let WorktreeProvisionLookup::Admitted(destination) = self.admit(&intent.handle, None)? {
            return Ok(WorktreeProvisionPlan::Admitted(destination));
        }
        let plan =
            worktree_providers::plan_apply_enabled_worktree_provider_with_lifecycle_from_config(
                intent,
                lifecycle,
                self.config,
            )?;
        let (planned, resolution) = match plan {
            worktree_providers::WorktreeProviderCreatePlan::Existing(resolution) => {
                (false, resolution)
            }
            worktree_providers::WorktreeProviderCreatePlan::WouldCreate(resolution) => {
                (true, resolution)
            }
        };
        let destination = command_provision_destination(resolution);
        Ok(if planned {
            WorktreeProvisionPlan::Planned(destination)
        } else {
            WorktreeProvisionPlan::Admitted(destination)
        })
    }

    fn ensure(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvision> {
        let provision = worktree_providers::provision_apply_enabled_worktree_provider_with_lifecycle_from_config(
            intent,
            lifecycle,
            self.config,
        )?;
        Ok(WorktreeProvision {
            destination: command_provision_destination(provision.resolution),
            action: if provision.action == "ensured" {
                WorktreeProvisionAction::Ensured
            } else {
                WorktreeProvisionAction::Admitted
            },
            idempotency_key: provision.idempotency_key,
        })
    }
}

impl WorktreeFinalizationProvider for CommandWorktreeProvider<'_> {
    fn finalize(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: worktree_providers::WorktreeProviderTerminalDisposition,
    ) -> Result<WorktreeFinalizationLookup> {
        let resolution =
            match worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
                handle,
                self.config,
                None,
            ) {
                Ok(resolution) => resolution,
                Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => {
                    return Ok(WorktreeFinalizationLookup::NotFound);
                }
                Err(error) => return Err(error),
            };
        if worktree_providers::worktree_provider_lifecycle_finalizer_argv_from_config(
            &resolution.provider_id,
            self.config,
        )?
        .is_none()
        {
            return Ok(WorktreeFinalizationLookup::Unsupported);
        }
        Ok(WorktreeFinalizationLookup::Finalized(
            worktree_providers::finalize_apply_enabled_worktree_provider_from_config(
                &resolution,
                lifecycle,
                disposition,
                self.config,
            )?,
        ))
    }
}

fn command_provision_destination(
    resolution: worktree_providers::WorktreeProviderResolution,
) -> WorktreeProvisionDestination {
    WorktreeProvisionDestination {
        ownership: WorktreeOwnership {
            provider: WorktreeProviderIdentity::Configured(resolution.provider_id),
            handle: resolution.worktree.handle,
            path: resolution.worktree.path,
            branch: resolution.worktree.branch,
            task_url: resolution.worktree.task_url,
        },
        exact_identity: None,
    }
}

/// Resolve through the single ordered provider boundary used by consumers.
pub fn resolve_worktree_ownership(handle: &str) -> Result<WorktreeOwnership> {
    resolve_worktree_ownership_from_config(handle, &defaults::load_config())
}

pub fn resolve_worktree_ownership_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeOwnership> {
    let native = NativeWorktreeProvider;
    if let WorktreeProviderLookup::Found(ownership) = native.resolve(handle)? {
        return Ok(ownership);
    }

    let command = CommandWorktreeProvider::new(config);
    if let WorktreeProviderLookup::Found(ownership) = command.resolve(handle)? {
        return Ok(ownership);
    }

    Err(worktree_providers::worktree_provider_not_found_error(
        handle, config, false,
    ))
}

/// Resolve a local mutation target through native ownership first, then the
/// configured command providers. Provider errors are authoritative and never
/// permit fallback.
pub fn resolve_worktree_mutation_target_from_config(
    reference: &str,
    config: &HomeboyConfig,
    context: WorktreeMutationContext<'_>,
) -> Result<WorktreeMutationTarget> {
    let native = NativeWorktreeProvider;
    if let WorktreeMutationLookup::Found(target) =
        native.resolve_for_mutation(reference, context)?
    {
        return Ok(target);
    }

    let command = CommandWorktreeProvider::new(config);
    if let WorktreeMutationLookup::Found(target) =
        command.resolve_for_mutation(reference, context)?
    {
        return Ok(target);
    }

    if Path::new(reference).is_dir() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "configured worktree providers do not own explicit destination path `{reference}`"
            ),
            Some(reference.to_string()),
            None,
        ));
    }
    Err(worktree_providers::worktree_provider_not_found_error(
        reference, config, true,
    ))
}

/// Admit an existing destination through native ownership first, then through
/// configured apply-enabled ownership. A selected provider is exact authority
/// for durable continuation and disables fallback.
pub fn admit_worktree_provision_from_config(
    handle: &str,
    selected_provider: Option<&WorktreeProviderIdentity>,
    config: &HomeboyConfig,
) -> Result<WorktreeProvisionLookup> {
    let native = NativeWorktreeProvider;
    if let WorktreeProvisionLookup::Admitted(destination) =
        native.admit(handle, selected_provider)?
    {
        return Ok(WorktreeProvisionLookup::Admitted(destination));
    }
    if selected_provider == Some(&WorktreeProviderIdentity::Native) {
        return Ok(WorktreeProvisionLookup::NotFound);
    }

    CommandWorktreeProvider::new(config).admit(handle, selected_provider)
}

/// Produce a non-mutating destination plan through the same provider selection
/// execution will use. Configured creation remains preferred when declared;
/// otherwise Homeboy's native task-worktree lifecycle is the provider.
pub fn plan_worktree_provision_from_config(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
    config: &HomeboyConfig,
) -> Result<WorktreeProvisionPlan> {
    if let WorktreeProvisionLookup::Admitted(destination) =
        admit_worktree_provision_from_config(&intent.handle, None, config)?
    {
        return Ok(WorktreeProvisionPlan::Admitted(destination));
    }
    if configured_provisioning_declared(config) {
        CommandWorktreeProvider::new(config).plan(intent, lifecycle)
    } else {
        NativeWorktreeProvider.plan(intent, lifecycle)
    }
}

/// Ensure an absent destination through its selected lifecycle provider. This
/// method does not admit the postcondition; callers must invoke `admit` again.
pub fn ensure_worktree_provision_from_config(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
    selected_provider: Option<&WorktreeProviderIdentity>,
    config: &HomeboyConfig,
) -> Result<WorktreeProvision> {
    match selected_provider {
        Some(WorktreeProviderIdentity::Native) => NativeWorktreeProvider.ensure(intent, lifecycle),
        Some(WorktreeProviderIdentity::Configured(_)) => {
            CommandWorktreeProvider::new(config).ensure(intent, lifecycle)
        }
        None if configured_provisioning_declared(config) => {
            CommandWorktreeProvider::new(config).ensure(intent, lifecycle)
        }
        None => NativeWorktreeProvider.ensure(intent, lifecycle),
    }
}

/// Finalize through native ownership first, then configured ownership. Absence
/// and unsupported optional finalization are explicit non-error outcomes.
pub fn finalize_worktree_from_config(
    handle: &str,
    lifecycle: &WorktreeProvisionLifecycle,
    disposition: worktree_providers::WorktreeProviderTerminalDisposition,
    config: &HomeboyConfig,
) -> Result<WorktreeFinalizationLookup> {
    let native = NativeWorktreeProvider;
    match native.finalize(handle, lifecycle, disposition)? {
        WorktreeFinalizationLookup::NotFound => {}
        outcome => return Ok(outcome),
    }
    CommandWorktreeProvider::new(config).finalize(handle, lifecycle, disposition)
}

pub fn worktree_finalization_not_found_error(handle: &str, config: &HomeboyConfig) -> Error {
    worktree_providers::worktree_provider_not_found_error(handle, config, true)
}

fn configured_provisioning_declared(config: &HomeboyConfig) -> bool {
    config.worktree_providers.values().any(|provider| {
        provider.enabled && provider.apply_enabled && provider.commands.ensure.is_some()
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::Path;

    use super::*;
    use crate::defaults::{
        WorktreeProviderCommands, WorktreeProviderConfig, WorktreeProviderKind,
        WorktreeProviderListResultMapping,
    };

    fn assert_lookup_conformance(
        provider: &dyn WorktreeProvider,
        handle: &str,
        expected_provider: WorktreeProviderIdentity,
        expected_path: &Path,
    ) {
        let WorktreeProviderLookup::Found(ownership) =
            provider.resolve(handle).expect("owned handle resolves")
        else {
            panic!("owned handle was not found");
        };
        assert_eq!(ownership.provider, expected_provider);
        assert_eq!(ownership.handle, handle);
        assert_eq!(Path::new(&ownership.path), expected_path);
        assert!(matches!(
            provider
                .resolve("missing@worktree")
                .expect("missing lookup"),
            WorktreeProviderLookup::NotFound
        ));
    }

    fn assert_unsafe_lookup(provider: &dyn WorktreeProvider, handle: &str) {
        provider
            .resolve(handle)
            .expect_err("unsafe owned handle must fail instead of falling through");
    }

    fn assert_mutation_conformance(
        provider: &dyn WorktreeMutationProvider,
        handle: &str,
        expected_provider: WorktreeProviderIdentity,
        expected_path: &Path,
    ) {
        let resolve = || {
            let WorktreeMutationLookup::Found(target) = provider
                .resolve_for_mutation(handle, WorktreeMutationContext::default())
                .expect("owned mutation target resolves")
            else {
                panic!("owned mutation target was not found");
            };
            target
        };
        let admitted = resolve();
        let revalidated = resolve();
        assert_eq!(admitted, revalidated, "mutation identity must remain exact");
        assert_eq!(admitted.provider, expected_provider);
        assert_eq!(admitted.handle, handle);
        assert_eq!(admitted.path, expected_path);
        assert!(matches!(
            provider
                .resolve_for_mutation("missing@worktree", WorktreeMutationContext::default())
                .expect("missing mutation lookup"),
            WorktreeMutationLookup::NotFound
        ));
    }

    fn assert_provision_admission_conformance(
        provider: &dyn WorktreeProvisionProvider,
        handle: &str,
        expected_provider: WorktreeProviderIdentity,
        expected_path: &Path,
    ) {
        let admit = || {
            provider
                .admit(handle, None)
                .expect("owned provision destination admits")
                .into_admitted(handle)
                .expect("owned destination")
        };
        let admitted = admit();
        let revalidated = admit();
        assert_eq!(
            admitted.ownership, revalidated.ownership,
            "admission ownership must remain exact"
        );
        assert_eq!(
            admitted.exact_identity.as_ref().map(|identity| (
                &identity.provider_id,
                &identity.token,
                &identity.handle,
                &identity.path,
                &identity.branch,
                identity.primary,
            )),
            revalidated.exact_identity.as_ref().map(|identity| (
                &identity.provider_id,
                &identity.token,
                &identity.handle,
                &identity.path,
                &identity.branch,
                identity.primary,
            )),
            "provider-issued exact identity must remain stable"
        );
        assert_eq!(admitted.ownership.provider, expected_provider);
        assert_eq!(admitted.ownership.handle, handle);
        assert_eq!(Path::new(&admitted.ownership.path), expected_path);
        assert!(matches!(
            provider
                .admit("missing@worktree", None)
                .expect("missing provision admission"),
            WorktreeProvisionLookup::NotFound
        ));
    }

    fn initialize_native_worktree(home: &Path) -> (tempfile::TempDir, std::path::PathBuf) {
        let source = tempfile::tempdir_in(home).expect("source checkout");
        let initialized = std::process::Command::new("git")
            .args(["init", "-b", "main"])
            .current_dir(source.path())
            .output()
            .expect("initialize source repository");
        assert!(initialized.status.success());
        std::fs::write(source.path().join("README"), "fixture\n").expect("source file");
        let added = std::process::Command::new("git")
            .args(["add", "README"])
            .current_dir(source.path())
            .output()
            .expect("stage source file");
        assert!(added.status.success());
        let committed = std::process::Command::new("git")
            .args([
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "fixture",
            ])
            .current_dir(source.path())
            .output()
            .expect("commit source file");
        assert!(committed.status.success());
        let components = home.join(".config/homeboy/components");
        std::fs::create_dir_all(&components).expect("component registry");
        std::fs::write(
            components.join("fixture.json"),
            serde_json::json!({
                "local_path": source.path(),
                "remote_path": "wp-content/plugins/fixture"
            })
            .to_string(),
        )
        .expect("component registration");
        let path = home.join("native-worktree");
        let created = std::process::Command::new("git")
            .args([
                "worktree",
                "add",
                "-b",
                "task/fixture-native",
                path.to_str().expect("UTF-8 worktree path"),
            ])
            .current_dir(source.path())
            .output()
            .expect("create native worktree");
        assert!(created.status.success());
        (source, path)
    }

    #[test]
    fn native_provider_conforms_to_shared_lookup_contract() {
        crate::test_support::with_isolated_home(|home| {
            let (source, path) = initialize_native_worktree(home.path());
            worktree::record_active_with_source_for_test("fixture@native", source.path(), &path);

            assert_lookup_conformance(
                &NativeWorktreeProvider,
                "fixture@native",
                WorktreeProviderIdentity::Native,
                &path,
            );
            assert_mutation_conformance(
                &NativeWorktreeProvider,
                "fixture@native",
                WorktreeProviderIdentity::Native,
                &path,
            );
            assert_provision_admission_conformance(
                &NativeWorktreeProvider,
                "fixture@native",
                WorktreeProviderIdentity::Native,
                &path,
            );
            let intent = WorktreeProvisionIntent {
                handle: "fixture@planned".to_string(),
                repo: "fixture".to_string(),
                base: "main".to_string(),
                head: "planned".to_string(),
                task_url: "https://example.test/issues/8017".to_string(),
            };
            let lifecycle = WorktreeProvisionLifecycle {
                purpose: "agent_task_cook".to_string(),
                owner_run_ref: "native-plan-run".to_string(),
                cleanup_policy: worktree_providers::WorktreeProviderCleanupPolicy::RemoveOnSuccess,
            };
            let WorktreeProvisionPlan::Planned(planned) = NativeWorktreeProvider
                .plan(&intent, &lifecycle)
                .expect("native plan")
            else {
                panic!("missing native destination must be planned");
            };
            assert!(!Path::new(&planned.ownership.path).exists());
            assert!(worktree::resolve_if_present(&intent.handle)
                .expect("preview registry lookup")
                .is_none());
            let ensured = NativeWorktreeProvider
                .ensure(&intent, &lifecycle)
                .expect("native ensure");
            assert_eq!(ensured.action, WorktreeProvisionAction::Ensured);
            assert_eq!(ensured.destination.ownership.path, planned.ownership.path);
            let replay = NativeWorktreeProvider
                .ensure(&intent, &lifecycle)
                .expect("native ensure replay");
            assert_eq!(replay.action, WorktreeProvisionAction::Admitted);
            assert_eq!(replay.idempotency_key, ensured.idempotency_key);
            let WorktreeFinalizationLookup::Finalized(finalized) = NativeWorktreeProvider
                .finalize(
                    &intent.handle,
                    &lifecycle,
                    worktree_providers::WorktreeProviderTerminalDisposition::Failed,
                )
                .expect("native finalization")
            else {
                panic!("native lifecycle must finalize");
            };
            assert_eq!(finalized.provider_id, "native");
            assert_eq!(finalized.owner_outcome, "failure");
            assert_eq!(finalized.lifecycle_state, "failed");
            let record = worktree::resolve_if_present(&intent.handle)
                .expect("native record lookup")
                .expect("native record");
            assert_eq!(
                record.cleanup_policy,
                worktree::CleanupPolicy::PreserveOnFailure
            );
            assert_eq!(record.terminal_disposition.as_deref(), Some("failed"));
            NativeWorktreeProvider
                .finalize(
                    &intent.handle,
                    &lifecycle,
                    worktree_providers::WorktreeProviderTerminalDisposition::Failed,
                )
                .expect("native finalization replay");
            NativeWorktreeProvider
                .finalize(
                    &intent.handle,
                    &lifecycle,
                    worktree_providers::WorktreeProviderTerminalDisposition::Succeeded,
                )
                .expect_err("terminal disposition cannot change");
            std::fs::write(path.join("dirty"), "dirty\n").expect("dirty native worktree");
            assert_unsafe_lookup(&NativeWorktreeProvider, "fixture@native");
        });
    }

    #[test]
    fn native_provider_treats_removed_records_as_not_found() {
        crate::test_support::with_isolated_home(|home| {
            let path = home.path().join("removed-worktree");
            worktree::record_removed_for_test("fixture@removed", &path);

            assert!(matches!(
                NativeWorktreeProvider
                    .resolve("fixture@removed")
                    .expect("removed lookup"),
                WorktreeProviderLookup::NotFound
            ));
        });
    }

    #[test]
    fn native_provider_rejects_colliding_manifest_identity() {
        crate::test_support::with_isolated_home(|home| {
            let path = home.path().join("colliding-worktree");
            worktree::record_removed_for_test("fixture@a/b", &path);

            let error = NativeWorktreeProvider
                .resolve("fixture@a?b")
                .expect_err("colliding handle must not resolve another manifest");
            assert!(error.message.contains("does not match requested handle"));
            let error = NativeWorktreeProvider
                .resolve_for_mutation("fixture@a?b", WorktreeMutationContext::default())
                .expect_err("colliding handle must not resolve another mutation target");
            assert!(error.message.contains("does not match requested handle"));
        });
    }

    #[test]
    fn command_provider_conforms_to_shared_lookup_contract() {
        crate::test_support::with_isolated_home(|_| {
            let workspace = tempfile::tempdir().expect("workspace");
            let initialized = std::process::Command::new("git")
                .args(["init", "-b", "command-branch"])
                .current_dir(workspace.path())
                .output()
                .expect("initialize git repository");
            assert!(initialized.status.success());

            let provider_dir = tempfile::tempdir().expect("provider directory");
            let script = provider_dir.path().join("provider");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\nprintf '%s\\n' '{}'\n",
                    serde_json::json!({
                        "worktrees": [{
                            "handle": "fixture@command",
                            "path": workspace.path(),
                            "branch": "command-branch",
                            "safety": { "dirty": false, "unpushed": false, "primary": false }
                        }, {
                            "handle": "fixture@unsafe",
                            "path": workspace.path(),
                            "branch": "command-branch",
                            "safety": { "dirty": true, "unpushed": false, "primary": false }
                        }]
                    })
                ),
            )
            .expect("provider script");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mut permissions = std::fs::metadata(&script)
                    .expect("provider metadata")
                    .permissions();
                permissions.set_mode(0o755);
                std::fs::set_permissions(&script, permissions).expect("executable provider");
            }

            let mut providers = HashMap::new();
            providers.insert(
                "command-fixture".to_string(),
                WorktreeProviderConfig {
                    enabled: true,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: true,
                    commands: WorktreeProviderCommands {
                        list: Some(vec![script.display().to_string()]),
                        ..Default::default()
                    },
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    list_result_mapping: Some(WorktreeProviderListResultMapping {
                        items: "$.worktrees".to_string(),
                        handle: "$.handle".to_string(),
                        path: "$.path".to_string(),
                        branch: "$.branch".to_string(),
                        dirty: "$.safety.dirty".to_string(),
                        unpushed: "$.safety.unpushed".to_string(),
                        primary: "$.safety.primary".to_string(),
                        task_url: None,
                    }),
                },
            );
            let config = HomeboyConfig {
                worktree_providers: providers,
                ..HomeboyConfig::default()
            };

            assert_lookup_conformance(
                &CommandWorktreeProvider::new(&config),
                "fixture@command",
                WorktreeProviderIdentity::Configured("command-fixture".to_string()),
                workspace.path(),
            );
            assert_mutation_conformance(
                &CommandWorktreeProvider::new(&config),
                "fixture@command",
                WorktreeProviderIdentity::Configured("command-fixture".to_string()),
                workspace.path(),
            );
            assert_provision_admission_conformance(
                &CommandWorktreeProvider::new(&config),
                "fixture@command",
                WorktreeProviderIdentity::Configured("command-fixture".to_string()),
                workspace.path(),
            );
            assert_unsafe_lookup(&CommandWorktreeProvider::new(&config), "fixture@unsafe");
        });
    }

    #[test]
    fn declared_command_provisioning_fails_closed_instead_of_falling_back_to_native() {
        crate::test_support::with_isolated_home(|_| {
            let mut providers = HashMap::new();
            providers.insert(
                "incomplete-command".to_string(),
                WorktreeProviderConfig {
                    enabled: true,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: true,
                    commands: WorktreeProviderCommands {
                        ensure: Some(vec!["true".to_string()]),
                        ..Default::default()
                    },
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    list_result_mapping: None,
                },
            );
            let config = HomeboyConfig {
                worktree_providers: providers,
                ..HomeboyConfig::default()
            };
            let intent = WorktreeProvisionIntent {
                handle: "fixture@planned".to_string(),
                repo: "fixture".to_string(),
                base: "main".to_string(),
                head: "planned".to_string(),
                task_url: "https://example.test/issues/8017".to_string(),
            };
            let lifecycle = WorktreeProvisionLifecycle {
                purpose: "agent_task_cook".to_string(),
                owner_run_ref: "command-plan-run".to_string(),
                cleanup_policy: worktree_providers::WorktreeProviderCleanupPolicy::RemoveOnSuccess,
            };

            let error = plan_worktree_provision_from_config(&intent, &lifecycle, &config)
                .expect_err("declared command ownership must remain authoritative");
            assert_eq!(
                error.details["worktree_provider_missing_required_capabilities"],
                serde_json::json!(["resolve_or_list"])
            );
            assert!(worktree::resolve_if_present(&intent.handle)
                .expect("native registry lookup")
                .is_none());
        });
    }
}
