//! The `workflow` driver against a real durable job store and real `step`
//! children: gates, iteration, approval, cancellation cascade, and recovery
//! after a crash between a committed decision and its step start.

use std::collections::BTreeMap;
use std::path::Path;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::*;
use crate::api_jobs::JobStore;
use crate::test_support::with_isolated_home;

fn request(definition: Value, cwd: &Path) -> WorkflowJobRequest {
    let definition = WorkflowDefinition::from_value(definition).unwrap();
    definition.validate(&|_| true).unwrap();
    let inputs = definition.resolve_inputs(&BTreeMap::new()).unwrap();
    let mut env = EnvMaterializationPlan::default();
    env.public_env
        .insert("PATH".to_string(), "/usr/bin:/bin".to_string());
    WorkflowJobRequest {
        schema: WORKFLOW_JOB_REQUEST_SCHEMA.to_string(),
        instance: definition.instance_id(&inputs, None),
        instance_name: None,
        definition_digest: definition.digest(),
        definition,
        inputs,
        homeboy_program: "/bin/sh".to_string(),
        cwd: cwd.display().to_string(),
        env,
        homeboy_env: EnvMaterializationPlan::default(),
        programs: BTreeMap::from([("sh".to_string(), "/bin/sh".to_string())]),
    }
}

fn admit(store: &JobStore, request: &WorkflowJobRequest) -> Uuid {
    register_workflow_job_driver();
    let envelope: crate::daemon::ControllerJobRequest =
        serde_json::from_value(request.submission().unwrap()).unwrap();
    let state = crate::daemon::controller_job_state(&envelope).unwrap();
    match store
        .admit_controller_job(
            "controller.workflow".to_string(),
            envelope.idempotency_key,
            state,
        )
        .unwrap()
    {
        crate::api_jobs::ControllerJobSubmissionOutcome::Submitted(id) => id,
        crate::api_jobs::ControllerJobSubmissionOutcome::Existing(job) => job.id,
    }
}

