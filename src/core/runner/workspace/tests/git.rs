use std::fs;
use std::path::Path;
use std::process::Command;

use super::git;
use crate::core::runner::workspace::git::{
    git_bundle_install_command, git_snapshot, materialize_git_command,
};
use crate::core::runner::workspace::sync::sync_workspace;
use crate::core::runner::workspace::types::{RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};
use crate::core::runner::workspace::util::git_output;

#[test]
fn controller_routed_git_sync_materializes_private_unavailable_remote_with_changed_since_base() {
    crate::test_support::with_isolated_home(|_| {
        let origin = tempfile::tempdir().expect("origin tempdir");
        let author = tempfile::tempdir().expect("author tempdir");
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        git(origin.path(), &["init", "--bare"]);
        git(origin.path(), &["config", "uploadpack.allowFilter", "true"]);
        git(author.path(), &["init", "-b", "main"]);
        git(author.path(), &["config", "user.email", "test@example.com"]);
        git(author.path(), &["config", "user.name", "Test User"]);
        fs::write(author.path().join("base-only.txt"), "base\n").expect("write base file");
        git(author.path(), &["add", "."]);
        git(author.path(), &["commit", "-m", "base"]);
        let base = git_output(author.path(), &["rev-parse", "HEAD"]).expect("base revision");
        let base_blob = git_output(author.path(), &["rev-parse", "HEAD:base-only.txt"])
            .expect("base blob revision");
        fs::remove_file(author.path().join("base-only.txt")).expect("remove base file");
        fs::write(author.path().join("file.txt"), "head\n").expect("write head file");
        git(author.path(), &["add", "."]);
        git(author.path(), &["commit", "-m", "head"]);
        let head = git_output(author.path(), &["rev-parse", "HEAD"]).expect("head revision");
        git(author.path(), &["checkout", "-b", "unrelated", &base]);
        fs::write(author.path().join("unrelated.txt"), "unrelated\n")
            .expect("write unrelated file");
        git(author.path(), &["add", "."]);
        git(author.path(), &["commit", "-m", "unrelated"]);
        let unrelated =
            git_output(author.path(), &["rev-parse", "HEAD"]).expect("unrelated revision");
        git(author.path(), &["checkout", &head]);
        git(
            author.path(),
            &[
                "remote",
                "add",
                "origin",
                &format!("file://{}", origin.path().display()),
            ],
        );
        git(author.path(), &["push", "origin", "main", "unrelated"]);
        git(
            source.path(),
            &[
                "clone",
                "--filter=blob:none",
                &format!("file://{}", origin.path().display()),
                ".",
            ],
        );
        assert!(
            git_output(
                source.path(),
                &["rev-list", "--objects", "--missing=print", &base]
            )
            .expect("list promised objects")
            .contains(&format!("?{base_blob}")),
            "the partial source fixture must omit the base-only blob"
        );
        git(
            source.path(),
            &[
                "fetch",
                "origin",
                "refs/heads/unrelated:refs/remotes/origin/unrelated",
            ],
        );
        git(source.path(), &["remote", "rename", "origin", "controller"]);
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.example.invalid/example-org/private-source.git",
            ],
        );
        git(source.path(), &["commit-graph", "write", "--reachable"]);
        assert!(
            source
                .path()
                .join(".git/objects/info/commit-graph")
                .exists(),
            "the fixture must retain a commit graph after its objects are removed"
        );
        remove_local_packs(source.path());
        assert!(
            git_without_lazy_fetch(
                source.path(),
                &["cat-file", "-e", &format!("{base}^{{commit}}")]
            )
            .is_err(),
            "the fixture must leave a commit-graph entry whose object is absent locally"
        );
        assert!(
            git_without_lazy_fetch(
                source.path(),
                &["cat-file", "-e", &format!("{unrelated}^{{commit}}")],
            )
            .is_err(),
            "the fixture must leave unrelated promisor objects absent locally"
        );
        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-git-bundle","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let sync_result = sync_workspace(
            "lab-local-git-bundle",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Git,
                controller_routed_git: true,
                changed_since_base: Some(base.clone()),
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        );

        let (output, exit_code) = sync_result.expect("sync workspace");

        assert_eq!(exit_code, 0);
        assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::Git);
        assert_eq!(output.current_workspace.local_path, output.local_path);
        assert_eq!(output.current_workspace.remote_path, output.remote_path);
        assert_eq!(
            output.current_workspace.sync_mode,
            RunnerWorkspaceSyncMode::Git
        );
        assert!(output.current_workspace.materialized);
        assert_eq!(
            output.current_workspace.source_commit,
            Some(output.snapshot_identity.clone())
        );
        assert_eq!(output.current_workspace.source_dirty, Some(false));
        let remote = Path::new(&output.remote_path);
        assert_eq!(
            git_output(remote, &["rev-parse", "--is-inside-work-tree"]).unwrap(),
            "true"
        );
        // The controller-bundle path repoints origin without fetching it. The
        // source URL is deliberately unavailable from the runner, so this
        // proves materialization did not fall back to a network clone.
        assert_eq!(
            git_output(remote, &["config", "--get", "remote.origin.url"]).unwrap(),
            "https://github.example.invalid/example-org/private-source.git"
        );
        assert_eq!(
            fs::read_to_string(remote.join("file.txt")).expect("read synced file"),
            "head\n"
        );
        assert_eq!(git_output(remote, &["rev-parse", &base]).unwrap(), base);
        assert_eq!(
            git_output(remote, &["cat-file", "-e", &base_blob]).unwrap(),
            ""
        );
        assert_eq!(
            git_output(remote, &["merge-base", &base, "HEAD"]).unwrap(),
            base
        );
        assert!(
            !git_output(
                source.path(),
                &["rev-list", "--objects", "--missing=print", &base]
            )
            .expect("list hydrated objects")
            .contains(&format!("?{base_blob}")),
            "the controller must hydrate the promised object before bundling"
        );
        assert!(
            git_without_lazy_fetch(
                source.path(),
                &["cat-file", "-e", &format!("{unrelated}^{{commit}}")],
            )
            .is_err(),
            "the controller must not hydrate unrelated refs"
        );
        assert!(
            git_without_lazy_fetch(
                remote,
                &["cat-file", "-e", &format!("{unrelated}^{{commit}}")]
            )
            .is_err(),
            "the runner bundle must not include unrelated refs"
        );
    });
}

