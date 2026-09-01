use super::*;
use crate::test_support::write_component_registration;

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
        terminal_disposition: None,
        branch_cleanup_intent: BranchCleanupIntent::DeleteWhenMerged,
        created_at: "2026-01-01T00:00:00Z".to_string(),
        state: TaskWorktreeState::Active,
        lifecycle_revision: 0,
        terminal_workspace_authority: None,
    }
}

fn succeeded_record(source: &Path, worktree: &Path) -> TaskWorktreeRecord {
    let mut record = fixture_record(source, worktree);
    record.run_id = Some("completed-owner".to_string());
    record.terminal_disposition = Some("succeeded".to_string());
    record.lifecycle_revision = 1;
    record
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

#[test]
fn ownership_probe_names_the_live_holder_for_a_nested_checkout_path() {
    let data_root = tempfile::tempdir().expect("data root");
    let checkout = tempfile::tempdir().expect("checkout");
    let nested = checkout.path().join("crates/component");
    fs::create_dir_all(&nested).expect("nested component");
    let mut record = fixture_record(checkout.path(), checkout.path());
    record.run_id = Some("ses_live_holder".to_string());
    write_record(&metadata_dir_in_root(data_root.path()), &record).expect("record worktree");
    let claims = crate::workspace_claim::WorkspaceClaimStore::new(
        data_root
            .path()
            .join(crate::workspace_claim::LOCAL_WORKSPACE_CLAIMS_DIR),
    );
    claims
        .register_owner(
            record.effective_workspace_identity().expect("identity"),
            "ses_live_holder",
            10_000,
            1_000,
        )
        .expect("register holder");

    let probe = ownership_probe_in_root(&nested, data_root.path(), 2_000)
        .expect("probe succeeds")
        .expect("managed checkout");

    assert_eq!(probe.holder.as_deref(), Some("ses_live_holder"));
    assert_eq!(probe.lifecycle_state, "active");
    assert_eq!(probe.activity, WorktreeLeaseActivity::Live);
    assert!(probe.heartbeat_fresh);
    assert_eq!(probe.lease_expires_at_ms, Some(11_000));
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

fn registered_create_fixture(home: &Path, id: &str) -> (PathBuf, WorktreeCreateOptions) {
    let parent = home.join("Developer");
    let source = parent.join(id);
    fs::create_dir_all(&source).expect("source directory");
    run_git(&source, &["init", "-q"]);
    run_git(&source, &["config", "user.email", "homeboy@example.com"]);
    run_git(&source, &["config", "user.name", "Homeboy Test"]);
    fs::write(source.join("README.md"), "initial\n").expect("initial file");
    fs::write(source.join("homeboy.json"), format!(r#"{{"id":"{id}"}}"#))
        .expect("component manifest");
    run_git(&source, &["add", "."]);
    run_git(&source, &["commit", "-q", "-m", "initial"]);
    write_component_registration(home, id, &source);
    (
        source,
        WorktreeCreateOptions {
            component_id: id.to_string(),
            branch: "fix/restore".to_string(),
            from: Some("HEAD".to_string()),
            task_url: Some("https://example.com/tasks/restore".to_string()),
            run_id: None,
            cleanup_policy: None,
            require_handoff_freshness: false,
        },
    )
}

#[test]
fn create_accepts_an_existing_repository_path_without_component_registration() {
    crate::test_support::with_isolated_home(|home| {
        let parent = home.path().join("Developer");
        let source = parent.join("repository-fixture");
        fs::create_dir_all(&source).expect("source directory");
        run_git(&source, &["init", "-q"]);
        run_git(&source, &["config", "user.email", "homeboy@example.com"]);
        run_git(&source, &["config", "user.name", "Homeboy Test"]);
        fs::write(source.join("README.md"), "initial\n").expect("initial file");
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-q", "-m", "initial"]);

        let created = create(WorktreeCreateOptions {
            component_id: source.to_string_lossy().to_string(),
            branch: "fix/repository-path".to_string(),
            from: Some("HEAD".to_string()),
            task_url: Some("https://example.com/tasks/repository-path".to_string()),
            run_id: None,
            cleanup_policy: None,
            require_handoff_freshness: false,
        })
        .expect("create from repository path");

        assert_eq!(created.record.component_id, "repository-fixture");
        assert_eq!(created.record.id, "repository-fixture@fix-repository-path");
        assert_eq!(created.record.source_checkout, source.to_string_lossy());
        assert_eq!(
            created.record.worktree_path,
            parent
                .join("repository-fixture@fix-repository-path")
                .to_string_lossy()
        );
        assert_eq!(created.record.branch, "fix/repository-path");
        assert!(Path::new(&created.record.worktree_path).is_dir());
    });
}

#[test]
fn import_records_an_exact_existing_worktree_without_changing_git_and_replays_idempotently() {
    crate::test_support::with_isolated_home(|home| {
        let (source, _) = registered_create_fixture(home.path(), "import-fixture");
        let branch = "fix/import";
        let handle = "import-fixture@fix-import";
        let path = source.parent().expect("source parent").join(handle);
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &path.to_string_lossy(),
                "HEAD",
            ],
        );
        let head_before = git::run_git(&path, &["rev-parse", "HEAD"], "git head").unwrap();
        let status_before = git::run_git(&path, &["status", "--porcelain"], "git status").unwrap();
        let options = WorktreeImportOptions {
            component_id: "import-fixture".to_string(),
            handle: handle.to_string(),
            path: path.display().to_string(),
            branch: branch.to_string(),
            base_ref: "HEAD~0".to_string(),
            task_url: Some("https://example.com/tasks/import".to_string()),
            owner_run_ref: Some("run-import".to_string()),
            cleanup_policy: CleanupPolicy::PreserveOnFailure,
            created_at: Some("2026-01-02T03:04:05Z".to_string()),
        };

        let imported = import(options.clone()).expect("import exact worktree");
        let replay = import(options).expect("replay exact import");

        assert!(imported.imported);
        assert!(!replay.imported);
        assert_eq!(replay.record, imported.record);
        assert_eq!(imported.record.created_at, "2026-01-02T03:04:05Z");
        assert_eq!(imported.record.run_id.as_deref(), Some("run-import"));
        assert_eq!(
            list()
                .expect("list imported worktree")
                .worktrees
                .into_iter()
                .find(|record| record.id == handle)
                .expect("imported record is listed"),
            imported.record
        );
        assert_eq!(
            status(handle).expect("status imported worktree").record,
            imported.record
        );
        assert_eq!(
            git::run_git(&path, &["rev-parse", "HEAD"], "git head").unwrap(),
            head_before
        );
        assert_eq!(
            git::run_git(&path, &["status", "--porcelain"], "git status").unwrap(),
            status_before
        );

        let finalized = finalize_provider_lifecycle(
            handle,
            "run-import",
            crate::worktree_provider::WorktreeTerminalDisposition::Failed,
        )
        .expect("finalize imported worktree");
        assert_eq!(finalized.terminal_disposition.as_deref(), Some("failed"));
        let cleanup = cleanup(WorktreeCleanupOptions {
            force: false,
            dry_run: true,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        })
        .expect("preview imported worktree cleanup");
        assert!(cleanup.dry_run);
        assert!(path.exists(), "cleanup preview must preserve the worktree");
    });
}

#[test]
fn import_refuses_primary_mismatched_handle_and_conflicting_replay() {
    crate::test_support::with_isolated_home(|home| {
        let (source, _) = registered_create_fixture(home.path(), "import-conflict");
        let branch = "fix/import";
        let handle = "import-conflict@fix-import";
        let path = source.parent().expect("source parent").join(handle);
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                branch,
                &path.to_string_lossy(),
                "HEAD",
            ],
        );
        let options = WorktreeImportOptions {
            component_id: "import-conflict".to_string(),
            handle: handle.to_string(),
            path: path.display().to_string(),
            branch: branch.to_string(),
            base_ref: "HEAD".to_string(),
            task_url: None,
            owner_run_ref: Some("run-import".to_string()),
            cleanup_policy: CleanupPolicy::RemoveWhenSafe,
            created_at: None,
        };

        let mut primary = options.clone();
        primary.path = source.display().to_string();
        assert!(import(primary).is_err());
        let mut wrong_handle = options.clone();
        wrong_handle.handle = "import-conflict@wrong".to_string();
        assert!(import(wrong_handle).is_err());
        let mut malformed = options.clone();
        malformed.created_at = Some("not-a-timestamp".to_string());
        assert!(import(malformed).is_err());
        import(options.clone()).expect("initial import");
        let mut conflicting = options;
        conflicting.base_ref = "origin/main".to_string();
        assert!(import(conflicting).is_err());
    });
}

