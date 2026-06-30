use super::*;
use reqwest::blocking::Client;
use serde_json::{json, Value};
use std::time::Duration;

#[test]
fn timeout_mirrors_remote_job_without_cancelling() {
    crate::test_support::with_isolated_home(|_| {
        let runner = ssh_runner();
        let job_id = uuid::Uuid::new_v4();
        let job = Job {
            id: job_id,
            operation: "runner.exec".to_string(),
            status: JobStatus::Running,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_001_000,
            started_at_ms: Some(1_700_000_000_000),
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            stale_reason: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
        };
        let err = daemon_job_wait_timeout(
            &runner,
            "/srv/homeboy/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            "runner daemon job",
            true,
        );
        let run_id = format!("runner-exec-bench-lab-{job_id}");

        assert!(err.message.contains("runner daemon job"));
        assert!(err.message.contains(job_id.to_string().as_str()));
        assert!(err.message.contains("lab"));
        assert!(err.message.contains("was not cancelled"));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains(&format!("homeboy runs show {run_id}"))));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains(&format!("homeboy runs artifacts {run_id}"))));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("Lab offload handoff: runner `lab` has daemon job")));
        assert!(err.hints.iter().any(|hint| hint.message.contains(
            "homeboy runner exec lab --cwd /srv/homeboy/project -- homeboy runs list --status running --limit 20"
        )));
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains(&format!("homeboy runner job cancel lab {job_id}"))));
        assert!(err.hints.iter().any(|hint| {
            hint.message.contains(RUNNER_EXEC_WAIT_TIMEOUT_ENV)
                && hint.message.contains("controller-side")
                && hint.message.contains("workload settings")
        }));

        let store = crate::core::observation::ObservationStore::open_initialized().expect("store");
        let mirrored = store
            .get_run(&run_id)
            .expect("get mirrored run")
            .expect("mirrored run");
        assert_eq!(mirrored.status, "running");
        assert_eq!(
            mirrored.metadata_json["lab"]["remote_job"]["id"].as_str(),
            Some(job_id.to_string().as_str())
        );
    });
}

fn running_job_with_id(id: uuid::Uuid) -> Job {
    Job {
        id,
        operation: "runner.exec".to_string(),
        status: JobStatus::Running,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_001_000,
        started_at_ms: Some(1_700_000_000_000),
        finished_at_ms: None,
        event_count: 0,
        source_snapshot: None,
        stale_reason: None,
        target_runner_id: None,
        target_project_id: None,
        claim_id: None,
        claimed_by_runner_id: None,
        claimed_at_ms: None,
        claim_expires_at_ms: None,
        artifacts: Vec::new(),
    }
}

#[test]
fn opt_in_cancels_remote_job_on_wait_timeout() {
    use std::cell::RefCell;
    use std::rc::Rc;
    crate::test_support::with_isolated_home(|_| {
        let _env = EnvVarGuard::set(RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV, "1");
        let calls: Rc<RefCell<Vec<(String, String)>>> = Rc::new(RefCell::new(Vec::new()));
        let recorder = calls.clone();
        let _hook = test_cancel_hook::install(Box::new(move |runner_id: &str, job_id: &str| {
            recorder
                .borrow_mut()
                .push((runner_id.to_string(), job_id.to_string()));
            Ok(())
        }));

        let runner = ssh_runner();
        let job_id = uuid::Uuid::new_v4();
        let job = running_job_with_id(job_id);
        let err = daemon_job_wait_timeout(
            &runner,
            "/srv/homeboy/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            "runner daemon job",
            true,
        );

        // The opt-in cancel primitive fired exactly once, targeting this job.
        assert_eq!(calls.borrow().len(), 1);
        assert_eq!(calls.borrow()[0], ("lab".to_string(), job_id.to_string()));
        // The timeout still surfaces, but no longer claims the job was left running.
        assert!(!err.message.contains("was not cancelled"));
        assert!(err.message.contains("remote cancellation was requested"));
        assert_eq!(err.details["cancel_on_wait_timeout"], "requested");
        assert!(err.hints.iter().any(|hint| hint
            .message
            .contains("requested remote cancellation of job")));
    });
}

