use super::*;

/// A caller reading a "missing handle" error needs the handle creation would
/// actually produce, so the slug rule has to be reachable outside this module.
#[test]
fn handle_for_branch_slugifies_the_branch_the_way_creation_does() {
    assert_eq!(
        handle_for_branch("homeboy", "fix/11168-wire-compiler-warning-provider"),
        "homeboy@fix-11168-wire-compiler-warning-provider"
    );
    assert_eq!(handle_for_branch("homeboy", "main"), "homeboy@main");
    assert_eq!(
        handle_for_branch("homeboy", "feat/a_b.c"),
        "homeboy@feat-a_b-c"
    );
}

#[test]
fn registry_read_lease_blocks_active_worktree_publication() {
    crate::test_support::with_isolated_home(|_| {
        let worktree = tempfile::tempdir().expect("worktree");
        let (reader_ready_tx, reader_ready_rx) = std::sync::mpsc::channel();
        let (reader_release_tx, reader_release_rx) = std::sync::mpsc::channel();
        let reader = std::thread::spawn(move || {
            with_task_worktree_registry_read_lock(|| {
                reader_ready_tx.send(()).expect("announce read lease");
                reader_release_rx.recv().expect("release read lease");
                Ok(())
            })
        });
        reader_ready_rx.recv().expect("read lease acquired");

        let path = worktree.path().to_path_buf();
        let (writer_done_tx, writer_done_rx) = std::sync::mpsc::channel();
        let writer = std::thread::spawn(move || {
            record_active_for_test("admitted-during-cleanup", &path);
            writer_done_tx.send(()).expect("announce published record");
        });
        assert!(
            writer_done_rx
                .recv_timeout(std::time::Duration::from_millis(100))
                .is_err(),
            "active worktree publication must wait for the cleanup read lease"
        );

        reader_release_tx.send(()).expect("release reader");
        reader.join().expect("join reader").expect("reader result");
        writer_done_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .expect("writer completes after read lease release");
        writer.join().expect("join writer");
        assert!(list()
            .expect("list worktrees")
            .worktrees
            .iter()
            .any(|record| record.id == "admitted-during-cleanup"));
    });
}

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
        workspace_identity: None,
        task_url: Some("https://example.com/task".to_string()),
        run_id: None,
        cleanup_policy: CleanupPolicy::RemoveWhenSafe,
        branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        state: TaskWorktreeState::Active,
        lifecycle_revision: 0,
        terminal_workspace_authority: None,
    }
}

fn exact_terminal_proof(record: &TaskWorktreeRecord) -> TerminalWorkspaceAuthorityProof {
    let authority_set = vec!["controller".to_string()];
    TerminalWorkspaceAuthorityProof {
        schema: TERMINAL_WORKSPACE_AUTHORITY_SCHEMA.to_string(),
        capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.to_string(),
        capability_version: 1,
        workspace: record.effective_workspace_identity().expect("identity"),
        task_worktree_id: record.id.clone(),
        manifest_revision: record.lifecycle_revision,
        run_id: record.run_id.clone(),
        controller_state: "Succeeded".to_string(),
        controller_version: 1,
        accepted_runner_id: None,
        accepted_runner_job_id: None,
        authority_set_fingerprint: authority_set_fingerprint(&authority_set),
        authority_set,
        observations: vec![TerminalWorkspaceAuthorityObservation {
            authority: "controller".to_string(),
            capability: TERMINAL_WORKSPACE_AUTHORITY_CAPABILITY.to_string(),
            capability_version: 1,
            status: "terminal".to_string(),
            evidence: "succeeded".to_string(),
            run_id: record.run_id.clone(),
            runner_job_id: None,
        }],
        issued_evidence: vec![match record.run_id.as_deref() {
            Some(run_id) => format!("controller-run:{run_id}"),
            None => "controller-no-run-id".to_string(),
        }],
    }
}

#[test]
fn terminal_authority_proof_requires_exact_run_terminal_state_and_complete_unique_authorities() {
    let temporary = tempfile::tempdir().expect("temporary paths");
    let mut record = fixture_record(temporary.path(), temporary.path());
    record.run_id = Some("run-a".to_string());
    let proof = exact_terminal_proof(&record);
    assert!(proof.exact_for(&record, Some("run-a")));
    assert!(!proof.exact_for(&record, Some("run-b")));
    assert!(!proof.exact_for(&record, None));

    let mut active = proof.clone();
    active.controller_state = "Running".to_string();
    assert!(!active.exact_for(&record, Some("run-a")));
    let mut wrong_capability = proof.clone();
    wrong_capability.observations[0].capability = "other".to_string();
    assert!(!wrong_capability.exact_for(&record, Some("run-a")));
    let mut duplicate = proof.clone();
    duplicate
        .observations
        .push(duplicate.observations[0].clone());
    assert!(!duplicate.exact_for(&record, Some("run-a")));
    let mut missing = proof.clone();
    missing.observations.clear();
    assert!(!missing.exact_for(&record, Some("run-a")));
    let mut extra = proof.clone();
    extra.authority_set.push("runner-a".to_string());
    extra.authority_set_fingerprint = authority_set_fingerprint(&extra.authority_set);
    assert!(!extra.exact_for(&record, Some("run-a")));

    let no_run_record = fixture_record(temporary.path(), temporary.path());
    let no_run_proof = exact_terminal_proof(&no_run_record);
    assert!(no_run_proof.exact_for(&no_run_record, None));
    assert!(!no_run_proof.exact_for(&no_run_record, Some("run-a")));
}

