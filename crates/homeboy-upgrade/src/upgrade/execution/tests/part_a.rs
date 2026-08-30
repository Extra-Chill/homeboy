#![cfg(test)]

use super::*;

#[test]
fn bound_captured_stream_retains_full_source_within_limit() {
    let (text, capture) = bound_captured_stream(b"boom", 1024);

    assert_eq!(text, "boom");
    assert_eq!(capture.limit_bytes, 1024);
    assert_eq!(capture.seen_bytes, 4);
    assert_eq!(capture.retained_bytes, 4);
    assert!(!capture.truncated);
}

#[test]
fn bound_captured_stream_keeps_trailing_tail_when_truncated() {
    let source = vec![b'x'; 16];
    let (text, capture) = bound_captured_stream(&source, 4);

    assert_eq!(text, "xxxx");
    assert_eq!(capture.limit_bytes, 4);
    assert_eq!(capture.retained_bytes, 4);
    assert_eq!(capture.seen_bytes, 16);
    assert!(capture.truncated);
}

#[test]
fn bound_captured_stream_retains_most_relevant_tail() {
    let source = b"head-noise-TAIL".to_vec();
    let (text, capture) = bound_captured_stream(&source, 4);

    assert_eq!(text, "TAIL");
    assert!(capture.truncated);
}

#[test]
fn annotate_truncation_notes_dropped_bytes() {
    let capture = StreamCaptureMetadata {
        limit_bytes: 4,
        seen_bytes: 16,
        retained_bytes: 4,
        truncated: true,
    };

    let annotated = annotate_truncation("TAIL", &capture);

    assert!(annotated.starts_with("TAIL"));
    assert!(annotated.contains("output truncated"));
    assert!(annotated.contains("retained 4 of 16 bytes"));
}

#[test]
fn annotate_truncation_leaves_untruncated_detail_unchanged() {
    let capture = StreamCaptureMetadata {
        limit_bytes: 1024,
        seen_bytes: 4,
        retained_bytes: 4,
        truncated: false,
    };

    assert_eq!(annotate_truncation("boom", &capture), "boom");
}

#[test]
fn upgrade_failure_detail_preserves_stderr_and_structured_stdout() {
    let detail = upgrade_failure_detail(
        b"homeboy upgrade: target darwin-arm64, asset homeboy.tar.xz",
        br#"{"status":"failed","recovery_command":"homeboy agent-task reconcile run-id --apply"}"#,
    )
    .expect("failure detail");

    assert!(detail.contains("stderr:\nhomeboy upgrade: target darwin-arm64"));
    assert!(detail.contains("stdout:\n{\"status\":\"failed\""));
    assert!(detail.contains("homeboy agent-task reconcile run-id --apply"));
}

#[test]
fn parses_homeboy_version_output() {
    assert_eq!(
        parse_cli_version_output("homeboy 0.158.0").as_deref(),
        Some("0.158.0")
    );
}

#[test]
fn command_output_with_timeout_captures_child_output() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'homeboy 0.247.5'; printf 'warn' >&2"]);

    let output =
        command_output_with_timeout(&mut command, Duration::from_secs(5)).expect("command output");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "homeboy 0.247.5");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "warn");
}

#[test]
fn source_upgrade_command_returns_after_same_binary_success() {
    let workspace = tempfile::tempdir().expect("workspace");

    run_source_upgrade_command(
        "printf 'built same-version binary\\n'",
        workspace.path(),
        Duration::from_secs(1),
        None,
    )
    .expect("source command completes");
}

#[test]
fn source_build_command_receives_build_only_contract() {
    let workspace = tempfile::tempdir().expect("workspace");
    let observed = workspace.path().join("build-only");
    let command = format!(
        "printf '%s' \"$HOMEBOY_UPGRADE_BUILD_ONLY\" > {}",
        quote_path(&observed.display().to_string())
    );

    run_source_upgrade_command(&command, workspace.path(), Duration::from_secs(1), None)
        .expect("source build completes");

    assert_eq!(
        std::fs::read_to_string(observed).expect("build-only value"),
        "1"
    );
}