#[test]
fn opt_in_off_leaves_remote_job_uncancelled() {
    use std::cell::RefCell;
    use std::rc::Rc;
    crate::test_support::with_isolated_home(|_| {
        let _env = EnvVarGuard::unset(RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV);
        let calls: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let recorder = calls.clone();
        let _hook = test_cancel_hook::install(Box::new(move |_runner_id: &str, _job_id: &str| {
            *recorder.borrow_mut() += 1;
            Ok(())
        }));

        let runner = ssh_runner();
        let job_id = uuid::Uuid::new_v4();
        let job = running_job_with_id(job_id);
        let err = daemon_job_wait_timeout(
            &runner,
            "/srv/homeboy/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            "runner daemon job",
            true,
        );

        // Default contract: the cancel primitive is never invoked.
        assert_eq!(*calls.borrow(), 0);
        assert!(err.message.contains("was not cancelled"));
        assert_eq!(err.details["cancel_on_wait_timeout"], "disabled");
    });
}

#[test]
fn opt_in_surfaces_remote_cancel_failure_on_wait_timeout() {
    use std::cell::RefCell;
    use std::rc::Rc;
    crate::test_support::with_isolated_home(|_| {
        let _env = EnvVarGuard::set(RUNNER_CANCEL_ON_WAIT_TIMEOUT_ENV, "true");
        let calls: Rc<RefCell<usize>> = Rc::new(RefCell::new(0));
        let recorder = calls.clone();
        let _hook = test_cancel_hook::install(Box::new(move |_runner_id: &str, _job_id: &str| {
            *recorder.borrow_mut() += 1;
            Err(crate::core::error::Error::internal_unexpected(
                "runner is not connected",
            ))
        }));

        let runner = ssh_runner();
        let job_id = uuid::Uuid::new_v4();
        let job = running_job_with_id(job_id);
        let err = daemon_job_wait_timeout(
            &runner,
            "/srv/homeboy/project",
            &["homeboy".to_string(), "bench".to_string()],
            &job,
            &[],
            "runner daemon job",
            true,
        );

        assert_eq!(*calls.borrow(), 1);
        assert!(err.message.contains("remote cancellation was requested"));
        assert!(err.message.contains("but failed"));
        assert!(err.message.contains("runner is not connected"));
        assert_eq!(err.details["cancel_on_wait_timeout"], "failed");
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("remote cancellation failed")));
    });
}

#[test]
fn lab_offload_handoff_hints_render_durable_commands() {
    let hints = lab_offload_handoff_hints(
        "homeboy-lab",
        Some("/home/user/Developer/project with spaces"),
        "job-123",
        Some("run-456"),
        DaemonJobHandoffState::InFlight,
        true,
    );
    let joined = hints.join("\n");

    assert!(joined.contains("runner `homeboy-lab`"));
    assert!(joined.contains("daemon job `job-123`"));
    assert!(joined.contains("still in flight"));
    assert!(joined.contains("Persisted run id: `run-456`"));
    assert!(joined.contains("homeboy runs show run-456"));
    assert!(joined.contains("homeboy runs evidence run-456"));
    assert!(joined.contains("homeboy runs artifacts run-456"));
    assert!(joined.contains(
        "homeboy runner exec homeboy-lab --cwd '/home/user/Developer/project with spaces' -- homeboy runs list --status running --limit 20"
    ));
    assert!(joined.contains("homeboy runner job logs homeboy-lab job-123 --follow"));
    assert!(joined.contains("Cancel: `homeboy runner job cancel homeboy-lab job-123`"));
}