fn terminal_claim() -> crate::workspace_claim::WorkspaceClaim {
    crate::workspace_claim::WorkspaceClaim {
        schema: crate::workspace_claim::WORKSPACE_CLAIM_SCHEMA.to_string(),
        protocol: crate::workspace_claim::WorkspaceClaimProtocol::current(),
        workspace: WorkspaceIdentity::new("task-worktree", "fixture/fixture@task").unwrap(),
        lifecycle_revision: 1,
        token: "test-fence".to_string(),
        expires_at_ms: u64::MAX,
    }
}

struct FixedLivenessAuthority(WorktreeLivenessAuthority);

impl WorktreeReconciliationAuthority for FixedLivenessAuthority {
    fn acquire(&self, _: &TaskWorktreeRecord) -> Result<WorktreeLivenessAuthority> {
        Ok(self.0.clone())
    }

    fn validate(
        &self,
        _: &TaskWorktreeRecord,
        _: &crate::workspace_claim::WorkspaceClaim,
    ) -> Result<bool> {
        Ok(true)
    }

    fn ready_to_commit(&self, _: &crate::workspace_claim::WorkspaceClaim) -> bool {
        true
    }
}

struct RestoreAuthority(PathBuf);

impl WorktreeReconciliationAuthority for RestoreAuthority {
    fn acquire(&self, _: &TaskWorktreeRecord) -> Result<WorktreeLivenessAuthority> {
        fs::create_dir_all(&self.0).unwrap();
        Ok(WorktreeLivenessAuthority::Terminal {
            claim: terminal_claim(),
            provenance: "test authority restored the workspace".to_string(),
        })
    }

    fn validate(
        &self,
        _: &TaskWorktreeRecord,
        _: &crate::workspace_claim::WorkspaceClaim,
    ) -> Result<bool> {
        Ok(true)
    }
}

struct SlowAuthority {
    started: std::sync::mpsc::Sender<()>,
    release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
}

impl WorktreeReconciliationAuthority for SlowAuthority {
    fn acquire(&self, _: &TaskWorktreeRecord) -> Result<WorktreeLivenessAuthority> {
        self.started.send(()).unwrap();
        self.release.lock().unwrap().recv().unwrap();
        Ok(WorktreeLivenessAuthority::Terminal {
            claim: terminal_claim(),
            provenance: "slow local authority".to_string(),
        })
    }

    fn validate(
        &self,
        _: &TaskWorktreeRecord,
        _: &crate::workspace_claim::WorkspaceClaim,
    ) -> Result<bool> {
        Ok(true)
    }
}

#[test]
fn task_worktree_identity_is_path_independent_and_rejects_conflicts() {
    let source = tempfile::tempdir().expect("source");
    let first = fixture_record(source.path(), &source.path().join("one"));
    let second = fixture_record(source.path(), &source.path().join("two"));
    let identity = first
        .effective_workspace_identity()
        .expect("derive legacy identity");

    assert_eq!(
        identity,
        second
            .effective_workspace_identity()
            .expect("same identity")
    );
    assert_eq!(identity.kind, "task-worktree");
    assert_eq!(identity.locator, "fixture/fixture@task");

    let claim_store = tempfile::tempdir().expect("claim store");
    let claims = crate::workspace_claim::WorkspaceClaimStore::new(claim_store.path());
    let owner = claims
        .register_owner(identity.clone(), "agent-task-run", 1_000, 1)
        .expect("register task-worktree owner");
    assert_eq!(owner.workspace, identity);
    assert!(claims
        .acquire(
            second
                .effective_workspace_identity()
                .expect("record identity"),
            1_000,
            2
        )
        .is_err());

    let mut conflicting = first;
    conflicting.workspace_identity = Some(
        crate::workspace_claim::WorkspaceIdentity::new("task-worktree", "other/record")
            .expect("identity"),
    );
    assert!(conflicting.effective_workspace_identity().is_err());
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

fn merged_task_branch_with_stale_upstream(source: &Path, worktree: &Path) {
    run_git(
        source,
        &["worktree", "add", "-b", "task", &worktree.to_string_lossy()],
    );
    fs::write(worktree.join("task.txt"), "task\n").unwrap();
    run_git(worktree, &["add", "."]);
    run_git(worktree, &["commit", "-q", "-m", "task"]);
    run_git(source, &["branch", "stale-upstream"]);
    run_git(
        source,
        &["remote", "add", "origin", &source.to_string_lossy()],
    );
    run_git(source, &["fetch", "-q", "origin", "stale-upstream"]);
    run_git(
        source,
        &["branch", "--set-upstream-to=origin/stale-upstream", "task"],
    );
    run_git(source, &["merge", "--no-ff", "-m", "merge task", "task"]);
    run_git(source, &["worktree", "remove", &worktree.to_string_lossy()]);
    assert!(!std::process::Command::new("git")
        .args(["branch", "-d", "task"])
        .current_dir(source)
        .status()
        .unwrap()
        .success());
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

    assert_eq!(output.counts.candidates, 0);
    assert_eq!(output.counts.removed, 0);
    assert_eq!(output.counts.skipped, 1);
    assert_eq!(output.counts.reconciliation_blockers, 1);
    assert!(output.skipped[0].reasons[0].contains("inventory --apply"));
    assert_eq!(updated.state, TaskWorktreeState::Active);
}

#[test]
fn cleanup_reports_missing_active_records_as_reconciliation_blockers() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    for index in 0..4 {
        let mut record = fixture_record(
            source.path(),
            &sibling_worktree_path(source.path(), &format!("missing-{index}")),
        );
        record.id = format!("fixture@missing-{index}");
        write_record(&store, &record).unwrap();
    }

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

    assert_eq!(output.counts.candidates, 0);
    assert_eq!(output.counts.reconciliation_blockers, 4);
    assert_eq!(output.counts.skipped, 4);
    assert!(output.candidates.is_empty());
}

