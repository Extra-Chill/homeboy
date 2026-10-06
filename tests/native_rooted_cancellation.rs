use std::{
    process::{Child, Command, Output},
    sync::atomic::{AtomicUsize, Ordering},
    sync::Arc,
    time::{Duration, Instant},
};

use homeboy_agents::agent_task_lifecycle::{
    self, AgentTaskLifecycleStore, AgentTaskRunState, LabOffloadProxyPlan,
    RunnerContinuationProvider, RunnerContinuationSubmission,
};
use homeboy_core::{
    api_jobs::{Job, RunnerJobLogSnapshot},
    daemon::ControllerJobRequest,
    error::{Error, Result},
};
use homeboy_runner_contract::{
    RunnerApiSubmitRequest, RunnerExecutionDispatch, RunnerJobLifecycleMetadata,
    RUNNER_API_SUBMIT_REQUEST_SCHEMA, RUNNER_API_V1,
};
use serde_json::{json, Value};

const RUN_ID: &str = "agent-task-15493-private-native-pending-attempt6";
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

fn pending_request() -> RunnerApiSubmitRequest {
    let key = format!("agent-task:v1:{RUNNER_ID}:{RUN_ID}");
    let mut envelope = homeboy_core::runner_execution_envelope::RunnerExecutionEnvelope::planned(
        RUN_ID,
        "reverse_broker",
    );
    envelope.dispatch = Some(RunnerExecutionDispatch {
        runner_id: RUNNER_ID.into(),
        project_id: None,
        operation: "runner.exec".into(),
        command: vec!["true".into()],
        cwd: Some("/tmp".into()),
        env: Default::default(),
        source_snapshot: None,
        require_paths: Vec::new(),
        extension_env_providers: Vec::new(),
    });
    envelope.lifecycle = Some(RunnerJobLifecycleMetadata {
        source: Some("reverse_broker".into()),
        kind: Some("runner.exec".into()),
        durable_run_id: Some(RUN_ID.into()),
        ..Default::default()
    });
    envelope.metadata = json!({"submission_key": key, "durable_run_id": RUN_ID});
    RunnerApiSubmitRequest {
        schema: RUNNER_API_SUBMIT_REQUEST_SCHEMA.into(),
        api_version: RUNNER_API_V1,
        submission_key: key,
        envelope,
        workspace_claim_binding: None,
        workspace_owner_lease: None,
        credential_delivery: None,
    }
}

fn seed_pending(store: &AgentTaskLifecycleStore, with_envelope: bool) {
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
    let request = pending_request();
    if with_envelope {
        agent_task_lifecycle::record_lab_offload_submission_envelope(RUN_ID, &request)
            .expect("persist exact pending envelope");
    } else {
        let fingerprint =
            homeboy_core::api_jobs::runner_api_submission_payload_fingerprint(&request)
                .expect("fingerprint");
        let now = chrono::Utc::now();
        store
            .mutate_record(RUN_ID, |record| {
                record.lab_handoff = Some(agent_task_lifecycle::AgentTaskLabHandoff {
                    state: agent_task_lifecycle::AgentTaskLabHandoffState::Pending,
                    authority: agent_task_lifecycle::AgentTaskLabHandoffAuthority::Controller,
                    runner_id: RUNNER_ID.into(),
                    submission_key: Some(request.submission_key.clone()),
                    payload_fingerprint: Some(fingerprint.clone()),
                    runner_job_id: None,
                    submitted_at: Some(now.to_rfc3339()),
                    acceptance_deadline_at: Some((now + chrono::Duration::minutes(5)).to_rfc3339()),
                    accepted_at: None,
                    expired_at: None,
                    workspace_identity: None,
                    workspace_lifecycle_revision: 0,
                    workspace_owner_lease: None,
                    workspace_claim: None,
                });
                record.metadata["runner_submission_intent"] = json!({
                    "state":"pending", "runner_id":RUNNER_ID,
                    "submission_key":request.submission_key,
                    "payload_fingerprint":fingerprint,
                    "replay_envelope_request":request,
                });
                true
            })
            .expect("persist second root pending intent");
    }
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

struct AdmissionProbe(Arc<AtomicUsize>);

impl RunnerContinuationProvider for AdmissionProbe {
    fn runner_job_log_snapshot(&self, _: &str, _: &str) -> Result<RunnerJobLogSnapshot> {
        Err(Error::internal_unexpected("unused private proof snapshot"))
    }
    fn is_runner_connected(&self, _: &str) -> bool {
        true
    }
    fn run_continuation_exec(&self, _: &str, _: &str, _: &[String], _: &str) -> Result<i32> {
        Err(Error::internal_unexpected(
            "unused private proof continuation",
        ))
    }
    fn submit_runner_api_request(&self, _: &str, _: RunnerContinuationSubmission) -> Result<Job> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Err(Error::internal_unexpected(
            "cancellation admission fence failed",
        ))
    }
}

