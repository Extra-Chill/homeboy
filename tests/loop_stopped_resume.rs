//! Explicit resume preserves completed effects and recovers a stopped workflow.
use serde_json::{json, Value};
use std::process::Command;
use std::time::{Duration, Instant};

struct DaemonGuard(&'static str, std::path::PathBuf);
impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = Command::new(self.0)
            .args(["daemon", "stop"])
            .env_clear()
            .env("HOME", &self.1)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output();
    }
}

#[test]
fn stopped_loop_resumes_and_recovers_failed_consumer_exactly_once() {
    homeboy_core::test_support::with_isolated_home(|home| {
        let root = home.path();
        let binary = env!("CARGO_BIN_EXE_homeboy");
        let _daemon = DaemonGuard(binary, root.to_path_buf());
        let cli = |args: &[&str]| -> Value {
            let output = Command::new(binary)
                .args(args)
                .env_clear()
                .env("HOME", root)
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
        let producer_effect = root.join("capture-effect");
        let consumer_effect = root.join("evaluation-effect");
        let ready = root.join("ready");
        let producer = format!("printf x >> '{}'; printf '%s' '{{\"artifacts\":{{\"capture\":{{\"id\":\"frozen-source\"}}}}}}' > \"$HOMEBOY_LOOP_ACTION_OUTPUT\"", producer_effect.display());
        let consumer = format!("test -f '{}' || exit 7; printf x >> '{}'; printf '%s' '{{\"artifacts\":{{\"evaluation\":{{\"ok\":true}}}}}}' > \"$HOMEBOY_LOOP_ACTION_OUTPUT\"", ready.display(), consumer_effect.display());
        let spec = json!({"schema":"homeboy/controller-spec/v1","controller_id":"stopped-resume-proof","phase":"evaluate","config_version":"v1",
            "artifacts":[{"artifact_id":"capture","kind":"fixture-capture","required":true},{"artifact_id":"evaluation","kind":"fixture-evaluation","required":true}],
            "workflows":[
                {"workflow_id":"capture","tasks":["Retain capture"],"runtime_execution":{"kind":"command","command":"/bin/sh","args":["-c",producer],"cwd":root,"timeout_seconds":10},"artifacts":["capture"],"emits":["capture"],"inputs":{}},
                {"workflow_id":"evaluate","tasks":["Evaluate capture"],"runtime_execution":{"kind":"command","command":"/bin/sh","args":["-c",consumer],"cwd":root,"timeout_seconds":10},"artifacts":["evaluation"],"emits":["evaluation"],"consumes":["capture"],"inputs":{}}
            ],"artifact_graph":[{"artifact_id":"capture","from_workflow_id":"capture","to_workflow_id":"evaluate","required":true}]});
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
        loop {
            let status = cli(&["agent-task", "loop", "status", "stopped-resume-proof"]);
            if status["status"]["controller"]["next_actions"][1]["status"] == "failed" {
                break;
            }
            assert!(Instant::now() < deadline, "initial evaluation did not fail");
            std::thread::sleep(Duration::from_millis(25));
        }
        cli(&["agent-task", "loop", "stop", "stopped-resume-proof"]);
        let stopped = cli(&["agent-task", "loop", "status", "stopped-resume-proof"]);
        assert_eq!(stopped["runtime"]["on"], false);
        std::fs::write(&ready, "ready").unwrap();
        cli(&["agent-task", "loop", "resume", "stopped-resume-proof"]);
        let resumed = cli(&["agent-task", "loop", "status", "stopped-resume-proof"]);
        assert_eq!(resumed["runtime"]["on"], true);
        assert_eq!(resumed["runtime"]["revolutions"], 2);
        assert_eq!(
            resumed["status"]["controller"]["next_actions"][0]["status"],
            "completed"
        );
        assert!(
            !consumer_effect.exists(),
            "resume must not silently replay a failed action"
        );
        cli(&[
            "agent-task",
            "controller",
            "run",
            "stopped-resume-proof",
            "--action-id",
            "action-2",
        ]);
        let recovered = cli(&["agent-task", "loop", "status", "stopped-resume-proof"]);
        assert_eq!(
            recovered["status"]["controller"]["next_actions"][1]["status"],
            "completed"
        );
        assert_eq!(std::fs::read_to_string(&producer_effect).unwrap(), "x");
        assert_eq!(std::fs::read_to_string(&consumer_effect).unwrap(), "x");
        // Recovery of completed work is refused without executing it again.
        let repeated = Command::new(binary)
            .args([
                "agent-task",
                "controller",
                "run",
                "stopped-resume-proof",
                "--action-id",
                "action-2",
            ])
            .env_clear()
            .env("HOME", root)
            .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output()
            .unwrap();
        assert!(!repeated.status.success());
        assert!(String::from_utf8_lossy(&repeated.stdout).contains("Completed, not pending"));
        assert_eq!(std::fs::read_to_string(&consumer_effect).unwrap(), "x");
        cli(&["agent-task", "loop", "stop", "stopped-resume-proof"]);
        cli(&["daemon", "stop"]);
    });
}
