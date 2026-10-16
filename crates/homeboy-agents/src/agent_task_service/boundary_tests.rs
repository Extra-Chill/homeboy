//! Cross-store boundary evidence. Subprocesses stop at durable boundaries; the
//! counted executor is reached only through the exact lifecycle claim.
use super::*;
use crate::agent_task_service::execution_step::execution_step_fixtures::kill_test_child_at_marker;
use homeboy_core::daemon::step_job::{self, StepJobDriver};
use homeboy_core::test_support::ControllerJobHarness;
use std::process::Command;
use std::time::{Duration, Instant};

struct Materializer {
    subject: String,
    calls: Arc<AtomicUsize>,
}

impl ExecutionStepTransport for Materializer {
    fn admission(&self, _: Vec<String>, runner: Option<String>) -> homeboy_core::Result<i32> {
        let store = test_lifecycle_store();
        let child = format!("{}-attempt-1", self.subject);
        agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
            &store,
            &self.subject,
            &child,
        )?;
        if let Some(runner) = runner {
            use std::io::Write;
            assert_eq!(runner, "boundary-runner");
            // The counted runner adapter accepts the exact reservation key.
            // Persist its count across the real supervising subprocess.
            let mut accepted = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(store.data_root().join("runner-acceptances"))
                .unwrap();
            writeln!(accepted, "{child}").unwrap();
        }
        let result = run_loaded_plan(
            test_plan(),
            Some(&child),
            Arc::new(CountingExecutor {
                calls: self.calls.clone(),
            }),
        )?;
        agent_task_lifecycle::record_cook_attempt_in_store(&store, &self.subject, 1, &child)?;
        Ok(result.exit_code)
    }
}

fn admit_execution_intent(
    store: &agent_task_lifecycle::AgentTaskLifecycleStore,
    intent: &ExecutionStepIntent,
) {
    register_execution_step_hooks();
    let harness = step_harness(
        intent,
        &store.data_root().join("boundary-jobs.sqlite"),
        "materializer",
    );
    let receipt =
        schedule_execution_intent_with(&serde_json::to_value(intent).unwrap(), |request| {
            Ok(harness.admit_keyed(request)?.id.to_string())
        })
        .unwrap();
    store
        .open_observation_initialized()
        .unwrap()
        .acknowledge_work_intent(&intent.intent_id, &receipt)
        .unwrap();
}

