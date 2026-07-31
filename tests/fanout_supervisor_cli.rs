use std::path::PathBuf;
use std::process::Command;

use homeboy::agents::agent_tasks::batch::{persist_fanout_run_batch, FanoutRunBatchChild};

#[test]
fn fanout_status_exposes_the_durable_supervisor_projection() {
    homeboy_core::test_support::with_isolated_home(|_| {
        persist_fanout_run_batch(
            "production-interface",
            "production-interface",
            &[
                FanoutRunBatchChild {
                    task_id: "child-a".to_string(),
                    run_id: "cook-child-a".to_string(),
                },
                FanoutRunBatchChild {
                    task_id: "child-b".to_string(),
                    run_id: "cook-child-b".to_string(),
                },
            ],
            serde_json::json!({
                "dependency_graph": {
                    "schema": "homeboy/agent-task-fanout-dependency-graph/v1",
                    "readiness": {
                        "states": {
                            "child-a": "ready",
                            "child-b": "blocked_by_dependency"
                        },
                        "ready": ["child-a"],
                        "blocked_paths": {
                            "child-b": ["child-b", "child-a"]
                        }
                    }
                }
            }),
        )
        .expect("persist fanout batch");

        let output = Command::new(homeboy_bin())
            .args(["agent-task", "fanout", "status", "production-interface"])
            .env("HOMEBOY_NO_UPDATE_CHECK", "1")
            .output()
            .expect("run Homeboy fanout status");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let output: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("fanout status JSON output");
        assert_eq!(
            output["data"]["schema"],
            "homeboy/agent-task-fanout-status/v2"
        );
        assert_eq!(
            output["data"]["portfolio"]["children"][0]["child_id"],
            "child-a"
        );
        assert_eq!(
            output["data"]["portfolio"]["children"][0]["next_action"],
            "inspect_blocked_candidate"
        );
        assert_eq!(
            output["data"]["portfolio"]["children"][1]["blocker"]["code"],
            "blocked_by_dependency"
        );
    });
}

fn homeboy_bin() -> PathBuf {
    PathBuf::from(std::env::var_os("CARGO_BIN_EXE_homeboy").expect("Homeboy binary path"))
}
