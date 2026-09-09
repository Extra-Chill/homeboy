use crate::error::{Error, Result};
use crate::worktree;
use std::path::{Path, PathBuf};

/// Canonical read-only ownership for a native Homeboy workspace.
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

#[derive(Debug, Clone)]
pub struct WorktreeListReport {
    pub native: worktree::WorktreeListOutput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeMutationTarget {
    pub handle: String,
    pub path: PathBuf,
    pub source_kind: Option<String>,
    pub branch: Option<String>,
    pub task_url: Option<String>,
}

/// Lifecycle ownership bound before native provisioning is allowed.
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
pub struct WorktreeProvisionIntent {
    pub handle: String,
    pub repo: String,
    pub base: String,
    pub head: String,
    pub task_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeProvisionDestination {
    pub ownership: WorktreeOwnership,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeFinalizationLookup {
    Finalized(WorktreeFinalization),
    Unsupported,
    NotFound,
}

/// Native worktree lifecycle boundary. It keeps registry identity, safety
/// validation, provisioning, and terminal finalization in one authority.
pub struct NativeWorktreeProvider;

impl NativeWorktreeProvider {
    pub fn resolve(&self, handle: &str) -> Result<Option<WorktreeOwnership>> {
        let Some(record) = worktree::resolve_workspace_ref_if_present(handle)? else {
            return Ok(None);
        };
        if record.handle() != handle {
            return Err(handle_mismatch_error(record.handle(), handle));
        }
        if record.state() == &worktree::TaskWorktreeState::Removed {
            return Ok(None);
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
        Ok(Some(WorktreeOwnership {
            handle: record.handle().to_string(),
            path: path.display().to_string(),
            kind,
            branch,
            task_url,
            provenance,
        }))
    }

    fn workspaces_from_records(
        records: impl IntoIterator<Item = worktree::WorkspaceRefRecord>,
    ) -> (
        Vec<WorktreeWorkspace>,
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

    fn workspace_from_record(record: &worktree::WorkspaceRefRecord) -> Result<WorktreeWorkspace> {
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
                worktree::WorkspaceRefRecord::Task(record) => record.created_at.clone(),
                worktree::WorkspaceRefRecord::Adopted(record) => record.created_at.clone(),
            }),
            terminal_disposition,
            safety,
        })
    }

    pub fn resolve_for_mutation(&self, reference: &str) -> Result<Option<WorktreeMutationTarget>> {
        let Some(record) = worktree::resolve_workspace_ref_if_present(reference)? else {
            return Ok(None);
        };
        if record.handle() != reference {
            return Err(handle_mismatch_error(record.handle(), reference));
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

    /// Resolve an active native workspace by its exact filesystem identity.
    /// Paths are accepted at the CLI boundary, but mutations use
    /// the registry handle that owns the linked task worktree.
    fn resolve_for_mutation_by_path(&self, path: &Path) -> Result<Option<WorktreeMutationTarget>> {
        let path = std::fs::canonicalize(path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?;
        for record in worktree::list_workspace_refs()? {
            if !matches!(record, worktree::WorkspaceRefRecord::Task(_)) {
                continue;
            }
            let registered = Path::new(record.path());
            let Ok(registered) = std::fs::canonicalize(registered) else {
                continue;
            };
            if registered == path {
                return self.resolve_for_mutation(record.handle());
            }
        }
        Ok(None)
    }

    pub fn plan(&self, intent: &WorktreeProvisionIntent) -> Result<WorktreeProvisionPlan> {
        let intent = native_provision_intent(intent)?;
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
        let intent = native_provision_intent(intent)?;
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
            require_handoff_freshness: false,
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
    ) -> Result<WorktreeFinalizationLookup> {
        self.finalize_with_effect_fence(handle, lifecycle, disposition, || Ok(()))
    }

    pub fn finalize_with_effect_fence(
        &self,
        handle: &str,
        lifecycle: &WorktreeProvisionLifecycle,
        disposition: WorktreeTerminalDisposition,
        before_effect: impl FnOnce() -> Result<()>,
    ) -> Result<WorktreeFinalizationLookup> {
        let Some(workspace) = worktree::resolve_workspace_ref_if_present(handle)? else {
            return Ok(WorktreeFinalizationLookup::NotFound);
        };
        let worktree::WorkspaceRefRecord::Task(record) = workspace else {
            return Ok(WorktreeFinalizationLookup::Unsupported);
        };
        if record.id != handle {
            return Err(handle_mismatch_error(&record.id, handle));
        }
        let record = worktree::finalize_provider_lifecycle_with_effect_fence(
            handle,
            &lifecycle.owner_run_ref,
            disposition,
            before_effect,
        )?;
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

fn handle_mismatch_error(record_handle: &str, requested_handle: &str) -> Error {
    Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "native worktree registry record `{record_handle}` does not match requested handle `{requested_handle}`"
        ),
        Some(requested_handle.to_string()),
        None,
    )
}

fn native_provision_intent(intent: &WorktreeProvisionIntent) -> Result<WorktreeProvisionIntent> {
    let repo = Path::new(&intent.repo);
    let repository = repo
        .is_dir()
        .then(|| repo.file_name())
        .flatten()
        .and_then(|name| name.to_str())
        .filter(|name| !name.is_empty())
        .unwrap_or(&intent.repo);
    let expected = worktree::handle_for_branch(repository, &intent.head);
    if intent.handle != expected {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "native worktree handle `{}` does not match repository `{}` and branch `{}`; expected `{expected}`",
                intent.handle, intent.repo, intent.head
            ),
            Some(intent.handle.clone()),
            None,
        ));
    }
    Ok(intent.clone())
}

pub fn resolve_worktree_ownership(handle: &str) -> Result<WorktreeOwnership> {
    NativeWorktreeProvider.resolve(handle)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!("native worktree `{handle}` was not found"),
            Some(handle.to_string()),
            None,
        )
    })
}