fn execute_and_assert_one_initial_attempt(subject: &str) {
    let store = test_lifecycle_store();
    let intent = current_intent(&store, subject);
    admit_execution_intent(&store, &intent);
    let calls = Arc::new(AtomicUsize::new(0));
    let transport = Materializer {
        subject: subject.into(),
        calls: calls.clone(),
    };
    for _ in 0..2 {
        assert_eq!(
            execute_counted(&intent.intent_id, &calls, &transport).unwrap(),
            0
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let child = format!("{subject}-attempt-1");
    assert_eq!(store.read_cook_index(subject).unwrap().latest_run_id, child);
    assert_eq!(
        store.read_record(&child).unwrap().state,
        AgentTaskRunState::Succeeded
    );
    assert!(!store
        .record_exists(&format!("{subject}-attempt-2"))
        .unwrap());
}

#[test]
fn execution_step_publication_crash_entry() {
    let Ok(boundary) = std::env::var("S8_PUBLICATION_BOUNDARY") else {
        return;
    };
    let store = test_lifecycle_store();
    let subject = "publication-boundary";
    let root = store.data_root().join("agent-task-cook-admissions");
    let staging = root.join("staging");
    let published = root.join("published");
    let mut binding = admission_binding(&staging, subject, None);
    binding["admission_inputs"]["input_manifest"]["path"] =
        serde_json::json!(published.join("manifest.json"));
    binding["input_publication"] = serde_json::json!({"state": "staged", "staging_root": staging, "published_root": published});
    std::fs::File::open(staging.join("manifest.json"))
        .unwrap()
        .sync_all()
        .unwrap();
    std::fs::File::open(&staging).unwrap().sync_all().unwrap();
    agent_task_lifecycle::prepare_unmaterialized_cook_admission(subject, binding, "queued", "test")
        .unwrap();
    if boundary == "rename" {
        std::fs::rename(&staging, &published).unwrap();
    }
    std::fs::write(std::env::var_os("S8_BOUNDARY_MARKER").unwrap(), "committed").unwrap();
    loop {
        std::thread::sleep(Duration::from_secs(1));
    }
}

#[test]
fn execution_step_staging_committed_and_rename_before_binding_recover_same_initial_attempt() {
    for boundary in ["staging", "rename"] {
        with_isolated_home(|home| {
            let marker = home.path().join("boundary");
            kill_test_child_at_marker(
                "agent_task_service::tests::boundary_tests::execution_step_publication_crash_entry",
                &[
                    ("S8_PUBLICATION_BOUNDARY", boundary.as_ref()),
                    ("S8_BOUNDARY_MARKER", marker.as_os_str()),
                ],
                &marker,
            );
            let store = test_lifecycle_store();
            let subject = "publication-boundary";
            assert_eq!(
                store.read_record(subject).unwrap().metadata["unmaterialized_cook_admission"]
                    ["state"],
                "preparing_inputs"
            );
            execute_and_assert_one_initial_attempt(subject);
            let record = store.read_record(subject).unwrap();
            assert_eq!(
                record.metadata["unmaterialized_cook_admission"]["binding"]["input_publication"]
                    ["state"],
                "published"
            );
            assert!(!store
                .data_root()
                .join("agent-task-cook-admissions/staging")
                .exists());
        });
    }
}

#[test]
fn execution_step_concurrent_drain_reuses_one_submission_and_executes_once() {
    with_isolated_home(|home| {
        let store = test_lifecycle_store();
        let subject = "concurrent-drain";
        agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
            &store,
            subject,
            admission_binding(&home.path().join("inputs"), subject, None),
            "queued",
            "test",
        )
        .unwrap();
        register_execution_step_hooks();
        let intent = current_intent(&store, subject);
        let path = store.data_root().join("boundary-jobs.sqlite");
        let harness = step_harness(&intent, &path, "drain");
        let barrier = Barrier::new(2);
        let receipts = std::thread::scope(|scope| {
            let drain = || {
                homeboy_core::daemon::orchestration::drain_work_intents_with(|work| {
                    barrier.wait();
                    schedule_execution_intent_with(&work.payload, |submission| {
                        Ok(harness.admit_keyed(submission)?.id.to_string())
                    })
                })
                .unwrap()
            };
            let first = scope.spawn(drain);
            let second = scope.spawn(drain);
            (first.join().unwrap(), second.join().unwrap())
        });
        assert_eq!(receipts.0, receipts.1);
        assert_eq!(harness.store().list().len(), 2);
        let restarted = harness.reopen(&path, "drain-restart").unwrap();
        assert_eq!(
            schedule_execution_intent_with(&serde_json::to_value(&intent).unwrap(), |submission| {
                Ok(restarted.admit_keyed(submission)?.id.to_string())
            })
            .unwrap(),
            receipts.0["scheduled"][0]
        );
        execute_and_assert_one_initial_attempt(subject);
    });
}

#[test]
fn execution_step_concurrent_rearm_placement_and_cancel_fence_all_old_children() {
    for cancel in [false, true] {
        with_isolated_home(|home| {
            let store = test_lifecycle_store();
            let subject = "race-admission";
            agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
                &store,
                subject,
                admission_binding(&home.path().join("inputs"), subject, None),
                "queued",
                "test",
            )
            .unwrap();
            let original = current_intent(&store, subject);
            admit_execution_intent(&store, &original);
            let reserved_child = format!("{subject}-attempt-1");
            agent_task_lifecycle::reserve_detached_cook_handoff_materialization_in_store(
                &store,
                subject,
                &reserved_child,
            )
            .unwrap();
            let barrier = Barrier::new(if cancel { 3 } else { 2 });
            std::thread::scope(|scope| {
                scope.spawn(|| {
                    barrier.wait();
                    agent_task_lifecycle::rearm_unmaterialized_cook_admission(subject).unwrap();
                });
                scope.spawn(|| {
                    barrier.wait();
                    let _ = agent_task_lifecycle::update_unmaterialized_cook_placement_in_store(
                        &store, subject, "local", "test",
                    );
                });
                if cancel {
                    scope.spawn(|| {
                        barrier.wait();
                        agent_task_lifecycle::cancel_run_in_store(
                            &store,
                            subject,
                            Some("race cancellation"),
                        )
                        .unwrap();
                    });
                }
            });
            let calls = Arc::new(AtomicUsize::new(0));
            let transport = Materializer {
                subject: subject.into(),
                calls: calls.clone(),
            };
            execute_counted(&original.intent_id, &calls, &transport).unwrap();
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            if cancel {
                let current = current_intent(&store, subject);
                execute_counted(&current.intent_id, &calls, &transport).unwrap();
                assert_eq!(calls.load(Ordering::SeqCst), 0);
                assert_eq!(
                    store.read_record(subject).unwrap().state,
                    AgentTaskRunState::Cancelled
                );
                assert!(store.read_cook_index(subject).is_err());
                assert_eq!(
                    store.read_record(subject).unwrap().metadata["detached_cook_handoff"]
                        ["materializing_attempt_run_id"],
                    reserved_child
                );
            } else {
                execute_and_assert_one_initial_attempt(subject);
            }
        });
    }
}

