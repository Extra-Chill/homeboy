use std::path::Path;

use crate::release::operation_record::{FinalizationClaim, OperationRecord, OperationRecordStore};
use homeboy_core::component::Component;
use homeboy_core::defaults;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;
use homeboy_core::worktree_provider::{
    self, worktree_provision_idempotency_key as worktree_provider_idempotency_key,
    WorktreeCleanupPolicy as WorktreeProviderCleanupPolicy, WorktreeFinalizationLookup,
    WorktreeProviderIdentity, WorktreeProvisionDestination,
    WorktreeProvisionIntent as WorktreeProviderCreateIntent,
    WorktreeProvisionLifecycle as WorktreeProviderLifecycleIntent,
    WorktreeTerminalDisposition as WorktreeProviderTerminalDisposition,
};
use uuid::Uuid;

use super::types::ReleaseWorkspaceOutput;

pub(super) struct ReleaseWorkspace {
    pub(super) component: Component,
    output: ReleaseWorkspaceOutput,
    owned: Option<(
        WorktreeProvisionDestination,
        WorktreeProviderLifecycleIntent,
    )>,
    record_owner: Option<String>,
    /// Bound to the roots the release boundary resolved. Provisioning and
    /// finalization write the same record from different phases of a release,
    /// so both must address the same home (#7505).
    store: OperationRecordStore,
}

impl ReleaseWorkspace {
    pub(super) fn source_sha(&self) -> Option<String> {
        self.output.source_sha.clone()
    }

