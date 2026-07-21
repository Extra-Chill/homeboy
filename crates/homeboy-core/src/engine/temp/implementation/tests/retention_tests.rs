use super::*;

#[test]
fn active_invocation_lease_survives_transient_pin_loss() {
    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let run_dir = super::super::super::run_dir::RunDir::create().expect("run dir");
    let path = run_dir.path().to_path_buf();
    let invocation = super::super::super::invocation::InvocationGuard::acquire(
        &run_dir,
        &super::super::super::invocation::InvocationRequirements::default(),
    )
    .expect("invocation");
    fs::remove_file(path.join(RUNTIME_TEMP_PIN_FILE)).expect("simulate pin loss");
    retain_failed_run_dir(&path);
    let mut options = bounded_options(true, Some("homeboy-run"));
    options.older_than_days = 0;

    let protected = cleanup_runtime_tmp_bounded(options).expect("protected cleanup");
    assert_eq!(protected.removed_count, 0);
    assert!(protected.rows[0].reason.contains("invocation lease"));
    assert!(path.exists());

    drop(invocation);
    let removed = cleanup_runtime_tmp_bounded(options).expect("released cleanup");
    assert_eq!(removed.removed_count, 1);
    assert!(!path.exists());
    env::remove_var(runtime_tmpdir_env());
}

#[test]
fn reused_pid_without_matching_starttime_does_not_protect_run() {
    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let (path, pin) = managed_run_temp_dir("homeboy-run-pid-reuse").expect("managed run");
    let mut owner = read_run_owner(&path).expect("owner");
    owner.owner_pid = std::process::id();
    owner.linux_starttime_ticks = Some(
        crate::process::linux_process_starttime_ticks(std::process::id())
            .expect("process identity")
            .unwrap_or(0)
            .saturating_add(1),
    );
    owner.created_at = "2000-01-01T00:00:00Z".to_string();
    write_run_owner(&path, &owner).expect("write owner");
    drop(pin);
    let mut options = bounded_options(true, Some("homeboy-run-pid-reuse"));
    options.older_than_days = 0;

    let output = cleanup_runtime_tmp_bounded(options).expect("cleanup");
    assert_eq!(output.removed_count, 1);
    assert!(!path.exists());
    env::remove_var(runtime_tmpdir_env());
}

#[cfg(unix)]
#[test]
fn corrupt_owner_metadata_is_quarantined_then_reclaimed() {
    use std::process::Command;

    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let (path, pin) = managed_run_temp_dir("homeboy-run-corrupt").expect("managed run");
    drop(pin);
    fs::write(path.join(RUN_OWNER_FILE), b"not-json").expect("corrupt owner");

    let recent = cleanup_runtime_tmp_bounded(bounded_options(true, Some("homeboy-run-corrupt")))
        .expect("recent quarantine");
    assert_eq!(recent.removed_count, 0);
    assert!(recent.rows[0].reason.contains("quarantine grace"));

    let status = Command::new("touch")
        .args(["-d", "2 days ago"])
        .arg(&path)
        .status()
        .expect("touch directory age");
    assert!(status.success());
    let stale = cleanup_runtime_tmp_bounded(bounded_options(true, Some("homeboy-run-corrupt")))
        .expect("stale quarantine");
    assert_eq!(stale.removed_count, 1);
    assert!(stale.rows[0].reason.contains("owner metadata is corrupt"));
    assert!(!path.exists());
    env::remove_var(runtime_tmpdir_env());
}

#[test]
fn stale_crashed_run_becomes_eligible() {
    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let (path, pin) = managed_run_temp_dir("homeboy-run-crashed").expect("managed run");
    let mut owner = read_run_owner(&path).expect("owner");
    owner.owner_pid = u32::MAX;
    owner.created_at = "2000-01-01T00:00:00Z".to_string();
    write_run_owner(&path, &owner).expect("stale owner");
    drop(pin);
    fs::write(
        path.join(RUNTIME_TEMP_PIN_FILE),
        format!("{RUNTIME_TEMP_PIN_SCHEMA_LINE}\nowner_pid={}\n", u32::MAX),
    )
    .expect("dead pin");

    let output = cleanup_runtime_tmp_bounded(bounded_options(true, Some("homeboy-run")))
        .expect("cleanup stale run");
    assert_eq!(output.removed_count, 1);
    assert!(!path.exists());
    env::remove_var(runtime_tmpdir_env());
}

