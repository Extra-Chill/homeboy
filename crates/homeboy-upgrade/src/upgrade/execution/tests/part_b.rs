#![cfg(test)]

use super::*;

#[test]
fn test_verify_upgrade_with_retry() {
    // Smoke test covering the happy path with a single immediate read.
    let (success, active) = verify_upgrade_with_retry(
        InstallMethod::Secondary,
        false,
        "0.157.1",
        None,
        1,
        std::time::Duration::from_millis(0),
        || {
            Some(ActiveBinaryInfo {
                version: Some("0.158.0".to_string()),
                build_identity: None,
            })
        },
        |_| {},
    );

    assert!(success);
    assert_eq!(
        active.and_then(|info| info.version).as_deref(),
        Some("0.158.0")
    );
}

#[test]
fn source_swap_failure_errors_when_active_binary_unchanged() {
    // Issue #5772: the source upgrade command exited 0 but the read-back
    // proves the active binary was not replaced — fail loudly instead of a
    // soft `upgraded: false` "completed".
    let err = source_swap_failure(
        InstallMethod::Source,
        false,
        Some("0.247.5"),
        Some("homeboy 0.247.5+old"),
        Some(Path::new("/src/homeboy")),
        Some(Path::new("/src/homeboy/target/release/homeboy")),
        Some(Path::new("/active/homeboy")),
        Some("homeboy 0.247.5+new"),
    )
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

#[test]
fn source_swap_failure_reports_version_when_no_build_identity() {
    let err = source_swap_failure(
        InstallMethod::Source,
        false,
        Some("0.247.5"),
        None,
        Some(Path::new("/src/homeboy")),
        Some(Path::new("/src/homeboy/target/release/homeboy")),
        None,
        None,
    )
    .expect("unverified source swap must surface an error");

    assert!(err.message.contains("0.247.5"));
}

#[test]
fn source_swap_failure_reports_placeholder_when_version_unverifiable() {
    let err = source_swap_failure(
        InstallMethod::Source,
        false,
        None,
        None,
        Some(Path::new("/src/homeboy")),
        Some(Path::new("/src/homeboy/target/release/homeboy")),
        None,
        None,
    )
    .expect("unverified source swap must surface an error");

    assert!(err.message.contains("an unverifiable version"));
}

#[test]
fn source_swap_failure_silent_on_verified_swap() {
    assert!(
        source_swap_failure(
            InstallMethod::Source,
            true,
            Some("0.249.0"),
            None,
            Some(Path::new("/src/homeboy")),
            Some(Path::new("/src/homeboy/target/release/homeboy")),
            None,
            None,
        )
        .is_none(),
        "a verified source swap is not a failure"
    );
}

#[test]
fn source_swap_failure_ignores_non_source_methods() {
    // Non-source methods keep their soft unverified reporting; only source
    // (where the swap is part of the command's contract) fails loudly here.
    assert!(source_swap_failure(
        InstallMethod::Binary,
        false,
        Some("0.247.5"),
        None,
        Some(Path::new("/src/homeboy")),
        Some(Path::new("/src/homeboy/target/release/homeboy")),
        None,
        None,
    )
    .is_none());
    assert!(source_swap_failure(
        InstallMethod::Secondary,
        false,
        Some("0.247.5"),
        None,
        Some(Path::new("/src/homeboy")),
        Some(Path::new("/src/homeboy/target/release/homeboy")),
        None,
        None,
    )
    .is_none());
}

#[test]
fn source_swap_failure_diagnostics_use_replacement_target_path() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let target = dir.path().join("configured-runner-homeboy");
    std::fs::create_dir_all(source.join("target/release")).expect("source dirs");
    std::fs::write(&target, "old").expect("target");

    let err = source_swap_failure(
        InstallMethod::Source,
        false,
        Some("0.255.8"),
        Some("homeboy 0.255.8+old"),
        Some(&source),
        Some(&source.join("target/release/homeboy")),
        Some(&target),
        Some("homeboy 0.255.8+new"),
    )
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
fn source_swap_failure_diagnostics_report_effective_cargo_target_binary() {
    let source = Path::new("/src/homeboy");
    let built = Path::new("/shared/cargo-target/release/homeboy");
    let active = Path::new("/bin/homeboy");

    let diagnostics = source_swap_failure_diagnostics_for_paths(
        Some(source),
        Some(built),
        Some(active),
        None,
        None,
    );

    assert_eq!(diagnostics.built_binary_path, built.display().to_string());
    assert!(diagnostics
        .copy_command
        .as_deref()
        .expect("copy command")
        .contains("'/shared/cargo-target/release/homeboy'"));
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
}

#[cfg(unix)]
#[test]
fn install_source_built_binary_sets_executable_mode() {
    use std::os::unix::fs::PermissionsExt;

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

    let mode = std::fs::metadata(&active)
        .expect("active metadata")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755);
}

