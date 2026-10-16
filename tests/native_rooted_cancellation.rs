use std::{
    process::{Child, Command, Output},
    time::{Duration, Instant},
};

use homeboy_agents::agent_task_lifecycle::{
    self, AgentTaskLifecycleStore, AgentTaskRunState, LabOffloadProxyPlan,
};
use homeboy_core::{api_jobs::Job, daemon::ControllerJobRequest};
use serde_json::{json, Value};

const RUN_ID: &str = "agent-task-15493-private-native-preparing-attempt6";
const RUNNER_ID: &str = "private-proof-runner";

struct OwnedChild(Child);

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if self.0.try_wait().ok().flatten().is_none() {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
}

fn seed_preparing(store: &AgentTaskLifecycleStore) {
    let command = vec!["homeboy".into(), "agent-task".into()];
    agent_task_lifecycle::record_lab_offload_planned_in_store(
        store,
        LabOffloadProxyPlan {
            run_id: RUN_ID,
            runner_id: RUNNER_ID,
            remote_workspace: "/private-runner/workspace",
            remote_command: &command,
            durable_plan: None,
        },
    )
    .expect("persist private run");
    agent_task_lifecycle::record_lab_offload_submission_intent_in_store(
        store,
        RUN_ID,
        RUNNER_ID,
        "/private-runner/workspace",
        &command,
        &[],
    )
    .expect("persist submission intent");
}

fn cli(binary: &str, args: &[&str]) -> Output {
    let mut command = Command::new(binary);
    command.args(args);
    homeboy_core::test_support::bounded_output(command)
}