fn remove_local_packs(path: &Path) {
    let pack_dir = path.join(".git/objects/pack");
    for entry in fs::read_dir(pack_dir).expect("read object pack directory") {
        let entry = entry.expect("read object pack entry");
        fs::remove_file(entry.path()).expect("remove local object pack entry");
    }
}

fn git_without_lazy_fetch(path: &Path, args: &[&str]) -> std::result::Result<(), String> {
    let output = Command::new("git")
        .args(args)
        .env("GIT_NO_LAZY_FETCH", "1")
        .current_dir(path)
        .output()
        .expect("run git without lazy fetch");
    output
        .status
        .success()
        .then_some(())
        .ok_or_else(|| String::from_utf8_lossy(&output.stderr).to_string())
}

#[test]
fn git_materialization_ignores_generated_homeboy_output_but_refuses_source_dirty_state() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        git(source.path(), &["init"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test User"]);
        fs::write(source.path().join("file.txt"), "base\n").expect("write file");
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "base"]);
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                &source.path().display().to_string(),
            ],
        );

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-generated-homeboy","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let sync = || {
            sync_workspace(
                "lab-local-generated-homeboy",
                RunnerWorkspaceSyncOptions {
                    path: source.path().display().to_string(),
                    mode: RunnerWorkspaceSyncMode::Git,
                    controller_routed_git: false,
                    changed_since_base: None,
                    git_fetch_refs: Vec::new(),
                    snapshot_includes: Vec::new(),
                    allow_dirty_lab_workspace: false,
                    run_isolation_token: None,
                },
            )
        };

        let (output, exit_code) = sync().expect("initial sync workspace");
        assert_eq!(exit_code, 0);
        let remote = Path::new(&output.remote_path);
        fs::create_dir_all(remote.join(".homeboy/experiments/stripe-ece"))
            .expect("create generated output dir");
        fs::write(
            remote.join(".homeboy/experiments/stripe-ece/compare.json"),
            "{}\n",
        )
        .expect("write generated output");

        let (output, exit_code) = sync().expect("generated .homeboy output is ignored");
        assert_eq!(exit_code, 0);
        let remote = Path::new(&output.remote_path);
        // Re-sync resets generated `.homeboy` *output* (experiments, scratch)
        // via `git clean -ffdqx`, so the experiment artifact written above is
        // gone. The sync then legitimately re-writes its own workspace metadata
        // marker under `.homeboy/runner-workspace.json` (used by orphaned-lab-
        // workspace pruning, #6678/376bfde56), so `.homeboy` itself persists
        // with only that metadata.
        assert!(!remote.join(".homeboy/experiments").exists());
        assert!(remote.join(".homeboy/runner-workspace.json").is_file());

        fs::write(remote.join("dirty-source.txt"), "dirty\n").expect("write dirty source file");
        let err = sync().expect_err("real dirty runner workspace is refused");
        assert!(err.message.contains("Homeboy Lab refused"));
        assert!(err.message.contains("dirty-source.txt"));
    });
}