#[test]
fn install_source_built_binary_reports_copy_failure() {
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
}

#[test]
fn shell_quote_handles_paths_with_single_quotes() {
    assert_eq!(quote_path("/tmp/homeboy's/bin"), "'/tmp/homeboy'\\''s/bin'");
}

#[test]
fn test_resolve_source_workspace() {
    let dir = checkout_with_package_name("homeboy");

    let resolved = resolve_source_workspace(Some(dir.path())).expect("source checkout");

    assert_eq!(resolved, dir.path());
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

#[test]
fn source_workspace_accepts_snapshot_without_git_metadata() {
    let dir = source_workspace_with_package_name("homeboy");

    let resolved = resolve_source_workspace(Some(dir.path())).expect("source snapshot");

    assert_eq!(resolved, dir.path());
}

#[test]
fn source_workspace_resolves_from_nested_checkout_path() {
    let dir = checkout_with_package_name("homeboy");
    let nested = dir.path().join("src/core");
    std::fs::create_dir_all(&nested).expect("nested dir");

    let resolved = resolve_source_workspace(Some(&nested)).expect("source checkout");

    assert_eq!(resolved, dir.path());
}

#[test]
fn source_upgrade_preparation_preserves_detached_checkout_identity() {
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

    std::fs::write(seed.path().join("src.txt"), "new source\n").expect("write source change");
    git(seed.path(), &["add", "."]);
    git(seed.path(), &["commit", "-m", "update source"]);
    git(seed.path(), &["push", "origin", "main"]);

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

    std::fs::write(seed.path().join("src.txt"), "new source\n").expect("write source change");
    git(seed.path(), &["add", "."]);
    git(seed.path(), &["commit", "-m", "update source"]);
    git(seed.path(), &["push", "origin", "main"]);

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

#[test]
fn source_upgrade_command_preserves_branch_checkout_command() {
    let checkout = source_workspace_with_package_name("homeboy");
    git(checkout.path(), &["init", "--initial-branch", "main"]);

    let command = source_upgrade_command_for_prepared_workspace(
        "git pull --ff-only\ncargo build --release",
        checkout.path(),
        false,
    )
    .expect("source command");

    assert_eq!(command, "git pull --ff-only\ncargo build --release");
}

#[test]
fn explicit_source_upgrade_command_skips_pull_for_detached_checkout() {
    let checkout = source_workspace_with_package_name("homeboy");
    git(checkout.path(), &["init", "--initial-branch", "main"]);
    git(
        checkout.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(checkout.path(), &["config", "user.name", "Homeboy Test"]);
    git(checkout.path(), &["add", "."]);
    git(checkout.path(), &["commit", "-m", "initial"]);
    git(checkout.path(), &["switch", "--detach", "HEAD"]);

    let command = source_upgrade_command_for_prepared_workspace(
        "git pull --ff-only\ncargo build --release",
        checkout.path(),
        true,
    )
    .expect("source command");

    assert!(
        command.contains("Skipping git $homeboy_git_arg for explicitly selected source checkout")
    );
    assert!(command.contains("HOMEBOY_GIT_GUARD_DIR"));
    assert!(command.contains("git pull --ff-only\ncargo build --release"));
}

#[test]
fn explicit_source_upgrade_command_skips_pull_for_local_only_branch() {
    let checkout = source_workspace_with_package_name("homeboy");
    git(checkout.path(), &["init", "--initial-branch", "local-only"]);
    git(
        checkout.path(),
        &["config", "user.email", "test@example.com"],
    );
    git(checkout.path(), &["config", "user.name", "Homeboy Test"]);
    git(checkout.path(), &["add", "."]);
    git(checkout.path(), &["commit", "-m", "initial"]);

    let command = source_upgrade_command_for_prepared_workspace(
        "git pull --ff-only\ncargo build --release",
        checkout.path(),
        true,
    )
    .expect("source command");

    assert!(
        command.contains("Skipping git $homeboy_git_arg for explicitly selected source checkout")
    );
    assert!(command.contains("git pull --ff-only\ncargo build --release"));
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
            shell_quote_path(&build_marker)
        );
        let command =
            source_upgrade_command_for_prepared_workspace(&upgrade_command, checkout.path(), true)
                .expect("explicit source command");

        run_source_upgrade_command(&command, checkout.path(), Duration::from_secs(5))
            .expect("guarded upgrade command reaches build phase");
        assert_eq!(std::fs::read_to_string(build_marker).unwrap(), "built");
    }
}

#[test]
fn source_upgrade_command_preserves_snapshot_command() {
    let checkout = source_workspace_with_package_name("homeboy");

    let command = source_upgrade_command_for_prepared_workspace(
        "cargo build --release",
        checkout.path(),
        true,
    )
    .expect("source command");

    assert_eq!(command, "cargo build --release");
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

#[test]
fn binary_404_upgrade_error_suggests_source_fallback() {
    let err = upgrade_failure_error(
        InstallMethod::Binary,
        "curl: (22) The requested URL returned error: 404",
    );

    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("No release asset")));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("--source-path")));
}