fn wait_terminal(store: &JobStore, job_id: Uuid, seconds: u64) -> crate::api_jobs::Job {
    let deadline = Instant::now() + Duration::from_secs(seconds);
    loop {
        let job = store.get(job_id).unwrap();
        if job.status.is_terminal() {
            return job;
        }
        assert!(
            Instant::now() < deadline,
            "job did not finish: {:?}",
            workflow_status(store, &instance_of(store, job_id))
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn instance_of(store: &JobStore, job_id: Uuid) -> String {
    let state = store.controller_job_state(job_id).unwrap();
    state.request["instance"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

fn sh(script: &str) -> Value {
    json!({ "program": "sh", "args": ["-c", script] })
}

/// `evaluate` reports findings on its first run and acceptance on its
/// second; the gate launches `fix` only on findings, and the `until` gate
/// passes on iteration 2. Every step job runs exactly once.
#[test]
fn gates_launch_the_next_step_and_until_passes_on_iteration_two() {
    with_isolated_home(|home| {
        let runs = home.path().join("runs.log");
        let counter = home.path().join("evaluations");
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "proof",
            "budget": {"max_step_runs": 10},
            "iterate": {"until": {"step": "evaluate", "output": "/verdict", "equals": "accepted"}, "max_iterations": 3},
            "steps": [
                {"id": "evaluate", "exec": sh(&format!(
                    "echo evaluate >> {runs}; echo x >> {counter}; \
                     if [ $(wc -l < {counter}) -ge 2 ]; then v=accepted; else v=findings; fi; \
                     printf '{{\"verdict\":\"%s\"}}' $v > \"$HOMEBOY_STEP_OUTPUT\"",
                    runs = runs.display(), counter = counter.display()))},
                {"id": "fix", "needs": ["evaluate"],
                 "when": {"step": "evaluate", "output": "/verdict", "equals": "findings"},
                 "exec": sh(&format!("echo fix-{{{{iteration}}}} >> {}", runs.display()))}
            ]
        });
        let store = JobStore::open_without_reconciliation(home.path().join("jobs.json"))
            .unwrap()
            .with_daemon_lease("workflow-test".to_string());
        let request = request(definition, home.path());
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        let job = wait_terminal(&store, job_id, 30);
        assert_eq!(job.status, JobStatus::Succeeded);
        let status = workflow_status(&store, &request.instance).unwrap();
        assert_eq!(status["status"], "succeeded");
        assert_eq!(status["iteration"], 2);
        assert_eq!(status["steps"]["fix"]["outcome"], "succeeded");
        assert_eq!(status["steps"]["fix"]["carried"], true);
        assert_eq!(
            std::fs::read_to_string(&runs).unwrap(),
            "evaluate\nfix-1\nevaluate\n"
        );
        for key in ["evaluate/1/1", "fix/1/1", "evaluate/2/1"] {
            let child = store
                .controller_job_for_key(&format!("{}/{key}", request.instance))
                .unwrap_or_else(|| panic!("{key} was submitted"));
            assert_eq!(child.status, JobStatus::Succeeded);
            assert_eq!(
                store.controller_job_state(child.id).unwrap().parent_job_id,
                Some(job_id)
            );
        }
        assert!(store
            .controller_job_for_key(&format!("{}/fix/2/1", request.instance))
            .is_none());
    });
}

/// The daemon committed a decision (checkpoint + queued step job) and died
/// before starting the step. Recovery resumes the instance from its
/// checkpoint, starts the admitted step once, and finishes.
#[test]
fn recovery_after_a_commit_starts_each_admitted_step_exactly_once() {
    with_isolated_home(|home| {
        let runs = home.path().join("runs.log");
        let path = home.path().join("jobs.json");
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "resume",
            "steps": [
                {"id": "a", "exec": sh(&format!("echo a >> {}; printf '{{\"n\":1}}' > \"$HOMEBOY_STEP_OUTPUT\"", runs.display()))},
                {"id": "b", "needs": ["a"], "exec": sh(&format!("echo b-{{{{steps.a.output/n}}}} >> {}", runs.display()))}
            ]
        });
        let request = request(definition, home.path());
        let store = JobStore::open_without_reconciliation(&path)
            .unwrap()
            .with_daemon_lease("dead-daemon".to_string());
        let job_id = admit(&store, &request);
        // The dead daemon's last act: claim the instance and commit the first
        // decision without starting the step it admitted.
        store.claim_controller_execution(job_id, false).unwrap();
        let (state, commands) = advance(
            &request.definition,
            &request.initial_state(),
            &Event::Start,
            now_ms(),
        );
        commit_without_start(&request, job_id, &state, &commands, &store);
        let queued = store
            .controller_job_for_key(&format!("{}/a/1/1", request.instance))
            .unwrap();
        assert_eq!(queued.status, JobStatus::Queued);

        let restarted = JobStore::open_without_reconciliation(&path)
            .unwrap()
            .with_daemon_lease("replacement".to_string());
        crate::daemon::recover_controller_jobs(&restarted);
        let job = wait_terminal(&restarted, job_id, 30);
        assert_eq!(job.status, JobStatus::Succeeded);
        assert_eq!(std::fs::read_to_string(&runs).unwrap(), "a\nb-1\n");
        // Replaying the committed decision converges on the same step jobs.
        let replay = restarted.commit_controller_advance(
            job_id,
            to_value(&state).unwrap(),
            Vec::new(),
            Vec::new(),
        );
        assert!(replay.is_err(), "a terminal instance admits nothing more");
        assert_eq!(
            restarted
                .controller_job_for_key(&format!("{}/a/1/1", request.instance))
                .unwrap()
                .id,
            queued.id
        );
    });
}

fn generation_state(lease_id: &str, dir: &Path) -> crate::daemon::DaemonState {
    crate::daemon::DaemonState {
        schema: crate::daemon::DAEMON_LEASE_SCHEMA.to_string(),
        lease_id: lease_id.to_string(),
        startup_token: "test".to_string(),
        address: format!("127.0.0.1:{}", 20_000 + lease_id.len()),
        pid: 1,
        state_path: dir.join("state.json").display().to_string(),
        started_at: "now".to_string(),
        last_seen_at: "now".to_string(),
        build_identity: crate::build_identity::current(),
        binary_sha256: None,
        runtime_paths: crate::daemon::DaemonRuntimeSnapshot {
            loaded_at: "now".to_string(),
            paths: Vec::new(),
        },
    }
}

/// #15830: a daemon generation died mid-workflow in a directory other than
/// the admission owner's. Two replacements race to restart it in place; the
/// directory's exclusive owner lock admits exactly one. The winner takes over
/// the generation's routing and resumes the instance from its checkpoint:
/// every step key is submitted once, every step runs once, and the work is
/// attributed to the lease now executing it.
#[test]
fn a_dead_generations_workflow_is_adopted_once_and_resumes_in_place() {
    with_isolated_home(|home| {
        let runs = home.path().join("runs.log");
        let dead_dir = home.path().join("generations/dead");
        let owner_dir = home.path().join("generations/owner");
        std::fs::create_dir_all(&dead_dir).unwrap();
        std::fs::create_dir_all(&owner_dir).unwrap();
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "adopted",
            "steps": [
                {"id": "evaluate", "exec": sh(&format!("echo evaluate >> {}", runs.display()))},
                {"id": "hold", "needs": ["evaluate"], "approval": {"prompt": "continue?"}},
                {"id": "record", "needs": ["hold"], "exec": sh(&format!("echo record >> {}", runs.display()))}
            ]
        });
        let request = request(definition, home.path());

        // Generation DEAD admitted the instance and committed its first
        // decision; then it died. OWNER serves admission from its own store.
        crate::daemon::generation_store::seed(&generation_state("DEAD", &dead_dir)).unwrap();
        crate::daemon::generation_store::activate(&generation_state("OWNER", &owner_dir)).unwrap();
        let store = JobStore::open_without_reconciliation(dead_dir.join("jobs.json"))
            .unwrap()
            .with_daemon_lease("DEAD".to_string());
        let job_id = admit(&store, &request);
        crate::daemon::generation_store::record_job(&job_id.to_string(), "DEAD").unwrap();
        store.claim_controller_execution(job_id, false).unwrap();
        let (state, commands) = advance(
            &request.definition,
            &request.initial_state(),
            &Event::Start,
            now_ms(),
        );
        commit_without_start(&request, job_id, &state, &commands, &store);
        drop(store);

        // Two replacement daemons race for the dead generation's directory.
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let contenders = ["REPLACEMENT-A", "REPLACEMENT-B"].map(|lease| {
            let barrier = barrier.clone();
            let state_path = dead_dir.join("state.json");
            std::thread::spawn(move || {
                barrier.wait();
                crate::daemon::try_acquire_daemon_owner_lock_at(&state_path)
                    .unwrap()
                    .map(|lock| (lease, lock))
            })
        });
        let mut winners = contenders
            .into_iter()
            .filter_map(|contender| contender.join().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(winners.len(), 1, "exactly one replacement owns the store");
        let (winner, _owner_lock) = winners.pop().unwrap();

        // The winner's startup: take over the generation, then resume.
        let replacement = generation_state(winner, &dead_dir);
        assert_eq!(
            crate::daemon::generation_store::succeed_in_place(&replacement).unwrap(),
            ["DEAD"]
        );
        let restarted = JobStore::open_without_reconciliation(dead_dir.join("jobs.json"))
            .unwrap()
            .with_daemon_lease(winner.to_string());
        crate::daemon::recover_controller_jobs(&restarted);
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            let status = workflow_status(&restarted, &request.instance).unwrap();
            if status["steps"]["hold"]["phase"] == "awaiting_approval" {
                break;
            }
            assert!(Instant::now() < deadline, "{status}");
            std::thread::sleep(Duration::from_millis(50));
        }
        workflow_approve(&restarted, &request.instance, Some(json!({"step": "hold"}))).unwrap();
        assert_eq!(
            wait_terminal(&restarted, job_id, 20).status,
            JobStatus::Succeeded
        );

        assert_eq!(
            std::fs::read_to_string(&runs).unwrap(),
            "evaluate\nrecord\n"
        );
        let children = restarted
            .list()
            .into_iter()
            .filter(|job| {
                restarted
                    .controller_job_state(job.id)
                    .is_ok_and(|state| state.parent_job_id == Some(job_id))
            })
            .collect::<Vec<_>>();
        assert_eq!(children.len(), 2, "one step job per submission key");
        for key in ["evaluate/1/1", "record/1/1"] {
            let child = restarted
                .controller_job_for_key(&format!("{}/{key}", request.instance))
                .unwrap_or_else(|| panic!("{key} was submitted"));
            assert_eq!(child.status, JobStatus::Succeeded);
            assert_eq!(child.daemon_lease_id.as_deref(), Some(winner));
        }
        assert_eq!(
            restarted.get(job_id).unwrap().daemon_lease_id.as_deref(),
            Some(winner),
            "the instance is attributed to the lease executing it"
        );
        let routed = crate::daemon::generation_store::endpoint_for_job(&job_id.to_string())
            .unwrap()
            .unwrap();
        assert_eq!(routed.lease_id, winner);
        assert_eq!(routed.address, replacement.address);
        assert!(crate::daemon::generation_store::endpoint_for_lease("DEAD")
            .unwrap()
            .is_none());
        assert_eq!(
            crate::daemon::generation_store::admitting()
                .unwrap()
                .unwrap()
                .lease_id,
            "OWNER"
        );
    });
}

