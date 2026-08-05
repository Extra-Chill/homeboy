use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::*;

/// A conclusively dead PID: spawn a child, reap it, and reuse its id.
///
/// Not PID 0 — `kill(0, 0)` addresses the caller's whole process group and
/// therefore reports *alive*, which would make every test using it vacuous.
fn dead_pid() -> u32 {
    let mut child = std::process::Command::new("true")
        .spawn()
        .expect("spawn short-lived child");
    let pid = child.id();
    child.wait().expect("reap short-lived child");
    assert!(
        !crate::process::pid_is_running(pid),
        "reaped child must read as dead"
    );
    pid
}

fn home(name: &str, owner_pid: Option<u32>, age_seconds: u64, size_bytes: u64) -> LeakedTestHome {
    LeakedTestHome {
        path: PathBuf::from("/tmp").join(name),
        size_bytes,
        age_seconds,
        owner_pid,
        owner_alive: owner_pid.is_some_and(owner_is_alive),
        verdict: LeakedTestHomeVerdict::OwnerAlive,
        reapable: false,
        removed: false,
        removal_error: None,
    }
}

fn seed_dir(root: &Path, name: &str, bytes: usize) -> PathBuf {
    let path = root.join(name);
    fs::create_dir_all(&path).expect("seed leaked home");
    fs::write(path.join("payload"), vec![b'x'; bytes]).expect("seed payload");
    path
}

fn scan(root: &Path, min_age: Duration, apply: bool) -> LeakedTestHomeCleanupOutput {
    cleanup_leaked_test_homes(LeakedTestHomeCleanupOptions {
        apply,
        min_age,
        roots: vec![root.to_path_buf()],
        ..LeakedTestHomeCleanupOptions::default()
    })
    .expect("scan leaked test homes")
}

fn entry_for<'a>(
    output: &'a LeakedTestHomeCleanupOutput,
    path: &Path,
) -> Option<&'a LeakedTestHome> {
    output.entries.iter().find(|entry| entry.path == path)
}

#[test]
fn an_owned_prefix_round_trips_through_the_pid_parser() {
    let prefix = owned_test_tempdir_prefix();
    assert!(prefix.starts_with(TEST_TEMPDIR_PREFIX));
    assert_eq!(
        test_tempdir_owner_pid(&format!("{prefix}AbCdEf")),
        Some(std::process::id())
    );
}

/// `None` from the parser means *unknown owner*, never *unowned*. Names written
/// before the PID prefix existed, and malformed ones, must land here rather than
/// being misread as owned by something.
#[test]
fn legacy_and_malformed_names_report_no_owner() {
    for name in [
        "hb-test-AbCdEf",
        "hb-test-",
        "hb-test-notapid-AbCdEf",
        "hb-test--AbCdEf",
        "something-else",
    ] {
        assert_eq!(
            test_tempdir_owner_pid(name),
            None,
            "{name} must have no owner"
        );
    }
}

/// The reaper has to look where the creator wrote. `$TMPDIR` is what `tempfile`
/// honors, so it leads the list, and the conventional roots follow without
/// duplicates.
#[test]
fn the_effective_roots_are_deduplicated_and_include_the_conventional_ones() {
    let roots = effective_temp_roots();
    let mut seen: Vec<&PathBuf> = Vec::new();
    for root in &roots {
        assert!(!seen.contains(&root), "{root:?} listed twice");
        seen.push(root);
    }
    assert!(roots.contains(&PathBuf::from("/tmp")));
    if let Some(tmpdir) = std::env::var_os(TEMPDIR_ENV) {
        let tmpdir = PathBuf::from(tmpdir);
        if !tmpdir.as_os_str().is_empty() {
            assert_eq!(roots.first(), Some(&tmpdir), "$TMPDIR must lead the roots");
        }
    }
}

/// The load-bearing safety property. A running owner is unreachable by every
/// reclaim path: not by age, not by an exhausted byte budget, not by both at
/// once.
#[test]
fn a_live_owners_home_is_never_reapable_at_any_age_or_budget() {
    let mut entries = vec![home(
        "hb-test-live",
        Some(std::process::id()),
        u64::MAX,
        1_000_000,
    )];

    classify(&mut entries, Duration::ZERO, 0);

    assert_eq!(entries[0].verdict, LeakedTestHomeVerdict::OwnerAlive);
    assert!(!entries[0].reapable);
    assert!(!LeakedTestHomeVerdict::OwnerAlive.is_reapable());
}

#[test]
fn an_abandoned_home_past_the_age_floor_is_reapable() {
    let mut entries = vec![home("hb-test-dead", Some(dead_pid()), 7_200, 10)];

    classify(&mut entries, Duration::from_secs(3_600), u64::MAX);

    assert_eq!(entries[0].verdict, LeakedTestHomeVerdict::AbandonedAndAged);
    assert!(entries[0].reapable);
}

