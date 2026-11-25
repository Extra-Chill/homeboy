#![cfg(test)]

use super::*;

#[test]
fn bound_captured_stream_keeps_the_trailing_tail_within_its_limit() {
    for (source, limit, text, seen, retained, truncated) in [
        (&b"boom"[..], 1024, "boom", 4, 4, false),
        (&b"head-noise-TAIL"[..], 4, "TAIL", 15, 4, true),
    ] {
        let (captured, capture) = bound_captured_stream(source, limit);

        assert_eq!(captured, text);
        assert_eq!(capture.limit_bytes, limit);
        assert_eq!(capture.seen_bytes, seen);
        assert_eq!(capture.retained_bytes, retained);
        assert_eq!(capture.truncated, truncated);
    }
}

#[test]
fn annotate_truncation_notes_dropped_bytes_only_when_truncated() {
    let (text, capture) = bound_captured_stream(b"boom", 1024);
    assert_eq!(annotate_truncation(&text, &capture), "boom");

    let (text, capture) = bound_captured_stream(&[b'x'; 16], 4);
    let annotated = annotate_truncation(&text, &capture);
    assert!(annotated.starts_with("xxxx"));
    assert!(annotated.contains("output truncated"));
    assert!(annotated.contains("retained 4 of 16 bytes"));
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

#[cfg(unix)]
#[test]
fn candidate_schema_query_admits_live_job_only_for_unchanged_schema() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let store = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            homeboy_core::paths::observation_db().unwrap(),
        )
        .unwrap();
        let job = store.create("controller.workflow");
        let candidate = home.path().join("candidate");
        let current = homeboy_core::observation::store::CURRENT_SCHEMA_VERSION;
        // An older release cannot answer the query; its schema is unknown.
        for schema in [Some(current), Some(current + 1), None] {
            let script = match schema {
                Some(schema) => format!(
                    "#!/bin/sh\n[ \"$1\" = --observation-schema-version ] || exit 2\nprintf '{schema}\\n'\n"
                ),
                None => "#!/bin/sh\necho 'unexpected argument' >&2\nexit 2\n".to_string(),
            };
            homeboy_test_support::write_executable(&candidate, script, 0o700).unwrap();
            let admission = run_verified_target_admission(&candidate);
            if schema == Some(current) {
                admission.expect("live work must not block same-schema replacement");
            } else {
                let error = admission.unwrap_err();
                assert_eq!(error.details["kind"], "schema_change");
                assert!(error.message.contains(&job.id.to_string()));
                assert!(error.message.contains("no binaries were replaced"));
            }
        }
    });
}

#[test]
fn unstructured_upgrade_cause_is_bounded_without_losing_full_evidence() {
    let detail = format!("installer failed: {}", "é".repeat(5000));
    let error = upgrade_failure_error(InstallMethod::Binary, &detail, None);
    assert!(error
        .message
        .starts_with("binary upgrade failed: installer failed:"));
    assert!(error.message.contains("cause truncated"));
    assert!(error.details["error"].as_str().unwrap().contains(&detail));
    assert_eq!(error.details["kind"], "upgrade_command_failed");
}

