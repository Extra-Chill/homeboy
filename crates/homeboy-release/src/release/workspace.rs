use std::path::Path;

use crate::release::operation_record::{FinalizationClaim, OperationRecord, OperationRecordStore};
use homeboy_core::component::Component;
use homeboy_core::error::{Error, Result};
use homeboy_core::git;
use homeboy_core::worktree::{self, CleanupPolicy, WorktreeCreateOptions};
use homeboy_core::worktree_provider::WorktreeTerminalDisposition;
use uuid::Uuid;

use super::types::ReleaseWorkspaceOutput;

const NATIVE_WORKSPACE_OPERATION: &str = "native_workspace";
const NATIVE_FINALIZATION_RECEIPT_SCHEMA: &str = "homeboy/release-native-worktree-finalization/v1";

pub(super) struct ReleaseWorkspace {
    pub(super) component: Component,
    output: ReleaseWorkspaceOutput,
    owned: Option<NativeWorkspace>,
    record_owner: Option<String>,
    /// Provisioning and finalization must address the same home (#7505).
    store: OperationRecordStore,
}

struct NativeWorkspace {
    handle: String,
    path: String,
    owner_run_ref: String,
}

impl ReleaseWorkspace {
    pub(super) fn source_sha(&self) -> Option<String> {
        self.output.source_sha.clone()
    }

    pub(super) fn select(
        roots: &homeboy_core::paths::PathRoots,
        component: &Component,
        head_release: bool,
    ) -> Result<Self> {
        let store = OperationRecordStore::in_roots(roots);
        if in_place_eligible(component, head_release) {
            return Ok(Self {
                component: component.clone(),
                output: ReleaseWorkspaceOutput::in_place(&component.local_path),
                owned: None,
                record_owner: None,
                store,
            });
        }

        let source_sha = verified_remote_default_sha(component)?;
        let owner_id = Uuid::new_v4();
        let owner_run_ref = format!("release/{owner_id}");
        let branch = format!(
            "release-{}-{}-{}",
            component.id,
            &source_sha[..12],
            &owner_id.simple().to_string()[..12]
        );
        let handle = worktree::handle_for_branch(&component.id, &branch);
        let intent = NativeProvisionIntent {
            repo: component.id.clone(),
            base: source_sha.clone(),
            head: branch.clone(),
        };

        // Publish recovery authority before creating the native checkout.
        store.create(&operation_record(
            owner_run_ref.clone(),
            component.id.clone(),
            handle.clone(),
            source_sha.clone(),
            "planning",
            native_provision_intent_value(&intent),
        ))?;
        store.update(&owner_run_ref, |record| {
            let mut record = record.ok_or_else(|| missing_record(&owner_run_ref))?;
            record.lifecycle_state = "provisioning".to_string();
            record
                .continuation_evidence
                .push("native worktree creation starting".to_string());
            Ok(record)
        })?;

        let created = worktree::create(WorktreeCreateOptions {
            component_id: component.id.clone(),
            branch,
            from: Some(source_sha.clone()),
            task_url: None,
            run_id: Some(owner_run_ref.clone()),
            cleanup_policy: Some(CleanupPolicy::RemoveWhenSafe),
            require_handoff_freshness: false,
        })?;
        let handle = created.record.id;
        let path = created.record.worktree_path;
        store.update(&owner_run_ref, |record| {
            let mut record = record.ok_or_else(|| missing_record(&owner_run_ref))?;
            record.handle = handle.clone();
            record.path = Some(path.clone());
            record.lifecycle_state = "provisioned".to_string();
            record
                .continuation_evidence
                .push("native worktree provisioned".to_string());
            Ok(record)
        })?;

        let workspace = NativeWorkspace {
            handle,
            path,
            owner_run_ref: owner_run_ref.clone(),
        };
        if let Err(validation_error) = verify_staging_workspace(&workspace.path, &source_sha) {
            store.update(&owner_run_ref, |record| {
                let mut record = record.ok_or_else(|| missing_record(&owner_run_ref))?;
                record.terminal_disposition =
                    Some(WorktreeTerminalDisposition::Failed.as_str().to_string());
                Ok(record)
            })?;
            let finalization_error = finalize_record(
                &store,
                &owner_run_ref,
                &workspace,
                WorktreeTerminalDisposition::Failed,
            )
            .err();
            return Err(Error::validation_invalid_argument(
                "release.workspace",
                match finalization_error {
                    Some(error) => format!(
                        "{validation_error}; native worktree reconciliation also failed: {error}"
                    ),
                    None => validation_error.to_string(),
                },
                Some(workspace.path),
                Some(vec![format!(
                    "Reconcile native workspace owner `{owner_run_ref}` before retrying."
                )]),
            ));
        }

        let mut staged = component.clone();
        staged.local_path = workspace.path.clone();
        Ok(Self {
            component: staged,
            output: ReleaseWorkspaceOutput {
                kind: "provider_owned".to_string(),
                path: workspace.path.clone(),
                provider_id: Some("native".to_string()),
                handle: Some(workspace.handle.clone()),
                owner_run_ref: Some(owner_run_ref.clone()),
                source_sha: Some(source_sha),
                final_disposition: None,
                continuation_ref: Some(owner_run_ref.clone()),
                finalization_error: None,
                reconciliation_ref: Some(owner_run_ref.clone()),
            },
            owned: Some(workspace),
            record_owner: Some(owner_run_ref),
            store,
        })
    }

