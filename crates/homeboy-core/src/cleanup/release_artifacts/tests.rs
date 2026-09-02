use super::*;

use std::time::{Duration, SystemTime};

use tempfile::TempDir;

/// One 435 MB build's worth of structure at test scale: a numbered durable copy
/// plus the canonical upload name, hardlinked to the same inode, exactly as
/// `stage_canonical_upload_path` writes them.
fn hardlinked_release(
    root: &Path,
    repo: &str,
    version: &str,
    bytes: usize,
    age: Duration,
) -> PathBuf {
    let dir = root.join(repo).join(version);
    fs::create_dir_all(&dir).expect("release version directory");
    let numbered = dir.join(format!("01-{repo}.zip"));
    fs::write(&numbered, vec![b'z'; bytes]).expect("durable artifact");
    let canonical = dir.join(format!("{repo}.zip"));
    fs::hard_link(&numbered, &canonical).expect("canonical upload name");
    age_release(&dir, age);
    dir
}

fn plain_release(root: &Path, repo: &str, version: &str, bytes: usize, age: Duration) -> PathBuf {
    let dir = root.join(repo).join(version);
    fs::create_dir_all(&dir).expect("release version directory");
    fs::write(dir.join(format!("01-{repo}.zip")), vec![b'z'; bytes]).expect("durable artifact");
    age_release(&dir, age);
    dir
}

/// Backdate every entry in a version directory. Age is read from the newest
/// write anywhere in the tree, so the directory itself has to move too.
fn age_release(dir: &Path, age: Duration) {
    let when = SystemTime::now() - age;
    let times = fs::FileTimes::new().set_modified(when);
    for entry in fs::read_dir(dir).expect("release directory").flatten() {
        fs::OpenOptions::new()
            .write(true)
            .open(entry.path())
            .expect("open artifact")
            .set_times(times)
            .expect("backdate artifact");
    }
    fs::File::open(dir)
        .expect("open version directory")
        .set_times(times)
        .expect("backdate version directory");
}

fn options(root: &Path) -> ReleaseArtifactCleanupOptions {
    ReleaseArtifactCleanupOptions {
        root: Some(root.to_path_buf()),
        min_age: Duration::ZERO,
        ..ReleaseArtifactCleanupOptions::default()
    }
}

fn day(count: u64) -> Duration {
    Duration::from_secs(count * 86_400)
}

fn version<'a>(
    output: &'a ReleaseArtifactCleanupOutput,
    version: &str,
) -> &'a ReleaseArtifactVersion {
    output
        .versions
        .iter()
        .find(|entry| entry.version == version)
        .unwrap_or_else(|| panic!("version {version} in report"))
}

/// The core bound. #14223's host held 14 builds of one repository at ~435 MB
/// each; a count budget keeps the newest N and nothing else.
#[test]
fn newest_n_releases_per_repo_are_retained_and_the_rest_are_candidates() {
    let store = TempDir::new().expect("release store");
    for (index, version) in ["0_24_0", "0_24_1", "0_24_2", "0_24_3", "0_24_4"]
        .into_iter()
        .enumerate()
    {
        // Newest last: index 0 is the oldest.
        plain_release(
            store.path(),
            "wp-codebox",
            version,
            256,
            day(10 - index as u64),
        );
    }

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        max_count_per_repo: 2,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    assert_eq!(output.inspected_count, 5);
    assert_eq!(output.candidate_count, 3);
    assert!(!version(&output, "0_24_4").eligible, "newest is retained");
    assert!(!version(&output, "0_24_3").eligible, "within count budget");
    for superseded in ["0_24_2", "0_24_1", "0_24_0"] {
        assert!(
            version(&output, superseded).eligible,
            "{superseded} is past the count budget"
        );
    }
}

/// Retention is per repository: a large repository exhausting its own budget
/// must not evict a small repository's history.
#[test]
fn retention_budgets_are_scoped_to_one_repository() {
    let store = TempDir::new().expect("release store");
    for (index, version) in ["0_1_0", "0_2_0", "0_3_0"].into_iter().enumerate() {
        plain_release(store.path(), "big", version, 4_096, day(9 - index as u64));
        plain_release(store.path(), "small", version, 8, day(9 - index as u64));
    }

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        max_count_per_repo: 1,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    for repo in ["big", "small"] {
        let summary = output
            .repos
            .iter()
            .find(|entry| entry.repo == repo)
            .expect("repository rollup");
        assert_eq!(summary.version_count, 3);
        assert_eq!(summary.candidate_count, 2, "{repo} keeps exactly its newest");
    }
}

