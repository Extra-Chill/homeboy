use crate::defaults::{self, HomeboyConfig};
use crate::error::{Error, Result};
use crate::{worktree, worktree_providers};
use std::collections::BTreeMap;
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
    pub kind: WorktreeWorkspaceKind,
    pub branch: Option<String>,
    pub task_url: Option<String>,
    pub provenance: Option<serde_json::Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeWorkspaceKind {
    TaskWorktree,
    AdoptedWorkspace,
    Configured,
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

pub trait WorktreePathProvider {
    fn resolve_path(&self, path: &Path) -> Result<Option<WorktreeMutationTarget>>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderSafety {
    pub dirty: bool,
    pub unpushed: bool,
    pub primary: bool,
    pub missing: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProviderWorkspace {
    pub ownership: WorktreeOwnership,
    pub repository: Option<String>,
    pub owner_run_ref: Option<String>,
    pub created_at: Option<String>,
    pub terminal_disposition: Option<String>,
    pub safety: WorktreeProviderSafety,
}

#[derive(Debug, Clone)]
pub struct WorktreeListReport {
    pub native: worktree::WorktreeListOutput,
    pub provider_worktrees: Vec<WorktreeProviderWorkspace>,
}

#[derive(Debug, Clone)]
pub enum WorktreeStatusEvidence {
    Native(worktree::WorktreeStatusOutput),
    Provider(WorktreeProviderWorkspace),
}

pub trait WorktreeInventoryProvider {
    fn list(&self) -> Result<Vec<WorktreeProviderWorkspace>>;

    fn observe(&self, handle: &str) -> Result<Option<WorktreeProviderWorkspace>> {
        Ok(self
            .list()?
            .into_iter()
            .find(|workspace| workspace.ownership.handle == handle))
    }
}

#[derive(Debug, Clone)]
pub struct WorktreeCleanupRequest {
    pub scope: WorktreeCleanupScope,
    pub providers: Vec<String>,
    pub all_configured_providers: bool,
    pub apply: bool,
    pub force: bool,
    pub cleanup_branches: bool,
    pub allow_unmerged_branches: bool,
    pub timeout: Option<std::time::Duration>,
    pub provider_run_id: Option<String>,
    pub provider_plan_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeCleanupScope {
    All,
    Native,
    Configured,
}

impl Default for WorktreeCleanupRequest {
    fn default() -> Self {
        Self {
            scope: WorktreeCleanupScope::Configured,
            providers: Vec::new(),
            all_configured_providers: true,
            apply: false,
            force: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
            timeout: None,
            provider_run_id: None,
            provider_plan_id: None,
        }
    }
}

#[derive(Debug, Clone)]
pub enum WorktreeCleanupEvidence {
    Native(worktree::WorktreeCleanupOutput),
    Configured(worktree_providers::WorktreeProviderCleanupOutput),
}

#[derive(Debug, Clone, Default)]
pub struct WorktreeCleanupReport {
    pub native: Option<worktree::WorktreeCleanupOutput>,
    pub configured: Option<worktree_providers::WorktreeProviderCleanupOutput>,
}

/// Cleanup capability with one request contract. Provider-specific evidence is
/// retained as an output projection without leaking provider selection to callers.
pub trait WorktreeCleanupProvider {
    fn cleanup(&self, request: &WorktreeCleanupRequest) -> Result<WorktreeCleanupEvidence>;
}

/// Canonical local target admitted for a provider-owned mutation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMutationTarget {
    pub provider: WorktreeProviderIdentity,
    pub handle: String,
    pub path: PathBuf,
    pub source_kind: Option<String>,
    pub branch: Option<String>,
    pub task_url: Option<String>,
    pub safety: Option<WorktreeProviderSafety>,
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
    pub trusted_unpushed_destination: Option<&'a WorktreeTrustedUnpushedDestination>,
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

pub type WorktreeExactIdentity = worktree_providers::WorktreeProviderExactIdentity;
pub type WorktreeSafetyAttestation = worktree_providers::WorktreeProviderSafetyAttestation;
pub type WorktreeConvergence = worktree_providers::WorktreeProviderConvergence;
pub type WorktreeTaskAttachment = worktree_providers::WorktreeProviderTaskAttachment;
pub type WorktreeTaskAttachmentStatus = worktree_providers::WorktreeProviderTaskAttachmentStatus;
pub type WorktreeSelfRepairContract = worktree_providers::WorktreeProviderSelfRepairContract;
pub type WorktreeCommandControl = worktree_providers::WorktreeProviderCommandControl;
pub type WorktreeTrustedUnpushedDestination = worktree_providers::TrustedUnpushedWorktree;
pub type ConfiguredWorktreeCleanupOutput = worktree_providers::WorktreeProviderCleanupOutput;
pub type WorktreeCleanupEffects = worktree_providers::WorktreeProviderCleanupEffects;

/// Optional opaque identity and safety-attestation capability. Exact provider
/// identity is deliberately separate from mutable safety evidence so durable
/// continuations can pin authority before revalidating current state.
pub trait WorktreeIdentityProvider {
    fn resolve_exact_identity(
        &self,
        handle: &str,
        selected_provider: Option<&str>,
    ) -> Result<WorktreeExactIdentity>;

    fn resolve_exact_identity_by_path(&self, path: &Path) -> Result<Option<WorktreeExactIdentity>>;

    fn attest_safety(&self, identity: &WorktreeExactIdentity) -> Result<WorktreeSafetyAttestation>;
}

/// Optional provider-owned preparation mutations used before task execution.
/// Every operation preserves provider identity and revalidates its postcondition.
pub trait WorktreePreparationProvider {
    fn converge_to_base(&self, handle: &str, base_sha: &str) -> Result<WorktreeConvergence>;

    fn materialize(&self, identity: &WorktreeExactIdentity) -> Result<WorktreeExactIdentity>;

    fn preview_task_attachment(
        &self,
        handle: &str,
        task_url: &str,
    ) -> Result<Option<WorktreeTaskAttachment>>;

    fn apply_task_attachment(
        &self,
        assessment: &WorktreeTaskAttachment,
    ) -> Result<WorktreeTaskAttachment>;
}

/// Optional discovery and configured ownership metadata capability.
pub trait WorktreeDiscoveryProvider {
    fn find_by_task(
        &self,
        task_url: &str,
        head: Option<&str>,
    ) -> Result<Option<WorktreeMutationTarget>>;

    fn self_repair_contract(&self, provider_id: &str)
        -> Result<Option<WorktreeSelfRepairContract>>;
}

/// Exact creation request shared by native and configured worktree providers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProvisionIntent {
    pub handle: String,
    pub repo: String,
    pub base: String,
    pub head: String,
    pub task_url: Option<String>,
}

/// Lifecycle ownership bound before a provisioning mutation is allowed.
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

impl WorktreeCleanupPolicy {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RemoveOnSuccess => "remove_on_success",
            Self::PreserveOnFailure => "preserve_on_failure",
        }
    }
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

    pub fn owner_outcome(self) -> &'static str {
        match self {
            Self::Succeeded => "success",
            Self::Failed | Self::Cancelled | Self::TimedOut | Self::Interrupted => "failure",
        }
    }

    pub fn lifecycle_state(self) -> &'static str {
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
    pub provider_id: String,
    pub handle: String,
    pub disposition: WorktreeTerminalDisposition,
    pub owner_outcome: String,
    pub lifecycle_state: String,
    pub inspection_path: String,
}

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

