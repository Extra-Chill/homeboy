use super::*;
use crate::core::engine::run_dir::RunDir;
use crate::test_support::{home_env_guard, with_isolated_home};

#[test]
fn test_env_vars() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
            .expect("invocation guard");
        let env = guard.env_vars();

        let id = value_for(&env, "HOMEBOY_INVOCATION_ID");
        assert!(id.starts_with("inv-"));
        assert!(Path::new(&value_for(&env, "HOMEBOY_INVOCATION_STATE_DIR")).is_dir());
        assert!(Path::new(&value_for(&env, "HOMEBOY_INVOCATION_ARTIFACT_DIR")).is_dir());
        assert!(Path::new(&value_for(&env, "HOMEBOY_INVOCATION_TMP_DIR")).is_dir());
        assert!(value_for_optional(&env, "HOMEBOY_INVOCATION_PORT_BASE").is_none());
        assert!(value_for_optional(&env, "HOMEBOY_INVOCATION_CONTEXT_JSON").is_some());

        run_dir.cleanup();
    });
}

#[test]
fn structured_context_matches_legacy_isolated_dirs() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
            .expect("invocation guard");
        let env = guard.env_vars();
        let context: serde_json::Value =
            serde_json::from_str(&value_for(&env, "HOMEBOY_INVOCATION_CONTEXT_JSON"))
                .expect("context json");

        assert_eq!(context["id"], value_for(&env, "HOMEBOY_INVOCATION_ID"));
        assert_eq!(
            context["state_dir"],
            value_for(&env, "HOMEBOY_INVOCATION_STATE_DIR")
        );
        assert_eq!(
            context["artifact_dir"],
            value_for(&env, "HOMEBOY_INVOCATION_ARTIFACT_DIR")
        );
        assert_eq!(
            context["tmp_dir"],
            value_for(&env, "HOMEBOY_INVOCATION_TMP_DIR")
        );
        assert!(context.get("port_range").is_none());
        assert!(context.get("named_leases").is_none());

        run_dir.cleanup();
    });
}

#[test]
fn port_ranges_do_not_overlap_while_leased() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let requirements = InvocationRequirements {
            port_range_size: Some(4),
            named_leases: Vec::new(),
        };

        let first = InvocationGuard::acquire(&run_dir, &requirements).expect("first lease");
        let second = InvocationGuard::acquire(&run_dir, &requirements).expect("second lease");
        let first_base: u16 = value_for(&first.env_vars(), "HOMEBOY_INVOCATION_PORT_BASE")
            .parse()
            .expect("first base");
        let first_max: u16 = value_for(&first.env_vars(), "HOMEBOY_INVOCATION_PORT_MAX")
            .parse()
            .expect("first max");
        let second_base: u16 = value_for(&second.env_vars(), "HOMEBOY_INVOCATION_PORT_BASE")
            .parse()
            .expect("second base");
        let context: serde_json::Value = serde_json::from_str(&value_for(
            &first.env_vars(),
            "HOMEBOY_INVOCATION_CONTEXT_JSON",
        ))
        .expect("context json");

        assert!(second_base > first_max);
        assert_eq!(first_max - first_base + 1, 4);
        assert_eq!(context["port_range"]["base"], first_base);
        assert_eq!(context["port_range"]["max"], first_max);

        run_dir.cleanup();
    });
}

#[test]
fn structured_context_includes_named_leases() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let requirements = InvocationRequirements {
            port_range_size: None,
            named_leases: vec!["playground-browser-profile".to_string()],
        };

        let guard = InvocationGuard::acquire(&run_dir, &requirements).expect("lease");
        let context: serde_json::Value = serde_json::from_str(&value_for(
            &guard.env_vars(),
            "HOMEBOY_INVOCATION_CONTEXT_JSON",
        ))
        .expect("context json");

        assert_eq!(
            context["named_leases"],
            serde_json::json!(["playground-browser-profile"])
        );

        run_dir.cleanup();
    });
}

#[test]
fn named_lease_conflicts_report_holder() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let requirements = InvocationRequirements {
            port_range_size: None,
            named_leases: vec!["playground-browser-profile".to_string()],
        };

        let _first = InvocationGuard::acquire(&run_dir, &requirements).expect("first lease");
        let err = InvocationGuard::acquire(&run_dir, &requirements).expect_err("lease conflict");
        let message = err.to_string();

        assert!(message.contains("playground-browser-profile"));
        assert!(message.contains("already held"));

        run_dir.cleanup();
    });
}

