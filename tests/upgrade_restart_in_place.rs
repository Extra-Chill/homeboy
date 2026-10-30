//! Upgrade's daemon step uses requested-stop recovery rather than draining.
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn upgrade_daemon_step_resumes_live_controller_work() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let prefix = home.path().join("bin");
        std::fs::create_dir_all(&prefix).unwrap();
        let binary = prefix.join("homeboy");
        std::fs::copy(env!("CARGO_BIN_EXE_homeboy"), &binary).unwrap();
        let path = format!("{}:{}", prefix.display(), std::env::var("PATH").unwrap());
        let _path = homeboy_core::test_support::EnvVarGuard::set("PATH", path);
        let invoke = |args: &[&str]| -> serde_json::Value {
            let output = Command::new(&binary).args(args).output().unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice(&output.stdout).unwrap()
        };
        invoke(&["daemon", "start"]);
        let definition = home.path().join("upgrade-work.json");
        std::fs::write(&definition, serde_json::json!({
            "schema": "homeboy/workflow/v1", "id": "upgrade-work",
            "steps": [
                {"id": "a", "exec": {"program": "sh", "args": ["-c", "sleep 4; echo a"]}},
                {"id": "b", "needs": ["a"], "exec": {"program": "sh", "args": ["-c", "sleep 4; echo b"]}}
            ]
        }).to_string()).unwrap();
        let run = invoke(&["workflow", "run", definition.to_str().unwrap()]);
        let instance = run["data"]["instance"].as_str().unwrap();
        for delay in [Duration::from_millis(1500), Duration::from_secs(4)] {
            std::thread::sleep(delay);
            let status = homeboy_core::daemon::read_status().unwrap();
            let lease = status.freshness.lease_id.as_deref().unwrap();
            let identity = homeboy_core::build_identity::current().display;
            assert!(
                homeboy_upgrade::upgrade::start_installed_daemon(&identity, Some(lease)).unwrap()
            );
            let restarted = homeboy_core::daemon::read_status().unwrap();
            assert_ne!(restarted.freshness.lease_id, status.freshness.lease_id);
        }
        let deadline = Instant::now() + Duration::from_secs(60);
        loop {
            let status = invoke(&["workflow", "status", instance]);
            if status["data"]["status"] == "succeeded" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "workflow did not resume: {status}"
            );
            std::thread::sleep(Duration::from_millis(250));
        }
        invoke(&["daemon", "stop"]);
    });
}