#[test]
fn lab_offload_handoff_hints_omit_cancel_when_transport_cannot_cancel() {
    let hints = lab_offload_handoff_hints(
        "homeboy-lab",
        Some("/srv/homeboy/project"),
        "job-123",
        None,
        DaemonJobHandoffState::InFlight,
        false,
    );
    let joined = hints.join("\n");

    assert!(joined.contains("still in flight"));
    assert!(!joined.contains("homeboy runner job cancel homeboy-lab job-123"));
}

#[test]
fn terminal_handoff_hints_reflect_succeeded_job_state() {
    let hints = lab_offload_handoff_hints(
        "homeboy-lab",
        Some("/srv/homeboy/project"),
        "job-123",
        Some("run-456"),
        DaemonJobHandoffState::Terminal(JobStatus::Succeeded),
        true,
    );
    let joined = hints.join("\n");

    assert!(joined.contains("finished with status `succeeded`"));
    assert!(joined.contains("homeboy runs show run-456"));
    assert!(joined.contains("homeboy runs evidence run-456"));
    assert!(joined.contains("homeboy runs artifacts run-456"));
    assert!(joined.contains("Final daemon job events/result"));
    assert!(joined.contains("homeboy runner job logs homeboy-lab job-123"));
    assert!(!joined.contains("still in flight"));
    assert!(!joined.contains("homeboy runner job cancel homeboy-lab job-123"));
}

#[test]
fn terminal_handoff_hints_reflect_failed_job_state() {
    let hints = lab_offload_handoff_hints(
        "homeboy-lab",
        Some("/srv/homeboy/project"),
        "job-123",
        Some("run-456"),
        DaemonJobHandoffState::Terminal(JobStatus::Failed),
        true,
    );
    let joined = hints.join("\n");

    assert!(joined.contains("finished with status `failed`"));
    assert!(joined.contains("Final daemon job events/result"));
    assert!(!joined.contains("still in flight"));
}

#[test]
fn terminal_handoff_hints_reflect_cancelled_job_state() {
    let hints = lab_offload_handoff_hints(
        "homeboy-lab",
        Some("/srv/homeboy/project"),
        "job-123",
        None,
        DaemonJobHandoffState::Terminal(JobStatus::Cancelled),
        true,
    );
    let joined = hints.join("\n");

    assert!(joined.contains("finished with status `cancelled`"));
    assert!(joined.contains("Persisted runner-side run id is not known"));
    assert!(joined.contains("Final daemon job events/result"));
    assert!(!joined.contains("still in flight"));
    assert!(!joined.contains("--status running"));
}

#[test]
fn lab_offload_handoff_persists_run_when_job_is_accepted() {
    crate::test_support::with_isolated_home(|_| {
        let runner = ssh_runner();
        let job_id = uuid::Uuid::new_v4();
        let job = Job {
            id: job_id,
            operation: "runner.exec".to_string(),
            status: JobStatus::Running,
            created_at_ms: 1_700_000_000_000,
            updated_at_ms: 1_700_000_001_000,
            started_at_ms: Some(1_700_000_000_000),
            finished_at_ms: None,
            event_count: 0,
            source_snapshot: None,
            stale_reason: None,
            target_runner_id: None,
            target_project_id: None,
            claim_id: None,
            claimed_by_runner_id: None,
            claimed_at_ms: None,
            claim_expires_at_ms: None,
            artifacts: Vec::new(),
        };

        let run_id = persist_lab_offload_handoff_run(
            &runner,
            "/srv/homeboy/project",
            &["homeboy".to_string(), "trace".to_string()],
            &job,
            None,
        )
        .expect("persist handoff run");

        assert_eq!(run_id, format!("runner-exec-trace-lab-{job_id}"));
        let store = crate::core::observation::ObservationStore::open_initialized().expect("store");
        let run = store
            .get_run(&run_id)
            .expect("get run")
            .expect("persisted handoff run");
        assert_eq!(run.status, "running");
        assert_eq!(run.cwd.as_deref(), Some("/srv/homeboy/project"));
        assert_eq!(
            run.metadata_json["lab"]["remote_job"]["id"].as_str(),
            Some(job_id.to_string().as_str())
        );
    });
}

