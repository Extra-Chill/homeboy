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

#[path = "workflow_notify_queue_tests.rs"]
mod notification_queue_tests;
#[path = "workflow_notify_tests.rs"]
mod notification_tests;

#[test]
fn workflow_job_errors_preserve_redacted_io_and_lock_diagnostics() {
    use crate::daemon::step_job::StepJobDriver;

    let mut io = Error::internal_io(
        "Permission denied; token=private-value",
        Some("create /state/workflows/output.json".into()),
    );
    io.details["env"] = json!({"SECRET": "must-not-be-published"});
    let busy = Error::observation_store_busy("/state/observation.db", "begin commit", 10_000);
    for driver in [
        &WorkflowJobDriver as &dyn ControllerJobDriver,
        &StepJobDriver,
    ] {
        let public = driver.public_error(&io);
        let diagnostic = &public.data["diagnostic"];
        assert_eq!(diagnostic["context"], "create /state/workflows/output.json");
        assert!(diagnostic["error"]
            .as_str()
            .unwrap()
            .contains("Permission denied"));
        assert!(!public.data.to_string().contains("private-value"));
        assert!(!public.data.to_string().contains("must-not-be-published"));
        let public = driver.public_error(&busy);
        assert_eq!(public.data["diagnostic"]["path"], "/state/observation.db");
        assert_eq!(public.data["diagnostic"]["operation"], "begin commit");
        assert_eq!(public.data["diagnostic"]["timeout_ms"], 10_000);
    }
}

fn request(definition: Value, cwd: &Path) -> WorkflowJobRequest {
    register_workflow_step_terminal_consumer(crate::daemon::step_job::STEP_RECEIPT_CONSUMER);
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
        resumed_from: None,
        definition_source: None,
        definition_digest: definition.digest(),
        definition,
        inputs,
        homeboy_program: "/bin/sh".to_string(),
        cwd: cwd.display().to_string(),
        parent: None,
        notification_route: None,
        env,
        homeboy_env: EnvMaterializationPlan::default(),
        programs: BTreeMap::from([("sh".to_string(), "/bin/sh".to_string())]),
    }
}

fn admit(store: &JobStore, request: &WorkflowJobRequest) -> Uuid {
    match admit_with_outcome(store, request).0 {
        crate::api_jobs::ControllerJobSubmissionOutcome::Submitted(id) => id,
        crate::api_jobs::ControllerJobSubmissionOutcome::Existing(job) => job.id,
    }
}