    pub(super) fn select(
        roots: &homeboy_core::paths::PathRoots,
        component: &Component,
    ) -> Result<Self> {
        let store = OperationRecordStore::in_roots(roots);
        if in_place_eligible(component) {
            return Ok(Self {
                component: component.clone(),
                output: ReleaseWorkspaceOutput::in_place(&component.local_path),
                owned: None,
                record_owner: None,
                store,
            });
        }

        let config = defaults::load_config();
        let source_sha = verified_remote_default_sha(component)?;
        let default_branch = git::default_branch_name(Path::new(&component.local_path))
            .unwrap_or_else(|| "main".to_string());
        let owner_run_ref = format!("release/{}", Uuid::new_v4());
        let lifecycle = WorktreeProviderLifecycleIntent {
            purpose: "release_staging".to_string(),
            owner_run_ref: owner_run_ref.clone(),
            cleanup_policy: WorktreeProviderCleanupPolicy::RemoveOnSuccess,
        };
        let handle = format!("release-{}-{}", component.id, &source_sha[..12]);
        let mut intent = WorktreeProviderCreateIntent {
            handle,
            repo: component.id.clone(),
            base: default_branch,
            head: source_sha.clone(),
            task_url: owner_run_ref,
        };
        let planned =
            worktree_provider::plan_worktree_provision_from_config(&intent, &lifecycle, &config)?;
        let destination = match planned {
            homeboy_core::worktree_provider::WorktreeProvisionPlan::Admitted(destination)
            | homeboy_core::worktree_provider::WorktreeProvisionPlan::Planned(destination) => {
                destination
            }
        };
        intent.handle = destination.ownership.handle;
        let selected_provider = destination.ownership.provider;
        let selected_provider_id = provider_evidence_id(&selected_provider);
        // Publish ownership before invoking the provider. A crash during ensure
        // is therefore recoverable through the same idempotency owner reference.
        store.create(&OperationRecord {
            owner_run_ref: lifecycle.owner_run_ref.clone(),
            operation: "provider_workspace".to_string(),
            subject: component.id.clone(),
            provider: selected_provider_id.clone(),
            handle: intent.handle.clone(),
            path: None,
            source_sha: source_sha.clone(),
            cleanup_policy: "remove_on_success".to_string(),
            lifecycle_state: "provisioning".to_string(),
            terminal_disposition: None,
            finalization_status: "pending".to_string(),
            finalization_lease: None,
            finalization_lease_started_ms: None,
            attempt_count: 0,
            continuation_evidence: vec![
                "release workspace ownership persisted before provider ensure".to_string(),
            ],
            attributes: serde_json::Map::from_iter([(
                "provision_intent".to_string(),
                serde_json::json!({
                    "repo": intent.repo.clone(), "base": intent.base.clone(), "head": intent.head.clone(),
                    "task_url": intent.task_url.clone(),
                    "idempotency_key": worktree_provider_idempotency_key(&intent),
                }),
            )]),
        })?;
        let provision = worktree_provider::ensure_worktree_provision_from_config(
            &intent,
            &lifecycle,
            Some(&selected_provider),
            &config,
        )?;
        if provision.destination.ownership.provider != selected_provider {
            return Err(Error::validation_invalid_argument(
                "release.workspace",
                "provider selection changed after durable ownership was recorded",
                Some(lifecycle.owner_run_ref.clone()),
                None,
            ));
        }
        store.update(&lifecycle.owner_run_ref, |record| {
            let mut record = record.ok_or_else(|| missing_record(&lifecycle.owner_run_ref))?;
            record.provider = provider_evidence_id(&provision.destination.ownership.provider);
            record.path = Some(provision.destination.ownership.path.clone());
            record.lifecycle_state = "provisioned".to_string();
            record
                .continuation_evidence
                .push("provider workspace provisioned".to_string());
            Ok(record)
        })?;
        if let Err(validation_error) = verify_staging_workspace(&provision.destination, &source_sha)
        {
            store.update(&lifecycle.owner_run_ref, |record| {
                let mut record = record.ok_or_else(|| missing_record(&lifecycle.owner_run_ref))?;
                record.terminal_disposition = Some(
                    WorktreeProviderTerminalDisposition::Failed
                        .as_str()
                        .to_string(),
                );
                Ok(record)
            })?;
            let finalization_error = finalize_record(
                &store,
                &lifecycle.owner_run_ref,
                &provision.destination,
                &lifecycle,
                WorktreeProviderTerminalDisposition::Failed,
            )
            .err();
            return Err(Error::validation_invalid_argument(
                "release.workspace",
                match finalization_error {
                    Some(error) => format!(
                        "{validation_error}; provider reconciliation also failed: {error}"
                    ),
                    None => validation_error.to_string(),
                },
                Some(provision.destination.ownership.path.clone()),
                Some(vec![format!(
                    "Reconcile the provider-owned workspace with owner reference `{}` before retrying.",
                    lifecycle.owner_run_ref
                )]),
            ));
        }
        let mut staged = component.clone();
        staged.local_path = provision.destination.ownership.path.clone();
        let record_owner = lifecycle.owner_run_ref.clone();
        Ok(Self {
            component: staged,
            output: ReleaseWorkspaceOutput {
                kind: "provider_owned".to_string(),
                path: provision.destination.ownership.path.clone(),
                provider_id: Some(provider_evidence_id(
                    &provision.destination.ownership.provider,
                )),
                handle: Some(provision.destination.ownership.handle.clone()),
                owner_run_ref: Some(lifecycle.owner_run_ref.clone()),
                source_sha: Some(source_sha),
                final_disposition: None,
                continuation_ref: Some(lifecycle.owner_run_ref.clone()),
                finalization_error: None,
                reconciliation_ref: Some(lifecycle.owner_run_ref.clone()),
            },
            owned: Some((provision.destination, lifecycle)),
            record_owner: Some(record_owner),
            store,
        })
    }

    pub(super) fn finalize(
        &mut self,
        disposition: WorktreeProviderTerminalDisposition,
        release_pushed: bool,
    ) -> ReleaseWorkspaceOutput {
        if let Some((resolution, lifecycle)) = self.owned.as_ref() {
            let Some(owner) = self.record_owner.as_ref() else {
                self.output.final_disposition = Some("finalization_pending".to_string());
                self.output.finalization_error =
                    Some("provider-owned workspace has no durable owner record".to_string());
                return self.output.clone();
            };
            let _ = self.store.update(owner, |record| {
                let mut record = record.ok_or_else(|| missing_record(owner))?;
                record.terminal_disposition = Some(disposition.as_str().to_string());
                record.attributes.insert(
                    "release_pushed".to_string(),
                    serde_json::Value::Bool(release_pushed),
                );
                Ok(record)
            });
            match finalize_record(&self.store, owner, resolution, lifecycle, disposition) {
                Ok(_) => {
                    self.owned = None;
                    self.output.final_disposition = Some(
                        if disposition == WorktreeProviderTerminalDisposition::Succeeded {
                            "cleanup_requested".to_string()
                        } else {
                            "preserved_for_inspection".to_string()
                        },
                    );
                }
                Err(error) => {
                    self.output.final_disposition = Some("finalization_pending".to_string());
                    self.output.finalization_error = Some(error.to_string());
                }
            }
        } else {
            self.output.final_disposition = Some("not_provider_owned".to_string());
        }
        self.output.clone()
    }
}