pub fn resolve_worktree_ownership_if_present(handle: &str) -> Result<Option<WorktreeOwnership>> {
    NativeWorktreeProvider.resolve(handle)
}

pub fn resolve_native_worktree_mutation_target(
    reference: &str,
) -> Result<Option<WorktreeMutationTarget>> {
    NativeWorktreeProvider.resolve_for_mutation(reference)
}

/// Resolve an active native mutation target from an exact linked-worktree path.
pub fn resolve_native_worktree_mutation_target_by_path(
    path: &Path,
) -> Result<Option<WorktreeMutationTarget>> {
    NativeWorktreeProvider.resolve_for_mutation_by_path(path)
}

pub fn list_worktree_inventory() -> Result<Vec<WorktreeWorkspace>> {
    let (workspaces, _) =
        NativeWorktreeProvider::workspaces_from_records(worktree::list_workspace_refs()?);
    Ok(workspaces)
}

pub fn list_worktrees() -> Result<WorktreeListReport> {
    let mut native = worktree::list()?;
    let (_, mut diagnostics) =
        NativeWorktreeProvider::workspaces_from_records(worktree::list_workspace_refs()?);
    native.diagnostics.append(&mut diagnostics);
    Ok(WorktreeListReport { native })
}

pub fn ensure_worktree_provision(
    intent: &WorktreeProvisionIntent,
    lifecycle: &WorktreeProvisionLifecycle,
) -> Result<WorktreeProvision> {
    NativeWorktreeProvider.ensure(intent, lifecycle)
}

pub fn plan_worktree_provision(intent: &WorktreeProvisionIntent) -> Result<WorktreeProvisionPlan> {
    NativeWorktreeProvider.plan(intent)
}

pub fn finalize_worktree(
    handle: &str,
    lifecycle: &WorktreeProvisionLifecycle,
    disposition: WorktreeTerminalDisposition,
) -> Result<WorktreeFinalizationLookup> {
    NativeWorktreeProvider.finalize(handle, lifecycle, disposition)
}

pub fn finalize_worktree_with_effect_fence(
    handle: &str,
    lifecycle: &WorktreeProvisionLifecycle,
    disposition: WorktreeTerminalDisposition,
    before_effect: impl FnOnce() -> Result<()>,
) -> Result<WorktreeFinalizationLookup> {
    NativeWorktreeProvider.finalize_with_effect_fence(handle, lifecycle, disposition, before_effect)
}

