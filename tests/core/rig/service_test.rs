//! Service supervisor tests for `src/core/rig/service.rs`.
//!
//! The process lifecycle (spawn / SIGTERM / SIGKILL) is validated manually
//! in the end-to-end smoke described in #1468. Unit scope here covers the
//! pure types and status-enum ergonomics that back the runner's reporting.

use crate::rig::service::ServiceStatus;

#[cfg(unix)]
mod lifecycle {
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    use crate::rig::service::{self, ServiceStatus};
    use crate::rig::spec::{DiscoverSpec, RigSpec, ServiceKind, ServiceSpec};
    use crate::rig::state::{RigState, ServiceState};
    use crate::test_support::with_isolated_home;

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
            pipeline: HashMap::new(),
            bench: None,
            app_launcher: None,
            bench_workloads: HashMap::new(),
            bench_profiles: HashMap::new(),
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
            pipeline: HashMap::new(),
            bench: None,
            app_launcher: None,
            bench_workloads: HashMap::new(),
            bench_profiles: HashMap::new(),
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
                !wait_until(Duration::from_secs(2), || unsafe {
                    libc::kill(-(pid as libc::pid_t), 0) == 0
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
    use crate::rig::service::parse_etime_seconds;
    // 2 minutes 30 seconds.
    assert_eq!(parse_etime_seconds("02:30"), Some(150));
    assert_eq!(parse_etime_seconds("0:01"), Some(1));
}

#[test]
fn test_parse_etime_hh_mm_ss() {
    use crate::rig::service::parse_etime_seconds;
    // 1h 02m 03s.
    assert_eq!(parse_etime_seconds("01:02:03"), Some(3_723));
}

#[test]
fn test_parse_etime_dd_hh_mm_ss() {
    use crate::rig::service::parse_etime_seconds;
    // 4 days, 9 hours, 27 minutes, 59 seconds — the format BSD `ps` emits
    // for a long-running daemon (matches what `etime` printed during dev).
    assert_eq!(parse_etime_seconds("04-09:27:59"), Some(379_679));
}

#[test]
fn test_parse_etime_rejects_garbage() {
    use crate::rig::service::parse_etime_seconds;
    assert_eq!(parse_etime_seconds(""), None);
    assert_eq!(parse_etime_seconds("not-a-time"), None);
    assert_eq!(parse_etime_seconds("01"), None);
    assert_eq!(parse_etime_seconds("a:b:c"), None);
}