#[test]
fn controller_routed_git_sync_materializes_bundle_for_public_remote() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        git(source.path(), &["init"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test User"]);
        git(source.path(), &["checkout", "-b", "fix/source-upgrade"]);
        fs::write(source.path().join("file.txt"), "source-upgrade\n").expect("write file");
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "source upgrade"]);
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/Extra-Chill/homeboy.git",
            ],
        );

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-controller-git","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = sync_workspace(
            "lab-local-controller-git",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Git,
                controller_routed_git: true,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect("sync workspace");

        assert_eq!(exit_code, 0);
        assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::Git);
        assert_eq!(output.current_workspace.local_path, output.local_path);
        assert_eq!(output.current_workspace.remote_path, output.remote_path);
        assert_eq!(
            output.current_workspace.sync_mode,
            RunnerWorkspaceSyncMode::Git
        );
        assert!(output.current_workspace.materialized);
        assert_eq!(
            output.current_workspace.source_commit,
            Some(output.snapshot_identity.clone())
        );
        assert_eq!(
            output.current_workspace.source_ref.as_deref(),
            Some("fix/source-upgrade")
        );
        assert_eq!(output.current_workspace.source_dirty, Some(false));
        let remote = Path::new(&output.remote_path);
        assert_eq!(
            git_output(remote, &["rev-parse", "--is-inside-work-tree"]).unwrap(),
            "true"
        );
        assert_eq!(
            git_output(remote, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "fix/source-upgrade"
        );
        assert_eq!(
            git_output(remote, &["config", "--get", "remote.origin.url"]).unwrap(),
            "https://github.com/Extra-Chill/homeboy.git"
        );
        assert_eq!(
            fs::read_to_string(remote.join("file.txt")).expect("read synced file"),
            "source-upgrade\n"
        );
    });
}

