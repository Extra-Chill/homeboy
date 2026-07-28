use super::*;
use crate::runner_download_cache::{record_download_intent, RunnerDownloadIntent};

/// A clock far enough ahead that everything created during a test reads as
/// past the age floor unless its mtime is deliberately moved forward.
fn clock_past_the_floor() -> SystemTime {
    SystemTime::now() + RUNNER_DOWNLOAD_MIN_AGE + Duration::from_secs(3_600)
}

fn options(apply: bool) -> RunnerDownloadCleanupOptions {
    RunnerDownloadCleanupOptions {
        apply,
        ..RunnerDownloadCleanupOptions::default()
    }
}

fn filters(options: &RunnerDownloadCleanupOptions) -> CleanupFilters {
    CleanupFilters::resolve(options).expect("resolve filters")
}

/// A veto that answers "no running run claims anything".
fn no_running_runs() -> LivenessVeto {
    LivenessVeto {
        running: Some(Vec::new()),
    }
}

/// A veto that could not be evaluated at all.
fn unavailable_liveness() -> LivenessVeto {
    LivenessVeto { running: None }
}

fn running_run(id: &str) -> RunRecord {
    RunRecord {
        id: id.to_string(),
        kind: "runner-exec".to_string(),
        component_id: None,
        started_at: "2026-07-22T00:00:00Z".to_string(),
        finished_at: None,
        status: RunStatus::Running.as_str().to_string(),
        command: None,
        cwd: None,
        homeboy_version: None,
        git_sha: None,
        rig_id: None,
        metadata_json: serde_json::json!({}),
    }
}

/// Write `<artifact-root>/runner/<runner>/<run>/<name>` **tagged as an internal
/// fetch**, and return the cache directory. This is the exact layout
/// `download_remote_artifact_with_intent(.., InternalFetch)` produces, and it is
/// the only shape this category can ever reclaim, so it is the default for
/// tests exercising the age floor and the liveness veto.
fn write_cached_download(
    root: &Path,
    runner: &str,
    run: &str,
    name: &str,
    bytes: &[u8],
) -> PathBuf {
    let cache = write_untagged_download(root, runner, run, name, bytes);
    record_download_intent(&cache, RunnerDownloadIntent::InternalFetch, name);
    cache
}

/// The same bytes with no intent marker: either an operator pull, or any cache
/// directory written before intent tagging existed. Retained unconditionally.
fn write_untagged_download(
    root: &Path,
    runner: &str,
    run: &str,
    name: &str,
    bytes: &[u8],
) -> PathBuf {
    let cache = root.join("runner").join(runner).join(run);
    fs::create_dir_all(&cache).expect("cache dir");
    fs::write(cache.join(name), bytes).expect("cached bytes");
    cache
}

/// Move one file's mtime, so a single cache directory can be made fresh or
/// future-dated relative to the injected clock without sleeping.
fn set_file_mtime(path: &Path, when: SystemTime) {
    let file = fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open for mtime");
    file.set_modified(when).expect("set mtime");
}

fn aged_sweep(
    root: &Path,
    options: &RunnerDownloadCleanupOptions,
    liveness: fn() -> LivenessVeto,
) -> RunnerDownloadCleanupOutcome {
    sweep_with(
        root,
        options,
        &filters(options),
        RUNNER_DOWNLOAD_MIN_AGE,
        clock_past_the_floor(),
        liveness,
    )
    .expect("sweep")
}

fn row_for<'a>(
    outcome: &'a RunnerDownloadCleanupOutcome,
    path: &str,
) -> &'a RunnerDownloadCleanupRow {
    outcome
        .rows
        .iter()
        .find(|row| row.path == path)
        .unwrap_or_else(|| panic!("no row for {path} in {:?}", outcome.rows))
}

#[test]
fn age_floor_is_the_shared_runner_floor_and_is_not_configurable() {
    // Every runner-scoped delete path resolves one named floor (#10316). This
    // category deliberately exposes no flag or configuration key to lower it.
    assert_eq!(
        RUNNER_DOWNLOAD_MIN_AGE,
        Duration::from_secs(crate::cleanup::RUNNER_MIN_AGE_HOURS * 3_600)
    );
    assert_eq!(RUNNER_DOWNLOAD_MIN_AGE, Duration::from_secs(24 * 60 * 60));
}