#[test]
fn inventory_reports_missing_path_without_fabricating_terminal_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "inventory-missing");
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &worktree);
    write_record(&store, &record).unwrap();

    let preview = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 10,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();
    assert_eq!(preview.cross_tab_scope, "task_worktree_page");
    assert_eq!(preview.cross_tab.active_path_missing, 1);
    assert_eq!(
        preview.records[0].missing_active.as_ref().unwrap().reason,
        MissingActiveWorktreeReason::BranchEvidenceUnavailable
    );
    let apply = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 10,
            apply: true,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();
    assert_eq!(
        read_record(&store, &record.id).unwrap().state,
        TaskWorktreeState::Active
    );
    assert_eq!(
        apply.authorization,
        WorktreeInventoryAuthorization::ExplicitApply
    );
    assert_eq!(
        apply.records[0].reconciliation.as_ref().unwrap().action,
        WorktreeReconciliationAction::Preserved
    );
    assert!(record_path(&store, &record.id).exists());
}

#[test]
fn inventory_keeps_finalized_manifest_when_path_is_removed_afterward() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "inventory-finalized");
    let store = dir.path().join("store");
    let mut record = fixture_record(source.path(), &worktree);
    record.state = TaskWorktreeState::Removed;
    write_record(&store, &record).unwrap();

    let inventory = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 10,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();
    assert_eq!(inventory.cross_tab.removed_path_missing, 1);
    assert!(inventory.records[0].missing_active.is_none());
    assert!(record_path(&store, &record.id).exists());
}

#[test]
fn inventory_reports_adopted_paths_without_reconciling_them() {
    let dir = tempfile::tempdir().unwrap();
    let adopted_store = dir.path().join("adopted");
    let record = AdoptedWorkspaceRecord {
        handle: "external".to_string(),
        path: dir.path().join("missing-adopted").display().to_string(),
        kind: None,
        provenance: None,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        state: TaskWorktreeState::Active,
    };
    write_adopted_record(&adopted_store, &record).unwrap();

    let inventory = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 10,
            apply: true,
            ..Default::default()
        },
        &dir.path().join("store"),
        &adopted_store,
    )
    .unwrap();
    assert!(!inventory.adopted.records[0].path_exists);
    assert_eq!(
        inventory.adopted.records[0].reason,
        MissingActiveWorktreeReason::AdoptedWorkspace
    );
    assert!(record_path(&adopted_store, &record.handle).exists());
}

#[test]
fn inventory_preserves_missing_records_when_local_evidence_is_incomplete() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    let mut live = fixture_record(source.path(), &sibling_worktree_path(source.path(), "live"));
    live.id = "fixture@live".to_string();
    live.run_id = Some("live-controller".to_string());
    write_record(&store, &live).unwrap();
    let mut ambiguous = fixture_record(
        &dir.path().join("missing-source"),
        &dir.path().join("missing-worktree"),
    );
    ambiguous.id = "fixture@ambiguous".to_string();
    ambiguous.component_id = "missing".to_string();
    write_record(&store, &ambiguous).unwrap();

    let inventory = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 10,
            apply: true,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();
    assert_eq!(inventory.cross_tab.active_path_missing, 2);
    assert_eq!(
        inventory.records[1].missing_active.as_ref().unwrap().reason,
        MissingActiveWorktreeReason::BranchEvidenceUnavailable
    );
    assert_eq!(
        inventory.records[0].missing_active.as_ref().unwrap().reason,
        MissingActiveWorktreeReason::SourceCheckoutUnavailable
    );
    assert_eq!(
        read_record(&store, &live.id).unwrap().state,
        TaskWorktreeState::Active
    );
    assert_eq!(
        read_record(&store, &ambiguous.id).unwrap().state,
        TaskWorktreeState::Active
    );
}

