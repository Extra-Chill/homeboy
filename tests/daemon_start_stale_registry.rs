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

#[test]
fn recovery_preview_and_execution_stay_on_registered_generation_with_legacy_store_live() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let router_dir = home.path().join(".config/homeboy/daemon");
        let legacy_dir = router_dir.clone();
        let generation_dir = router_dir.join("generations/test-generation");
        std::fs::create_dir_all(&generation_dir).expect("create generation state directory");
        let cli = |args: &[&str], state_dir: &std::path::Path, bypass: bool| {
            let mut command = Command::new(env!("CARGO_BIN_EXE_homeboy"));
            command
                .args(args)
                .env_clear()
                .env("HOME", home.path())
                .env("HOMEBOY_DAEMON_STATE_DIR", state_dir)
                .env("HOMEBOY_DAEMON_ROUTER_DIR", &router_dir)
                .env("HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS", "900")
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOMEBOY_NO_UPDATE_CHECK", "1");
            if bypass {
                command.env("HOMEBOY_DAEMON_ROUTER_BYPASS", "1");
            }
            command.output().expect("run disposable daemon command")
        };

        let legacy_start = cli(&["daemon", "start"], &legacy_dir, true);
        assert!(
            legacy_start.status.success(),
            "{}",
            String::from_utf8_lossy(&legacy_start.stdout)
        );
        let legacy_start: Value = serde_json::from_slice(&legacy_start.stdout).unwrap();
        let legacy_lease = legacy_start["data"]["lease_id"]
            .as_str()
            .unwrap()
            .to_string();
        let legacy_state_path =
            std::path::PathBuf::from(legacy_start["data"]["state_path"].as_str().unwrap());
        let legacy_state = std::fs::read(&legacy_state_path).expect("legacy lease bytes");
        let legacy_jobs = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            legacy_dir.join("jobs.json"),
        )
        .expect("open legacy durable store");
        let legacy_job_a = legacy_jobs.create("legacy-protected-job-a");
        let legacy_job_b = legacy_jobs.create("legacy-protected-job-b");

        let mut legacy_state_json: Value = serde_json::from_slice(&legacy_state).unwrap();
        legacy_state_json["build_identity"]["version"] = serde_json::json!("0.0.0");
        legacy_state_json["build_identity"]["display"] = serde_json::json!("homeboy 0.0.0+legacy");
        std::fs::write(
            &legacy_state_path,
            serde_json::to_vec(&legacy_state_json).unwrap(),
        )
        .expect("make legacy daemon stale to trigger generation rotation");
        let generation_start = cli(&["daemon", "ensure-running"], &legacy_dir, false);
        assert!(
            generation_start.status.success(),
            "{}",
            String::from_utf8_lossy(&generation_start.stdout)
        );
        let generation_start: Value = serde_json::from_slice(&generation_start.stdout).unwrap();
        let generation_lease = generation_start["data"]["lease_id"]
            .as_str()
            .unwrap()
            .to_string();
        let generation_state_path =
            std::path::PathBuf::from(generation_start["data"]["state_path"].as_str().unwrap());

        // Keep the root-store daemon alive but remove its lease, matching the
        // unleased legacy candidate that used to be invisible to generation status.
        std::fs::remove_file(&legacy_state_path).expect("remove legacy lease, preserve process");
        let mut generation_state: Value = serde_json::from_slice(
            &std::fs::read(&generation_state_path).expect("generation lease"),
        )
        .unwrap();
        generation_state["build_identity"]["version"] = serde_json::json!("0.0.0");
        generation_state["build_identity"]["display"] = serde_json::json!("homeboy 0.0.0+legacy");
        std::fs::write(
            &generation_state_path,
            serde_json::to_vec(&generation_state).unwrap(),
        )
        .expect("make registered generation stale");

        let core_status = homeboy_core::daemon::read_status().expect("read core daemon status");
        assert_eq!(
            core_status.freshness.repair_plan[0].code,
            "daemon_ensure_running"
        );

        let status = cli(&["daemon", "status", "--full"], &legacy_dir, false);
        let preview = cli(&["daemon", "recover", "--dry-run"], &legacy_dir, false);
        let apply = cli(&["daemon", "recover", "--yes"], &legacy_dir, false);
        let current_status = cli(&["daemon", "status", "--full"], &legacy_dir, false);
        let admission = cli(&["daemon", "ensure-running"], &legacy_dir, false);
        let status: Value = serde_json::from_slice(&status.stdout).expect("status JSON");
        let preview: Value = serde_json::from_slice(&preview.stdout).expect("preview JSON");
        let apply: Value = serde_json::from_slice(&apply.stdout).expect("apply JSON");
        let current_status: Value =
            serde_json::from_slice(&current_status.stdout).expect("post-apply status JSON");
        let admission: Value = serde_json::from_slice(&admission.stdout).expect("admission JSON");
        let legacy_jobs_preserved = [legacy_job_a.id, legacy_job_b.id]
            .into_iter()
            .all(|job_id| {
                legacy_jobs
                    .get(job_id)
                    .is_ok_and(|job| job.status == homeboy_core::api_jobs::JobStatus::Queued)
            });

        // Restore and retire only disposable daemons, even when the contract
        // assertions below fail. Keep the active jobs until both processes stop.
        legacy_jobs
            .cancel(legacy_job_a.id, "integration test cleanup")
            .expect("terminalize first legacy test job");
        legacy_jobs
            .cancel(legacy_job_b.id, "integration test cleanup")
            .expect("terminalize second legacy test job");
        std::fs::write(&legacy_state_path, &legacy_state).expect("restore legacy lease");
        for (lease, state_dir) in [
            (legacy_lease.as_str(), legacy_dir.as_path()),
            (generation_lease.as_str(), generation_dir.as_path()),
        ] {
            let _ = cli(&["daemon", "stop", "--lease-id", lease], state_dir, true);
        }
        if let Some(current_lease) = current_status["data"]["freshness"]["lease_id"].as_str() {
            let current_dir =
                std::path::Path::new(current_status["data"]["state_path"].as_str().unwrap())
                    .parent()
                    .unwrap();
            let _ = cli(
                &["daemon", "stop", "--lease-id", current_lease],
                current_dir,
                true,
            );
        }

        assert_eq!(
            status["data"]["state_path"],
            generation_state_path.display().to_string()
        );
        assert_eq!(status["data"]["freshness"]["active_jobs"], 0);
        assert_eq!(
            status["data"]["freshness"]["repair_plan"][0]["code"], "daemon_ensure_running",
            "status: {status}"
        );
        assert!(
            status["data"]["process_candidates"]
                .as_array()
                .unwrap()
                .iter()
                .any(|candidate| {
                    candidate["durable_store_path"]
                        == legacy_dir.join("jobs.json").display().to_string()
                        && candidate["ownership"] == "unrelated"
                }),
            "the live legacy process is retained as unrelated to generation authority"
        );
        assert_eq!(preview["data"]["lease_id"], generation_lease);
        assert_eq!(
            preview["data"]["store_path"],
            generation_state_path.display().to_string()
        );
        assert_eq!(preview["data"]["active_jobs"], 0);
        assert_eq!(
            preview["data"]["plan"]["steps"].as_array().unwrap().len(),
            1,
            "preview: {preview}"
        );
        assert_eq!(
            preview["data"]["plan"]["steps"][0]["code"],
            "daemon_ensure_running"
        );
        assert_eq!(apply["data"]["executed"], true, "apply: {apply}");
        assert_eq!(apply["data"]["fresh"], true);
        assert_eq!(apply["data"]["applied_steps"][0], "daemon_ensure_running");
        assert_eq!(
            apply["data"]["store_path"],
            current_status["data"]["state_path"]
        );
        assert_ne!(
            current_status["data"]["freshness"]["lease_id"],
            generation_lease
        );
        assert!(
            current_status["data"]["state_path"]
                .as_str()
                .unwrap()
                .starts_with(
                    generation_dir
                        .to_str()
                        .unwrap_or_else(|| router_dir.to_str().unwrap())
                )
                || current_status["data"]["state_path"]
                    .as_str()
                    .unwrap()
                    .contains("/generations/")
        );
        assert_eq!(
            admission["data"]["lease_id"],
            current_status["data"]["freshness"]["lease_id"]
        );
        assert_eq!(
            admission["data"]["state_path"],
            current_status["data"]["state_path"]
        );
        assert!(
            legacy_jobs_preserved,
            "recovery mutated legacy protected jobs"
        );
        assert_eq!(
            legacy_jobs.get(legacy_job_a.id).unwrap().status,
            homeboy_core::api_jobs::JobStatus::Cancelled,
            "cleanup terminalizes the disposable legacy job after its preservation assertion"
        );
        assert_eq!(
            legacy_jobs.get(legacy_job_b.id).unwrap().status,
            homeboy_core::api_jobs::JobStatus::Cancelled
        );
    });
}