#[test]
fn missing_cache_root_is_an_empty_plan_and_never_opens_the_store() {
    let home = tempfile::tempdir().expect("home");
    let options = options(true);
    let outcome = sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        clock_past_the_floor(),
        || panic!("liveness must not be read when there is no cache root"),
    )
    .expect("sweep");

    assert_eq!(outcome.inspected_count, 0);
    assert_eq!(outcome.planned_count, 0);
    assert!(outcome.rows.is_empty());
    assert_eq!(outcome.liveness, RunnerDownloadLiveness::NotConsulted);
}

#[test]
fn a_freshly_pulled_artifact_is_never_removed() {
    // The data-loss case #10564 was filed for: `homeboy runs artifacts <run>
    // --pull` writes here, and a bare `homeboy cleanup --apply` used to remove
    // the whole tree with no age floor at all. The cache is tagged
    // `internal_fetch` so the age floor is the only thing that can save it —
    // an operator-owned tag would make the assertion pass for the wrong reason.
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let options = options(true);

    // The clock is the real one, so the pull that just happened is seconds old.
    let outcome = sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        SystemTime::now(),
        no_running_runs,
    )
    .expect("sweep");

    assert_eq!(outcome.inspected_count, 1);
    assert_eq!(outcome.planned_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 1);
    assert!(row_for(&outcome, "lab/run-1")
        .reason
        .contains("newer than the runner download age floor"));
    assert!(cache.join("trace.zip").exists());
}

#[test]
fn a_fresh_cache_survives_beside_a_stale_one_that_is_reclaimed() {
    // The old implementation removed the whole root, so one stale cache took
    // every fresh sibling with it. Removal is now per cache directory.
    let home = tempfile::tempdir().expect("home");
    let stale = write_cached_download(home.path(), "lab", "stale-run", "old.bin", b"old");
    let fresh = write_cached_download(home.path(), "lab", "fresh-run", "new.bin", b"new");
    let clock = clock_past_the_floor();
    // The fresh cache is dated at the injected "now", so its newest byte is
    // zero seconds old while the stale cache is well past the floor.
    set_file_mtime(&fresh.join("new.bin"), clock);

    let options = options(true);
    let outcome = sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        clock,
        no_running_runs,
    )
    .expect("sweep");

    assert_eq!(outcome.inspected_count, 2);
    assert_eq!(outcome.removed_count, 1);
    assert_eq!(outcome.skipped_count, 1);
    assert!(!stale.exists());
    assert!(fresh.join("new.bin").exists());
    assert_eq!(row_for(&outcome, "lab/stale-run").action, "removed");
    assert_eq!(row_for(&outcome, "lab/fresh-run").action, "skip");
}

#[test]
fn the_newest_byte_in_a_cache_decides_the_whole_cache() {
    // A cache directory holding one old artifact and one just-pulled artifact
    // must be retained in full: partial removal would delete evidence the
    // operator is holding beside the file they just fetched.
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "old.bin", b"old");
    fs::write(cache.join("new.bin"), b"new").expect("second artifact");
    let clock = clock_past_the_floor();
    set_file_mtime(&cache.join("new.bin"), clock);

    let options = options(true);
    let outcome = sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        clock,
        no_running_runs,
    )
    .expect("sweep");

    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 1);
    assert!(cache.join("old.bin").exists());
    assert!(cache.join("new.bin").exists());
}

#[test]
fn a_future_dated_cache_is_retained_rather_than_treated_as_old() {
    // Clock skew must not round down to "old enough".
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let clock = clock_past_the_floor();
    set_file_mtime(&cache.join("trace.zip"), clock + Duration::from_secs(3_600));

    let options = options(true);
    let outcome = sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        clock,
        no_running_runs,
    )
    .expect("sweep");

    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 1);
    assert_eq!(row_for(&outcome, "lab/run-1").age_seconds, 0);
    assert!(row_for(&outcome, "lab/run-1").reason.contains("future"));
    assert!(cache.exists());
}

#[test]
fn a_non_terminal_run_vetoes_its_cache_even_past_the_age_floor() {
    let home = tempfile::tempdir().expect("home");
    let claimed = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let unclaimed = write_cached_download(home.path(), "lab", "run-2", "trace.zip", b"trace");

    let options = options(true);
    let outcome = sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        clock_past_the_floor(),
        || LivenessVeto {
            running: Some(vec![running_run("run-1")]),
        },
    )
    .expect("sweep");

    assert_eq!(outcome.liveness, RunnerDownloadLiveness::ObservationStore);
    assert_eq!(outcome.removed_count, 1);
    assert!(claimed.exists());
    assert!(!unclaimed.exists());
    assert!(row_for(&outcome, "lab/run-1")
        .reason
        .contains("non-terminal run"));
}