#[test]
fn finalization_is_owner_bound_idempotent_conflict_safe_and_never_cleans_up() {
    crate::test_support::with_isolated_home(|home| {
        let (_, mut options) = registered_create_fixture(home.path(), "finalize-fixture");
        options.run_id = Some("run-finalize".to_string());
        let created = create(options).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);

        assert!(finalize_provider_lifecycle(
            &created.record.id,
            "wrong-owner",
            crate::worktree_provider::WorktreeTerminalDisposition::Failed,
        )
        .is_err());
        let finalized = finalize_provider_lifecycle(
            &created.record.id,
            "run-finalize",
            crate::worktree_provider::WorktreeTerminalDisposition::Failed,
        )
        .expect("finalize worktree");
        let replay = finalize_provider_lifecycle(
            &created.record.id,
            "run-finalize",
            crate::worktree_provider::WorktreeTerminalDisposition::Failed,
        )
        .expect("replay finalization");

        assert_eq!(replay, finalized);
        assert_eq!(finalized.lifecycle_revision, 1);
        assert_eq!(finalized.terminal_disposition.as_deref(), Some("failed"));
        assert_eq!(finalized.cleanup_policy, CleanupPolicy::PreserveOnFailure);
        assert!(path.exists(), "finalization must not clean up the worktree");
        assert!(finalize_provider_lifecycle(
            &created.record.id,
            "run-finalize",
            crate::worktree_provider::WorktreeTerminalDisposition::Succeeded,
        )
        .is_err());
    });
}

