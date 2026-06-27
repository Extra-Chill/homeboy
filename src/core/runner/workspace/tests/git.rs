use std::fs;
use std::path::Path;
use std::process::Command;

use super::git;
use crate::core::runner::workspace::git::{git_snapshot, materialize_git_command};
use crate::core::runner::workspace::sync::sync_workspace;
use crate::core::runner::workspace::types::{RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions};
use crate::core::runner::workspace::util::git_output;

#[test]
fn git_sync_for_private_remote_materializes_controller_bundle_checkout() {
    crate::test_support::with_isolated_home(|_| {
        // Recognize `github.example.com` as a private/proxied source host so the
        // sync takes the hermetic controller-bundle path
        // (`materialize_git_from_controller_bundle`) instead of attempting a
        // real `git clone` over SSH. This keeps the test fully hermetic: it
        // exercises private-remote materialization without reaching the
        // network. `with_isolated_home` serializes env-mutating tests via a
        // global lock, so setting/clearing this env var here is race-free.
        let prior_private_hosts = std::env::var("HOMEBOY_PRIVATE_PROXIED_SOURCE_HOSTS").ok();
        std::env::set_var("HOMEBOY_PRIVATE_PROXIED_SOURCE_HOSTS", "github.example.com");

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
                "git@github.example.com:example-org/conductor.git",
            ],
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
                controller_routed_git: false,
                changed_since_base: None,
                git_fetch_refs: Vec::new(),
                snapshot_includes: Vec::new(),
                allow_dirty_lab_workspace: false,
                run_isolation_token: None,
            },
        );

        // Restore the prior env value before asserting so a failure does not
        // leak the override into subsequent tests.
        match prior_private_hosts {
            Some(value) => std::env::set_var("HOMEBOY_PRIVATE_PROXIED_SOURCE_HOSTS", value),
            None => std::env::remove_var("HOMEBOY_PRIVATE_PROXIED_SOURCE_HOSTS"),
        }

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
        // The controller-bundle path repoints origin at the real (private)
        // remote URL without fetching from it, proving the checkout was
        // materialized from the local bundle rather than a network clone.
        assert_eq!(
            git_output(remote, &["config", "--get", "remote.origin.url"]).unwrap(),
            "git@github.example.com:example-org/conductor.git"
        );
        assert_eq!(
            fs::read_to_string(remote.join("file.txt")).expect("read synced file"),
            "base\n"
        );
    });
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

    let err = match git_snapshot(source.path(), None, Vec::new()) {
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
    assert!(hint_text.contains("--force-hot"));
    assert!(hint_text.contains("homeboy runner workspace sync <runner-id>"));
}

#[test]
fn dirty_changed_since_git_sync_explains_snapshot_is_unavailable() {
    let source = super::dirty_git_repo();

    let err = match git_snapshot(source.path(), Some("abc123"), Vec::new()) {
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
    assert!(hint_text.contains("--force-hot"));
    assert!(hint_text.contains("Omit --changed-since"));
}