#[test]
fn inventory_pages_the_cross_tab_without_claiming_uninspected_records() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    let mut first = fixture_record(
        source.path(),
        &sibling_worktree_path(source.path(), "first"),
    );
    first.id = "fixture@a".to_string();
    let mut second = fixture_record(
        source.path(),
        &sibling_worktree_path(source.path(), "second"),
    );
    second.id = "fixture@b".to_string();
    write_record(&store, &first).unwrap();
    write_record(&store, &second).unwrap();

    let first_page = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 1,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();

    assert_eq!(first_page.total, 2);
    assert!(first_page.truncated);
    assert_eq!(first_page.cross_tab_scope, "task_worktree_page");
    assert_eq!(first_page.cross_tab.active_path_missing, 1);
    assert_eq!(first_page.next_cursor.as_deref(), Some("fixture@a"));

    let second_page = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 1,
            cursor: first_page.next_cursor,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();
    assert!(!second_page.truncated);
    assert_eq!(second_page.records[0].record.id, "fixture@b");
}

#[test]
fn inventory_reports_actual_dirty_source_and_unpushed_branch_evidence() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    let mut dirty = fixture_record(
        source.path(),
        &sibling_worktree_path(source.path(), "dirty"),
    );
    dirty.id = "fixture@dirty".to_string();
    fs::write(source.path().join("dirty.txt"), "dirty\n").unwrap();
    write_record(&store, &dirty).unwrap();

    let dirty_inventory = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 10,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();
    let dirty_missing = dirty_inventory.records[0].missing_active.as_ref().unwrap();
    assert_eq!(
        dirty_missing.reason,
        MissingActiveWorktreeReason::SourceDirty
    );
    assert_eq!(dirty_missing.local_evidence.source_dirty, Some(true));

    fs::remove_file(source.path().join("dirty.txt")).unwrap();
    let base = std::process::Command::new("git")
        .args(["branch", "--show-current"])
        .current_dir(source.path())
        .output()
        .unwrap();
    let base = String::from_utf8(base.stdout).unwrap().trim().to_string();
    run_git(source.path(), &["checkout", "-q", "-b", "task"]);
    fs::write(source.path().join("task.txt"), "task\n").unwrap();
    run_git(source.path(), &["add", "."]);
    run_git(source.path(), &["commit", "-q", "-m", "task"]);
    run_git(source.path(), &["checkout", "-q", &base]);

    let mut unpushed = fixture_record(
        source.path(),
        &sibling_worktree_path(source.path(), "unpushed"),
    );
    unpushed.id = "fixture@unpushed".to_string();
    unpushed.branch = "task".to_string();
    unpushed.base_ref = base;
    write_record(&store, &unpushed).unwrap();
    let unpushed_inventory = inventory_with_store(
        WorktreeInventoryOptions {
            limit: 10,
            cursor: Some("fixture@dirty".to_string()),
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
    )
    .unwrap();
    let unpushed_missing = unpushed_inventory.records[0]
        .missing_active
        .as_ref()
        .unwrap();
    assert_eq!(
        unpushed_missing.reason,
        MissingActiveWorktreeReason::UnpushedBranch
    );
    assert_eq!(
        unpushed_missing.local_evidence.unpushed_branch_commits,
        Some(1)
    );
}

#[test]
fn inventory_reconciles_only_a_leased_terminal_clean_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    run_git(source.path(), &["branch", "task"]);
    let mut record = fixture_record(source.path(), &sibling_worktree_path(source.path(), "gone"));
    record.base_ref = "HEAD".to_string();
    write_record(&store, &record).unwrap();
    let authority = FixedLivenessAuthority(WorktreeLivenessAuthority::Terminal {
        claim: terminal_claim(),
        provenance: "authoritative local terminal receipt".to_string(),
    });

    let output = inventory_with_store_and_authority(
        WorktreeInventoryOptions {
            limit: 10,
            apply: true,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
        &authority,
    )
    .unwrap();

    assert_eq!(
        output.authorization,
        WorktreeInventoryAuthorization::ExplicitApply
    );
    assert_eq!(
        output.records[0].reconciliation.as_ref().unwrap().action,
        WorktreeReconciliationAction::Reconciled
    );
    assert!(output.records[0]
        .reconciliation
        .as_ref()
        .unwrap()
        .provenance
        .contains("leased manifest re-read"));
    assert_eq!(
        read_record(&store, &record.id).unwrap().state,
        TaskWorktreeState::Removed
    );
}