#[test]
fn default_ownerless_create_remains_eligible_for_safe_cleanup() {
    crate::test_support::with_isolated_home(|home| {
        let (_, options) = registered_create_fixture(home.path(), "ownerless-cleanup");
        let created = create(options).expect("create ownerless task worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        assert_eq!(created.record.run_id, None);
        assert_eq!(created.record.terminal_disposition, None);
        assert_eq!(created.record.cleanup_policy, CleanupPolicy::RemoveWhenSafe);

        let output = cleanup(WorktreeCleanupOptions {
            force: false,
            dry_run: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        })
        .expect("clean up ownerless task worktree");

        assert_eq!(output.counts.removed, 1);
        assert_eq!(output.counts.skipped, 0);
        assert!(!path.exists());
    });
}

#[test]
fn owned_remove_when_safe_worktree_requires_succeeded_terminal_finalization() {
    let data_root = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "owned-nonterminal-cleanup");
    run_git(
        source.path(),
        &[
            "worktree",
            "add",
            "-b",
            "owned-nonterminal-cleanup",
            &worktree.to_string_lossy(),
        ],
    );
    let store = data_root.path().join("task-worktrees");
    let mut record = fixture_record(source.path(), &worktree);
    record.run_id = Some("lifecycle-owner".to_string());
    write_record(&store, &record).unwrap();

    for terminal_disposition in [None, Some("failed".to_string())] {
        record.terminal_disposition = terminal_disposition;
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

        assert_eq!(output.counts.removed, 0);
        assert_eq!(output.counts.skipped, 1);
        assert!(output.skipped[0].reasons.iter().any(|reason| {
            reason.contains("explicit succeeded finalization from lifecycle owner")
        }));
        assert!(worktree.exists());
    }
}

