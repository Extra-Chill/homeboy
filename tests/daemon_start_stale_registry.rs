//! Startup must observe its candidate, not the stale pre-activation router.
use serde_json::Value;
use std::process::Command;

#[test]
fn lease_stop_uses_registered_generation_instead_of_inherited_store() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let cli = |args: &[&str], state_dir: Option<&std::path::Path>, bypass: bool| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_homeboy"));
            command
                .args(args)
                .env_clear()
                .env("HOME", home.path())
                .env(
                    "HOMEBOY_DAEMON_ROUTER_DIR",
                    home.path().join(".config/homeboy/daemon"),
                )
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOMEBOY_NO_UPDATE_CHECK", "1");
            if let Some(path) = state_dir {
                command.env("HOMEBOY_DAEMON_STATE_DIR", path);
            }
            if bypass {
                command.env("HOMEBOY_DAEMON_ROUTER_BYPASS", "1");
            }
            command.output().expect("run disposable daemon command")
        };
        let first = cli(&["daemon", "start"], None, false);
        assert!(
            first.status.success(),
            "{}",
            String::from_utf8_lossy(&first.stdout)
        );
        let first: Value = serde_json::from_slice(&first.stdout).unwrap();
        let lease = first["data"]["lease_id"].as_str().unwrap();
        let state_path = std::path::PathBuf::from(first["data"]["state_path"].as_str().unwrap());
        let foreign_dir = home.path().join("unrelated-store");
        let foreign_jobs = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            foreign_dir.join("jobs.json"),
        )
        .expect("open unrelated durable store");
        let foreign_job = foreign_jobs.create("protected-unrelated-job");
        let stop = cli(
            &["daemon", "stop", "--lease-id", lease],
            Some(&foreign_dir),
            false,
        );
        // Clean up through the exact frame even when the regression assertion
        // fails, so this test never leaves a live disposable daemon behind.
        let cleanup = cli(
            &["daemon", "stop", "--lease-id", lease],
            state_path.parent(),
            true,
        );
        assert!(
            cleanup.status.success(),
            "{}",
            String::from_utf8_lossy(&cleanup.stdout)
        );
        assert!(
            stop.status.success(),
            "{}",
            String::from_utf8_lossy(&stop.stdout)
        );
        let stop: Value = serde_json::from_slice(&stop.stdout).unwrap();
        assert_eq!(stop["data"]["state_path"], state_path.display().to_string());
        assert_eq!(stop["data"]["stopped"], true);
        assert!(!foreign_dir.join("state.json").exists());
        assert_eq!(
            foreign_jobs.get(foreign_job.id).unwrap().status,
            homeboy_core::api_jobs::JobStatus::Queued
        );
    });
}

#[test]
fn startup_activates_verified_candidate_when_prior_registry_store_is_absent() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let cli = |args: &[&str]| -> Value {
            let output = Command::new(env!("CARGO_BIN_EXE_homeboy"))
                .args(args)
                .env_clear()
                .env("HOME", home.path())
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOMEBOY_NO_UPDATE_CHECK", "1")
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<Value>(&output.stdout).unwrap()["data"].clone()
        };
        cli(&["daemon", "start"]);
        let first = cli(&["daemon", "status"]);
        cli(&["daemon", "stop"]);
        let registry_path = home.path().join(".config/homeboy/daemon/generations.json");
        let mut registry: Value =
            serde_json::from_slice(&std::fs::read(&registry_path).unwrap()).unwrap();
        let owner = registry["generations"]["admission_owner"]
            .as_str()
            .unwrap()
            .to_string();
        registry["generations"]["generations"][&owner]["endpoint"]["state_dir"] =
            serde_json::json!(home.path().join("absent-prior-store"));
        std::fs::write(&registry_path, serde_json::to_vec(&registry).unwrap()).unwrap();
        cli(&["daemon", "start"]);
        let replacement = cli(&["daemon", "status"]);
        cli(&["daemon", "stop"]);
        assert_eq!(replacement["admits_work"], true);
        assert_eq!(replacement["fresh"], true);
        assert_ne!(
            replacement["daemon"]["lease_id"],
            first["daemon"]["lease_id"]
        );
        let registry: Value =
            serde_json::from_slice(&std::fs::read(registry_path).unwrap()).unwrap();
        assert_eq!(
            registry["generations"]["admission_owner"],
            replacement["daemon"]["lease_id"]
        );
        assert_eq!(
            registry["generations"]["generations"][&owner]["drain_state"],
            "draining"
        );
    });
}
