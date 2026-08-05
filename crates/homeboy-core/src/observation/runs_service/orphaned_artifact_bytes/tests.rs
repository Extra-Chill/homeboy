use super::*;

/// A clock far enough ahead that everything created during the test reads as
/// crash residue against the real age floor.
fn clock_past_the_floor() -> SystemTime {
    SystemTime::now() + ORPHANED_ARTIFACT_BYTES_MIN_AGE + Duration::from_secs(3600)
}

fn staging_name() -> String {
    format!(".artifact-{}.staging", Uuid::new_v4())
}

fn scratch_name(label: &str) -> String {
    format!("patch-{label}-{}", Uuid::new_v4())
}

fn options(apply: bool) -> OrphanedArtifactBytesCleanupOptions {
    OrphanedArtifactBytesCleanupOptions {
        apply,
        ..OrphanedArtifactBytesCleanupOptions::default()
    }
}

/// Sweep with the production age floor and a clock that has moved past it.
fn aged_sweep(root: &Path, apply: bool) -> OrphanedArtifactBytesCleanupOutcome {
    sweep(
        root,
        options(apply),
        ORPHANED_ARTIFACT_BYTES_MIN_AGE,
        clock_past_the_floor(),
    )
    .expect("sweep")
}

#[test]
fn missing_artifact_root_is_an_empty_sweep_not_an_error() {
    let home = tempfile::tempdir().expect("home");
    let outcome = aged_sweep(&home.path().join("absent"), true);
    assert_eq!(outcome.inspected_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert!(outcome.rows.is_empty());
}

#[test]
fn aged_staging_and_scratch_are_reaped_on_apply() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    let run_dir = root.join("run-1");
    fs::create_dir_all(&run_dir).expect("run dir");
    let staging = run_dir.join(staging_name());
    fs::write(&staging, b"staged bytes").expect("staging bytes");

    let scratch = root.join("_scratch").join(scratch_name("baseline"));
    fs::create_dir_all(scratch.join("nested")).expect("scratch tree");
    fs::write(scratch.join("nested/file"), b"baseline").expect("scratch bytes");

    let dry = aged_sweep(root, false);
    assert!(dry.dry_run);
    assert_eq!(dry.inspected_count, 2);
    assert_eq!(dry.planned_count, 2);
    assert_eq!(dry.removed_count, 0);
    assert!(dry.planned_size_bytes > 0);
    assert!(staging.exists(), "dry run must not delete");
    assert!(scratch.exists(), "dry run must not delete");
    assert!(dry.rows.iter().all(|row| row.action == "remove"));

    let applied = aged_sweep(root, true);
    assert!(!applied.dry_run);
    assert_eq!(applied.removed_count, 2);
    assert_eq!(applied.skipped_count, 0);
    assert!(!staging.exists());
    assert!(!scratch.exists());
    assert!(applied.rows.iter().all(|row| row.action == "removed"));
    assert!(
        applied.rows.iter().all(|row| row.size_measured),
        "local fixtures are measurable"
    );
    // Both owners are attributed so an operator can see which constructor leaked.
    let owners = applied
        .rows
        .iter()
        .map(|row| row.owner)
        .collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        owners,
        ["artifact-staging", "patch-capture-scratch"]
            .into_iter()
            .collect()
    );
}

#[test]
fn young_scratch_is_never_reaped() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    let run_dir = root.join("run-1");
    fs::create_dir_all(&run_dir).expect("run dir");
    let staging = run_dir.join(staging_name());
    fs::write(&staging, b"in flight").expect("staging bytes");
    let scratch = root.join("_scratch").join(scratch_name("after"));
    fs::create_dir_all(&scratch).expect("scratch dir");

    let outcome = sweep(
        root,
        options(true),
        ORPHANED_ARTIFACT_BYTES_MIN_AGE,
        SystemTime::now(),
    )
    .expect("sweep");
    assert_eq!(outcome.inspected_count, 2);
    assert_eq!(outcome.planned_count, 0);
    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 2);
    assert!(staging.exists());
    assert!(scratch.exists());
    assert!(outcome
        .rows
        .iter()
        .all(|row| row.reason.contains("age floor")));
}

