//! Service supervisor tests for `src/core/rig/service.rs`.
//!
//! The process lifecycle (spawn / SIGTERM / SIGKILL) is validated manually
//! in the end-to-end smoke described in #1468. Unit scope here covers the
//! pure types and status-enum ergonomics that back the runner's reporting.

use crate::service::ServiceStatus;

#[cfg(unix)]
mod lifecycle {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use crate::service::{self, ServiceStatus};
    use crate::spec::{DiscoverSpec, RigSpec, ServiceKind, ServiceSpec};
    use crate::state::{RigState, ServiceState};
    use homeboy_core::test_support::with_isolated_home;

    fn command_rig(id: &str, command: &str, cwd: Option<String>) -> RigSpec {
        let mut services = HashMap::new();
        services.insert(
            "cmd".to_string(),
            ServiceSpec {
                kind: ServiceKind::Command,
                cwd,
                port: None,
                command: Some(command.to_string()),
                env: HashMap::new(),
                health: None,
                discover: None,
            },
        );
        RigSpec {
            id: id.to_string(),
            description: String::new(),
            components: HashMap::new(),
            services,
            symlinks: Vec::new(),
            shared_paths: Vec::new(),
            resources: Default::default(),
            lifecycle: Default::default(),
            requirements: Default::default(),
            pipeline: HashMap::new(),
            bench: None,
            fuzz: None,
            trace: Default::default(),
            app_launcher: None,
            toolchain: None,
            bench_workloads: HashMap::new(),
            trace_workloads: HashMap::new(),
            fuzz_workloads: Default::default(),
            trace_workload_defaults: HashMap::new(),
            trace_phase_templates: HashMap::new(),
            trace_variants: HashMap::new(),
            trace_profiles: HashMap::new(),
            trace_experiments: HashMap::new(),
            trace_guardrails: Vec::new(),
            bench_profiles: HashMap::new(),
            fuzz_profiles: HashMap::new(),
        }
    }

    fn single_service_rig(id: &str, service: ServiceSpec) -> RigSpec {
        let mut services = HashMap::new();
        services.insert("svc".to_string(), service);
        RigSpec {
            id: id.to_string(),
            description: String::new(),
            components: HashMap::new(),
            services,
            symlinks: Vec::new(),
            shared_paths: Vec::new(),
            resources: Default::default(),
            lifecycle: Default::default(),
            requirements: Default::default(),
            pipeline: HashMap::new(),
            bench: None,
            fuzz: None,
            trace: Default::default(),
            app_launcher: None,
            toolchain: None,
            bench_workloads: HashMap::new(),
            trace_workloads: HashMap::new(),
            fuzz_workloads: Default::default(),
            trace_workload_defaults: HashMap::new(),
            trace_phase_templates: HashMap::new(),
            trace_variants: HashMap::new(),
            trace_profiles: HashMap::new(),
            trace_experiments: HashMap::new(),
            trace_guardrails: Vec::new(),
            bench_profiles: HashMap::new(),
            fuzz_profiles: HashMap::new(),
        }
    }