#[test]
fn unavailable_liveness_retains_everything_instead_of_vetoing_nothing() {
    // Fail closed: an unreadable observation store must retain, not release.
    // The inverse (treating "no answer" as "no veto") is the fail-open shape
    // cleanup must never have.
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let options = options(true);

    let outcome = aged_sweep(home.path(), &options, unavailable_liveness);

    assert_eq!(outcome.liveness, RunnerDownloadLiveness::Unavailable);
    assert_eq!(outcome.planned_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 1);
    assert!(row_for(&outcome, "lab/run-1")
        .reason
        .contains("fail closed"));
    assert!(cache.exists());
}

#[test]
fn a_missing_run_row_does_not_by_itself_authorize_removal() {
    // Row absence is not a delete proof (#10284): runner-side run ids often
    // have no local row at all. The age floor is the authorization, so a cache
    // with no matching row is still retained while it is young.
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "no-such-run", "trace.zip", b"trace");
    let options = options(true);

    let outcome = sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        SystemTime::now(),
        no_running_runs,
    )
    .expect("sweep");

    assert_eq!(outcome.removed_count, 0);
    assert!(cache.exists());
}

#[test]
fn entries_that_are_not_the_canonical_shape_are_reported_and_never_removed() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path().join("runner");
    fs::create_dir_all(&root).expect("cache root");
    let loose = root.join("loose.txt");
    fs::write(&loose, b"operator file").expect("loose file");
    let bare_runner = root.join("lab");
    fs::create_dir_all(&bare_runner).expect("bare runner dir");
    let loose_run_entry = bare_runner.join("stray.txt");
    fs::write(&loose_run_entry, b"stray").expect("stray file");

    let options = options(true);
    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.planned_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 2);
    assert!(loose.exists());
    assert!(loose_run_entry.exists());
    for path in ["loose.txt", "lab/stray.txt"] {
        assert!(row_for(&outcome, path).reason.contains("canonical"));
    }
}

#[test]
fn a_symlinked_cache_directory_is_never_followed_or_removed() {
    let home = tempfile::tempdir().expect("home");
    let outside = home.path().join("outside");
    fs::create_dir_all(&outside).expect("outside dir");
    fs::write(outside.join("precious.bin"), b"precious").expect("precious bytes");
    let runner_dir = home.path().join("runner").join("lab");
    fs::create_dir_all(&runner_dir).expect("runner dir");
    std::os::unix::fs::symlink(&outside, runner_dir.join("run-1")).expect("symlink");

    let options = options(true);
    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.planned_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert!(outside.join("precious.bin").exists());
    assert!(row_for(&outcome, "lab/run-1").reason.contains("canonical"));
}

#[test]
fn the_cache_root_itself_is_never_removed() {
    let home = tempfile::tempdir().expect("home");
    write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let options = options(true);

    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.removed_count, 1);
    // The root survives; only the emptied `<runner-id>` directory is pruned,
    // and only because `remove_dir` refuses a non-empty directory.
    assert!(home.path().join("runner").exists());
    assert!(!home.path().join("runner").join("lab").exists());
}

#[test]
fn an_emptied_runner_directory_is_pruned_only_when_a_sibling_does_not_survive() {
    let home = tempfile::tempdir().expect("home");
    write_cached_download(home.path(), "lab", "stale-run", "old.bin", b"old");
    let fresh = write_cached_download(home.path(), "lab", "fresh-run", "new.bin", b"new");
    let clock = clock_past_the_floor();
    set_file_mtime(&fresh.join("new.bin"), clock);

    let options = options(true);
    sweep_with(
        home.path(),
        &options,
        &filters(&options),
        RUNNER_DOWNLOAD_MIN_AGE,
        clock,
        no_running_runs,
    )
    .expect("sweep");

    // `remove_dir` is non-recursive, so the surviving sibling keeps its parent.
    assert!(fresh.exists());
    assert!(home.path().join("runner").join("lab").exists());
}

