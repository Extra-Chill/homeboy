use super::*;

fn run_git(dir: &Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn fixture_record(source: &Path, worktree: &Path) -> TaskWorktreeRecord {
    TaskWorktreeRecord {
        id: "fixture@task".to_string(),
        component_id: "fixture".to_string(),
        source_checkout: source.to_string_lossy().to_string(),
        worktree_path: worktree.to_string_lossy().to_string(),
        branch: "task".to_string(),
        base_ref: "HEAD".to_string(),
        task_url: Some("https://example.com/task".to_string()),
        run_id: None,
        cleanup_policy: CleanupPolicy::RemoveWhenSafe,
        branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        state: TaskWorktreeState::Active,
    }
}

fn git_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().unwrap();
    run_git(temp.path(), &["init", "-q"]);
    run_git(
        temp.path(),
        &["config", "user.email", "homeboy@example.com"],
    );
    run_git(temp.path(), &["config", "user.name", "Homeboy Test"]);
    fs::write(temp.path().join("README.md"), "initial\n").unwrap();
    run_git(temp.path(), &["add", "."]);
    run_git(temp.path(), &["commit", "-q", "-m", "initial"]);
    temp
}

fn write_component_registration(home: &Path, id: &str, local_path: &Path) {
    let dir = home.join(".config/homeboy/components");
    fs::create_dir_all(&dir).expect("components dir");
    fs::write(
        dir.join(format!("{id}.json")),
        serde_json::json!({
            "local_path": local_path,
            "remote_path": format!("wp-content/plugins/{id}")
        })
        .to_string(),
    )
    .expect("component registration");
}

#[test]
fn metadata_round_trips_and_lists() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source");
    let worktree = dir.path().join("source@task");
    fs::create_dir_all(&source).unwrap();
    fs::create_dir_all(&worktree).unwrap();
    let store = dir.path().join("store");
    let record = fixture_record(&source, &worktree);

    write_record(&store, &record).unwrap();
    let listed = list_with_store(&store).unwrap();

    assert_eq!(listed.worktrees, vec![record]);
}

#[test]
fn safety_report_blocks_dirty_worktree() {
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "dirty");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "dirty-task",
            &worktree.to_string_lossy(),
        ],
    );
    fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();

    let report = safety_report(&fixture_record(source.path(), &worktree)).unwrap();

    assert!(report.dirty);
    assert!(!report.safe);
    assert!(report
        .reasons
        .iter()
        .any(|reason| reason == "dirty worktree"));
}

#[test]
fn safety_report_blocks_primary_checkout() {
    let source = git_repo();

    let report = safety_report(&fixture_record(source.path(), source.path())).unwrap();

    assert!(report.primary_checkout);
    assert!(!report.path_contained);
    assert!(!report.worktree_missing);
    assert!(!report.safe);
}

#[test]
fn safety_report_allows_missing_contained_worktree() {
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "missing");

    let report = safety_report(&fixture_record(source.path(), &worktree)).unwrap();

    assert!(report.worktree_missing);
    assert!(report.path_contained);
    assert!(!report.primary_checkout);
    assert!(!report.dirty);
    assert_eq!(report.unpushed_commits, 0);
    assert!(report.safe);
}

