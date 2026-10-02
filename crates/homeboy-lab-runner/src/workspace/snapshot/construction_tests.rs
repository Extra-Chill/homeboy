use super::*;
use homeboy_core::cooperative_control::CooperativeControl;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

fn controlled(cancelled: impl Fn() -> bool + Send + Sync + 'static) -> WorkspaceControl {
    WorkspaceControl::new(CooperativeControl::new(
        Instant::now() + Duration::from_secs(60),
        Arc::new(cancelled),
    ))
}

#[cfg(unix)]
#[test]
fn indexed_internal_links_preserve_exact_large_filesystem_manifest() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let source = tempfile::tempdir().unwrap();
    let legacy = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    fs::create_dir(source.path().join("target")).unwrap();
    fs::create_dir(source.path().join("other")).unwrap();
    fs::write(source.path().join("other/data"), b"nested target\n").unwrap();
    for file in 0..8 {
        let path = source.path().join(format!("target/file-{file}"));
        fs::write(&path, format!("payload {file}\n")).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o751)).unwrap();
    }
    symlink("../other", source.path().join("target/nested")).unwrap();
    for link in 0..512 {
        symlink("target", source.path().join(format!("link-{link:04}"))).unwrap();
    }
    // The lexical prefix of an internal link is an unrelated real directory.
    fs::create_dir(source.path().join("link-0000-sibling")).unwrap();
    fs::write(
        source.path().join("link-0000-sibling/data"),
        b"must survive\n",
    )
    .unwrap();
    symlink("target/file-0", source.path().join("file-link")).unwrap();
    symlink("unavailable", source.path().join("dangling")).unwrap();
    let manifest = snapshot_input_manifest(source.path(), &[]).unwrap();
    let control = WorkspaceControl::default();
    let index = InternalLinkIndex::discover(
        &content_hash_root(source.path()).unwrap(),
        &manifest.selection,
        &control,
    )
    .unwrap();
    assert_eq!(index.0.len(), 1026);
    assert!(!index.below(Path::new("link-0000-sibling/data")));
    assert!(index.below(Path::new("link-0000/nested/data")));
    let links = index.0.iter().cloned().collect::<Vec<_>>();
    let old_probes = AtomicUsize::new(0);
    let old_contains = |entry: &SnapshotSelectionEntry| {
        links.iter().any(|link| {
            old_probes.fetch_add(1, Ordering::Relaxed);
            entry.relative == *link
        })
    };
    let old_below = |entry: &SnapshotSelectionEntry| {
        links.iter().any(|link| {
            old_probes.fetch_add(1, Ordering::Relaxed);
            entry.relative != *link && entry.relative.starts_with(link)
        })
    };
    let before = Instant::now();
    materialize_selection_with_lookup(legacy.path(), &manifest, &control, old_contains, old_below)
        .unwrap();
    let legacy_elapsed = before.elapsed();
    let after = Instant::now();
    let stage = materialize_snapshot_stage(source.path(), &manifest, Some(scratch.path())).unwrap();
    let indexed_elapsed = after.elapsed();
    let staged = stage.path().join("source");
    let current = snapshot_stable_manifest(source.path(), &[]).unwrap();
    assert_eq!(manifest.stable_manifest, current);
    assert_eq!(snapshot_stable_manifest(&staged, &[]).unwrap(), current);
    assert_eq!(
        snapshot_stable_manifest(legacy.path(), &[]).unwrap(),
        current
    );
    assert_eq!(
        fs::read_link(staged.join("link-0000")).unwrap(),
        Path::new("target")
    );
    assert_eq!(
        fs::read_link(staged.join("target/nested")).unwrap(),
        Path::new("../other")
    );
    assert_eq!(
        fs::read_link(staged.join("dangling")).unwrap(),
        Path::new("unavailable")
    );
    assert_eq!(
        fs::metadata(staged.join("target/file-0"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o751
    );
    // Deterministic upper bound for all four construction passes, independent
    // of the fixture's symlink cardinality and filesystem scheduling.
    let indexed_probe_bound: usize = manifest
        .selection
        .iter()
        .map(|entry| 4 * (1 + entry.relative.ancestors().skip(1).count()))
        .sum();
    assert!(old_probes.load(Ordering::Relaxed) > indexed_probe_bound * 20);
    eprintln!("snapshot construction fixture: manifest_entries={} selected_entries={} internal_links={} legacy_us={} indexed_us={} legacy_probes={} indexed_probe_bound={}",
        current.inventory.entry_count, manifest.selection.len(), index.0.len(), legacy_elapsed.as_micros(), indexed_elapsed.as_micros(), old_probes.load(Ordering::Relaxed), indexed_probe_bound);
    drop(stage);
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[test]
fn cancellation_during_large_file_copy_cleans_only_owned_scratch() {
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let large = source.path().join("large");
    let file = fs::File::create(&large).unwrap();
    file.set_len((SNAPSHOT_COPY_CHUNK_BYTES * 128) as u64)
        .unwrap();
    fs::create_dir(scratch.path().join("unrelated-owner")).unwrap();
    fs::write(scratch.path().join("unrelated-owner/receipt"), b"active").unwrap();
    let manifest = snapshot_input_manifest(source.path(), &[]).unwrap();
    let scratch_path = scratch.path().to_path_buf();
    let observed_bytes = Arc::new(AtomicUsize::new(0));
    let observed = observed_bytes.clone();
    let control = controlled(move || {
        for entry in fs::read_dir(&scratch_path).unwrap().flatten() {
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with("homeboy-snapshot-stage-")
            {
                continue;
            }
            if let Ok(metadata) = fs::metadata(entry.path().join("source/large")) {
                if metadata.len() >= (SNAPSHOT_COPY_CHUNK_BYTES * 2) as u64 {
                    observed.store(metadata.len() as usize, Ordering::SeqCst);
                    return true;
                }
            }
        }
        false
    });
    let error = materialize_snapshot_stage_controlled(
        source.path(),
        &manifest,
        Some(scratch.path()),
        &control,
    )
    .unwrap_err();
    assert_eq!(error.details["workspace_sync"]["cancelled"], true);
    assert_ne!(error.details["classification"], "snapshot_construction");
    assert_eq!(
        observed_bytes.load(Ordering::SeqCst),
        SNAPSHOT_COPY_CHUNK_BYTES * 2
    );
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 1);
    assert_eq!(
        fs::read(scratch.path().join("unrelated-owner/receipt")).unwrap(),
        b"active"
    );
    assert_eq!(
        fs::metadata(large).unwrap().len(),
        (SNAPSHOT_COPY_CHUNK_BYTES * 128) as u64
    );
}

#[test]
fn cancellation_interrupts_manifest_hashing_inside_one_large_file() {
    let source = tempfile::tempdir().unwrap();
    fs::File::create(source.path().join("large"))
        .unwrap()
        .set_len((SNAPSHOT_COPY_CHUNK_BYTES * 128) as u64)
        .unwrap();
    let checkpoints = Arc::new(AtomicUsize::new(0));
    let observed = checkpoints.clone();
    let control = controlled(move || observed.fetch_add(1, Ordering::SeqCst) >= 12);
    let error = snapshot_input_manifest_controlled(source.path(), &[], &control).unwrap_err();
    assert_eq!(error.details["workspace_sync"]["cancelled"], true);
    assert_eq!(checkpoints.load(Ordering::SeqCst), 13);
}

#[test]
fn cancellation_interrupts_internal_link_discovery_before_copying() {
    let source = tempfile::tempdir().unwrap();
    for index in 0..64 {
        fs::write(source.path().join(format!("file-{index}")), b"source").unwrap();
    }
    let manifest = snapshot_input_manifest(source.path(), &[]).unwrap();
    let checkpoints = Arc::new(AtomicUsize::new(0));
    let observed = checkpoints.clone();
    let control = controlled(move || observed.fetch_add(1, Ordering::SeqCst) >= 16);
    let error = match InternalLinkIndex::discover(source.path(), &manifest.selection, &control) {
        Ok(_) => panic!("link discovery ignored cancellation"),
        Err(error) => error,
    };
    assert_eq!(error.details["workspace_sync"]["cancelled"], true);
    assert_eq!(checkpoints.load(Ordering::SeqCst), 17);
}

#[test]
fn transfer_cancellation_retains_uncertain_effect_without_stopping_unrelated_execution() {
    use homeboy_engine_primitives::command::ExecutionOwner;
    use std::process::Command;
    let fixture = tempfile::tempdir().unwrap();
    let accepted = fixture.path().join("transfer-accepted");
    let observed = accepted.clone();
    let control = controlled(move || observed.exists());
    let mut unrelated = ExecutionOwner::spawn(Command::new("sleep").arg("60")).unwrap();
    let error = control
        .shell(
            &format!(
                "printf accepted > {}; sleep 60",
                shell::quote_arg(&accepted.display().to_string())
            ),
            "transfer fixture",
        )
        .unwrap_err();
    assert_eq!(error.details["workspace_sync"]["cancelled"], true);
    assert_eq!(error.details["workspace_sync"]["command_started"], true);
    assert_eq!(
        error.details["workspace_sync"]["remote_effect_uncertain"],
        true
    );
    assert_eq!(fs::read(accepted).unwrap(), b"accepted");
    assert!(
        unrelated.try_wait().unwrap().is_none(),
        "transfer cleanup stopped an unrelated owned execution"
    );
    unrelated.drain_and_reap().unwrap();
}

#[test]
fn expired_construction_budget_never_dispatches_target() {
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    let marker = scratch.path().join("provider-ran");
    fs::write(source.path().join("source"), b"source").unwrap();
    let control = WorkspaceControl::before(Some(Instant::now()));
    let error = materialize_snapshot_piped_controlled(
        source.path(),
        &format!("touch {}", shell::quote_arg(&marker.display().to_string())),
        &[],
        "deadline fixture",
        Some(scratch.path()),
        &control,
    )
    .unwrap_err();
    assert_eq!(error.details["workspace_sync"]["timed_out"], true);
    assert_ne!(error.details["workspace_sync"]["cancelled"], true);
    assert!(!marker.exists());
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
}

#[test]
fn deadline_expiring_during_copy_retains_timeout_and_cleans_scratch() {
    let source = tempfile::tempdir().unwrap();
    let scratch = tempfile::tempdir().unwrap();
    fs::File::create(source.path().join("large"))
        .unwrap()
        .set_len((SNAPSHOT_COPY_CHUNK_BYTES * 128) as u64)
        .unwrap();
    let manifest = snapshot_input_manifest(source.path(), &[]).unwrap();
    let scratch_path = scratch.path().to_path_buf();
    let deadline = Instant::now() + Duration::from_millis(100);
    let control = WorkspaceControl::new(CooperativeControl::new(
        deadline,
        Arc::new(move || {
            if fs::read_dir(&scratch_path).unwrap().flatten().any(|entry| {
                fs::metadata(entry.path().join("source/large"))
                    .is_ok_and(|metadata| metadata.len() > 0)
            }) {
                // Drive the clock across the real deadline at a deterministic work
                // boundary rather than hoping a fixture runs slowly enough.
                while Instant::now() < deadline {
                    std::thread::yield_now();
                }
            }
            false
        }),
    ));
    let error = materialize_snapshot_stage_controlled(
        source.path(),
        &manifest,
        Some(scratch.path()),
        &control,
    )
    .unwrap_err();
    assert_eq!(error.details["workspace_sync"]["timed_out"], true);
    assert_ne!(error.details["workspace_sync"]["cancelled"], true);
    assert_eq!(fs::read_dir(scratch.path()).unwrap().count(), 0);
}