#[test]
fn narrowing_filters_select_candidates_without_bypassing_the_predicate() {
    let home = tempfile::tempdir().expect("home");
    let targeted = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let other = write_cached_download(home.path(), "lab", "run-2", "trace.zip", b"trace");

    // Naming a run id asks "clean this one"; it does not ask to skip the floor.
    let narrowed = RunnerDownloadCleanupOptions {
        apply: true,
        runner: Some("lab".to_string()),
        run_id: Some("run-1".to_string()),
        ..RunnerDownloadCleanupOptions::default()
    };
    let fresh = sweep_with(
        home.path(),
        &narrowed,
        &filters(&narrowed),
        RUNNER_DOWNLOAD_MIN_AGE,
        SystemTime::now(),
        no_running_runs,
    )
    .expect("sweep");

    assert_eq!(fresh.inspected_count, 1);
    assert_eq!(fresh.removed_count, 0);
    assert!(targeted.exists());
    assert!(fresh
        .root
        .ends_with(Path::new("runner").join("lab").join("run-1")));

    // Past the floor the same filter reclaims exactly the named cache.
    let aged = aged_sweep(home.path(), &narrowed, no_running_runs);
    assert_eq!(aged.inspected_count, 1);
    assert_eq!(aged.removed_count, 1);
    assert!(!targeted.exists());
    assert!(other.exists());
}

#[test]
fn the_inspection_budget_truncates_instead_of_widening() {
    let home = tempfile::tempdir().expect("home");
    write_cached_download(home.path(), "lab", "run-1", "a.bin", b"a");
    write_cached_download(home.path(), "lab", "run-2", "b.bin", b"b");

    let bounded = RunnerDownloadCleanupOptions {
        apply: true,
        limit: 1,
        ..RunnerDownloadCleanupOptions::default()
    };
    let outcome = aged_sweep(home.path(), &bounded, no_running_runs);

    assert!(outcome.truncated);
    assert_eq!(outcome.inspected_count, 1);
    assert_eq!(outcome.removed_count, 1);

    // A zero budget is the fail-closed resolution of an unrepresentable limit
    // (`CleanupPolicy::scan_limit`). It must remove nothing at all.
    let zero = RunnerDownloadCleanupOptions {
        apply: true,
        limit: 0,
        ..RunnerDownloadCleanupOptions::default()
    };
    let outcome = aged_sweep(home.path(), &zero, no_running_runs);
    assert!(outcome.truncated);
    assert_eq!(outcome.inspected_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert!(home
        .path()
        .join("runner")
        .join("lab")
        .join("run-2")
        .exists());
}

#[test]
fn dry_run_plans_without_touching_bytes() {
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let options = options(false);

    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert!(outcome.dry_run);
    assert_eq!(outcome.planned_count, 1);
    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.planned_size_bytes, 5);
    assert_eq!(outcome.removed_size_bytes, 0);
    assert_eq!(outcome.file_count, 1);
    assert_eq!(outcome.directory_count, 0);
    assert!(cache.join("trace.zip").exists());
    let row = row_for(&outcome, "lab/run-1");
    assert_eq!(row.action, "remove");
    assert!(row.size_measured);
}

#[test]
fn run_id_requires_runner_and_filters_must_be_single_path_components() {
    let missing_runner = RunnerDownloadCleanupOptions {
        run_id: Some("run-1".to_string()),
        ..RunnerDownloadCleanupOptions::default()
    };
    let error = CleanupFilters::resolve(&missing_runner).expect_err("missing runner");
    assert!(error.message.contains("--run-id requires --runner"));

    for (runner, run_id) in [
        (Some("../outside".to_string()), None),
        (Some("lab".to_string()), Some("../outside".to_string())),
        (Some("/tmp/outside".to_string()), None),
    ] {
        let traversal = RunnerDownloadCleanupOptions {
            runner,
            run_id,
            ..RunnerDownloadCleanupOptions::default()
        };
        let error = CleanupFilters::resolve(&traversal).expect_err("traversal");
        assert!(error.message.contains("single path component"));
    }
}

#[test]
fn a_symlink_inside_a_cache_is_counted_but_never_followed() {
    let home = tempfile::tempdir().expect("home");
    let outside = home.path().join("outside");
    fs::create_dir_all(&outside).expect("outside dir");
    fs::write(outside.join("precious.bin"), b"precious bytes").expect("precious");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    std::os::unix::fs::symlink(&outside, cache.join("link")).expect("symlink");

    let options = options(true);
    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.removed_count, 1);
    // `remove_dir_all` unlinks the symlink; the target is untouched.
    assert!(outside.join("precious.bin").exists());
    // The link contributed no measured bytes from the far side of the link.
    assert_eq!(outcome.removed_size_bytes, 5);
}