fn provider_evidence_id(provider: &WorktreeProviderIdentity) -> String {
    match provider {
        WorktreeProviderIdentity::Native => "native".to_string(),
        WorktreeProviderIdentity::Configured(provider_id) => provider_id.clone(),
    }
}

fn provider_identity_from_evidence(provider_id: &str) -> WorktreeProviderIdentity {
    if provider_id == "native" {
        WorktreeProviderIdentity::Native
    } else {
        WorktreeProviderIdentity::Configured(provider_id.to_string())
    }
}

pub(super) fn reconcile_pending(
    roots: &homeboy_core::paths::PathRoots,
    component_id: &str,
    selector: Option<&str>,
) -> Result<Option<OperationRecord>> {
    let store = OperationRecordStore::in_roots(roots);
    let records = match selector {
        Some(owner) => store.load(owner)?.into_iter().collect(),
        None => store.pending_for_subject("provider_workspace", component_id)?,
    };
    let record = match records.as_slice() {
        [] => return Ok(None),
        [record] => record.clone(),
        _ => return Err(Error::validation_invalid_argument(
            "owner_run_ref",
            "more than one provider workspace requires reconciliation; select one with --owner-run-ref",
            None,
            Some(records.iter().map(|record| record.owner_run_ref.clone()).collect()),
        )),
    };
    if record.subject != component_id || record.operation != "provider_workspace" {
        return Err(Error::validation_invalid_argument(
            "owner_run_ref",
            "owner reference does not belong to this release component",
            Some(record.owner_run_ref),
            None,
        ));
    }
    // Completion is durable and terminal. It must be recognized before any
    // provider lookup so a removed provider or workspace cannot revive legacy
    // Git recovery behavior.
    if record.finalization_status == "completed" {
        return Ok(Some(record));
    }
    let intent = record
        .attributes
        .get("provision_intent")
        .cloned()
        .ok_or_else(|| {
            Error::validation_invalid_argument(
            "owner_run_ref",
            "operation record lacks the immutable provider provision intent needed for recovery",
            Some(record.owner_run_ref.clone()),
            None,
        )
        })
        .and_then(|value| {
            serde_json::from_value::<PersistedProvisionIntent>(value).map_err(|error| {
                Error::validation_invalid_argument(
                    "owner_run_ref",
                    format!("operation record has an invalid provider provision intent: {error}"),
                    Some(record.owner_run_ref.clone()),
                    None,
                )
            })
        })?;
    let lifecycle = WorktreeProviderLifecycleIntent {
        purpose: "release_staging".to_string(),
        owner_run_ref: record.owner_run_ref.clone(),
        cleanup_policy: WorktreeProviderCleanupPolicy::RemoveOnSuccess,
    };
    let create_intent = WorktreeProviderCreateIntent {
        handle: record.handle.clone(),
        repo: intent.repo,
        base: intent.base,
        head: intent.head,
        task_url: intent.task_url,
    };
    let config = defaults::load_config();
    // A crash may happen before ensure or after it returns but before the record
    // update. Re-running ensure with the persisted key closes both windows.
    let selected_provider = provider_identity_from_evidence(&record.provider);
    let destination = if record.path.is_none() {
        let provision = worktree_provider::ensure_worktree_provision_from_config(
            &create_intent,
            &lifecycle,
            Some(&selected_provider),
            &config,
        )?;
        if provision.destination.ownership.provider != selected_provider {
            return Err(Error::validation_invalid_argument(
                "owner_run_ref",
                "provider selection changed during workspace recovery",
                Some(record.owner_run_ref.clone()),
                None,
            ));
        }
        store.update(&record.owner_run_ref, |current| {
            let mut current = current.ok_or_else(|| missing_record(&record.owner_run_ref))?;
            current.path = Some(provision.destination.ownership.path.clone());
            current.lifecycle_state = "provisioned".to_string();
            current
                .continuation_evidence
                .push("provider workspace recovered from persisted provision intent".to_string());
            Ok(current)
        })?;
        provision.destination
    } else {
        worktree_provider::admit_worktree_provision_from_config(
            &record.handle,
            Some(&selected_provider),
            &config,
        )?
        .into_admitted(&record.handle)?
    };
    if destination.ownership.provider != selected_provider
        || record
            .path
            .as_deref()
            .is_some_and(|path| destination.ownership.path != path)
    {
        return Err(Error::validation_invalid_argument(
            "owner_run_ref",
            "provider workspace identity no longer matches its durable record",
            Some(record.owner_run_ref),
            None,
        ));
    }
    let disposition = match record.terminal_disposition.as_deref() {
        Some("succeeded")
            if record
                .attributes
                .get("release_pushed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false) =>
        {
            WorktreeProviderTerminalDisposition::Succeeded
        }
        Some("failed") => WorktreeProviderTerminalDisposition::Failed,
        Some("cancelled") => WorktreeProviderTerminalDisposition::Cancelled,
        Some("timed_out") => WorktreeProviderTerminalDisposition::TimedOut,
        _ => WorktreeProviderTerminalDisposition::Interrupted,
    };
    finalize_record(
        &store,
        &record.owner_run_ref,
        &destination,
        &lifecycle,
        disposition,
    )
    .map(Some)
}

pub(super) fn output_from_record(record: &OperationRecord) -> ReleaseWorkspaceOutput {
    ReleaseWorkspaceOutput {
        kind: "provider_owned".to_string(),
        path: record.path.clone().unwrap_or_default(),
        provider_id: Some(record.provider.clone()),
        handle: Some(record.handle.clone()),
        owner_run_ref: Some(record.owner_run_ref.clone()),
        source_sha: Some(record.source_sha.clone()),
        final_disposition: record.terminal_disposition.clone(),
        continuation_ref: Some(record.owner_run_ref.clone()),
        finalization_error: (record.finalization_status != "completed")
            .then(|| "provider workspace finalization remains pending".to_string()),
        reconciliation_ref: Some(record.owner_run_ref.clone()),
    }
}

fn missing_record(owner_run_ref: &str) -> Error {
    Error::validation_invalid_argument(
        "owner_run_ref",
        "operation record disappeared during lifecycle update",
        Some(owner_run_ref.to_string()),
        None,
    )
}

#[derive(serde::Deserialize)]
struct PersistedProvisionIntent {
    repo: String,
    base: String,
    head: String,
    task_url: String,
    #[allow(dead_code)]
    idempotency_key: String,
}

fn finalize_record(
    store: &OperationRecordStore,
    owner: &str,
    destination: &WorktreeProvisionDestination,
    lifecycle: &WorktreeProviderLifecycleIntent,
    disposition: WorktreeProviderTerminalDisposition,
) -> Result<OperationRecord> {
    match store.claim_finalization(owner)? {
        FinalizationClaim::AlreadyCompleted(record) => Ok(record),
        FinalizationClaim::InProgress(record) => Err(Error::validation_invalid_argument(
            "owner_run_ref",
            "provider workspace finalization is already in progress",
            Some(record.owner_run_ref),
            Some(vec![
                "Retry recovery after the active finalizer completes or its lease expires."
                    .to_string(),
            ]),
        )),
        FinalizationClaim::Claimed { lease, record: _ } => {
            match worktree_provider::finalize_worktree_from_config(
                &destination.ownership.handle,
                lifecycle,
                disposition,
                &defaults::load_config(),
            ) {
                Ok(WorktreeFinalizationLookup::Finalized(_)) => {
                    store.complete_finalization(owner, &lease)
                }
                Ok(WorktreeFinalizationLookup::Unsupported) => {
                    let error = Error::validation_invalid_argument(
                        "release.workspace",
                        "release staging provider does not support lifecycle finalization",
                        Some(destination.ownership.handle.clone()),
                        None,
                    );
                    let _ = store.fail_finalization(owner, &lease, error.to_string());
                    Err(error)
                }
                Ok(WorktreeFinalizationLookup::NotFound) => {
                    let error = worktree_provider::worktree_finalization_not_found_error(
                        &destination.ownership.handle,
                        &defaults::load_config(),
                    );
                    let _ = store.fail_finalization(owner, &lease, error.to_string());
                    Err(error)
                }
                Err(error) => {
                    let _ = store.fail_finalization(owner, &lease, error.to_string());
                    Err(error)
                }
            }
        }
    }
}

fn in_place_eligible(component: &Component) -> bool {
    let path = Path::new(&component.local_path);
    if !git::is_git_repo(&component.local_path)
        || git::status_porcelain(path).as_deref() != Some("")
    {
        return false;
    }
    let Some(branch) = git::current_branch(path) else {
        return false;
    };
    let default_branch = git::default_branch_name(path).unwrap_or_else(|| "main".to_string());
    if branch != default_branch {
        return false;
    }
    // Existing clean default-branch releases retain their established remote
    // synchronization preflight. Staging is only required when this checkout
    // itself cannot safely carry release mutations.
    git::head_sha(path).is_some()
}

fn verified_remote_default_sha(component: &Component) -> Result<String> {
    let path = Path::new(&component.local_path);
    let remote = git::resolve_default_remote(path);
    let branch = git::default_branch_name(path).unwrap_or_else(|| "main".to_string());
    let reference = format!("{remote}/{branch}");
    let value = git::run_git(path, &["rev-parse", "--verify", &format!("{reference}^{{commit}}")], "verify release staging source")
        .map(|value| value.trim().to_string())
        .map_err(|_| Error::validation_invalid_argument(
            "release.workspace",
            format!("release staging requires an immutable verified default-branch SHA at `{reference}`"),
            Some(component.local_path.clone()),
            Some(vec!["Fetch the default branch, then retry the release.".to_string()]),
        ))?;
    if value.is_empty() {
        return Err(Error::validation_invalid_argument(
            "release.workspace",
            "release staging source SHA was empty",
            Some(component.local_path.clone()),
            None,
        ));
    }
    Ok(value)
}

fn verify_staging_workspace(
    destination: &WorktreeProvisionDestination,
    source_sha: &str,
) -> Result<()> {
    let path = Path::new(&destination.ownership.path);
    let head = git::head_sha(path).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release.workspace",
            "provider staging workspace has no checked-out HEAD",
            Some(destination.ownership.path.clone()),
            None,
        )
    })?;
    if head != source_sha
        || git::status_porcelain(path).as_deref() != Some("")
        || git::current_branch(path).is_none()
    {
        return Err(Error::validation_invalid_argument(
            "release.workspace",
            "provider staging workspace must be a clean checked-out branch at the verified immutable source SHA",
            Some(destination.ownership.path.clone()),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{finalize_record, in_place_eligible, reconcile_pending, ReleaseWorkspace};
    use crate::release::operation_record::{OperationRecord, OperationRecordStore};

    /// The isolated home each test below installs, named as roots.
    fn test_roots() -> homeboy_core::paths::PathRoots {
        homeboy_core::paths::PathRoots::from_environment().expect("path roots")
    }

    /// The record store for the isolated home each test below installs.
    ///
    /// `with_isolated_home` establishes the home; this binds a store to it once,
    /// the same way the release boundary binds one for a whole command (#7505).
    fn test_store() -> OperationRecordStore {
        OperationRecordStore::in_roots(
            &homeboy_core::paths::PathRoots::from_environment().expect("path roots"),
        )
    }

    use homeboy_core::component::Component;
    use homeboy_core::defaults::{
        save_config, HomeboyConfig, WorktreeProviderCommands, WorktreeProviderConfig,
        WorktreeProviderKind,
    };
    use homeboy_core::git;
    use homeboy_core::worktree_provider::{
        WorktreeCleanupPolicy as WorktreeProviderCleanupPolicy, WorktreeOwnership,
        WorktreeProviderIdentity, WorktreeProvisionDestination,
        WorktreeProvisionLifecycle as WorktreeProviderLifecycleIntent,
        WorktreeTerminalDisposition as WorktreeProviderTerminalDisposition, WorktreeWorkspaceKind,
    };
    use std::path::Path;
    use std::process::Command;

    #[test]
    fn clean_default_checkout_remains_eligible_for_in_place_release() {
        let directory = tempfile::tempdir().expect("repository");
        let path = directory.path();
        for args in [
            vec!["init", "--quiet", "--initial-branch", "main"],
            vec!["config", "user.email", "test@example.invalid"],
            vec!["config", "user.name", "Homeboy Test"],
        ] {
            assert!(Command::new("git")
                .args(args)
                .current_dir(path)
                .status()
                .expect("git")
                .success());
        }
        std::fs::write(path.join("README.md"), "fixture\n").expect("write");
        assert!(Command::new("git")
            .args(["add", "."])
            .current_dir(path)
            .status()
            .expect("git add")
            .success());
        assert!(Command::new("git")
            .args(["commit", "-qm", "initial"])
            .current_dir(path)
            .status()
            .expect("git commit")
            .success());
        let component = Component {
            local_path: path.to_string_lossy().to_string(),
            ..Default::default()
        };
        assert!(in_place_eligible(&component));
        std::fs::write(path.join("README.md"), "dirty\n").expect("dirty");
        assert!(!in_place_eligible(&component));
    }

    #[test]
    fn dirty_default_checkout_stages_through_the_builtin_provider() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let remote = home.path().join("origin.git");
            let source = home.path().join("source");
            std::fs::create_dir(&source).expect("source checkout");
            assert!(Command::new("git")
                .args(["init", "--bare", remote.to_str().expect("remote path")])
                .status()
                .expect("bare remote")
                .success());
            for args in [
                vec!["init", "--quiet", "--initial-branch", "main"],
                vec!["config", "user.email", "test@example.invalid"],
                vec!["config", "user.name", "Homeboy Test"],
                vec![
                    "remote",
                    "add",
                    "origin",
                    remote.to_str().expect("remote path"),
                ],
            ] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("git")
                    .success());
            }
            std::fs::write(source.join("README.md"), "fixture\n").expect("source file");
            for args in [
                vec!["add", "."],
                vec!["commit", "-qm", "initial"],
                vec!["push", "-u", "origin", "main"],
            ] {
                assert!(Command::new("git")
                    .args(args)
                    .current_dir(&source)
                    .status()
                    .expect("git")
                    .success());
            }
            std::fs::write(source.join("README.md"), "dirty\n").expect("dirty source");
            let components = home.path().join(".config/homeboy/components");
            std::fs::create_dir_all(&components).expect("component registry");
            std::fs::write(
                components.join("fixture.json"),
                serde_json::json!({
                    "id": "fixture",
                    "local_path": source,
                    "remote_path": "fixture"
                })
                .to_string(),
            )
            .expect("component registration");
            let component = Component {
                id: "fixture".to_string(),
                local_path: source.display().to_string(),
                remote_path: "fixture".to_string(),
                remote_url: Some(remote.display().to_string()),
                ..Default::default()
            };

            let workspace = ReleaseWorkspace::select(&test_roots(), &component)
                .expect("built-in provider staging");

            assert_eq!(workspace.output.kind, "provider_owned");
            assert_eq!(workspace.output.provider_id.as_deref(), Some("native"));
            assert_ne!(workspace.component.local_path, component.local_path);
            assert_eq!(
                git::head_sha(Path::new(&workspace.component.local_path)),
                git::head_sha(&source)
            );
            assert_eq!(
                git::status_porcelain(Path::new(&workspace.component.local_path)).as_deref(),
                Some("")
            );
        });
    }

    #[test]
    fn completed_recovery_never_looks_up_a_removed_provider_or_workspace() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let record = OperationRecord {
                owner_run_ref: "release/completed".to_string(),
                operation: "provider_workspace".to_string(),
                subject: "component".to_string(),
                provider: "removed-provider".to_string(),
                handle: "deleted-workspace".to_string(),
                path: Some("/no/longer/exists".to_string()),
                source_sha: "abc".to_string(),
                cleanup_policy: "remove_on_success".to_string(),
                lifecycle_state: "finalized".to_string(),
                terminal_disposition: Some("succeeded".to_string()),
                finalization_status: "completed".to_string(),
                finalization_lease: None,
                finalization_lease_started_ms: None,
                attempt_count: 1,
                continuation_evidence: Vec::new(),
                attributes: serde_json::Map::new(),
            };
            test_store()
                .create(&record)
                .expect("persist completed record");
            assert_eq!(
                reconcile_pending(&test_roots(), "component", Some("release/completed"))
                    .expect("completed recovery must no-op")
                    .expect("completed record")
                    .finalization_status,
                "completed"
            );
            assert!(reconcile_pending(&test_roots(), "component", None)
                .expect("implicit recovery ignores historical completion")
                .is_none());
        });
    }

    #[test]
    fn active_finalization_lease_remains_pending_for_racing_recovery() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let owner = "release/finalizing";
            test_store()
                .create(&OperationRecord {
                    owner_run_ref: owner.to_string(),
                    operation: "provider_workspace".to_string(),
                    subject: "component".to_string(),
                    provider: "fixture".to_string(),
                    handle: "release-fixture".to_string(),
                    path: Some("/workspace".to_string()),
                    source_sha: "abc".to_string(),
                    cleanup_policy: "remove_on_success".to_string(),
                    lifecycle_state: "provisioned".to_string(),
                    terminal_disposition: Some("succeeded".to_string()),
                    finalization_status: "pending".to_string(),
                    finalization_lease: None,
                    finalization_lease_started_ms: None,
                    attempt_count: 0,
                    continuation_evidence: Vec::new(),
                    attributes: serde_json::Map::new(),
                })
                .expect("persist record");
            let _claim = test_store().claim_finalization(owner).expect("claim lease");
            let destination = WorktreeProvisionDestination {
                ownership: WorktreeOwnership {
                    provider: WorktreeProviderIdentity::Configured("fixture".to_string()),
                    handle: "release-fixture".to_string(),
                    path: "/workspace".to_string(),
                    kind: WorktreeWorkspaceKind::Configured,
                    branch: Some("main".to_string()),
                    task_url: None,
                    provenance: None,
                },
                exact_identity: None,
            };
            let lifecycle = WorktreeProviderLifecycleIntent {
                purpose: "release_staging".to_string(),
                owner_run_ref: owner.to_string(),
                cleanup_policy: WorktreeProviderCleanupPolicy::RemoveOnSuccess,
            };

            let error = finalize_record(
                &test_store(),
                owner,
                &destination,
                &lifecycle,
                WorktreeProviderTerminalDisposition::Succeeded,
            )
            .expect_err("racing recovery must not report finalization success");
            assert!(error.message.contains("already in progress"));
            assert_eq!(
                test_store()
                    .load(owner)
                    .expect("load")
                    .expect("record")
                    .finalization_status,
                "finalizing"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn recovery_ensures_a_pre_ensure_record_then_finalizes_and_closes_it() {
        use std::os::unix::fs::PermissionsExt;

        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("tempdir");
            let workspace = temp.path().join("workspace");
            let ensures = temp.path().join("ensures");
            let finalizations = temp.path().join("finalizations");
            let script = temp.path().join("provider");
            std::fs::write(
                &script,
                format!(
                    "#!/bin/sh\ncase \"$1\" in\nresolve) if [ -d '{workspace}' ]; then printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"release-fixture\",\"path\":\"{workspace}\",\"branch\":\"main\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'; else printf '%s\\n' '{{\"worktrees\":[]}}'; fi ;;\nensure) printf '%s\\n' \"$9\" >> '{ensures}'; mkdir -p '{workspace}'; git init -q -b main '{workspace}' ;;\nfinalize) key=\"${{10}}\"; if [ ! -f '{finalizations}' ] || ! grep -Fqx \"$key\" '{finalizations}'; then printf '%s\\n' \"$key\" >> '{finalizations}'; fi ;;\nesac\n",
                    workspace = workspace.display(),
                    ensures = ensures.display(),
                    finalizations = finalizations.display(),
                ),
            )
            .expect("write provider");
            let mut permissions = std::fs::metadata(&script).expect("metadata").permissions();
            permissions.set_mode(0o755);
            std::fs::set_permissions(&script, permissions).expect("executable");

            let mut config = HomeboyConfig::default();
            config.worktree_providers.insert(
                "fixture".to_string(),
                WorktreeProviderConfig {
                    enabled: true,
                    kind: WorktreeProviderKind::Command,
                    apply_enabled: true,
                    commands: WorktreeProviderCommands {
                        resolve: Some(vec![
                            script.display().to_string(),
                            "resolve".to_string(),
                            "{handle}".to_string(),
                        ]),
                        ensure: Some(vec![
                            script.display().to_string(),
                            "ensure".to_string(),
                            "{handle}".to_string(),
                            "{repo}".to_string(),
                            "{base}".to_string(),
                            "{head}".to_string(),
                            "{task_url}".to_string(),
                            "{idempotency_key}".to_string(),
                            "{purpose}".to_string(),
                            "{owner_run_ref}".to_string(),
                            "{cleanup_policy}".to_string(),
                        ]),
                        ..Default::default()
                    },
                    lookup_timeout_ms: 10_000,
                    mutation_timeout_ms: 30_000,
                    lookup_output_limit_bytes: 64 * 1024,
                    list_result_mapping: Some(
                        homeboy_core::defaults::WorktreeProviderListResultMapping {
                            items: "$.worktrees".to_string(),
                            handle: "$.handle".to_string(),
                            path: "$.path".to_string(),
                            branch: "$.branch".to_string(),
                            dirty: "$.safety.dirty".to_string(),
                            unpushed: "$.safety.unpushed".to_string(),
                            primary: "$.safety.primary".to_string(),
                            task_url: None,
                        },
                    ),
                },
            );
            config.settings.insert(
                homeboy_core::worktree_providers::WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY.to_string(),
                serde_json::json!({ "fixture": { "finalize": [script.display().to_string(), "finalize", "{handle}", "{purpose}", "{owner_run_ref}", "{cleanup_policy}", "{disposition}", "{owner_outcome}", "{lifecycle_state}", "{idempotency_key}"] } }),
            );
            save_config(&config).expect("save provider config");
            let owner = "release/pre-ensure";
            test_store().create(&OperationRecord {
                owner_run_ref: owner.to_string(), operation: "provider_workspace".to_string(), subject: "component".to_string(), provider: "fixture".to_string(), handle: "release-fixture".to_string(), path: None, source_sha: "abc".to_string(), cleanup_policy: "remove_on_success".to_string(), lifecycle_state: "provisioning".to_string(), terminal_disposition: None, finalization_status: "pending".to_string(), finalization_lease: None, finalization_lease_started_ms: None, attempt_count: 0, continuation_evidence: Vec::new(),
                attributes: serde_json::Map::from_iter([("provision_intent".to_string(), serde_json::json!({ "repo": "repo", "base": "main", "head": "abc", "task_url": "release/pre-ensure", "idempotency_key": "release-fixture:repo:main:abc" }))]),
            }).expect("persist pre-ensure record");

            let completed = reconcile_pending(&test_roots(), "component", Some(owner))
                .expect("recover provider workspace")
                .expect("record");
            assert_eq!(completed.finalization_status, "completed");
            let repeated = reconcile_pending(&test_roots(), "component", Some(owner))
                .expect("completed recovery is idempotent")
                .expect("record");
            assert_eq!(repeated.finalization_status, "completed");
            assert_eq!(
                std::fs::read_to_string(ensures)
                    .expect("ensure calls")
                    .lines()
                    .count(),
                1
            );
            assert_eq!(
                std::fs::read_to_string(finalizations)
                    .expect("finalize calls")
                    .lines()
                    .count(),
                1
            );
            assert!(test_store()
                .pending_for_subject("provider_workspace", "component")
                .expect("pending records")
                .is_empty());
        });
    }
}