#[test]
fn reverse_broker_exec_detached_surfaces_persisted_run_id() {
    crate::test_support::with_isolated_home(|_| {
        allow_unauthenticated_loopback_broker();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let _ = crate::core::daemon::serve_listener(listener);
        });
        let broker_url = format!("http://{addr}");

        let stable_run_id = "agent-task-run-123";
        let (output, exit_code) = exec_via_reverse_broker(
            &ssh_runner(),
            &broker_url,
            "/srv/homeboy/project".to_string(),
            Some("extrachill".to_string()),
            vec!["homeboy".to_string(), "test".to_string()],
            Default::default(),
            Vec::new(),
            false,
            None,
            Vec::new(),
            None,
            Some(stable_run_id.to_string()),
            true,
            true,
            true,
        )
        .expect("reverse broker detached exec");

        assert_eq!(exit_code, 0);
        assert_eq!(output.mode, RunnerExecMode::ReverseBroker);
        let job_id = output.job_id.as_deref().expect("job id");
        let mirror_run_id = output.mirror_run_id.as_deref().expect("mirror run id");
        assert_eq!(mirror_run_id, stable_run_id);
        assert_eq!(
            output
                .runner_result
                .as_ref()
                .and_then(|result| result.mirror_run_id.as_deref()),
            Some(mirror_run_id)
        );
        assert_eq!(
            output
                .handoff
                .as_ref()
                .and_then(|handoff| handoff.result.as_ref())
                .and_then(|result| result.mirror_run_id.as_deref()),
            Some(mirror_run_id)
        );

        let stdout: Value = serde_json::from_str(&output.stdout).expect("handoff stdout json");
        assert_eq!(stdout["persisted_run_id"].as_str(), Some(mirror_run_id));
        assert_eq!(stdout["mirror_run_id"].as_str(), Some(mirror_run_id));
        assert_eq!(stdout["job_id"].as_str(), Some(job_id));

        let jobs: Value = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("client")
            .get(format!("{broker_url}/jobs"))
            .send()
            .expect("jobs response")
            .json()
            .expect("jobs json");
        assert_eq!(
            jobs["data"]["body"]["active_runner_jobs"][0]["durable_run_id"].as_str(),
            Some(stable_run_id)
        );

        let store = crate::core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        let run = store
            .get_run(mirror_run_id)
            .expect("read mirror run")
            .expect("mirror run");
        assert_eq!(
            run.metadata_json["lab"]["remote_job"]["id"].as_str(),
            Some(job_id)
        );
    });
}