#[test]
fn inventory_refuses_expired_workspace_claims() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    run_git(source.path(), &["branch", "task"]);
    let record = fixture_record(source.path(), &sibling_worktree_path(source.path(), "gone"));
    write_record(&store, &record).unwrap();

    let mut expired = terminal_claim();
    expired.expires_at_ms = 1;
    let expired_authority = FixedLivenessAuthority(WorktreeLivenessAuthority::Terminal {
        claim: expired,
        provenance: "expired claim".to_string(),
    });
    let expired_output = inventory_with_store_and_authority(
        WorktreeInventoryOptions {
            limit: 10,
            apply: true,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
        &expired_authority,
    )
    .unwrap();
    assert_eq!(
        expired_output.records[0]
            .reconciliation
            .as_ref()
            .unwrap()
            .action,
        WorktreeReconciliationAction::Refused
    );
    assert_eq!(
        read_record(&store, &record.id).unwrap().state,
        TaskWorktreeState::Active
    );
}

#[test]
fn inventory_preserves_live_and_incomplete_remote_authority() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    run_git(source.path(), &["branch", "task"]);
    let record = fixture_record(source.path(), &sibling_worktree_path(source.path(), "gone"));
    write_record(&store, &record).unwrap();

    for (authority, expected_action) in [
        (
            FixedLivenessAuthority(WorktreeLivenessAuthority::Live {
                provenance: "remote runner reports active".to_string(),
            }),
            WorktreeReconciliationAction::Preserved,
        ),
        (
            FixedLivenessAuthority(WorktreeLivenessAuthority::Incomplete {
                reason: "external provider cannot enumerate offloaded runs".to_string(),
            }),
            WorktreeReconciliationAction::Refused,
        ),
    ] {
        let output = inventory_with_store_and_authority(
            WorktreeInventoryOptions {
                limit: 10,
                apply: true,
                ..Default::default()
            },
            &store,
            &dir.path().join("adopted"),
            &authority,
        )
        .unwrap();
        assert_eq!(
            output.records[0].reconciliation.as_ref().unwrap().action,
            expected_action
        );
        assert_eq!(
            read_record(&store, &record.id).unwrap().state,
            TaskWorktreeState::Active
        );
    }
}

#[test]
fn inventory_preserves_a_workspace_with_a_second_active_owner() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    run_git(source.path(), &["branch", "task"]);
    let path = sibling_worktree_path(source.path(), "shared");
    let mut first = fixture_record(source.path(), &path);
    first.id = "fixture@first".to_string();
    let mut second = fixture_record(source.path(), &path);
    second.id = "fixture@second".to_string();
    write_record(&store, &first).unwrap();
    write_record(&store, &second).unwrap();
    let authority = FixedLivenessAuthority(WorktreeLivenessAuthority::Terminal {
        claim: terminal_claim(),
        provenance: "terminal".to_string(),
    });

    let output = inventory_with_store_and_authority(
        WorktreeInventoryOptions {
            limit: 10,
            apply: true,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
        &authority,
    )
    .unwrap();
    assert!(output
        .records
        .iter()
        .all(|item| item.reconciliation.as_ref().unwrap().action
            == WorktreeReconciliationAction::Preserved));
    assert_eq!(
        read_record(&store, &first.id).unwrap().state,
        TaskWorktreeState::Active
    );
    assert_eq!(
        read_record(&store, &second.id).unwrap().state,
        TaskWorktreeState::Active
    );
}

#[test]
fn inventory_reports_a_path_restored_during_authority_snapshot() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    run_git(source.path(), &["branch", "task"]);
    let path = sibling_worktree_path(source.path(), "restored");
    let record = fixture_record(source.path(), &path);
    write_record(&store, &record).unwrap();

    let output = inventory_with_store_and_authority(
        WorktreeInventoryOptions {
            limit: 10,
            apply: true,
            ..Default::default()
        },
        &store,
        &dir.path().join("adopted"),
        &RestoreAuthority(path),
    )
    .unwrap();
    assert!(output.records[0].path_exists);
    assert!(output.records[0].missing_active.is_none());
    assert_eq!(
        read_record(&store, &record.id).unwrap().state,
        TaskWorktreeState::Active
    );
}

#[test]
fn inventory_does_not_hold_the_registry_lease_while_authority_is_slow() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    run_git(source.path(), &["branch", "task"]);
    let record = fixture_record(source.path(), &sibling_worktree_path(source.path(), "slow"));
    write_record(&store, &record).unwrap();
    let (started_tx, started_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let authority = std::sync::Arc::new(SlowAuthority {
        started: started_tx,
        release: std::sync::Mutex::new(release_rx),
    });
    let store_for_thread = store.clone();
    let adopted = dir.path().join("adopted");
    let authority_for_thread = authority.clone();
    let worker = std::thread::spawn(move || {
        inventory_with_store_and_authority(
            WorktreeInventoryOptions {
                limit: 10,
                apply: true,
                ..Default::default()
            },
            &store_for_thread,
            &adopted,
            authority_for_thread.as_ref(),
        )
    });
    started_rx
        .recv_timeout(std::time::Duration::from_secs(5))
        .unwrap();
    let (lock_tx, lock_rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        with_task_worktree_registry_read_lock(|| {
            lock_tx.send(()).unwrap();
            Ok(())
        })
        .unwrap();
    });
    lock_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("slow authority must run before the write lease");
    release_tx.send(()).unwrap();
    worker.join().unwrap().unwrap();
}

