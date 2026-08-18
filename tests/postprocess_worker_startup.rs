use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use homeboy::agents::agent_task::AgentTaskOutcome;
use homeboy::agents::agent_task_scheduler::{
    AgentTaskAggregateStatus, AgentTaskArtifactPostprocessStep, AgentTaskExecutorAdapter,
    AgentTaskPlan, AgentTaskScheduler,
};
use homeboy::core::artifacts::{
    ArtifactPostprocessAction, ArtifactPostprocessPlan, ArtifactPostprocessRoot,
    ARTIFACT_POSTPROCESS_HELPER_REGISTRY_ENV, ARTIFACT_POSTPROCESS_HELPER_REGISTRY_SCHEMA,
    ARTIFACT_POSTPROCESS_PLAN_SCHEMA,
};
use homeboy_engine_primitives::content_hash;

struct NoopExecutor;

static POSTPROCESS_TEST_LOCK: Mutex<()> = Mutex::new(());

impl AgentTaskExecutorAdapter for NoopExecutor {
    fn execute(
        &self,
        request: homeboy::agents::agent_task::AgentTaskRequest,
        _context: homeboy::agents::agent_task_scheduler::AgentTaskExecutionContext,
    ) -> AgentTaskOutcome {
        AgentTaskOutcome {
            task_id: request.task_id,
            ..Default::default()
        }
    }
}

fn install_helper(home: &std::path::Path) {
    let helper = home.join("postprocess-helper");
    std::fs::write(
        &helper,
        "#!/bin/sh\ncase \"$1\" in report) printf report > \"$HOMEBOY_ARTIFACT_POSTPROCESS_OUTPUT\" ;; esac\n",
    )
    .expect("helper");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&helper, std::fs::Permissions::from_mode(0o755))
            .expect("helper permissions");
    }
    let registry = home.join("postprocess-helpers.json");
    std::fs::write(
        &registry,
        serde_json::json!({
            "schema": ARTIFACT_POSTPROCESS_HELPER_REGISTRY_SCHEMA,
            "helpers": [{
                "id": "fixture",
                "path": helper,
                "sha256": content_hash::sha256_hex(&std::fs::read(&helper).expect("helper bytes")),
                "actions": ["report"]
            }]
        })
        .to_string(),
    )
    .expect("registry");
    std::env::set_var(ARTIFACT_POSTPROCESS_HELPER_REGISTRY_ENV, registry);
}

fn postprocess_plan() -> AgentTaskPlan {
    AgentTaskPlan {
        postprocess_steps: vec![AgentTaskArtifactPostprocessStep {
            id: "compose".to_string(),
            depends_on: Vec::new(),
            required: true,
            plan: ArtifactPostprocessPlan {
                schema: ARTIFACT_POSTPROCESS_PLAN_SCHEMA.to_string(),
                plan_id: "compose".to_string(),
                artifact_roots: vec![ArtifactPostprocessRoot {
                    id: "output".to_string(),
                    path: "unused".to_string(),
                    persisted_ref: None,
                    manifest_path: None,
                }],
                actions: vec![ArtifactPostprocessAction {
                    id: Some("report".to_string()),
                    helper: "fixture".to_string(),
                    action: "report".to_string(),
                    input: None,
                    output: "report.txt".to_string(),
                    parameters: BTreeMap::new(),
                    required: true,
                    side_effects: vec!["artifact_root_output".to_string()],
                }],
                reviewer_refs: Vec::new(),
                metadata: serde_json::json!({}),
            },
        }],
        ..AgentTaskPlan::new("startup-race", Vec::new())
    }
}