#[test]
fn detached_handoff_output_includes_runner_job_and_agent_task_followups() {
    crate::test_support::with_isolated_home(|_| {
        let runner = ssh_runner();
        let job = running_job();
        let job_id = job.id.to_string();

        let (output, exit_code) = detached_handoff_output(
            &runner,
            RunnerExecMode::Daemon,
            "/srv/homeboy/project".to_string(),
            vec![
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ],
            SourceSnapshot::existing_remote("lab", "/srv/homeboy/project", Some("/srv/homeboy")),
            job,
            Vec::new(),
            Some("agent-task-run-6454".to_string()),
        );

        assert_eq!(exit_code, 0);
        assert_eq!(output.job_id.as_deref(), Some(job_id.as_str()));
        assert_eq!(output.mirror_run_id.as_deref(), Some("agent-task-run-6454"));
        let json: Value = serde_json::from_str(&output.stdout).expect("handoff JSON");
        let envelope: crate::command_contract::RunnerHandoffEnvelope =
            serde_json::from_value(json.clone()).expect("typed handoff envelope");
        assert_eq!(
            envelope.schema,
            crate::command_contract::RUNNER_HANDOFF_ENVELOPE_SCHEMA
        );
        assert_eq!(envelope.status, "handoff_complete");
        assert_eq!(envelope.execution_location, "runner:lab");
        assert_eq!(envelope.identity.runner_id, "lab");
        assert_eq!(envelope.identity.runner_job_id, job_id);
        assert_eq!(
            envelope.identity.persisted_run_id.as_deref(),
            Some("agent-task-run-6454")
        );
        assert_eq!(
            envelope.identity.run_id.as_deref(),
            Some("agent-task-run-6454")
        );
        assert_eq!(
            envelope.identity.handoff_id.as_deref(),
            Some(format!("runner:lab:job:{job_id}").as_str())
        );
        assert_eq!(envelope.runner_id, "lab");
        assert_eq!(envelope.job_id, job_id);
        assert_eq!(envelope.remote_cwd, "/srv/homeboy/project");
        assert_eq!(
            envelope.durable_run_id.as_deref(),
            Some("agent-task-run-6454")
        );
        assert_eq!(
            envelope.persisted_run_id.as_deref(),
            Some("agent-task-run-6454")
        );
        assert_eq!(
            envelope.mirror_run_id.as_deref(),
            Some("agent-task-run-6454")
        );
        assert_eq!(json["status"], "handoff_complete");
        assert_eq!(json["identity"]["runner_id"], "lab");
        assert_eq!(json["identity"]["runner_job_id"], job_id);
        assert_eq!(json["identity"]["persisted_run_id"], "agent-task-run-6454");
        assert_eq!(json["identity"]["run_id"], "agent-task-run-6454");
        assert_eq!(
            json["identity"]["handoff_id"],
            format!("runner:lab:job:{job_id}")
        );
        assert_eq!(json["job_id"], job_id);
        assert_eq!(json["durable_run_id"], "agent-task-run-6454");
        assert_eq!(
            json["follow_commands"]["status"],
            "homeboy agent-task status agent-task-run-6454"
        );
        assert_eq!(
            json["follow_commands"]["logs"],
            "homeboy agent-task logs agent-task-run-6454"
        );
        assert_eq!(
            json["follow_commands"]["job_logs"],
            format!("homeboy runner job logs lab {job_id} --follow")
        );
    });
}

#[test]
fn runner_handoff_envelope_omits_agent_task_followups_without_run_id() {
    let envelope = crate::command_contract::RunnerHandoffEnvelope::detached_lab_offload(
        "lab",
        "job-123",
        "/srv/homeboy/project".to_string(),
        None,
    );
    let json = serde_json::to_value(&envelope).expect("serialize handoff envelope");

    assert_eq!(
        json["schema"],
        crate::command_contract::RUNNER_HANDOFF_ENVELOPE_SCHEMA
    );
    assert_eq!(json["status"], "handoff_complete");
    assert_eq!(json["execution_location"], "runner:lab");
    assert_eq!(json["identity"]["runner_id"], "lab");
    assert_eq!(json["identity"]["runner_job_id"], "job-123");
    assert_eq!(json["identity"]["handoff_id"], "runner:lab:job:job-123");
    assert!(json["identity"].get("persisted_run_id").is_none());
    assert!(json["identity"].get("run_id").is_none());
    assert_eq!(json["durable_run_id"], Value::Null);
    assert_eq!(json["persisted_run_id"], Value::Null);
    assert_eq!(json["mirror_run_id"], Value::Null);
    assert_eq!(
        json["follow_commands"]["job_logs"],
        "homeboy runner job logs lab job-123 --follow"
    );
    assert_eq!(
        json["follow_commands"]["job_cancel"],
        "homeboy runner job cancel lab job-123"
    );
    assert!(json["follow_commands"].get("status").is_none());
    assert!(json["follow_commands"].get("logs").is_none());
    assert!(json["follow_commands"].get("artifacts").is_none());
}

