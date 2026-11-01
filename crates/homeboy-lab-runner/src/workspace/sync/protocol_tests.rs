use super::protocol::*;
use super::*;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

fn fixture(transport: &Path, destination: &Path) -> CommitRequest {
    fs::create_dir_all(transport.join("0/source")).unwrap();
    fs::write(transport.join("0/source/file"), b"exact\0bytes\xff").unwrap();
    let options = RunnerWorkspaceSyncOptions {
        path: transport.join("0/source").display().to_string(),
        mode: RunnerWorkspaceSyncMode::Snapshot,
        controller_routed_git: false,
        changed_since_base: None,
        git_fetch_refs: Vec::new(),
        snapshot_includes: Vec::new(),
        allow_dirty_lab_workspace: false,
        validation_dependency_ids: None,
        run_isolation_token: None,
    };
    let source = transport.join("0/source");
    let (snapshot, _, manifest, hash) =
        snapshot_content_controlled(&source, &[], &[], &WorkspaceControl::default()).unwrap();
    let remote = destination.display().to_string();
    let metadata = workspace_metadata(WorkspaceMetadataRequest {
        runner_id: "protocol",
        local_path: &source,
        remote_path: &remote,
        sync_mode: options.mode,
        actual_materialization_mode: Some("filesystem_snapshot"),
        fallback_reason: None,
        source_transfer: None,
        snapshot_identity: &snapshot,
        snapshot_excludes: &[],
        content_manifest: Some(manifest.clone()),
        run_id: None,
        cleanup_policy: ResourceCleanupPolicy::DeleteOnSuccess,
    });
    fs::create_dir(transport.join("blobs")).unwrap();
    fs::rename(
        source.join("file"),
        transport
            .join("blobs")
            .join(super::blobs::key(manifest.entries[0].sha256.as_deref().unwrap()).unwrap()),
    )
    .unwrap();
    fs::remove_dir(&source).unwrap();
    CommitRequest {
        lab_contract_version: homeboy_lab_contract::LAB_CONTRACT_VERSION,
        roots: vec![CommitRoot {
            summary: RootSummary {
                remote_path: remote,
                object_cache: String::new(),
                manifest,
                content_hash: hash,
            },
            metadata,
            install_git: None,
            prepared_git: false,
            source_evidence: None,
        }],
    }
}

#[test]
fn commit_rejects_corrupt_bytes_before_publishing_anything() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let destination = root.path().join("workspace");
    let request = fixture(incoming.path(), &destination);
    write_json(&incoming.path().join("commit.json"), &request).unwrap();
    let blob = fs::read_dir(incoming.path().join("blobs"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    fs::write(blob, b"corrupt").unwrap();
    assert!(
        format!("{:?}", materialization_commit(incoming.path()).unwrap_err())
            .contains("digest mismatch")
    );
    assert!(!destination.exists());
    assert!(!Path::new(&workspace_metadata_path(destination.to_str().unwrap())).exists());
}

#[test]
fn interrupted_commit_replay_converges_without_replacing_another_lease() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let destination = root.path().join("workspace");
    let mut request = fixture(incoming.path(), &destination);
    write_json(&incoming.path().join("commit.json"), &request).unwrap();
    let cancelled = Arc::new(AtomicBool::new(false));
    let stop = cancelled.clone();
    let installed = destination.clone();
    let control =
        WorkspaceControl::new(homeboy_core::cooperative_control::CooperativeControl::new(
            Instant::now() + Duration::from_secs(30),
            Arc::new(move || stop.load(Ordering::SeqCst)),
        ))
        .with_checkpoint_observer(Arc::new(move || {
            if installed.exists() {
                cancelled.store(true, Ordering::SeqCst);
                return Err(io("cancelled after publish"));
            }
            Ok(())
        }));
    assert!(materialization_commit_controlled(incoming.path(), &control).is_err());
    assert!(destination.exists());
    assert!(
        Path::new(&workspace_metadata_path(destination.to_str().unwrap())).exists(),
        "metadata precedes visibility"
    );
    // A reconnect re-sends the immutable stream. The existing complete tree is
    // verified, not replaced, and the same lease remains authoritative.
    // Blob replay recreates the temporary assembled root.

    materialization_commit(incoming.path()).unwrap();
    assert_eq!(
        fs::read(destination.join("file")).unwrap(),
        b"exact\0bytes\xff"
    );
    request.roots[0].metadata.workspace_lease = Some("workspace:another-job".into());
    write_json(&incoming.path().join("commit.json"), &request).unwrap();
    assert!(
        format!("{:?}", materialization_commit(incoming.path()).unwrap_err())
            .contains("another lease")
    );
}