#[test]
fn an_untagged_cache_is_retained_forever_no_matter_how_old() {
    // #10585. Age proves the bytes are old; it cannot prove they are homeboy's.
    // Every cache directory written before intent tagging existed is untagged,
    // and untagged means operator-owned.
    let home = tempfile::tempdir().expect("home");
    let cache = write_untagged_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    let options = options(true);

    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.planned_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 1);
    let row = row_for(&outcome, "lab/run-1");
    assert_eq!(row.intent, "unrecorded");
    assert!(row.reason.contains("fail closed"));
    // The age is still reported, so an operator can see what is accumulating.
    assert!(row.age_seconds >= RUNNER_DOWNLOAD_MIN_AGE.as_secs());
    assert!(cache.join("trace.zip").exists());
}

#[test]
fn an_operator_pull_is_retained_past_the_age_floor() {
    let home = tempfile::tempdir().expect("home");
    let pulled = write_untagged_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    record_download_intent(&pulled, RunnerDownloadIntent::OperatorPull, "artifact-1");
    let internal = write_cached_download(home.path(), "lab", "run-2", "trace.zip", b"trace");

    let options = options(true);
    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.removed_count, 1);
    assert!(pulled.exists(), "an operator pull is never swept");
    assert!(!internal.exists(), "an internal fetch past the floor is");
    assert_eq!(row_for(&outcome, "lab/run-1").intent, "operator_pull");
    assert!(row_for(&outcome, "lab/run-1")
        .reason
        .contains("only the operator releases them"));
    assert_eq!(row_for(&outcome, "lab/run-2").intent, "internal_fetch");
    assert_eq!(row_for(&outcome, "lab/run-2").action, "removed");
}

#[test]
fn an_internal_fetch_that_later_served_an_operator_pull_is_retained() {
    // Operator ownership is sticky in the writer, and the predicate must honour
    // the merged tag rather than the most recent fetch.
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    record_download_intent(&cache, RunnerDownloadIntent::OperatorPull, "artifact-2");

    let options = options(true);
    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.removed_count, 0);
    assert_eq!(row_for(&outcome, "lab/run-1").intent, "operator_pull");
    assert!(cache.exists());
}

#[test]
fn a_corrupt_intent_marker_retains_rather_than_releases() {
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    fs::write(cache.join(RUNNER_DOWNLOAD_MARKER_FILE), b"{ truncated").expect("corrupt marker");

    let options = options(true);
    let outcome = aged_sweep(home.path(), &options, no_running_runs);

    assert_eq!(outcome.removed_count, 0);
    assert_eq!(row_for(&outcome, "lab/run-1").intent, "unreadable");
    assert!(row_for(&outcome, "lab/run-1")
        .reason
        .contains("fail closed"));
    assert!(cache.exists());
}

#[test]
fn the_intent_marker_is_not_counted_as_downloaded_bytes_but_is_still_removed() {
    // The marker is homeboy's bookkeeping. Counting it would inflate every
    // reported file count and size by a file the operator never downloaded.
    let home = tempfile::tempdir().expect("home");
    let cache = write_cached_download(home.path(), "lab", "run-1", "trace.zip", b"trace");
    assert!(cache.join(RUNNER_DOWNLOAD_MARKER_FILE).exists());

    let dry = aged_sweep(home.path(), &options(false), no_running_runs);
    assert_eq!(dry.file_count, 1);
    assert_eq!(dry.planned_size_bytes, 5);

    let applied = aged_sweep(home.path(), &options(true), no_running_runs);
    assert_eq!(applied.removed_count, 1);
    assert!(!cache.exists());
}

#[test]
fn a_size_measurement_failure_does_not_move_the_verdict() {
    // Size is advisory. A scan that cannot total the bytes still reports the
    // same action; it only marks the number untrustworthy.
    let now = SystemTime::now();
    let aged = Some(now - Duration::from_secs(48 * 3_600));
    let scan = SubtreeScan {
        newest_mtime: aged,
        size_measured: false,
        ..SubtreeScan::default()
    };
    assert!(scan.newest_age(now).expect("age") >= RUNNER_DOWNLOAD_MIN_AGE);

    // An mtime failure, by contrast, must remove the age entirely.
    let mut blind = SubtreeScan {
        newest_mtime: aged,
        ..SubtreeScan::default()
    };
    blind.unreadable();
    assert!(blind.newest_age(now).is_none());
}