    fn wait_until(timeout: Duration, mut predicate: impl FnMut() -> bool) -> bool {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if predicate() {
                return true;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        predicate()
    }

    #[test]
    fn test_http_static_service_still_starts_and_stops() {
        with_isolated_home(|_home| {
            let tmp = tempfile::tempdir().expect("tmpdir");
            std::fs::write(tmp.path().join("index.html"), "ok").expect("index");
            let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
            let port = listener.local_addr().expect("local_addr").port();
            drop(listener);
            let rig = single_service_rig(
                "service-http-static",
                ServiceSpec {
                    kind: ServiceKind::HttpStatic,
                    cwd: Some(tmp.path().to_string_lossy().into_owned()),
                    port: Some(port),
                    command: None,
                    env: HashMap::new(),
                    health: None,
                    discover: None,
                },
            );

            let pid = service::start(&rig, "svc").expect("start http-static service");
            assert!(
                wait_until(Duration::from_secs(5), || std::net::TcpStream::connect((
                    "127.0.0.1",
                    port
                ))
                .is_ok()),
                "http-static listener should accept connections"
            );
            assert_eq!(
                service::status(&rig.id, "svc").expect("status"),
                ServiceStatus::Running(pid)
            );

            service::stop(&rig, "svc").expect("stop http-static service");
            assert_eq!(
                service::status(&rig.id, "svc").expect("status after stop"),
                ServiceStatus::Stopped
            );
        });
    }

    #[test]
    fn test_external_services_remain_adoption_only() {
        with_isolated_home(|_home| {
            let rig = single_service_rig(
                "service-external",
                ServiceSpec {
                    kind: ServiceKind::External,
                    cwd: None,
                    port: None,
                    command: None,
                    env: HashMap::new(),
                    health: None,
                    discover: Some(DiscoverSpec {
                        pattern: "homeboy-test-no-such-external-process-XQZ-1463".to_string(),
                        argv_contains: Vec::new(),
                    }),
                },
            );

            let err = service::start(&rig, "svc").expect_err("external start rejected");
            assert!(
                err.message.contains("adopted, not spawned"),
                "unexpected error: {}",
                err.message
            );
            service::stop(&rig, "svc").expect("external stop with no match is idempotent");
            assert_eq!(
                service::status(&rig.id, "svc").expect("status"),
                ServiceStatus::Stopped
            );
        });
    }

    #[test]
    fn test_discover_newest_from_ps_requires_all_argv_selectors() {
        let now = 1_000;
        let ps_output = r#"
101  00:00:50  node /Applications/Sample.app/app-server-child.mjs
202  00:00:40  node /Users/user/Developer/sample-app@feature-a/preview-server-child.mjs app-server-child.mjs
303  00:00:30  node /Users/user/Developer/sample-app@other/preview-server-child.mjs app-server-child.mjs
"#;
        let selectors = vec![
            "sample-app@feature-a".to_string(),
            "preview-server-child.mjs".to_string(),
        ];

        let found = super::super::platform::discover_newest_from_ps(
            "app-server-child.mjs",
            &selectors,
            now,
            999,
            ps_output,
        )
        .expect("matching process");

        assert_eq!(found.pid, 202);
        assert_eq!(found.started_at_epoch, 960);
    }

    #[test]
    fn test_discover_newest_from_ps_returns_none_when_argv_selector_misses() {
        let selectors = vec!["sample-app@feature-a".to_string()];
        let found = super::super::platform::discover_newest_from_ps(
            "app-server-child.mjs",
            &selectors,
            1_000,
            999,
            "101  00:00:50  node /Applications/Sample.app/app-server-child.mjs",
        );

        assert_eq!(found, None);
    }

    #[test]
    fn test_discover_newest_for_spec_returns_none_when_no_process_matches() {
        let discover = DiscoverSpec {
            pattern: "homeboy-test-no-such-process-XQZ-1750".to_string(),
            argv_contains: vec!["homeboy-test-no-such-selector-XQZ-1750".to_string()],
        };

        assert_eq!(service::discover_newest_for_spec(&discover).unwrap(), None);
    }

    #[test]
    fn test_discover_external_pid_returns_none_when_no_process_matches() {
        let discover = DiscoverSpec {
            pattern: "homeboy-test-no-such-external-process-XQZ-1750".to_string(),
            argv_contains: Vec::new(),
        };

        assert_eq!(service::discover_external_pid(&discover).unwrap(), None);
    }

    #[test]
    fn test_command_service_start_status_stop_lifecycle() {
        with_isolated_home(|_home| {
            let rig = command_rig("service-lifecycle", "sleep 30", None);
            let pid = service::start(&rig, "cmd").expect("start command service");

            assert_eq!(
                service::status(&rig.id, "cmd").expect("status"),
                ServiceStatus::Running(pid)
            );

            service::stop(&rig, "cmd").expect("stop command service");
            assert_eq!(
                service::status(&rig.id, "cmd").expect("status after stop"),
                ServiceStatus::Stopped
            );
        });
    }

    #[test]
    fn test_command_service_stop_kills_process_group_children() {
        with_isolated_home(|_home| {
            let rig = command_rig("service-process-group", "sleep 30 & wait", None);
            let pid = service::start(&rig, "cmd").expect("start command service");
            assert!(
                wait_until(Duration::from_secs(2), || unsafe {
                    libc::kill(-(pid as libc::pid_t), 0) == 0
                }),
                "process group should exist after start"
            );

            service::stop(&rig, "cmd").expect("stop command service");
            assert!(
                !wait_until(Duration::from_secs(2), || {
                    homeboy_core::process::process_group_is_running(pid as i32)
                }),
                "stop should terminate the whole managed process group, not just the shell"
            );
        });
    }

    #[test]
    fn test_start_overwrites_stale_pid_state() {
        with_isolated_home(|_home| {
            let rig = command_rig("service-stale", "sleep 30", None);
            let mut state = RigState::default();
            state.services.insert(
                "cmd".to_string(),
                ServiceState {
                    pid: Some(999_999),
                    started_at: Some("2026-04-24T00:00:00Z".to_string()),
                    status: "running".to_string(),
                },
            );
            state.save(&rig.id).expect("save stale state");

            assert_eq!(
                service::status(&rig.id, "cmd").expect("stale status"),
                ServiceStatus::Stale(999_999)
            );
            let pid = service::start(&rig, "cmd").expect("start replaces stale pid");
            assert_ne!(pid, 999_999);
            assert_eq!(
                service::status(&rig.id, "cmd").expect("fresh status"),
                ServiceStatus::Running(pid)
            );

            service::stop(&rig, "cmd").expect("cleanup");
        });
    }

    #[test]
    fn test_command_service_writes_to_supervisor_log() {
        with_isolated_home(|_home| {
            let tmp = tempfile::tempdir().expect("tmpdir");
            let rig = command_rig(
                "service-log",
                "printf supervisor-log-marker; sleep 30",
                Some(tmp.path().to_string_lossy().into_owned()),
            );
            let pid = service::start(&rig, "cmd").expect("start command service");
            let log_path = service::log_path(&rig.id, "cmd").expect("log path");
            assert!(
                wait_until(Duration::from_secs(2), || std::fs::read_to_string(
                    &log_path
                )
                .map(|s| s.contains("supervisor-log-marker"))
                .unwrap_or(false)),
                "expected command output in log {}",
                log_path.display()
            );

            assert_eq!(
                service::status(&rig.id, "cmd").expect("status"),
                ServiceStatus::Running(pid)
            );
            service::stop(&rig, "cmd").expect("cleanup");
        });
    }
}

#[test]
fn test_service_status_variants_distinguish() {
    let running = ServiceStatus::Running(42);
    let stopped = ServiceStatus::Stopped;
    let stale = ServiceStatus::Stale(42);

    assert_ne!(running, stopped);
    assert_ne!(running, stale);
    assert_ne!(stopped, stale);
}

#[test]
fn test_service_status_running_carries_pid() {
    match ServiceStatus::Running(12345) {
        ServiceStatus::Running(pid) => assert_eq!(pid, 12345),
        other => panic!("expected Running, got {:?}", other),
    }
}

#[test]
fn test_service_status_stale_carries_pid() {
    match ServiceStatus::Stale(67890) {
        ServiceStatus::Stale(pid) => assert_eq!(pid, 67890),
        other => panic!("expected Stale, got {:?}", other),
    }
}

#[test]
fn test_parse_etime_mm_ss() {
    use crate::service::parse_etime_seconds;
    // 2 minutes 30 seconds.
    assert_eq!(parse_etime_seconds("02:30"), Some(150));
    assert_eq!(parse_etime_seconds("0:01"), Some(1));
}

#[test]
fn test_parse_etime_hh_mm_ss() {
    use crate::service::parse_etime_seconds;
    // 1h 02m 03s.
    assert_eq!(parse_etime_seconds("01:02:03"), Some(3_723));
}

#[test]
fn test_parse_etime_dd_hh_mm_ss() {
    use crate::service::parse_etime_seconds;
    // 4 days, 9 hours, 27 minutes, 59 seconds — the format BSD `ps` emits
    // for a long-running daemon (matches what `etime` printed during dev).
    assert_eq!(parse_etime_seconds("04-09:27:59"), Some(379_679));
}

#[test]
fn test_parse_etime_rejects_garbage() {
    use crate::service::parse_etime_seconds;
    assert_eq!(parse_etime_seconds(""), None);
    assert_eq!(parse_etime_seconds("not-a-time"), None);
    assert_eq!(parse_etime_seconds("01"), None);
    assert_eq!(parse_etime_seconds("a:b:c"), None);
}

/// #11128: rig service logs were an unbounded append -- `create(true)
/// .append(true)`, no rotation, no TTL, and no cleanup category -- so a chatty
/// service filled the disk and nothing reclaimed it.
mod log_rotation {
    use crate::service::log_rotation::{rotate_log_if_oversized, rotation_renames};
    use crate::service::{RIG_LOG_MAX_BYTES, RIG_LOG_MAX_GENERATIONS};

