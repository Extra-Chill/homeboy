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
    )
    .expect("source command completes");
}

#[cfg(unix)]
#[test]
fn source_upgrade_timeout_terminates_the_entire_child_process_group() {
    let workspace = tempfile::tempdir().expect("workspace");
    let pid_file = workspace.path().join("child.pid");
    let command = format!("sleep 30 & echo $! > {}; wait", shell_quote_path(&pid_file));

    let err = run_source_upgrade_command(&command, workspace.path(), Duration::from_millis(50))
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
    // The shell is the process-group leader and timeout termination must
    // remove its background child as well as reap the direct child.
    for _ in 0..40 {
        if unsafe { libc::kill(child_pid, 0) } != 0 {
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
fn source_built_binary_path_uses_source_local_target_by_default() {
    let workspace = Path::new("/workspace/homeboy");

    assert_eq!(
        source_built_binary_path_for_target_dir(workspace, None),
        workspace.join("target/release/homeboy")
    );
}

#[test]
fn source_built_binary_path_uses_absolute_cargo_target_dir() {
    let workspace = Path::new("/workspace/homeboy");
    let target_dir = Path::new("/shared/cargo-target");

    assert_eq!(
        source_built_binary_path_for_target_dir(workspace, Some(target_dir.as_os_str())),
        target_dir.join("release/homeboy")
    );
}

#[test]
fn source_built_binary_path_resolves_relative_cargo_target_dir_from_workspace() {
    let workspace = Path::new("/workspace/homeboy");

    assert_eq!(
        source_built_binary_path_for_target_dir(
            workspace,
            Some(std::ffi::OsStr::new("shared/target"))
        ),
        workspace.join("shared/target/release/homeboy")
    );
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