#[test]
fn owned_worktree_survives_push_and_pr_boundary_until_finalization_and_cwd_exit() {
    crate::test_support::with_isolated_home(|home| {
        let developer = home.path().join("Developer");
        let remote = home.path().join("remote.git");
        let source = developer.join("lifecycle-fixture");
        fs::create_dir_all(&developer).expect("developer directory");
        fs::create_dir_all(&remote).expect("remote directory");
        run_git(&remote, &["init", "--bare", "-q"]);
        run_git(
            &developer,
            &[
                "clone",
                "-q",
                &remote.to_string_lossy(),
                "lifecycle-fixture",
            ],
        );
        run_git(&source, &["config", "user.email", "homeboy@example.com"]);
        run_git(&source, &["config", "user.name", "Homeboy Test"]);
        fs::write(source.join("README.md"), "initial\n").expect("initial file");
        fs::write(source.join("homeboy.json"), r#"{"id":"lifecycle-fixture"}"#)
            .expect("component manifest");
        run_git(&source, &["add", "."]);
        run_git(&source, &["commit", "-q", "-m", "initial"]);
        run_git(&source, &["branch", "-M", "main"]);
        run_git(&source, &["push", "-q", "-u", "origin", "main"]);
        write_component_registration(home.path(), "lifecycle-fixture", &source);

        let owner = "cook-lifecycle-attempt-1";
        let created = create(WorktreeCreateOptions {
            component_id: "lifecycle-fixture".to_string(),
            branch: "fix/lifecycle".to_string(),
            from: Some("main".to_string()),
            task_url: Some("https://github.com/Extra-Chill/homeboy/issues/13971".to_string()),
            run_id: Some(owner.to_string()),
            cleanup_policy: Some(CleanupPolicy::RemoveWhenSafe),
            require_handoff_freshness: false,
        })
        .expect("owned worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        fs::write(path.join("fix.txt"), "fixed\n").expect("task change");
        run_git(&path, &["add", "."]);
        run_git(&path, &["commit", "-q", "-m", "fix lifecycle"]);
        {
            let _cwd = CurrentDirGuard::set(&path);
            run_git(&path, &["push", "-q", "-u", "origin", "fix/lifecycle"]);

            let active_status = status(&created.record.id).expect("active owned status");
            assert!(
                active_status
                    .safety
                    .reasons
                    .iter()
                    .any(|reason| reason.contains("live current working directory")),
                "active lookup must report why the caller workspace cannot be removed"
            );
            let before_pr = cleanup(WorktreeCleanupOptions {
                force: true,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            })
            .expect("cleanup before PR creation");
            assert_eq!(before_pr.counts.removed, 0);
            assert!(before_pr.skipped[0]
                .reasons
                .iter()
                .any(|reason| reason.contains("explicit succeeded finalization")));
            assert!(path.exists(), "a clean push is not terminal owner evidence");

            let bin = home.path().join("bin");
            fs::create_dir(&bin).expect("fake gh bin");
            let gh_log = home.path().join("gh-cwd.log");
            let fake_gh = bin.join("gh");
            fs::write(
                &fake_gh,
                r#"#!/bin/sh
if [ "$1 $2" = "pr create" ]; then
  test -d "$PWD" || exit 2
  pwd > "$HOMEBOY_FAKE_GH_CWD_LOG"
  printf '%s\n' 'https://github.com/example/lifecycle-fixture/pull/13971'
fi
"#,
            )
            .expect("write fake gh");
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&fake_gh, fs::Permissions::from_mode(0o755))
                    .expect("make fake gh executable");
            }
            let mut command_path = std::ffi::OsString::from(bin.as_os_str());
            command_path.push(":");
            command_path.push(std::env::var_os("PATH").unwrap_or_default());
            let _path_env = crate::test_support::EnvVarGuard::set("PATH", command_path);
            let _gh_log_env =
                crate::test_support::EnvVarGuard::set("HOMEBOY_FAKE_GH_CWD_LOG", &gh_log);
            fs::write(
                home.path()
                    .join(".config/homeboy/components/lifecycle-fixture.json"),
                serde_json::json!({
                    "local_path": source,
                    "remote_path": "wp-content/plugins/lifecycle-fixture",
                    "remote_url": "https://github.com/example/lifecycle-fixture.git"
                })
                .to_string(),
            )
            .expect("GitHub component registration");

            let pr = crate::git::pr_create(
                Some("lifecycle-fixture"),
                crate::git::PrCreateOptions {
                    base: "main".to_string(),
                    head: "fix/lifecycle".to_string(),
                    title: "Fix lifecycle".to_string(),
                    body: "Lifecycle fixture".to_string(),
                    draft: false,
                    path: Some(path.display().to_string()),
                },
            )
            .expect("create PR through fake gh");
            assert_eq!(pr.number, Some(13971));
            assert_eq!(
                PathBuf::from(fs::read_to_string(&gh_log).expect("fake gh cwd").trim()),
                path.canonicalize().expect("canonical worktree")
            );
            assert!(
                path.exists(),
                "PR creation must retain its caller workspace"
            );

            finalize_provider_lifecycle(
                &created.record.id,
                owner,
                crate::worktree_provider::WorktreeTerminalDisposition::Succeeded,
            )
            .expect("explicit terminal owner finalization");
            let finalized_status = status(&created.record.id).expect("finalized status");
            assert_eq!(finalized_status.record.run_id.as_deref(), Some(owner));
            assert_eq!(
                finalized_status.record.terminal_disposition.as_deref(),
                Some("succeeded")
            );
            assert_eq!(finalized_status.record.lifecycle_revision, 1);

            let direct_remove = remove(WorktreeRemoveOptions {
                id: created.record.id.clone(),
                force: true,
                cleanup_branch: false,
                allow_unmerged_branch: false,
            })
            .expect_err("forced direct removal cannot remove the caller cwd");
            assert!(direct_remove
                .message
                .contains("live current working directory"));
            let live_cwd = cleanup(WorktreeCleanupOptions {
                force: true,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            })
            .expect("cleanup while caller cwd is live");
            assert_eq!(live_cwd.counts.removed, 0);
            assert!(live_cwd.skipped[0]
                .reasons
                .iter()
                .any(|reason| reason.contains("live current working directory")));
            assert!(path.exists());
        }

        let after_cwd_exit = cleanup(WorktreeCleanupOptions {
            force: false,
            dry_run: false,
            cleanup_branches: false,
            allow_unmerged_branches: false,
        })
        .expect("cleanup after terminal finalization and cwd exit");
        assert_eq!(after_cwd_exit.counts.removed, 1);
        assert!(!path.exists());
    });
}