#[test]
fn future_dated_scratch_fails_closed() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    let scratch = root.join("_scratch").join(scratch_name("baseline"));
    fs::create_dir_all(&scratch).expect("scratch dir");

    // A clock behind the entry's modification time is the clock-skew case. It
    // must not round down to "old enough".
    let outcome = sweep(
        root,
        options(true),
        Duration::ZERO,
        SystemTime::now() - Duration::from_secs(3600),
    )
    .expect("sweep");
    assert_eq!(outcome.removed_count, 0);
    assert_eq!(outcome.skipped_count, 1);
    assert!(outcome.rows[0].reason.contains("future"));
    assert!(scratch.exists());
}

#[test]
fn subsystem_owned_artifact_root_subtrees_are_never_candidates() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    // Every one of these is a live top-level subtree owned by another
    // subsystem and carrying no `artifacts.path` row at that path. The
    // row-join orphan reaper proposed in #10284 would delete all of them.
    for owned in [
        "runner/runner-a",
        "runner-attach/runner-a",
        "runner-exec-attach/runner-a",
        "agent-task/task-a",
        "agent-task-loop-controller/loop-a",
        "controller-scratch-recovery/run-a",
        "recovered-runner-artifacts/run-a",
        "executor-finalized/finalized-a",
        "preview-consumer/preview-a",
    ] {
        let path = root.join(owned);
        fs::create_dir_all(&path).expect("subsystem tree");
        fs::write(path.join("evidence.json"), b"{}").expect("subsystem bytes");
    }
    // Owners that moved off `std::env::temp_dir()` and onto the artifact root
    // in #11128. Each writer removes its own scratch; none of them is a
    // candidate for this sweep, so routing them here must not put live state in
    // front of a reaper. `deploy-ref` and `cook-baseline` hold whole `git`
    // worktrees, which is exactly the shape a row-join reaper would eat.
    for owned in [
        "deploy-ref/00000000-0000-4000-8000-00000000000a",
        "deploy-payload/00000000-0000-4000-8000-00000000000b",
        "cook-baseline/baseline-00000000-0000-4000-8000-00000000000c",
        "repo-local-gates/gate-task",
    ] {
        let path = root.join(owned);
        fs::create_dir_all(&path).expect("relocated tree");
        fs::write(path.join("evidence.json"), b"{}").expect("relocated bytes");
    }
    // The rig lifecycle index stages a plain *file* at depth two, which is the
    // same depth and entry type as the staging family. It survives because the
    // name is not staging-shaped, not because files are exempt.
    let rig_staging = root
        .join("rig-resource-lifecycle")
        .join("homeboy-rig-resource-lifecycle-run-1-42.json");
    fs::create_dir_all(rig_staging.parent().expect("rig parent")).expect("rig tree");
    fs::write(&rig_staging, b"{}").expect("rig bytes");
    // A published artifact that has not yet been inserted is the create-then-
    // register window. It must survive too.
    let run_dir = root.join("run-1");
    fs::create_dir_all(&run_dir).expect("run dir");
    let published = run_dir.join("artifact-1-report.json");
    fs::write(&published, b"{}").expect("published bytes");

    let outcome = aged_sweep(root, true);
    assert_eq!(outcome.inspected_count, 0, "{:#?}", outcome.rows);
    assert_eq!(outcome.removed_count, 0);
    assert!(published.exists());
    assert!(rig_staging.exists(), "rig lifecycle staging was reaped");
    for owned in [
        "runner",
        "runner-attach",
        "runner-exec-attach",
        "agent-task",
        "agent-task-loop-controller",
        "controller-scratch-recovery",
        "recovered-runner-artifacts",
        "executor-finalized",
        "preview-consumer",
        "deploy-ref",
        "deploy-payload",
        "cook-baseline",
        "repo-local-gates",
        "rig-resource-lifecycle",
    ] {
        assert!(root.join(owned).exists(), "{owned} was reaped");
    }
}