#[test]
fn cleanup_marks_missing_worktree_record_removed() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "missing-cleanup");
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &worktree);
    write_record(&store, &record).unwrap();

    let output = cleanup_with_store(
        WorktreeCleanupOptions {
            force: false,
            dry_run: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();
    let updated = read_record(&store, &record.id).unwrap();

    assert_eq!(output.counts.candidates, 1);
    assert_eq!(output.counts.removed, 1);
    assert_eq!(output.counts.skipped, 0);
    assert_eq!(output.removed.len(), 1);
    assert!(output.removed[0].removed);
    assert!(output.removed[0].safety.worktree_missing);
    assert_eq!(updated.state, TaskWorktreeState::Removed);
}

#[test]
fn cleanup_deletes_merged_task_branch_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    run_git(source.path(), &["branch", "task"]);
    let worktree = sibling_worktree_path(source.path(), "merged-branch-cleanup");
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &worktree);
    write_record(&store, &record).unwrap();

    let output = cleanup_with_store(
        WorktreeCleanupOptions {
            force: false,
            dry_run: false,
            cleanup_branches: true,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();

    assert_eq!(output.counts.branch_delete_candidates, 1);
    assert_eq!(output.counts.branches_deleted, 1);
    assert_eq!(
        output.removed[0].branch_cleanup.status,
        BranchCleanupStatus::Deleted
    );
    assert!(std::process::Command::new("git")
        .args(["show-ref", "--verify", "--quiet", "refs/heads/task"])
        .current_dir(source.path())
        .status()
        .unwrap()
        .code()
        .is_some_and(|code| code != 0));
}

#[test]
fn cleanup_reports_unmerged_task_branch_without_deleting_by_default() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    run_git(source.path(), &["checkout", "-q", "-b", "task"]);
    fs::write(source.path().join("task.txt"), "task\n").unwrap();
    run_git(source.path(), &["add", "."]);
    run_git(source.path(), &["commit", "-q", "-m", "task"]);
    run_git(source.path(), &["checkout", "-q", "-"]);
    let worktree = sibling_worktree_path(source.path(), "unmerged-branch-cleanup");
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &worktree);
    write_record(&store, &record).unwrap();

    let output = cleanup_with_store(
        WorktreeCleanupOptions {
            force: false,
            dry_run: false,
            cleanup_branches: true,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();

    assert_eq!(output.counts.branch_delete_candidates, 0);
    assert_eq!(output.counts.branches_deleted, 0);
    assert_eq!(output.counts.unmerged_branches, 1);
    assert_eq!(
        output.removed[0].branch_cleanup.status,
        BranchCleanupStatus::Unmerged
    );
    run_git(
        source.path(),
        &["show-ref", "--verify", "--quiet", "refs/heads/task"],
    );
}

#[test]
fn status_repairs_missing_source_checkout_from_component_checkout() {
    use crate::test_support::with_isolated_home;

    with_isolated_home(|home| {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let missing_source = sibling_worktree_path(source.path(), "removed-source");
        let worktree = sibling_worktree_path(source.path(), "status-repair");
        let store = dir.path().join("store");
        write_component_registration(home.path(), "fixture", source.path());
        let record = fixture_record(&missing_source, &worktree);
        write_record(&store, &record).unwrap();

        let output = status_with_store(&record.id, &store).unwrap();
        let updated = read_record(&store, &record.id).unwrap();

        assert_eq!(
            PathBuf::from(&output.record.source_checkout),
            source.path().canonicalize().unwrap()
        );
        assert_eq!(updated.source_checkout, output.record.source_checkout);
        assert!(output.safety.worktree_missing);
        assert!(output.safety.safe);
    });
}

#[test]
fn status_reports_missing_source_checkout_as_validation_diagnostic() {
    use crate::test_support::with_isolated_home;

    with_isolated_home(|_| {
        let dir = tempfile::tempdir().unwrap();
        let missing_source = dir.path().join("removed-source");
        let worktree = dir.path().join("fixture@task");
        let store = dir.path().join("store");
        let record = fixture_record(&missing_source, &worktree);
        write_record(&store, &record).unwrap();

        let err = status_with_store(&record.id, &store).unwrap_err();

        assert_eq!(err.code, crate::error::ErrorCode::ValidationInvalidArgument);
        assert_eq!(
            err.details.get("field").and_then(|field| field.as_str()),
            Some("source_checkout")
        );
        assert!(err
            .to_string()
            .contains("Task worktree source checkout is missing"));
    });
}

#[test]
fn cleanup_skips_unrepairable_missing_source_and_continues() {
    use crate::test_support::with_isolated_home;

    with_isolated_home(|_| {
        let dir = tempfile::tempdir().unwrap();
        let source = git_repo();
        let store = dir.path().join("store");
        let mut unrepairable = fixture_record(
            &dir.path().join("removed-source"),
            &dir.path().join("unrepairable@task"),
        );
        unrepairable.id = "unrepairable@task".to_string();
        unrepairable.component_id = "unrepairable".to_string();
        write_record(&store, &unrepairable).unwrap();
        let removable_worktree = sibling_worktree_path(source.path(), "cleanup-continues");
        let mut removable = fixture_record(source.path(), &removable_worktree);
        removable.id = "fixture@cleanup-continues".to_string();
        write_record(&store, &removable).unwrap();

        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force: false,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();
        let skipped = read_record(&store, &unrepairable.id).unwrap();
        let removed = read_record(&store, &removable.id).unwrap();

        assert_eq!(output.counts.candidates, 2);
        assert_eq!(output.counts.removed, 1);
        assert_eq!(output.counts.skipped, 1);
        assert_eq!(output.removed[0].record.id, removable.id);
        assert_eq!(output.skipped[0].record.id, unrepairable.id);
        assert_eq!(skipped.state, TaskWorktreeState::Active);
        assert_eq!(removed.state, TaskWorktreeState::Removed);
    });
}

#[test]
fn cleanup_skips_dirty_worktree_without_force() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "dirty-cleanup-refused");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "dirty-cleanup-refused",
            &worktree.to_string_lossy(),
        ],
    );
    fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();
    let store = dir.path().join("store");
    let mut dirty_record = fixture_record(source.path(), &worktree);
    dirty_record.id = "fixture@dirty".to_string();
    let mut safe_record = fixture_record(
        source.path(),
        &sibling_worktree_path(source.path(), "missing-after-dirty"),
    );
    safe_record.id = "fixture@missing".to_string();
    write_record(&store, &dirty_record).unwrap();
    write_record(&store, &safe_record).unwrap();

    let output = cleanup_with_store(
        WorktreeCleanupOptions {
            force: false,
            dry_run: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();
    let updated = read_record(&store, &dirty_record.id).unwrap();

    assert_eq!(output.counts.candidates, 2);
    assert_eq!(output.counts.removed, 1);
    assert_eq!(output.counts.skipped, 1);
    assert_eq!(output.skipped[0].record.id, dirty_record.id);
    assert!(output.skipped[0]
        .reasons
        .iter()
        .any(|reason| reason == "dirty worktree"));
    assert_eq!(output.removed[0].record.id, safe_record.id);
    assert_eq!(updated.state, TaskWorktreeState::Active);
    assert!(worktree.exists());
}

#[test]
fn cleanup_force_still_skips_primary_checkout_hard_gate() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), source.path());
    write_record(&store, &record).unwrap();

    let output = cleanup_with_store(
        WorktreeCleanupOptions {
            force: true,
            dry_run: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();
    let updated = read_record(&store, &record.id).unwrap();

    assert_eq!(output.counts.candidates, 1);
    assert_eq!(output.counts.removed, 0);
    assert_eq!(output.counts.skipped, 1);
    assert!(output.skipped[0]
        .reasons
        .iter()
        .any(|reason| reason == "refuses to remove primary checkout"));
    assert_eq!(updated.state, TaskWorktreeState::Active);
    assert!(source.path().exists());
}

#[test]
fn cleanup_dry_run_reports_safe_candidate_without_removing() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "dry-run-cleanup");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "dry-run-cleanup",
            &worktree.to_string_lossy(),
        ],
    );
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &worktree);
    write_record(&store, &record).unwrap();

    let output = cleanup_with_store(
        WorktreeCleanupOptions {
            force: false,
            dry_run: true,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();
    let updated = read_record(&store, &record.id).unwrap();

    assert!(output.dry_run);
    assert_eq!(output.counts.candidates, 1);
    assert_eq!(output.counts.removed, 0);
    assert_eq!(output.counts.skipped, 0);
    assert_eq!(output.candidates[0].record.id, record.id);
    assert!(!output.candidates[0].safety.worktree_missing);
    assert_eq!(updated.state, TaskWorktreeState::Active);
    assert!(worktree.exists());
}

#[test]
fn cleanup_force_removes_dirty_worktree_after_homeboy_gates_pass() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "dirty-cleanup-forced");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "dirty-cleanup-forced",
            &worktree.to_string_lossy(),
        ],
    );
    fs::write(worktree.join("dirty.txt"), "dirty\n").unwrap();
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &worktree);
    write_record(&store, &record).unwrap();

    let output = cleanup_with_store(
        WorktreeCleanupOptions {
            force: true,
            dry_run: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();
    let updated = read_record(&store, &record.id).unwrap();

    assert_eq!(output.counts.candidates, 1);
    assert_eq!(output.counts.removed, 1);
    assert_eq!(output.counts.skipped, 0);
    assert!(output.removed[0].removed);
    assert!(output.removed[0].safety.dirty);
    assert_eq!(updated.state, TaskWorktreeState::Removed);
    assert!(!worktree.exists());
}

#[test]
fn safety_report_blocks_unpushed_commits() {
    let remote = tempfile::tempdir().unwrap();
    run_git(remote.path(), &["init", "--bare", "-q"]);
    let source = tempfile::tempdir().unwrap();
    run_git(
        source.path(),
        &["clone", &remote.path().to_string_lossy(), "."],
    );
    run_git(
        source.path(),
        &["config", "user.email", "homeboy@example.com"],
    );
    run_git(source.path(), &["config", "user.name", "Homeboy Test"]);
    fs::write(source.path().join("README.md"), "initial\n").unwrap();
    run_git(source.path(), &["add", "."]);
    run_git(source.path(), &["commit", "-q", "-m", "initial"]);
    run_git(source.path(), &["push", "-u", "origin", "HEAD:main"]);

    let worktree = sibling_worktree_path(source.path(), "unpushed");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "unpushed-task",
            &worktree.to_string_lossy(),
            "HEAD",
        ],
    );
    fs::write(worktree.join("change.txt"), "change\n").unwrap();
    run_git(&worktree, &["add", "."]);
    run_git(&worktree, &["commit", "-q", "-m", "change"]);

    let mut record = fixture_record(source.path(), &worktree);
    record.base_ref = "origin/main".to_string();
    let report = safety_report(&record).unwrap();

    assert_eq!(report.unpushed_commits, 1);
    assert!(!report.safe);
}