/// Admit the request and report the durable admission outcome, so a test can
/// tell a created instance from an existing one.
fn admit_with_outcome(
    store: &JobStore,
    request: &WorkflowJobRequest,
) -> (crate::api_jobs::ControllerJobSubmissionOutcome, String) {
    register_workflow_job_driver();
    let envelope: crate::daemon::ControllerJobRequest =
        serde_json::from_value(request.submission().unwrap()).unwrap();
    let key = envelope.idempotency_key.clone();
    let state = crate::daemon::controller_job_state(&envelope).unwrap();
    let outcome = store
        .admit_controller_job(
            "controller.workflow".to_string(),
            envelope.idempotency_key,
            state,
        )
        .unwrap();
    (outcome, key)
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

#[test]
fn resume_digest_uses_resolved_inputs_and_requires_success() {
    let old = request(
        json!({
            "schema": "homeboy/workflow/v1", "id": "resume-inputs",
            "inputs": {"value": {"default": "one"}, "unused": {"default": "x"}},
            "steps": [
                {"id": "a", "exec": sh("echo {{inputs.value}}")},
                {"id": "b", "needs": ["a"], "exec": sh("echo b")},
                {"id": "c", "exec": sh("echo c")}
            ]
        }),
        Path::new("/tmp"),
    );
    let mut source = old.initial_state();
    for id in ["a", "b"] {
        source.steps.get_mut(id).unwrap().phase = StepPhase::Done;
        source.steps.get_mut(id).unwrap().outcome = Some("succeeded".into());
    }
    source.steps.get_mut("c").unwrap().phase = StepPhase::Done;
    source.steps.get_mut("c").unwrap().outcome = Some("skipped".into());
    for (input, expected) in [("unused", true), ("value", false)] {
        let mut new = old.clone();
        new.inputs.insert(input.into(), json!("changed"));
        let mut target = new.initial_state();
        reuse_steps(&new, &old, &source, &mut target).unwrap();
        assert_eq!(target.steps["a"].reused, expected);
        assert_eq!(target.steps["b"].reused, expected);
        assert!(!target.steps["c"].reused);
    }
    // Equivalent templated definitions compare after substitution.
    let mut literal = old.clone();
    literal.definition.steps[0].exec.as_mut().unwrap().args[1] = "echo one".into();
    let mut target = literal.initial_state();
    reuse_steps(&literal, &old, &source, &mut target).unwrap();
    assert!(target.steps["a"].reused);
}

#[test]
fn resume_reuses_only_unchanged_succeeded_dependency_chains() {
    with_isolated_home(|home| {
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-resume-test".to_string());
        let definition = json!({
            "schema": "homeboy/workflow/v1", "id": "resume-chain",
            "steps": [
                {"id": "a", "exec": sh("touch a; printf '{\"value\":42}' > \"$HOMEBOY_STEP_OUTPUT\"")},
                {"id": "b", "needs": ["a"], "exec": sh("touch b")},
                {"id": "c", "needs": ["b"], "exec": sh("exit 7")}
            ]
        });
        let original = request(definition.clone(), home.path());
        let job = admit(&store, &original);
        crate::daemon::start_controller_job(job, &store).unwrap();
        assert_eq!(wait_terminal(&store, job, 30).status, JobStatus::Failed);
        std::fs::remove_file(home.path().join("a")).unwrap();
        std::fs::remove_file(home.path().join("b")).unwrap();

        for change_b in [false, true] {
            let mut fixed = definition.clone();
            fixed["steps"][2]["exec"] = sh("touch c");
            if change_b {
                fixed["steps"][1]["exec"] = sh("touch b; echo changed");
            }
            let mut resumed = request(fixed, home.path());
            resumed.resumed_from = Some(original.instance.clone());
            resumed.instance = resumed.definition.instance_id_with_resume(
                &resumed.inputs,
                None,
                resumed.resumed_from.as_deref(),
            );
            assert_ne!(
                resumed.instance,
                resumed.definition.instance_id(&resumed.inputs, None)
            );
            let job = admit(&store, &resumed);
            crate::daemon::start_controller_job(job, &store).unwrap();
            assert_eq!(wait_terminal(&store, job, 30).status, JobStatus::Succeeded);
            let view = workflow_status(&store, &resumed.instance).unwrap();
            assert_eq!(view["resumed_from"], original.instance);
            assert_eq!(view["steps"]["a"]["reused"], true);
            assert_eq!(view["steps"]["b"]["reused"], !change_b);
            assert_eq!(view["steps"]["c"]["launched"], true);
            assert!(!home.path().join("a").exists());
            assert_eq!(home.path().join("b").exists(), change_b);
            assert!(home.path().join("c").exists());
            let controller = store.controller_job_state(job).unwrap();
            let state: InstanceState =
                serde_json::from_value(controller.checkpoint.unwrap()).unwrap();
            assert_eq!(state.steps["a"].output, Some(json!({"value": 42})));
            assert_eq!(state.step_runs, if change_b { 2 } else { 1 });
            std::fs::remove_file(home.path().join("c")).unwrap();
        }
    });
}

#[test]
fn resume_from_a_next_pass_instance_keeps_its_iteration_and_previous() {
    with_isolated_home(|home| {
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-resume-pass-test".into());
        let definition = json!({
            "schema": "homeboy/workflow/v1", "id": "resume-pass",
            "next_pass": {"max_iterations": 3},
            "steps": [
                {"id": "import", "exec": sh("touch import-{{inputs.iteration}}")},
                {"id": "evaluate", "needs": ["import"], "exec": sh("touch evaluate")},
                {"id": "finish", "needs": ["evaluate"], "exec": sh("exit 7")}
            ]
        });
        let mut original = request(definition.clone(), home.path());
        let previous =
            json!({"outputs": {"evaluate": {"verdict": "retry"}}, "unchanged_passes": 1});
        original.inputs.insert("iteration".into(), json!(3));
        original.inputs.insert("previous".into(), previous.clone());
        original.instance = original.definition.instance_id(&original.inputs, None);
        let job = admit(&store, &original);
        crate::daemon::start_controller_job(job, &store).unwrap();
        assert_eq!(wait_terminal(&store, job, 30).status, JobStatus::Failed);
        std::fs::remove_file(home.path().join("import-3")).unwrap();
        std::fs::remove_file(home.path().join("evaluate")).unwrap();

        let mut fixed = definition;
        fixed["steps"][2]["exec"] = sh("touch finish");
        let mut resumed = request(fixed, home.path());
        assert_eq!(resumed.inputs["iteration"], 1);
        resumed
            .definition
            .carry_resumed_pass(&mut resumed.inputs, &original.inputs);
        resumed.resumed_from = Some(original.instance.clone());
        resumed.instance = resumed.definition.instance_id_with_resume(
            &resumed.inputs,
            None,
            resumed.resumed_from.as_deref(),
        );
        let job = admit(&store, &resumed);
        crate::daemon::start_controller_job(job, &store).unwrap();
        assert_eq!(wait_terminal(&store, job, 30).status, JobStatus::Succeeded);
        let view = workflow_status(&store, &resumed.instance).unwrap();
        assert_eq!(view["inputs"]["iteration"], 3);
        assert_eq!(view["inputs"]["previous"], previous);
        assert_eq!(view["steps"]["import"]["reused"], true);
        assert_eq!(view["steps"]["evaluate"]["reused"], true);
        assert!(!home.path().join("import-3").exists());
        assert!(!home.path().join("evaluate").exists());
        assert!(home.path().join("finish").exists());
    });
}

#[test]
fn workflow_history_survives_unrelated_terminal_byte_budget_churn() {
    with_isolated_home(|home| {
        let path = home.path().join(crate::paths::OBSERVATION_DB_FILENAME);
        let store = JobStore::open_without_reconciliation(&path)
            .unwrap()
            .with_daemon_lease("workflow-retention-test".to_string());
        let original = request(
            json!({
                "schema": "homeboy/workflow/v1", "id": "retained",
                "steps": [{"id": "a", "exec": sh("echo done")}]
            }),
            home.path(),
        );
        let job = admit(&store, &original);
        crate::daemon::start_controller_job(job, &store).unwrap();
        assert_eq!(wait_terminal(&store, job, 30).status, JobStatus::Succeeded);

        // Each unrelated result exceeds the default 1 MiB budget. Multiple
        // completions must evict old jobs, never the workflow checkpoint.
        let mut first = None;
        for _ in 0..3 {
            let unrelated = store.create("unrelated");
            first.get_or_insert(unrelated.id);
            store.start(unrelated.id).unwrap();
            store
                .complete(
                    unrelated.id,
                    Some(json!({"payload": "x".repeat(1_100_000)})),
                )
                .unwrap();
        }
        assert!(store.get(first.unwrap()).is_err());
        drop(store);
        let store = JobStore::open_without_reconciliation(&path)
            .unwrap()
            .with_daemon_lease("workflow-retention-test".to_string());
        assert_eq!(
            workflow_status(&store, &original.instance).unwrap()["status"],
            "succeeded"
        );

        let mut resumed = original.clone();
        resumed.resumed_from = Some(original.instance.clone());
        resumed.instance = resumed.definition.instance_id_with_resume(
            &resumed.inputs,
            None,
            resumed.resumed_from.as_deref(),
        );
        let job = admit(&store, &resumed);
        crate::daemon::start_controller_job(job, &store).unwrap();
        assert_eq!(wait_terminal(&store, job, 30).status, JobStatus::Succeeded);
        assert_eq!(
            workflow_status(&store, &resumed.instance).unwrap()["steps"]["a"]["reused"],
            true
        );
    });
}

#[test]
fn workflow_expiry_reports_when_and_persists_a_small_marker() {
    with_isolated_home(|home| {
        let path = home.path().join(crate::paths::OBSERVATION_DB_FILENAME);
        let store = JobStore::open_without_reconciliation(&path)
            .unwrap()
            .with_daemon_lease("workflow-expiry-test".to_string());
        let original = request(
            json!({
                "schema": "homeboy/workflow/v1", "id": "expires",
                "steps": [{"id": "a", "exec": sh("echo done")}]
            }),
            home.path(),
        );
        let job = admit(&store, &original);
        crate::daemon::start_controller_job(job, &store).unwrap();
        let terminal = wait_terminal(&store, job, 30);
        assert!(workflow_status(&store, &original.instance).is_ok());
        let mut config = crate::defaults::load_config();
        config.retention.workflow_instance_days = 0;
        crate::defaults::save_config(&config).unwrap();
        // A read enforces the deadline even without a compaction write.
        let error = workflow_status(&store, &original.instance)
            .unwrap_err()
            .to_string();
        assert!(error.contains("expired at"), "{error}");
        assert!(
            error.contains(&terminal.finished_at_ms.unwrap().to_string()),
            "{error}"
        );
        store.create("trigger-expiry-compaction");
        drop(store);
        let store = JobStore::open_without_reconciliation(&path).unwrap();
        let marker = store.controller_job_state(job).unwrap();
        assert_eq!(
            marker.request,
            json!({"expired_at_ms": terminal.finished_at_ms.unwrap()})
        );
        assert!(marker.checkpoint.is_none());
        assert!(store.events(job).unwrap().is_empty());
        assert!(workflow_status(&store, &original.instance)
            .unwrap_err()
            .to_string()
            .contains("expired at"));
    });
}

#[test]
fn failed_step_cause_reaches_the_workflow_status() {
    with_isolated_home(|home| {
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-failure-test".to_string());
        let request = request(
            json!({
                "schema": "homeboy/workflow/v1",
                "id": "failure-cause",
                "steps": [{"id": "prepare", "exec": sh("echo repository-does-not-exist >&2; exit 7")}]
            }),
            home.path(),
        );
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        assert_eq!(wait_terminal(&store, job_id, 30).status, JobStatus::Failed);

        let initial = InstanceState::new(
            &request.definition,
            &request.instance,
            request.inputs.clone(),
        );
        let (running, _) = advance(&request.definition, &initial, &Event::Start, 0);
        let key = running.steps["prepare"].key.as_deref().unwrap();
        let result = step_result(&request, &running, key, &store).unwrap();
        let error = result.error.clone().expect("failed child has an error");
        assert!(error.contains("repository-does-not-exist"), "{error}");
        let reason = format!("step `prepare` did not succeed: {error}");
        let (finished, _) = advance(
            &request.definition,
            &running,
            &Event::StepFinished(result),
            1,
        );
        assert_eq!(
            finished.steps["prepare"].error.as_deref(),
            Some(error.as_str())
        );
        assert_eq!(finished.reason.as_deref(), Some(reason.as_str()));
        let status = workflow_status(&store, &request.instance).unwrap();
        assert_eq!(status["steps"]["prepare"]["error"], error);
        assert_eq!(status["reason"], reason);
    });
}

/// An output gate on an upstream step decides whether the next step runs:
/// `evaluate` reports findings, `fix` runs because of it, and a step whose
/// gate is false is skipped. Every step job runs exactly once.
#[test]
fn when_gates_launch_the_next_step_and_skip_the_rest() {
    with_isolated_home(|home| {
        let runs = home.path().join("runs.log");
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "proof",
            "steps": [
                {"id": "evaluate", "exec": sh(&format!(
                    "echo evaluate >> {runs}; \
                     printf '{{\"verdict\":\"findings\"}}' > \"$HOMEBOY_STEP_OUTPUT\"",
                    runs = runs.display()))},
                {"id": "fix", "needs": ["evaluate"],
                 "when": {"step": "evaluate", "output": "/verdict", "equals": "findings"},
                 "exec": sh(&format!("echo fix >> {}", runs.display()))},
                {"id": "release", "needs": ["evaluate"],
                 "when": {"step": "evaluate", "output": "/verdict", "equals": "accepted"},
                 "exec": sh(&format!("echo release >> {}", runs.display()))}
            ]
        });
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-test".to_string());
        let request = request(definition, home.path());
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        let job = wait_terminal(&store, job_id, 30);
        assert_eq!(job.status, JobStatus::Succeeded);
        let status = workflow_status(&store, &request.instance).unwrap();
        assert_eq!(status["status"], "succeeded");
        assert_eq!(status["steps"]["fix"]["outcome"], "succeeded");
        assert_eq!(status["steps"]["release"]["outcome"], "skipped");
        assert_eq!(std::fs::read_to_string(&runs).unwrap(), "evaluate\nfix\n");
        for key in ["evaluate/1/1", "fix/1/1"] {
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
            .controller_job_for_key(&format!("{}/release/1/1", request.instance))
            .is_none());
    });
}