#[test]
fn test_register_child_process() {
    with_isolated_home(|_| {
        let guard =
            register_child_process("inv-test", std::process::id(), None, "self".to_string())
                .expect("child record");
        assert!(guard.path.exists());

        let path = guard.path.clone();
        drop(guard);

        assert!(
            !path.exists(),
            "child record should be removed on normal exit"
        );
    });
}

#[cfg(unix)]
#[test]
fn test_cleanup_stale_child_records() {
    with_isolated_home(|_| {
        let mut child = spawn_isolated_sleep();
        let pid = child.id();
        write_test_child_record("inv-stale", 999_999, pid, Some(pid as i32));

        let cleaned = cleanup_stale_child_records().expect("cleanup stale records");

        assert_eq!(cleaned, 1);
        assert_child_exits(&mut child);
        assert!(!InvocationChildRecord::record_path("inv-stale", pid)
            .unwrap()
            .exists());
    });
}

#[cfg(unix)]
#[test]
fn test_cleanup_invocation_children() {
    with_isolated_home(|_| {
        let mut first = spawn_isolated_sleep();
        let mut second = spawn_isolated_sleep();
        let first_pid = first.id();
        let second_pid = second.id();
        write_test_child_record("inv-first", 999_999, first_pid, Some(first_pid as i32));
        write_test_child_record("inv-second", 999_999, second_pid, Some(second_pid as i32));

        let cleaned = cleanup_invocation_children("inv-first").expect("cleanup first invocation");

        assert_eq!(cleaned, 1);
        assert_child_exits(&mut first);
        assert!(pid_is_alive(second_pid as libc::pid_t));

        let _ = cleanup_invocation_children("inv-second");
        assert_child_exits(&mut second);
    });
}

fn value_for(env: &[(String, String)], key: &str) -> String {
    value_for_optional(env, key).unwrap_or_else(|| panic!("missing {key}"))
}

fn value_for_optional(env: &[(String, String)], key: &str) -> Option<String> {
    env.iter()
        .find_map(|(candidate, value)| (candidate == key).then(|| value.clone()))
}

#[cfg(unix)]
fn spawn_isolated_sleep() -> std::process::Child {
    use std::os::unix::process::CommandExt;

    let mut command = std::process::Command::new("sh");
    command.args(["-c", "sleep 30"]);
    unsafe {
        command.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    command.spawn().expect("spawn isolated sleep")
}

#[cfg(unix)]
fn write_test_child_record(invocation_id: &str, owner_pid: u32, root_pid: u32, pgid: Option<i32>) {
    let dir = InvocationChildRecord::children_dir(invocation_id).expect("child dir");
    std::fs::create_dir_all(&dir).expect("create child dir");
    let record = InvocationChildRecord {
        invocation_id: invocation_id.to_string(),
        owner_pid,
        owner_started_at: None,
        child: crate::core::engine::resource::ChildProcessIdentity {
            root_pid,
            command_label: "sleep".to_string(),
        },
        root_started_at: InvocationChildRecord::process_started_at(root_pid),
        pgid,
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    let json = serde_json::to_string_pretty(&record).expect("serialize child record");
    std::fs::write(
        InvocationChildRecord::record_path(invocation_id, root_pid).expect("record path"),
        json,
    )
    .expect("write child record");
}

#[cfg(unix)]
fn assert_child_exits(child: &mut std::process::Child) {
    for _ in 0..30 {
        if child.try_wait().expect("try wait").is_some() {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let _ = child.kill();
    panic!("child {} should have exited", child.id());
}

#[cfg(unix)]
fn pid_is_alive(pid: libc::pid_t) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

// --- issue #2311: short invocation runtime path & sockaddr_un budget -------

#[test]
fn invocation_state_dir_lives_under_runtime_root_not_run_dir() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
            .expect("invocation guard");
        let env = guard.env_vars();
        let state_dir = value_for(&env, "HOMEBOY_INVOCATION_STATE_DIR");

        let runtime_root = invocation_runtime_root().expect("runtime root");
        assert!(
            Path::new(&state_dir).starts_with(&runtime_root),
            "state dir {} should live under runtime root {}",
            state_dir,
            runtime_root.display()
        );
        assert!(
            !Path::new(&state_dir).starts_with(run_dir.path()),
            "state dir {} must not be nested under run_dir {}",
            state_dir,
            run_dir.path().display()
        );

        run_dir.cleanup();
    });
}

#[test]
fn invocation_state_dir_fits_under_sockaddr_un_budget() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
            .expect("invocation guard");
        let env = guard.env_vars();

        for key in [
            "HOMEBOY_INVOCATION_STATE_DIR",
            "HOMEBOY_INVOCATION_ARTIFACT_DIR",
            "HOMEBOY_INVOCATION_TMP_DIR",
        ] {
            let dir = value_for(&env, key);
            // budget = path bytes + 1 separator + 32-byte filename headroom
            let needed = dir.len() + 1 + SOCKET_HEADROOM_BYTES;
            assert!(
                needed <= SUN_PATH_CAPACITY,
                "{key} = {dir} needs {needed} bytes, exceeds sockaddr_un capacity \
                 {SUN_PATH_CAPACITY}"
            );
            // Headroom must be at least 32 bytes for downstream socket names.
            let headroom = SUN_PATH_CAPACITY - dir.len() - 1;
            assert!(
                headroom >= SOCKET_HEADROOM_BYTES,
                "{key} = {dir} only leaves {headroom} bytes of headroom, \
                 less than required {SOCKET_HEADROOM_BYTES}"
            );
        }

        run_dir.cleanup();
    });
}