fn sibling_worktree_path(source: &Path, suffix: &str) -> PathBuf {
    let name = source
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("source");
    source.with_file_name(format!("{name}-{suffix}-worktree"))
}

fn queue_options() -> WorktreeQueueCreateOptions {
    WorktreeQueueCreateOptions {
        repo: "homeboy".to_string(),
        branches: vec!["cook/one".to_string(), "cook/two".to_string()],
        from: "origin/main".to_string(),
        task_url: Some("https://github.com/Extra-Chill/homeboy/issues/5786".to_string()),
        task_ref: Some("Extra-Chill/homeboy#5786".to_string()),
        dry_run: true,
        retry_after_seconds: 30,
    }
}

#[test]
fn queue_create_dry_run_returns_queued_rows_with_exact_homeboy_commands() {
    let output = queue_create(queue_options()).unwrap();

    assert_eq!(output.schema, "homeboy/worktree-queue-create/v1");
    assert_eq!(output.rows.len(), 2);
    assert_eq!(output.rows[0].status, WorktreeQueueCreateStatus::Queued);
    assert_eq!(output.rows[0].handle, "homeboy@cook-one");
    assert_eq!(
        output.rows[0].command,
        vec![
            "homeboy",
            "worktree",
            "create",
            "homeboy",
            "--branch",
            "cook/one",
            "--from",
            "origin/main",
            "--task-url",
            "https://github.com/Extra-Chill/homeboy/issues/5786",
        ]
    );
}