#[test]
fn command_output_with_timeout_captures_child_output() {
    let mut command = Command::new("sh");
    command.args(["-c", "printf 'homeboy 0.247.5'; printf 'warn' >&2"]);

    let output = command_output_with_timeout(&mut command, Duration::from_secs(5), "test command")
        .expect("command output");

    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout), "homeboy 0.247.5");
    assert_eq!(String::from_utf8_lossy(&output.stderr), "warn");
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
    homeboy_core::test_support::with_isolated_home(|_| {
        let store = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            homeboy_core::paths::observation_db().unwrap(),
        )
        .unwrap();
        let _job = store.create("controller.workflow");
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(
            workspace.path().join("Cargo.toml"),
            "[package]\nname = \"homeboy\"\nversion = \"0.352.0\"\n",
        )
        .expect("manifest");
        let installed = workspace.path().join("installed-homeboy");
        let candidate = workspace.path().join("staged-homeboy");
        homeboy_test_support::write_executable(
            &installed,
            "#!/bin/sh\nif [ \"$1\" = self ]; then exit 64; fi\nprintf 'homeboy 0.351.0+old\\n'\n",
            0o755,
        )
        .expect("installed controller");
        homeboy_test_support::write_executable(&candidate, format!(
        "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'homeboy 0.352.0+new\\n'; exit 0; fi\nprintf '{}\\n'\n",
        homeboy_core::observation::store::CURRENT_SCHEMA_VERSION
    ), 0o755).expect("candidate");

        let candidate_identity =
            verify_source_candidate_target_admission(workspace.path(), &candidate, None)
                .expect("new candidate, not the old controller, admits replacement");
        assert_eq!(candidate_identity.version.as_deref(), Some("0.352.0"));
        assert_eq!(
            candidate_identity.build_identity.as_deref(),
            Some("homeboy 0.352.0+new")
        );
        homeboy_test_support::write_executable(&candidate, "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'homeboy 0.352.0+new\\n'; exit 0; fi\nexit 1\n", 0o755)
    .expect("failing staged candidate");

        let error = verify_source_candidate_target_admission(workspace.path(), &candidate, None)
            .expect_err("candidate admission failure blocks promotion");

        assert!(error
            .message
            .contains("observation schema change to an unknown version"));
        assert_eq!(
            std::fs::read_to_string(&installed).expect("installed bytes"),
            "#!/bin/sh\nif [ \"$1\" = self ]; then exit 64; fi\nprintf 'homeboy 0.351.0+old\\n'\n"
        );
    });
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
    assert!(
        source_promotion_is_superseded(false, Some(&newer_active), workspace.path())
            .expect("readable identity decides admission")
    );
    let older_active = homeboy_core::build_identity::BuildIdentity {
        display: "homeboy 1.2.2+old".to_string(),
        version: "1.2.2".to_string(),
        git_commit: Some("old".to_string()),
        git_dirty: Some(false),
    };
    // In the reverse completion order, the newer candidate remains eligible.
    assert!(
        !source_promotion_is_superseded(false, Some(&older_active), workspace.path())
            .expect("readable identity decides admission")
    );
    assert!(
        !source_promotion_is_superseded(true, Some(&newer_active), workspace.path())
            .expect("force bypasses supersession")
    );
}