#[test]
fn invocation_dirs_are_unique_across_concurrent_invocations() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let mut paths = std::collections::HashSet::new();
        let mut guards = Vec::new();

        for _ in 0..16 {
            let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
                .expect("invocation guard");
            let dir = value_for(&guard.env_vars(), "HOMEBOY_INVOCATION_STATE_DIR");
            assert!(paths.insert(dir.clone()), "duplicate state dir: {dir}");
            guards.push(guard);
        }

        run_dir.cleanup();
    });
}

#[test]
fn invocation_id_path_component_is_short() {
    // The short id used in path components should be ~10 chars, far below
    // the 36-char UUID we used to embed.
    let id = short_invocation_id();
    assert!(id.len() <= 12, "short id should be <= 12 chars: {id}");
    assert!(id.len() >= 8, "short id should be >= 8 chars: {id}");
}

#[test]
fn invocation_runtime_root_honors_override_env() {
    // This test sets/restores HOMEBOY_INVOCATION_RUNTIME_DIR explicitly to
    // verify the env-driven fallback selection.
    let _guard = home_env_guard();
    let prior = std::env::var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
    let dir = tempfile::tempdir().expect("tempdir");
    std::env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, dir.path());

    let root = invocation_runtime_root().expect("runtime root");
    assert_eq!(root, dir.path());

    match prior {
        Some(value) => std::env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, value),
        None => std::env::remove_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV),
    }
}

#[test]
fn enforce_path_budget_rejects_overlong_paths() {
    let mut s = String::from("/");
    while s.len() < SUN_PATH_CAPACITY {
        s.push('z');
    }
    let path = std::path::PathBuf::from(s);
    let err = enforce_path_budget(&path).expect_err("overlong path should fail");
    let message = err.to_string();
    assert!(
        message.contains("sockaddr_un"),
        "error should mention sockaddr_un: {message}"
    );
    assert!(
        message.contains(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV),
        "error should suggest the override env: {message}"
    );
}

#[test]
fn invocation_dir_permission_error_names_runtime_root_and_remediation() {
    let runtime_root = std::path::Path::new("/tmp/hb-owned-by-another-user");
    let dir = runtime_root.join("abc1234567");
    let err = invocation_dir_create_error(
        &dir,
        runtime_root,
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, "permission denied"),
    );
    let message = err.to_string();

    assert!(message.contains("Homeboy cannot create invocation dir"));
    assert!(message.contains("/tmp/hb-owned-by-another-user"));
    assert!(message.contains("current user"));
    assert!(message.contains(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV));
    assert!(message.contains("remove or chown"));
}