#[test]
fn cleanup_deletes_merged_task_branch_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "merged-branch-cleanup");
    merged_task_branch_with_stale_upstream(source.path(), &worktree);
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
    assert_eq!(output.counts.skipped, 1);
    assert!(std::process::Command::new("git")
        .args(["show-ref", "--verify", "--quiet", "refs/heads/task"])
        .current_dir(source.path())
        .status()
        .unwrap()
        .code()
        .is_some_and(|code| code == 0));
    assert!(!worktree.exists());

    let retry = cleanup_with_store(
        WorktreeCleanupOptions {
            force: false,
            dry_run: false,
            cleanup_branches: true,
            allow_unmerged_branches: false,
        },
        &store,
    )
    .unwrap();
    assert_eq!(retry.counts.candidates, 0);
    assert_eq!(retry.counts.removed, 0);
    assert_eq!(retry.counts.branches_deleted, 0);
    assert_eq!(retry.counts.reconciliation_blockers, 1);
}

#[test]
fn remove_deletes_merged_branch_with_stale_upstream_when_requested() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "exact-merged-branch-cleanup");
    merged_task_branch_with_stale_upstream(source.path(), &worktree);
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &worktree);
    write_record(&store, &record).unwrap();

    let output = remove_with_store(
        WorktreeRemoveOptions {
            id: record.id.clone(),
            force: false,
            cleanup_branch: true,
            allow_unmerged_branch: false,
        },
        &store,
    )
    .unwrap();

    assert_eq!(output.branch_cleanup.status, BranchCleanupStatus::Deleted);
    assert!(output.branch_cleanup.deleted);
    assert!(std::process::Command::new("git")
        .args(["show-ref", "--verify", "--quiet", "refs/heads/task"])
        .current_dir(source.path())
        .status()
        .unwrap()
        .code()
        .is_some_and(|code| code != 0));
    assert!(!worktree.exists());
}

#[test]
fn cleanup_keeps_branch_when_worktree_removal_fails_and_continues() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let locked_worktree = sibling_worktree_path(source.path(), "locked-cleanup");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "locked-task",
            &locked_worktree.to_string_lossy(),
        ],
    );
    run_git(
        source.path(),
        &["worktree", "lock", &locked_worktree.to_string_lossy()],
    );
    let store = dir.path().join("store");
    let mut locked_record = fixture_record(source.path(), &locked_worktree);
    locked_record.id = "fixture@locked".to_string();
    locked_record.branch = "locked-task".to_string();
    let removable_worktree = sibling_worktree_path(source.path(), "cleanup-continues");
    let mut removable_record = fixture_record(source.path(), &removable_worktree);
    removable_record.id = "fixture@removable".to_string();
    write_record(&store, &locked_record).unwrap();
    write_record(&store, &removable_record).unwrap();

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

    assert_eq!(output.counts.candidates, 1);
    assert_eq!(output.counts.removed, 0);
    assert_eq!(output.counts.skipped, 2);
    assert_eq!(output.counts.reconciliation_blockers, 1);
    assert!(locked_worktree.exists());
    run_git(
        source.path(),
        &["show-ref", "--verify", "--quiet", "refs/heads/locked-task"],
    );
    assert_eq!(
        read_record(&store, &locked_record.id).unwrap().state,
        TaskWorktreeState::Active
    );
    assert_eq!(
        read_record(&store, &removable_record.id).unwrap().state,
        TaskWorktreeState::Active
    );
}

