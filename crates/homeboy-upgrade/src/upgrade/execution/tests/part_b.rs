#![cfg(test)]

use super::*;

#[test]
fn binary_swap_failure_identifies_the_target_and_observed_destination() {
    let err = binary_swap_failure(
        "0.326.1",
        Some(Path::new("/Users/chubes/.cargo/bin/homeboy")),
        Some(&ActiveBinaryInfo {
            version: Some("0.326.0".to_string()),
            build_identity: Some("homeboy 0.326.0".to_string()),
        }),
    );

    assert!(err.message.contains("0.326.1"));
    assert!(err.message.contains("/Users/chubes/.cargo/bin/homeboy"));
    assert!(err.message.contains("observed 0.326.0"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("type -a homeboy")));
}

#[cfg(unix)]
#[test]
fn staged_verification_rejects_a_wrong_candidate_version() {
    let directory = tempfile::tempdir().expect("stage directory");
    let staged = directory.path().join("homeboy");
    homeboy_test_support::write_executable(
        &staged,
        "#!/bin/sh\necho '{\"identity\":\"0.376.0\",\"observation_schema_version\":0}'\n",
        0o755,
    )
    .expect("write stale staged candidate");

    let error = verify_staged_release_candidate("0.376.1", &staged)
        .expect_err("a wrong staged version must not verify");

    assert!(
        error
            .message
            .contains("did not activate the selected release 0.376.1"),
        "{}",
        error.message
    );
    assert!(error.message.contains(&staged.display().to_string()));
    assert!(error.message.contains("observed 0.376.0"));
}

#[test]
fn source_swap_failure_errors_when_active_binary_unchanged() {
    // Issue #5772: the source upgrade command exited 0 but the read-back
    // proves the active binary was not replaced — fail loudly instead of a
    // soft `upgraded: false` "completed".
    let err = source_swap_failure(SourceSwapVerification {
        method: InstallMethod::Source,
        success: false,
        new_version: Some("0.247.5"),
        new_build_identity: Some("homeboy 0.247.5+old"),
        source_workspace: Some(Path::new("/src/homeboy")),
        built_binary: Some(Path::new("/src/homeboy/target/release/homeboy")),
        replacement_target: Some(Path::new("/active/homeboy")),
        built_binary_identity: Some("homeboy 0.247.5+new"),
    })
    .expect("unverified source swap must surface an error");

    assert!(err.message.contains("active binary was not replaced"));
    assert!(err.message.contains("homeboy 0.247.5+old"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("--method source")));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("Active binary path:")));
    assert!(err.hints.iter().any(|hint| hint
        .message
        .contains("Built source binary: /src/homeboy/target/release/homeboy")));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("Replacement target path:")));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("Permissions:")));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("Built identity: homeboy 0.247.5+new")));
    assert!(err.hints.iter().any(|hint| hint
        .message
        .contains("Installed identity: homeboy 0.247.5+old")));
}

/// Only an unverified source swap fails loudly; non-source methods keep their
/// soft unverified reporting. The message names whatever identity was read.
#[test]
fn source_swap_failure_is_raised_only_for_unverified_source_swaps() {
    for (method, success, new_version, expected) in [
        (
            InstallMethod::Source,
            false,
            Some("0.247.5"),
            Some("0.247.5"),
        ),
        (
            InstallMethod::Source,
            false,
            None,
            Some("an unverifiable version"),
        ),
        (InstallMethod::Source, true, Some("0.249.0"), None),
        (InstallMethod::Binary, false, Some("0.247.5"), None),
        (InstallMethod::Homebrew, false, Some("0.247.5"), None),
    ] {
        let err = source_swap_failure(SourceSwapVerification {
            method,
            success,
            new_version,
            new_build_identity: None,
            source_workspace: Some(Path::new("/src/homeboy")),
            built_binary: Some(Path::new("/src/homeboy/target/release/homeboy")),
            replacement_target: None,
            built_binary_identity: None,
        });

        match expected {
            Some(fragment) => assert!(err.expect("source swap fails").message.contains(fragment)),
            None => assert!(err.is_none(), "{method:?} success={success}"),
        }
    }
}