#[test]
fn invocation_drop_cleans_up_root_directory() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let (state_dir, artifact_dir, tmp_dir) = {
            let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
                .expect("invocation guard");
            let env = guard.env_vars();
            (
                std::path::PathBuf::from(value_for(&env, "HOMEBOY_INVOCATION_STATE_DIR")),
                std::path::PathBuf::from(value_for(&env, "HOMEBOY_INVOCATION_ARTIFACT_DIR")),
                std::path::PathBuf::from(value_for(&env, "HOMEBOY_INVOCATION_TMP_DIR")),
            )
        };
        for path in [&state_dir, &artifact_dir, &tmp_dir] {
            assert!(
                !path.exists(),
                "invocation dir should be removed on Drop: {}",
                path.display()
            );
        }

        run_dir.cleanup();
    });
}

#[test]
fn preserve_artifacts_emits_manifest_for_invocation_files() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
            .expect("invocation guard");
        let artifact = std::path::PathBuf::from(value_for(
            &guard.env_vars(),
            "HOMEBOY_INVOCATION_ARTIFACT_DIR",
        ));
        std::fs::create_dir_all(artifact.join("nested")).expect("mkdir artifact nested");
        std::fs::write(artifact.join("nested/result.json"), r#"{"ok":true}"#)
            .expect("write artifact");

        let preserved = guard
            .preserve_artifacts(&run_dir)
            .expect("preserve artifacts")
            .expect("artifact path");
        let manifest = homeboy::core::artifact_manifest::read_manifest_from_root(&preserved)
            .expect("read manifest");

        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(manifest.artifacts[0].path, "nested/result.json");
        assert_eq!(manifest.artifacts[0].kind, "file");
        assert_eq!(
            manifest.artifacts[0].content_type.as_deref(),
            Some("application/json")
        );

        run_dir.cleanup();
    });
}

#[test]
fn preserve_artifacts_normalizes_existing_manifest() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
            .expect("invocation guard");
        let artifact = std::path::PathBuf::from(value_for(
            &guard.env_vars(),
            "HOMEBOY_INVOCATION_ARTIFACT_DIR",
        ));
        std::fs::write(artifact.join("declared.log"), "hello").expect("write artifact");
        let manifest = homeboy::core::artifact_manifest::ArtifactManifest::new(vec![
            homeboy::core::artifact_manifest::ArtifactManifestEntry {
                id: None,
                path: "declared.log".to_string(),
                kind: "runner-log".to_string(),
                role: None,
                label: Some("Runner log".to_string()),
                semantic_key: None,
                content_type: None,
                provenance: None,
                viewer: None,
                public_url: None,
                public_url_state: None,
                size_bytes: None,
                sha256: None,
                redaction: Some(homeboy::core::artifact_manifest::ArtifactRedactionState::Raw),
                metadata: serde_json::json!({ "source": "test" }),
            },
        ]);
        homeboy::core::artifact_manifest::write_manifest_to_root(&artifact, &manifest)
            .expect("write source manifest");

        let preserved = guard
            .preserve_artifacts(&run_dir)
            .expect("preserve artifacts")
            .expect("artifact path");
        let manifest = homeboy::core::artifact_manifest::read_manifest_from_root(&preserved)
            .expect("read manifest");

        assert_eq!(manifest.artifacts.len(), 1);
        assert_eq!(manifest.artifacts[0].path, "declared.log");
        assert_eq!(manifest.artifacts[0].kind, "runner-log");
        assert_eq!(manifest.artifacts[0].size_bytes, Some(5));
        assert!(manifest.artifacts[0].sha256.is_some());

        run_dir.cleanup();
    });
}

// --- followup: STATE_DIR is the leaf the workload owns ---------------------