#[test]
fn create_restores_missing_active_record_from_an_unclaimed_existing_branch() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "restore-fixture");
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        run_git(&source, &["worktree", "remove", &path.to_string_lossy()]);

        let restored = create(options).expect("restore task worktree");

        let reconciliation = restored.reconciliation.expect("restore evidence");
        assert_eq!(reconciliation.action, WorktreeCreateAction::Restored);
        assert_eq!(reconciliation.previous.git_registration, "missing");
        assert_eq!(reconciliation.current.git_registration, "registered");
        assert!(path.exists());
        assert_eq!(
            git::run_git(&path, &["branch", "--show-current"], "git branch")
                .unwrap()
                .trim(),
            "fix/restore"
        );
    });
}

#[test]
fn create_returns_existing_matching_task_worktree_idempotently() {
    crate::test_support::with_isolated_home(|home| {
        let (_, options) = registered_create_fixture(home.path(), "existing-fixture");
        let created = create(options.clone()).expect("create task worktree");
        let existing = create(options).expect("reuse task worktree");

        assert_eq!(existing.record, created.record);
        assert!(created.reconciliation.is_none());
        assert!(existing.reconciliation.is_none());
    });
}

fn add_bare_origin(home: &Path, source: &Path) -> PathBuf {
    let remote = home.join("remote.git");
    fs::create_dir_all(&remote).expect("remote directory");
    run_git(&remote, &["init", "--bare", "-q"]);
    run_git(source, &["branch", "-M", "main"]);
    run_git(
        source,
        &["remote", "add", "origin", &remote.to_string_lossy()],
    );
    run_git(source, &["push", "-q", "-u", "origin", "main"]);
    run_git(&remote, &["symbolic-ref", "HEAD", "refs/heads/main"]);
    remote
}

#[test]
fn create_can_issue_remote_verified_handoff_freshness() {
    crate::test_support::with_isolated_home(|home| {
        let (source, mut options) = registered_create_fixture(home.path(), "fresh-fixture");
        add_bare_origin(home.path(), &source);
        options.from = Some("origin/main".to_string());
        options.require_handoff_freshness = true;

        let created = create(options).expect("create with freshness proof");
        let freshness = created.handoff_freshness.expect("freshness evidence");

        assert_eq!(freshness.status, "verified");
        assert_eq!(freshness.proof.handle, created.record.id);
        assert_eq!(freshness.proof.resolved_base_ref, "origin/main");
        assert_eq!(
            freshness.proof.remote_default_sha,
            freshness.proof.remote_default_advertised_sha
        );
        assert_eq!(
            freshness.proof.worktree_sha,
            freshness.proof.resolved_base_sha
        );
    });
}

#[test]
fn freshness_required_create_fetches_an_advanced_remote_base() {
    crate::test_support::with_isolated_home(|home| {
        let (source, mut options) = registered_create_fixture(home.path(), "advanced-fixture");
        let remote = add_bare_origin(home.path(), &source);
        let updater = home.path().join("updater");
        run_git(
            home.path(),
            &[
                "clone",
                "-q",
                &remote.to_string_lossy(),
                &updater.to_string_lossy(),
            ],
        );
        run_git(&updater, &["config", "user.email", "homeboy@example.com"]);
        run_git(&updater, &["config", "user.name", "Homeboy Test"]);
        fs::write(updater.join("advanced.txt"), "advanced\n").expect("advanced file");
        run_git(&updater, &["add", "."]);
        run_git(&updater, &["commit", "-q", "-m", "advance remote"]);
        run_git(&updater, &["push", "-q", "origin", "main"]);
        let advanced_sha = git::run_git(&updater, &["rev-parse", "HEAD"], "advanced sha")
            .unwrap()
            .trim()
            .to_string();

        options.from = Some("origin/main".to_string());
        options.require_handoff_freshness = true;
        let created = create(options).expect("create from refreshed base");
        let proof = created.handoff_freshness.expect("freshness").proof;

        assert_eq!(proof.resolved_base_sha, advanced_sha);
        assert_eq!(proof.worktree_sha, advanced_sha);
    });
}