#[test]
fn lookalike_names_outside_the_owned_shapes_are_ignored() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    let run_dir = root.join("run-1");
    fs::create_dir_all(&run_dir).expect("run dir");
    let scratch_dir = root.join("_scratch");
    fs::create_dir_all(&scratch_dir).expect("scratch dir");

    // Prefix and suffix match, but the middle is not a UUID.
    let bad_staging = run_dir.join(".artifact-not-a-uuid.staging");
    fs::write(&bad_staging, b"operator file").expect("bytes");
    // A staging-shaped *directory* is not what the constructor emits.
    let staging_dir = run_dir.join(staging_name());
    fs::create_dir_all(&staging_dir).expect("staging dir");
    // Patch-prefixed but unstructured.
    let bad_scratch = scratch_dir.join("patch-notes");
    fs::create_dir_all(&bad_scratch).expect("bad scratch");
    // Correct scratch shape, wrong container.
    let misplaced = run_dir.join(scratch_name("baseline"));
    fs::create_dir_all(&misplaced).expect("misplaced scratch");
    // A scratch-shaped *file* is not what the constructor emits.
    let scratch_file = scratch_dir.join(scratch_name("after"));
    fs::write(&scratch_file, b"not a tree").expect("bytes");

    let outcome = aged_sweep(root, true);
    assert_eq!(outcome.inspected_count, 0, "{:#?}", outcome.rows);
    assert!(bad_staging.exists());
    assert!(staging_dir.exists());
    assert!(bad_scratch.exists());
    assert!(misplaced.exists());
    assert!(scratch_file.exists());
}

#[test]
fn nested_scratch_shaped_paths_are_not_reached() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    // Depth three. Only depth two is ever classified, so a runner-owned tree
    // that happens to contain a staging-shaped name is untouched.
    let nested = root.join("runner").join("runner-a");
    fs::create_dir_all(&nested).expect("nested tree");
    let decoy = nested.join(staging_name());
    fs::write(&decoy, b"runner owned").expect("bytes");

    let outcome = aged_sweep(root, true);
    assert_eq!(outcome.inspected_count, 0);
    assert!(decoy.exists());
}

#[test]
fn limit_bounds_the_sweep_and_reports_truncation() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    let run_dir = root.join("run-1");
    fs::create_dir_all(&run_dir).expect("run dir");
    for _ in 0..5 {
        fs::write(run_dir.join(staging_name()), b"staged").expect("staging bytes");
    }

    let outcome = sweep(
        root,
        OrphanedArtifactBytesCleanupOptions {
            apply: true,
            limit: 2,
        },
        ORPHANED_ARTIFACT_BYTES_MIN_AGE,
        clock_past_the_floor(),
    )
    .expect("sweep");
    assert!(outcome.truncated);
    assert_eq!(outcome.inspected_count, 2);
    assert_eq!(outcome.removed_count, 2);
    assert_eq!(
        fs::read_dir(&run_dir).expect("run dir").count(),
        3,
        "limit must leave the remainder for the next sweep"
    );
}

#[test]
fn owned_name_shapes_match_their_constructors() {
    let id = Uuid::new_v4();
    assert!(is_artifact_staging_name(&format!(".artifact-{id}.staging")));
    assert!(!is_artifact_staging_name(".artifact-.staging"));
    assert!(!is_artifact_staging_name(&format!(".artifact-{id}")));
    assert!(!is_artifact_staging_name(&format!("artifact-{id}.staging")));

    // Both labels `patch_capture::create_scratch_dir` uses today, plus an
    // unknown one, so a future label needs no second code change. The trailing
    // UUID is matched by fixed width because a hyphenated UUID contains
    // hyphens — splitting on the last separator would never parse.
    assert!(is_patch_capture_scratch_name(&format!(
        "patch-baseline-{id}"
    )));
    assert!(is_patch_capture_scratch_name(&format!("patch-after-{id}")));
    assert!(is_patch_capture_scratch_name(&format!("patch-future-{id}")));
    assert!(!is_patch_capture_scratch_name(&format!("patch-{id}")));
    assert!(!is_patch_capture_scratch_name("patch-baseline-not-a-uuid"));
    assert!(!is_patch_capture_scratch_name(&format!(
        "scratch-after-{id}"
    )));
    assert!(!is_patch_capture_scratch_name("patch-"));
}

