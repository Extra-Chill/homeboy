//! Explicit live release/daemon proof in a disposable HOME and binary prefix.
//! Run with `cargo test --test detached_upgrade_live -- --ignored --test-threads=1`.

#[test]
#[ignore = "downloads a published release and replaces only the copied disposable binary"]
fn caller_exit_does_not_strand_upgrade_behind_live_cook_pin() {
    homeboy_core::test_support::with_isolated_home(|home| {
        use std::process::Command;
        use std::time::{Duration, Instant};

        let bin = home.path().join("bin");
        std::fs::create_dir_all(&bin).expect("disposable binary prefix");
        let binary = bin.join("homeboy");
        std::fs::copy(env!("CARGO_BIN_EXE_homeboy"), &binary).expect("copy candidate binary");
        let path = format!(
            "{}:/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin",
            bin.display()
        );
        let invoke = |args: &[&str]| {
            Command::new(&binary)
                .args(args)
                .env_clear()
                .env("HOME", home.path())
                .env("PATH", &path)
                .env("USER", "homeboy-live-test")
                .output()
                .expect("run disposable Homeboy")
        };

        let pin = homeboy_core::runtime_promotion::pin_cook_generation("live-upgrade-foreign-pin")
            .expect("hold live foreign Cook generation pin");
        let output = invoke(&[
            "upgrade",
            "--method",
            "binary",
            "--version",
            &format!("v{}", env!("CARGO_PKG_VERSION")),
            "--skip-extensions",
            "--skip-runners",
        ]);
        assert!(
            output.status.success(),
            "admission: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let admission: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("admission JSON");
        let data = &admission["data"];
        assert_eq!(data["status"], "queued", "{admission}");
        let id = data["operation_id"].as_str().expect("operation identity");
        let while_pinned = invoke(&["upgrade", "status", id]);
        let running: serde_json::Value =
            serde_json::from_slice(&while_pinned.stdout).expect("running JSON");
        assert_eq!(running["data"]["status"], "running", "{running}");
        std::thread::sleep(Duration::from_secs(2));
        let still_pinned: serde_json::Value =
            serde_json::from_slice(&invoke(&["upgrade", "status", id]).stdout)
                .expect("still pinned status JSON");
        assert_eq!(still_pinned["data"]["status"], "running", "{still_pinned}");
        let owner = still_pinned["data"]["owner_pid"]
            .as_i64()
            .expect("owned worker PID") as libc::pid_t;
        assert_ne!(owner as u32, std::process::id());
        assert_eq!(unsafe { libc::kill(owner, libc::SIGTERM) }, 0);
        let dead_worker_deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let previous: serde_json::Value =
                serde_json::from_slice(&invoke(&["upgrade", "status", id]).stdout)
                    .expect("previous worker status JSON");
            if previous["data"]["status"] != "running" {
                assert_eq!(previous["data"]["phase"], "interrupted", "{previous}");
                assert_eq!(previous["data"]["status"], "error", "{previous}");
                break;
            }
            assert!(
                Instant::now() < dead_worker_deadline,
                "dead worker still running"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        let retry = invoke(&[
            "upgrade",
            "--method",
            "binary",
            "--version",
            &format!("v{}", env!("CARGO_PKG_VERSION")),
            "--skip-extensions",
            "--skip-runners",
        ]);
        assert!(
            retry.status.success(),
            "retry admission: {}",
            String::from_utf8_lossy(&retry.stderr)
        );
        let retry: serde_json::Value = serde_json::from_slice(&retry.stdout).expect("retry JSON");
        assert_eq!(retry["data"]["status"], "queued", "{retry}");
        let retry_id = retry["data"]["operation_id"]
            .as_str()
            .expect("retry identity");
        assert_ne!(retry_id, id);
        drop(pin);

        let deadline = Instant::now() + Duration::from_secs(240);
        let terminal = loop {
            let output = invoke(&["upgrade", "status", retry_id]);
            let status: serde_json::Value =
                serde_json::from_slice(&output.stdout).expect("status JSON");
            if status["data"]["status"] != "running" {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "upgrade did not finish: {status}"
            );
            std::thread::sleep(Duration::from_millis(500));
        };
        assert_eq!(terminal["data"]["status"], "pass", "{terminal}");
        let previous: serde_json::Value =
            serde_json::from_slice(&invoke(&["upgrade", "status", id]).stdout)
                .expect("previous attempt remains terminal");
        assert_eq!(previous["data"]["status"], "error", "{previous}");
        assert_eq!(
            terminal["data"]["controller"]["status"], "updated",
            "{terminal}"
        );
        let daemon: serde_json::Value =
            serde_json::from_slice(&invoke(&["daemon", "status"]).stdout)
                .expect("daemon status JSON");
        assert_eq!(daemon["data"]["running"], true, "{daemon}");
        assert_eq!(daemon["data"]["reachable"], true, "{daemon}");
        assert_eq!(
            daemon["data"]["daemon"]["active_version"],
            env!("CARGO_PKG_VERSION")
        );
        assert_eq!(
            daemon["data"]["daemon"]["active_build"],
            daemon["data"]["daemon"]["desired_build"]
        );
        assert!(daemon["data"]["daemon"]["lease_id"]
            .as_str()
            .is_some_and(|id| !id.is_empty()));
        let _ = invoke(&["daemon", "stop"]);
    });
}