#[test]
fn freshness_failure_refuses_before_worktree_allocation() {
    crate::test_support::with_isolated_home(|home| {
        let (source, mut options) = registered_create_fixture(home.path(), "failed-fixture");
        let missing = home.path().join("missing-remote.git");
        run_git(
            &source,
            &["remote", "add", "origin", &missing.to_string_lossy()],
        );
        options.from = Some("origin/main".to_string());
        options.require_handoff_freshness = true;
        let branch = options.branch.clone();
        let expected_path = source.parent().unwrap().join("failed-fixture@fix-restore");

        create(options).expect_err("unverifiable remote must fail closed");

        assert!(!expected_path.exists());
        assert!(git::run_git(
            &source,
            &[
                "show-ref",
                "--verify",
                "--quiet",
                &format!("refs/heads/{branch}")
            ],
            "branch absence"
        )
        .is_err());
    });
}

/// #13647: worktree provisioning inherited whatever ambient global Git
/// identity happened to be configured in the surrounding environment,
/// instead of the identity the target remote's host actually requires. A
/// commit made in a freshly created task worktree therefore carried the
/// wrong author. Creation must pin the repository-local identity declared
/// for the remote's host up front, so agent-authored commits carry the
/// right author without anyone remembering to check.
#[test]
fn create_pins_worktree_identity_to_the_target_remotes_host_policy() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "identity-fixture");
        run_git(
            &source,
            &[
                "remote",
                "add",
                "origin",
                "git@github-extrachill.example.test:extra-chill/homeboy.git",
            ],
        );

        let mut config = crate::defaults::HomeboyConfig::default();
        config.git_hosts.insert(
            "github-extrachill.example.test".to_string(),
            crate::defaults::GitHostConfig {
                name: "Extra Chill Author".to_string(),
                email: "author@extrachill.example.test".to_string(),
            },
        );
        crate::defaults::save_config(&config).expect("save git host identity policy");

        // Simulate the ambient identity a shared provisioning environment can
        // inherit from an unrelated organization's global gitconfig (#13647).
        run_git(
            home.path(),
            &["config", "--global", "user.name", "Chris Huber"],
        );
        run_git(
            home.path(),
            &[
                "config",
                "--global",
                "user.email",
                "chris.huber@automattic.com",
            ],
        );

        let created = create(options).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);

        assert_eq!(
            git::run_git(&path, &["config", "user.name"], "git config user.name")
                .unwrap()
                .trim(),
            "Extra Chill Author"
        );
        assert_eq!(
            git::run_git(&path, &["config", "user.email"], "git config user.email")
                .unwrap()
                .trim(),
            "author@extrachill.example.test"
        );

        // The reported defect is the *committed* author, not just config
        // plumbing: make a commit in the new worktree and check it end to end.
        fs::write(path.join("task.txt"), "task\n").unwrap();
        run_git(&path, &["add", "."]);
        run_git(&path, &["commit", "-q", "-m", "task work"]);
        let author = git::run_git(
            &path,
            &["log", "-1", "--format=%an <%ae>"],
            "git log author",
        )
        .unwrap();
        assert_eq!(
            author.trim(),
            "Extra Chill Author <author@extrachill.example.test>"
        );
    });
}

#[test]
fn create_reuses_existing_worktree_with_a_relative_gitdir_pointer() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "relative-existing-fixture");
        run_git(&source, &["config", "worktree.useRelativePaths", "true"]);
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        let pointer = fs::read_to_string(path.join(".git")).expect("worktree pointer");

        let existing = create(options).expect("reuse relative-pointer worktree");

        assert!(pointer
            .trim_start_matches("gitdir:")
            .trim()
            .starts_with(".."));
        assert_eq!(existing.record, created.record);
        assert!(existing.reconciliation.is_none());
    });
}

#[test]
fn create_restores_worktree_with_relative_gitdir_pointers() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "relative-restore-fixture");
        run_git(&source, &["config", "worktree.useRelativePaths", "true"]);
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        run_git(&source, &["worktree", "remove", &path.to_string_lossy()]);

        let restored = create(options).expect("restore relative-pointer worktree");
        let pointer = fs::read_to_string(path.join(".git")).expect("restored worktree pointer");

        assert!(pointer
            .trim_start_matches("gitdir:")
            .trim()
            .starts_with(".."));
        assert_eq!(
            restored.reconciliation.expect("restore evidence").action,
            WorktreeCreateAction::Restored
        );
    });
}