#[test]
fn execution_step_rearm_cancels_superseded_waiting_job_and_lost_cleanup_ack_is_idempotent() {
    with_isolated_home(|home| {
        let store = test_lifecycle_store();
        let subject = "superseded-wait";
        agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
            &store,
            subject,
            admission_binding(&home.path().join("inputs"), subject, None),
            "queued",
            "test",
        )
        .unwrap();
        register_execution_step_hooks();
        let initial = current_intent(&store, subject);
        let harness = step_harness(
            &initial,
            &home.path().join("cleanup-jobs.sqlite"),
            "cleanup",
        );
        let old_job = harness
            .admit_keyed(initial.step.submission().unwrap())
            .unwrap();
        agent_task_lifecycle::rearm_unmaterialized_cook_admission(subject).unwrap();
        let replacement = current_intent(&store, subject);
        let payload = serde_json::to_value(&replacement).unwrap();
        let schedule = |lose_ack| {
            super::super::execution_step::schedule_execution_intent_with_cleanup(
                &payload,
                |request| Ok(harness.admit_keyed(request)?.id.to_string()),
                |id| {
                    assert_eq!(id, old_job.id.to_string());
                    harness.cancel_keyed(old_job.id, "superseded")?;
                    if lose_ack {
                        Err(homeboy_core::Error::internal_unexpected(
                            "lost cancellation ACK",
                        ))
                    } else {
                        Ok(())
                    }
                },
            )
        };
        assert!(schedule(true).is_err());
        let receipt = schedule(false).unwrap();
        assert_eq!(schedule(false).unwrap(), receipt);
        assert_eq!(harness.store().list().len(), 3);
        let deadline = Instant::now() + Duration::from_secs(10);
        while !harness
            .store()
            .get(old_job.id)
            .unwrap()
            .status
            .is_terminal()
        {
            assert!(
                Instant::now() < deadline,
                "superseded waiting job did not cancel"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            harness.store().get(old_job.id).unwrap().status,
            homeboy_core::api_jobs::JobStatus::Cancelled
        );
        execute_and_assert_one_initial_attempt(subject);
    });
}