/// Exercise the serving loop, live step supervision, and startup recovery
/// together. The service-manager fixture owns replacement, just as systemd
/// does in production; no alternate workflow adoption path is involved.
#[test]
fn stale_daemon_hands_live_workflow_to_replacement_at_step_boundary() {
    use crate::daemon::*;
    use crate::paths;
    use std::net::TcpListener;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    with_isolated_home(|home| {
        let wait = |condition: &dyn Fn() -> bool| {
            let deadline = Instant::now() + Duration::from_secs(20);
            while !condition() {
                assert!(Instant::now() < deadline, "handoff timed out");
                std::thread::sleep(Duration::from_millis(20));
            }
        };
        let store = JobStore::open_without_reconciliation(paths::observation_db().unwrap())
            .unwrap()
            .with_daemon_scope(daemon_scope_for_state_path(&state_path().unwrap()));
        let started = home.path().join("started");
        let release = home.path().join("release");
        let next = home.path().join("next");
        let request = request(
            json!({
                "schema": "homeboy/workflow/v1", "id": "stale-handoff",
                "steps": [
                    {"id": "a", "exec": sh(&format!(
                        "touch {}; while [ ! -f {} ]; do sleep 0.05; done",
                        started.display(), release.display()))},
                    {"id": "b", "needs": ["a"], "exec": sh(&format!("touch {}", next.display()))}
                ]
            }),
            home.path(),
        );
        let job_id = admit(&store, &request);
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let owner = acquire_daemon_owner_lock().unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let flag = shutdown.clone();
        let server = std::thread::spawn(move || {
            serve_listener_with_analysis_runner_locked(
                listener,
                UnsupportedAnalysisJobRunner,
                owner,
                None,
                Some(flag),
                Some(DaemonServiceManager::Systemd),
            )
        });
        wait(&|| started.exists());
        let lease: DaemonState =
            serde_json::from_slice(&std::fs::read(state_path().unwrap()).unwrap()).unwrap();
        let snapshot = || {
            JobStore::open_without_reconciliation(paths::observation_db().unwrap())
                .unwrap()
                .with_daemon_scope(daemon_scope_for_state_path(&state_path().unwrap()))
                .with_daemon_lease(lease.lease_id.clone())
        };
        let store = snapshot();
        // Installation made the serving lease stale. Upgrade schedules its
        // existing exact-lease drain rather than killing the protected child.
        let mut stale = lease.clone();
        stale.build_identity.version = "0.0.0".to_string();
        std::fs::write(state_path().unwrap(), serde_json::to_vec(&stale).unwrap()).unwrap();
        drain_then_replace(&lease.lease_id).unwrap();
        let runtime = controller_job_runtime_key(&store, job_id);
        wait(&|| {
            !controller_job_runtimes()
                .lock()
                .unwrap()
                .contains_key(&runtime)
        });
        let store = snapshot();
        let child = store
            .controller_job_for_key(&format!("{}/a/1/1", request.instance))
            .unwrap();
        let evidence =
            store.active_daemon_job_recovery_evidence(Some(&lease.lease_id), pid_is_running);
        assert_eq!(
            evidence
                .iter()
                .find(|e| e.job_id == job_id)
                .unwrap()
                .disposition,
            crate::api_jobs::DaemonActiveJobRecoveryDisposition::DriverRecovery
        );
        assert!(evidence
            .iter()
            .find(|e| e.job_id == child.id)
            .unwrap()
            .disposition
            .blocks_daemon_replacement());
        assert!(
            !server.is_finished(),
            "protected step still holds the old daemon"
        );
        assert!(!maintenance_allowed());
        assert!(!next.exists());
        assert!(store
            .controller_job_for_key(&format!("{}/b/1/1", request.instance))
            .is_none());
        std::fs::write(&release, "go").unwrap();
        wait(&|| server.is_finished());
        server.join().unwrap().unwrap();
        let store = snapshot();
        assert_eq!(store.get(child.id).unwrap().status, JobStatus::Succeeded);
        assert!(!next.exists(), "old daemon must not schedule the next step");
        assert!(!store.get(job_id).unwrap().status.is_terminal());
        assert!(store
            .controller_job_state(job_id)
            .unwrap()
            .checkpoint
            .is_some());

        // A service manager starts the replacement. Its normal startup calls
        // recover_controller_jobs and adopts the checkpoint under its new lease.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let owner = acquire_daemon_owner_lock().unwrap();
        let flag = shutdown.clone();
        let replacement = std::thread::spawn(move || {
            serve_listener_with_analysis_runner_locked(
                listener,
                UnsupportedAnalysisJobRunner,
                owner,
                None,
                Some(flag),
                Some(DaemonServiceManager::Systemd),
            )
        });
        wait(&|| next.exists());
        wait(&|| snapshot().get(job_id).unwrap().status.is_terminal());
        let adopted = snapshot().get(job_id).unwrap();
        assert_eq!(adopted.status, JobStatus::Succeeded);
        assert_ne!(
            adopted.daemon_lease_id.as_deref(),
            Some(lease.lease_id.as_str())
        );
        shutdown.store(true, Ordering::Release);
        wait(&|| replacement.is_finished());
        replacement.join().unwrap().unwrap();
    });
}