#[test]
fn reverse_broker_exec_submits_job_and_polls_result() {
    crate::test_support::with_isolated_home(|_| {
        allow_unauthenticated_loopback_broker();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
        let addr = listener.local_addr().expect("addr");
        std::thread::spawn(move || {
            let _ = crate::core::daemon::serve_listener(listener);
        });
        let broker_url = format!("http://{addr}");
        let worker_broker_url = broker_url.clone();
        let worker = std::thread::spawn(move || {
            let client = Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .expect("client");
            let claim = loop {
                let response: Value = client
                    .post(format!("{}/runner/jobs/claim", worker_broker_url))
                    .json(&json!({
                        "runner_id": "lab",
                        "lease_ms": 30_000,
                    }))
                    .send()
                    .expect("claim response")
                    .json()
                    .expect("claim json");
                let claim = response["data"]["body"]["claim"].clone();
                if !claim.is_null() {
                    break claim;
                }
                std::thread::sleep(Duration::from_millis(20));
            };
            let job_id = claim["job"]["id"].as_str().expect("job id").to_string();
            let claim_id = claim["job"]["claim_id"]
                .as_str()
                .expect("claim id")
                .to_string();
            client
                .post(format!("{}/runner/jobs/{job_id}/events", worker_broker_url))
                .json(&json!({
                    "runner_id": "lab",
                    "claim_id": claim_id.clone(),
                    "kind": "progress",
                    "message": "running test worker"
                }))
                .send()
                .expect("event response");
            client
                .post(format!("{}/runner/jobs/{job_id}/finish", worker_broker_url))
                .json(&json!({
                    "runner_id": "lab",
                    "claim_id": claim_id,
                    "result": {
                        "exit_code": 0,
                        "stdout": "reverse ok",
                        "stderr": "",
                        "data": {
                            "patch": {
                                "patch_artifact_id": "reverse-patch"
                            }
                        },
                        "artifacts": [{
                            "id": "reverse-patch",
                            "name": "reverse.patch",
                            "path": "/srv/homeboy/.homeboy/artifacts/reverse.patch",
                            "mime": "text/x-diff",
                            "size_bytes": 12,
                            "sha256": "abc123",
                            "metadata": { "kind": "lab_fix_patch" }
                        }]
                    }
                }))
                .send()
                .expect("finish response");
        });

        let (output, exit_code) = exec_via_reverse_broker(
            &ssh_runner(),
            &broker_url,
            "/srv/homeboy/project".to_string(),
            Some("extrachill".to_string()),
            vec!["homeboy".to_string(), "test".to_string()],
            Default::default(),
            Vec::new(),
            false,
            None,
            Vec::new(),
            None,
            None,
            false,
            true,
            true,
        )
        .expect("reverse broker exec");
        worker.join().expect("worker joins");

        assert_eq!(exit_code, 0);
        assert_eq!(output.mode, RunnerExecMode::ReverseBroker);
        assert_eq!(output.stdout, "reverse ok");
        assert_eq!(output.runner_id, "lab");
        assert!(output.job_id.is_some());
        let mirror_run_id = output.mirror_run_id.as_deref().expect("mirror run id");
        assert!(mirror_run_id.starts_with("runner-exec-test-lab-"));
        assert_eq!(
            output
                .patch
                .as_ref()
                .and_then(|patch| patch.get("patch_artifact_path").and_then(Value::as_str)),
            Some("metadata-only:reverse-patch")
        );
        assert_eq!(
            output
                .mutation_artifacts
                .as_ref()
                .and_then(|artifacts| artifacts.patch_ref.as_ref())
                .map(|artifact| artifact.artifact_id.as_str()),
            Some("reverse-patch")
        );
        assert_eq!(
            output
                .runner_result
                .as_ref()
                .and_then(|result| result.mutation_artifacts.as_ref())
                .and_then(|artifacts| artifacts.patch_ref.as_ref())
                .map(|artifact| artifact.artifact_id.as_str()),
            Some("reverse-patch")
        );
        assert!(output
            .job_events
            .expect("events")
            .iter()
            .any(|event| { event.kind == crate::core::api_jobs::JobEventKind::Progress }));

        let store = crate::core::observation::ObservationStore::open_initialized()
            .expect("observation store");
        let run = store
            .get_run(mirror_run_id)
            .expect("read mirror run")
            .expect("mirror run");
        assert_eq!(
            run.metadata_json["lab"]["reverse_broker"]["runner_id"].as_str(),
            Some("lab")
        );
        assert_eq!(
            run.metadata_json["lab"]["reverse_broker"]["broker_url"].as_str(),
            Some(broker_url.as_str())
        );
        assert_eq!(
            run.metadata_json["lab"]["reverse_broker"]["stdout"].as_str(),
            Some("reverse ok")
        );
        let artifact = store
            .get_artifact("reverse-patch")
            .expect("read reverse artifact")
            .expect("reverse artifact");
        assert_eq!(artifact.run_id, mirror_run_id);
        assert_eq!(artifact.path, "metadata-only:reverse-patch");
    });
}