#[test]
fn execution_step_gated_crash_child_entry() {
    let Ok(id) = std::env::var("S8_GATED_INTENT") else {
        return;
    };
    if !step_job::await_step_child_start().unwrap() {
        return;
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let store = test_lifecycle_store();
    let intent = load_execution_intent(&store, &id).unwrap();
    let materializer = Materializer {
        subject: intent.subject_id,
        calls: calls.clone(),
    };
    if matches!(intent.mode, ExecutionStepMode::Admission { .. }) {
        execute_counted(&id, &calls, &materializer).unwrap();
    } else {
        execute_counted(&id, &calls, &QueuedRunTransport).unwrap();
    }
    if calls.load(Ordering::SeqCst) > 0 {
        std::fs::write(std::env::var_os("S8_BOUNDARY_MARKER").unwrap(), "provider").unwrap();
    }
}

#[test]
fn execution_step_interrupted_spawn_gates_old_child_then_recovery_executes_once() {
    with_isolated_home(|home| {
        let store = test_lifecycle_store();
        agent_task_lifecycle::submit_plan(&test_plan(), Some("gate-source")).unwrap();
        agent_task_lifecycle::retry_with_force_and_preflight_in_store(
            &store,
            "gate-source",
            Some("gate-retry"),
            false,
            &|_| Ok(()),
        )
        .unwrap();
        let intent = current_intent(&store, "gate-retry");
        register_execution_step_hooks();
        let path = home.path().join("gate-jobs.sqlite");
        let harness = ControllerJobHarness::durable(
            Arc::new(StepJobDriver),
            serde_json::to_value(&intent.step).unwrap(),
            &path,
            "gate-original",
        )
        .unwrap();
        harness
            .handle()
            .checkpoint(serde_json::json!({"schema": "homeboy/step-job-checkpoint/v1", "request": intent.step, "phase": "spawning"}))
            .unwrap();
        let marker = home.path().join("provider");
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "agent_task_service::tests::boundary_tests::execution_step_gated_crash_child_entry",
                "--nocapture",
            ])
            .env(
                step_job::STEP_JOB_ID_ENV,
                harness.job().unwrap().id.to_string(),
            )
            .env(step_job::STEP_JOB_STORE_ENV, &path)
            .env("S8_GATED_INTENT", &intent.intent_id)
            .env("S8_BOUNDARY_MARKER", &marker)
            .spawn()
            .unwrap();
        std::thread::sleep(Duration::from_millis(250));
        assert!(child.try_wait().unwrap().is_none());
        assert!(!marker.exists());
        let restarted = harness.reopen(&path, "gate-restarted").unwrap();
        restarted.recover_after_restart();
        restarted.wait_until_terminal().unwrap();
        assert!(child.wait().unwrap().success());
        assert!(!marker.exists());
        restarted.store().deliver_pending_step_terminals().unwrap();
        let recovered = current_intent(&store, "gate-retry");
        assert_eq!(recovered.subject_id, intent.subject_id);
        assert_eq!(recovered.recovery_attempt, 1);
        let calls = Arc::new(AtomicUsize::new(0));
        for id in [
            &intent.intent_id,
            &recovered.intent_id,
            &recovered.intent_id,
        ] {
            execute_counted(id, &calls, &QueuedRunTransport).unwrap();
        }
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            store.read_record("gate-retry").unwrap().state,
            AgentTaskRunState::Succeeded
        );
    });
}