#[test]
fn missing_tool_upgrade_error_suggests_source_fallback() {
    let err = upgrade_failure_error(
        InstallMethod::Secondary,
        &format!(
            "sh: 1: {}: not found",
            defaults::secondary_install_method_key()
        ),
    );

    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("Required executable")));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("--source-path")));
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
fn verify_upgrade_with_retry_terminates_on_identity_mismatch() {
    // Issue #9686: even when `read_active` always returns the same identity
    // (i.e. the swap was a no-op), the bounded retry must terminate and report
    // failure rather than looping indefinitely.
    let attempts = 4;
    let mut reads = 0u32;
    let (success, last) = verify_upgrade_with_retry(
        InstallMethod::Source,
        true,
        "0.300.0",
        Some("homeboy 0.300.0+old"),
        attempts,
        std::time::Duration::from_millis(0),
        || {
            reads += 1;
            Some(ActiveBinaryInfo {
                version: Some("0.300.0".to_string()),
                build_identity: Some("homeboy 0.300.0+old".to_string()),
            })
        },
        |_| {},
    );
    assert!(!success, "identity mismatch must fail closed");
    assert_eq!(reads, attempts, "must attempt exactly the configured count");
    assert_eq!(
        last.and_then(|i| i.build_identity).as_deref(),
        Some("homeboy 0.300.0+old")
    );
}

#[test]
fn source_swap_failure_includes_identity_comparison() {
    let err = source_swap_failure(
        InstallMethod::Source,
        false,
        Some("0.300.0"),
        Some("homeboy 0.300.0+old"),
        Some(Path::new("/src/homeboy")),
        Some(Path::new("/src/homeboy/target/release/homeboy")),
        Some(Path::new("/active/homeboy")),
        Some("homeboy 0.300.0+new"),
    )
    .expect("unverified source swap must surface an error");

    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("Built identity: homeboy 0.300.0+new")));
    assert!(err.hints.iter().any(|hint| hint
        .message
        .contains("Installed identity: homeboy 0.300.0+old")));
}

#[test]
fn install_source_built_binary_cleans_up_temp_on_copy_failure() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let target_dir = dir.path().join("bin");
    std::fs::create_dir_all(&source).expect("source dir");
    std::fs::create_dir_all(&target_dir).expect("target dir");
    let active = target_dir.join("homeboy");
    std::fs::write(&active, "old").expect("active binary");

    let built = source.join("target/release/homeboy");
    let _ = install_source_built_binary(&built, &active);

    // Verify no stale .homeboy-upgrade.*.tmp files remain in the bin directory.
    let stale: Vec<_> = std::fs::read_dir(&target_dir)
        .expect("read target dir")
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.file_name()
                .to_string_lossy()
                .starts_with(".homeboy-upgrade.")
        })
        .collect();
    assert!(
        stale.is_empty(),
        "no stale temp files should remain after a failed install, found: {:?}",
        stale
    );
}