#[test]
fn state_dir_is_the_invocation_leaf_with_artifact_and_tmp_as_siblings() {
    with_isolated_home(|_| {
        let run_dir = RunDir::create().expect("run dir");
        let guard = InvocationGuard::acquire(&run_dir, &InvocationRequirements::default())
            .expect("invocation guard");
        let env = guard.env_vars();

        let state = std::path::PathBuf::from(value_for(&env, "HOMEBOY_INVOCATION_STATE_DIR"));
        let artifact = std::path::PathBuf::from(value_for(&env, "HOMEBOY_INVOCATION_ARTIFACT_DIR"));
        let tmp = std::path::PathBuf::from(value_for(&env, "HOMEBOY_INVOCATION_TMP_DIR"));

        // STATE_DIR is the invocation leaf — workloads bind sockets directly
        // under it without any extra Homeboy-injected segment. ARTIFACT_DIR
        // and TMP_DIR live alongside as siblings under the runtime root, so
        // they cannot collide with workload-created subdirs under STATE_DIR.
        assert_eq!(
            state.parent(),
            artifact.parent(),
            "siblings under same root"
        );
        assert_eq!(state.parent(), tmp.parent(), "siblings under same root");

        // Distinct leaves (no two env vars pointing at the same dir).
        assert_ne!(state, artifact);
        assert_ne!(state, tmp);
        assert_ne!(artifact, tmp);

        // No `s/a/t` subdir layer — STATE_DIR's basename is the short id
        // (10 hex chars), not `s`.
        let state_basename = state
            .file_name()
            .and_then(|n| n.to_str())
            .expect("state basename");
        assert_eq!(
            state_basename.len(),
            10,
            "STATE_DIR basename should be the 10-char short id, not a subdir: {state_basename}"
        );

        run_dir.cleanup();
    });
}

#[test]
fn realistic_socket_path_fits_under_sockaddr_un_on_default_platform_root() {
    // Regression for #2311 follow-up: on macOS the default $TMPDIR is
    // ~/var/folders/<14>/T/ ≈ 50 bytes, so anchoring to /tmp instead is
    // what makes the budget work. Build the longest realistic socket path
    // the Studio site-build workload produces and assert it fits under
    // sockaddr_un on the default platform root (no override env).

    // Save and clear the runtime override so the platform detection ladder
    // is exercised end-to-end.
    let _guard = home_env_guard();
    let prior = std::env::var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
    std::env::remove_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV);

    let runtime_root = invocation_runtime_root().expect("platform runtime root");

    // Worst-case STATE_DIR: runtime_root + '/' + 10-char short id.
    let state_dir = runtime_root.join("a1b2c3d4e5");

    // Realistic workload-relative socket suffix: a 32-char workload id
    // plus the canonical `daemon/daemon.sock` filename. This is what the
    // Studio site-build workload + similar rigs append under STATE_DIR.
    let workload_relative = "studio-agent-site-build-restaurant/daemon/daemon.sock";
    assert!(
        workload_relative.len() >= 32,
        "regression test should use a realistic 32+ byte socket suffix"
    );

    let full_socket_path = state_dir.join(workload_relative);
    let full_len = full_socket_path.to_string_lossy().len();

    assert!(
        full_len <= SUN_PATH_CAPACITY,
        "realistic socket path is {full_len} bytes, exceeds platform sun_path \
         capacity {SUN_PATH_CAPACITY}: {} (runtime_root = {})",
        full_socket_path.display(),
        runtime_root.display()
    );

    // STATE_DIR alone must also satisfy the homeboy ≥48-byte (macOS) /
    // ≥32-byte (Linux) headroom contract under default platform root.
    enforce_path_budget(&state_dir).expect("default platform root meets budget contract");

    match prior {
        Some(value) => std::env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, value),
        None => std::env::remove_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV),
    }
}

#[test]
fn studio_site_build_socket_path_fits_macos_sockaddr_un() {
    // Specific regression for the failure observed in #2311 follow-up:
    // Studio's daemon binds to
    //   $HOMEBOY_INVOCATION_STATE_DIR/studio-agent-site-build/daemon/daemon.sock
    // and previously got EINVAL on macOS because the prefix was already
    // ~64 bytes by the time the workload appended its 41-byte suffix.
    let _guard = home_env_guard();
    let prior = std::env::var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV).ok();
    std::env::remove_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV);

    let runtime_root = invocation_runtime_root().expect("platform runtime root");
    let state_dir = runtime_root.join("0123456789");
    let socket = state_dir.join("studio-agent-site-build/daemon/daemon.sock");
    let socket_len = socket.to_string_lossy().len();

    // macOS sockaddr_un sun_path = 104; Linux = 108. Test must hold on
    // either platform's actual default root.
    assert!(
        socket_len <= SUN_PATH_CAPACITY,
        "Studio site-build socket path is {socket_len} bytes, exceeds {SUN_PATH_CAPACITY}: {}",
        socket.display()
    );

    match prior {
        Some(value) => std::env::set_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV, value),
        None => std::env::remove_var(HOMEBOY_INVOCATION_RUNTIME_DIR_ENV),
    }
}