/// The module docstring's headline invariant: size is advisory and never gates
/// removal. `best_effort_size` gives up past `MAX_DEPTH` and returns `None`,
/// which is the same shape a failed `read_dir` produces. The candidate must
/// still be removed, with `size_measured: false` marking the number as a floor
/// rather than a fact -- the opposite of the lab workspace pruner, which
/// silently became a no-op whenever `du` failed.
#[test]
fn an_unmeasurable_candidate_is_still_removed_and_reported_as_unmeasured() {
    let home = tempfile::tempdir().expect("home");
    let root = home.path();
    let scratch = root.join("_scratch").join(scratch_name("baseline"));
    // Past the 64-level walk bound, so the size walk returns `None`.
    let mut deep = scratch.clone();
    for level in 0..70 {
        deep = deep.join(format!("d{level}"));
    }
    fs::create_dir_all(&deep).expect("deep tree");
    fs::write(deep.join("leaf"), b"unreachable by the size walk").expect("leaf bytes");

    let dry = aged_sweep(root, false);
    assert_eq!(dry.planned_count, 1);
    assert_eq!(dry.rows.len(), 1);
    assert_eq!(dry.rows[0].action, "remove");
    assert!(
        !dry.rows[0].size_measured,
        "a walk that gave up must not claim a measurement"
    );
    assert_eq!(dry.rows[0].size_bytes, 0);
    assert_eq!(dry.planned_size_bytes, 0);
    assert!(scratch.exists(), "dry run must not delete");

    let applied = aged_sweep(root, true);
    assert_eq!(applied.removed_count, 1, "{:#?}", applied.rows);
    assert_eq!(applied.skipped_count, 0);
    assert!(!applied.rows[0].size_measured);
    assert_eq!(applied.removed_size_bytes, 0);
    assert!(
        !scratch.exists(),
        "an unmeasurable candidate is still reaped"
    );
}

/// A missing artifact root is an empty sweep, but an *unreadable* one is an
/// error. The two must not collapse: reporting "nothing to reap" for a root
/// that could not be read would hide the leak the sweep exists to surface.
#[test]
fn an_artifact_root_that_is_not_a_directory_is_an_error_not_an_empty_sweep() {
    let home = tempfile::tempdir().expect("home");
    let not_a_directory = home.path().join("artifacts");
    fs::write(&not_a_directory, b"an operator put a file here").expect("root file");

    let error = sweep(
        &not_a_directory,
        options(true),
        ORPHANED_ARTIFACT_BYTES_MIN_AGE,
        clock_past_the_floor(),
    )
    .expect_err("an unreadable artifact root is an error");
    assert_eq!(error.code, crate::ErrorCode::InternalIoError);
    assert!(
        error.details["context"]
            .as_str()
            .unwrap_or_default()
            .contains("artifact root"),
        "the error must name the artifact root it could not read: {:?}",
        error.details
    );
    // The contrast that matters: absence is not failure.
    let absent = aged_sweep(&home.path().join("never-created"), true);
    assert_eq!(absent.inspected_count, 0);
}

fn presentable_row(index: usize) -> OrphanedArtifactBytesRow {
    OrphanedArtifactBytesRow {
        path: format!("/artifacts/_scratch/patch-baseline-{index}"),
        owner: "patch-capture-scratch",
        entry_type: "directory",
        age_seconds: 90_000,
        size_bytes: 4096,
        size_measured: true,
        action: "remove".to_string(),
        reason: "crash-orphaned scratch owned by a single private constructor".to_string(),
    }
}