#[test]
fn cleanup_separates_actionable_candidates_from_reconciliation_blockers() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    let removable = sibling_worktree_path(source.path(), "mixed-removable");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "mixed-removable",
            &removable.to_string_lossy(),
        ],
    );
    let removable_record = fixture_record(source.path(), &removable);
    let mut missing_record = fixture_record(
        source.path(),
        &sibling_worktree_path(source.path(), "mixed-missing"),
    );
    missing_record.id = "fixture@mixed-missing".to_string();
    write_record(&store, &removable_record).unwrap();
    write_record(&store, &missing_record).unwrap();

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

    assert_eq!(output.counts.candidates, 1);
    assert_eq!(output.counts.reconciliation_blockers, 1);
    assert_eq!(output.counts.skipped, 1);
    assert_eq!(output.candidates.len(), 1);
    assert_eq!(output.skipped.len(), 1);
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
    assert_eq!(output.counts.unmerged_branches, 0);
    assert_eq!(output.counts.skipped, 1);
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

        assert_eq!(output.counts.candidates, 0);
        assert_eq!(output.counts.removed, 0);
        assert_eq!(output.counts.skipped, 2);
        assert_eq!(output.counts.reconciliation_blockers, 1);
        assert_eq!(skipped.state, TaskWorktreeState::Active);
        assert_eq!(removed.state, TaskWorktreeState::Active);
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

    assert_eq!(output.counts.candidates, 0);
    assert_eq!(output.counts.removed, 0);
    assert_eq!(output.counts.skipped, 2);
    assert_eq!(output.counts.reconciliation_blockers, 1);
    assert_eq!(output.skipped[0].record.id, dirty_record.id);
    assert!(output.skipped[0]
        .reasons
        .iter()
        .any(|reason| reason == "dirty worktree"));
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

    assert_eq!(output.counts.candidates, 0);
    assert_eq!(output.counts.removed, 0);
    assert_eq!(output.counts.skipped, 1);
    assert_eq!(output.counts.reconciliation_blockers, 0);
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
        requests: vec![
            WorktreeQueueCreateRequest {
                branch: "cook/one".to_string(),
                task_url: Some("https://github.com/Extra-Chill/homeboy/issues/5786".to_string()),
                task_ref: Some("Extra-Chill/homeboy#5786".to_string()),
                run_id: None,
                provider_lifecycle: None,
            },
            WorktreeQueueCreateRequest {
                branch: "cook/two".to_string(),
                task_url: Some("https://github.com/Extra-Chill/homeboy/issues/5786".to_string()),
                task_ref: Some("Extra-Chill/homeboy#5786".to_string()),
                run_id: None,
                provider_lifecycle: None,
            },
        ],
        from: "origin/main".to_string(),
        dry_run: true,
        retry_after_seconds: 30,
    }
}