#[test]
fn controller_routed_git_sync_rejects_shallow_source_checkout() {
    crate::test_support::with_isolated_home(|_| {
        let origin = tempfile::tempdir().expect("origin tempdir");
        let clone_parent = tempfile::tempdir().expect("clone parent tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");

        git(origin.path(), &["init"]);
        git(origin.path(), &["config", "user.email", "test@example.com"]);
        git(origin.path(), &["config", "user.name", "Test User"]);
        fs::write(origin.path().join("file.txt"), "base\n").expect("write base file");
        git(origin.path(), &["add", "."]);
        git(origin.path(), &["commit", "-m", "base"]);
        fs::write(origin.path().join("file.txt"), "tip\n").expect("write tip file");
        git(origin.path(), &["commit", "-am", "tip"]);

        let source = clone_parent.path().join("source");
        let remote_url = format!("file://{}", origin.path().display());
        let clone_output = Command::new("git")
            .arg("clone")
            .arg("--depth")
            .arg("1")
            .arg(&remote_url)
            .arg(&source)
            .output()
            .expect("run shallow clone");
        assert!(
            clone_output.status.success(),
            "git clone failed: {}",
            String::from_utf8_lossy(&clone_output.stderr)
        );
        assert_eq!(
            git_output(&source, &["rev-parse", "--is-shallow-repository"]).unwrap(),
            "true"
        );

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-shallow-git","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let err = sync_workspace(
            "lab-local-shallow-git",
            RunnerWorkspaceSyncOptions {
                path: source.display().to_string(),
                mode: RunnerWorkspaceSyncMode::Git,
                controller_routed_git: true,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect_err("shallow source checkout should fail before bundle creation");

        assert!(err.message.contains("source checkout is shallow"));
        assert!(!err.message.contains("missing"));
        assert!(!err.message.contains("object"));
        let hint_text = err.details["tried"]
            .as_array()
            .expect("shallow checkout error includes recovery options")
            .iter()
            .filter_map(|value| value.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        assert!(hint_text.contains("fetch --unshallow"));
        assert!(hint_text.contains("--method source"));
    });
}

#[test]
fn git_sync_of_detached_extension_source_preserves_source_revision() {
    crate::test_support::with_isolated_home(|_| {
        let source = tempfile::tempdir().expect("source tempdir");
        let runner_root = tempfile::tempdir().expect("runner root tempdir");
        git(source.path(), &["init"]);
        git(source.path(), &["config", "user.email", "test@example.com"]);
        git(source.path(), &["config", "user.name", "Test User"]);
        fs::create_dir_all(source.path().join("wordpress")).expect("extension dir");
        fs::write(
            source.path().join("wordpress/wordpress.json"),
            r#"{"name":"WordPress","version":"1.0.0"}"#,
        )
        .expect("write extension manifest");
        git(source.path(), &["add", "."]);
        git(source.path(), &["commit", "-m", "base"]);
        git(source.path(), &["checkout", "--detach", "HEAD"]);
        fs::write(
            source.path().join("wordpress/wordpress.json"),
            r#"{"name":"WordPress","version":"2.0.0"}"#,
        )
        .expect("write detached extension manifest");
        git(source.path(), &["add", "."]);
        git(
            source.path(),
            &["commit", "-m", "detached extension update"],
        );
        git(
            source.path(),
            &[
                "remote",
                "add",
                "origin",
                "https://github.com/Extra-Chill/homeboy-extensions.git",
            ],
        );
        let detached_head = git_output(source.path(), &["rev-parse", "HEAD"]).unwrap();
        let detached_short = git_output(source.path(), &["rev-parse", "--short", "HEAD"]).unwrap();

        crate::core::runner::create(
            &format!(
                r#"{{"id":"lab-local-detached-extension","kind":"local","workspace_root":"{}"}}"#,
                runner_root.path().display()
            ),
            false,
        )
        .expect("create runner");

        let (output, exit_code) = sync_workspace(
            "lab-local-detached-extension",
            RunnerWorkspaceSyncOptions {
                path: source.path().display().to_string(),
                mode: RunnerWorkspaceSyncMode::Git,
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        )
        .expect("sync workspace");

        assert_eq!(exit_code, 0);
        assert_eq!(output.sync_mode, RunnerWorkspaceSyncMode::Git);
        assert_eq!(output.snapshot_identity, detached_head);
        let remote = Path::new(&output.remote_path);
        assert_eq!(
            git_output(remote, &["rev-parse", "HEAD"]).unwrap(),
            detached_head
        );
        assert_eq!(
            git_output(remote, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap(),
            "HEAD"
        );

        let install = crate::core::extension::install(
            &remote.join("wordpress").display().to_string(),
            Some("wordpress"),
        )
        .expect("install extension from synced detached workspace");

        assert_eq!(
            install.source_revision.as_deref(),
            Some(detached_short.as_str())
        );
        assert_eq!(
            crate::core::extension::read_source_revision("wordpress").as_deref(),
            Some(detached_short.as_str())
        );
    });
}

#[test]
fn git_materialization_fetches_changed_since_base_before_checkout() {
    let command = materialize_git_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "https://github.com/Extra-Chill/homeboy.git",
        "abc123",
        Some("def456"),
        &[],
        false,
    );

    assert!(command.contains("fetch --prune origin '+refs/heads/*:refs/remotes/origin/*'"));
    assert!(command.contains("rev-parse --verify -q 'def456^{commit}'"));
    assert!(command.contains("fetch origin def456"));
    assert!(command.contains("checkout --detach abc123"));
    assert!(command.contains("reset --hard"));
    assert!(command.contains("reset --hard abc123"));
}

#[test]
fn git_bundle_materialization_disables_lazy_fetches() {
    let command = git_bundle_install_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "abc123",
        None,
        "https://github.example.invalid/example-org/private-source.git",
        false,
    );

    assert!(command.contains("export GIT_NO_LAZY_FETCH=1"));
}

#[test]
fn git_materialization_restores_workspace_owner_after_root_run() {
    let command = materialize_git_command(
        "/var/lib/sampleplugin/workspace/_lab_workspaces/homeboy-abc",
        "https://github.com/Extra-Chill/homeboy.git",
        "abc123",
        None,
        &[],
        false,
    );

    assert!(command.contains("owner_path=$parent"));
    assert!(command.contains("stat -c '%u:%g'"));
    assert!(command.contains("stat -f '%u:%g'"));
    assert!(command.contains("[ \"$(id -u)\" = \"0\" ]"));
    assert!(command.contains("[ \"$owner\" != \"0:0\" ]"));
    assert!(command.contains("chown \"$owner\" $parent"));
    assert!(command.contains("chown -R \"$owner\" $dest"));
}

#[test]
fn git_materialization_fetches_extra_refs_before_checkout() {
    let command = materialize_git_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "https://github.com/Extra-Chill/homeboy.git",
        "abc123",
        None,
        &["refs/pull/5530/head".to_string()],
        false,
    );

    assert!(command.contains("fetch origin refs/pull/5530/head"));
    assert!(command.contains("checkout --detach abc123"));
}

#[test]
fn git_materialization_fetches_extra_refs_before_changed_since_sha() {
    let command = materialize_git_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "https://github.com/Extra-Chill/homeboy.git",
        "abc123",
        Some("def456"),
        &["refs/heads/main".to_string()],
        false,
    );

    let extra_ref_index = command
        .find("fetch origin refs/heads/main")
        .expect("fetches advertised ref");
    let changed_since_index = command
        .find("rev-parse --verify -q 'def456^{commit}'")
        .expect("verifies changed-since commit");

    assert!(extra_ref_index < changed_since_index);
}

#[test]
fn git_materialization_refuses_dirty_remote_workspace_by_default() {
    let command = materialize_git_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "https://github.com/Extra-Chill/homeboy.git",
        "abc123",
        None,
        &[],
        false,
    );

    assert!(command.contains("Homeboy Lab refused to overwrite a dirty runner workspace"));
    assert!(command.contains("exit 97"));
    assert!(command.contains("Pass --allow-dirty-lab-workspace"));
    assert!(command.contains("${path#.homeboy/}"));
    assert!(command.contains("git -C \"$dest\" reset --hard"));
}