fn presentable_outcome(row_count: usize) -> OrphanedArtifactBytesCleanupOutcome {
    OrphanedArtifactBytesCleanupOutcome {
        dry_run: true,
        artifact_root: PathBuf::from("/artifacts"),
        min_age_seconds: ORPHANED_ARTIFACT_BYTES_MIN_AGE.as_secs(),
        inspected_count: row_count,
        planned_count: row_count,
        removed_count: 0,
        skipped_count: 0,
        planned_size_bytes: 4096 * row_count as u64,
        removed_size_bytes: 0,
        truncated: false,
        rows: (0..row_count).map(presentable_row).collect(),
        row_detail: None,
    }
}

/// The default reader is bounded: more candidates than `OutputBudget::COLLECTION`
/// allows are trimmed, and the omission is reported with a continue command
/// rather than silently dropped.
#[test]
fn the_default_presentation_bounds_rows_and_reports_the_omission() {
    let row_count = OutputBudget::COLLECTION.max_items + 5;
    let mut outcome = presentable_outcome(row_count);

    present_orphaned_artifact_bytes_cleanup(&mut outcome, false, "export".to_string())
        .expect("present");

    assert_eq!(outcome.rows.len(), OutputBudget::COLLECTION.max_items);
    let detail = outcome.row_detail.expect("row detail");
    assert_eq!(detail.presentation, OutputPresentation::BoundedCollection);
    assert_eq!(detail.total_items, row_count);
    assert_eq!(detail.returned_items, OutputBudget::COLLECTION.max_items);
    assert_eq!(detail.omitted_items, 5);
    assert!(detail.truncated);
    assert!(
        !detail.total_bytes_known,
        "a trimmed page cannot know the total"
    );
    assert_eq!(detail.export_command, "export");
    // The counts describe the whole sweep, not the trimmed page: an operator
    // must not read a bounded reader as "only 20 paths leaked".
    assert_eq!(outcome.planned_count, row_count);
}

/// An explicit export is lossless: every row is returned and nothing is
/// reported as omitted, so `truncated` cannot be true on the export path.
#[test]
fn the_export_presentation_returns_every_row_untruncated() {
    let row_count = OutputBudget::COLLECTION.max_items + 5;
    let mut outcome = presentable_outcome(row_count);

    present_orphaned_artifact_bytes_cleanup(&mut outcome, true, "export".to_string())
        .expect("present");

    assert_eq!(outcome.rows.len(), row_count, "export must not drop rows");
    let detail = outcome.row_detail.expect("row detail");
    assert_eq!(detail.presentation, OutputPresentation::LosslessExport);
    assert_eq!(detail.returned_items, row_count);
    assert_eq!(detail.omitted_items, 0);
    assert_eq!(detail.omitted_bytes, 0);
    assert!(!detail.truncated);
    assert!(detail.total_bytes_known);
    assert!(detail.total_bytes > 0);
}

#[cfg(unix)]
#[test]
fn symlinked_scratch_is_ignored_rather_than_followed() {
    let home = tempfile::tempdir().expect("home");
    let outside = home.path().join("outside");
    fs::create_dir_all(outside.join("precious")).expect("outside tree");
    fs::write(outside.join("precious/data"), b"keep").expect("outside bytes");

    let root = home.path().join("artifacts");
    let scratch_dir = root.join("_scratch");
    fs::create_dir_all(&scratch_dir).expect("scratch dir");
    let link = scratch_dir.join(scratch_name("baseline"));
    std::os::unix::fs::symlink(outside.join("precious"), &link).expect("symlink");

    let outcome = aged_sweep(&root, true);
    assert_eq!(outcome.inspected_count, 0, "{:#?}", outcome.rows);
    assert!(outside.join("precious/data").exists());
    assert!(
        fs::symlink_metadata(&link).is_ok(),
        "the link itself is kept too"
    );
}
