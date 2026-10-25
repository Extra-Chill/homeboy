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
    let (snapshot, _, manifest, hash) = snapshot_identity_manifest_and_hash_controlled(
        &source,
        &[],
        &[],
        &WorkspaceControl::default(),
    )
    .unwrap();
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
    CommitRequest {
        identity: homeboy_product_identity::build_identity().display,
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
    fs::write(incoming.path().join("0/source/file"), b"corrupt").unwrap();
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
    fs::create_dir(incoming.path().join("0/source")).unwrap();
    fs::write(incoming.path().join("0/source/file"), b"exact\0bytes\xff").unwrap();
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
fn commit_refuses_identity_drift_since_plan() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let destination = root.path().join("workspace");
    let mut request = fixture(incoming.path(), &destination);
    request.identity = "homeboy 0.0.0+stale".into();
    write_json(&incoming.path().join("commit.json"), &request).unwrap();
    assert!(
        format!("{:?}", materialization_commit(incoming.path()).unwrap_err())
            .contains("identity changed")
    );
    assert!(!destination.exists());
}

#[test]
fn plan_capacity_response_includes_aggregate_requirements() {
    let root = tempfile::tempdir().unwrap();
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    let commit = fixture(incoming.path(), &root.path().join("workspace"));
    let request = PlanRequest {
        identity: commit.identity,
        roots: commit.roots.into_iter().map(|root| root.summary).collect(),
    };
    write_json(&incoming.path().join("plan.json"), &request).unwrap();
    let response: PlanResponse =
        serde_json::from_value(materialization_plan(&incoming.path().join("plan.json")).unwrap())
            .unwrap();
    assert_eq!(response.tips.len(), 1);
    assert_eq!(response.missing[0], request.roots[0].manifest.entries);
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
    let requirement = snapshot_filesystem_requirement(2, 2);
    assert!(!snapshot_probe_has_capacity(&probe, requirement));
    let refusal = snapshot_capacity_error(&probe, requirement, &runner, None);
    assert_eq!(refusal.details["available_bytes"], 1);
    assert_eq!(refusal.retryable, Some(true));
}

#[test]
fn plan_identity_mismatch_stops_before_commit() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().unwrap();
        let runner_root = tempfile::tempdir().unwrap();
        fs::write(source.path().join("input"), b"source").unwrap();
        crate::create(&serde_json::json!({"id":"protocol-identity", "kind":"local", "workspace_root":runner_root.path()}).to_string(), false).unwrap();
        let options = RunnerWorkspaceSyncOptions {
            path: source.path().display().to_string(),
            mode: RunnerWorkspaceSyncMode::Snapshot,
            controller_routed_git: false,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
            validation_dependency_ids: None,
            run_isolation_token: None,
        };
        for identity in ["homeboy 0.0.0+abcdef1", "homeboy 99.0.0+abcdef1"] {
            INVOCATIONS.with(|counter| counter.set(0));
            let result =
                crate::controller_identity::with_converging_controller(Some(identity), || {
                    sync_workspace_batch_controlled(
                        "protocol-identity",
                        vec![(options.clone(), Vec::new())],
                        &WorkspaceControl::default(),
                    )
                });
            assert!(format!("{:?}", result.unwrap_err()).contains("identity mismatch"));
            INVOCATIONS.with(|counter| assert_eq!(counter.get(), 1));
        }
        assert!(!runner_root.path().join("_lab_workspaces").exists());
    });
}