fn ack(output: &Output) -> Value {
    let value: Value = serde_json::from_slice(&output.stdout).unwrap_or_else(|error| {
        panic!(
            "parse CLI output {error}: {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    value
        .get("data")
        .and_then(|data| data.get("cancellation"))
        .or_else(|| {
            value
                .get("data")
                .filter(|data| data.get("acknowledgement").is_some())
        })
        .cloned()
        .unwrap_or(value)
}

fn daemon_address() -> String {
    let state_dir =
        std::env::var(homeboy_core::paths::DAEMON_STATE_DIR_ENV).expect("private daemon state dir");
    let state: Value = serde_json::from_slice(
        &std::fs::read(std::path::Path::new(&state_dir).join("state.json"))
            .expect("read private daemon lease"),
    )
    .expect("parse private daemon lease");
    state["address"]
        .as_str()
        .expect("daemon address")
        .to_string()
}

fn daemon_request(method: &str, path: &str, body: Option<Value>) -> Value {
    let client = reqwest::blocking::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(5))
        .build()
        .expect("private loopback client");
    let url = format!("http://{}{path}", daemon_address());
    let response = match (method, body) {
        ("GET", None) => client.get(url).send(),
        ("POST", Some(body)) => client.post(url).json(&body).send(),
        _ => panic!("unsupported private daemon request"),
    }
    .expect("call private daemon");
    assert!(
        response.status().is_success(),
        "daemon response: {}",
        response.status()
    );
    response.json().expect("decode private daemon response")
}

fn controller_job_status(job_id: &str) -> homeboy_core::api_jobs::JobStatus {
    let response = daemon_request("GET", &format!("/jobs/{job_id}"), None);
    serde_json::from_value::<Job>(
        response
            .pointer("/data/body/job")
            .expect("public daemon job projection")
            .clone(),
    )
    .expect("decode private controller job")
    .status
}

#[test]
fn native_private_daemon_cancels_only_the_rooted_staging_owner() {
    homeboy_core::test_support::with_isolated_home(|_| {
        eprintln!("NATIVE_PHASE=seed_private_roots");
        let binary = env!("CARGO_BIN_EXE_homeboy");
        std::env::set_var("HOMEBOY_COMMAND", &binary);
        let store =
            AgentTaskLifecycleStore::from_current_environment().expect("private lifecycle store");
        let roots = store.roots();
        let other_root = AgentTaskLifecycleStore::new(homeboy_core::paths::PathRoots::new(
            roots.config().to_path_buf(),
            roots.data().join("other-controller-root"),
            roots.artifacts().join("other-controller-root"),
        ));
        seed_preparing(&store);
        seed_preparing(&other_root);

        eprintln!("NATIVE_PHASE=start_private_daemon");
        std::env::set_var("HOMEBOY_COMMAND", &binary);
        let daemon_process = OwnedChild(
            Command::new(&binary)
                .args(["daemon", "serve", "--addr", "127.0.0.1:0"])
                .spawn()
                .expect("start owned private daemon"),
        );
        let daemon_deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < daemon_deadline {
            if std::path::Path::new(
                &std::env::var(homeboy_core::paths::DAEMON_STATE_DIR_ENV).unwrap(),
            )
            .join("state.json")
            .exists()
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            std::path::Path::new(
                &std::env::var(homeboy_core::paths::DAEMON_STATE_DIR_ENV).unwrap()
            )
            .join("state.json")
            .exists(),
            "private daemon publishes its own lease"
        );

        eprintln!("NATIVE_PHASE=admit_controlled_owner");
        // A daemon `step` owns one long-running process that ignores SIGTERM, so
        // cancellation stays pending through the bounded termination grace and
        // the owner is cancelled only once the process tree is gone.
        let work_request =
            serde_json::to_value(homeboy_core::daemon::step_job::StepJobRequest::new(
                format!("native-proof-owner:{RUN_ID}"),
                "/bin/sh",
                vec![
                    "-c".to_string(),
                    "trap '' TERM; exec sleep 3600".to_string(),
                ],
                std::env::temp_dir().display().to_string(),
                Default::default(),
                homeboy_core::daemon::step_job::STEP_RECEIPT_CONSUMER,
            ))
            .expect("serialize controlled owner step");
        let response = daemon_request(
            "POST",
            "/controller/jobs",
            Some(
                serde_json::to_value(ControllerJobRequest {
                    job_type: homeboy_core::daemon::step_job::STEP_JOB_TYPE.to_string(),
                    version: 1,
                    idempotency_key: format!("native-proof-work:{RUN_ID}"),
                    active_idempotency_key: None,
                    request: work_request,
                })
                .expect("serialize controlled job"),
            ),
        );
        let controller_job: Job = serde_json::from_value(
            response
                .pointer("/data/body/job")
                .expect("admitted controller job")
                .clone(),
        )
        .expect("decode controller job");
        let controller_job_id = controller_job.id.to_string();
        daemon_request(
            "POST",
            &format!("/controller/jobs/{controller_job_id}/start"),
            Some(json!({})),
        );
        agent_task_lifecycle::record_lab_staging_controller_job_in_store(
            &store,
            RUN_ID,
            RUNNER_ID,
            &controller_job_id,
        )
        .expect("bind staging owner to selected lifecycle root");
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline {
            if controller_job_status(&controller_job_id)
                == homeboy_core::api_jobs::JobStatus::Running
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert_eq!(
            controller_job_status(&controller_job_id),
            homeboy_core::api_jobs::JobStatus::Running,
            "native private owner is running before cancellation"
        );

        eprintln!("NATIVE_PHASE=request_cancellation");
        let cancel_args = [
            "agent-task",
            "cancel",
            RUN_ID,
            "--reason",
            "native private staged owner cancellation",
            "--idempotency-key",
            "native-15493-cancel-effect",
        ];
        let first = cli(&binary, &cancel_args);
        let first_ack = ack(&first);
        println!("NATIVE_FIRST_EXIT={:?}", first.status.code());
        println!("NATIVE_FIRST_ACK={first_ack}");
        // The step owner stops within the bounded termination grace, so the
        // cancellation completes in the first request.
        assert_eq!(first_ack["accepted"], "succeeded");
        assert_eq!(first_ack["disposition"], "cancelled");
        assert_eq!(first_ack["terminal"], true);
        assert_eq!(
            other_root.read_record(RUN_ID).unwrap().state,
            AgentTaskRunState::Queued
        );

        let replay = cli(&binary, &cancel_args);
        let replay_ack = ack(&replay);
        println!("NATIVE_REPLAY_EXIT={:?}", replay.status.code());
        println!("NATIVE_REPLAY_ACK={replay_ack}");
        assert_eq!(
            replay_ack, first_ack,
            "same idempotency key replays the retained acknowledgement"
        );

        let deadline = Instant::now() + Duration::from_secs(10);
        let mut controller_cancelled = false;
        while Instant::now() < deadline {
            if controller_job_status(&controller_job_id)
                == homeboy_core::api_jobs::JobStatus::Cancelled
            {
                controller_cancelled = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(
            controller_cancelled,
            "private controller owner reaches cancelled terminal"
        );
        let status = cli(&binary, &["agent-task", "status", RUN_ID]);
        println!("NATIVE_STATUS_EXIT={:?}", status.status.code());
        println!("NATIVE_STATUS={}", String::from_utf8_lossy(&status.stdout));
        assert!(status.status.success());
        assert_eq!(
            store.read_record(RUN_ID).unwrap().state,
            AgentTaskRunState::Cancelled
        );
        assert_eq!(
            other_root.read_record(RUN_ID).unwrap().state,
            AgentTaskRunState::Queued
        );
        let event_kinds = store
            .open_observation_readonly()
            .expect("private event ledger")
            .control_plane_event_stream(
                &homeboy_core::control_plane_contract::RunId::new(RUN_ID).unwrap(),
            )
            .expect("private run event stream")
            .unwrap()
            .iter()
            .map(|event| event.kind.clone())
            .collect::<Vec<_>>();
        println!("NATIVE_EVENT_KINDS={event_kinds:?}");
        assert!(event_kinds.iter().any(|kind| kind == "action.succeeded"));
        assert!(!event_kinds.iter().any(|kind| kind == "action.failed"));

        drop(daemon_process);
    });
}