#[test]
fn source_completion_fails_closed_when_installed_identity_disappears() {
    let workspace = tempfile::tempdir().expect("workspace");
    std::fs::write(
        workspace.path().join("Cargo.toml"),
        "[package]\nname = \"homeboy\"\nversion = \"1.2.3\"\n",
    )
    .expect("candidate manifest");

    let error = source_promotion_is_superseded(false, None, workspace.path())
        .expect_err("missing final identity blocks source replacement");
    assert!(error
        .message
        .contains("cannot revalidate the installed controller"));
    assert!(
        !source_promotion_is_superseded(true, None, workspace.path())
            .expect("force explicitly permits missing identity")
    );
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
fn source_upgrade_timeout_preserves_typed_build_deadline_evidence() {
    let error = source_upgrade_timeout_error(Duration::from_secs(1));

    assert_eq!(
        error.code,
        homeboy_core::error::ErrorCode::InternalUnexpected
    );
    assert_eq!(
        error.details["kind"],
        serde_json::json!("source_build_deadline_exceeded")
    );
    assert_eq!(
        error.details["phase"],
        serde_json::json!("building_candidate")
    );
    assert_eq!(error.details["budget_ms"], serde_json::json!(1_000));
    assert_eq!(error.details["retryable"], serde_json::json!(true));
    assert_eq!(error.retryable, Some(true));
}

#[cfg(unix)]
#[test]
fn installer_completion_reaps_background_process_group() {
    let workspace = tempfile::tempdir().expect("workspace");
    let pid_file = workspace.path().join("child.pid");
    let shell = format!(
        "sleep 30 & echo $! > {}; printf installed",
        quote_path(&pid_file.display().to_string())
    );
    let output = supervise_installer_shell_command(
        &shell,
        Duration::from_secs(1),
        "test installer",
        |_| Ok(()),
        |_| {},
    )
    .expect("installer command completes");
    assert!(output.status.success());
    assert_eq!(output.stdout, b"installed");

    let child_pid = std::fs::read_to_string(&pid_file)
        .expect("background child pid")
        .trim()
        .parse::<u32>()
        .expect("numeric pid");
    assert!(
        !homeboy_core::process::pid_is_running(child_pid),
        "successful installer left a runnable descendant"
    );
}

#[cfg(unix)]
#[test]
fn installer_timeout_terminates_and_reaps_the_process_tree() {
    let workspace = tempfile::tempdir().expect("workspace");
    let pid_file = workspace.path().join("child.pid");
    let shell = format!(
        "sleep 30 & echo $! > {}; wait",
        quote_path(&pid_file.display().to_string())
    );
    let error = supervise_installer_shell_command(
        &shell,
        Duration::from_millis(50),
        "test installer",
        |_| Ok(()),
        |_| {},
    )
    .expect_err("installer command times out");
    assert!(error
        .details
        .to_string()
        .to_lowercase()
        .contains("timed out"));
    let child_pid = std::fs::read_to_string(&pid_file)
        .expect("background child pid")
        .trim()
        .parse::<u32>()
        .expect("numeric pid");
    assert!(
        !homeboy_core::process::pid_is_running(child_pid),
        "timed-out installer left a runnable descendant"
    );
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture for controller-death supervision"]
fn installer_controller_death_fixture() {
    let pid_file = std::env::var("HOMEBOY_INSTALLER_PID_FILE").expect("pid file");
    let release_file = std::env::var("HOMEBOY_INSTALLER_RELEASE_FILE").expect("release file");
    let target = std::env::var("HOMEBOY_INSTALLER_TARGET").expect("target");
    let shell = if std::env::var_os("HOMEBOY_INSTALLER_ESCAPE_SETSID").is_some() {
        let test_binary = std::env::current_exe().expect("test executable");
        format!(
            "{} --ignored --exact upgrade::execution::tests::part_a::installer_setsid_descendant_fixture --nocapture & wait",
            quote_path(&test_binary.display().to_string()),
        )
    } else {
        format!(
            "echo $$ > {}; n=0; while [ ! -e {} ] && [ $n -lt 6000 ]; do sleep 0.02; n=$((n+1)); done; printf stale > {}",
            quote_path(&pid_file),
            quote_path(&release_file),
            quote_path(&target),
        )
    };
    if std::env::var_os("HOMEBOY_INSTALLER_USE_PROMOTION_LEASE").is_some() {
        let lease = homeboy_core::runtime_promotion::acquire(
            "controller-death fixture",
            "active controller",
        )
        .expect("acquire fixture mutation lease");
        run_installer_shell_command(
            &shell,
            Some(&lease),
            Duration::from_secs(30),
            "controller-death fixture",
            |_| {},
        )
        .expect("fixture installer completes only when not killed");
    } else {
        supervise_installer_shell_command(
            &shell,
            Duration::from_secs(30),
            "controller-death fixture",
            |_| Ok(()),
            |_| {},
        )
        .expect("fixture installer completes only when not killed");
    }
}

#[cfg(unix)]
#[test]
#[ignore = "subprocess fixture for escaped installer descendant"]
fn installer_setsid_descendant_fixture() {
    let target = std::env::var("HOMEBOY_INSTALLER_TARGET").expect("target");
    assert_ne!(unsafe { libc::setsid() }, -1, "detach installer descendant");
    if let Some(pid_file) = std::env::var_os("HOMEBOY_INSTALLER_PID_FILE") {
        std::fs::write(pid_file, std::process::id().to_string()).expect("publish escaped PID");
    }
    if let Some(release_file) = std::env::var_os("HOMEBOY_INSTALLER_RELEASE_FILE") {
        let release_file = PathBuf::from(release_file);
        while !release_file.exists() {
            std::thread::sleep(Duration::from_millis(20));
        }
    } else {
        std::thread::sleep(Duration::from_millis(
            std::env::var("HOMEBOY_INSTALLER_MUTATION_DELAY_MS")
                .ok()
                .and_then(|value| value.parse().ok())
                .unwrap_or(250),
        ));
    }
    std::fs::write(target, b"stale").expect("escaped descendant mutation");
    std::thread::sleep(Duration::from_secs(30));
}

#[cfg(target_os = "linux")]
#[test]
#[ignore = "subprocess fixture for a rapidly reparented installer descendant"]
fn installer_fast_double_fork_fixture() {
    use std::ffi::CString;

    let target = std::env::var("HOMEBOY_INSTALLER_TARGET").expect("target");
    let pid_file = std::env::var("HOMEBOY_INSTALLER_PID_FILE").expect("pid file");
    let script = CString::new(format!(
        "printf '%s' $$ > {}; sleep 0.25; printf stale > {}; sleep 30",
        quote_path(&pid_file),
        quote_path(&target),
    ))
    .expect("fixture script");
    let shell = c"/bin/sh";
    let shell_name = c"sh";
    let command_flag = c"-c";

    let intermediate = unsafe { libc::fork() };
    assert_ne!(intermediate, -1, "fork intermediate");
    if intermediate == 0 {
        let daemon = unsafe { libc::fork() };
        if daemon < 0 {
            unsafe { libc::_exit(2) };
        }
        if daemon > 0 {
            unsafe { libc::_exit(0) };
        }
        if unsafe { libc::setsid() } == -1 {
            unsafe { libc::_exit(3) };
        }
        let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
        for fd in 0..if max > 0 { max as i32 } else { 1024 } {
            unsafe {
                libc::close(fd);
            }
        }
        unsafe {
            libc::execl(
                shell.as_ptr(),
                shell_name.as_ptr(),
                command_flag.as_ptr(),
                script.as_ptr(),
                std::ptr::null::<libc::c_char>(),
            );
            libc::_exit(4);
        }
    }
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(intermediate, &mut status, 0) },
        intermediate,
        "reap intermediate"
    );
    // Keep the root alive until the post-exec descendant exists to be contained.
    wait_for_installer_pid(
        Path::new(&pid_file),
        Instant::now() + Duration::from_secs(1),
    );
}