    pub(super) fn finalize(
        &mut self,
        disposition: WorktreeTerminalDisposition,
        release_pushed: bool,
    ) -> ReleaseWorkspaceOutput {
        if let Some(workspace) = self.owned.as_ref() {
            let Some(owner) = self.record_owner.as_ref() else {
                self.output.final_disposition = Some("finalization_pending".to_string());
                self.output.finalization_error =
                    Some("native workspace has no durable owner record".to_string());
                return self.output.clone();
            };
            if disposition == WorktreeTerminalDisposition::Succeeded && !release_pushed {
                self.output.final_disposition = Some("finalization_pending".to_string());
                self.output.finalization_error = Some(
                    "successful workspace cleanup requires durable publication evidence"
                        .to_string(),
                );
                return self.output.clone();
            }
            // Persist both terminal intent and publication evidence before the
            // native lifecycle mutation crosses its effect fence.
            if let Err(error) = self.store.update(owner, |record| {
                let mut record = record.ok_or_else(|| missing_record(owner))?;
                record.terminal_disposition = Some(disposition.as_str().to_string());
                record.attributes.insert(
                    "release_pushed".to_string(),
                    serde_json::Value::Bool(release_pushed),
                );
                Ok(record)
            }) {
                self.output.final_disposition = Some("finalization_pending".to_string());
                self.output.finalization_error = Some(error.to_string());
                return self.output.clone();
            }
            match finalize_record(&self.store, owner, workspace, disposition) {
                Ok(_) => {
                    self.owned = None;
                    self.output.final_disposition =
                        Some(if disposition == WorktreeTerminalDisposition::Succeeded {
                            "cleanup_requested".to_string()
                        } else {
                            "preserved_for_inspection".to_string()
                        });
                }
                Err(error) => {
                    self.output.final_disposition = Some("finalization_pending".to_string());
                    self.output.finalization_error = Some(error.to_string());
                }
            }
        } else {
            self.output.final_disposition = Some("not_native_owned".to_string());
        }
        self.output.clone()
    }
}