#[test]
fn failed_runs_converge_under_count_and_byte_bounds() {
    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let first = failed_run("homeboy-run-bound", 32);
    std::thread::sleep(Duration::from_millis(5));
    let second = failed_run("homeboy-run-bound", 64);
    let mut count_options = bounded_options(true, Some("homeboy-run-bound"));
    count_options.run_max_count = 1;
    count_options.limit = 1;

    let first_page = cleanup_runtime_tmp_bounded(count_options).expect("first count page");
    assert_eq!(first_page.totals.inspected_count, 1);
    assert_eq!(first_page.removed_count, 0);
    assert!(first_page.has_more);
    let cursor = first_page.next_cursor.expect("next cursor");
    count_options.cursor = Some(&cursor);
    let count_output = cleanup_runtime_tmp_bounded(count_options).expect("second count page");
    assert_eq!(count_output.removed_count, 1);
    assert!(first.exists() ^ second.exists());

    let mut byte_options = bounded_options(true, Some("homeboy-run-bound"));
    byte_options.run_max_bytes = 0;
    let byte_output = cleanup_runtime_tmp_bounded(byte_options).expect("byte cleanup");
    assert_eq!(byte_output.removed_count, 1);
    assert!(!first.exists() && !second.exists());
    env::remove_var(runtime_tmpdir_env());
}

#[test]
fn apply_reports_verified_bytes_and_is_idempotent() {
    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let path = failed_run("homeboy-run-accounting", 257);
    let expected = path_storage_measure(&path).expect("size").logical_bytes;
    let mut options = bounded_options(true, Some("homeboy-run-accounting"));
    options.older_than_days = 0;

    let applied = cleanup_runtime_tmp_bounded(options).expect("apply");
    assert_eq!(applied.removed_count, 1);
    assert_eq!(applied.totals.removed_size_bytes, expected);
    assert!(applied.verified_reclaimed_bytes <= applied.removed_allocated_bytes);
    assert!(!path.exists());

    let repeated = cleanup_runtime_tmp_bounded(options).expect("repeat apply");
    assert_eq!(repeated.removed_count, 0);
    assert_eq!(repeated.totals.removed_size_bytes, 0);
    env::remove_var(runtime_tmpdir_env());
}

#[test]
fn stale_heartbeat_cannot_steal_live_cleanup_lock() {
    use std::sync::mpsc;

    let root = tempfile::tempdir().expect("tempdir");
    let first =
        super::super::cleanup_support::acquire_cleanup_lock(root.path()).expect("first lock");
    let owner_path = first.path.join(CLEANUP_LOCK_OWNER_FILE);
    let mut owner: RuntimeTempCleanupLockOwner =
        serde_json::from_slice(&fs::read(&owner_path).expect("owner record")).expect("owner json");
    owner.heartbeat_unix_ms = 0;
    super::super::cleanup_support::write_cleanup_lock_owner(&owner_path, &owner)
        .expect("stale heartbeat");
    let path = root.path().to_path_buf();
    let (tx, rx) = mpsc::channel();
    let contender = std::thread::spawn(move || {
        tx.send(super::super::cleanup_support::acquire_cleanup_lock(&path))
            .expect("send result");
    });

    assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
    drop(first);
    let second = rx
        .recv_timeout(Duration::from_secs(2))
        .expect("contender completes")
        .expect("second lock");
    drop(second);
    contender.join().expect("contender join");
}

#[cfg(unix)]
#[test]
fn storage_accounting_handles_sparse_hardlink_and_symlink_entries() {
    use std::os::unix::fs::symlink;

    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let path = runtime_temp_dir("homeboy-storage-accounting").expect("runtime dir");
    let sparse = path.join("sparse.bin");
    fs::File::create(&sparse)
        .expect("sparse file")
        .set_len(8 * 1024 * 1024)
        .expect("sparse length");
    fs::hard_link(&sparse, path.join("hardlink.bin")).expect("hardlink");
    symlink(&sparse, path.join("symlink.bin")).expect("symlink");
    let measure = path_storage_measure(&path).expect("storage measure");

    assert!(measure.logical_bytes >= 8 * 1024 * 1024);
    assert!(measure.logical_bytes < 16 * 1024 * 1024);
    assert!(measure.allocated_bytes < measure.logical_bytes);

    let output =
        cleanup_runtime_tmp(true, 0, Some("homeboy-storage-accounting"), 10).expect("cleanup");
    assert_eq!(output.removed_count, 1);
    assert_eq!(output.rows[0].allocated_bytes, measure.allocated_bytes);
    assert!(output.rows[0].verified_reclaimed_bytes <= measure.allocated_bytes);
    assert!(!path.exists());
    env::remove_var(runtime_tmpdir_env());
}

#[test]
fn concurrent_cleanup_serializes_and_removes_once() {
    let _guard = home_env_guard();
    let root = tempfile::tempdir().expect("tempdir");
    env::set_var(runtime_tmpdir_env(), root.path());
    let path = failed_run("homeboy-run-concurrent", 128);
    let mut options = bounded_options(true, Some("homeboy-run-concurrent"));
    options.older_than_days = 0;

    let first = std::thread::spawn(move || cleanup_runtime_tmp_bounded(options).expect("first"));
    let second = std::thread::spawn(move || cleanup_runtime_tmp_bounded(options).expect("second"));
    let outputs = [
        first.join().expect("first join"),
        second.join().expect("second join"),
    ];

    assert_eq!(
        outputs
            .iter()
            .map(|output| output.removed_count)
            .sum::<usize>(),
        1
    );
    assert!(!path.exists());
    env::remove_var(runtime_tmpdir_env());
}