/// The age floor is what stops a home being deleted out from under a process
/// that created it microseconds ago and has not yet become observable.
#[test]
fn an_abandoned_home_inside_the_age_floor_is_retained() {
    let mut entries = vec![home("hb-test-young", Some(dead_pid()), 1, 10)];

    classify(&mut entries, Duration::from_secs(3_600), u64::MAX);

    assert_eq!(entries[0].verdict, LeakedTestHomeVerdict::RetainedTooYoung);
    assert!(!entries[0].reapable);
}

/// "No owner recorded" is not proof of death, so an unknown-owner entry only
/// ever leaves on the age floor — the byte budget must not reach it.
#[test]
fn an_unknown_owner_is_retained_and_never_promoted_by_the_budget() {
    let mut entries = vec![home("hb-test-legacy", None, 1, 5_000)];

    classify(&mut entries, Duration::from_secs(3_600), 0);

    assert_eq!(
        entries[0].verdict,
        LeakedTestHomeVerdict::RetainedUnknownOwner
    );
    assert!(!entries[0].reapable);

    // The same entry does leave once it is genuinely stale.
    let mut aged = vec![home("hb-test-legacy", None, 7_200, 5_000)];
    classify(&mut aged, Duration::from_secs(3_600), u64::MAX);
    assert_eq!(aged[0].verdict, LeakedTestHomeVerdict::AbandonedAndAged);
}

/// A day count cannot bound a directory accumulating 666 MB entries at an
/// unbounded rate. The budget relaxes the age floor — for the oldest abandoned
/// entries, and only as far as the overage requires.
#[test]
fn the_byte_budget_promotes_the_oldest_abandoned_entries_only_as_far_as_needed() {
    let dead = dead_pid();
    let mut entries = vec![
        home("hb-test-newest", Some(dead), 10, 100),
        home("hb-test-oldest", Some(dead), 300, 100),
        home("hb-test-middle", Some(dead), 200, 100),
    ];

    // 300 retained bytes against a 150-byte budget: 150 must go, which two
    // entries cover. The third stays.
    classify(&mut entries, Duration::from_secs(3_600), 150);

    let promoted: Vec<&str> = entries
        .iter()
        .filter(|entry| entry.verdict == LeakedTestHomeVerdict::AbandonedOverBudget)
        .filter_map(|entry| entry.path.file_name().and_then(|name| name.to_str()))
        .collect();
    assert_eq!(promoted, ["hb-test-oldest", "hb-test-middle"]);
    assert_eq!(
        entries[0].verdict,
        LeakedTestHomeVerdict::RetainedTooYoung,
        "the newest abandoned entry is not needed to meet the budget"
    );
}

/// A live owner's bytes count against the budget but can never satisfy it. The
/// budget must spend itself on abandoned entries and then stop, not reach for
/// the only remaining candidate.
#[test]
fn an_exhausted_budget_never_reaches_past_the_abandoned_entries() {
    let mut entries = vec![
        home("hb-test-live", Some(std::process::id()), 10_000, 1_000_000),
        home("hb-test-dead", Some(dead_pid()), 10, 10),
    ];

    classify(&mut entries, Duration::from_secs(3_600), 0);

    assert_eq!(entries[0].verdict, LeakedTestHomeVerdict::OwnerAlive);
    assert!(!entries[0].reapable);
    assert_eq!(
        entries[1].verdict,
        LeakedTestHomeVerdict::AbandonedOverBudget
    );
}

/// Only directories directly under a root, carrying the marker prefix, are ever
/// candidates. Everything else in a shared `/tmp` belongs to someone else.
#[test]
fn only_marked_directories_directly_under_a_root_are_candidates() {
    let root = tempfile::tempdir().expect("scan root");
    let marked = seed_dir(
        root.path(),
        &format!("{TEST_TEMPDIR_PREFIX}{}-aaa", dead_pid()),
        8,
    );
    let foreign = seed_dir(root.path(), "someones-important-data", 8);
    let nested = seed_dir(
        &foreign,
        &format!("{TEST_TEMPDIR_PREFIX}{}-nested", dead_pid()),
        8,
    );
    let stray_file = root.path().join("hb-test-not-a-dir");
    fs::write(&stray_file, b"file").expect("write stray file");

    let output = scan(root.path(), Duration::ZERO, true);

    assert!(
        !marked.exists(),
        "a marked abandoned directory is reclaimed"
    );
    assert!(foreign.exists(), "unmarked directories are never touched");
    assert!(
        nested.exists(),
        "the scan never recurses looking for candidates"
    );
    assert!(
        stray_file.exists(),
        "a marked non-directory is never touched"
    );
    assert_eq!(output.inspected_count, 1);
    assert_eq!(output.removed_count, 1);
}

#[cfg(unix)]
#[test]
fn a_symlink_wearing_the_marker_is_never_followed_or_removed() {
    let root = tempfile::tempdir().expect("scan root");
    let target = tempfile::tempdir().expect("symlink target");
    fs::write(target.path().join("precious"), b"precious").expect("seed target");
    let link = root
        .path()
        .join(format!("{TEST_TEMPDIR_PREFIX}{}-link", dead_pid()));
    std::os::unix::fs::symlink(target.path(), &link).expect("create symlink");

    let output = scan(root.path(), Duration::ZERO, true);

    assert_eq!(output.inspected_count, 0);
    assert!(link.exists(), "the symlink itself is left in place");
    assert!(
        target.path().join("precious").exists(),
        "a symlink must never become a delete path into someone else's tree"
    );
}