pub(super) fn reconcile_pending(
    roots: &homeboy_core::paths::PathRoots,
    component_id: &str,
    selector: Option<&str>,
) -> Result<Option<OperationRecord>> {
    let Some(owner) = selector else {
        return Ok(None);
    };
    let store = OperationRecordStore::in_roots(roots);
    let mut record = match store.load(owner)? {
        Some(record) => record,
        None => return Ok(None),
    };
    if record.subject != component_id || record.operation != NATIVE_WORKSPACE_OPERATION {
        return Err(Error::validation_invalid_argument(
            "owner_run_ref",
            "owner reference does not belong to this release component",
            Some(record.owner_run_ref),
            None,
        ));
    }
    // A completed exact receipt is terminal and requires no workspace lookup.
    if record.finalization_status == "completed" {
        if release_finalization_receipt_matches_record(&record) {
            return Ok(Some(record));
        }
        return Err(Error::validation_invalid_argument(
            "owner_run_ref",
            "completed native finalization record lacks an exact durable receipt",
            Some(record.owner_run_ref),
            None,
        ));
    }
    let intent = record
        .attributes
        .get("provision_intent")
        .cloned()
        .ok_or_else(|| {
            invalid_record(
                &record,
                "operation record lacks native provision intent needed for recovery",
            )
        })
        .and_then(|value| {
            serde_json::from_value::<NativeProvisionIntent>(value).map_err(|error| {
                invalid_record(
                    &record,
                    format!("operation record has invalid native provision intent: {error}"),
                )
            })
        })?;

    let native = match worktree::resolve_if_present(&record.handle)? {
        Some(native) => native,
        None if record.path.is_none() => {
            worktree::create(WorktreeCreateOptions {
                component_id: intent.repo,
                branch: intent.head,
                from: Some(intent.base),
                task_url: None,
                run_id: Some(record.owner_run_ref.clone()),
                cleanup_policy: Some(CleanupPolicy::RemoveWhenSafe),
                require_handoff_freshness: false,
            })?
            .record
        }
        None => {
            return Err(Error::validation_invalid_argument(
                "owner_run_ref",
                "native workspace recorded for recovery no longer exists",
                Some(record.owner_run_ref),
                None,
            ));
        }
    };
    if native.id != record.handle
        || native.run_id.as_deref() != Some(record.owner_run_ref.as_str())
        || record
            .path
            .as_deref()
            .is_some_and(|path| path != native.worktree_path)
    {
        return Err(invalid_record(
            &record,
            "native workspace identity no longer matches its durable record",
        ));
    }
    if record.path.is_none() {
        record = store.update(&record.owner_run_ref, |current| {
            let mut current = current.ok_or_else(|| missing_record(&record.owner_run_ref))?;
            current.path = Some(native.worktree_path.clone());
            current.lifecycle_state = "provisioned".to_string();
            current
                .continuation_evidence
                .push("native worktree recovered from persisted provision intent".to_string());
            Ok(current)
        })?;
    }
    let disposition = recovery_disposition(&record);
    if record.terminal_disposition.as_deref() != Some(disposition.as_str()) {
        record = store.update(&record.owner_run_ref, |current| {
            let mut current = current.ok_or_else(|| missing_record(&record.owner_run_ref))?;
            current.terminal_disposition = Some(disposition.as_str().to_string());
            Ok(current)
        })?;
    }
    finalize_record(
        &store,
        &record.owner_run_ref,
        &NativeWorkspace {
            handle: native.id,
            path: native.worktree_path,
            owner_run_ref: record.owner_run_ref.clone(),
        },
        disposition,
    )
    .map(Some)
}

pub(super) fn output_from_record(record: &OperationRecord) -> ReleaseWorkspaceOutput {
    ReleaseWorkspaceOutput {
        kind: "provider_owned".to_string(),
        path: record.path.clone().unwrap_or_default(),
        provider_id: Some("native".to_string()),
        handle: Some(record.handle.clone()),
        owner_run_ref: Some(record.owner_run_ref.clone()),
        source_sha: Some(record.source_sha.clone()),
        final_disposition: record.terminal_disposition.clone(),
        continuation_ref: Some(record.owner_run_ref.clone()),
        finalization_error: (record.finalization_status != "completed")
            .then(|| "native workspace finalization remains pending".to_string()),
        reconciliation_ref: Some(record.owner_run_ref.clone()),
    }
}