#[cfg(target_os = "linux")]
#[test]
fn installer_completion_contains_fast_double_fork_after_inherited_descriptors_close() {
    let workspace = tempfile::tempdir().expect("workspace");
    let target = workspace.path().join("homeboy");
    let pid_file = workspace.path().join("daemon.pid");
    std::fs::write(&target, b"original").expect("write target");
    let test_binary = std::env::current_exe().expect("test executable");
    let shell = format!(
        "{} --ignored --exact upgrade::execution::tests::part_a::installer_fast_double_fork_fixture --nocapture",
        quote_path(&test_binary.display().to_string()),
    );

    let output = supervise_installer_shell_command(
        &shell,
        Duration::from_secs(2),
        "fast double-fork installer",
        |_| Ok(()),
        |command| {
            command
                .env("HOMEBOY_INSTALLER_TARGET", &target)
                .env("HOMEBOY_INSTALLER_PID_FILE", &pid_file);
        },
    )
    .expect("root completion contains reparented descendant");
    assert!(output.status.success());
    std::thread::sleep(Duration::from_millis(350));
    assert_eq!(
        std::fs::read(&target).expect("read target"),
        b"original",
        "descriptor-closing daemon mutated after root completion"
    );
    let daemon_pid = std::fs::read_to_string(&pid_file)
        .expect("daemon pid")
        .parse::<u32>()
        .expect("numeric daemon pid");
    assert!(
        !homeboy_core::process::pid_is_running(daemon_pid),
        "successor could start while descriptor-closing daemon remained runnable"
    );
}