#[cfg(unix)]
#[test]
fn staged_source_candidate_owns_admission_and_preserves_installed_bytes_on_failure() {
    use std::os::unix::fs::PermissionsExt;

    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(
        workspace.path().join("Cargo.toml"),
        "[package]\nname = \"homeboy\"\nversion = \"0.352.0\"\n",
    )
    .expect("manifest");
    let installed = workspace.path().join("installed-homeboy");
    let candidate = workspace.path().join("staged-homeboy");
    let evidence = workspace.path().join("admission-evidence");
    std::fs::write(
        &installed,
        "#!/bin/sh\nif [ \"$1\" = self ]; then exit 64; fi\nprintf 'homeboy 0.351.0+old\\n'\n",
    )
    .expect("installed controller");
    std::fs::write(
        &candidate,
        format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'homeboy 0.352.0+new\\n'; exit 0; fi\nprintf '%s|%s|%s\\n' \"$1\" \"$4\" \"$6\" > {}\nexit 0\n",
            quote_path(&evidence.display().to_string())
        ),
    )
    .expect("staged candidate");
    for path in [&installed, &candidate] {
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755))
            .expect("executable fixture");
    }

    verify_source_candidate_target_admission(workspace.path(), &candidate, None, Some(&installed))
        .expect("new candidate, not the old controller, admits replacement");
    assert_eq!(
        std::fs::read_to_string(&evidence).expect("admission evidence"),
        "self|homeboy 0.351.0+old|0.352.0\n"
    );

    std::fs::write(
        &candidate,
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'homeboy 0.352.0+new\\n'; exit 0; fi\nexit 1\n",
    )
    .expect("failing staged candidate");
    std::fs::set_permissions(&candidate, std::fs::Permissions::from_mode(0o755))
        .expect("failing candidate executable");
    let error = verify_source_candidate_target_admission(
        workspace.path(),
        &candidate,
        None,
        Some(&installed),
    )
    .expect_err("candidate admission failure blocks promotion");

    assert!(error.message.contains("verified source candidate refused"));
    assert_eq!(
        std::fs::read_to_string(&installed).expect("installed bytes"),
        "#!/bin/sh\nif [ \"$1\" = self ]; then exit 64; fi\nprintf 'homeboy 0.351.0+old\\n'\n"
    );
}

#[test]
fn older_source_completion_is_superseded_unless_forced() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(
        workspace.path().join("Cargo.toml"),
        "[package]\nname = \"homeboy\"\nversion = \"1.2.3\"\n",
    )
    .expect("candidate manifest");
    let newer_active = homeboy_core::build_identity::BuildIdentity {
        display: "homeboy 1.2.4+new".to_string(),
        version: "1.2.4".to_string(),
        git_commit: Some("new".to_string()),
        git_dirty: Some(false),
    };

    // This models the old build completing after the newer build has already
    // promoted: re-reading active identity under the lease makes it a no-op.
    assert!(source_promotion_is_superseded(
        false,
        Some(&newer_active),
        workspace.path()
    ));
    let older_active = homeboy_core::build_identity::BuildIdentity {
        display: "homeboy 1.2.2+old".to_string(),
        version: "1.2.2".to_string(),
        git_commit: Some("old".to_string()),
        git_dirty: Some(false),
    };
    // In the reverse completion order, the newer candidate remains eligible.
    assert!(!source_promotion_is_superseded(
        false,
        Some(&older_active),
        workspace.path()
    ));
    assert!(!source_promotion_is_superseded(
        true,
        Some(&newer_active),
        workspace.path()
    ));
}

#[test]
fn cleanup_context_preserves_the_primary_upgrade_error_contract() {
    let primary = upgrade_failure_error(
        InstallMethod::Binary,
        "curl: (22) The requested URL returned error: 404",
        None,
    );
    let expected = primary.clone();
    let error = append_cleanup_failure_context(
        primary,
        Some(std::io::Error::other("cleanup process group failed")),
    );

    assert_eq!(error.code, expected.code);
    assert_eq!(error.details, expected.details);
    assert_eq!(
        error
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>(),
        expected
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>()
    );
    assert!(error.message.starts_with(&expected.message));
    assert!(error.message.contains("cleanup process group failed"));
}

#[test]
fn cleanup_context_is_bounded() {
    let error = append_cleanup_failure_context(
        Error::internal_io("primary failure", Some("source upgrade".to_string())),
        Some(std::io::Error::other("x".repeat(2_000))),
    );

    assert!(error.message.len() < 1_200);
    assert!(error.message.ends_with("... [truncated]"));
}