#[test]
fn source_swap_failure_diagnostics_use_replacement_target_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let target = dir.path().join("configured-runner-homeboy");
    std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
    std::fs::write(&target, "old").expect("target");

    let err = source_swap_failure(SourceSwapVerification {
        method: InstallMethod::Source,
        success: false,
        new_version: Some("0.255.8"),
        new_build_identity: Some("homeboy 0.255.8+old"),
        source_workspace: Some(&source),
        built_binary: Some(&source.join("target/release/homeboy")),
        replacement_target: Some(&target),
        built_binary_identity: Some("homeboy 0.255.8+new"),
    })
    .expect("unverified source swap must surface an error");

    assert!(err.hints.iter().any(|hint| hint
        .message
        .contains(&format!("Active binary path: {}", target.display()))));
    assert!(err.hints.iter().any(|hint| hint
        .message
        .contains(&format!("Replacement target path: {}", target.display()))));
    let first_recovery = err
        .hints
        .iter()
        .find(|hint| hint.message.contains("Homeboy") || hint.message.contains("replacement"))
        .map(|hint| hint.message.as_str())
        .unwrap_or_default();
    assert!(first_recovery.contains("Retry through just-built Homeboy"));
}

#[test]
fn source_swap_failure_diagnostics_include_paths_permissions_and_remediation() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source checkout");
    let bin_dir = dir.path().join("bin dir");
    std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
    std::fs::create_dir_all(&bin_dir).expect("bin dir");
    let active = bin_dir.join("homeboy active");
    let built = source.join("target/release/homeboy");
    std::fs::write(&active, "old").expect("active");
    std::fs::write(&built, "new").expect("built");

    let diagnostics = source_swap_failure_diagnostics_for_paths(
        Some(&source),
        Some(&built),
        Some(&active),
        Some("homeboy 0.255.8+new"),
        Some("homeboy 0.255.8+old"),
    );

    assert_eq!(diagnostics.active_path, active.display().to_string());
    assert_eq!(diagnostics.built_binary_path, built.display().to_string());
    assert_eq!(diagnostics.replacement_path, active.display().to_string());
    assert!(diagnostics.permissions.contains("active="));
    assert!(diagnostics.permissions.contains("parent="));
    assert!(diagnostics.permissions.contains("writable="));
    let expected_copy_command = format!(
        "install -m 0755 '{}' '{}'",
        built.display(),
        active.display()
    );
    assert_eq!(
        diagnostics.copy_command.as_deref(),
        Some(expected_copy_command.as_str())
    );
    let built_command = diagnostics
        .built_binary_command
        .as_deref()
        .expect("built command");
    assert!(built_command.contains("upgrade --method source"));
    assert!(built_command.contains("--source-path"));
    assert!(built_command.contains(&source.display().to_string()));
}

#[test]
fn install_source_built_binary_replaces_active_target() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let target_dir = dir.path().join("bin");
    std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
    std::fs::create_dir_all(&target_dir).expect("target dir");
    let built = source.join("target/release/homeboy");
    let active = target_dir.join("homeboy");
    std::fs::write(&built, "new homeboy").expect("built binary");
    std::fs::write(&active, "old homeboy").expect("active binary");

    install_source_built_binary(&built, &active).expect("install source binary");

    assert_eq!(
        std::fs::read_to_string(&active).expect("active"),
        "new homeboy"
    );
    assert!(binary_files_match(&built, &active).expect("files match"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&active)
            .expect("active metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }
}

#[test]
fn install_source_built_binary_reports_copy_failure_and_leaves_no_temp_files() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let target_dir = dir.path().join("bin");
    std::fs::create_dir_all(&source).expect("source dir");
    std::fs::create_dir_all(&target_dir).expect("target dir");
    let active = target_dir.join("homeboy");
    std::fs::write(&active, "old homeboy").expect("active binary");

    let built = source.join("target/release/homeboy");
    let err = install_source_built_binary(&built, &active).expect_err("missing build fails");

    let details = err.details.to_string();
    assert!(details.contains("target/release/homeboy"));
    assert!(details.contains("install source-built binary"));
    assert_eq!(
        std::fs::read_to_string(&active).expect("active"),
        "old homeboy"
    );
    let stale: Vec<_> = std::fs::read_dir(&target_dir)
        .expect("read target dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".homeboy-upgrade.")
        })
        .collect();
    assert!(stale.is_empty(), "stale temp files remain: {stale:?}");
}

#[test]
fn shell_quote_handles_paths_with_single_quotes() {
    assert_eq!(quote_path("/tmp/homeboy's/bin"), "'/tmp/homeboy'\\''s/bin'");
}