/// A bare count is a poor bound when per-release size spans two orders of
/// magnitude, so a byte ceiling binds independently of the count.
#[test]
fn a_byte_budget_binds_before_the_count_budget_for_a_large_repository() {
    let store = TempDir::new().expect("release store");
    for (index, version) in ["0_21_0", "0_21_1", "0_21_2"].into_iter().enumerate() {
        plain_release(
            store.path(),
            "wp-codebox",
            version,
            64 * 1024,
            day(9 - index as u64),
        );
    }

    let newest = measure_release_version(&store.path().join("wp-codebox/0_21_2")).allocated_bytes;
    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        // Generous count, tight bytes: only the byte budget can bind.
        max_count_per_repo: 100,
        max_bytes_per_repo: newest,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    assert_eq!(output.candidate_count, 2);
    assert!(!version(&output, "0_21_2").eligible);
    assert!(version(&output, "0_21_1").eligible);
    assert!(version(&output, "0_21_0").eligible);
    assert!(version(&output, "0_21_1").retention_reasons.is_empty());
}

/// Rule 1 of the safety contract. A bound must never empty a repository.
#[test]
fn the_latest_release_survives_every_budget() {
    let store = TempDir::new().expect("release store");
    plain_release(store.path(), "wp-codebox", "0_26_7", 4_096, day(400));

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        max_count_per_repo: 0,
        max_bytes_per_repo: 0,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    let latest = version(&output, "0_26_7");
    assert_eq!(latest.rank, 0);
    assert!(!latest.eligible);
    assert_eq!(
        latest.retention_reasons,
        vec![REASON_LATEST.to_string()],
        "the newest release is protected by rank, not by a budget"
    );
    assert_eq!(output.candidate_count, 0);
    assert_eq!(output.estimated_bytes, 0);
}

/// An apply with the tightest possible budgets still cannot empty a repository.
#[test]
fn apply_never_removes_the_only_release_a_repository_has() {
    let store = TempDir::new().expect("release store");
    let only = plain_release(store.path(), "wp-codebox", "0_26_7", 4_096, day(400));

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        apply: true,
        max_count_per_repo: 0,
        max_bytes_per_repo: 0,
        ..options(store.path())
    })
    .expect("apply release artifact retention");

    assert_eq!(output.removed_count, 0);
    assert_eq!(output.reclaimed_bytes, 0);
    assert!(only.is_dir(), "the current release is still on disk");
}

/// The accounting bug the layout invites: two names, one inode. A naive
/// `st_size` sum reports ~2x the disk a removal returns.
#[test]
fn size_accounting_counts_a_hardlinked_pair_once() {
    let store = TempDir::new().expect("release store");
    let payload = 128 * 1024;
    let dir = hardlinked_release(store.path(), "wp-codebox", "0_24_5", payload, day(1));

    let usage = measure_release_version(&dir);

    assert_eq!(
        usage.logical_bytes,
        (payload * 2) as u64,
        "the naive sum sees both names"
    );
    assert_eq!(
        usage.hardlink_duplicate_bytes, payload as u64,
        "exactly one of the two names is a duplicate"
    );
    // Allocated bytes carry the directory's own blocks, so the payload is a
    // floor rather than an equality. The load-bearing claim is that it is
    // nowhere near the doubled figure.
    assert!(usage.allocated_bytes >= payload as u64);
    assert!(
        usage.allocated_bytes < (payload as u64 * 3) / 2,
        "hardlinked bytes must not be billed twice: {} vs {payload}",
        usage.allocated_bytes
    );
}

/// The correction has to reach the reported reclaim, not just the measurement
/// helper, or the policy over-reports by ~2x exactly as #14223 describes.
#[test]
fn reported_reclaimable_bytes_are_hardlink_corrected() {
    let store = TempDir::new().expect("release store");
    let payload = 128 * 1024;
    hardlinked_release(store.path(), "wp-codebox", "0_24_5", payload, day(1));
    let superseded = hardlinked_release(store.path(), "wp-codebox", "0_24_4", payload, day(2));

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        max_count_per_repo: 1,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    assert_eq!(output.candidate_count, 1);
    let candidate = version(&output, "0_24_4");
    assert_eq!(candidate.logical_bytes, (payload * 2) as u64);
    assert!(
        output.estimated_bytes < (payload as u64 * 3) / 2,
        "estimated bytes must be allocated, not doubled: {}",
        output.estimated_bytes
    );

    let applied = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        apply: true,
        max_count_per_repo: 1,
        ..options(store.path())
    })
    .expect("apply release artifact retention");

    assert_eq!(applied.removed_count, 1);
    assert!(!superseded.exists());
    assert!(
        applied.reclaimed_bytes < (payload as u64 * 3) / 2,
        "reclaimed bytes must not double-bill the hardlinked pair"
    );
}

