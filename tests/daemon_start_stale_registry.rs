//! Startup must observe its candidate, not the stale pre-activation router.
use serde_json::Value;
use std::process::Command;

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