#[test]
fn source_workspace_resolves_checkouts_snapshots_and_nested_paths() {
    let checkout = checkout_with_package_name("homeboy");
    let nested = checkout.path().join("src/core");
    std::fs::create_dir_all(&nested).expect("nested dir");
    let snapshot = source_workspace_with_package_name("homeboy");

    for (input, expected) in [
        (checkout.path(), checkout.path()),
        (nested.as_path(), checkout.path()),
        (snapshot.path(), snapshot.path()),
    ] {
        let resolved = resolve_source_workspace(Some(input)).expect("source workspace");
        assert_eq!(resolved, expected, "{}", input.display());
    }
}

#[test]
fn source_workspace_rejects_non_homeboy_checkout() {
    let dir = checkout_with_package_name("other");

    let err = resolve_source_workspace(Some(dir.path())).expect_err("invalid checkout");

    assert!(err.message.contains("Homeboy source workspace"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("--source-path")));
}

/// The controller checkout resolution must depend only on the running
/// executable's own path, never on the caller's working directory (#14736).
/// The test process's cwd is not the fabricated checkout, so a resolved answer
/// can only have come from `exe_path`. A binary with no ancestor checkout
/// marker (Homebrew or bare download) resolves to `None`.
#[test]
fn controller_source_checkout_resolves_from_exe_path_not_cwd() {
    let dir = checkout_with_package_name("homeboy");
    let exe_path = dir.path().join("target/release/homeboy");
    std::fs::create_dir_all(exe_path.parent().expect("parent")).expect("target dir");
    std::fs::write(&exe_path, "fake binary").expect("write fake exe");

    let resolved =
        controller_source_checkout_from(&exe_path).expect("checkout resolves from exe path");
    assert_eq!(resolved, dir.path().canonicalize().expect("canonicalize"));

    let bare = tempfile::tempdir().expect("tempdir");
    let installed = bare.path().join("bin/homeboy");
    std::fs::create_dir_all(installed.parent().expect("parent")).expect("bin dir");
    std::fs::write(&installed, "fake binary").expect("write fake exe");
    assert!(controller_source_checkout_from(&installed).is_none());
}

/// A bare `main` remote plus a committed seed checkout tracking it.
fn seeded_remote() -> (tempfile::TempDir, tempfile::TempDir) {
    let remote = tempfile::tempdir().expect("remote tempdir");
    git(
        remote.path(),
        &["init", "--bare", "--initial-branch", "main"],
    );

    let seed = source_workspace_with_package_name("homeboy");
    git(seed.path(), &["init", "--initial-branch", "main"]);
    git(seed.path(), &["config", "user.email", "test@example.com"]);
    git(seed.path(), &["config", "user.name", "Homeboy Test"]);
    git(seed.path(), &["add", "."]);
    git(seed.path(), &["commit", "-m", "initial"]);
    git(
        seed.path(),
        &[
            "remote",
            "add",
            "origin",
            &remote.path().display().to_string(),
        ],
    );
    git(seed.path(), &["push", "-u", "origin", "main"]);
    (remote, seed)
}

/// Pushes a new commit from `seed` so other clones fall behind origin.
fn push_source_change(seed: &Path) {
    std::fs::write(seed.join("src.txt"), "new source\n").expect("write source change");
    git(seed, &["add", "."]);
    git(seed, &["commit", "-m", "update source"]);
    git(seed, &["push", "origin", "main"]);
}

#[test]
fn source_upgrade_preparation_preserves_detached_checkout_identity() {
    let (remote, seed) = seeded_remote();

    let checkout = tempfile::tempdir().expect("checkout tempdir");
    std::fs::remove_dir(checkout.path()).expect("remove placeholder checkout dir");
    git(
        remote.path(),
        &[
            "clone",
            &remote.path().display().to_string(),
            &checkout.path().display().to_string(),
        ],
    );

    push_source_change(seed.path());

    git(checkout.path(), &["switch", "--detach", "HEAD"]);
    let stale_head = git_stdout(checkout.path(), &["rev-parse", "HEAD"]);

    let source_revision =
        prepare_source_workspace_for_upgrade(checkout.path()).expect("prepare detached checkout");

    let prepared_head = git_stdout(checkout.path(), &["rev-parse", "HEAD"]);
    assert_eq!(prepared_head, stale_head);
    assert_eq!(source_revision.as_deref(), Some(stale_head.as_str()));
    assert_eq!(
        git_stdout(checkout.path(), &["branch", "--show-current"]),
        ""
    );
}