/// The daemon committed a decision (checkpoint + queued step job) and died
/// before starting the step. Recovery resumes the instance from its
/// checkpoint, starts the admitted step once, and finishes.
#[test]
fn recovery_after_a_commit_starts_each_admitted_step_exactly_once() {
    with_isolated_home(|home| {
        let runs = home.path().join("runs.log");
        let path = home.path().join(crate::paths::OBSERVATION_DB_FILENAME);
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

/// #15830: a daemon died mid-workflow. Two replacements race to restart it in
/// place; the directory's exclusive owner lock admits exactly one. The winner
/// resumes the instance from its checkpoint: every step key is submitted once,
/// every step runs once, and the work is attributed to the lease now executing
/// it.
#[test]
fn a_dead_daemons_workflow_is_adopted_once_and_resumes_in_place() {
    with_isolated_home(|home| {
        let runs = home.path().join("runs.log");
        let dead_dir = home.path().join("daemon");
        std::fs::create_dir_all(&dead_dir).unwrap();
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

        // Daemon DEAD admitted the instance and committed its first decision;
        // then it died.
        let store = JobStore::open_without_reconciliation(
            dead_dir.join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("DEAD".to_string());
        let job_id = admit(&store, &request);
        store.claim_controller_execution(job_id, false).unwrap();
        let (state, commands) = advance(
            &request.definition,
            &request.initial_state(),
            &Event::Start,
            now_ms(),
        );
        commit_without_start(&request, job_id, &state, &commands, &store);
        drop(store);

        // Two replacement daemons race for the dead daemon's directory.
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

        // The winner's startup resumes the checkpointed instance.
        let restarted = JobStore::open_without_reconciliation(
            dead_dir.join(crate::paths::OBSERVATION_DB_FILENAME),
        )
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
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
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
fn approval_decisions_route_branches_and_template_feedback() {
    for decision in ["approved", "rejected"] {
        with_isolated_home(|home| {
            let runs = home.path().join("decision.log");
            let definition = json!({
                "schema": "homeboy/workflow/v1", "id": "decision",
                "steps": [
                    {"id": "review", "approval": {"prompt": "ship?"}},
                    {"id": "merge", "needs": ["review"],
                     "when": {"step": "review", "output": "/decision", "equals": "approved"},
                     "exec": sh(&format!("echo merge >> {}", runs.display()))},
                    {"id": "revise", "needs": ["review"],
                     "when": {"step": "review", "output": "/decision", "equals": "rejected"},
                     "exec": sh(&format!("echo '{{{{steps.review.output/reason}}}}' >> {}", runs.display()))},
                    {"id": "default", "needs": ["review"],
                     "exec": sh(&format!("echo default >> {}", runs.display()))}
                ]
            });
            let store = JobStore::open_without_reconciliation(
                home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
            )
            .unwrap()
            .with_daemon_lease("workflow-test".to_string());
            let request = request(definition, home.path());
            let job_id = admit(&store, &request);
            crate::daemon::start_controller_job(job_id, &store).unwrap();
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                let status = workflow_status(&store, &request.instance).unwrap();
                if status["steps"]["review"]["phase"] == "awaiting_approval" {
                    break;
                }
                assert!(Instant::now() < deadline, "{status}");
                std::thread::sleep(Duration::from_millis(50));
            }
            let mut body =
                json!({"step": "review", "reason": "fix projection", "actor": "reviewer"});
            // The existing approval API defaults to approved for compatibility.
            if decision == "rejected" {
                body["decision"] = json!(decision);
            }
            workflow_approve(&store, &request.instance, Some(body)).unwrap();
            assert_eq!(
                wait_terminal(&store, job_id, 15).status,
                JobStatus::Succeeded
            );
            let status = workflow_status(&store, &request.instance).unwrap();
            assert_eq!(status["steps"]["review"]["output"]["decision"], decision);
            assert_eq!(
                status["steps"]["review"]["output"]["reason"],
                "fix projection"
            );
            assert_eq!(status["steps"]["review"]["output"]["actor"], "reviewer");
            if decision == "rejected" {
                assert_eq!(status["steps"]["review"]["outcome"], "rejected");
                assert_eq!(status["steps"]["merge"]["outcome"], "skipped");
                assert_eq!(status["steps"]["default"]["outcome"], "skipped");
                assert_eq!(std::fs::read_to_string(&runs).unwrap(), "fix projection\n");
            } else {
                assert_eq!(status["steps"]["revise"]["outcome"], "skipped");
                let output = std::fs::read_to_string(&runs).unwrap();
                assert!(output.contains("merge\n") && output.contains("default\n"));
            }
        });
    }
}

#[test]
fn cancelling_the_instance_cancels_its_running_step() {
    with_isolated_home(|home| {
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "long",
            "steps": [{"id": "sleep", "exec": sh("sleep 60")}]
        });
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
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

fn fan_out(join: Value, item_script: &str) -> Value {
    json!({
        "schema": "homeboy/workflow/v1",
        "id": "fan",
        "steps": [
            {"id": "prepare", "exec": sh(
                "printf '{\"children\":[{\"n\":1},{\"n\":2},{\"n\":3}]}' > \"$HOMEBOY_STEP_OUTPUT\"")},
            {"id": "work", "needs": ["prepare"],
             "foreach": "{{steps.prepare.output/children}}", "max_parallel": 2, "join": join,
             "exec": {"program": "sh", "args": ["-c", item_script, "item", "{{item.n}}"]}},
            {"id": "report", "needs": ["work"], "always": true, "exec": sh("true")}
        ]
    })
}

/// Three items, two at a time, item 2 fails: `join: all` fails the instance
/// and `min_succeeded: 2` passes it. No more than two items ever run at once,
/// and the always-on report runs either way.
#[test]
fn fan_out_runs_items_two_at_a_time_and_joins() {
    for (join, expected) in [
        (json!("all"), JobStatus::Failed),
        (json!({"min_succeeded": 2}), JobStatus::Succeeded),
    ] {
        with_isolated_home(|home| {
            let log = home.path().join("items.log");
            let script = format!(
                "echo start-$1 >> {log}; \
                 running=$(( $(grep -c start {log}) - $(grep -c end {log}) )); \
                 echo running-$running >> {log}; sleep 0.3; echo end-$1 >> {log}; \
                 [ \"$1\" != 2 ]",
                log = log.display()
            );
            let store = JobStore::open_without_reconciliation(
                home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
            )
            .unwrap()
            .with_daemon_lease("workflow-test".to_string());
            let request = request(fan_out(join.clone(), &script), home.path());
            let job_id = admit(&store, &request);
            crate::daemon::start_controller_job(job_id, &store).unwrap();
            assert_eq!(wait_terminal(&store, job_id, 60).status, expected, "{join}");
            let status = workflow_status(&store, &request.instance).unwrap();
            let items = status["steps"]["work"]["items"].as_array().unwrap();
            assert_eq!(
                items
                    .iter()
                    .map(|item| item["outcome"].as_str().unwrap())
                    .collect::<Vec<_>>(),
                vec!["succeeded", "failed", "succeeded"]
            );
            assert_eq!(status["steps"]["report"]["outcome"], "succeeded");
            let log = std::fs::read_to_string(&log).unwrap();
            assert_eq!(log.matches("start-").count(), 3, "{log}");
            assert!(!log.contains("running-3"), "more than two items ran: {log}");
            for index in 0..3 {
                let key = format!("{}/work/1/item-{index}/1", request.instance);
                let child = store.controller_job_for_key(&key).unwrap();
                assert_eq!(
                    store.controller_job_state(child.id).unwrap().parent_job_id,
                    Some(job_id)
                );
            }
        });
    }
}

/// A store that retains one terminal job compacts each settled item as soon
/// as the next one settles, usually before the driver's next poll. The
/// driver still observes every item from its durable tombstone and finishes.
#[test]
fn items_compacted_before_the_driver_observes_them_still_settle() {
    with_isolated_home(|home| {
        let store = JobStore::open_with_retention(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
            100,
            1,
        )
        .unwrap()
        .with_daemon_lease("workflow-test".to_string());
        let request = request(fan_out(json!("all"), "true"), home.path());
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        assert_eq!(
            wait_terminal(&store, job_id, 30).status,
            JobStatus::Succeeded
        );
        let status = workflow_status(&store, &request.instance).unwrap();
        let items = status["steps"]["work"]["items"].as_array().unwrap();
        assert!(
            items.iter().all(|item| item["outcome"] == "succeeded"),
            "{status}"
        );
        let compacted = (0..3)
            .map(|index| format!("{}/work/1/item-{index}/1", request.instance))
            .filter(|key| {
                store
                    .controller_job_for_key(key)
                    .is_some_and(|child| store.get(child.id).is_err())
            })
            .count();
        assert!(compacted >= 2, "items were not compacted: {status}");
    });
}

/// Cancelling mid-wave stops every running item and admits no further one.
#[test]
fn cancelling_mid_wave_stops_every_running_item() {
    with_isolated_home(|home| {
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-test".to_string());
        let request = request(fan_out(json!("all"), "sleep 60"), home.path());
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        let key = |index: usize| format!("{}/work/1/item-{index}/1", request.instance);
        let deadline = Instant::now() + Duration::from_secs(20);
        let running = loop {
            let running = (0..2)
                .filter_map(|index| store.controller_job_for_key(&key(index)))
                .filter(|child| child.status == JobStatus::Running)
                .collect::<Vec<_>>();
            if running.len() == 2 {
                break running;
            }
            assert!(Instant::now() < deadline, "the first wave never started");
            std::thread::sleep(Duration::from_millis(50));
        };
        crate::daemon::cancel_controller_job(job_id, "operator cancel", &store).unwrap();
        assert_eq!(
            wait_terminal(&store, job_id, 20).status,
            JobStatus::Cancelled
        );
        for child in running {
            assert_eq!(
                wait_terminal(&store, child.id, 20).status,
                JobStatus::Cancelled
            );
        }
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            store.controller_job_for_key(&key(2)).is_none(),
            "a cancelled fan-out admitted another item"
        );
        assert_eq!(
            workflow_status(&store, &request.instance).unwrap()["status"],
            "cancelled"
        );
    });
}

#[test]
fn a_cook_step_that_exits_without_recording_its_run_fails() {
    assert_eq!(
        cook_step_outcome(Some(WorkflowRunOutcome::Unknown), FinishedStatus::Succeeded),
        Some(Some("failed".to_string())),
        "a clean exit with no Cook record must not settle as succeeded"
    );
    assert_eq!(
        cook_step_outcome(Some(WorkflowRunOutcome::Unknown), FinishedStatus::Cancelled),
        Some(None),
        "a cancelled step keeps its own status"
    );
    assert_eq!(
        cook_step_outcome(Some(WorkflowRunOutcome::Pending), FinishedStatus::Succeeded),
        None
    );
    assert_eq!(
        cook_step_outcome(
            Some(WorkflowRunOutcome::Settled {
                outcome: "succeeded".to_string(),
                reason: None,
            }),
            FinishedStatus::Succeeded
        ),
        Some(Some("succeeded".to_string()))
    );
}

/// #17758: a failed cook foreach item reports its Cook's terminal reason and
/// the Cook id it launched, not the exit JSON of the process that waited on it.
#[test]
fn a_failed_cook_item_error_carries_the_cooks_terminal_reason() {
    with_isolated_home(|home| {
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-cook-reason-test".to_string());
        let request = request(
            json!({
                "schema": "homeboy/workflow/v1",
                "id": "cook-reason",
                "steps": [
                    {"id": "prepare", "exec": sh(
                        "printf '{\"children\":[{\"n\":1}]}' > \"$HOMEBOY_STEP_OUTPUT\"")},
                    {"id": "cook", "needs": ["prepare"],
                     "foreach": "{{steps.prepare.output/children}}",
                     "cook": {"prompt": "fix {{item.n}}"}}
                ]
            }),
            home.path(),
        );
        let instance = request.instance.clone();
        let prefix = instance.clone();
        register_workflow_run_outcome_resolver(Arc::new(move |cook_id: &str| {
            if cook_id.starts_with(&prefix) {
                WorkflowRunOutcome::Settled {
                    outcome: "failed".to_string(),
                    reason: Some("gate `cargo-test` failed".to_string()),
                }
            } else {
                WorkflowRunOutcome::Unknown
            }
        }));
        let job_id = admit(&store, &request);
        crate::daemon::start_controller_job(job_id, &store).unwrap();
        assert_eq!(wait_terminal(&store, job_id, 30).status, JobStatus::Failed);

        let status = workflow_status(&store, &instance).unwrap();
        let item = &status["steps"]["cook"]["items"][0];
        let cook_id = format!("{instance}-cook-i1-n0-a1");
        assert_eq!(item["run_id"], cook_id.as_str(), "{status}");
        assert_eq!(item["outcome"], "failed", "{status}");
        assert_eq!(
            item["error"],
            format!("Cook `{cook_id}` ended `failed`: gate `cargo-test` failed"),
            "{status}"
        );
    });
}

/// A self-admitting chain: the `again` step runs `workflow run <same
/// definition> --input iteration=N+1`, gated on the evaluate output. The step
/// process sees `HOMEBOY_WORKFLOW_INSTANCE`, so `workflow run` records the
/// admitting instance as the child's `parent`; a replayed admission converges
/// on the existing child; and a watcher follows the chain to its last
/// instance and reports that instance's terminal status.
#[test]
fn a_step_that_readmits_the_definition_chains_child_instances() {
    with_isolated_home(|home| {
        let definition = json!({
            "schema": "homeboy/workflow/v1",
            "id": "chain",
            "inputs": {"iteration": {"default": 1}},
            "steps": [
                {"id": "evaluate",
                 "exec": sh("printf '{\"iteration\":{{inputs.iteration}}}' > \"$HOMEBOY_STEP_OUTPUT\"")},
                {"id": "again", "needs": ["evaluate"],
                 "when": {"step": "evaluate", "output": "/iteration", "lt": 3},
                 "homeboy": ["workflow", "run", "-", "--input", "iteration=2"]}
            ]
        });
        // Stands in for the `homeboy` binary: records the instance environment
        // its step ran with, which is what `workflow run` reads at admission.
        let parent_env = home.path().join("step-instance.txt");
        homeboy_test_support::write_executable(
            home.path().join("fake-homeboy"),
            format!(
                "#!/bin/sh\nprintf '%s' \"$HOMEBOY_WORKFLOW_INSTANCE\" > {}\n",
                parent_env.display()
            ),
            0o755,
        )
        .unwrap();
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-chain-test".to_string());

        // Pass 1: the parent instance runs its `again` step.
        let mut parent = request(definition.clone(), home.path());
        parent.homeboy_program = home.path().join("fake-homeboy").display().to_string();
        let parent_id = parent.instance.clone();
        let parent_job = admit(&store, &parent);
        crate::daemon::start_controller_job(parent_job, &store).unwrap();
        assert_eq!(
            wait_terminal(&store, parent_job, 30).status,
            JobStatus::Succeeded
        );
        assert_eq!(
            std::fs::read_to_string(&parent_env).unwrap(),
            parent_id,
            "the re-admitting step ran with HOMEBOY_WORKFLOW_INSTANCE set"
        );

        // `workflow run` inside that step admits pass 2 with the parent env.
        let chain_definition = WorkflowDefinition::from_value(definition.clone()).unwrap();
        chain_definition.validate(&|_| true).unwrap();
        let step_program = parent.homeboy_program.clone();
        let child_request = |iteration: i64, parent: &str| {
            let provided = BTreeMap::from([("iteration".to_string(), json!(iteration))]);
            let mut child = request(definition.clone(), home.path());
            child.homeboy_program = step_program.clone();
            child.inputs = chain_definition
                .resolve_inputs(&provided)
                .expect("declared input");
            child.instance = chain_definition.instance_id(&child.inputs, None);
            child.definition_digest = chain_definition.digest();
            child.parent = Some(parent.to_string());
            child
        };
        let child = child_request(2, &parent_id);
        let (outcome, _) = admit_with_outcome(&store, &child);
        assert!(
            matches!(
                outcome,
                crate::api_jobs::ControllerJobSubmissionOutcome::Submitted(_)
            ),
            "the first admission creates the child"
        );
        // A replayed admission of the same definition and inputs converges on
        // the existing child instance.
        let child_job = store.controller_job_for_key(&child.instance).unwrap().id;
        let (replay, _) = admit_with_outcome(&store, &child);
        match replay {
            crate::api_jobs::ControllerJobSubmissionOutcome::Existing(job) => {
                assert_eq!(job.id, child_job, "the replay returned the existing child")
            }
            crate::api_jobs::ControllerJobSubmissionOutcome::Submitted(_) => {
                panic!("the replayed admission created a second child")
            }
        }
        crate::daemon::start_controller_job(child_job, &store).unwrap();
        assert_eq!(
            wait_terminal(&store, child_job, 30).status,
            JobStatus::Succeeded
        );
        assert_eq!(
            std::fs::read_to_string(&parent_env).unwrap(),
            child.instance,
            "the child's own step env named the child"
        );

        // Pass 3, admitted by pass 2's step. Its gate is false, so the chain
        // ends here.
        let grandchild = child_request(3, &child.instance);
        let grandchild_job = admit(&store, &grandchild);
        crate::daemon::start_controller_job(grandchild_job, &store).unwrap();
        assert_eq!(
            wait_terminal(&store, grandchild_job, 30).status,
            JobStatus::Succeeded
        );

        // The chain is visible from both ends.
        let parent_view = workflow_status(&store, &parent_id).unwrap();
        assert_eq!(parent_view["parent"], Value::Null);
        assert_eq!(
            parent_view["children"],
            json!([child.instance]),
            "the parent lists exactly its child"
        );
        let child_view = workflow_status(&store, &child.instance).unwrap();
        assert_eq!(child_view["parent"], json!(parent_id));
        assert_eq!(child_view["children"], json!([grandchild.instance]));
        let grandchild_view = workflow_status(&store, &grandchild.instance).unwrap();
        assert_eq!(grandchild_view["parent"], json!(child.instance));
        assert_eq!(grandchild_view["children"], json!([]));

        // `--wait` follows the chain from its head to the last instance.
        let mut watched = parent_view;
        while let Some(next) = chain_successor(&watched) {
            watched = workflow_status(&store, &next).unwrap();
        }
        assert_eq!(watched["instance"], json!(grandchild.instance));
        assert_eq!(watched["status"], "succeeded");
    });
}

#[test]
fn next_pass_validation_failure_records_terminal_reason() {
    with_isolated_home(|home| {
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-next-pass-validation-test".into());
        let definition = json!({
            "schema": "homeboy/workflow/v1", "id": "invalid-next-pass",
            "next_pass": {"max_iterations": 2},
            "steps": [{"id": "work", "exec": sh(
                "printf '%s' '{\"schema\":\"homeboy/workflow/v1\",\"id\":\"invalid-next-pass\",\"steps\":[]}' > loop.json"
            )}]
        });
        let source = home.path().join("loop.json");
        std::fs::write(&source, serde_json::to_string(&definition).unwrap()).unwrap();
        let mut original = request(definition, home.path());
        original.definition_source = Some(source.display().to_string());
        let job = admit(&store, &original);
        crate::daemon::start_controller_job(job, &store).unwrap();
        assert_eq!(wait_terminal(&store, job, 30).status, JobStatus::Failed);

        let view = workflow_status(&store, &original.instance).unwrap();
        assert_eq!(view["status"], "failed");
        let invalid = WorkflowDefinition::parse(&std::fs::read_to_string(source).unwrap())
            .unwrap()
            .validate(&|_| true)
            .unwrap_err();
        let reason = format!("next_pass not admitted: {invalid}");
        assert_eq!(view["reason"], reason);
        assert_eq!(view["children"], json!([]));
        let checkpoint = store.controller_job_state(job).unwrap().checkpoint.unwrap();
        assert_eq!(checkpoint["status"], "failed");
        assert_eq!(checkpoint["reason"], reason);
    });
}

#[test]
fn native_next_pass_forwards_inputs_and_stops_a_stable_chain() {
    with_isolated_home(|home| {
        let store = JobStore::open_without_reconciliation(
            home.path().join(crate::paths::OBSERVATION_DB_FILENAME),
        )
        .unwrap()
        .with_daemon_lease("workflow-next-pass-test".into());
        for (id, cap, stable, expected, ended) in [
            (
                "stable",
                8,
                3,
                3,
                "chain ended: step `sample` output `/value` unchanged for 3 passes",
            ),
            ("capped", 2, 8, 2, "chain ended: max_iterations 2 reached"),
        ] {
            let original = request(
                json!({
                    "schema": "homeboy/workflow/v1", "id": id,
                    "inputs": {"carried": {"default": "keep"}, "changed": {"default": "first"}},
                    "next_pass": {"max_iterations": cap,
                        "inputs": {"changed": "{{steps.sample.output/value}}"},
                        "stop_unchanged": {"step": "sample", "output": "/value", "passes": stable}},
                    "steps": [
                        {"id": "work", "foreach": ["a", "b"], "max_parallel": 1,
                         "join": {"min_succeeded": 0}, "exec": sh("exit 1")},
                        {"id": "sample", "needs": ["work"],
                         "exec": sh("printf '{\"value\":42}' > \"$HOMEBOY_STEP_OUTPUT\"")}
                    ]
                }),
                home.path(),
            );
            let mut current = original.clone();
            let job = admit(&store, &current);
            crate::daemon::start_controller_job(job, &store).unwrap();
            for iteration in 1..=expected {
                let job = store.controller_job_for_key(&current.instance).unwrap();
                assert_eq!(
                    wait_terminal(&store, job.id, 30).status,
                    JobStatus::Succeeded
                );
                let checkpoint = store
                    .controller_job_state(job.id)
                    .unwrap()
                    .checkpoint
                    .unwrap();
                assert_eq!(checkpoint["iteration"], iteration);
                let status = workflow_status(&store, &current.instance).unwrap();
                assert_eq!(status["status"], "succeeded");
                assert_eq!(status["steps"]["sample"]["outcome"], "succeeded");
                let work = &status["steps"]["work"];
                assert_eq!(work["outcome"], "succeeded");
                assert_eq!(work["output"]["total"], 2);
                assert_eq!(work["output"]["succeeded"], 0);
                let items = work["items"].as_array().unwrap();
                let output_items = work["output"]["items"].as_array().unwrap();
                assert_eq!(items.len(), 2);
                assert_eq!(output_items.len(), 2);
                for (item, output) in items.iter().zip(output_items) {
                    assert_eq!(item["outcome"], "failed");
                    assert_eq!(output["outcome"], item["outcome"]);
                    assert_eq!(output["error"], item["error"]);
                }
                assert_eq!(current.inputs["carried"], "keep");
                let children = child_instances(&store, &current.instance);
                if iteration == expected {
                    assert!(children.is_empty());
                    assert_eq!(status["reason"], ended);
                } else {
                    assert_eq!(children.len(), 1);
                    let (_, child) = instance_job(&store, &children[0]).unwrap();
                    current = WorkflowJobRequest::parse(&child.request).unwrap();
                    assert_eq!(current.inputs["changed"], 42);
                    assert_eq!(current.inputs["previous"]["outputs"]["sample"]["value"], 42);
                }
            }
        }
    });
}