/// The end-to-end statement of the safety contract, on a real filesystem.
#[test]
fn an_apply_reclaims_the_abandoned_home_and_spares_the_live_one() {
    let root = tempfile::tempdir().expect("scan root");
    let abandoned = seed_dir(
        root.path(),
        &format!("{TEST_TEMPDIR_PREFIX}{}-gone", dead_pid()),
        64,
    );
    let live = seed_dir(
        root.path(),
        &format!("{}live", owned_test_tempdir_prefix()),
        64,
    );

    let output = scan(root.path(), Duration::ZERO, true);

    assert!(!abandoned.exists(), "an abandoned home must be reclaimed");
    assert!(
        live.exists(),
        "this process is alive, so its own home must survive"
    );
    assert_eq!(output.removed_count, 1);
    assert!(output.removed_size_bytes > 0);
    assert_eq!(
        entry_for(&output, &live).map(|entry| entry.verdict),
        Some(LeakedTestHomeVerdict::OwnerAlive)
    );
}

#[test]
fn a_dry_run_plans_without_removing_anything() {
    let root = tempfile::tempdir().expect("scan root");
    let abandoned = seed_dir(
        root.path(),
        &format!("{TEST_TEMPDIR_PREFIX}{}-gone", dead_pid()),
        64,
    );

    let output = scan(root.path(), Duration::ZERO, false);

    assert!(abandoned.exists(), "a dry run must not delete");
    assert!(output.dry_run);
    assert_eq!(output.planned_count, 1);
    assert_eq!(output.removed_count, 0);
    assert_eq!(output.removed_size_bytes, 0);
    assert!(output.planned_size_bytes > 0);
}

/// #11073's core complaint: a category that reports `0` because it looked in the
/// wrong directory is worse than one that says it did not look. Every root is
/// reported, and an unreadable one carries its reason.
#[test]
fn a_root_that_cannot_be_read_is_reported_with_a_reason_not_as_a_silent_zero() {
    let root = tempfile::tempdir().expect("scan root");
    let missing = root.path().join("does-not-exist");

    let output = cleanup_leaked_test_homes(LeakedTestHomeCleanupOptions {
        roots: vec![missing.clone(), root.path().to_path_buf()],
        min_age: Duration::ZERO,
        ..LeakedTestHomeCleanupOptions::default()
    })
    .expect("scan leaked test homes");

    assert_eq!(output.roots.len(), 2);
    let unreadable = &output.roots[0];
    assert_eq!(unreadable.path, missing);
    assert!(!unreadable.inspected);
    assert!(
        unreadable
            .skip_reason
            .as_ref()
            .is_some_and(|reason| !reason.trim().is_empty()),
        "an uninspected root must say why"
    );
    assert!(output.roots[1].inspected);
    assert!(output.roots[1].skip_reason.is_none());
}

#[test]
fn duplicate_roots_are_inspected_once() {
    let root = tempfile::tempdir().expect("scan root");
    seed_dir(
        root.path(),
        &format!("{TEST_TEMPDIR_PREFIX}{}-dupe", dead_pid()),
        8,
    );

    let output = cleanup_leaked_test_homes(LeakedTestHomeCleanupOptions {
        roots: vec![root.path().to_path_buf(), root.path().to_path_buf()],
        min_age: Duration::ZERO,
        ..LeakedTestHomeCleanupOptions::default()
    })
    .expect("scan leaked test homes");

    assert_eq!(output.roots.len(), 1);
    assert_eq!(output.inspected_count, 1);
}

/// A bounded pass reports that it was bounded, so its totals read as a floor
/// rather than as the whole leak.
#[test]
fn an_inspection_limit_is_reported_rather_than_silently_narrowing_the_totals() {
    let root = tempfile::tempdir().expect("scan root");
    let pid = dead_pid();
    for index in 0..3 {
        seed_dir(
            root.path(),
            &format!("{TEST_TEMPDIR_PREFIX}{pid}-entry{index}"),
            8,
        );
    }

    let output = cleanup_leaked_test_homes(LeakedTestHomeCleanupOptions {
        roots: vec![root.path().to_path_buf()],
        min_age: Duration::ZERO,
        limit: 2,
        ..LeakedTestHomeCleanupOptions::default()
    })
    .expect("scan leaked test homes");

    assert!(output.truncated);
    assert_eq!(output.inspected_count, 2);
}

/// The default options must not delete anything on their own: a caller that
/// forgets to opt in gets a dry run behind an hour-long age floor.
#[test]
fn the_defaults_are_a_dry_run_behind_an_age_floor() {
    let options = LeakedTestHomeCleanupOptions::default();
    assert!(!options.apply);
    assert!(options.min_age >= Duration::from_secs(3_600));
    assert!(options.roots.is_empty());
}