/// #14222's shape, structurally excluded. `eligible` is derived from the full
/// retention decision, so a populated reason can never coexist with advertised
/// reclaim.
#[test]
fn eligibility_and_retention_reasons_can_never_disagree() {
    let store = TempDir::new().expect("release store");
    for (index, version) in ["0_1_0", "0_2_0", "0_3_0", "0_4_0"].into_iter().enumerate() {
        plain_release(
            store.path(),
            "wp-codebox",
            version,
            1_024,
            day(8 - index as u64),
        );
    }
    // A young entry that the count budget would otherwise reach: it must be
    // held back by the age floor, and its eligibility must follow.
    plain_release(store.path(), "wp-codebox", "0_0_9", 1_024, Duration::ZERO);

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        max_count_per_repo: 1,
        min_age: Duration::from_secs(3_600),
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    for entry in &output.versions {
        assert_eq!(
            entry.eligible,
            entry.retention_reasons.is_empty(),
            "{} advertises reclaim it also gives a reason to retain: {:?}",
            entry.version,
            entry.retention_reasons
        );
    }

    let young = version(&output, "0_0_9");
    assert!(!young.eligible);
    assert!(young
        .retention_reasons
        .iter()
        .any(|reason| reason == REASON_TOO_YOUNG));

    // The advertised plan is exactly what the apply performs. That equality is
    // the whole point: a dry run that promises bytes an apply never frees is
    // the bug being avoided.
    let planned_bytes = output.estimated_bytes;
    let planned_count = output.candidate_count;
    let applied = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        apply: true,
        max_count_per_repo: 1,
        min_age: Duration::from_secs(3_600),
        ..options(store.path())
    })
    .expect("apply release artifact retention");
    assert_eq!(applied.removed_count, planned_count);
    assert_eq!(applied.reclaimed_bytes, planned_bytes);
}

/// `candidate_count` and `estimated_bytes` describe removable entries only,
/// never the inspected total. Reporting the inspected total is what made
/// `candidate_count: 588, applied_count: 1` read as a 587-resource failure in
/// #9483.
#[test]
fn candidate_metrics_describe_only_removable_versions() {
    let store = TempDir::new().expect("release store");
    for (index, version) in ["0_1_0", "0_2_0", "0_3_0"].into_iter().enumerate() {
        plain_release(
            store.path(),
            "wp-codebox",
            version,
            1_024,
            day(9 - index as u64),
        );
    }

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        max_count_per_repo: 2,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    assert_eq!(output.inspected_count, 3);
    assert_eq!(output.candidate_count, 1);
    assert_eq!(output.skipped_count, 2);
    assert_eq!(output.estimated_bytes, version(&output, "0_1_0").size_bytes);
    assert!(output.estimated_bytes < output.total_size_bytes);
}

/// Rule 2: a sweep racing a publication retains rather than deletes.
///
/// Rank alone cannot cover this. Rank is assigned by age, so the newest entry
/// is protected as rank 0 whatever the clock says — the floor only becomes
/// load-bearing when a *second* release was cut inside the same window, which
/// is exactly the back-to-back publish this asserts. Without the floor the
/// second one is rank 1, past a `max_count_per_repo` of 1, and deleted while
/// its publication is still in flight.
#[test]
fn a_release_published_inside_the_age_floor_is_never_pruned() {
    let store = TempDir::new().expect("release store");
    plain_release(store.path(), "wp-codebox", "0_1_0", 1_024, day(10));
    // Two releases cut back to back, both still inside the floor.
    plain_release(store.path(), "wp-codebox", "0_2_0", 1_024, Duration::ZERO);
    plain_release(store.path(), "wp-codebox", "0_3_0", 1_024, Duration::ZERO);

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        apply: true,
        max_count_per_repo: 1,
        min_age: Duration::from_secs(3_600),
        ..options(store.path())
    })
    .expect("apply release artifact retention");

    assert!(store.path().join("wp-codebox/0_2_0").is_dir());
    assert!(store.path().join("wp-codebox/0_3_0").is_dir());
    // Only the aged, superseded release goes.
    assert_eq!(output.removed_count, 1);
    assert!(!store.path().join("wp-codebox/0_1_0").exists());
}