#[test]
fn scheduler_recovers_a_worker_killed_before_claim_creation() {
    let _lock = POSTPROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    homeboy::core::test_support::with_isolated_home(|home| {
        install_helper(home.path());
        std::env::set_var("HOMEBOY_POSTPROCESS_WORKER", env!("CARGO_BIN_EXE_homeboy"));
        std::env::set_var("HOMEBOY_POSTPROCESS_WORKER_CLAIM_DELAY_MS", "1500");

        let plan = postprocess_plan();
        let root = homeboy::core::artifacts::root()
            .expect("artifact root")
            .join("agent-task/postprocess/race-run/compose");
        let scheduler = Arc::new(AgentTaskScheduler::new(NoopExecutor).with_run_id("race-run"));
        let worker = thread::spawn({
            let scheduler = Arc::clone(&scheduler);
            move || scheduler.run(plan)
        });

        let worker_file = root.join("worker.json");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker_file.is_file() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if !worker_file.is_file() && worker.is_finished() {
            panic!(
                "scheduler exited before persisting worker startup identity: {:#?}",
                worker.join().expect("scheduler thread")
            );
        }
        assert!(
            worker_file.is_file(),
            "scheduler remained live without persisting worker startup identity"
        );
        let worker_record: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&worker_file).expect("worker record"))
                .expect("worker json");
        let start_identity: homeboy::core::process::ProcessStartIdentity =
            serde_json::from_value(worker_record["start_identity"].clone())
                .expect("worker start identity");
        assert_eq!(
            homeboy::core::process::process_identity_state_with_start_identity(
                worker_record["pid"].as_u64().expect("worker pid") as u32,
                None,
                Some(&start_identity),
            ),
            homeboy::core::process::ProcessIdentityState::Live,
            "worker record identifies the live subprocess"
        );
        assert!(
            !root.join("claim.json").exists(),
            "claim creation is delayed"
        );
        assert!(
            !root.join("checkpoint.json").exists(),
            "a live, identified worker is not checkpointed as failed"
        );
        assert!(!worker.is_finished(), "scheduler waits for the live worker");

        let pid = worker_record["pid"].as_i64().expect("worker pid") as i32;
        #[cfg(unix)]
        unsafe {
            libc::kill(pid, libc::SIGKILL);
        }

        let recovered = worker.join().expect("scheduler thread");
        assert_eq!(
            recovered.status,
            AgentTaskAggregateStatus::Succeeded,
            "postprocess outcomes: {:#?}",
            recovered.outcomes
        );
        assert!(
            root.join("checkpoint.json").is_file(),
            "recovery checkpointed success"
        );
        assert!(
            root.join("claim.json").exists() || root.join("current.json").is_file(),
            "replacement worker completed after the empty pre-claim recovery"
        );
    });
}

#[test]
fn scheduler_restart_adopts_a_live_worker_without_reinvoking_helper() {
    let _lock = POSTPROCESS_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    homeboy::core::test_support::with_isolated_home(|home| {
        install_helper(home.path());
        std::env::set_var("HOMEBOY_POSTPROCESS_WORKER", env!("CARGO_BIN_EXE_homeboy"));
        std::env::set_var("HOMEBOY_POSTPROCESS_WORKER_CLAIM_DELAY_MS", "1500");

        let root = homeboy::core::artifacts::root()
            .expect("artifact root")
            .join("agent-task/postprocess/restart-run/compose");
        let first = thread::spawn(|| {
            AgentTaskScheduler::new(NoopExecutor)
                .with_run_id("restart-run")
                .run(postprocess_plan())
        });
        let worker_file = root.join("worker.json");
        let deadline = Instant::now() + Duration::from_secs(5);
        while !worker_file.is_file() && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        if !worker_file.is_file() && first.is_finished() {
            panic!(
                "first scheduler exited before persisting worker startup identity: {:#?}",
                first.join().expect("first scheduler thread")
            );
        }
        assert!(
            worker_file.is_file(),
            "first scheduler remained live without persisting its worker"
        );

        let restarted = thread::spawn(|| {
            AgentTaskScheduler::new(NoopExecutor)
                .with_run_id("restart-run")
                .run(postprocess_plan())
        });
        let first = first.join().expect("first scheduler");
        let restarted = restarted.join().expect("restarted scheduler");

        assert_eq!(first.status, AgentTaskAggregateStatus::Succeeded);
        assert_eq!(restarted.status, AgentTaskAggregateStatus::Succeeded);
        let versions = std::fs::read_dir(root.join("versions"))
            .expect("versions")
            .count();
        assert_eq!(versions, 1, "live worker helper was invoked once");
        assert!(root.join("checkpoint.json").is_file());
    });
}