#[test]
fn two_root_commit_reuses_review_dependency_without_lease_metadata() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let candidate = root.path().join("candidate");
    let dependency = root.path().join("dependency");
    // The Git/review path materializes dependency snapshots and source evidence
    // but does not create a RunnerWorkspaceMetadata record for that sibling.
    fs::create_dir(&dependency).unwrap();
    fs::write(dependency.join("file"), b"old prepared dependency").unwrap();
    let mut request = fixture(incoming.path(), &candidate);
    let mut second: CommitRoot =
        serde_json::from_value(serde_json::to_value(&request.roots[0]).unwrap()).unwrap();
    second.summary.remote_path = dependency.display().to_string();
    second.metadata.remote_path = second.summary.remote_path.clone();
    second.source_evidence = Some("dependency-source-evidence".into());
    request.roots.push(second);
    write_json(&incoming.path().join("commit.json"), &request).unwrap();
    materialization_commit(incoming.path()).unwrap();
    for path in [&candidate, &dependency] {
        assert_eq!(fs::read(path.join("file")).unwrap(), b"exact\0bytes\xff");
    }
    let evidence = homeboy_lab_contract::lab::handoff::runner_workspace_lab_source_evidence_path(
        dependency.to_str().unwrap(),
    );
    assert_eq!(
        fs::read_to_string(evidence).unwrap(),
        "dependency-source-evidence"
    );
    // Repeated Cook preflights accept the same complete sibling too.
    materialization_commit(incoming.path()).unwrap();
}

#[test]
fn commit_refuses_contract_drift_since_plan() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let destination = root.path().join("workspace");
    let mut request = fixture(incoming.path(), &destination);
    request.lab_contract_version += 1;
    write_json(&incoming.path().join("commit.json"), &request).unwrap();
    let error = format!("{:?}", materialization_commit(incoming.path()).unwrap_err());
    assert!(error.contains(&format!(
        "Lab contract {} != {}",
        request.lab_contract_version,
        homeboy_lab_contract::LAB_CONTRACT_VERSION
    )));
    assert!(!destination.exists());
}

#[test]
fn plan_capacity_response_includes_aggregate_requirements() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let commit = fixture(incoming.path(), &root.path().join("workspace"));
    let request = PlanRequest {
        lab_contract_version: commit.lab_contract_version,
        roots: commit.roots.into_iter().map(|root| root.summary).collect(),
    };
    write_json(&incoming.path().join("plan.json"), &request).unwrap();
    let response: PlanResponse =
        serde_json::from_value(materialization_plan(&incoming.path().join("plan.json")).unwrap())
            .unwrap();
    assert_eq!(response.tips.len(), 1);
    assert_eq!(
        response.missing,
        super::blobs::inventory(&request.roots)
            .unwrap()
            .into_keys()
            .collect::<Vec<_>>()
    );
    let runner = crate::Runner {
        id: "capacity".into(),
        ..serde_json::from_value::<crate::Runner>(
            serde_json::json!({"id":"capacity", "kind":"local"}),
        )
        .unwrap()
    };
    let probe = SnapshotFilesystemProbe {
        identity: "runner:capacity".into(),
        path: root.path().display().to_string(),
        role: "runner snapshot destination",
        available_bytes: 1,
        available_inodes: 1,
    };
    let requirement = SnapshotFilesystemRequirement {
        bytes: 2,
        inodes: 2,
    };
    assert!(!snapshot_probe_has_capacity(&probe, requirement));
    let refusal = snapshot_capacity_error(&probe, requirement, &runner, None);
    assert_eq!(refusal.details["available_bytes"], 1);
    assert_eq!(refusal.retryable, Some(true));
}

#[test]
fn plan_accepts_different_build_with_same_contract_and_refuses_contract_mismatch() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let destination = root.path().join("workspace");
    let commit = fixture(incoming.path(), &destination);
    let version = homeboy_lab_contract::LAB_CONTRACT_VERSION;
    // Keep the old identity field in this wire fixture: before contract-based
    // compatibility this request failed with "runner build identity mismatch".
    let mut request = serde_json::json!({
        "identity": "homeboy 0.0.0+different-build",
        "lab_contract_version": version,
        "roots": commit.roots.into_iter().map(|root| root.summary).collect::<Vec<_>>(),
    });
    let input = incoming.path().join("plan.json");
    write_json(&input, &request).unwrap();
    let response = materialization_plan_controlled(&input, &WorkspaceControl::default()).unwrap();
    assert_eq!(response["lab_contract_version"], version);
    assert!(response.get("identity").is_none());
    assert!(!destination.exists());

    request["lab_contract_version"] = (version + 1).into();
    write_json(&input, &request).unwrap();
    let error = materialization_plan_controlled(&input, &WorkspaceControl::default()).unwrap_err();
    assert!(format!("{error:?}").contains(&format!("Lab contract {} != {version}", version + 1)));
    assert!(!destination.exists());
}