#[test]
fn execution_step_runner_ineligible_then_eligible_across_daemon_restart_executes_once() {
    with_isolated_home(|home| {
        let store = test_lifecycle_store();
        let subject = "runner-restart";
        let mut binding = admission_binding(&home.path().join("inputs"), subject, None);
        binding["placement"] =
            serde_json::json!({"requested": "lab", "runner_ref": "boundary-runner"});
        binding["admission_inputs"]["argv"]
            .as_array_mut()
            .unwrap()
            .splice(
                1..1,
                [
                    serde_json::json!("--runner"),
                    serde_json::json!("boundary-runner"),
                ],
            );
        agent_task_lifecycle::record_unmaterialized_cook_admission_in_store(
            &store, subject, binding, "queued", "test",
        )
        .unwrap();
        let intent = current_intent(&store, subject);
        assert_eq!(intent.runner.as_deref(), Some("boundary-runner"));
        assert_eq!(intent.step.wait.runner_eligible, intent.runner);
        let eligible = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = eligible.clone();
        step_job::register_step_runner_eligibility(Arc::new(move |runner| {
            assert_eq!(runner, "boundary-runner");
            observed.load(Ordering::SeqCst)
        }));
        let marker = home.path().join("accepted-provider");
        let mut request = intent.step.clone();
        // The test-binary parsing adapter invokes the same agents entry and real
        // start gate, without requiring an installed CLI in a unit fixture.
        request.args = vec![
            "--exact".into(),
            "agent_task_service::tests::boundary_tests::execution_step_gated_crash_child_entry"
                .into(),
            "--nocapture".into(),
        ];
        request
            .env
            .public_env
            .insert("S8_GATED_INTENT".into(), intent.intent_id.clone());
        request
            .env
            .public_env
            .insert("S8_BOUNDARY_MARKER".into(), marker.display().to_string());
        request.wait.not_before = None;
        request.wait.runner_eligible = Some("boundary-runner".into());
        register_execution_step_hooks();
        let path = home.path().join("runner-jobs.sqlite");
        let harness = ControllerJobHarness::durable(
            Arc::new(StepJobDriver),
            serde_json::to_value(&request).unwrap(),
            &path,
            "runner-original",
        )
        .unwrap();
        harness.handle().checkpoint(serde_json::json!({"schema": "homeboy/step-job-checkpoint/v1", "request": request, "phase": "waiting"})).unwrap();
        let restarted = harness.reopen(&path, "runner-restarted").unwrap();
        restarted.recover_after_restart();
        std::thread::sleep(Duration::from_millis(300));
        assert!(!marker.exists());
        assert!(!store.data_root().join("runner-acceptances").exists());
        assert!(
            store.read_cook_index(subject).is_err(),
            "no destination work before eligibility"
        );
        assert_eq!(current_intent(&store, subject).intent_id, intent.intent_id);
        assert_eq!(
            store.read_record(subject).unwrap().metadata["execution_scheduling"]
                ["recovery_failures"],
            0
        );
        eligible.store(true, Ordering::SeqCst);
        // This fixture materializes a controller executable as well as running
        // the gated child. Allow bounded filesystem latency under the full
        // workspace suites; the ineligible assertions above are independent of
        // this completion deadline.
        let deadline = Instant::now() + Duration::from_secs(60);
        while !restarted.job().unwrap().status.is_terminal() {
            assert!(
                Instant::now() < deadline,
                "eligible exact child did not finish"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(
            restarted.wait_until_terminal().unwrap().status,
            homeboy_core::api_jobs::JobStatus::Succeeded
        );
        assert!(marker.exists());
        let child = format!("{subject}-attempt-1");
        assert_eq!(
            std::fs::read_to_string(store.data_root().join("runner-acceptances"))
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            vec![child.as_str()]
        );
        assert_eq!(store.read_cook_index(subject).unwrap().latest_run_id, child);
        assert_eq!(
            store.read_record(&child).unwrap().state,
            AgentTaskRunState::Succeeded
        );
        assert_eq!(
            store.read_record(&child).unwrap().metadata["provider_executions_consumed"],
            1
        );
        let calls = Arc::new(AtomicUsize::new(0));
        execute_counted(
            &intent.intent_id,
            &calls,
            &Materializer {
                subject: subject.into(),
                calls: calls.clone(),
            },
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        step_job::register_step_runner_eligibility(Arc::new(|_| true));
    });
}
