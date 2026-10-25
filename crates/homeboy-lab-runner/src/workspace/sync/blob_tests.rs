use super::*;

fn options(path: &Path) -> RunnerWorkspaceSyncOptions {
    RunnerWorkspaceSyncOptions {
        path: path.display().to_string(),
        mode: RunnerWorkspaceSyncMode::SnapshotGit,
        controller_routed_git: false,
        changed_since_base: None,
        git_fetch_refs: Vec::new(),
        snapshot_includes: Vec::new(),
        allow_dirty_lab_workspace: false,
        validation_dependency_ids: Some(Vec::new()),
        run_isolation_token: None,
    }
}

#[test]
fn content_addressed_stages_reuse_repair_and_preserve_exact_trees() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().unwrap();
        let extra = tempfile::tempdir().unwrap();
        let runner_root = tempfile::tempdir().unwrap();
        let git = |args: &[&str]| {
            assert!(std::process::Command::new("git")
                .args(args)
                .current_dir(source.path())
                .status()
                .unwrap()
                .success());
        };
        git(&["init", "-q"]);
        fs::write(source.path().join("dirty"), b"committed").unwrap();
        fs::write(source.path().join("deleted"), b"gone").unwrap();
        git(&["add", "."]);
        git(&[
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.com",
            "commit",
            "-qm",
            "baseline",
        ]);
        fs::write(source.path().join("dirty"), b"dirty\0\xff").unwrap();
        fs::remove_file(source.path().join("deleted")).unwrap();
        fs::write(source.path().join("untracked"), b"new\n").unwrap();
        fs::write(source.path().join("script"), b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(
            source.path().join("script"),
            fs::Permissions::from_mode(0o755),
        )
        .unwrap();
        symlink("dirty", source.path().join("link")).unwrap();
        symlink("absent", source.path().join("dangling")).unwrap();
        fs::write(extra.path().join("shared"), b"new\n").unwrap();
        crate::create(
            &serde_json::json!({"id":"blobs", "kind":"local", "workspace_root":runner_root.path()})
                .to_string(),
            false,
        )
        .unwrap();
        let stage = || {
            protocol_controller::INVOCATIONS.with(|c| c.set(0));
            let result = sync_workspace_batch_controlled(
                "blobs",
                vec![
                    (options(source.path()), vec![]),
                    (options(extra.path()), vec![]),
                ],
                &WorkspaceControl::default(),
            )
            .unwrap();
            protocol_controller::INVOCATIONS.with(|c| assert_eq!(c.get(), 2));
            for output in &result {
                assert_eq!(output.snapshot_identity, output.content.as_ref().unwrap().1);
            }
            let remote = Path::new(&result[0].remote_path);
            assert_eq!(
                fs::read(remote.join("dirty")).unwrap(),
                fs::read(source.path().join("dirty")).unwrap()
            );
            assert_eq!(fs::read(remote.join("untracked")).unwrap(), b"new\n");
            assert!(!remote.join("deleted").exists());
            assert_eq!(
                fs::read_link(remote.join("link")).unwrap(),
                Path::new("dirty")
            );
            assert_eq!(
                fs::read_link(remote.join("dangling")).unwrap(),
                Path::new("absent")
            );
            assert_eq!(
                fs::metadata(remote.join("script"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o755
            );
            assert_eq!(
                fs::read(Path::new(&result[1].remote_path).join("shared")).unwrap(),
                b"new\n"
            );
            // Only execution views and metadata live under _lab_workspaces;
            // transfer scratch is gone and no controller source stage exists.
            assert!(!fs::read_dir(runner_root.path()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp")));
            result
        };
        let first = stage();
        let stats = first[0]
            .materialization_plan
            .snapshot_transfer
            .as_ref()
            .unwrap();
        assert_eq!(stats.missing_blobs, stats.total_blobs);
        assert_eq!(stats.missing_bytes, stats.total_bytes);
        assert_eq!(
            stats.total_blobs, 3,
            "deduplicated across roots and symlink paths"
        );
        fs::write(source.path().join("dirty"), b"one changed file").unwrap();
        let second = stage();
        assert_eq!(
            second[0]
                .materialization_plan
                .snapshot_transfer
                .as_ref()
                .unwrap()
                .missing_blobs,
            1
        );
        // Mutating a build's execution inode must not mutate the shared cache.
        fs::write(
            Path::new(&first[0].remote_path).join("untracked"),
            b"build output",
        )
        .unwrap();
        let digest = second[0]
            .content
            .as_ref()
            .unwrap()
            .0
            .entries
            .iter()
            .find(|e| e.path == "dirty")
            .unwrap()
            .sha256
            .as_deref()
            .unwrap();
        let blob = blobs::store(&second[0].remote_path).join(blobs::key(digest).unwrap());
        fs::write(&blob, b"corrupt").unwrap();
        let repaired = stage();
        assert_eq!(
            repaired[0]
                .materialization_plan
                .snapshot_transfer
                .as_ref()
                .unwrap()
                .missing_blobs,
            1
        );
        assert!(blobs::valid(&blob, digest, &WorkspaceControl::default()).unwrap());
        let warm = stage();
        assert_eq!(
            warm[0]
                .materialization_plan
                .snapshot_transfer
                .as_ref()
                .unwrap()
                .missing_blobs,
            0
        );
    });
}

#[test]
fn streaming_refuses_same_size_mutation_and_has_no_commit_seal() {
    use sha2::{Digest, Sha256};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("input");
    let bytes = vec![b'a'; 256 * 1024];
    fs::write(&file, &bytes).unwrap();
    let source =
        blobs::Source::new(file.clone(), format!("sha256:{:x}", Sha256::digest(&bytes))).unwrap();
    let changed = Arc::new(AtomicUsize::new(0));
    let flag = changed.clone();
    let control = WorkspaceControl::default().with_checkpoint_observer(Arc::new(move || {
        if flag.fetch_add(1, Ordering::SeqCst) == 1 {
            fs::write(&file, vec![b'b'; 256 * 1024]).unwrap();
        }
        Ok(())
    }));
    let mut archive = tar::Builder::new(Vec::new());
    let error = source
        .append(&mut archive, Path::new("blobs/input"), &control)
        .unwrap_err();
    assert_eq!(error.details["classification"], "snapshot_construction");
    assert!(error.message.contains("refusing a mixed snapshot"));
    assert!(changed.load(Ordering::SeqCst) > 1);
    let incoming = tempfile::tempdir_in(root.path()).unwrap();
    fs::write(incoming.path().join("commit.json"), b"{}").unwrap();
    let runner: Runner = serde_json::from_value(
        serde_json::json!({"id":"stability", "kind":"local", "workspace_root":root.path()}),
    )
    .unwrap();
    let digest = source.digest.clone();
    let mut sources = std::collections::BTreeMap::new();
    sources.insert(digest.clone(), source);
    let error = protocol_controller::invoke(
        &runner,
        incoming.path(),
        Some((&sources, &[digest])),
        &WorkspaceControl::default(),
    )
    .unwrap_err();
    assert_eq!(error.details["classification"], "snapshot_construction");
}

#[test]
fn blob_retention_is_age_and_size_bounded_lru() {
    use std::time::SystemTime;
    let root = tempfile::tempdir().unwrap();
    let old = root.path().join("old");
    let older = root.path().join("older");
    let recent = root.path().join("recent");
    for path in [&old, &older, &recent] {
        fs::write(path, b"bytes").unwrap();
    }
    fs::File::open(&old)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(8 * 86400))
        .unwrap();
    fs::File::open(&older)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(60))
        .unwrap();
    // Sparse files exercise the byte cap without allocating gigabytes.
    fs::OpenOptions::new()
        .write(true)
        .open(&older)
        .unwrap()
        .set_len(11 * 1024 * 1024 * 1024)
        .unwrap();
    fs::File::open(&older)
        .unwrap()
        .set_modified(SystemTime::now() - Duration::from_secs(60))
        .unwrap();
    blobs::prune(root.path()).unwrap();
    assert!(!old.exists());
    assert!(!older.exists());
    assert!(recent.exists());
}