#[test]
fn source_upgrade_preparation_preserves_local_only_worktree_branch() {
    let (remote, seed) = seeded_remote();

    let root = tempfile::tempdir().expect("root tempdir");
    let main_checkout = root.path().join("main-checkout");
    git(
        root.path(),
        &[
            "clone",
            &remote.path().display().to_string(),
            &main_checkout.display().to_string(),
        ],
    );

    let source_worktree = root.path().join("source-worktree");
    git(
        &main_checkout,
        &[
            "worktree",
            "add",
            "-b",
            "feature-upgrade-source",
            &source_worktree.display().to_string(),
            "HEAD",
        ],
    );

    push_source_change(seed.path());

    let switch_main = std::process::Command::new("git")
        .arg("-C")
        .arg(&source_worktree)
        .args(["switch", "main"])
        .output()
        .expect("run git switch main");
    assert!(
        !switch_main.status.success(),
        "test setup should reproduce branch ownership failure"
    );

    let source_head = git_stdout(&source_worktree, &["rev-parse", "HEAD"]);
    let source_revision =
        prepare_source_workspace_for_upgrade(&source_worktree).expect("prepare source worktree");

    assert_eq!(
        git_stdout(&source_worktree, &["branch", "--show-current"]),
        "feature-upgrade-source"
    );
    assert_eq!(
        git_stdout(&source_worktree, &["rev-parse", "HEAD"]),
        source_head
    );
    assert_eq!(source_revision.as_deref(), Some(source_head.as_str()));

    let command = source_upgrade_command_for_prepared_workspace(
        "git pull --ff-only\ncargo build --release",
        &source_worktree,
        true,
    )
    .expect("explicit source command");
    assert!(
        command.contains("Skipping git $homeboy_git_arg for explicitly selected source checkout")
    );
}

#[test]
fn source_upgrade_preparation_rejects_dirty_source_checkout() {
    let (_remote, seed) = seeded_remote();
    std::fs::write(seed.path().join("uncommitted.txt"), "dirty\n").expect("dirty file");

    let err = prepare_source_workspace_for_upgrade(seed.path()).expect_err("dirty rejected");

    assert!(err.message.contains("uncommitted changes"));
}

#[test]
fn source_upgrade_preparation_rejects_checkout_without_a_commit() {
    let checkout = source_workspace_with_package_name("homeboy");
    git(checkout.path(), &["init", "--initial-branch", "main"]);

    let err = source_workspace_revision(checkout.path()).expect_err("unverified checkout rejected");

    assert!(err.message.contains("immutable commit"));
}

/// Commands are passed through unchanged for a non-explicit branch checkout
/// and for a snapshot without git metadata.
#[test]
fn source_upgrade_command_passes_through_when_no_git_guard_applies() {
    let branch_checkout = source_workspace_with_package_name("homeboy");
    git(
        branch_checkout.path(),
        &["init", "--initial-branch", "main"],
    );
    let snapshot = source_workspace_with_package_name("homeboy");

    for (workspace, explicit, raw) in [
        (
            branch_checkout.path(),
            false,
            "git pull --ff-only\ncargo build --release",
        ),
        (snapshot.path(), true, "cargo build --release"),
    ] {
        let command = source_upgrade_command_for_prepared_workspace(raw, workspace, explicit)
            .expect("source command");
        assert_eq!(command, raw);
    }
}

#[test]
fn explicit_source_upgrade_runs_build_phase_without_upstream_on_local_branch_or_detached_head() {
    for detached in [false, true] {
        let checkout = source_workspace_with_package_name("homeboy");
        git(checkout.path(), &["init", "--initial-branch", "local-only"]);
        git(
            checkout.path(),
            &["config", "user.email", "test@example.com"],
        );
        git(checkout.path(), &["config", "user.name", "Homeboy Test"]);
        git(checkout.path(), &["add", "."]);
        git(checkout.path(), &["commit", "-m", "initial"]);
        if detached {
            git(checkout.path(), &["switch", "--detach", "HEAD"]);
        }

        let build_marker = checkout.path().join("build-phase");
        let upgrade_command = format!(
            "set -e\ngit fetch origin\ngit reset --hard origin/main\nsh -c 'git pull --ff-only'\nprintf built > {}",
            quote_path(&build_marker.display().to_string())
        );
        let command =
            source_upgrade_command_for_prepared_workspace(&upgrade_command, checkout.path(), true)
                .expect("explicit source command");

        run_source_upgrade_command(&command, checkout.path(), Duration::from_secs(5), None)
            .expect("guarded upgrade command reaches build phase");
        assert_eq!(std::fs::read_to_string(build_marker).unwrap(), "built");
    }
}