    fn write(path: &std::path::Path, bytes: usize) {
        std::fs::write(path, vec![b'x'; bytes]).expect("write log");
    }

    fn read(path: &std::path::Path) -> Option<String> {
        std::fs::read_to_string(path).ok()
    }

    #[test]
    fn a_log_below_the_cap_is_left_alone() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("svc.log");
        write(&log, 8);

        assert!(!rotate_log_if_oversized(&log, 16, 3).expect("rotate"));
        assert_eq!(log.metadata().expect("metadata").len(), 8);
        assert!(!dir.path().join("svc.log.1").exists());
    }

    #[test]
    fn a_log_at_the_cap_rotates_to_the_first_generation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("svc.log");
        std::fs::write(&log, b"oldest").expect("write log");

        assert!(rotate_log_if_oversized(&log, 6, 3).expect("rotate"));

        assert!(!log.exists(), "the live log is renamed, not copied");
        assert_eq!(
            read(&dir.path().join("svc.log.1")).as_deref(),
            Some("oldest")
        );
    }

    /// The bound is the product of size and generation count. Rotating more
    /// times than there are generations must not accumulate files, and the
    /// bytes that fall off the end are the oldest ones.
    #[test]
    fn rotation_retains_a_fixed_number_of_generations_and_drops_the_oldest() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("svc.log");

        for generation in 0..6 {
            std::fs::write(&log, format!("gen-{generation}")).expect("write log");
            assert!(rotate_log_if_oversized(&log, 1, 2).expect("rotate"));
        }

        assert_eq!(
            read(&dir.path().join("svc.log.1")).as_deref(),
            Some("gen-5")
        );
        assert_eq!(
            read(&dir.path().join("svc.log.2")).as_deref(),
            Some("gen-4")
        );
        assert!(
            !dir.path().join("svc.log.3").exists(),
            "a third generation would make the bound unbounded"
        );
    }

    /// Renames run oldest-first. Newest-first would copy generation 1 over
    /// generation 2 and lose the older bytes before they were promoted.
    #[test]
    fn the_rename_chain_runs_oldest_first() {
        let renames = rotation_renames(std::path::Path::new("/logs/svc.log"), 3);

        let order: Vec<String> = renames
            .iter()
            .map(|(from, to)| format!("{}->{}", from.display(), to.display()))
            .collect();
        assert_eq!(
            order,
            vec![
                "/logs/svc.log.2->/logs/svc.log.3".to_string(),
                "/logs/svc.log.1->/logs/svc.log.2".to_string(),
                "/logs/svc.log->/logs/svc.log.1".to_string(),
            ]
        );
    }

    /// Rotation must never invent a log for a service that has not run.
    #[test]
    fn a_missing_log_is_not_rotated_and_is_not_created() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("never-started.log");

        assert!(!rotate_log_if_oversized(&log, 1, 3).expect("rotate"));
        assert!(!log.exists());
        assert!(!dir.path().join("never-started.log.1").exists());
    }

    /// A zero bound is a disabled bound, not a bound that deletes everything.
    #[test]
    fn a_zero_size_or_generation_bound_disables_rotation() {
        let dir = tempfile::tempdir().expect("tempdir");
        let log = dir.path().join("svc.log");
        write(&log, 32);

        assert!(!rotate_log_if_oversized(&log, 0, 3).expect("zero size"));
        assert!(!rotate_log_if_oversized(&log, 1, 0).expect("zero generations"));
        assert_eq!(log.metadata().expect("metadata").len(), 32);
    }

    #[test]
    fn the_shipped_bounds_are_finite() {
        const {
            assert!(RIG_LOG_MAX_BYTES > 0);
            assert!(RIG_LOG_MAX_GENERATIONS > 0);
        }
    }
}
