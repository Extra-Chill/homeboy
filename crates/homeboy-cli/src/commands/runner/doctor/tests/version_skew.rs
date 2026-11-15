use super::super::*;
use std::io::{Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Exercise the doctor lifecycle, not only its check builder: a compatible
/// daemon must not hide exact-build drift from the configured command.
#[test]
fn doctor_version_skew_observes_daemon_and_reobserves_after_alignment() {
    crate::test_support::with_isolated_home(|home| {
        let identity = homeboy_product_identity::build_identity();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let addr = listener.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let stopped = stop.clone();
        let capabilities = homeboy_lab_contract::required_lab_handoff_capabilities();
        let version = serde_json::json!({
            "version": identity.version,
            "build_identity": {"display": identity.display},
            "lab_contract_version": homeboy_lab_contract::LAB_CONTRACT_VERSION,
            "lab_handoff_capabilities": capabilities,
        })
        .to_string();
        let thread = std::thread::spawn(move || {
            while !stopped.load(Ordering::Relaxed) {
                let Ok((mut stream, _)) = listener.accept() else {
                    std::thread::sleep(std::time::Duration::from_millis(5));
                    continue;
                };
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(1)))
                    .unwrap();
                let mut request = [0; 4096];
                let len = stream.read(&mut request).unwrap_or(0);
                if len == 0 {
                    continue;
                }
                let body = if String::from_utf8_lossy(&request[..len]).contains("/version") {
                    version.clone()
                } else {
                    serde_json::json!({"pid": 4242, "freshness": {
                        "fresh": true, "lease_id": "lease-version-skew", "pid": 4242,
                        "restartable": false, "active_jobs": 0, "repair_plan": []
                    }})
                    .to_string()
                };
                let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body);
            }
        });
        struct ServerGuard(Arc<AtomicBool>, Option<std::thread::JoinHandle<()>>);
        impl Drop for ServerGuard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Relaxed);
                self.1.take().unwrap().join().unwrap();
            }
        }
        let _server = ServerGuard(stop, Some(thread));
        let executable = home.path().join("configured-homeboy");
        let write_command = |display: &str| {
            let evidence = serde_json::json!({
                "version": identity.version, "display": display,
                "git_commit": identity.git_commit, "git_dirty": false,
                "lab_contract_version": homeboy_lab_contract::LAB_CONTRACT_VERSION,
                "lab_handoff_capabilities": capabilities,
            });
            std::fs::write(&executable, format!("#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'homeboy {}'; else printf '%s\\n' '{}'; fi\n", identity.version, evidence)).unwrap();
            std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755)).unwrap();
        };
        // Same release and capabilities, different exact configured build.
        let other = format!("homeboy {}+deadbeef", identity.version);
        write_command(&other);
        server::create(
            r#"{"id":"skew-runner","host":"localhost","user":"tester"}"#,
            false,
        )
        .unwrap();
        runner::create(
            &serde_json::json!({
                "id":"skew-runner", "kind":"ssh", "server_id":"skew-runner",
                "workspace_root": home.path(), "homeboy_path": executable,
            })
            .to_string(),
            false,
        )
        .unwrap();
        let mut session = super::daemon::health_session(addr);
        session.runner_id = "skew-runner".to_string();
        session.server_id = Some("skew-runner".to_string());
        session.remote_daemon_lease_id = Some("lease-version-skew".to_string());
        session.homeboy_version = identity.version.clone();
        session.homeboy_build_identity = Some(identity.display.clone());
        // Use the lifecycle's own selected session path, so controller identity
        // and isolated roots are resolved exactly as they are in production.
        let path = runner::persisted_status("skew-runner")
            .unwrap()
            .session_path;
        std::fs::create_dir_all(std::path::Path::new(&path).parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&session).unwrap()).unwrap();

        let observe = || run("skew-runner").unwrap().0;
        let report = observe();
        let pair = |id: &str| {
            report
                .checks
                .iter()
                .find(|check| check.id == id)
                .expect("build pair check")
        };
        assert_eq!(
            pair("homeboy.controller_daemon").status,
            RunnerDoctorStatus::Ok
        );
        let drift = pair("homeboy.daemon_command");
        assert_eq!(drift.status, RunnerDoctorStatus::Error);
        assert_eq!(
            drift.details["daemon_build_identity"],
            identity.display.strip_prefix("homeboy ").unwrap()
        );
        assert_eq!(
            drift.details["homeboy_path_build_identity"],
            other.strip_prefix("homeboy ").unwrap()
        );
        assert!(
            matches!(&drift.remediation_action, Some(types::RunnerRepairAction::RefreshHomeboy { git_ref: Some(reference), allow_downgrade: false }) if reference == &format!("v{}", identity.version))
        );

        write_command(&identity.display);
        let repaired = observe();
        for id in ["homeboy.controller_daemon", "homeboy.daemon_command"] {
            assert_eq!(
                repaired
                    .checks
                    .iter()
                    .find(|check| check.id == id)
                    .unwrap()
                    .status,
                RunnerDoctorStatus::Ok
            );
        }
    });
}