/// Rule 3: retention is monotone in age. Keeping an older entry while pruning a
/// newer one would make the store's contents depend on directory iteration
/// order rather than on the policy.
#[test]
fn a_large_middle_release_does_not_rescue_older_ones() {
    let store = TempDir::new().expect("release store");
    plain_release(store.path(), "wp-codebox", "0_3_0", 1_024, day(1));
    // Big enough to blow the budget on its own.
    plain_release(store.path(), "wp-codebox", "0_2_0", 256 * 1024, day(2));
    // Small enough that a per-entry budget check would let it back in.
    plain_release(store.path(), "wp-codebox", "0_1_0", 8, day(3));

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        max_count_per_repo: 100,
        max_bytes_per_repo: 64 * 1024,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    assert!(!version(&output, "0_3_0").eligible);
    assert!(version(&output, "0_2_0").eligible);
    assert!(
        version(&output, "0_1_0").eligible,
        "an exhausted budget stays exhausted for every older entry"
    );
}

/// A store that was never read must say so rather than contribute a silent
/// zero, the same contract `leaked-test-homes` adopted after #11073.
#[test]
fn an_absent_store_reports_that_it_was_not_inspected() {
    let store = TempDir::new().expect("release store");
    let output = cleanup_release_artifacts(options(&store.path().join("never-created")))
        .expect("plan release artifact retention");

    assert!(!output.root_inspected);
    assert!(output.skip_reason.is_some());
    assert_eq!(output.candidate_count, 0);
    assert_eq!(output.estimated_bytes, 0);
}

/// Defaults must bound nothing until a caller resolves a policy. An unbounded
/// default that silently deleted would be a far worse failure than the
/// unbounded growth this module exists to stop.
#[test]
fn default_options_plan_no_removals() {
    let store = TempDir::new().expect("release store");
    for (index, version) in ["0_1_0", "0_2_0", "0_3_0"].into_iter().enumerate() {
        plain_release(
            store.path(),
            "wp-codebox",
            version,
            1_024,
            day(9 - index as u64),
        );
    }

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        root: Some(store.path().to_path_buf()),
        ..ReleaseArtifactCleanupOptions::default()
    })
    .expect("plan release artifact retention");

    assert_eq!(output.inspected_count, 3);
    assert_eq!(output.candidate_count, 0);
}

/// Neither budget may be widened by a symlink pointing outside the store.
#[test]
fn symlinked_entries_are_not_release_directories() {
    let store = TempDir::new().expect("release store");
    let outside = TempDir::new().expect("unrelated tree");
    fs::write(outside.path().join("payload.bin"), vec![b'z'; 1_024]).expect("unrelated payload");
    plain_release(store.path(), "wp-codebox", "0_1_0", 1_024, day(9));

    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), store.path().join("wp-codebox/0_0_1"))
        .expect("symlinked version");
    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), store.path().join("elsewhere"))
        .expect("symlinked repo");

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        apply: true,
        max_count_per_repo: 0,
        max_bytes_per_repo: 0,
        ..options(store.path())
    })
    .expect("apply release artifact retention");

    assert_eq!(output.inspected_count, 1);
    assert_eq!(output.removed_count, 0);
    assert!(outside.path().join("payload.bin").is_file());
}

/// The scan limit bounds one pass and says so, so a partial total is never read
/// as a complete one.
#[test]
fn the_inspection_limit_marks_its_totals_as_a_floor() {
    let store = TempDir::new().expect("release store");
    for (index, version) in ["0_1_0", "0_2_0", "0_3_0", "0_4_0"].into_iter().enumerate() {
        plain_release(
            store.path(),
            "wp-codebox",
            version,
            1_024,
            day(9 - index as u64),
        );
    }

    let output = cleanup_release_artifacts(ReleaseArtifactCleanupOptions {
        limit: 2,
        max_count_per_repo: 1,
        ..options(store.path())
    })
    .expect("plan release artifact retention");

    assert!(output.truncated);
    assert_eq!(output.inspected_count, 2);
}