#[derive(Debug, Clone)]
pub enum WorktreeProviderCreateOutput {
    Native(worktree::WorktreeCreateOutput),
    Configured(WorktreeProvision),
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ConfiguredWorktreeCreateEvidence {
    pub provider: String,
    pub handle: String,
    pub path: String,
    pub branch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub task_url: Option<String>,
    pub provision_action: &'static str,
    pub idempotency_key: String,
}

impl From<WorktreeProvision> for ConfiguredWorktreeCreateEvidence {
    fn from(provision: WorktreeProvision) -> Self {
        let provider = match provision.destination.ownership.provider {
            WorktreeProviderIdentity::Native => "native".to_string(),
            WorktreeProviderIdentity::Configured(provider) => provider,
        };
        Self {
            provider,
            handle: provision.destination.ownership.handle,
            path: provision.destination.ownership.path,
            branch: provision
                .destination
                .ownership
                .branch
                .expect("configured worktree creation has a branch"),
            task_url: provision.destination.ownership.task_url,
            provision_action: match provision.action {
                WorktreeProvisionAction::Admitted => "admitted",
                WorktreeProvisionAction::Ensured => "ensured",
            },
            idempotency_key: provision.idempotency_key,
        }
    }
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
    Finalized(WorktreeFinalization),
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
        disposition: WorktreeTerminalDisposition,
    ) -> Result<WorktreeFinalizationLookup>;
}

/// Complete lifecycle contract implemented by every provider. Optional remote
/// preparation and discovery capabilities remain separate because the built-in
/// local provider does not need them.
pub trait WorktreeLifecycleProvider:
    WorktreeProvider
    + WorktreeInventoryProvider
    + WorktreeCleanupProvider
    + WorktreeMutationProvider
    + WorktreeProvisionProvider
    + WorktreeFinalizationProvider
{
}

impl<T> WorktreeLifecycleProvider for T where
    T: WorktreeProvider
        + WorktreeInventoryProvider
        + WorktreeCleanupProvider
        + WorktreeMutationProvider
        + WorktreeProvisionProvider
        + WorktreeFinalizationProvider
{
}

/// Built-in provider for Homeboy's standalone task and adopted-workspace registries.
pub struct NativeWorktreeProvider;

