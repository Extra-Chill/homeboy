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
        // A retained legacy loop can lose its supervisor receipt. Persist a
        // real failed resume acknowledgement, then restore exact terminal
        // custody without changing the controller generation or old effect.
        homeboy::agents::orchestration::register();
        let controller =
            homeboy::agents::agent_task_loop_controller::load_controller("stopped-resume-proof")
                .unwrap();
        let generation = controller.updated_at.clone();
        let canonical =
            homeboy::agents::agent_task_loop_controller::control_plane_run_id(&controller.loop_id)
                .unwrap();
        let failed_effect =
            serde_json::from_value(json!(format!("loop-resume:{}:{}", canonical, generation)))
                .unwrap();
        let mut lost = controller.clone();
        lost.metadata["work_job"]["job_id"] = json!(uuid::Uuid::new_v4().to_string());
        homeboy::agents::agent_task_loop_controller::write_controller(&lost).unwrap();
        let failed_resume = || {
            let output = Command::new(binary)
                .args(["agent-task", "loop", "resume", "stopped-resume-proof"])
                .env_clear()
                .env("HOME", root)
                .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
                .env("HOMEBOY_NO_UPDATE_CHECK", "1")
                .output()
                .unwrap();
            assert!(!output.status.success(), "missing custody must fail closed");
            assert!(String::from_utf8_lossy(&output.stdout).contains("observed quiescent"));
        };
        failed_resume();
        let predecessor =
            homeboy::core::control_plane::effect_status(&canonical, &failed_effect).unwrap();
        assert_eq!(
            serde_json::to_value(predecessor.acknowledgement.as_ref().unwrap().outcome).unwrap(),
            "failed"
        );
        let unavailable = cli(&["agent-task", "loop", "status", "stopped-resume-proof"]);
        assert_eq!(unavailable["work"]["outcome"], "unknown");
        assert_eq!(
            unavailable["work"]["recovery"]["state"],
            "terminal_custody_unavailable"
        );
        homeboy::agents::agent_task_loop_controller::write_controller(&controller).unwrap();
        // Exercise real pruning and generation retirement before the new intent.
        let jobs = homeboy_core::api_jobs::JobStore::open_without_reconciliation(
            homeboy_core::paths::daemon_jobs_file().unwrap(),
        )
        .unwrap();
        let ids = jobs.terminal_controller_job_ids("work", 1).unwrap();
        jobs.prune_terminal_controller_jobs("work", 1, &ids)
            .unwrap();
        cli(&["daemon", "stop"]);
        failed_resume(); // unchanged default identity replays its immutable failure
        let rearm_diagnostics = cli(&["agent-task", "loop", "status", "stopped-resume-proof"]);
        let suggested = rearm_diagnostics["status"]["diagnostics"]["next_commands"][0]
            .as_str()
            .unwrap();
        assert!(suggested.contains("--rearm-after"));
        assert!(suggested.contains(&failed_effect.0));
        assert!(suggested.contains("--expected-updated-at"));
        assert_eq!(
            homeboy::core::control_plane::effect_status(&canonical, &failed_effect).unwrap(),
            predecessor
        );
        let rearm_args = [
            "agent-task",
            "loop",
            "resume",
            "stopped-resume-proof",
            "--rearm-after",
            &failed_effect.0,
            "--idempotency-key",
            "evaluation-recovery-1",
            "--expected-updated-at",
            &generation,
        ];
        std::fs::write(&ready, "ready").unwrap();
        let rearmed = cli(&rearm_args);
        let replayed = cli(&rearm_args);
        assert_eq!(rearmed["acknowledgement"], replayed["acknowledgement"]);
        assert_eq!(
            homeboy::core::control_plane::effect_status(&canonical, &failed_effect).unwrap(),
            predecessor
        );
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
        // Replaying the successful rearm after a new stop returns the prior
        // acknowledgement and cannot clear the newer stop epoch.
        cli(&rearm_args);
        assert_eq!(
            cli(&["agent-task", "loop", "status", "stopped-resume-proof"])["runtime"]["on"],
            false
        );
        assert_eq!(std::fs::read_to_string(&producer_effect).unwrap(), "x");
        assert_eq!(std::fs::read_to_string(&consumer_effect).unwrap(), "x");
        cli(&["daemon", "stop"]);
    });
}