fn allow_unauthenticated_loopback_broker() {
    super::super::super::broker_auth::BrokerAuthStore {
        allow_unauthenticated_loopback: true,
        ..Default::default()
    }
    .save()
    .expect("save loopback broker auth opt-in");
}

#[test]
fn daemon_exec_failure_without_error_field_is_actionable() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = [0; 4096];
        let _ = std::io::Read::read(&mut stream, &mut buffer).expect("read request");
        let body = serde_json::json!({
            "success": false,
            "data": {
                "error": "validation.invalid_argument",
                "message": "Invalid argument 'cwd': runner exec requires an absolute cwd"
            }
        })
        .to_string();
        let response = format!(
            "HTTP/1.1 400 Bad Request\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
    });

    let err = exec_via_daemon(
        &ssh_runner(),
        &format!("http://{addr}"),
        "/srv/homeboy/project".to_string(),
        None,
        vec!["homeboy".to_string(), "--version".to_string()],
        Default::default(),
        Vec::new(),
        false,
        None,
        Vec::new(),
        None,
        None,
        false,
        true,
        true,
    )
    .expect_err("daemon exec failure");

    assert!(err.message.contains("daemon exec request failed"));
    assert!(err.message.contains("validation.invalid_argument"));
    assert!(err.message.contains("runner exec requires an absolute cwd"));
    assert!(!err.message.contains(": null"));
}

#[test]
fn daemon_exec_request_failed_error_surfaces_payload_detail() {
    let envelope = DaemonEnvelope {
        success: false,
        data: Some(serde_json::json!({
            "error": "validation.invalid_argument",
            "message": "bad cwd"
        })),
        error: None,
    };
    let err = daemon_exec_request_failed_error("lab", 400, &envelope);
    assert!(err.message.contains("daemon exec request failed"));
    assert!(err.message.contains("validation.invalid_argument"));
    assert!(err.message.contains("bad cwd"));
    assert!(!err.message.contains("null"));
}

#[test]
fn terminal_lab_result_transport_error_preserves_recovery_ids() {
    let runner = ssh_runner();
    let job_id =
        uuid::Uuid::parse_str("94cd841d-47f8-41c5-be42-88510314c513").expect("issue job id");
    let job = Job {
        id: job_id,
        operation: "runner.exec".to_string(),
        status: JobStatus::Succeeded,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_001_000,
        started_at_ms: Some(1_700_000_000_000),
        finished_at_ms: Some(1_700_000_001_000),
        event_count: 1,
        source_snapshot: None,
        stale_reason: None,
        target_runner_id: None,
        target_project_id: None,
        claim_id: None,
        claimed_by_runner_id: None,
        claimed_at_ms: None,
        claim_expires_at_ms: None,
        artifacts: Vec::new(),
    };
    let source = Error::internal_json(
        "error decoding response body",
        Some("parse daemon response".to_string()),
    );

    let err = lab_terminal_result_transport_error(
        &runner,
        "/srv/homeboy/a8c-intelligence",
        &[
            "homeboy".to_string(),
            "refactor".to_string(),
            "--from".to_string(),
            "lint".to_string(),
            "--write".to_string(),
            "a8c-intelligence".to_string(),
        ],
        &job,
        source,
    );

    let run_id = format!("runner-exec-refactor-lab-{job_id}");
    assert_eq!(err.code, ErrorCode::RunnerLabTransportFailure);
    assert!(err.message.contains("Lab transport/reporting failure"));
    assert!(err.message.contains("not a remote command failure"));
    assert_eq!(err.details["runner_id"], "lab");
    assert_eq!(err.details["job_id"], job_id.to_string());
    assert_eq!(err.details["persisted_run_id"], run_id);
    assert_eq!(err.details["source"]["context"], "parse daemon response");
    let hints = err
        .hints
        .iter()
        .map(|hint| hint.message.as_str())
        .collect::<Vec<_>>();
    assert!(hints
        .iter()
        .any(|hint| hint.contains(&format!("homeboy runs show {run_id}"))));
    assert!(hints
        .iter()
        .any(|hint| hint.contains(&format!("homeboy runs evidence {run_id}"))));
    assert!(hints
        .iter()
        .any(|hint| hint.contains(&format!("homeboy runs artifacts {run_id}"))));
    assert!(hints
        .iter()
        .any(|hint| hint.contains(&format!("homeboy runner job logs lab {job_id}"))));
    assert!(!err.message.contains("--force-hot"));
    assert!(!hints.iter().any(|hint| hint.contains("--allow-local-hot")));
}