#[test]
fn create_refuses_a_replaced_path_without_mutating_it() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "replaced-fixture");
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(created.record.worktree_path);
        run_git(&source, &["worktree", "remove", &path.to_string_lossy()]);
        fs::create_dir(&path).expect("replacement directory");
        fs::write(path.join("sentinel"), "replacement\n").expect("replacement sentinel");

        let error = create(options).expect_err("replacement is not a linked worktree");

        assert!(error.message.contains("not registered"));
        assert_eq!(
            fs::read_to_string(path.join("sentinel")).unwrap(),
            "replacement\n"
        );
    });
}

#[test]
fn create_refuses_branch_claimed_by_another_worktree_path() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "conflict-fixture");
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(created.record.worktree_path);
        run_git(&source, &["worktree", "remove", &path.to_string_lossy()]);
        let other = source.with_file_name("another-owner");
        run_git(
            &source,
            &["worktree", "add", &other.to_string_lossy(), "fix/restore"],
        );

        let error = create(options).expect_err("branch ownership conflict");

        assert!(error.message.contains("already claimed"));
        assert!(error
            .details
            .get("tried")
            .and_then(serde_json::Value::as_array)
            .expect("bounded ownership evidence")
            .iter()
            .any(|item| item == &format!("owner_path={}", other.display())));
    });
}

#[test]
fn create_prunes_only_a_proven_prunable_registration_before_restoring() {
    crate::test_support::with_isolated_home(|home| {
        let (_, options) = registered_create_fixture(home.path(), "prunable-fixture");
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        fs::remove_dir_all(&path).expect("remove path but retain git registration");

        let restored = create(options).expect("prune and restore task worktree");

        let reconciliation = restored.reconciliation.expect("restore evidence");
        assert_eq!(reconciliation.action, WorktreeCreateAction::Restored);
        assert_eq!(reconciliation.previous.git_registration, "missing");
        assert!(path.exists());
    });
}

#[test]
fn create_exact_stale_relative_cleanup_preserves_foreign_registration() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "exact-cleanup-fixture");
        run_git(&source, &["config", "worktree.useRelativePaths", "true"]);
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        let foreign = source.with_file_name("foreign-prunable-worktree");
        run_git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "foreign/prunable",
                &foreign.to_string_lossy(),
            ],
        );
        fs::remove_dir_all(&path).expect("remove target path");
        fs::remove_dir_all(&foreign).expect("remove foreign path");

        let restored = create(options).expect("exact cleanup and restore");
        let porcelain = git::run_git(
            &source,
            &["worktree", "list", "--porcelain"],
            "git worktree list",
        )
        .expect("worktree registrations");

        assert_eq!(
            restored.reconciliation.expect("restore evidence").action,
            WorktreeCreateAction::Restored
        );
        assert!(porcelain.contains(&format!("worktree {}", foreign.display())));
        assert!(porcelain.contains("prunable"));
    });
}