#[cfg(unix)]
#[test]
fn source_upgrade_completion_reaps_background_process_group() {
    let workspace = tempfile::tempdir().expect("workspace");
    let pid_file = workspace.path().join("child.pid");
    let command = format!(
        "sleep 30 & echo $! > {}; printf built",
        quote_path(&pid_file.display().to_string())
    );

    run_source_upgrade_command(&command, workspace.path(), Duration::from_secs(1), None)
        .expect("source command completes");

    let child_pid = std::fs::read_to_string(&pid_file)
        .expect("background child pid")
        .trim()
        .parse::<libc::pid_t>()
        .expect("numeric pid");
    let state = Command::new("ps")
        .args(["-o", "stat=", "-p", &child_pid.to_string()])
        .output()
        .expect("inspect background child state");
    assert!(
        state.stdout.is_empty()
            || String::from_utf8_lossy(&state.stdout)
                .trim_start()
                .starts_with('Z'),
        "background child {child_pid} remained runnable: {}",
        String::from_utf8_lossy(&state.stdout)
    );
}

#[cfg(unix)]
#[test]
fn source_upgrade_timeout_terminates_the_entire_child_process_group() {
    let workspace = tempfile::tempdir().expect("workspace");
    let pid_file = workspace.path().join("child.pid");
    let command = format!(
        "sleep 30 & echo $! > {}; wait",
        quote_path(&pid_file.display().to_string())
    );

    let err =
        run_source_upgrade_command(&command, workspace.path(), Duration::from_millis(50), None)
            .expect_err("long-running source command times out");
    assert!(
        err.details.to_string().to_lowercase().contains("timed out"),
        "unexpected timeout error: {err:?}"
    );

    let child_pid = std::fs::read_to_string(&pid_file)
        .expect("background child pid")
        .trim()
        .parse::<i32>()
        .expect("numeric pid");
    // The shell is the process-group leader and timeout termination must stop
    // its background child as well as reap the direct child.
    for _ in 0..40 {
        if !homeboy_core::process::pid_is_running(child_pid as u32) {
            return;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
    panic!("source-upgrade child must not be orphaned");
}

#[test]
fn test_execute_upgrade() {
    assert_eq!(
        parse_cli_version_output("homeboy 0.158.0").as_deref(),
        Some("0.158.0")
    );
    assert!(!upgrade_verification_result(
        InstallMethod::Source,
        false,
        "0.157.1",
        Some("0.157.1"),
        Some("commit old, dirty=false"),
        Some("commit new, dirty=false"),
    ));
}

#[test]
fn test_upgrade_verification_result() {
    assert!(upgrade_verification_result(
        InstallMethod::Secondary,
        false,
        "0.157.1",
        Some("0.158.0"),
        None,
        None,
    ));
    assert!(!upgrade_verification_result(
        InstallMethod::Secondary,
        false,
        "0.157.1",
        Some("0.157.1"),
        Some("commit old, dirty=false"),
        Some("commit new, dirty=false"),
    ));
    assert!(!upgrade_verification_result(
        InstallMethod::Source,
        true,
        "0.157.1",
        None,
        Some("commit old, dirty=false"),
        Some("commit new, dirty=false"),
    ));
}

#[test]
fn forced_source_upgrade_rejects_unchanged_same_version_build_identity() {
    assert!(!upgrade_verification_result(
        InstallMethod::Source,
        true,
        "0.157.1",
        Some("0.157.1"),
        Some("commit same, dirty=false"),
        Some("commit same, dirty=false"),
    ));
}

#[test]
fn forced_secondary_upgrade_accepts_same_version_active_binary() {
    assert!(upgrade_verification_result(
        InstallMethod::Secondary,
        true,
        "0.157.1",
        Some("0.157.1"),
        Some("commit same, dirty=false"),
        Some("commit same, dirty=false"),
    ));
}

#[test]
fn verification_accepts_newer_active_binary() {
    assert!(upgrade_verification_result(
        InstallMethod::Secondary,
        false,
        "0.157.1",
        Some("0.158.0"),
        None,
        None,
    ));
}

#[test]
fn verification_rejects_missing_active_binary_version() {
    assert!(!upgrade_verification_result(
        InstallMethod::Source,
        true,
        "0.157.1",
        None,
        Some("commit old, dirty=false"),
        Some("commit new, dirty=false"),
    ));
}

#[test]
fn forced_source_upgrade_accepts_same_version_with_new_build_identity() {
    assert!(upgrade_verification_result(
        InstallMethod::Source,
        true,
        "0.157.1",
        Some("0.157.1"),
        Some("homeboy 0.157.1+old"),
        Some("homeboy 0.157.1+new"),
    ));
}

#[test]
fn source_install_byte_match_rejects_same_version_stale_binary() {
    let dir = tempfile::tempdir().expect("tempdir");
    let source = dir.path().join("source");
    let built = source.join("target/release/homeboy");
    let active = dir.path().join("bin/homeboy");
    std::fs::create_dir_all(built.parent().expect("built parent")).expect("built parent dir");
    std::fs::create_dir_all(active.parent().expect("active parent")).expect("active parent dir");
    std::fs::write(&built, b"homeboy 0.281.20 with new source behavior")
        .expect("write built binary");
    std::fs::write(&active, b"homeboy 0.281.20 stale installed binary")
        .expect("write active binary");

    assert!(
        !upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.281.20",
            Some("0.281.20"),
            Some("homeboy 0.281.20"),
            Some("homeboy 0.281.20"),
        ),
        "identity-only verification cannot prove same-version source replacement"
    );
    assert!(
        !source_install_matches_binary_path(&built, &active).expect("compare binaries"),
        "same-version stale active binary must not verify"
    );

    std::fs::copy(&built, &active).expect("install built binary");

    assert!(
        source_install_matches_binary_path(&built, &active).expect("compare binaries"),
        "source upgrade only verifies after the active binary is the built artifact"
    );
}

#[test]
fn source_built_binary_path_uses_the_managed_target() {
    let target = Path::new("/managed/homeboy-cargo-target");

    assert_eq!(
        source_built_binary_path(target),
        target.join("release/homeboy")
    );
}

#[test]
fn source_build_command_uses_the_managed_target_outside_the_checkout() {
    let workspace = tempfile::tempdir().expect("workspace");
    let target_dir = Path::new("/shared/cargo-target");
    let observed = workspace.path().join("cargo-target");
    let command = format!(
        "printf '%s' \"$CARGO_TARGET_DIR\" > {}",
        quote_path(&observed.display().to_string())
    );

    run_source_upgrade_command(
        &command,
        workspace.path(),
        Duration::from_secs(1),
        Some((target_dir, "isolated")),
    )
    .expect("source build completes");

    assert_eq!(
        std::fs::read_to_string(observed).expect("target value"),
        "/shared/cargo-target"
    );
}

#[test]
fn source_upgrade_reports_external_target_deletion_separately_from_capacity() {
    let target = tempfile::tempdir().expect("target");
    let deleted = target.path().join("deleted");
    let status = Command::new("sh")
        .args(["-c", "exit 1"])
        .status()
        .expect("status");

    let error = source_upgrade_command_failure(status, Some(&deleted));

    assert!(error
        .message
        .contains("deleted while its lifecycle lease was active"));
    assert!(error
        .hints
        .iter()
        .any(|hint| hint.message.contains("external deletion")));
}

#[test]
fn forced_source_upgrade_accepts_same_version_without_build_identity() {
    assert!(upgrade_verification_result(
        InstallMethod::Source,
        true,
        "0.157.1",
        Some("0.157.1"),
        None,
        Some("homeboy 0.157.1+new"),
    ));
}

#[test]
fn non_forced_upgrade_rejects_same_version_active_binary() {
    assert!(!upgrade_verification_result(
        InstallMethod::Source,
        false,
        "0.157.1",
        Some("0.157.1"),
        Some("homeboy 0.157.1+old"),
        Some("homeboy 0.157.1+old"),
    ));
}

#[test]
fn parses_homeboy_version_output_with_build_identity() {
    let info = parse_cli_version_info("homeboy 0.158.0+abc123-dirty");

    assert_eq!(info.version.as_deref(), Some("0.158.0"));
    assert_eq!(
        info.build_identity.as_deref(),
        Some("homeboy 0.158.0+abc123-dirty")
    );
}

#[test]
fn parses_plain_homeboy_version_output_as_build_identity() {
    let info = parse_cli_version_info("homeboy 0.158.0");

    assert_eq!(info.version.as_deref(), Some("0.158.0"));
    assert_eq!(info.build_identity.as_deref(), Some("homeboy 0.158.0"));
}

#[test]
fn parses_build_identity_display_with_commit() {
    // The installed target's `--version` string is reconstructed so the
    // source-upgrade decision can compare against it rather than the invoking
    // candidate (#9371).
    let identity =
        parse_build_identity_display("homeboy 0.298.1+4a57291e16d9").expect("parseable identity");

    assert_eq!(identity.version, "0.298.1");
    assert_eq!(identity.git_commit.as_deref(), Some("4a57291e16d9"));
    assert_eq!(identity.git_dirty, Some(false));
    assert_eq!(identity.display, "homeboy 0.298.1+4a57291e16d9");
}

#[test]
fn parses_build_identity_display_with_dirty_commit() {
    let identity =
        parse_build_identity_display("homeboy 0.298.1+4a57291e16d9-dirty").expect("parseable");

    assert_eq!(identity.version, "0.298.1");
    assert_eq!(identity.git_commit.as_deref(), Some("4a57291e16d9"));
    assert_eq!(identity.git_dirty, Some(true));
}

#[test]
fn parses_plain_build_identity_display_without_commit() {
    let identity = parse_build_identity_display("homeboy 0.298.1").expect("parseable");

    assert_eq!(identity.version, "0.298.1");
    assert_eq!(identity.git_commit, None);
    assert_eq!(identity.git_dirty, None);
}

#[test]
fn rejects_unparseable_build_identity_display() {
    // A non-semver version cannot participate in the deterministic decision and
    // must stay on the safe unverifiable path rather than pretend to parse.
    assert!(parse_build_identity_display("homeboy not-a-version").is_none());
    assert!(parse_build_identity_display("").is_none());
}

#[test]
fn verify_retry_succeeds_after_transient_unreadable_binary() {
    // Issue #3463: the swap succeeded but the first read-back of the new
    // binary returns nothing (racing the just-replaced binary). A later
    // attempt reports the upgraded version and verification must succeed.
    let reads = std::cell::RefCell::new(vec![
        None,
        Some(ActiveBinaryInfo {
            version: Some("0.220.3".to_string()),
            build_identity: None,
        }),
    ]);
    let mut sleeps = 0u32;

    let (success, active) = verify_upgrade_with_retry(
        InstallMethod::Binary,
        false,
        "0.220.0",
        None,
        5,
        std::time::Duration::from_millis(0),
        || reads.borrow_mut().remove(0),
        |_| sleeps += 1,
    );

    assert!(success, "transient read-back failure should be retried");
    assert_eq!(
        active.and_then(|info| info.version).as_deref(),
        Some("0.220.3")
    );
    assert_eq!(sleeps, 1, "should sleep once between the two attempts");
}

#[test]
fn verify_retry_succeeds_after_stale_old_version() {
    // The read-back briefly reports the old version before the new binary
    // is observable; the retry should pick up the upgraded version.
    let reads = std::cell::RefCell::new(vec![
        Some(ActiveBinaryInfo {
            version: Some("0.220.0".to_string()),
            build_identity: None,
        }),
        Some(ActiveBinaryInfo {
            version: Some("0.220.3".to_string()),
            build_identity: None,
        }),
    ]);

    let (success, active) = verify_upgrade_with_retry(
        InstallMethod::Binary,
        false,
        "0.220.0",
        None,
        5,
        std::time::Duration::from_millis(0),
        || reads.borrow_mut().remove(0),
        |_| {},
    );

    assert!(success);
    assert_eq!(
        active.and_then(|info| info.version).as_deref(),
        Some("0.220.3")
    );
}

#[test]
fn verify_retry_first_attempt_success_does_not_sleep() {
    let mut sleeps = 0u32;

    let (success, active) = verify_upgrade_with_retry(
        InstallMethod::Binary,
        false,
        "0.220.0",
        None,
        5,
        std::time::Duration::from_millis(0),
        || {
            Some(ActiveBinaryInfo {
                version: Some("0.220.3".to_string()),
                build_identity: None,
            })
        },
        |_| sleeps += 1,
    );

    assert!(success);
    assert_eq!(
        active.and_then(|info| info.version).as_deref(),
        Some("0.220.3")
    );
    assert_eq!(sleeps, 0, "no retries needed when first read verifies");
}

#[test]
fn verify_retry_exhausts_attempts_when_never_readable() {
    let mut reads = 0u32;
    let mut sleeps = 0u32;

    let (success, active) = verify_upgrade_with_retry(
        InstallMethod::Binary,
        false,
        "0.220.0",
        None,
        3,
        std::time::Duration::from_millis(0),
        || {
            reads += 1;
            None
        },
        |_| sleeps += 1,
    );

    assert!(
        !success,
        "genuinely unverifiable upgrade still reports false"
    );
    assert!(active.is_none());
    assert_eq!(reads, 3, "all attempts consumed");
    assert_eq!(sleeps, 2, "sleeps between attempts but not after the last");
}

#[test]
fn verify_retry_reports_last_seen_version_on_exhaustion() {
    // The new version never becomes observable, but a stale old-version
    // read is retained so the caller can still surface a version string.
    let (success, active) = verify_upgrade_with_retry(
        InstallMethod::Binary,
        false,
        "0.220.0",
        None,
        2,
        std::time::Duration::from_millis(0),
        || {
            Some(ActiveBinaryInfo {
                version: Some("0.220.0".to_string()),
                build_identity: None,
            })
        },
        |_| {},
    );

    assert!(!success);
    assert_eq!(
        active.and_then(|info| info.version).as_deref(),
        Some("0.220.0")
    );
}