#[test]
fn daemon_exec_request_failed_error_handles_null_payload_with_reconnect_hint() {
    // The historical #3631/#3624 symptom: a stale/restarting daemon answers
    // with an empty/null error payload. We must never surface a bare `null`,
    // and we must point the operator at reconnecting.
    let envelope = DaemonEnvelope {
        success: false,
        data: None,
        error: Some(Value::Null),
    };
    let err = daemon_exec_request_failed_error("lab", 502, &envelope);
    assert!(!err.message.contains("null"));
    assert!(err.message.contains("stale") || err.message.contains("restarted"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("homeboy runner connect lab")));
}

#[test]
fn daemon_exec_stale_response_error_is_actionable() {
    let err = daemon_exec_stale_response_error("lab", 200, "expected value at line 1 column 1");
    assert!(err.message.contains("unreadable exec response"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("homeboy runner connect lab")));
}

#[test]
fn daemon_exec_empty_envelope_over_http_is_actionable_not_null() {
    // A stale daemon that answers `{"success": false}` with no error/data.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("listener");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept");
        let mut buffer = [0; 4096];
        let _ = std::io::Read::read(&mut stream, &mut buffer).expect("read request");
        let body = serde_json::json!({ "success": false }).to_string();
        let response = format!(
            "HTTP/1.1 502 Bad Gateway\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        std::io::Write::write_all(&mut stream, response.as_bytes()).expect("write response");
    });

    let err = exec_via_daemon(
        &ssh_runner(),
        &format!("http://{addr}"),
        "/srv/homeboy/project".to_string(),
        None,
        vec!["homeboy".to_string(), "--version".to_string()],
        Default::default(),
        Vec::new(),
        false,
        None,
        Vec::new(),
        None,
        None,
        false,
        true,
        true,
    )
    .expect_err("daemon exec failure");

    assert!(!err.message.contains(": null"));
    assert!(err.message.contains("no result") || err.message.contains("stale"));
    assert!(err
        .hints
        .iter()
        .any(|hint| hint.message.contains("homeboy runner connect")));
}

fn running_job() -> Job {
    Job {
        id: uuid::Uuid::new_v4(),
        operation: "runner.exec".to_string(),
        status: JobStatus::Running,
        created_at_ms: 1_700_000_000_000,
        updated_at_ms: 1_700_000_001_000,
        started_at_ms: Some(1_700_000_000_000),
        finished_at_ms: None,
        event_count: 0,
        source_snapshot: None,
        stale_reason: None,
        target_runner_id: None,
        target_project_id: None,
        claim_id: None,
        claimed_by_runner_id: None,
        claimed_at_ms: None,
        claim_expires_at_ms: None,
        artifacts: Vec::new(),
    }
}