pub fn worktree_provision_idempotency_key(intent: &WorktreeProvisionIntent) -> String {
    format!(
        "{}:{}:{}:{}",
        intent.repo,
        intent.base,
        intent.head,
        intent.task_url.as_deref().unwrap_or_default()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_provision_rejects_a_noncanonical_handle_before_mutation() {
        let intent = WorktreeProvisionIntent {
            repo: "fixture".to_string(),
            base: "main".to_string(),
            head: "fix/13940".to_string(),
            handle: "fixture@wrong".to_string(),
            task_url: None,
        };

        let error = plan_worktree_provision(&intent).expect_err("mismatched handle must fail");

        assert_eq!(error.details["field"], "to_worktree");
        assert!(error.message.contains("expected `fixture@fix-13940`"));
    }

    #[test]
    fn native_provider_keeps_active_and_adopted_workspaces_distinct() {
        crate::test_support::with_isolated_home(|home| {
            let path = home.path().join("adopted-workspace");
            std::fs::create_dir(&path).expect("adopted workspace");
            worktree::adopt(worktree::WorktreeAdoptOptions {
                handle: "fixture@adopted".to_string(),
                path: path.display().to_string(),
                kind: Some("fixture".to_string()),
                provenance: Some(serde_json::json!({ "owner": "contract-test" })),
            })
            .expect("adopt workspace");

            let ownership = NativeWorktreeProvider
                .resolve("fixture@adopted")
                .expect("resolve adopted")
                .expect("owned workspace");
            assert_eq!(ownership.kind, WorktreeWorkspaceKind::AdoptedWorkspace);
            assert_eq!(
                ownership.provenance,
                Some(serde_json::json!({ "owner": "contract-test" }))
            );
            assert!(matches!(
                NativeWorktreeProvider
                    .finalize(
                        "fixture@adopted",
                        &WorktreeProvisionLifecycle {
                            purpose: "test".to_string(),
                            owner_run_ref: "run".to_string(),
                            cleanup_policy: WorktreeCleanupPolicy::RemoveOnSuccess,
                        },
                        WorktreeTerminalDisposition::Succeeded,
                    )
                    .expect("adopted finalization"),
                WorktreeFinalizationLookup::Unsupported
            ));
        });
    }

    #[test]
    fn native_provider_resolves_registered_task_worktree_by_exact_path() {
        crate::test_support::with_isolated_home(|home| {
            let primary = home.path().join("primary");
            let path = home.path().join("native-worktree");
            std::fs::create_dir_all(&primary).expect("primary checkout");
            std::fs::create_dir_all(&path).expect("native worktree");
            worktree::record_active_for_test("fixture@native", &path);

            let target = resolve_native_worktree_mutation_target_by_path(&path)
                .expect("native path lookup")
                .expect("native path is registry-owned");
            assert_eq!(target.handle, "fixture@native");
            assert_eq!(target.path, path);
            assert!(resolve_native_worktree_mutation_target_by_path(&primary)
                .expect("primary path lookup")
                .is_none());
        });
    }

    #[test]
    fn list_report_keeps_valid_inventory_when_a_record_source_is_unrecoverable() {
        crate::test_support::with_isolated_home(|home| {
            let source = tempfile::tempdir_in(home.path()).expect("source checkout");
            for args in [
                vec!["init", "-q", "-b", "main"],
                vec!["config", "user.email", "homeboy@example.test"],
                vec!["config", "user.name", "Homeboy Test"],
            ] {
                assert!(std::process::Command::new("git")
                    .args(args)
                    .current_dir(source.path())
                    .status()
                    .expect("initialize source")
                    .success());
            }
            std::fs::write(source.path().join("README"), "fixture\n").expect("source file");
            assert!(std::process::Command::new("git")
                .args(["add", "README"])
                .current_dir(source.path())
                .status()
                .expect("stage source")
                .success());
            assert!(std::process::Command::new("git")
                .args(["commit", "-q", "-m", "fixture"])
                .current_dir(source.path())
                .status()
                .expect("commit source")
                .success());
            crate::test_support::write_component_registration(
                home.path(),
                "fixture",
                source.path(),
            );
            let valid = home.path().join("fixture@valid");
            assert!(std::process::Command::new("git")
                .args([
                    "worktree",
                    "add",
                    "-b",
                    "valid",
                    valid.to_str().expect("UTF-8 worktree path"),
                ])
                .current_dir(source.path())
                .status()
                .expect("create native worktree")
                .success());
            worktree::record_active_for_test("fixture@valid", &valid);
            let records = crate::paths::observation_db()
                .expect("observation database")
                .parent()
                .expect("observation database parent")
                .join("task-worktrees");
            let valid_record = records.join(format!(
                "{}.json",
                crate::paths::sanitize_path_segment("fixture@valid")
            ));
            let missing_record = records.join(format!(
                "{}.json",
                crate::paths::sanitize_path_segment("missing@source")
            ));
            let mut record: serde_json::Value =
                serde_json::from_slice(&std::fs::read(valid_record).expect("valid record"))
                    .expect("parse valid record");
            record["id"] = serde_json::json!("missing@source");
            record["component_id"] = serde_json::json!("unrecoverable-component");
            record["source_checkout"] = serde_json::json!(home.path().join("missing-source"));
            record["worktree_path"] = serde_json::json!(home.path().join("missing-worktree"));
            std::fs::write(
                missing_record,
                serde_json::to_vec_pretty(&record).expect("serialize missing record"),
            )
            .expect("write missing record");

            let report = list_worktrees().expect("list report");
            assert!(report
                .native
                .worktrees
                .iter()
                .any(|record| record.id == "fixture@valid"));
            assert!(report
                .native
                .worktrees
                .iter()
                .any(|record| record.id == "missing@source"));
            assert!(!report.native.diagnostics.is_empty());
        });
    }

    #[test]
    fn native_provider_treats_removed_records_as_not_found() {
        crate::test_support::with_isolated_home(|home| {
            worktree::record_removed_for_test("fixture@removed", &home.path().join("removed"));
            assert!(NativeWorktreeProvider
                .resolve("fixture@removed")
                .expect("removed lookup")
                .is_none());
        });
    }

    #[test]
    fn native_provider_rejects_colliding_manifest_identity() {
        crate::test_support::with_isolated_home(|home| {
            worktree::record_removed_for_test("fixture@a/b", &home.path().join("colliding"));
            let error = NativeWorktreeProvider
                .resolve("fixture@a?b")
                .expect_err("colliding handle must not resolve another manifest");
            assert!(error.message.contains("does not match requested handle"));
        });
    }
}