#[cfg(unix)]
#[test]
fn installer_timeout_kills_setsid_descendant_before_delayed_mutation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let test_binary = std::env::current_exe().expect("test executable");
    let shell = format!(
        "{} --ignored --exact upgrade::execution::tests::part_a::installer_setsid_descendant_fixture --nocapture & wait",
        quote_path(&test_binary.display().to_string()),
    );
    for attempt in 0..3 {
        let target = workspace.path().join(format!("homeboy-{attempt}"));
        let pid_file = workspace.path().join(format!("descendant-{attempt}.pid"));
        std::fs::write(&target, b"original").expect("write target");
        supervise_installer_shell_command(
            &shell,
            Duration::from_millis(75),
            "setsid descendant installer",
            |_| Ok(()),
            |command| {
                command
                    .env("HOMEBOY_INSTALLER_TARGET", &target)
                    .env("HOMEBOY_INSTALLER_PID_FILE", &pid_file);
            },
        )
        .expect_err("installer with escaped descendant times out");
        let descendant_pid =
            wait_for_installer_pid(&pid_file, Instant::now() + Duration::from_secs(1));
        assert_eq!(
            std::fs::read(&target).expect("read target immediately after timeout"),
            b"original",
            "setsid descendant mutated before timeout cleanup returned on attempt {attempt}"
        );
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(
            std::fs::read(&target).expect("read target after delayed mutation window"),
            b"original",
            "setsid descendant mutated after installer cleanup on attempt {attempt}"
        );
        assert!(
            !homeboy_core::process::pid_is_running(descendant_pid),
            "setsid descendant {descendant_pid} remained runnable on attempt {attempt}"
        );
    }
}

#[cfg(unix)]
#[test]
fn source_timeout_kills_setsid_descendant_before_staged_validation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let target = workspace.path().join("built-candidate");
    std::fs::write(&target, b"original").expect("write staged target");
    let test_binary = std::env::current_exe().expect("test executable");
    let command = format!(
        "HOMEBOY_INSTALLER_TARGET={} HOMEBOY_INSTALLER_MUTATION_DELAY_MS=1000 {} --ignored --exact upgrade::execution::tests::part_a::installer_setsid_descendant_fixture --nocapture & wait",
        quote_path(&target.display().to_string()),
        quote_path(&test_binary.display().to_string()),
    );

    run_source_upgrade_command(&command, workspace.path(), Duration::from_millis(250), None)
        .expect_err("source command with escaped descendant times out");
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(
        std::fs::read(&target).expect("read staged target"),
        b"original",
        "source descendant mutated after guarded cleanup"
    );
}