#[test]
fn queue_create_dry_run_returns_queued_rows_with_exact_homeboy_commands() {
    use crate::test_support::with_isolated_home;

    with_isolated_home(|home| {
        let parent = home.path().join("Developer");
        let source = parent.join("queue-fixture");
        fs::create_dir_all(&source).unwrap();
        run_git(&source, &["init", "-q"]);
        run_git(&source, &["config", "user.email", "homeboy@example.com"]);
        run_git(&source, &["config", "user.name", "Homeboy Test"]);
        fs::write(source.join("homeboy.json"), r#"{"id":"queue-fixture"}"#).unwrap();
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-q", "-m", "initial"]);
        write_component_registration(home.path(), "queue-fixture", &source);

        let mut options = queue_options();
        options.repo = "queue-fixture".to_string();
        options.from = "HEAD".to_string();
        let output = queue_create(options).unwrap();

        assert_eq!(output.schema, "homeboy/worktree-queue-create/v1");
        assert_eq!(output.rows.len(), 2);
        assert_eq!(
            output.rows[0].status,
            WorktreeQueueCreateStatus::WouldCreate
        );
        assert_eq!(output.rows[0].handle, "queue-fixture@cook-one");
        assert_eq!(
            output.rows[0].path.as_deref(),
            Some(
                parent
                    .canonicalize()
                    .unwrap()
                    .join("queue-fixture@cook-one")
                    .to_str()
                    .unwrap()
            )
        );
        assert_eq!(
            output.rows[0].command,
            vec![
                "homeboy",
                "worktree",
                "create",
                "queue-fixture",
                "--branch",
                "cook/one",
                "--from",
                "HEAD",
                "--task-url",
                "https://github.com/Extra-Chill/homeboy/issues/5786",
            ]
        );
        assert!(!parent.join("queue-fixture@cook-one").exists());
    });
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
            requests: vec![WorktreeQueueCreateRequest {
                branch: "cook/one".to_string(),
                task_url: Some("https://github.com/Extra-Chill/homeboy/issues/5924".to_string()),
                task_ref: None,
                run_id: None,
                provider_lifecycle: None,
            }],
            from: "HEAD".to_string(),
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

#[cfg(unix)]
#[test]
fn queue_create_uses_provider_lifecycle_with_per_child_metadata() {
    use std::os::unix::fs::PermissionsExt;

    crate::test_support::with_isolated_home(|_| {
        let temp = tempfile::tempdir().expect("provider fixture");
        let workspace = temp.path().join("workspace");
        let records = temp.path().join("records");
        let script = temp.path().join("provider");
        std::fs::write(
            &script,
            format!(
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  if [ -d '{}' ]; then printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@fix-12124\",\"path\":\"{}\",\"branch\":\"fix/12124\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'; else printf '%s\\n' '{{\"worktrees\":[]}}'; fi\nelif [ \"$1\" = ensure ]; then\n  printf 'ensure|%s|%s|%s|%s|%s|%s|%s|%s\\n' \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\" \"$8\" \"$9\" >> '{}'\n  if [ ! -d '{}' ]; then git init -q -b fix/12124 '{}'; fi\nelse\n  printf 'finalize|%s|%s|%s|%s|%s|%s\\n' \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\" >> '{}'\nfi\n",
                workspace.display(),
                workspace.display(),
                records.display(),
                workspace.display(),
                workspace.display(),
                records.display(),
            ),
        )
        .expect("write provider");
        let mut permissions = std::fs::metadata(&script)
            .expect("provider metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&script, permissions).expect("make provider executable");

        let mut config = crate::defaults::HomeboyConfig::default();
        config.worktree_providers.insert(
            "fixture".to_string(),
            crate::defaults::WorktreeProviderConfig {
                enabled: true,
                kind: crate::defaults::WorktreeProviderKind::Command,
                apply_enabled: true,
                lookup_timeout_ms: 10_000,
                mutation_timeout_ms: 30_000,
                lookup_output_limit_bytes: 64 * 1024,
                commands: crate::defaults::WorktreeProviderCommands {
                    resolve: Some(vec![
                        script.display().to_string(),
                        "resolve".to_string(),
                        "{handle}".to_string(),
                    ]),
                    resolve_not_found_exit_codes: vec![1],
                    ensure: Some(vec![
                        script.display().to_string(),
                        "ensure".to_string(),
                        "{handle}".to_string(),
                        "{repo}".to_string(),
                        "{base}".to_string(),
                        "{head}".to_string(),
                        "{task_url}".to_string(),
                        "{purpose}".to_string(),
                        "{owner_run_ref}".to_string(),
                        "{cleanup_policy}".to_string(),
                    ]),
                    ..Default::default()
                },
                list_result_mapping: Some(crate::defaults::WorktreeProviderListResultMapping {
                    items: "$.worktrees".to_string(),
                    handle: "$.handle".to_string(),
                    path: "$.path".to_string(),
                    branch: "$.branch".to_string(),
                    dirty: "$.safety.dirty".to_string(),
                    unpushed: "$.safety.unpushed".to_string(),
                    primary: "$.safety.primary".to_string(),
                    task_url: None,
                }),
            },
        );
        config.settings.insert(
            crate::worktree_providers::WORKTREE_PROVIDER_LIFECYCLE_SETTINGS_KEY.to_string(),
            serde_json::json!({ "fixture": { "finalize": [script.display().to_string(), "finalize", "{handle}", "{purpose}", "{owner_run_ref}", "{cleanup_policy}", "{disposition}", "{idempotency_key}"] } }),
        );
        crate::defaults::save_config(&config).expect("save provider config");

        let lifecycle = crate::worktree_providers::WorktreeProviderLifecycleIntent {
            purpose: "agent_task_cook".to_string(),
            owner_run_ref: "cook-issue-12124".to_string(),
            cleanup_policy:
                crate::worktree_providers::WorktreeProviderCleanupPolicy::RemoveOnSuccess,
        };
        let request = WorktreeQueueCreateRequest {
            branch: "fix/12124".to_string(),
            task_url: Some("https://github.com/Extra-Chill/homeboy/issues/12124".to_string()),
            task_ref: Some("Extra-Chill/homeboy#12124".to_string()),
            run_id: Some(lifecycle.owner_run_ref.clone()),
            provider_lifecycle: Some(lifecycle.clone()),
        };
        let options = WorktreeQueueCreateOptions {
            repo: "homeboy".to_string(),
            requests: vec![request],
            from: "main".to_string(),
            dry_run: false,
            retry_after_seconds: 30,
        };
        let first = queue_create(options.clone()).expect("provider creates worktree");
        let second = queue_create(options).expect("provider reuses worktree");
        assert_eq!(
            first.rows[0].path.as_deref(),
            workspace.to_str(),
            "provider queue row: {:?}",
            first.rows[0]
        );
        assert_eq!(second.rows[0].status, WorktreeQueueCreateStatus::Created);
        let records_text = std::fs::read_to_string(&records).expect("provider records");
        assert!(records_text.lines().all(|line| line == "ensure|homeboy@fix-12124|homeboy|main|fix/12124|https://github.com/Extra-Chill/homeboy/issues/12124|agent_task_cook|cook-issue-12124|remove_on_success"));

        let resolution =
            crate::worktree_providers::resolve_apply_enabled_worktree_provider_from_config(
                "homeboy@fix-12124",
                &config,
                None,
            )
            .expect("resolve provider worktree");
        crate::worktree_providers::finalize_apply_enabled_worktree_provider_from_config(
            &resolution,
            &lifecycle,
            crate::worktree_providers::WorktreeProviderTerminalDisposition::Succeeded,
            &config,
        )
        .expect("finalize provider worktree");
        assert!(std::fs::read_to_string(records).expect("finalization record").contains(
            "finalize|homeboy@fix-12124|agent_task_cook|cook-issue-12124|remove_on_success|succeeded|finalize:cook-issue-12124"
        ));
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
            requests: vec![WorktreeQueueCreateRequest {
                branch: "cook/lab".to_string(),
                task_url: None,
                task_ref: None,
                run_id: None,
                provider_lifecycle: None,
            }],
            from: "HEAD".to_string(),
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