#[test]
fn git_materialization_override_is_noisy_but_allows_reset() {
    let command = materialize_git_command(
        "/srv/homeboy/_lab_workspaces/homeboy-abc",
        "https://github.com/Extra-Chill/homeboy.git",
        "abc123",
        None,
        &[],
        true,
    );

    assert!(command.contains("Homeboy Lab warning: --allow-dirty-lab-workspace"));
    assert!(!command.contains("Homeboy Lab refused"));
    assert!(!command.contains("exit 97"));
    assert!(command.contains("git -C \"$dest\" reset --hard"));
}

#[test]
fn dirty_git_sync_without_changed_since_reports_supported_remediation() {
    let source = super::dirty_git_repo();

    let err = match git_snapshot(source.path(), None, Vec::new(), false) {
        Ok(_) => panic!("dirty git sync should fail"),
        Err(err) => err,
    };

    assert!(err.message.contains("requires a clean working tree"));
    assert!(!err.message.contains("use --mode snapshot"));
    let hint_text = err.details["tried"]
        .as_array()
        .expect("dirty git sync error includes recovery options")
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(hint_text.contains("Commit or stash"));
    assert!(hint_text.contains("--placement local"));
    assert!(hint_text.contains("homeboy runner workspace sync <runner-id>"));
}

#[test]
fn dirty_changed_since_git_sync_explains_snapshot_is_unavailable() {
    let source = super::dirty_git_repo();

    let err = match git_snapshot(source.path(), Some("abc123"), Vec::new(), false) {
        Ok(_) => panic!("dirty changed-since git sync should fail"),
        Err(err) => err,
    };

    assert!(err.message.contains("requires a clean working tree"));
    assert!(err
        .message
        .contains("snapshot sync cannot honor --changed-since"));
    assert!(err.message.contains("because it excludes .git metadata"));
    assert!(!err.message.contains("use --mode snapshot"));
    let hint_text = err.details["tried"]
        .as_array()
        .expect("changed-since error includes recovery options")
        .iter()
        .filter_map(|value| value.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(hint_text.contains("--placement local"));
    assert!(hint_text.contains("Omit --changed-since"));
}