impl WorktreeProvider for NativeWorktreeProvider {
    fn resolve(&self, handle: &str) -> Result<WorktreeProviderLookup> {
        let Some(record) = worktree::resolve_workspace_ref_if_present(handle)? else {
            return Ok(WorktreeProviderLookup::NotFound);
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
            return Ok(WorktreeProviderLookup::NotFound);
        }
        let path = PathBuf::from(record.path());
        let (kind, branch, task_url, provenance) = match &record {
            worktree::WorkspaceRefRecord::Task(record) => {
                if record.branch.trim().is_empty() {
                    return Err(Error::validation_invalid_argument(
                        "to_worktree",
                        format!("native worktree `{handle}` has no branch"),
                        Some(handle.to_string()),
                        None,
                    ));
                }
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
        Ok(WorktreeProviderLookup::Found(WorktreeOwnership {
            provider: WorktreeProviderIdentity::Native,
            handle: record.handle().to_string(),
            path: path.display().to_string(),
            kind,
            branch,
            task_url,
            provenance,
        }))
    }
}

impl WorktreeInventoryProvider for NativeWorktreeProvider {
    fn list(&self) -> Result<Vec<WorktreeProviderWorkspace>> {
        worktree::list_workspace_refs()?
            .into_iter()
            .filter(|record| record.state() == &worktree::TaskWorktreeState::Active)
            .map(|record| Self::workspace_from_record(&record))
            .collect()
    }
}

impl NativeWorktreeProvider {
    fn workspaces_from_records(
        records: impl IntoIterator<Item = worktree::WorkspaceRefRecord>,
    ) -> (
        Vec<WorktreeProviderWorkspace>,
        Vec<worktree::WorktreeListDiagnostic>,
    ) {
        let mut workspaces = Vec::new();
        let mut diagnostics = Vec::new();
        for record in records {
            if record.state() != &worktree::TaskWorktreeState::Active {
                continue;
            }
            match Self::workspace_from_record(&record) {
                Ok(workspace) => workspaces.push(workspace),
                Err(error) => diagnostics.push(worktree::WorktreeListDiagnostic::from_error(
                    error,
                    Some(record.handle().to_string()),
                    Some(record.path().to_string()),
                )),
            }
        }
        (workspaces, diagnostics)
    }

    fn workspace_from_record(
        record: &worktree::WorkspaceRefRecord,
    ) -> Result<WorktreeProviderWorkspace> {
        let (kind, branch, task_url, repository, owner_run_ref, terminal_disposition, safety) =
            match record {
                worktree::WorkspaceRefRecord::Task(record) => {
                    let safety = worktree::safety_report_for_provider(record)?;
                    (
                        WorktreeWorkspaceKind::TaskWorktree,
                        Some(record.branch.clone()),
                        record.task_url.clone(),
                        Some(record.component_id.clone()),
                        record.run_id.clone(),
                        record.terminal_disposition.clone(),
                        WorktreeProviderSafety {
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
                    WorktreeProviderSafety {
                        dirty: false,
                        unpushed: false,
                        primary: false,
                        missing: !Path::new(&record.path).is_dir(),
                    },
                ),
            };
        Ok(WorktreeProviderWorkspace {
            ownership: WorktreeOwnership {
                provider: WorktreeProviderIdentity::Native,
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
                worktree::WorkspaceRefRecord::Task(record) => record.created_at.clone(),
                worktree::WorkspaceRefRecord::Adopted(record) => record.created_at.clone(),
            }),
            terminal_disposition,
            safety,
        })
    }
}

impl WorktreeCleanupProvider for NativeWorktreeProvider {
    fn cleanup(&self, request: &WorktreeCleanupRequest) -> Result<WorktreeCleanupEvidence> {
        worktree::cleanup(worktree::WorktreeCleanupOptions {
            force: request.force,
            dry_run: !request.apply,
            cleanup_branches: request.cleanup_branches,
            allow_unmerged_branches: request.allow_unmerged_branches,
        })
        .map(WorktreeCleanupEvidence::Native)
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
            source_kind: Some(record.source_kind().to_string()),
            branch: match &record {
                worktree::WorkspaceRefRecord::Task(record) => Some(record.branch.clone()),
                worktree::WorkspaceRefRecord::Adopted(_) => None,
            },
            task_url: match &record {
                worktree::WorkspaceRefRecord::Task(record) => record.task_url.clone(),
                worktree::WorkspaceRefRecord::Adopted(_) => None,
            },
            safety: None,
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
        let intent = native_provision_intent(intent);
        if let WorktreeProvisionLookup::Admitted(destination) = self.admit(&intent.handle, None)? {
            return Ok(WorktreeProvisionPlan::Admitted(destination));
        }
        Ok(WorktreeProvisionPlan::Planned(
            WorktreeProvisionDestination {
                ownership: WorktreeOwnership {
                    provider: WorktreeProviderIdentity::Native,
                    handle: intent.handle.clone(),
                    path: worktree::planned_create_path(&intent.repo, &intent.head, &intent.base)?,
                    kind: WorktreeWorkspaceKind::TaskWorktree,
                    branch: Some(intent.head.clone()),
                    task_url: intent.task_url.clone(),
                    provenance: None,
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
        let intent = native_provision_intent(intent);
        if let WorktreeProvisionLookup::Admitted(destination) = self.admit(&intent.handle, None)? {
            return Ok(WorktreeProvision {
                destination,
                action: WorktreeProvisionAction::Admitted,
                idempotency_key: worktree_providers::worktree_provider_idempotency_key(&intent),
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
            require_handoff_freshness: false,
        })?;
        Ok(WorktreeProvision {
            destination: WorktreeProvisionDestination {
                ownership: WorktreeOwnership {
                    provider: WorktreeProviderIdentity::Native,
                    handle: created.record.id,
                    path: created.record.worktree_path,
                    kind: WorktreeWorkspaceKind::TaskWorktree,
                    branch: Some(created.record.branch),
                    task_url: created.record.task_url,
                    provenance: None,
                },
                exact_identity: None,
            },
            action: WorktreeProvisionAction::Ensured,
            idempotency_key: worktree_providers::worktree_provider_idempotency_key(&intent),
        })
    }
}

impl WorktreeFinalizationProvider for NativeWorktreeProvider {
    fn finalize(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: WorktreeTerminalDisposition,
    ) -> Result<WorktreeFinalizationLookup> {
        let Some(workspace) = worktree::resolve_workspace_ref_if_present(handle)? else {
            return Ok(WorktreeFinalizationLookup::NotFound);
        };
        let worktree::WorkspaceRefRecord::Task(record) = workspace else {
            return Ok(WorktreeFinalizationLookup::Unsupported);
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
            WorktreeFinalization {
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

fn native_provision_intent(intent: &WorktreeProvisionIntent) -> WorktreeProvisionIntent {
    let mut intent = intent.clone();
    intent.handle = worktree::handle_for_branch(&intent.repo, &intent.head);
    intent
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
                kind: WorktreeWorkspaceKind::Configured,
                branch: Some(resolution.worktree.branch),
                task_url: resolution.worktree.task_url,
                provenance: None,
            })),
            Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => {
                Ok(WorktreeProviderLookup::NotFound)
            }
            Err(error) => Err(error),
        }
    }
}

impl WorktreePathProvider for CommandWorktreeProvider<'_> {
    fn resolve_path(&self, path: &Path) -> Result<Option<WorktreeMutationTarget>> {
        Ok(
            worktree_providers::resolve_worktree_provider_path_from_config(path, self.config)?
                .map(command_mutation_target),
        )
    }
}

impl WorktreeInventoryProvider for CommandWorktreeProvider<'_> {
    fn list(&self) -> Result<Vec<WorktreeProviderWorkspace>> {
        Ok(
            worktree_providers::list_enabled_worktree_providers_from_config(self.config)?
                .into_iter()
                .map(command_workspace)
                .collect(),
        )
    }

    fn observe(&self, handle: &str) -> Result<Option<WorktreeProviderWorkspace>> {
        match worktree_providers::observe_worktree_provider_from_config(handle, self.config) {
            Ok(resolution) => Ok(Some(command_workspace(resolution))),
            Err(error) if worktree_providers::is_worktree_provider_not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

impl WorktreeCleanupProvider for CommandWorktreeProvider<'_> {
    fn cleanup(&self, request: &WorktreeCleanupRequest) -> Result<WorktreeCleanupEvidence> {
        worktree_providers::cleanup_worktree_providers_from_config(
            worktree_providers::WorktreeProviderCleanupOptions {
                provider: request.providers.clone(),
                all_providers: request.all_configured_providers,
                apply: request.apply,
                timeout: request.timeout,
                provider_run_id: request.provider_run_id.clone(),
                provider_plan_id: request.provider_plan_id.clone(),
            },
            self.config.clone(),
        )
        .map(WorktreeCleanupEvidence::Configured)
    }
}

fn command_workspace(
    resolution: worktree_providers::WorktreeProviderResolution,
) -> WorktreeProviderWorkspace {
    let worktree = resolution.worktree;
    WorktreeProviderWorkspace {
        ownership: WorktreeOwnership {
            provider: WorktreeProviderIdentity::Configured(resolution.provider_id),
            handle: worktree.handle,
            path: worktree.path,
            kind: WorktreeWorkspaceKind::Configured,
            branch: Some(worktree.branch),
            task_url: worktree.task_url,
            provenance: None,
        },
        repository: None,
        owner_run_ref: None,
        created_at: None,
        terminal_disposition: None,
        safety: WorktreeProviderSafety {
            dirty: worktree.safety.dirty,
            unpushed: worktree.safety.unpushed,
            primary: worktree.safety.primary,
            missing: false,
        },
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
            Some(resolution) => WorktreeMutationLookup::Found(command_mutation_target(resolution)),
            None => WorktreeMutationLookup::NotFound,
        })
    }
}

impl WorktreeIdentityProvider for CommandWorktreeProvider<'_> {
    fn resolve_exact_identity(
        &self,
        handle: &str,
        selected_provider: Option<&str>,
    ) -> Result<WorktreeExactIdentity> {
        match selected_provider {
            Some(provider_id) => {
                worktree_providers::resolve_apply_enabled_worktree_provider_identity_by_id_from_config(
                    handle,
                    provider_id,
                    self.config,
                )
            }
            None => worktree_providers::resolve_apply_enabled_worktree_provider_identity_from_config(
                handle,
                self.config,
            ),
        }
    }

    fn resolve_exact_identity_by_path(&self, path: &Path) -> Result<Option<WorktreeExactIdentity>> {
        worktree_providers::resolve_apply_enabled_worktree_provider_identity_by_path_from_config(
            path,
            self.config,
        )
    }

    fn attest_safety(&self, identity: &WorktreeExactIdentity) -> Result<WorktreeSafetyAttestation> {
        worktree_providers::attest_apply_enabled_worktree_provider_safety_from_config(
            identity,
            self.config,
        )
    }
}

impl WorktreePreparationProvider for CommandWorktreeProvider<'_> {
    fn converge_to_base(&self, handle: &str, base_sha: &str) -> Result<WorktreeConvergence> {
        worktree_providers::converge_apply_enabled_worktree_provider_to_base_from_config(
            handle,
            base_sha,
            self.config,
        )
    }

    fn materialize(&self, identity: &WorktreeExactIdentity) -> Result<WorktreeExactIdentity> {
        worktree_providers::materialize_apply_enabled_worktree_provider_identity_from_config(
            identity,
            self.config,
        )
    }

    fn preview_task_attachment(
        &self,
        handle: &str,
        task_url: &str,
    ) -> Result<Option<WorktreeTaskAttachment>> {
        worktree_providers::preview_apply_enabled_worktree_provider_task_attachment_from_config(
            handle,
            task_url,
            self.config,
        )
    }

    fn apply_task_attachment(
        &self,
        assessment: &WorktreeTaskAttachment,
    ) -> Result<WorktreeTaskAttachment> {
        worktree_providers::apply_worktree_provider_task_attachment_from_config(
            assessment,
            self.config,
        )
    }
}

impl WorktreeDiscoveryProvider for CommandWorktreeProvider<'_> {
    fn find_by_task(
        &self,
        task_url: &str,
        head: Option<&str>,
    ) -> Result<Option<WorktreeMutationTarget>> {
        Ok(
            worktree_providers::find_apply_enabled_worktree_provider_by_task_url_and_head_from_config(
                task_url,
                head,
                self.config,
            )?
            .map(command_mutation_target),
        )
    }

    fn self_repair_contract(
        &self,
        provider_id: &str,
    ) -> Result<Option<WorktreeSelfRepairContract>> {
        worktree_providers::worktree_provider_self_repair_contract_from_config(
            provider_id,
            self.config,
        )
    }
}

fn command_mutation_target(
    resolution: worktree_providers::WorktreeProviderResolution,
) -> WorktreeMutationTarget {
    WorktreeMutationTarget {
        provider: WorktreeProviderIdentity::Configured(resolution.provider_id),
        handle: resolution.worktree.handle,
        path: PathBuf::from(resolution.worktree.path),
        source_kind: None,
        branch: Some(resolution.worktree.branch),
        task_url: resolution.worktree.task_url,
        safety: Some(WorktreeProviderSafety {
            dirty: resolution.worktree.safety.dirty,
            unpushed: resolution.worktree.safety.unpushed,
            primary: resolution.worktree.safety.primary,
            missing: false,
        }),
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
                        kind: WorktreeWorkspaceKind::Configured,
                        branch: Some(identity.branch.clone()),
                        task_url: None,
                        provenance: None,
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
        Ok(command_provision(provision))
    }
}

impl WorktreeFinalizationProvider for CommandWorktreeProvider<'_> {
    fn finalize(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: WorktreeTerminalDisposition,
    ) -> Result<WorktreeFinalizationLookup> {
        let resolution =
            match worktree_providers::observe_apply_enabled_worktree_provider_from_config(
                handle,
                self.config,
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
            kind: WorktreeWorkspaceKind::Configured,
            branch: Some(resolution.worktree.branch),
            task_url: resolution.worktree.task_url,
            provenance: None,
        },
        exact_identity: None,
    }
}

fn command_provision(
    provision: worktree_providers::WorktreeProviderProvision,
) -> WorktreeProvision {
    WorktreeProvision {
        destination: command_provision_destination(provision.resolution),
        action: if provision.action == "ensured" {
            WorktreeProvisionAction::Ensured
        } else {
            WorktreeProvisionAction::Admitted
        },
        idempotency_key: provision.idempotency_key,
    }
}

/// Ordered provider registry consumed by Homeboy workflows. Provider selection,
/// fallback, collision detection, and cleanup fanout live here rather than in
/// individual callers.
pub struct WorktreeProviderRegistry<'a> {
    config: &'a HomeboyConfig,
}

