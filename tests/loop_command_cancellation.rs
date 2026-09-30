//! Live daemon regression for #15268 in a disposable controller store.
use serde_json::{json, Value};
use std::process::Command;
use std::time::{Duration, Instant};

#[test]
fn stopped_command_cannot_mutate_or_resurrect_controller() {
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
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
            serde_json::from_slice::<Value>(&output.stdout).expect("CLI JSON")["data"].clone()
        };
        let spec = json!({
            "schema": "homeboy/controller-spec/v1", "controller_id": "command-cancel-proof",
            "phase": "prove", "config_version": "v1",
            "workflows": [{"workflow_id": "command", "tasks": ["Run command cancellation fixture"],
                "runtime_execution": {"kind":"command", "command":"/bin/sh",
                    "args":["-c", format!("echo $$ > '{}'; sleep 3; touch '{}'; sleep 10", started.display(), mutation.display())],
                    "cwd":root, "timeout_seconds":20}, "inputs":{}}]
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
        let child: i32 = std::fs::read_to_string(&started)
            .expect("child PID")
            .trim()
            .parse()
            .expect("PID");
        cli(&["agent-task", "loop", "stop", "command-cancel-proof"]);
        assert_eq!(
            unsafe { libc::kill(child, 0) },
            -1,
            "owned child still lives after stop acknowledgement"
        );
        std::thread::sleep(Duration::from_secs(4));
        assert!(!mutation.exists(), "cancelled process mutated after stop");
        let status = cli(&["agent-task", "loop", "status", "command-cancel-proof"]);
        assert_eq!(status["status"]["controller"]["state"], "abandoned");
        assert_eq!(
            status["status"]["controller"]["next_actions"][0]["status"],
            "cancelled"
        );
        assert_eq!(status["work"]["status"], "cancelled");
        cli(&["daemon", "stop"]);
        cli(&["daemon", "start"]);
        let restarted = cli(&["agent-task", "loop", "status", "command-cancel-proof"]);
        assert_eq!(restarted["status"]["controller"]["state"], "abandoned");
        assert!(!mutation.exists());
        cli(&["daemon", "stop"]);
    });
}