#[test]
fn create_restoration_preserves_dirty_source_and_unpushed_branch_commit() {
    crate::test_support::with_isolated_home(|home| {
        let (source, options) = registered_create_fixture(home.path(), "preserve-fixture");
        let created = create(options.clone()).expect("create task worktree");
        let path = PathBuf::from(&created.record.worktree_path);
        fs::write(path.join("candidate.txt"), "candidate\n").expect("candidate change");
        run_git(&path, &["add", "."]);
        run_git(&path, &["commit", "-q", "-m", "candidate"]);
        fs::write(source.join("source-dirty.txt"), "dirty\n").expect("dirty source");
        run_git(
            &source,
            &["worktree", "remove", "--force", &path.to_string_lossy()],
        );

        let restored = create(options).expect("restore without discarding local state");

        assert_eq!(
            restored.reconciliation.expect("restore evidence").action,
            WorktreeCreateAction::Restored
        );
        assert!(source.join("source-dirty.txt").exists());
        assert_eq!(
            fs::read_to_string(path.join("candidate.txt")).unwrap(),
            "candidate\n"
        );
    });
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
    let record = succeeded_record(source.path(), &worktree);
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
fn cleanup_and_remove_refuse_durable_live_workspace_owners_even_with_force() {
    let data_root = tempfile::tempdir().unwrap();
    let source = git_repo();
    let worktree = sibling_worktree_path(source.path(), "durably-owned-cleanup");
    run_git(
        source.path(),
        &["worktree", "add", "-b", "task", &worktree.to_string_lossy()],
    );
    let store = data_root.path().join("task-worktrees");
    let mut record = fixture_record(source.path(), &worktree);
    record.run_id = Some("completed-owner".to_string());
    record.terminal_disposition = Some("succeeded".to_string());
    record.lifecycle_revision = 1;
    write_record(&store, &record).unwrap();
    let claims = crate::workspace_claim::WorkspaceClaimStore::new(
        data_root
            .path()
            .join(crate::workspace_claim::LOCAL_WORKSPACE_CLAIMS_DIR),
    );
    let now = now_ms();
    let owner = claims
        .register_owner(
            record.effective_workspace_identity().unwrap(),
            "live-agent",
            60_000,
            now,
        )
        .unwrap();

    for force in [false, true] {
        let output = cleanup_with_store(
            WorktreeCleanupOptions {
                force,
                dry_run: false,
                cleanup_branches: false,
                allow_unmerged_branches: false,
            },
            &store,
        )
        .unwrap();
        assert_eq!(output.counts.removed, 0);
        assert!(output.skipped[0]
            .reasons
            .iter()
            .any(|reason| reason.contains("durable live owner")));
        assert!(worktree.exists());
    }
    let error = remove_with_store(
        WorktreeRemoveOptions {
            id: record.id.clone(),
            force: true,
            cleanup_branch: false,
            allow_unmerged_branch: false,
        },
        &store,
    )
    .expect_err("direct removal must honor durable ownership");
    assert!(error.message.contains("durable live owner"));

    claims.release_owner(&owner, now + 1).unwrap();
    let removed = remove_with_store(
        WorktreeRemoveOptions {
            id: record.id,
            force: false,
            cleanup_branch: false,
            allow_unmerged_branch: false,
        },
        &store,
    )
    .unwrap();
    assert!(removed.removed);
    assert_eq!(
        removed.record.terminal_disposition.as_deref(),
        Some("succeeded")
    );
    assert_eq!(removed.record.lifecycle_revision, 2);
    assert!(!worktree.exists());
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
    let record = succeeded_record(source.path(), &worktree);
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
    let mut locked_record = succeeded_record(source.path(), &locked_worktree);
    locked_record.id = "fixture@locked".to_string();
    locked_record.branch = "locked-task".to_string();
    let removable_worktree = sibling_worktree_path(source.path(), "cleanup-continues");
    let mut removable_record = succeeded_record(source.path(), &removable_worktree);
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
    let removable_record = succeeded_record(source.path(), &removable);
    let mut missing_record = succeeded_record(
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
    let record = succeeded_record(source.path(), &worktree);
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
fn list_retains_valid_records_and_diagnoses_malformed_manifests() {
    let dir = tempfile::tempdir().unwrap();
    let source = git_repo();
    let store = dir.path().join("store");
    let record = fixture_record(source.path(), &dir.path().join("fixture@task"));
    write_record(&store, &record).unwrap();
    fs::write(store.join("malformed.json"), "not json\n").unwrap();

    let output = list_with_store(&store).unwrap();

    assert_eq!(output.worktrees, vec![record]);
    assert_eq!(output.diagnostics.len(), 1);
    let diagnostic = &output.diagnostics[0];
    assert_eq!(diagnostic.code, "internal.json_error");
    assert_eq!(
        diagnostic.record_path.as_deref(),
        Some(store.join("malformed.json").to_str().unwrap())
    );
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
    let record = succeeded_record(source.path(), &worktree);
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
    let record = succeeded_record(source.path(), &worktree);
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
                "#!/bin/sh\nif [ \"$1\" = resolve ]; then\n  if [ -d '{}' ]; then printf '%s\\n' '{{\"worktrees\":[{{\"handle\":\"homeboy@fix-12124\",\"path\":\"{}\",\"branch\":\"fix/12124\",\"task_url\":\"https://github.com/Extra-Chill/homeboy/issues/12124\",\"safety\":{{\"dirty\":false,\"unpushed\":false,\"primary\":false}}}}]}}'; else printf '%s\\n' '{{\"worktrees\":[]}}'; fi\nelif [ \"$1\" = ensure ]; then\n  printf 'ensure|%s|%s|%s|%s|%s|%s|%s|%s\\n' \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\" \"$8\" \"$9\" >> '{}'\n  if [ ! -d '{}' ]; then git init -q -b fix/12124 '{}'; fi\nelse\n  printf 'finalize|%s|%s|%s|%s|%s|%s\\n' \"$2\" \"$3\" \"$4\" \"$5\" \"$6\" \"$7\" >> '{}'\nfi\n",
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
                    task_url: Some("$.task_url".to_string()),
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