fn operation_record(
    owner_run_ref: String,
    subject: String,
    handle: String,
    source_sha: String,
    lifecycle_state: &str,
    provision_intent: serde_json::Value,
) -> OperationRecord {
    OperationRecord {
        owner_run_ref,
        operation: NATIVE_WORKSPACE_OPERATION.to_string(),
        subject,
        provider: "native".to_string(),
        handle,
        path: None,
        source_sha,
        cleanup_policy: "remove_on_success".to_string(),
        lifecycle_state: lifecycle_state.to_string(),
        terminal_disposition: None,
        finalization_status: "pending".to_string(),
        finalization_lease: None,
        finalization_lease_started_ms: None,
        attempt_count: 0,
        mutation_attempted: false,
        continuation_evidence: vec![
            "release workspace ownership persisted before native creation".to_string(),
        ],
        attributes: serde_json::Map::from_iter([(
            "provision_intent".to_string(),
            provision_intent,
        )]),
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

fn invalid_record(record: &OperationRecord, message: impl Into<String>) -> Error {
    Error::validation_invalid_argument(
        "owner_run_ref",
        message.into(),
        Some(record.owner_run_ref.clone()),
        None,
    )
}

#[derive(serde::Deserialize, serde::Serialize)]
struct NativeProvisionIntent {
    repo: String,
    base: String,
    head: String,
}

fn native_provision_intent_value(intent: &NativeProvisionIntent) -> serde_json::Value {
    serde_json::json!({ "repo": intent.repo, "base": intent.base, "head": intent.head })
}

fn recovery_disposition(record: &OperationRecord) -> WorktreeTerminalDisposition {
    match record.terminal_disposition.as_deref() {
        Some("succeeded")
            if record
                .attributes
                .get("release_pushed")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false) =>
        {
            WorktreeTerminalDisposition::Succeeded
        }
        Some("failed") => WorktreeTerminalDisposition::Failed,
        Some("cancelled") => WorktreeTerminalDisposition::Cancelled,
        Some("timed_out") => WorktreeTerminalDisposition::TimedOut,
        _ => WorktreeTerminalDisposition::Interrupted,
    }
}

fn finalization_idempotency_key(
    handle: &str,
    owner_run_ref: &str,
    disposition: WorktreeTerminalDisposition,
) -> String {
    format!("{handle}:{owner_run_ref}:{}", disposition.as_str())
}

fn finalize_record(
    store: &OperationRecordStore,
    owner: &str,
    workspace: &NativeWorkspace,
    disposition: WorktreeTerminalDisposition,
) -> Result<OperationRecord> {
    match store.claim_finalization(owner)? {
        FinalizationClaim::AlreadyCompleted(record) => {
            if release_finalization_receipt_matches(&record, workspace, disposition) {
                Ok(record)
            } else {
                Err(Error::validation_invalid_argument(
                    "release.workspace",
                    "completed native finalization receipt does not exactly match the request",
                    Some(owner.to_string()),
                    None,
                ))
            }
        }
        FinalizationClaim::InProgress(record) => Err(Error::validation_invalid_argument(
            "owner_run_ref",
            "native workspace finalization is already in progress",
            Some(record.owner_run_ref),
            Some(vec![
                "Retry recovery after the active finalizer completes or its lease expires."
                    .to_string(),
            ]),
        )),
        FinalizationClaim::Claimed { lease, record } => {
            if record.operation != NATIVE_WORKSPACE_OPERATION
                || record.provider != "native"
                || record.handle != workspace.handle
                || record.owner_run_ref != workspace.owner_run_ref
                || record.path.as_deref() != Some(workspace.path.as_str())
                || record.terminal_disposition.as_deref() != Some(disposition.as_str())
            {
                let error = Error::validation_invalid_argument(
                    "release.workspace",
                    "native finalization claim does not exactly match durable lifecycle intent",
                    Some(owner.to_string()),
                    None,
                );
                let _ = store.fail_finalization(owner, &lease, error.to_string());
                return Err(error);
            }
            let receipt = serde_json::json!({
                "schema": NATIVE_FINALIZATION_RECEIPT_SCHEMA,
                "provider_id": "native",
                "handle": workspace.handle,
                "path": workspace.path,
                "owner_run_ref": workspace.owner_run_ref,
                "cleanup_policy": record.cleanup_policy,
                "disposition": disposition.as_str(),
                "idempotency_key": finalization_idempotency_key(&workspace.handle, &workspace.owner_run_ref, disposition),
            });
            match worktree::finalize_provider_lifecycle_with_effect_fence(
                &workspace.handle,
                &workspace.owner_run_ref,
                disposition,
                || store.mark_mutation_attempted(owner, &lease).map(|_| ()),
            ) {
                Ok(_) => store.complete_finalization(owner, &lease, receipt),
                Err(error) => {
                    let _ = store.fail_finalization(owner, &lease, error.to_string());
                    Err(error)
                }
            }
        }
    }
}

fn release_finalization_receipt_matches(
    record: &OperationRecord,
    workspace: &NativeWorkspace,
    disposition: WorktreeTerminalDisposition,
) -> bool {
    record.operation == NATIVE_WORKSPACE_OPERATION
        && record.provider == "native"
        && record.handle == workspace.handle
        && record.path.as_deref() == Some(workspace.path.as_str())
        && record.owner_run_ref == workspace.owner_run_ref
        && record.terminal_disposition.as_deref() == Some(disposition.as_str())
        && release_finalization_receipt_matches_record(record)
}

fn release_finalization_receipt_matches_record(record: &OperationRecord) -> bool {
    if record.operation != NATIVE_WORKSPACE_OPERATION
        || record.provider != "native"
        || record.cleanup_policy != "remove_on_success"
    {
        return false;
    }
    let Some(receipt) = record.attributes.get("finalization_receipt") else {
        return false;
    };
    let Some(path) = record.path.as_deref() else {
        return false;
    };
    let Some(disposition) = record.terminal_disposition.as_deref() else {
        return false;
    };
    receipt["schema"] == NATIVE_FINALIZATION_RECEIPT_SCHEMA
        && receipt["provider_id"] == "native"
        && receipt["handle"] == record.handle
        && receipt["path"] == path
        && receipt["owner_run_ref"] == record.owner_run_ref
        && receipt["cleanup_policy"] == record.cleanup_policy
        && receipt["disposition"] == disposition
        && receipt["idempotency_key"]
            == finalization_idempotency_key(
                &record.handle,
                &record.owner_run_ref,
                recovery_disposition(record),
            )
}

fn in_place_eligible(component: &Component, head_release: bool) -> bool {
    let path = Path::new(&component.local_path);
    if !git::is_git_repo(&component.local_path) {
        return false;
    }
    if head_release {
        return git::head_sha(path).is_some();
    }
    if git::status_porcelain(path).as_deref() != Some("") {
        return false;
    }
    let Some(branch) = git::current_branch(path) else {
        return false;
    };
    let default_branch = git::default_branch_name(path).unwrap_or_else(|| "main".to_string());
    branch == default_branch && git::head_sha(path).is_some()
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

fn verify_staging_workspace(path: &str, source_sha: &str) -> Result<()> {
    let path = Path::new(path);
    let head = git::head_sha(path).ok_or_else(|| {
        Error::validation_invalid_argument(
            "release.workspace",
            "native staging workspace has no checked-out HEAD",
            Some(path.display().to_string()),
            None,
        )
    })?;
    if head != source_sha
        || git::status_porcelain(path).as_deref() != Some("")
        || git::current_branch(path).is_none()
    {
        return Err(Error::validation_invalid_argument(
            "release.workspace",
            "native staging workspace must be a clean checked-out branch at the verified immutable source SHA",
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        finalize_record, in_place_eligible, operation_record, reconcile_pending,
        NativeProvisionIntent, NativeWorkspace, ReleaseWorkspace, WorktreeTerminalDisposition,
    };
    use crate::release::operation_record::OperationRecordStore;
    use homeboy_core::component::Component;
    use homeboy_core::git;
    use homeboy_core::worktree;
    use std::path::Path;
    use std::process::Command;

    fn test_roots() -> homeboy_core::paths::PathRoots {
        homeboy_core::paths::PathRoots::from_environment().expect("path roots")
    }

    fn test_store() -> OperationRecordStore {
        OperationRecordStore::in_roots(&test_roots())
    }

    fn git(path: &Path, args: &[&str]) {
        assert!(Command::new("git")
            .args(args)
            .current_dir(path)
            .status()
            .expect("git")
            .success());
    }

    fn fixture_component(home: &tempfile::TempDir) -> Component {
        let remote = home.path().join("origin.git");
        let source = home.path().join("source");
        std::fs::create_dir(&source).expect("source checkout");
        assert!(Command::new("git")
            .args(["init", "--bare", remote.to_str().expect("remote path")])
            .status()
            .expect("bare remote")
            .success());
        git(&source, &["init", "--quiet", "--initial-branch", "main"]);
        git(&source, &["config", "user.email", "test@example.invalid"]);
        git(&source, &["config", "user.name", "Homeboy Test"]);
        git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                remote.to_str().expect("remote path"),
            ],
        );
        std::fs::write(source.join("README.md"), "fixture\n").expect("source file");
        git(&source, &["add", "."]);
        git(&source, &["commit", "-qm", "initial"]);
        git(&source, &["push", "-u", "origin", "main"]);
        let components = home.path().join(".config/homeboy/components");
        std::fs::create_dir_all(&components).expect("component registry");
        std::fs::write(
            components.join("fixture.json"),
            serde_json::json!({
                "id": "fixture", "local_path": source, "remote_path": "fixture"
            })
            .to_string(),
        )
        .expect("component registration");
        Component {
            id: "fixture".to_string(),
            local_path: source.display().to_string(),
            remote_path: "fixture".to_string(),
            remote_url: Some(remote.display().to_string()),
            ..Default::default()
        }
    }

    #[test]
    fn clean_default_checkout_remains_eligible_for_in_place_release() {
        let directory = tempfile::tempdir().expect("repository");
        let path = directory.path();
        git(path, &["init", "--quiet", "--initial-branch", "main"]);
        git(path, &["config", "user.email", "test@example.invalid"]);
        git(path, &["config", "user.name", "Homeboy Test"]);
        std::fs::write(path.join("README.md"), "fixture\n").expect("write");
        git(path, &["add", "."]);
        git(path, &["commit", "-qm", "initial"]);
        let component = Component {
            local_path: path.display().to_string(),
            ..Default::default()
        };
        assert!(in_place_eligible(&component, false));
        std::fs::write(path.join("README.md"), "dirty\n").expect("dirty");
        assert!(!in_place_eligible(&component, false));
    }

    #[test]
    fn dirty_default_checkout_stages_through_native_worktree() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let component = fixture_component(home);
            std::fs::write(
                Path::new(&component.local_path).join("README.md"),
                "dirty\n",
            )
            .expect("dirty source");
            let workspace =
                ReleaseWorkspace::select(&test_roots(), &component, false).expect("native staging");
            let owner = workspace
                .output
                .owner_run_ref
                .as_deref()
                .expect("release owner");
            let record = test_store().load(owner).expect("load").expect("record");
            assert_eq!(record.operation, "native_workspace");
            assert_eq!(record.provider, "native");
            assert_eq!(workspace.output.provider_id.as_deref(), Some("native"));
            assert!(workspace
                .output
                .handle
                .as_deref()
                .is_some_and(|handle| handle.starts_with("fixture@release-fixture-")));
            assert_eq!(
                worktree::resolve(workspace.output.handle.as_deref().expect("handle"))
                    .expect("native record")
                    .run_id
                    .as_deref(),
                Some(owner)
            );
            assert_eq!(
                git::head_sha(Path::new(&workspace.component.local_path)),
                git::head_sha(
                    Path::new(&component.local_path)
                        .join(".git")
                        .parent()
                        .expect("source root")
                )
            );
            assert_eq!(
                git::status_porcelain(Path::new(&workspace.component.local_path)).as_deref(),
                Some("")
            );
        });
    }

    #[test]
    fn completed_native_recovery_does_not_look_up_the_workspace() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let owner = "release/completed";
            let mut record = operation_record(
                owner.to_string(),
                "component".to_string(),
                "deleted-workspace".to_string(),
                "abc".to_string(),
                "provisioned",
                serde_json::json!({"repo":"component","base":"abc","head":"release"}),
            );
            record.path = Some("/no/longer/exists".to_string());
            record.terminal_disposition = Some("succeeded".to_string());
            record.finalization_status = "completed".to_string();
            record
                .attributes
                .insert("release_pushed".to_string(), serde_json::Value::Bool(true));
            record.attributes.insert("finalization_receipt".to_string(), serde_json::json!({
                "schema": "homeboy/release-native-worktree-finalization/v1", "provider_id": "native",
                "handle": "deleted-workspace", "path": "/no/longer/exists", "owner_run_ref": owner,
                "cleanup_policy": "remove_on_success", "disposition": "succeeded",
                "idempotency_key": "deleted-workspace:release/completed:succeeded"
            }));
            test_store().create(&record).expect("persist record");
            assert_eq!(
                reconcile_pending(&test_roots(), "component", Some(owner))
                    .expect("no lookup")
                    .expect("record")
                    .finalization_status,
                "completed"
            );
        });
    }

    #[test]
    fn recovery_is_explicit_and_preserves_historical_native_workspaces() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let record = operation_record(
                "release/old".to_string(),
                "component".to_string(),
                "missing".to_string(),
                "abc".to_string(),
                "provisioning",
                serde_json::json!({"repo":"component","base":"abc","head":"release"}),
            );
            test_store().create(&record).expect("persist record");
            assert!(reconcile_pending(&test_roots(), "component", None)
                .expect("implicit recovery")
                .is_none());
            assert_eq!(
                test_store()
                    .for_subject("native_workspace", "component", true)
                    .expect("pending")
                    .len(),
                1
            );
        });
    }

    #[test]
    fn active_finalization_lease_remains_pending_for_racing_recovery() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let owner = "release/finalizing";
            let mut record = operation_record(
                owner.to_string(),
                "component".to_string(),
                "release-fixture".to_string(),
                "abc".to_string(),
                "provisioned",
                serde_json::json!({"repo":"component","base":"abc","head":"release"}),
            );
            record.path = Some("/workspace".to_string());
            record.terminal_disposition = Some("succeeded".to_string());
            test_store().create(&record).expect("persist record");
            let _claim = test_store().claim_finalization(owner).expect("claim lease");
            let error = finalize_record(
                &test_store(),
                owner,
                &NativeWorkspace {
                    handle: "release-fixture".to_string(),
                    path: "/workspace".to_string(),
                    owner_run_ref: owner.to_string(),
                },
                WorktreeTerminalDisposition::Succeeded,
            )
            .expect_err("racing recovery remains pending");
            assert!(error.message.contains("already in progress"));
        });
    }

    #[test]
    fn recovery_creates_from_persisted_native_intent_then_finalizes_once() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let component = fixture_component(home);
            let source_sha = git::head_sha(Path::new(&component.local_path)).expect("source sha");
            let owner = "release/pre-create";
            let branch = "release-fixture-recovery";
            let handle = worktree::handle_for_branch(&component.id, branch);
            test_store()
                .create(&operation_record(
                    owner.to_string(),
                    component.id.clone(),
                    handle.clone(),
                    source_sha.clone(),
                    "provisioning",
                    serde_json::to_value(NativeProvisionIntent {
                        repo: component.id.clone(),
                        base: source_sha,
                        head: branch.to_string(),
                    })
                    .expect("intent"),
                ))
                .expect("persist pre-create record");
            let completed = reconcile_pending(&test_roots(), &component.id, Some(owner))
                .expect("recover")
                .expect("record");
            assert_eq!(completed.finalization_status, "completed");
            assert_eq!(
                worktree::resolve(&handle)
                    .expect("native record")
                    .terminal_disposition
                    .as_deref(),
                Some("interrupted")
            );
            assert_eq!(
                reconcile_pending(&test_roots(), &component.id, Some(owner))
                    .expect("repeat")
                    .expect("record")
                    .attempt_count,
                1
            );
        });
    }
}