#[test]
fn explicit_source_upgrade_skips_git_switch_main_and_reaches_build_phase() {
    // #6987: an upgrade command that runs `git switch main` must not switch the
    // explicitly selected source checkout — otherwise it collides with another
    // worktree that owns main. The guard skips switch/checkout and the build
    // phase still runs; the source stays on its original branch.
    let checkout = source_workspace_with_package_name("homeboy");
    git(
        checkout.path(),
        &["init", "--initial-branch", "task-branch"],
    );
    git(
        checkout.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(checkout.path(), &["config", "user.name", "Homeboy Test"]);
    git(checkout.path(), &["add", "."]);
    git(checkout.path(), &["commit", "-m", "initial"]);

    let build_marker = checkout.path().join("build-phase");
    // A command that would move the checkout to main (and, without the guard,
    // `set -e` would abort before the build). `git checkout -b` is also skipped.
    let upgrade_command = format!(
        "set -e\ngit switch main\ngit checkout main\nprintf built > {}",
        quote_path(&build_marker.display().to_string())
    );
    let command =
        source_upgrade_command_for_prepared_workspace(&upgrade_command, checkout.path(), true)
            .expect("explicit source command");

    run_source_upgrade_command(&command, checkout.path(), Duration::from_secs(5), None)
        .expect("guarded upgrade command reaches build phase despite git switch main");

    assert_eq!(std::fs::read_to_string(build_marker).unwrap(), "built");
    // The source checkout is untouched: still on its original task branch.
    assert_eq!(
        git_stdout(checkout.path(), &["branch", "--show-current"]),
        "task-branch"
    );
}

#[test]
fn executable_workspace_only_resolves_target_build_paths() {
    let path = Path::new("/repo/target/release/homeboy");
    assert_eq!(
        workspace_from_exe_path(path).as_deref(),
        Some(Path::new("/repo"))
    );

    let installed = Path::new("/usr/local/bin/homeboy");
    assert!(workspace_from_exe_path(installed).is_none());
}

/// A release 404 names the asset URL and target triple it looked for (#11750),
/// admits an undetermined target instead of guessing one, and always offers
/// the source fallback.
#[test]
fn binary_404_upgrade_error_diagnoses_the_missing_asset() {
    let release = |target: Option<&str>| SelectedRelease {
        tag: "v0.333.0".to_string(),
        version: "0.333.0".to_string(),
        target: target.map(str::to_string),
    };
    let resolved = release(Some("x86_64-unknown-linux-gnu"));
    let unresolved = release(None);
    for (selected, expected) in [
        (None, vec!["No release asset", "--source-path"]),
        (
            Some(&resolved),
            vec![
                "x86_64-unknown-linux-gnu",
                "https://github.com/Extra-Chill/homeboy/releases/download/v0.333.0/homeboy-x86_64-unknown-linux-gnu.tar.xz",
                "--version <TAG>",
                "--source-path",
            ],
        ),
        (Some(&unresolved), vec!["could not be determined", "--source-path"]),
    ] {
        let err = upgrade_failure_error(
            InstallMethod::Binary,
            "curl: (22) The requested URL returned error: 404",
            selected,
        );
        let hints: Vec<&str> = err.hints.iter().map(|hint| hint.message.as_str()).collect();
        for fragment in expected {
            assert!(
                hints.iter().any(|hint| hint.contains(fragment)),
                "missing {fragment:?} in {hints:?}"
            );
        }
    }
}

#[test]
fn verify_source_install_with_retry_terminates_on_mismatch() {
    // Issue #9686: a source swap failure must NOT loop forever. The bounded
    // retry must terminate after the configured number of attempts, returning
    // false to signal the mismatch.
    let attempts = 3;
    let mut sleep_count = 0u32;
    let success = verify_source_install_with_retry(
        None,
        attempts,
        std::time::Duration::from_millis(0),
        |_| {
            sleep_count += 1;
        },
    );
    assert!(!success, "mismatch must fail closed");
    assert_eq!(sleep_count, attempts - 1, "must sleep between each attempt");
}

#[test]
fn a_replaced_running_executable_resolves_to_its_installed_pathname() {
    let dir = tempfile::tempdir().unwrap();
    let installed = dir.path().join("homeboy");
    std::fs::write(&installed, b"new binary").unwrap();
    let running = std::path::PathBuf::from(format!("{} (deleted)", installed.display()));
    // Linux names the running image of a replaced binary `<path> (deleted)`.
    assert_eq!(replaced_executable_pathname(running), installed);
    // A live pathname is returned unchanged.
    assert_eq!(replaced_executable_pathname(installed.clone()), installed);
}