#[test]
fn queue_create_records_successful_homeboy_worktree() {
    use crate::test_support::with_isolated_home;

    with_isolated_home(|home| {
        let parent = home.path().join("Developer");
        let source = parent.join("queue-fixture");
        let worktree_path = parent.join("queue-fixture@cook-one");
        if parent.exists() {
            fs::remove_dir_all(&parent).unwrap();
        }
        fs::create_dir_all(&parent).unwrap();
        fs::create_dir_all(&source).unwrap();
        run_git(&source, &["init", "-q"]);
        run_git(&source, &["config", "user.email", "homeboy@example.com"]);
        run_git(&source, &["config", "user.name", "Homeboy Test"]);
        fs::write(source.join("README.md"), "initial\n").unwrap();
        fs::write(source.join("homeboy.json"), r#"{"id":"queue-fixture"}"#).unwrap();
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-q", "-m", "initial"]);
        write_component_registration(home.path(), "queue-fixture", &source);

        let output = queue_create(WorktreeQueueCreateOptions {
            repo: "queue-fixture".to_string(),
            branches: vec!["cook/one".to_string()],
            from: "HEAD".to_string(),
            task_url: Some("https://github.com/Extra-Chill/homeboy/issues/5924".to_string()),
            task_ref: None,
            dry_run: false,
            retry_after_seconds: 30,
        })
        .unwrap();

        assert_eq!(
            output.rows[0].status,
            WorktreeQueueCreateStatus::Created,
            "queue row failed: {:?}",
            output.rows[0].error
        );
        assert_eq!(output.rows[0].handle, "queue-fixture@cook-one");
        assert!(output.rows[0].path.is_some());
        let record = resolve("queue-fixture@cook-one").expect("queued worktree record");
        assert!(Path::new(&record.worktree_path).exists());
        assert_eq!(
            PathBuf::from(&record.worktree_path).canonicalize().unwrap(),
            worktree_path.canonicalize().unwrap()
        );
        assert_eq!(record.branch, "cook/one");
        assert_eq!(record.base_ref, "HEAD");
        assert_eq!(
            record.task_url.as_deref(),
            Some("https://github.com/Extra-Chill/homeboy/issues/5924")
        );
    });
}

#[test]
fn queue_create_uses_runner_checkout_when_lab_snapshot_is_not_git_backed() {
    use crate::test_support::with_isolated_home;

    with_isolated_home(|home| {
        let runner_root = home.path().join("Developer");
        let source = runner_root.join("lab-fixture");
        let snapshot = runner_root.join("_lab_workspaces/job-123");
        fs::create_dir_all(&source).unwrap();
        fs::create_dir_all(&snapshot).unwrap();
        run_git(&source, &["init", "-q"]);
        run_git(&source, &["config", "user.email", "homeboy@example.com"]);
        run_git(&source, &["config", "user.name", "Homeboy Test"]);
        fs::write(source.join("README.md"), "initial\n").unwrap();
        fs::write(source.join("homeboy.json"), r#"{"id":"lab-fixture"}"#).unwrap();
        fs::write(snapshot.join("homeboy.json"), r#"{"id":"lab-fixture"}"#).unwrap();
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-q", "-m", "initial"]);
        write_component_registration(home.path(), "lab-fixture", &snapshot);
        let _cwd = CurrentDirGuard::set(&snapshot);

        let output = queue_create(WorktreeQueueCreateOptions {
            repo: "lab-fixture".to_string(),
            branches: vec!["cook/lab".to_string()],
            from: "HEAD".to_string(),
            task_url: None,
            task_ref: None,
            dry_run: false,
            retry_after_seconds: 30,
        })
        .unwrap();

        assert_eq!(
            output.rows[0].status,
            WorktreeQueueCreateStatus::Created,
            "queue row failed: {:?}",
            output.rows[0].error
        );
        let record = resolve("lab-fixture@cook-lab").expect("queued worktree record");
        assert_eq!(
            PathBuf::from(record.source_checkout),
            source.canonicalize().unwrap()
        );
        assert!(runner_root.join("lab-fixture@cook-lab").exists());
    });
}

struct CurrentDirGuard {
    prior: PathBuf,
}

impl CurrentDirGuard {
    fn set(path: &Path) -> Self {
        let prior = std::env::current_dir().unwrap();
        std::env::set_current_dir(path).unwrap();
        Self { prior }
    }
}

impl Drop for CurrentDirGuard {
    fn drop(&mut self) {
        std::env::set_current_dir(&self.prior).unwrap();
    }
}