impl<'a> WorktreeProviderRegistry<'a> {
    pub fn new(config: &'a HomeboyConfig) -> Self {
        Self { config }
    }

    pub fn resolve(&self, handle: &str) -> Result<WorktreeOwnership> {
        self.resolve_if_present(handle)?.ok_or_else(|| {
            worktree_providers::worktree_provider_not_found_error(handle, self.config, false)
        })
    }

    pub fn resolve_if_present(&self, handle: &str) -> Result<Option<WorktreeOwnership>> {
        if let WorktreeProviderLookup::Found(ownership) = NativeWorktreeProvider.resolve(handle)? {
            return Ok(Some(ownership));
        }
        if let WorktreeProviderLookup::Found(ownership) =
            CommandWorktreeProvider::new(self.config).resolve(handle)?
        {
            return Ok(Some(ownership));
        }
        Ok(None)
    }

    pub fn list(&self) -> Result<Vec<WorktreeProviderWorkspace>> {
        let mut by_handle = BTreeMap::new();
        for workspace in NativeWorktreeProvider
            .list()?
            .into_iter()
            .chain(CommandWorktreeProvider::new(self.config).list()?)
        {
            if let Some(existing) = by_handle.insert(workspace.ownership.handle.clone(), workspace)
            {
                let handle = existing.ownership.handle;
                return Err(Error::validation_invalid_argument(
                    "worktree_provider",
                    format!("multiple providers claim worktree handle `{handle}`"),
                    Some(handle),
                    None,
                ));
            }
        }
        Ok(by_handle.into_values().collect())
    }

    pub fn observe(&self, handle: &str) -> Result<WorktreeProviderWorkspace> {
        if let Some(workspace) = NativeWorktreeProvider.observe(handle)? {
            return Ok(workspace);
        }
        if let Some(workspace) = CommandWorktreeProvider::new(self.config).observe(handle)? {
            return Ok(workspace);
        }
        Err(worktree_providers::worktree_provider_not_found_error(
            handle,
            self.config,
            false,
        ))
    }

    pub fn list_report(&self) -> Result<WorktreeListReport> {
        let mut native = worktree::list()?;
        let (native_worktrees, mut diagnostics) =
            NativeWorktreeProvider::workspaces_from_records(worktree::list_workspace_refs()?);
        native.diagnostics.append(&mut diagnostics);
        let provider_worktrees = native_worktrees
            .into_iter()
            .chain(CommandWorktreeProvider::new(self.config).list()?)
            .filter(|workspace| {
                matches!(
                    workspace.ownership.provider,
                    WorktreeProviderIdentity::Configured(_)
                )
            })
            .collect();
        Ok(WorktreeListReport {
            native,
            provider_worktrees,
        })
    }

    pub fn status(&self, handle: &str) -> Result<WorktreeStatusEvidence> {
        if worktree::resolve_if_present(handle)?.is_some() {
            return worktree::status(handle).map(WorktreeStatusEvidence::Native);
        }
        self.observe(handle).map(WorktreeStatusEvidence::Provider)
    }

    pub fn cleanup(&self, request: &WorktreeCleanupRequest) -> Result<WorktreeCleanupReport> {
        let mut report = WorktreeCleanupReport::default();
        if matches!(
            request.scope,
            WorktreeCleanupScope::All | WorktreeCleanupScope::Native
        ) {
            let WorktreeCleanupEvidence::Native(output) =
                NativeWorktreeProvider.cleanup(request)?
            else {
                unreachable!("native provider returned configured cleanup evidence")
            };
            report.native = Some(output);
        }
        if matches!(
            request.scope,
            WorktreeCleanupScope::All | WorktreeCleanupScope::Configured
        ) {
            let WorktreeCleanupEvidence::Configured(output) =
                CommandWorktreeProvider::new(self.config).cleanup(request)?
            else {
                unreachable!("command provider returned native cleanup evidence")
            };
            report.configured = Some(output);
        }
        Ok(report)
    }

