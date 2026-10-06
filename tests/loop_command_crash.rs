//! #15275: guarded command death settles one linked WorkJob as unknown failure.
use serde_json::{json, Value};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn daemon_crash_quiesces_command_without_attestation_or_redispatch() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = home.path();
        let started = root.join("started");
        let mutation = root.join("mutation");
        let binary = env!("CARGO_BIN_EXE_homeboy");
        let cli = |args: &[&str]| -> Value {
            let output = Command::new(binary)
                .args(args)
                .env_clear()
                .env("HOME", root)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOMEBOY_NO_UPDATE_CHECK", "1")
                .output()
                .expect("CLI output");
            assert!(
                output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stderr),
                String::from_utf8_lossy(&output.stdout)
            );
            serde_json::from_slice::<Value>(&output.stdout).expect("CLI JSON")["data"].clone()
        };
        let spec = json!({
            "schema":"homeboy/controller-spec/v1", "loop_id": "command-crash-proof",
            "phase":"prove", "config_version":"v1",
            "workflows":[{"workflow_id":"command", "tasks":["Run guarded command crash fixture"],
                "runtime_execution":{"kind":"command", "command":"/bin/sh",
                    "args":["-c",format!("echo $$ > '{}'; sleep 5; touch '{}'; sleep 10",started.display(),mutation.display())],
                    "cwd":root,"timeout_seconds":20},"inputs":{}}]
        });
        cli(&[
            "agent-task",
            "loop",
            "define",
            &spec.to_string(),
            "--on",
            "--resume",
            "--revolution-limit",
            "3",
        ]);
        let deadline = Instant::now() + Duration::from_secs(30);
        while !started.exists() {
            assert!(Instant::now() < deadline, "command did not start");
            std::thread::sleep(Duration::from_millis(25));
        }
        let controller_path = homeboy_core::paths::homeboy_data()
            .expect("data root")
            .join("agent-task-loops/command-crash-proof/controller.json");
        loop {
            let record: Value =
                serde_json::from_slice(&std::fs::read(&controller_path).expect("controller file"))
                    .expect("controller");
            if record["metadata"]["command_recovery"]["state"] == "running" {
                break;
            }
            assert!(Instant::now() < deadline, "ownership not published");
            std::thread::sleep(Duration::from_millis(25));
        }
        let daemon = cli(&["daemon", "status"]);
        let pid = daemon["daemon"]["pid"].as_i64().expect("daemon PID") as i32;
        assert_eq!(unsafe { libc::kill(pid, libc::SIGKILL) }, 0);
        std::thread::sleep(Duration::from_secs(7));
        assert!(
            !mutation.exists(),
            "command escaped daemon-death containment"
        );
        // Ordinary recovery: no operator workload-absence assertion.
        cli(&["daemon", "recover", "--yes"]);
        // Recovery preserves the WorkJob for its driver's asynchronous resume;
        // status reads no longer terminalize the domain as a side effect.
        let deadline = Instant::now() + Duration::from_secs(10);
        let status = loop {
            let status = cli(&["agent-task", "loop", "status", "command-crash-proof"]);
            if status["work"]["status"] == "failed" {
                break status;
            }
            assert!(
                Instant::now() < deadline,
                "driver did not settle guarded command owner loss: {status}"
            );
            std::thread::sleep(Duration::from_millis(25));
        };
        assert_eq!(status["status"]["controller"]["state"], "failed");
        assert_eq!(status["work"]["status"], "failed");
        std::thread::sleep(Duration::from_secs(1));
        assert!(!mutation.exists(), "recovery redispatched command");
        cli(&["daemon", "stop"]);
    });
}