fn commit_without_start(
    request: &WorkflowJobRequest,
    job_id: Uuid,
    next: &InstanceState,
    commands: &[Command],
    store: &JobStore,
) {
    let admissions = commands
        .iter()
        .filter_map(|command| match command {
            Command::SubmitStep(submission) => Some(submission),
            _ => None,
        })
        .map(|submission| {
            let envelope: crate::daemon::ControllerJobRequest = serde_json::from_value(
                step_request(request, submission)
                    .unwrap()
                    .submission()
                    .unwrap(),
            )
            .unwrap();
            (
                "controller.step".to_string(),
                envelope.idempotency_key.clone(),
                crate::daemon::controller_job_state(&envelope).unwrap(),
            )
        })
        .collect::<Vec<_>>();
    let admitted = store
        .commit_controller_advance(
            job_id,
            to_value(next).unwrap(),
            admissions.clone(),
            Vec::new(),
        )
        .unwrap();
    // A replayed decision returns the same jobs instead of new ones.
    let replayed = store
        .commit_controller_advance(job_id, to_value(next).unwrap(), admissions, Vec::new())
        .unwrap();
    assert_eq!(admitted, replayed);
}

#[test]
fn approval_waits_for_a_recorded_approval() {
    with_isolated_home(|home| {
        let runs = home.path().join("runs.log");
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "gated",
            "steps": [
                {"id": "ship", "approval": {"prompt": "ship {{instance}}?"}},
                {"id": "after", "needs": ["ship"], "exec": sh(&format!("echo after >> {}", runs.display()))}
            ]
        });
        let store = JobStore::open_without_reconciliation(home.path().join("jobs.json"))
            .unwrap()
            .with_daemon_lease("workflow-test".to_string());
        let request = request(definition, home.path());
        let job_id = admit(&store, &request);
        assert!(
            workflow_approve(&store, &request.instance, Some(json!({"step": "ship"}))).is_err()
        );
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        loop {
            let status = workflow_status(&store, &request.instance).unwrap();
            if status["steps"]["ship"]["phase"] == "awaiting_approval" {
                assert_eq!(
                    status["steps"]["ship"]["prompt"],
                    format!("ship {}?", request.instance)
                );
                break;
            }
            assert!(Instant::now() < deadline, "{status}");
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(!runs.exists());
        assert!(
            workflow_approve(&store, &request.instance, Some(json!({"step": "after"}))).is_err()
        );
        workflow_approve(&store, &request.instance, Some(json!({"step": "ship"}))).unwrap();
        assert_eq!(
            wait_terminal(&store, job_id, 15).status,
            JobStatus::Succeeded
        );
        assert_eq!(std::fs::read_to_string(&runs).unwrap(), "after\n");
    });
}

#[test]
fn cancelling_the_instance_cancels_its_running_step() {
    with_isolated_home(|home| {
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "long",
            "steps": [{"id": "sleep", "exec": sh("sleep 60")}]
        });
        let store = JobStore::open_without_reconciliation(home.path().join("jobs.json"))
            .unwrap()
            .with_daemon_lease("workflow-test".to_string());
        let request = request(definition, home.path());
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        let key = format!("{}/sleep/1/1", request.instance);
        let deadline = Instant::now() + Duration::from_secs(10);
        let child = loop {
            if let Some(child) = store.controller_job_for_key(&key) {
                if child.status == JobStatus::Running {
                    break child;
                }
            }
            assert!(Instant::now() < deadline, "step never started");
            std::thread::sleep(Duration::from_millis(50));
        };
        crate::daemon::cancel_controller_job(job_id, "test cancel", &store).unwrap();
        assert_eq!(
            wait_terminal(&store, job_id, 20).status,
            JobStatus::Cancelled
        );
        assert_eq!(
            wait_terminal(&store, child.id, 20).status,
            JobStatus::Cancelled
        );
        assert_eq!(
            workflow_status(&store, &request.instance).unwrap()["status"],
            "cancelled"
        );
    });
}