    pub fn resolve_mutation(
        &self,
        reference: &str,
        context: WorktreeMutationContext<'_>,
    ) -> Result<WorktreeMutationTarget> {
        if let WorktreeMutationLookup::Found(target) =
            NativeWorktreeProvider.resolve_for_mutation(reference, context)?
        {
            return Ok(target);
        }
        if let WorktreeMutationLookup::Found(target) =
            CommandWorktreeProvider::new(self.config).resolve_for_mutation(reference, context)?
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
            reference,
            self.config,
            true,
        ))
    }

    pub fn admit(
        &self,
        handle: &str,
        selected_provider: Option<&WorktreeProviderIdentity>,
    ) -> Result<WorktreeProvisionLookup> {
        if let WorktreeProvisionLookup::Admitted(destination) =
            NativeWorktreeProvider.admit(handle, selected_provider)?
        {
            return Ok(WorktreeProvisionLookup::Admitted(destination));
        }
        if selected_provider == Some(&WorktreeProviderIdentity::Native) {
            return Ok(WorktreeProvisionLookup::NotFound);
        }
        CommandWorktreeProvider::new(self.config).admit(handle, selected_provider)
    }

    pub fn select_provision_provider(
        &self,
        intent: &WorktreeProvisionIntent,
    ) -> Result<WorktreeProviderIdentity> {
        if configured_provisioning_declared(self.config) {
            return worktree_providers::select_apply_enabled_worktree_provider_from_config(
                intent,
                self.config,
            )
            .map(WorktreeProviderIdentity::Configured);
        }
        Ok(WorktreeProviderIdentity::Native)
    }

    pub fn plan(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
    ) -> Result<WorktreeProvisionPlan> {
        if let WorktreeProvisionLookup::Admitted(destination) = self.admit(&intent.handle, None)? {
            return Ok(WorktreeProvisionPlan::Admitted(destination));
        }
        if configured_provisioning_declared(self.config) {
            CommandWorktreeProvider::new(self.config).plan(intent, lifecycle)
        } else {
            NativeWorktreeProvider.plan(intent, lifecycle)
        }
    }

    pub fn ensure(
        &self,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
        selected_provider: Option<&WorktreeProviderIdentity>,
    ) -> Result<WorktreeProvision> {
        match selected_provider {
            Some(WorktreeProviderIdentity::Native) => {
                NativeWorktreeProvider.ensure(intent, lifecycle)
            }
            Some(WorktreeProviderIdentity::Configured(_)) => {
                CommandWorktreeProvider::new(self.config).ensure(intent, lifecycle)
            }
            None if configured_provisioning_declared(self.config) => {
                CommandWorktreeProvider::new(self.config).ensure(intent, lifecycle)
            }
            None => NativeWorktreeProvider.ensure(intent, lifecycle),
        }
    }

    pub fn finalize(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: WorktreeTerminalDisposition,
    ) -> Result<WorktreeFinalizationLookup> {
        match NativeWorktreeProvider.finalize(handle, lifecycle, disposition)? {
            WorktreeFinalizationLookup::NotFound => {}
            outcome => return Ok(outcome),
        }
        CommandWorktreeProvider::new(self.config).finalize(handle, lifecycle, disposition)
    }

    pub fn create(
        &self,
        options: worktree::WorktreeCreateOptions,
    ) -> Result<WorktreeProviderCreateOutput> {
        let handle = worktree::handle_for_branch(&options.component_id, &options.branch);
        let intent = WorktreeProvisionIntent {
            handle,
            repo: options.component_id.clone(),
            base: options.from.clone().unwrap_or_else(|| "HEAD".to_string()),
            head: options.branch.clone(),
            task_url: options.task_url.clone(),
        };
        let configured_creation = configured_provisioning_declared(self.config);
        if configured_creation
            && intent
                .task_url
                .as_deref()
                .is_none_or(|task_url| task_url.trim().is_empty())
        {
            return Err(Error::validation_missing_argument(vec![
                "--task-url is required for configured-provider worktree creation".to_string(),
            ]));
        }
        if configured_creation && options.run_id.is_none() {
            return Err(Error::validation_missing_argument(vec![
                "--run-id is required for configured-provider worktree creation".to_string(),
            ]));
        }
        let selected = self.select_provision_provider(&intent)?;
        if selected == WorktreeProviderIdentity::Native {
            return worktree::create(options).map(WorktreeProviderCreateOutput::Native);
        }
        let owner_run_ref = options.run_id.clone().ok_or_else(|| {
            Error::internal_unexpected("configured worktree creation lost its validated owner run")
        })?;
        let cleanup_policy = match options
            .cleanup_policy
            .unwrap_or(worktree::CleanupPolicy::PreserveOnFailure)
        {
            worktree::CleanupPolicy::RemoveWhenSafe => WorktreeCleanupPolicy::RemoveOnSuccess,
            worktree::CleanupPolicy::PreserveOnFailure => WorktreeCleanupPolicy::PreserveOnFailure,
        };
        let lifecycle = WorktreeProvisionLifecycle {
            purpose: "worktree_create".to_string(),
            owner_run_ref,
            cleanup_policy,
        };
        let provision = self.ensure(&intent, &lifecycle, Some(&selected))?;
        let admitted = self
            .admit(&intent.handle, Some(&selected))?
            .into_admitted(&intent.handle)?;
        if admitted != provision.destination {
            return Err(Error::validation_invalid_argument(
                "worktree_provider",
                "configured provider postcondition does not match its ensured destination",
                Some(intent.handle),
                None,
            ));
        }
        Ok(WorktreeProviderCreateOutput::Configured(provision))
    }
}

/// Resolve through the single ordered provider boundary used by consumers.
pub fn resolve_worktree_ownership(handle: &str) -> Result<WorktreeOwnership> {
    resolve_worktree_ownership_from_config(handle, &defaults::load_config())
}

pub fn resolve_worktree_ownership_if_present(handle: &str) -> Result<Option<WorktreeOwnership>> {
    WorktreeProviderRegistry::new(&defaults::load_config()).resolve_if_present(handle)
}

pub fn resolve_worktree_ownership_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeOwnership> {
    WorktreeProviderRegistry::new(config).resolve(handle)
}

pub fn resolve_configured_worktree_path(path: &Path) -> Result<Option<WorktreeMutationTarget>> {
    resolve_configured_worktree_path_from_config(path, &defaults::load_config())
}

pub fn resolve_configured_worktree_path_from_config(
    path: &Path,
    config: &HomeboyConfig,
) -> Result<Option<WorktreeMutationTarget>> {
    CommandWorktreeProvider::new(config).resolve_path(path)
}

pub fn list_worktree_provider_inventory() -> Result<Vec<WorktreeProviderWorkspace>> {
    list_worktree_provider_inventory_from_config(&defaults::load_config())
}

pub fn list_worktree_provider_inventory_from_config(
    config: &HomeboyConfig,
) -> Result<Vec<WorktreeProviderWorkspace>> {
    WorktreeProviderRegistry::new(config).list()
}

pub fn observe_worktree_provider_workspace(handle: &str) -> Result<WorktreeProviderWorkspace> {
    observe_worktree_provider_workspace_from_config(handle, &defaults::load_config())
}

pub fn observe_worktree_provider_workspace_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderWorkspace> {
    WorktreeProviderRegistry::new(config).observe(handle)
}

pub fn list_worktrees() -> Result<WorktreeListReport> {
    WorktreeProviderRegistry::new(&defaults::load_config()).list_report()
}

pub fn worktree_status(handle: &str) -> Result<WorktreeStatusEvidence> {
    WorktreeProviderRegistry::new(&defaults::load_config()).status(handle)
}

pub fn cleanup_worktrees_from_config(
    request: &WorktreeCleanupRequest,
    config: &HomeboyConfig,
) -> Result<WorktreeCleanupReport> {
    WorktreeProviderRegistry::new(config).cleanup(request)
}

/// Resolve a local mutation target through native ownership first, then the
/// configured command providers. Provider errors are authoritative and never
/// permit fallback.
pub fn resolve_worktree_mutation_target_from_config(
    reference: &str,
    config: &HomeboyConfig,
    context: WorktreeMutationContext<'_>,
) -> Result<WorktreeMutationTarget> {
    WorktreeProviderRegistry::new(config).resolve_mutation(reference, context)
}

pub fn resolve_native_worktree_mutation_target(
    reference: &str,
    context: WorktreeMutationContext<'_>,
) -> Result<Option<WorktreeMutationTarget>> {
    Ok(
        match NativeWorktreeProvider.resolve_for_mutation(reference, context)? {
            WorktreeMutationLookup::Found(target) => Some(target),
            WorktreeMutationLookup::NotFound => None,
        },
    )
}

/// Resolve a mutation target through configured command-provider authority
/// only. This is used when durable task evidence already selected configured
/// ownership and native fallback would violate that binding.
pub fn resolve_configured_worktree_mutation_target_from_config(
    reference: &str,
    config: &HomeboyConfig,
    context: WorktreeMutationContext<'_>,
) -> Result<WorktreeMutationTarget> {
    match CommandWorktreeProvider::new(config).resolve_for_mutation(reference, context)? {
        WorktreeMutationLookup::Found(target) => Ok(target),
        WorktreeMutationLookup::NotFound => Err(
            worktree_providers::worktree_provider_not_found_error(reference, config, true),
        ),
    }
}

pub fn resolve_configured_worktree_exact_identity_from_config(
    handle: &str,
    selected_provider: Option<&str>,
    config: &HomeboyConfig,
) -> Result<WorktreeExactIdentity> {
    CommandWorktreeProvider::new(config).resolve_exact_identity(handle, selected_provider)
}