#[test]
fn native_private_daemon_pending_action_terminal_owner_and_fence() {
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
        seed_pending(&store, true);
        seed_pending(&other_root, false);

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
        let mut child = OwnedChild(
            Command::new("sh")
                .args(["-c", "exec sleep 3600"])
                .spawn()
                .expect("spawn controlled private owner process"),
        );
        let child_identity = homeboy_core::process::process_start_identity(child.0.id())
            .expect("read process identity")
            .expect("owned child identity");
        let work_request = json!({
            "schema":"homeboy/work-job-request/v1",
            "work_type":"agent-task-cook",
            "work_version":2,
            "request":{
                "schema":"homeboy/agent-task-cook-job/v2",
                "idempotency_key":format!("agent-task-cook:{RUN_ID}"),
                "request":{
                    "schema":"homeboy/agent-task-cook-job/v2",
                    "cook_id":RUN_ID,
                    "child_pid":child.0.id(),
                    "child_start_identity":child_identity,
                    "pinned_retry_run_id":RUN_ID
                },
                "phase":"queued",
                "run_id":RUN_ID,
                "terminal_state":null
            }
        });
        let response = daemon_request(
            "POST",
            "/controller/jobs",
            Some(
                serde_json::to_value(ControllerJobRequest {
                    job_type: "work".to_string(),
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
        assert_eq!(first_ack["accepted"], "failed");
        assert_eq!(first_ack["disposition"], "requested");
        assert_eq!(first_ack["terminal"], false);
        assert!(first_ack["message"]
            .as_str()
            .is_some_and(|detail| detail.contains(&controller_job_id)));
        assert_eq!(
            store.read_record(RUN_ID).unwrap().state,
            AgentTaskRunState::Queued
        );
        assert_eq!(
            controller_job_status(&controller_job_id),
            homeboy_core::api_jobs::JobStatus::Running
        );
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
        assert_eq!(
            controller_job_status(&controller_job_id),
            homeboy_core::api_jobs::JobStatus::Running
        );

        child.0.kill().expect("release controlled owner");
        child.0.wait().expect("reap private owner process");
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
        assert!(event_kinds.iter().any(|kind| kind == "action.failed"));
        assert!(!event_kinds.iter().any(|kind| kind == "action.succeeded"));

        let submissions = Arc::new(AtomicUsize::new(0));
        agent_task_lifecycle::register_runner_continuation_provider(Box::new(AdmissionProbe(
            Arc::clone(&submissions),
        )));
        assert!(
            agent_task_lifecycle::with_pending_runner_submission_admission_in_store(
                &store,
                RUN_ID,
                &pending_request(),
                || {
                    submissions.fetch_add(1, Ordering::SeqCst);
                    Ok(())
                },
            )
            .is_err()
        );
        assert_eq!(submissions.load(Ordering::SeqCst), 0);
        println!("NATIVE_POST_CANCEL_SUBMIT_CALLS=0");
        drop(daemon_process);
    });
}