#[cfg(unix)]
fn wait_for_installer_pid(path: &Path, deadline: Instant) -> u32 {
    loop {
        if let Ok(contents) = std::fs::read_to_string(path) {
            if let Ok(pid) = contents.trim().parse::<u32>() {
                return pid;
            }
        }
        assert!(
            Instant::now() < deadline,
            "installer fixture did not publish its PID"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(unix)]
#[test]
fn controller_death_cannot_leave_an_installer_to_mutate_after_successor_handoff() {
    homeboy_core::test_support::with_isolated_home(|_| {
        let workspace = tempfile::tempdir().expect("workspace");
        let pid_file = workspace.path().join("installer.pid");
        let release_file = workspace.path().join("release");
        let target = workspace.path().join("homeboy");
        let mut controller = Command::new(std::env::current_exe().expect("test executable"));
        controller
            .args([
                "--ignored",
                "--exact",
                "upgrade::execution::tests::part_a::installer_controller_death_fixture",
                "--nocapture",
            ])
            .env("HOMEBOY_INSTALLER_PID_FILE", &pid_file)
            .env("HOMEBOY_INSTALLER_RELEASE_FILE", &release_file)
            .env("HOMEBOY_INSTALLER_TARGET", &target)
            .env("HOMEBOY_INSTALLER_USE_PROMOTION_LEASE", "1")
            .env("HOMEBOY_INSTALLER_ESCAPE_SETSID", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let mut controller = controller.spawn().expect("spawn controller fixture");

        let installer_pid =
            wait_for_installer_pid(&pid_file, Instant::now() + Duration::from_secs(5));
        controller.kill().expect("kill old controller");
        controller.wait().expect("reap old controller");

        // Race immediately. The inherited admission fence, not a PID polling
        // barrier in this test, must delay successor mutation until cleanup has
        // synchronously made the old installer non-runnable.
        let successor = homeboy_core::runtime_promotion::acquire_waiting_for_compatible(
            "successor controller",
            "active controller",
            Duration::from_secs(5),
            |_| {},
        )
        .expect("successor acquires only after installer cleanup");
        assert!(
            !homeboy_core::process::pid_is_running(installer_pid),
            "successor acquired while the old installer remained runnable"
        );
        std::fs::write(&target, b"successor").expect("successor mutation");
        drop(successor);
        std::fs::write(&release_file, b"release stale installer").expect("release barrier");
        std::thread::sleep(Duration::from_millis(100));
        assert_eq!(
            std::fs::read(&target).expect("read installed bytes"),
            b"successor",
            "orphaned installer mutated after successor handoff"
        );
    });
}

#[cfg(unix)]
#[test]
fn controller_death_before_guard_attachment_cannot_start_installer_mutation() {
    let workspace = tempfile::tempdir().expect("workspace");
    let pid_file = workspace.path().join("installer.pid");
    let ready_file = workspace.path().join("spawned-before-attach");
    let release_file = workspace.path().join("release");
    let target = workspace.path().join("homeboy");
    let mut controller = Command::new(std::env::current_exe().expect("test executable"));
    controller
        .args([
            "--ignored",
            "--exact",
            "upgrade::execution::tests::part_a::installer_controller_death_fixture",
            "--nocapture",
        ])
        .env("HOMEBOY_INSTALLER_PID_FILE", &pid_file)
        .env("HOMEBOY_INSTALLER_GATE_PID_FILE", &pid_file)
        .env("HOMEBOY_INSTALLER_BEFORE_ATTACH_READY", &ready_file)
        .env("HOMEBOY_INSTALLER_RELEASE_FILE", &release_file)
        .env("HOMEBOY_INSTALLER_TARGET", &target)
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    let mut controller = controller.spawn().expect("spawn controller fixture");

    let deadline = Instant::now() + Duration::from_secs(5);
    while !ready_file.exists() || !pid_file.exists() {
        assert!(
            Instant::now() < deadline,
            "pre-attach fixture did not start"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let installer_pid = wait_for_installer_pid(&pid_file, deadline);
    controller
        .kill()
        .expect("kill controller before guard attachment");
    controller.wait().expect("reap controller fixture");

    let deadline = Instant::now() + Duration::from_secs(5);
    while homeboy_core::process::pid_is_running(installer_pid) {
        assert!(
            Instant::now() < deadline,
            "start-gated installer survived its controller"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::write(&target, b"successor").expect("successor mutation");
    std::fs::write(&release_file, b"release").expect("release stale mutation barrier");
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        std::fs::read(&target).expect("read successor bytes"),
        b"successor"
    );
}

/// Verification passes only when the active binary proves the upgrade landed:
/// a newer version, or (when forced) a same-version binary whose identity
/// changed. Source builds must prove a new build identity; binary installs
/// may reinstall the same release when forced.
#[test]
fn upgrade_verification_requires_a_proven_new_binary() {
    use InstallMethod::{Binary, Source};
    for (method, force, active, previous_identity, new_identity, verified) in [
        (Binary, false, Some("0.158.0"), None, None, true),
        (
            Binary,
            false,
            Some("0.157.1"),
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
            false,
        ),
        (
            Binary,
            true,
            Some("0.157.1"),
            Some("commit same, dirty=false"),
            Some("commit same, dirty=false"),
            true,
        ),
        (
            Source,
            false,
            Some("0.157.1"),
            Some("homeboy 0.157.1+old"),
            Some("homeboy 0.157.1+old"),
            false,
        ),
        (
            Source,
            true,
            Some("0.157.1"),
            Some("commit same, dirty=false"),
            Some("commit same, dirty=false"),
            false,
        ),
        (
            Source,
            true,
            Some("0.157.1"),
            Some("homeboy 0.157.1+old"),
            Some("homeboy 0.157.1+new"),
            true,
        ),
        (
            Source,
            true,
            Some("0.157.1"),
            None,
            Some("homeboy 0.157.1+new"),
            true,
        ),
        (
            Source,
            true,
            None,
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
            false,
        ),
    ] {
        assert_eq!(
            upgrade_verification_result(
                method,
                force,
                "0.157.1",
                active,
                previous_identity,
                new_identity,
            ),
            verified,
            "{method:?} force={force} active={active:?} {previous_identity:?} -> {new_identity:?}"
        );
    }
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
fn parses_homeboy_version_output_with_build_identity() {
    for (output, version, identity) in [
        ("homeboy 0.158.0", "0.158.0", "homeboy 0.158.0"),
        (
            "homeboy 0.158.0+abc123-dirty",
            "0.158.0",
            "homeboy 0.158.0+abc123-dirty",
        ),
    ] {
        let info = parse_cli_version_info(output);

        assert_eq!(info.version.as_deref(), Some(version));
        assert_eq!(info.build_identity.as_deref(), Some(identity));
        assert_eq!(parse_cli_version_output(output).as_deref(), Some(version));
    }
}

/// Read-back retry is bounded: it retries transient unreadable or stale
/// reads (#3463), sleeps only between attempts, terminates after the
/// configured attempts on a persistent mismatch (#9686), and reports the last
/// version it saw.
#[test]
fn verify_retry_is_bounded_and_reports_the_last_read() {
    let info = |version: &str, identity: Option<&str>| {
        Some(ActiveBinaryInfo {
            version: Some(version.to_string()),
            build_identity: identity.map(str::to_string),
        })
    };
    let new = || info("0.220.3", None);
    let old = || info("0.220.0", None);
    let same_identity = || info("0.220.0", Some("homeboy 0.220.0+old"));
    for (method, force, previous_identity, attempts, sequence, success, reads, last) in [
        (
            InstallMethod::Binary,
            false,
            None,
            5,
            vec![new()],
            true,
            1,
            Some("0.220.3"),
        ),
        (
            InstallMethod::Binary,
            false,
            None,
            5,
            vec![None, new()],
            true,
            2,
            Some("0.220.3"),
        ),
        (
            InstallMethod::Binary,
            false,
            None,
            5,
            vec![old(), new()],
            true,
            2,
            Some("0.220.3"),
        ),
        (
            InstallMethod::Binary,
            false,
            None,
            3,
            vec![None, None, None],
            false,
            3,
            None,
        ),
        (
            InstallMethod::Binary,
            false,
            None,
            2,
            vec![old(), old()],
            false,
            2,
            Some("0.220.0"),
        ),
        (
            InstallMethod::Source,
            true,
            Some("homeboy 0.220.0+old"),
            4,
            vec![
                same_identity(),
                same_identity(),
                same_identity(),
                same_identity(),
            ],
            false,
            4,
            Some("0.220.0"),
        ),
    ] {
        let sequence = std::cell::RefCell::new(sequence);
        let mut read_count = 0u32;
        let mut sleeps = 0u32;

        let (verified, active) = verify_upgrade_with_retry(
            method,
            force,
            "0.220.0",
            previous_identity,
            attempts,
            std::time::Duration::from_millis(0),
            || {
                read_count += 1;
                sequence.borrow_mut().remove(0)
            },
            |_| sleeps += 1,
        );

        assert_eq!(verified, success);
        assert_eq!(read_count, reads);
        assert_eq!(
            sleeps,
            reads - 1,
            "sleeps between attempts, never after the last"
        );
        assert_eq!(active.and_then(|info| info.version).as_deref(), last);
    }
}

#[cfg(unix)]
#[test]
fn installer_start_gate_supports_high_pipe_descriptors_and_waits_for_release() {
    let mut gate = InstallerStartGate::new().unwrap();
    let high_fd = unsafe { libc::fcntl(gate.read_fd, libc::F_DUPFD, 20) };
    assert!(high_fd >= 20);
    unsafe {
        libc::close(gate.read_fd);
    }
    gate.read_fd = high_fd;
    let output = tempfile::tempdir().unwrap();
    let marker = output.path().join("installer-ran");
    let mut child = gate
        .command("touch \"$INSTALLER_TEST_MARKER\"")
        .env("INSTALLER_TEST_MARKER", &marker)
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !marker.exists(),
        "installer is fenced before admission release"
    );
    assert!(child.try_wait().unwrap().is_none());
    gate.release().unwrap();
    assert!(child.wait().unwrap().success());
    assert!(marker.exists());
}