pub fn resolve_configured_worktree_exact_identity_by_path_from_config(
    path: &Path,
    config: &HomeboyConfig,
) -> Result<Option<WorktreeExactIdentity>> {
    CommandWorktreeProvider::new(config).resolve_exact_identity_by_path(path)
}

pub fn attest_configured_worktree_safety_from_config(
    identity: &WorktreeExactIdentity,
    config: &HomeboyConfig,
) -> Result<WorktreeSafetyAttestation> {
    CommandWorktreeProvider::new(config).attest_safety(identity)
}

pub fn converge_configured_worktree_to_base_from_config(
    handle: &str,
    base_sha: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeConvergence> {
    CommandWorktreeProvider::new(config).converge_to_base(handle, base_sha)
}

pub fn materialize_configured_worktree_from_config(
    identity: &WorktreeExactIdentity,
    config: &HomeboyConfig,
) -> Result<WorktreeExactIdentity> {
    CommandWorktreeProvider::new(config).materialize(identity)
}

pub fn preview_configured_worktree_task_attachment_from_config(
    handle: &str,
    task_url: &str,
    config: &HomeboyConfig,
) -> Result<Option<WorktreeTaskAttachment>> {
    CommandWorktreeProvider::new(config).preview_task_attachment(handle, task_url)
}

pub fn apply_configured_worktree_task_attachment_from_config(
    assessment: &WorktreeTaskAttachment,
    config: &HomeboyConfig,
) -> Result<WorktreeTaskAttachment> {
    CommandWorktreeProvider::new(config).apply_task_attachment(assessment)
}

pub fn find_configured_worktree_by_task_from_config(
    task_url: &str,
    head: Option<&str>,
    config: &HomeboyConfig,
) -> Result<Option<WorktreeMutationTarget>> {
    CommandWorktreeProvider::new(config).find_by_task(task_url, head)
}

pub fn configured_worktree_self_repair_contract_from_config(
    provider_id: &str,
    config: &HomeboyConfig,
) -> Result<Option<WorktreeSelfRepairContract>> {
    CommandWorktreeProvider::new(config).self_repair_contract(provider_id)
}

pub fn configured_worktree_path_requires_materialization(path: &str) -> bool {
    worktree_providers::worktree_provider_path_requires_materialization(path)
}

pub fn unsupported_configured_worktree_task_attachment_error(
    handle: &str,
    task_url: &str,
) -> Error {
    worktree_providers::unsupported_worktree_provider_task_attachment_error(handle, task_url)
}

pub fn configured_worktree_lifecycle_ensure_argv_from_config(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
    config: &HomeboyConfig,
) -> Result<Vec<String>> {
    worktree_providers::worktree_provider_lifecycle_ensure_argv_from_config(
        intent, lifecycle, config,
    )
}

pub fn with_configured_worktree_command_control<T>(
    control: WorktreeCommandControl,
    run: impl FnOnce() -> T,
) -> T {
    worktree_providers::with_worktree_provider_command_control(control, run)
}

pub fn validate_worktree_root(path: &Path, handle: &str) -> Result<()> {
    worktree_providers::validate_task_worktree_root(path, handle)
}

pub fn validate_worktree_repository_identity(
    path: &Path,
    expected_remote: Option<&str>,
    expected_repository_name: Option<&str>,
) -> Result<()> {
    worktree_providers::validate_task_worktree_repository_identity(
        path,
        expected_remote,
        expected_repository_name,
    )
}

pub fn normalize_worktree_task_url(task_url: &str) -> String {
    worktree_providers::normalize_task_url(task_url)
}

pub fn worktree_provision_idempotency_key(intent: &WorktreeProvisionIntent) -> String {
    worktree_providers::worktree_provider_idempotency_key(intent)
}

pub fn compact_worktree_provider_failure_details(
    details: &serde_json::Value,
) -> Option<serde_json::Value> {
    worktree_providers::compact_provider_failure_details(details)
}

pub fn validate_configured_worktree_creation_contracts(config: &HomeboyConfig) -> Result<()> {
    worktree_providers::validate_active_workspace_creation_provider_contracts(config)
}

/// Compatibility for persisted Cook recipes created before lifecycle ownership
/// fields were recorded. New provisioning always uses `ensure_worktree_provision_from_config`.
pub fn ensure_legacy_configured_worktree_from_config(
    intent: &WorktreeProvisionIntent,
    config: &HomeboyConfig,
) -> Result<WorktreeProvision> {
    let provision =
        worktree_providers::provision_apply_enabled_worktree_provider_from_config(intent, config)?;
    Ok(command_provision(provision))
}

/// Admit an existing destination through native ownership first, then through
/// configured apply-enabled ownership. A selected provider is exact authority
/// for durable continuation and disables fallback.
pub fn admit_worktree_provision_from_config(
    handle: &str,
    selected_provider: Option<&WorktreeProviderIdentity>,
    config: &HomeboyConfig,
) -> Result<WorktreeProvisionLookup> {
    WorktreeProviderRegistry::new(config).admit(handle, selected_provider)
}

/// Select the provider that will own a future ensure without invoking its
/// mutation or requiring the optional read-only planning capability.
pub fn select_worktree_provision_provider_from_config(
    intent: &WorktreeProvisionIntent,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderIdentity> {
    WorktreeProviderRegistry::new(config).select_provision_provider(intent)
}

/// Produce a non-mutating destination plan through the same provider selection
/// execution will use. Configured creation remains preferred when declared;
/// otherwise Homeboy's native task-worktree lifecycle is the provider.
pub fn plan_worktree_provision_from_config(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
    config: &HomeboyConfig,
) -> Result<WorktreeProvisionPlan> {
    WorktreeProviderRegistry::new(config).plan(intent, lifecycle)
}

/// Ensure an absent destination through its selected lifecycle provider. This
/// method does not admit the postcondition; callers must invoke `admit` again.
pub fn ensure_worktree_provision_from_config(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
    selected_provider: Option<&WorktreeProviderIdentity>,
    config: &HomeboyConfig,
) -> Result<WorktreeProvision> {
    WorktreeProviderRegistry::new(config).ensure(intent, lifecycle, selected_provider)
}

/// Create through the selected provider while preserving the built-in
/// provider's stable output. Configured creation requires explicit task and run
/// ownership before any provider mutation is invoked.
pub fn create_worktree_from_config(
    options: worktree::WorktreeCreateOptions,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderCreateOutput> {
    WorktreeProviderRegistry::new(config).create(options)
}

pub fn create_worktree(
    options: worktree::WorktreeCreateOptions,
) -> Result<WorktreeProviderCreateOutput> {
    create_worktree_from_config(options, &defaults::load_config())
}

/// Finalize through native ownership first, then configured ownership. Absence
/// and unsupported optional finalization are explicit non-error outcomes.
pub fn finalize_worktree_from_config(
    handle: &str,
    lifecycle: &WorktreeProvisionLifecycle,
    disposition: WorktreeTerminalDisposition,
    config: &HomeboyConfig,
) -> Result<WorktreeFinalizationLookup> {
    WorktreeProviderRegistry::new(config).finalize(handle, lifecycle, disposition)
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

    fn assert_lifecycle_conformance(
        provider: &dyn WorktreeLifecycleProvider,
        intent: &WorktreeProvisionIntent,
        lifecycle: &WorktreeProvisionLifecycle,
        expected_provider: WorktreeProviderIdentity,
    ) -> WorktreeFinalization {
        let WorktreeProvisionPlan::Planned(planned) = provider
            .plan(intent, lifecycle)
            .expect("missing destination plans without mutation")
        else {
            panic!("missing destination must be planned");
        };
        assert_eq!(planned.ownership.provider, expected_provider);
        let ensured = provider
            .ensure(intent, lifecycle)
            .expect("destination ensure");
        assert_eq!(ensured.action, WorktreeProvisionAction::Ensured);
        assert_eq!(ensured.destination.ownership, planned.ownership);
        let replay = provider
            .ensure(intent, lifecycle)
            .expect("destination ensure replay");
        assert_eq!(replay.action, WorktreeProvisionAction::Admitted);
        assert_eq!(replay.idempotency_key, ensured.idempotency_key);
        let WorktreeFinalizationLookup::Finalized(finalized) = provider
            .finalize(
                intent.handle.as_str(),
                lifecycle,
                WorktreeTerminalDisposition::Failed,
            )
            .expect("terminal finalization")
        else {
            panic!("owned lifecycle must finalize");
        };
        let WorktreeFinalizationLookup::Finalized(replayed) = provider
            .finalize(
                intent.handle.as_str(),
                lifecycle,
                WorktreeTerminalDisposition::Failed,
            )
            .expect("terminal finalization replay")
        else {
            panic!("owned lifecycle finalization replay must remain supported");
        };
        assert_eq!(replayed, finalized);
        let cleanup = provider
            .cleanup(&WorktreeCleanupRequest {
                scope: match expected_provider {
                    WorktreeProviderIdentity::Native => WorktreeCleanupScope::Native,
                    WorktreeProviderIdentity::Configured(_) => WorktreeCleanupScope::Configured,
                },
                providers: match &expected_provider {
                    WorktreeProviderIdentity::Native => Vec::new(),
                    WorktreeProviderIdentity::Configured(provider) => vec![provider.clone()],
                },
                all_configured_providers: false,
                apply: false,
                force: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
                timeout: None,
                provider_run_id: None,
                provider_plan_id: None,
            })
            .expect("cleanup preview");
        match cleanup {
            WorktreeCleanupEvidence::Native(output) => assert!(output.dry_run),
            WorktreeCleanupEvidence::Configured(output) => {
                assert_eq!(output.success_count, 1);
                assert_eq!(output.failure_count, 0);
            }
        }
        finalized
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
            let adopted_path = home.path().join("adopted-workspace");
            std::fs::create_dir(&adopted_path).expect("adopted workspace");
            worktree::adopt(worktree::WorktreeAdoptOptions {
                handle: "fixture@adopted".to_string(),
                path: adopted_path.display().to_string(),
                kind: Some("fixture".to_string()),
                provenance: Some(serde_json::json!({ "owner": "contract-test" })),
            })
            .expect("adopt workspace");

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
            assert_lookup_conformance(
                &NativeWorktreeProvider,
                "fixture@adopted",
                WorktreeProviderIdentity::Native,
                &adopted_path,
            );
            assert_mutation_conformance(
                &NativeWorktreeProvider,
                "fixture@adopted",
                WorktreeProviderIdentity::Native,
                &adopted_path,
            );
            let native_inventory = NativeWorktreeProvider.list().expect("native inventory");
            assert_eq!(native_inventory.len(), 2);
            let adopted = native_inventory
                .iter()
                .find(|workspace| workspace.ownership.handle == "fixture@adopted")
                .expect("adopted workspace inventory");
            assert_eq!(
                adopted.ownership.kind,
                WorktreeWorkspaceKind::AdoptedWorkspace
            );
            assert_eq!(
                adopted.ownership.provenance,
                Some(serde_json::json!({ "owner": "contract-test" }))
            );
            let native = native_inventory
                .iter()
                .find(|workspace| workspace.ownership.handle == "fixture@native")
                .expect("task worktree inventory");
            assert_eq!(native.repository.as_deref(), Some("fixture"));
            let intent = WorktreeProvisionIntent {
                handle: "fixture@planned".to_string(),
                repo: "fixture".to_string(),
                base: "main".to_string(),
                head: "planned".to_string(),
                task_url: Some("https://example.test/issues/8017".to_string()),
            };
            let lifecycle = WorktreeProvisionLifecycle {
                purpose: "agent_task_cook".to_string(),
                owner_run_ref: "native-plan-run".to_string(),
                cleanup_policy: WorktreeCleanupPolicy::RemoveOnSuccess,
            };
            assert!(matches!(
                NativeWorktreeProvider
                    .finalize(
                        "fixture@adopted",
                        &lifecycle,
                        WorktreeTerminalDisposition::Succeeded,
                    )
                    .expect("adopted finalization capability"),
                WorktreeFinalizationLookup::Unsupported
            ));
            assert_eq!(
                select_worktree_provision_provider_from_config(&intent, &HomeboyConfig::default())
                    .expect("native provider selection"),
                WorktreeProviderIdentity::Native
            );
            assert!(worktree::resolve_if_present(&intent.handle)
                .expect("preview registry lookup")
                .is_none());
            let finalized = assert_lifecycle_conformance(
                &NativeWorktreeProvider,
                &intent,
                &lifecycle,
                WorktreeProviderIdentity::Native,
            );
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
                    WorktreeTerminalDisposition::Succeeded,
                )
                .expect_err("terminal disposition cannot change");
            std::fs::write(path.join("dirty"), "dirty\n").expect("dirty native worktree");
            assert_unsafe_lookup(&NativeWorktreeProvider, "fixture@native");
        });
    }

    #[test]
    fn list_report_keeps_valid_inventory_when_a_record_source_is_unrecoverable() {
        crate::test_support::with_isolated_home(|home| {
            let (source, path) = initialize_native_worktree(home.path());
            worktree::record_active_with_source_for_test("fixture@native", source.path(), &path);

            let missing_source = home.path().join("missing-source");
            let missing_worktree = home.path().join("missing-worktree");
            worktree::record_active_with_source_for_test(
                "missing@source",
                &missing_source,
                &missing_worktree,
            );
            let record_path = crate::paths::observation_db()
                .expect("observation database")
                .parent()
                .expect("observation database parent")
                .join("task-worktrees")
                .join(format!(
                    "{}.json",
                    crate::paths::sanitize_path_segment("missing@source")
                ));
            let mut record: serde_json::Value = serde_json::from_slice(
                &std::fs::read(&record_path).expect("missing-source record"),
            )
            .expect("parse missing-source record");
            record["component_id"] = serde_json::json!("unrecoverable-component");
            std::fs::write(
                &record_path,
                serde_json::to_vec_pretty(&record).expect("serialize missing-source record"),
            )
            .expect("write missing-source record");

            let report = WorktreeProviderRegistry::new(&defaults::load_config())
                .list_report()
                .expect("list report");

            assert!(report
                .native
                .worktrees
                .iter()
                .any(|record| record.id == "missing@source"));
            assert!(report
                .native
                .worktrees
                .iter()
                .any(|record| record.id == "fixture@native"));
            assert_eq!(report.native.diagnostics.len(), 1);
            let diagnostic = &report.native.diagnostics[0];
            assert_eq!(diagnostic.code, "validation.invalid_argument");
            assert_eq!(diagnostic.record_id.as_deref(), Some("missing@source"));
            assert_eq!(
                diagnostic.details["field"].as_str(),
                Some("source_checkout")
            );
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
                            "task_url": "https://example.test/issues/8017",
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
                        task_url: Some("$.task_url".to_string()),
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
            let mutation = resolve_configured_worktree_mutation_target_from_config(
                "fixture@command",
                &config,
                WorktreeMutationContext::default(),
            )
            .expect("configured mutation facade");
            assert_eq!(mutation.branch.as_deref(), Some("command-branch"));
            assert_eq!(
                mutation.task_url.as_deref(),
                Some("https://example.test/issues/8017")
            );
            assert_eq!(
                mutation.safety,
                Some(WorktreeProviderSafety {
                    dirty: false,
                    unpushed: false,
                    primary: false,
                    missing: false,
                })
            );
            let by_path = resolve_configured_worktree_path_from_config(workspace.path(), &config)
                .expect("configured path facade")
                .expect("provider owns path");
            assert_eq!(by_path.handle, "fixture@command");
            let identity = resolve_configured_worktree_exact_identity_from_config(
                "fixture@command",
                None,
                &config,
            )
            .expect("configured identity facade");
            let safety = attest_configured_worktree_safety_from_config(&identity, &config)
                .expect("configured safety facade");
            assert!(safety.fresh);
            assert!(!safety.dirty);
            assert_eq!(
                materialize_configured_worktree_from_config(&identity, &config)
                    .expect("local identity needs no materialization"),
                identity
            );
            let task_owned = find_configured_worktree_by_task_from_config(
                "https://example.test/issues/8017",
                Some("command-branch"),
                &config,
            )
            .expect("configured task discovery facade")
            .expect("task-owned worktree");
            assert_eq!(task_owned.handle, "fixture@command");
            assert_provision_admission_conformance(
                &CommandWorktreeProvider::new(&config),
                "fixture@command",
                WorktreeProviderIdentity::Configured("command-fixture".to_string()),
                workspace.path(),
            );
            let command_inventory = CommandWorktreeProvider::new(&config)
                .list()
                .expect("command inventory");
            assert_eq!(command_inventory.len(), 2);
            assert!(command_inventory.iter().any(|workspace| {
                workspace.ownership.handle == "fixture@unsafe" && workspace.safety.dirty
            }));
            let unsafe_observation = CommandWorktreeProvider::new(&config)
                .observe("fixture@unsafe")
                .expect("unsafe observation")
                .expect("unsafe provider workspace");
            assert!(unsafe_observation.safety.dirty);
            assert_unsafe_lookup(&CommandWorktreeProvider::new(&config), "fixture@unsafe");
        });
    }

    #[cfg(unix)]
    #[test]
    fn command_provider_conforms_to_shared_lifecycle_contract() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|home| {
            let source = home.path().join("command-source");
            std::fs::create_dir(&source).expect("source checkout");
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec!["config", "user.email", "homeboy@example.test"],
                vec!["config", "user.name", "Homeboy Test"],
            ] {
                assert!(std::process::Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("initialize source")
                    .success());
            }
            std::fs::write(source.join("README"), "fixture\n").expect("source file");
            for args in [vec!["add", "README"], vec!["commit", "-q", "-m", "fixture"]] {
                assert!(std::process::Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("commit source")
                    .success());
            }

            let workspace = home.path().join("command-lifecycle-worktree");
            let state = home.path().join("command-lifecycle-state");
            let finalizations = home.path().join("command-finalizations");
            let script = home.path().join("command-lifecycle-provider");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\ncase \"$1\" in\nresolve)\n  if [ -f '{state}' ]; then printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"fixture@command-lifecycle\",\"path\":\"{workspace}\",\"branch\":\"command-lifecycle\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'; else exit 1; fi\n  ;;\nplan)\n  printf '%s\\n' \"{{\\\"worktrees\\\":[{{\\\"handle\\\":\\\"$2\\\",\\\"path\\\":\\\"{workspace}\\\",\\\"branch\\\":\\\"$5\\\",\\\"safety\\\":{{\\\"dirty\\\":false,\\\"unpushed\\\":false,\\\"primary\\\":false}}}}]}}\"\n  ;;\nensure)\n  git -C '{source}' worktree add -q -b command-lifecycle '{workspace}' main\n  touch '{state}'\n  ;;\ncleanup)\n  printf '%s\\n' '{{\"mode\":\"preview\"}}'\n  ;;\nfinalize)\n  key=\"${{10}}\"\n  if [ ! -f '{finalizations}' ] || ! grep -Fqx \"$key\" '{finalizations}'; then printf '%s\\n' \"$key\" >> '{finalizations}'; fi\n  ;;\nesac\n",
                    state = state.display(),
                    workspace = workspace.display(),
                    source = source.display(),
                    finalizations = finalizations.display(),
                ),
            )
            .expect("provider script");
            let mut permissions = std::fs::metadata(&script)
                .expect("provider metadata")
                .permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("executable provider");

            let mut providers = HashMap::new();
            providers.insert(
                "command-lifecycle".to_string(),
                WorktreeProviderConfig {
                    enabled: true,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: true,
                    commands: WorktreeProviderCommands {
                        resolve: Some(vec![script.display().to_string(), "resolve".to_string()]),
                        resolve_not_found_exit_codes: vec![1],
                        plan: Some(vec![
                            script.display().to_string(),
                            "plan".to_string(),
                            "{handle}".to_string(),
                            "{repo}".to_string(),
                            "{base}".to_string(),
                            "{head}".to_string(),
                            "{task_url}".to_string(),
                            "{idempotency_key}".to_string(),
                        ]),
                        ensure: Some(vec![script.display().to_string(), "ensure".to_string()]),
                        cleanup_preview: Some(vec![
                            script.display().to_string(),
                            "cleanup".to_string(),
                        ]),
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
            let mut config = HomeboyConfig {
                worktree_providers: providers,
                ..HomeboyConfig::default()
            };
            config.settings.insert(
                worktree_providers::WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY.to_string(),
                serde_json::json!({
                    "command-lifecycle": {
                        "finalize": [
                            script.display().to_string(), "finalize", "{handle}", "{purpose}",
                            "{owner_run_ref}", "{cleanup_policy}", "{disposition}",
                            "{owner_outcome}", "{lifecycle_state}", "{idempotency_key}"
                        ]
                    }
                }),
            );
            let intent = WorktreeProvisionIntent {
                handle: "fixture@command-lifecycle".to_string(),
                repo: "fixture".to_string(),
                base: "main".to_string(),
                head: "command-lifecycle".to_string(),
                task_url: Some("https://example.test/issues/8017".to_string()),
            };
            let lifecycle = WorktreeProvisionLifecycle {
                purpose: "contract_test".to_string(),
                owner_run_ref: "command-contract-run".to_string(),
                cleanup_policy: WorktreeCleanupPolicy::RemoveOnSuccess,
            };

            let finalized = assert_lifecycle_conformance(
                &CommandWorktreeProvider::new(&config),
                &intent,
                &lifecycle,
                WorktreeProviderIdentity::Configured("command-lifecycle".to_string()),
            );
            assert_eq!(finalized.provider_id, "command-lifecycle");
            assert_eq!(
                std::fs::read_to_string(finalizations)
                    .expect("finalization evidence")
                    .lines()
                    .count(),
                1,
            );
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
            let missing_ownership = create_worktree_from_config(
                worktree::WorktreeCreateOptions {
                    component_id: "fixture".to_string(),
                    branch: "planned".to_string(),
                    from: Some("main".to_string()),
                    task_url: None,
                    run_id: None,
                    cleanup_policy: None,
                    require_handoff_freshness: false,
                },
                &config,
            )
            .expect_err("configured creation requires durable ownership");
            assert_eq!(
                missing_ownership.details["args"][0],
                "--task-url is required for configured-provider worktree creation"
            );
            let intent = WorktreeProvisionIntent {
                handle: "fixture@planned".to_string(),
                repo: "fixture".to_string(),
                base: "main".to_string(),
                head: "planned".to_string(),
                task_url: Some("https://example.test/issues/8017".to_string()),
            };
            let lifecycle = WorktreeProvisionLifecycle {
                purpose: "agent_task_cook".to_string(),
                owner_run_ref: "command-plan-run".to_string(),
                cleanup_policy: WorktreeCleanupPolicy::RemoveOnSuccess,
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
