//! Split partition of agent_task_lifecycle tests (see mod.rs for shared setup).
#![cfg(test)]

use super::*;
use crate::agent_task::{
    AgentTaskArtifact, AgentTaskExecutionHandle, AgentTaskOutcomeStatus, AgentTaskSourceRef,
};
use crate::agent_task_scheduler::{
    AgentTaskAggregate, AgentTaskAggregateStatus, AgentTaskAggregateTotals,
    AGENT_TASK_AGGREGATE_SCHEMA,
};
use homeboy_core::api_jobs::JobStore;
use homeboy_core::test_support::with_isolated_home;
use sha2::Digest;
use std::process::Command;
use std::sync::{Arc, Mutex};

/// The tests below drive the store-rooted entry points. Resolving the store
/// once here keeps the ambient lookup in one place and lets the ambient
/// wrappers be deleted (#7505).
fn test_lifecycle_store() -> AgentTaskLifecycleStore {
    AgentTaskLifecycleStore::from_current_environment().expect("lifecycle store")
}

fn supervising_submission_metadata(run_id: &str) -> serde_json::Map<String, Value> {
    serde_json::Map::from_iter([(
        "local_cook_supervisor".to_string(),
        json!({
            "state": "supervising",
            "job_id": uuid::Uuid::new_v4(),
            "job_type": crate::agent_task_service::AGENT_TASK_COOK_JOB_TYPE,
            "pinned_run_id": run_id,
        }),
    )])
}

#[test]
fn parallel_local_cook_submissions_publish_their_runner_pid_atomically() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = Arc::new(AgentTaskLifecycleStore::new(context.path_roots()));
    let runs = ["parallel-local-cook-a", "parallel-local-cook-b"];

    std::thread::scope(|scope| {
        let handles = runs.map(|run_id| {
            let store = Arc::clone(&store);
            scope.spawn(move || {
                submit_plan_with_runtime_admission_in_store(
                    &store,
                    &test_plan(),
                    Some(run_id),
                    None,
                    Some(supervising_submission_metadata(run_id)),
                    None,
                    |_| Ok(json!({ "build": "test" })),
                )
                .expect("submit supervised local Cook")
            })
        });
        for handle in handles {
            let record = handle.join().expect("submission thread");
            assert_eq!(record.metadata["runner_pid"], std::process::id());
            assert!(record.owner_process_is_running());
        }
    });

    for run_id in runs {
        let persisted = store.read_record(run_id).expect("persisted local Cook");
        assert_eq!(persisted.metadata["runner_pid"], std::process::id());
        assert!(persisted.owner_process_is_running());
    }
}

#[test]
fn persisted_plan_retry_keeps_supervisor_ownership_until_runner_pid_is_published() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "persisted-plan-local-retry";
    let cook_id = "persisted-plan-cook";
    let plan = test_plan();
    let now = chrono::Utc::now();
    let mut metadata = supervising_submission_metadata(run_id);
    metadata.insert("cook_id".to_string(), json!(cook_id));
    metadata["local_cook_supervisor"]["state"] = json!("pending");
    metadata["local_cook_supervisor"]["lease_started_at"] = json!(now.to_rfc3339());
    metadata["local_cook_supervisor"]["lease_expires_at"] =
        json!((now + chrono::Duration::seconds(LOCAL_COOK_SUPERVISOR_LEASE_SECONDS)).to_rfc3339());
    submit_plan_with_runtime_admission_in_store(
        &store,
        &plan,
        Some(run_id),
        None,
        Some(metadata),
        None,
        |_| Ok(json!({ "build": "test" })),
    )
    .expect("persist retry plan");

    record_local_cook_retry_supervisor_in_store(
        &store,
        run_id,
        cook_id,
        &uuid::Uuid::new_v4().to_string(),
    )
    .expect("project retry supervisor");
    let supervised = store.read_record(run_id).expect("supervised retry");
    assert!(supervised.has_live_pending_local_cook_supervisor(now));
    assert!(supervised.metadata.get("runner_pid").is_none());

    let persisted_plan = store
        .read_controller_plan(run_id)
        .expect("persisted retry plan");
    let resumed = submit_plan_with_runtime_admission_in_store(
        &store,
        &persisted_plan,
        Some(run_id),
        None,
        None,
        None,
        |_| Ok(json!({ "build": "test" })),
    )
    .expect("resume persisted retry plan");
    assert_eq!(resumed.metadata["runner_pid"], std::process::id());
    assert!(resumed.owner_process_is_running());
    assert_eq!(
        resumed.metadata["local_cook_supervisor"]["state"],
        "supervising"
    );
}

fn seed_unmaterialized_admission_parent(store: &AgentTaskLifecycleStore, cook_id: &str) {
    store
        .submit_plan_with_runtime_admission(&test_plan(), cook_id, |_| Ok(json!({})))
        .expect("submit admission parent");
    store
        .mutate_record(cook_id, |record| {
            record.metadata["detached_cook_handoff"] = json!({
                "state": "pending",
                "admission_state": "pre_supervisor",
                "admission_deadline_at": (chrono::Utc::now()
                    + chrono::Duration::hours(1))
                    .to_rfc3339(),
                "cook_id": cook_id,
                "cancellation_fence": { "state": "open" },
            });
            true
        })
        .expect("seed admission parent");
}

#[test]
fn pending_local_retry_launcher_claim_converges_live_owner_then_reclaims_dead_owner() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "pending-local-retry";
    let cook_id = "pending-local-retry-cook";
    store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit pending retry");
    let mut owner = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn distinct launcher owner");
    let owner_identity = homeboy_core::process::process_start_identity(owner.id())
        .expect("inspect launcher owner")
        .expect("launcher owner identity");
    store
        .mutate_record(run_id, |record| {
            record.metadata["cook_id"] = json!(cook_id);
            record.metadata["local_cook_supervisor"] = json!({
                "state": "pending",
                "pinned_run_id": run_id,
                "lease_started_at": chrono::Utc::now().to_rfc3339(),
                "lease_expires_at": (chrono::Utc::now()
                    + chrono::Duration::seconds(LOCAL_COOK_SUPERVISOR_LEASE_SECONDS))
                    .to_rfc3339(),
                "launcher_pid": owner.id(),
                "launcher_process_start_identity": owner_identity,
            });
            true
        })
        .expect("persist pending launcher");

    assert_eq!(
        claim_local_cook_retry_launch_in_store(&store, run_id, cook_id).expect("live owner claim"),
        LocalCookRetryLaunchClaim::OwnedElsewhere,
        "a live launcher remains the sole process allowed to spawn"
    );
    owner.kill().expect("kill initial launcher");
    owner.wait().expect("reap initial launcher");

    assert_eq!(
        claim_local_cook_retry_launch_in_store(&store, run_id, cook_id)
            .expect("dead owner takeover"),
        LocalCookRetryLaunchClaim::Acquired,
        "a dead launcher is atomically replaced by this caller"
    );
    let record = store.read_record(run_id).expect("read reclaimed retry");
    assert_eq!(
        record.metadata["local_cook_supervisor"]["launcher_pid"].as_u64(),
        Some(u64::from(std::process::id()))
    );
    assert!(
        !record.metadata["local_cook_supervisor"]["launcher_reclaimed_at"].is_null(),
        "takeover remains durable evidence"
    );
}

#[test]
fn spawned_local_retry_child_is_reclaimed_without_a_second_spawn() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "spawned-local-retry";
    let cook_id = "spawned-local-retry-cook";
    store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit pending retry");
    store
        .mutate_record(run_id, |record| {
            record.metadata["cook_id"] = json!(cook_id);
            record.metadata["local_cook_supervisor"] = json!({
                "state": "pending",
                "pinned_run_id": run_id,
            });
            true
        })
        .expect("seed pending retry");
    let mut child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn retry child");
    let identity = homeboy_core::process::process_start_identity(child.id())
        .expect("inspect retry child")
        .expect("retry child identity");
    record_local_cook_retry_child_in_store(
        &store,
        run_id,
        cook_id,
        child.id(),
        identity.clone(),
        "one-child-token",
        "/tmp/one-child-token",
    )
    .expect("persist retry child before submission");

    assert!(matches!(
        claim_local_cook_retry_launch_in_store(&store, run_id, cook_id)
            .expect("recover spawned child"),
        LocalCookRetryLaunchClaim::ChildSpawned { pid, start_identity, .. }
            if pid == child.id() && start_identity == identity
    ));
    child.kill().expect("kill retry child");
    child.wait().expect("reap retry child");
    assert_eq!(
        claim_local_cook_retry_launch_in_store(&store, run_id, cook_id)
            .expect("observe dead child"),
        LocalCookRetryLaunchClaim::ChildExited,
    );
}

#[test]
fn unmaterialized_cook_admission_is_typed_secret_free_and_idempotent() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    seed_unmaterialized_admission_parent(&store, "cook-unmaterialized-stale");
    let binding = json!({
        "schema": "homeboy/unmaterialized-cook-binding/v1",
        "worktree_ref": "repo@fix-12443",
        "task": { "prompt_ref": "sha256:prompt" },
        "provider_runtime_refs": {
            "backend": "provider",
            "secret_env_names": ["TOKEN"],
            "provider_config_ref": "sha256:config"
        },
        "placement": { "local_fallback": false, "candidate_runner_refs": ["lab"] }
    });

    let first = record_unmaterialized_cook_admission_in_store(
        &store,
        "cook-unmaterialized-stale",
        binding.clone(),
        "blocked_runner_stale",
        "runner stale token=must-redact",
    )
    .expect("admitted");
    let replay = record_unmaterialized_cook_admission_in_store(
        &store,
        "cook-unmaterialized-stale",
        binding,
        "blocked_runner_stale",
        "runner stale token=must-redact",
    )
    .expect("idempotent replay");

    assert_eq!(first.run_id, replay.run_id);
    assert_eq!(
        replay.metadata["unmaterialized_cook_admission"]["state"],
        "blocked_runner_stale"
    );
    assert_eq!(
        replay.metadata["unmaterialized_cook_admission"]["binding"]["placement"]["local_fallback"],
        false
    );
    assert!(detached_cook_admission_is_live(&replay, chrono::Utc::now()));
    let serialized = serde_json::to_string(&replay).expect("serialize admission");
    assert!(!serialized.contains("must-redact"), "{serialized}");
    for command in ["status", "watch", "cancel", "resume"] {
        assert!(replay.metadata["unmaterialized_cook_admission"]["commands"][command].is_string());
    }
    assert!(replay.metadata["unmaterialized_cook_admission"]["commands"]
        .get("run")
        .is_none());
}

#[test]
fn unmaterialized_cook_admission_refuses_identity_rebinding_and_cancels_without_a_child() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let store = AgentTaskLifecycleStore::new(context.path_roots());
    seed_unmaterialized_admission_parent(&store, "cook-unmaterialized-cancel");
    record_unmaterialized_cook_admission_in_store(
        &store,
        "cook-unmaterialized-cancel",
        json!({ "request_ref": "sha256:first" }),
        "blocked_runner_unavailable",
        "runner disconnected",
    )
    .expect("admitted");
    let error = record_unmaterialized_cook_admission_in_store(
        &store,
        "cook-unmaterialized-cancel",
        json!({ "request_ref": "sha256:different" }),
        "queued",
        "runner ready",
    )
    .expect_err("identity rebinding refused");
    assert!(error.message.contains("different unmaterialized admission"));

    let cancelled = cancel_run_in_store(
        &store,
        "cook-unmaterialized-cancel",
        Some("operator cancellation"),
    )
    .expect("cancel admission");
    assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
    assert_eq!(
        cancelled.metadata["detached_cook_handoff"]["cancellation_fence"]["state"],
        "cancelled"
    );
    assert!(cancelled.metadata["detached_cook_handoff"]["child_pid"].is_null());
}

#[test]
fn replay_claim_consumption_validates_token_and_generation_exactly_once() {
    with_isolated_home(|_| {
        let cook_id = "cook-replay-consume";
        record_unmaterialized_cook_admission_in_store(
            &test_lifecycle_store(),
            cook_id,
            json!({ "placement": { "candidate_runner_refs": ["lab"] } }),
            "queued",
            "eligible",
        )
        .expect("admitted");
        rewrite_record_for_test(cook_id, |record| {
            record.metadata["unmaterialized_cook_admission"]["fence"] = json!(7);
            record.metadata["unmaterialized_cook_admission"]["lease"] = json!({
                "state": "claimed",
                "fence": 7,
                "token": "token-7",
                "expires_at": "2999-01-01T00:00:00+00:00",
            });
        })
        .expect("claim replay");

        assert!(
            !consume_unmaterialized_cook_replay_claim(cook_id, 6, "token-7")
                .expect("wrong fence rejected")
        );
        assert!(
            !consume_unmaterialized_cook_replay_claim(cook_id, 7, "wrong")
                .expect("wrong token rejected")
        );
        assert!(
            consume_unmaterialized_cook_replay_claim(cook_id, 7, "token-7")
                .expect("exact claim consumed")
        );
        assert!(
            renew_unmaterialized_cook_replay_claim(cook_id, 7, "token-7")
                .expect("exact claim renewed at materialization")
        );
        assert!(
            !renew_unmaterialized_cook_replay_claim(cook_id, 8, "token-7")
                .expect("superseded fence rejected at materialization")
        );
        assert!(
            !consume_unmaterialized_cook_replay_claim(cook_id, 7, "token-7")
                .expect("duplicate consumption rejected")
        );
        let record = exact_record(cook_id).expect("consumed record");
        assert_eq!(
            record.metadata["unmaterialized_cook_admission"]["state"],
            "materializing"
        );
        assert_eq!(
            record.metadata["unmaterialized_cook_admission"]["lease"]["state"],
            "materializing"
        );
        assert!(record.metadata["unmaterialized_cook_admission"]["lease"]
            .get("expires_at")
            .is_none());
        assert_eq!(
            record.metadata["unmaterialized_cook_admission"]["lease"]["owner"]["pid"],
            std::process::id()
        );
        assert_eq!(
            serde_json::from_value::<homeboy_core::process::ProcessStartIdentity>(
                record.metadata["unmaterialized_cook_admission"]["lease"]["owner"]
                    ["process_start_identity"]
                    .clone(),
            )
            .expect("persisted process identity"),
            homeboy_core::process::process_start_identity(std::process::id())
                .expect("inspect test process")
                .expect("test process identity"),
        );
        assert!(record.tasks.is_empty());
    });
}

#[test]
fn scoped_resume_rearms_backoff_but_preserves_terminal_and_materializing_owners() {
    with_isolated_home(|_| {
        for cook_id in ["resume-blocked", "resume-materializing", "resume-terminal"] {
            record_unmaterialized_cook_admission_in_store(
                &test_lifecycle_store(),
                cook_id,
                json!({ "placement": { "local_fallback": false } }),
                "blocked_runner_unavailable",
                "waiting",
            )
            .expect("admitted");
        }
        rewrite_record_for_test("resume-materializing", |record| {
            record.metadata["unmaterialized_cook_admission"]["state"] = json!("materializing");
            record.metadata["unmaterialized_cook_admission"]["lease"] =
                json!({ "state": "materializing", "fence": 1, "token": "owner" });
        })
        .unwrap();
        cancel_run("resume-terminal", Some("cancelled by operator")).unwrap();

        let blocked = rearm_unmaterialized_cook_admission("resume-blocked").unwrap();
        assert_eq!(
            blocked.metadata["unmaterialized_cook_admission"]["reason"],
            "explicit scoped resume requested"
        );
        let materializing = rearm_unmaterialized_cook_admission("resume-materializing").unwrap();
        assert_eq!(
            materializing.metadata["unmaterialized_cook_admission"]["lease"]["token"],
            "owner"
        );
        let terminal = rearm_unmaterialized_cook_admission("resume-terminal").unwrap();
        assert_eq!(terminal.state, AgentTaskRunState::Cancelled);
        assert_eq!(
            terminal.metadata["unmaterialized_cook_admission"]["reason"],
            "waiting"
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The submission, the hand-written running projection and the status
/// read that judges it are one home, so "the accepted daemon authority kept
/// this submission live" is asserted about the record this test planted rather
/// than one an ambient home happened to hold under the same run id.
///
/// The hermetic context is named `test_context` because `context` is already
/// this test's accepted `RunnerJobExecutionContext`, which the assertions read.
#[test]
fn accepted_daemon_context_keeps_a_pidless_runner_submission_live() {
    let test_context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(test_context.path_roots());
    let run_id = "accepted-daemon-runner-submission";
    let context =
        homeboy_core::runner_job_execution_context::RunnerJobExecutionContext::direct_daemon(
            Some(run_id),
            "runner-1",
            "00000000-0000-4000-8000-000000000001",
            "homeboy",
            "reservation-1",
        )
        .expect("accepted context");
    let mut record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit run");
    set_run_state(&mut record, AgentTaskRunState::Running);
    record.updated_at = Some(now_timestamp());
    project_runner_execution_context(&mut record.metadata, &context)
        .expect("project accepted authority");
    lifecycle_store
        .write_record(&record)
        .expect("persist runner-local record");

    let status = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("runner-local status")
    .record;
    assert_eq!(status.runner_job_id(), Some(context.runner_job_id()));
    assert!(status.metadata.get("runner_execution_context").is_some());
    assert!(status.metadata.get("stale_running").is_none());
    assert!(status.metadata.get("stale_running_reason").is_none());
}

/// Stays on `with_isolated_home` because the retry API still resolves its store
/// from the ambient test home. The injected failure itself is thread-scoped, so
/// parallel explicit-store tests cannot consume it (#11897).
#[test]
fn retry_first_visible_record_always_has_indexed_predecessor_identity() {
    with_isolated_home(|_| {
        let source_id = "retry-atomic-source";
        let retry_id = "retry-atomic-successor";
        submit_plan(&test_plan(), Some(source_id)).expect("submit source");

        // The former implementation had a durable submitted record at this
        // point, then wrote `retry_of` separately. The first record write now
        // includes retry provenance, so an interruption leaves no successor
        // visible to the indexed lookup at all.
        store::fail_next_record_write_for_test();
        assert!(retry(source_id, Some(retry_id)).is_err());
        assert!(!run_record_exists(retry_id).expect("retry remains absent"));
        assert!(store::read_retry_successors(source_id)
            .expect("indexed retry lookup")
            .is_empty());

        let retry = retry(source_id, Some(retry_id)).expect("retry after failed first write");
        let successors = store::read_retry_successors(source_id).expect("indexed retry lookup");
        assert_eq!(successors.len(), 1);
        assert_eq!(successors[0].run_id, retry_id);
        assert_eq!(successors[0].metadata["retry_of"], source_id);
        assert_eq!(retry.metadata["retry_of"], source_id);
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The lineage decision this test makes — "is a successor already
/// active?" — is a scan of the store's own records, so it is only evidence of
/// anything if the reservations, the cancellations that terminalize them and
/// the source record read back at the end all name one home.
///
/// The retries are spelled as `retry_with_runtime_admission_in_store(.., force,
/// true, None, stub)` rather than `retry_with_force_in_store`, which is the
/// same call with `enforce_lineage_reservation = true` but supplies the real
/// controller admission. Admission is machine-global by design — it writes
/// under `paths::controller_runtimes_store()` and takes a cross-process lock —
/// so a test that no longer mutates HOME would enqueue against the *real
/// operator* runtime store. This is the same substitution the split-root
/// re-entry proof below already makes.
#[test]
fn retry_lineage_refuses_an_active_successor_and_requires_force_after_terminal_successors() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let reserve = |run_id: &str, requested_run_id: &str, force: bool| {
        retry_with_runtime_admission_in_store(
            &lifecycle_store,
            run_id,
            Some(requested_run_id),
            force,
            true,
            None,
            |_| Ok(json!({})),
        )
    };
    let source_id = "retry-lineage-source";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), source_id, |_| Ok(json!({})))
        .expect("submit source");

    let first =
        reserve(source_id, "retry-lineage-first", false).expect("first retry reserves successor");
    let replayed = reserve(source_id, "retry-lineage-first", false)
        .expect("exact active retry reservation is idempotent");
    assert_eq!(replayed.run_id, first.run_id);
    let active = reserve(source_id, "retry-lineage-second", false)
        .expect_err("active successor prevents duplicate retry");
    assert!(active.message.contains("retry-lineage-first"));
    assert!(active
        .message
        .contains("homeboy agent-task status retry-lineage-first"));

    let forced_active = reserve(source_id, "retry-lineage-first", true)
        .expect("force creates a distinct successor beside the active retry");
    assert_ne!(forced_active.run_id, first.run_id);
    assert_eq!(forced_active.metadata["retried_from"], source_id);
    assert_eq!(forced_active.metadata["retry_root"], source_id);

    cancel_run_in_store(
        &lifecycle_store,
        &first.run_id,
        Some("test terminal successor"),
    )
    .expect("terminalize successor");
    cancel_run_in_store(
        &lifecycle_store,
        &forced_active.run_id,
        Some("test terminal forced successor"),
    )
    .expect("terminalize forced successor");
    let terminal = reserve(source_id, "retry-lineage-second", false)
        .expect_err("terminal successor requires explicit force");
    assert_eq!(terminal.details["field"], "force");

    let forced =
        reserve(source_id, "retry-lineage-second", true).expect("force creates next retry");
    assert_eq!(forced.metadata["retried_from"], source_id);
    assert_eq!(forced.metadata["retry_root"], source_id);
    let source = exact_record_in_store(&lifecycle_store, source_id).expect("source record");
    assert_eq!(
        source.metadata["retries"],
        json!([
            "retry-lineage-first",
            forced_active.run_id,
            "retry-lineage-second"
        ])
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The whole claim is that a runner's re-submission reads the lineage
/// off the record already there and writes it back, so the reservation, the
/// descendant, the re-submission and the read-back all have to be one home —
/// lineage carried forward from another installation's record would satisfy
/// every assertion below while proving nothing.
///
/// The retries use `retry_with_runtime_admission_in_store` and the runner
/// re-submission uses `submit_plan_with_runtime_admission_in_store` with a stub
/// admission, for the reason given on the retry-lineage proof above.
#[test]
fn runner_resubmission_preserves_existing_retry_lineage_metadata() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let source_id = "retry-resubmission-source";
    let first_retry_id = "retry-resubmission-first";
    let second_retry_id = "retry-resubmission-second";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, source_id, |_| Ok(json!({})))
        .expect("submit source");
    retry_with_runtime_admission_in_store(
        &lifecycle_store,
        source_id,
        Some(first_retry_id),
        false,
        true,
        None,
        |_| Ok(json!({})),
    )
    .expect("reserve first retry");
    retry_with_runtime_admission_in_store(
        &lifecycle_store,
        first_retry_id,
        Some(second_retry_id),
        true,
        true,
        None,
        |_| Ok(json!({})),
    )
    .expect("record descendant retry");

    let resubmitted = submit_plan_with_runtime_admission_in_store(
        &lifecycle_store,
        &plan,
        Some(first_retry_id),
        Some("fixture-runner".to_string()),
        None,
        None,
        |_| Ok(json!({ "runner": "fixture-runtime" })),
    )
    .expect("runner resubmits the existing retry record");

    assert_eq!(resubmitted.metadata["retried_from"], source_id);
    assert_eq!(resubmitted.metadata["retry_root"], source_id);
    assert_eq!(resubmitted.metadata["retries"], json!([second_retry_id]));
    assert_eq!(resubmitted.metadata["retry_of"], source_id);
    assert_eq!(
        exact_record_in_store(&lifecycle_store, first_retry_id)
            .expect("persisted resubmitted retry")
            .metadata["retries"],
        json!([second_retry_id])
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Progress recorded before a cancellation and progress recorded after
/// it are the same durable record, so "the terminal state did not erase the
/// Cook progress" is only a claim about one home.
#[test]
fn cook_progress_is_durable_across_active_and_terminal_lifecycle_states() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-progress-lifecycle";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit run");

    let active = record_cook_progress_in_store(
        &lifecycle_store,
        run_id,
        "provider_start",
        1,
        Some("fixture provider"),
    )
    .expect("record active progress");
    assert_eq!(active.metadata["cook_progress"]["phase"], "provider_start");
    assert!(active.lifecycle.heartbeat.is_some());

    let cancelled = cancel_run_in_store(&lifecycle_store, run_id, Some("fixture cancellation"))
        .expect("cancel run");
    assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
    let terminal =
        record_cook_progress_in_store(&lifecycle_store, run_id, "terminal", 1, Some("cancelled"))
            .expect("retain terminal progress");
    assert_eq!(terminal.state, AgentTaskRunState::Cancelled);
    assert_eq!(terminal.metadata["cook_progress"]["phase"], "terminal");
    let terminal =
        record_cook_terminal_result_in_store(&lifecycle_store, run_id, "no_candidate", false, 1)
            .expect("record terminal Cook result");
    assert_eq!(
        terminal.metadata["cook_progress"]["terminal_success"],
        false
    );
    assert_eq!(terminal.metadata["cook_progress"]["exit_code"], 1);

    let repeated = record_cook_progress_in_store(
        &lifecycle_store,
        run_id,
        "durable_identity",
        1,
        Some("restarted Cook"),
    )
    .expect("retain exact terminal result after restart");
    assert_eq!(
        repeated.metadata["cook_progress"],
        terminal.metadata["cook_progress"]
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The retained sample is read back out of the record the first sample
/// was written into, which is the whole point: a carry-forward read from
/// another home would report a sample this test never observed.
#[test]
fn cook_progress_carries_provider_activity_and_survives_a_failed_probe() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-progress-activity";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit run");

    // A sample makes it into the record an operator reads, dated.
    let sampled = record_cook_progress_with_activity_in_store(
        &lifecycle_store,
        run_id,
        "heartbeat",
        1,
        Some("provider execution is still running"),
        Some(json!({
            "files_changed": 0,
            "command": "cargo test -q -p homeboy-agents",
            "command_elapsed_seconds": 372
        })),
    )
    .expect("record sampled progress");
    assert_eq!(
        sampled.metadata["cook_progress"]["activity"]["files_changed"],
        0
    );
    let observed_at = sampled.metadata["cook_progress"]["activity_observed_at"].clone();
    assert!(observed_at.is_string(), "a fresh sample is dated");

    // A probe that could not read the worktree or the process table is not
    // evidence the provider stopped. The last real sample is carried
    // forward with its own observation time, so a reader can see both what
    // was last seen and how stale it is.
    let unsampled = record_cook_progress_in_store(
        &lifecycle_store,
        run_id,
        "heartbeat",
        1,
        Some("still running"),
    )
    .expect("record unsampled progress");
    assert_eq!(
        unsampled.metadata["cook_progress"]["activity"]["command"],
        "cargo test -q -p homeboy-agents"
    );
    assert_eq!(
        unsampled.metadata["cook_progress"]["activity_observed_at"], observed_at,
        "a retained sample keeps the time it was actually observed"
    );
}

#[test]
fn cook_progress_recorder_writes_to_the_injected_store_not_an_ambient_root() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-progress-rooted";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit run into injected store");
    record_cook_progress_with_activity_in_store(
        &lifecycle_store,
        run_id,
        "provider_start",
        1,
        Some("rooted progress"),
        None,
    )
    .expect("record progress into injected store");
    let record = lifecycle_store.read_record(run_id).expect("read run");
    assert_eq!(record.metadata["cook_progress"]["phase"], "provider_start");
}

#[test]
fn cook_write_siblings_persist_into_the_injected_store_and_not_a_second_root() {
    // The negative half is the proof. Both roots are given the *same* run
    // identity, and every write below is made through a sibling that was handed
    // `seeded`. A sibling that ignored its parameter and resolved a root from
    // the process environment would leave `seeded` empty, or — if the ambient
    // root ever coincided with `other` — would show up in the second store.
    // Asserting that `other` still reads its own untouched record is what
    // distinguishes "wrote where I was told" from "wrote wherever the
    // environment points" (#7505). No environment is mutated here: both roots
    // come from `HermeticTestContext::path_roots()`.
    let seeded_context = homeboy_core::test_support::HermeticTestContext::new();
    let other_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(seeded_context.data_dir(), other_context.data_dir());

    let seeded =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(seeded_context.path_roots());
    let other =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(other_context.path_roots());

    let run_id = "rooted-cook-write";
    let cook_id = "rooted-cook-write-cook";
    let plan_only_run_id = "rooted-cook-write-plan-only";
    let plan = test_plan();
    for lifecycle_store in [&seeded, &other] {
        lifecycle_store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
            .expect("submit the same run identity into both roots");
    }

    // The terminal notification outcome is a file beside the Cook index rather
    // than an observation row, so it is checked at the path each store owns.
    let notification_outcome_path =
        |lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore| {
            lifecycle_store
                .cook_index_path(cook_id)
                .with_file_name("notification-outcome.json")
        };

    persist_controller_plan_in_store(&seeded, plan_only_run_id, &plan)
        .expect("persist a controller plan into the injected store");
    let progress = record_cook_progress_in_store(&seeded, run_id, "terminal", 1, Some("rooted"))
        .expect("record Cook progress into the injected store");
    assert_eq!(progress.metadata["cook_progress"]["phase"], "terminal");
    let terminal =
        record_cook_terminal_result_in_store(&seeded, run_id, "intentional_no_change", true, 0)
            .expect("record the terminal Cook result into the injected store");
    assert_eq!(terminal.metadata["cook_progress"]["terminal_success"], true);
    record_cook_controller_failure_in_store(&seeded, run_id, &json!({ "reason": "rooted" }))
        .expect("record a controller failure into the injected store");
    record_cook_observer_event_in_store(&seeded, run_id, "terminal", json!({ "code": "rooted" }))
        .expect("record an observer event into the injected store");
    record_cook_supervision_in_store(
        &seeded,
        run_id,
        1,
        Some(json!({ "cpu_seconds": 7 })),
        vec![json!({ "budget": "wall_clock" })],
    )
    .expect("record a supervision tick into the injected store");
    record_cook_supervision_stop_in_store(&seeded, run_id, 1, json!({ "signalled": true }))
        .expect("record a supervision stop into the injected store");
    record_cook_recovery_checkpoint_in_store(
        &seeded,
        run_id,
        "verification_pending",
        "homeboy agent-task cook-continue",
    )
    .expect("record a recovery checkpoint into the injected store");
    record_cook_force_with_lease_receipt_in_store(
        &seeded,
        run_id,
        json!({ "remote_sha": "abc123" }),
    )
    .expect("record a force-with-lease receipt into the injected store");
    record_cook_terminal_notification_outcome_in_store(
        &seeded,
        cook_id,
        json!({ "state": "delivered" }),
    )
    .expect("record a terminal notification outcome into the injected store");

    // The positive: every write is durable in the store that was handed in.
    let persisted = seeded
        .read_record(run_id)
        .expect("read the seeded record back");
    assert_eq!(persisted.metadata["cook_progress"]["phase"], "terminal");
    assert_eq!(
        persisted.metadata["cook_progress"]["terminal_success"],
        true
    );
    assert_eq!(persisted.metadata["cook_progress"]["exit_code"], 0);
    assert_eq!(
        persisted.metadata["cook_controller_failure"]["reason"],
        "rooted"
    );
    assert_eq!(
        persisted.metadata["cook_observer_events"][0]["phase"],
        "terminal"
    );
    assert_eq!(
        persisted.metadata["cook_resource_timeline"][0]["sample"]["cpu_seconds"],
        7
    );
    let supervision_events = persisted.metadata["cook_supervision_events"]
        .as_array()
        .expect("supervision events are an array");
    assert_eq!(supervision_events.len(), 2);
    assert_eq!(supervision_events[0]["kind"], "budget_breached");
    assert_eq!(supervision_events[1]["kind"], "stop_executed");
    assert_eq!(
        persisted.metadata["cook_recovery_checkpoint"]["phase"],
        "verification_pending"
    );
    assert_eq!(
        persisted.metadata["cook_force_with_lease_receipt"]["remote_sha"],
        "abc123"
    );
    assert_eq!(
        seeded
            .read_controller_plan(plan_only_run_id)
            .expect("the injected store owns the persisted plan")
            .plan_id,
        plan.plan_id
    );
    assert!(notification_outcome_path(&seeded).exists());

    // The negative: the second root holds the same run identity and saw none of
    // it. Any sibling that ignored the store it was handed fails here.
    let untouched = other
        .read_record(run_id)
        .expect("the second root still has its own record");
    for key in [
        "cook_progress",
        "cook_controller_failure",
        "cook_observer_events",
        "cook_resource_timeline",
        "cook_supervision_events",
        "cook_recovery_checkpoint",
        "cook_force_with_lease_receipt",
    ] {
        assert!(
            untouched.metadata.get(key).is_none(),
            "second root must not observe `{key}` written through the injected store"
        );
    }
    assert!(other.read_controller_plan(plan_only_run_id).is_err());
    assert!(!notification_outcome_path(&other).exists());
}

#[test]
fn claim_family_siblings_coordinate_only_inside_the_injected_store() {
    // The claim family decides which worker wins a run, so its negative half
    // has to be sharper than absence: both roots are seeded with the *same*
    // queue, the same detached-admission parent, and the same Cook
    // notification identity, and every coordinating call below is made through
    // a sibling handed `seeded`. The second root is then asserted to be
    // *untouched* — still queued, still pending, still able to win its own
    // notification claim and expire its own lease. A claim that leaked into an
    // ambient root would satisfy every positive assertion here and still fail
    // that (#7505).
    //
    // No environment is mutated: both roots come from
    // `HermeticTestContext::path_roots()`.
    let seeded_context = homeboy_core::test_support::HermeticTestContext::new();
    let other_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(seeded_context.data_dir(), other_context.data_dir());

    let seeded =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(seeded_context.path_roots());
    let other =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(other_context.path_roots());

    let queued_run_id = "rooted-claim-queued";
    let handoff_cook_id = "rooted-claim-detached-parent";
    let notification_cook_id = "rooted-claim-notification";
    let plan = test_plan();

    for lifecycle_store in [&seeded, &other] {
        for run_id in [queued_run_id, handoff_cook_id] {
            lifecycle_store
                .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
                .expect("submit the same queue into both roots");
            // Remove the admission-supplied runtime pin rather than leaving a
            // malformed one. An invalid *present* pin sends
            // `validate_controller_runtime_in_store` through
            // `migrate_legacy_pin_and_persist`. With no pin at all the preflight
            // fails on the record itself without materializing runtime state.
            lifecycle_store
                .mutate_record(run_id, |record| {
                    record
                        .ensure_metadata_object()
                        .remove(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY)
                        .is_some()
                })
                .expect("strip the controller runtime pin");
        }
        lifecycle_store
            .mutate_record(handoff_cook_id, |record| {
                record.ensure_metadata_object().insert(
                    "detached_cook_handoff".to_string(),
                    json!({
                        "cook_id": handoff_cook_id,
                        "state": "pending",
                        "admission_deadline_at": "2020-01-01T00:00:00Z",
                    }),
                );
                true
            })
            .expect("seed an expired detached Cook admission into both roots");
    }

    let notification_claim_path =
        |lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore| {
            lifecycle_store
                .cook_index_path(notification_cook_id)
                .with_file_name("notification-claim.json")
        };
    let notification_path =
        |lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore| {
            lifecycle_store
                .cook_index_path(notification_cook_id)
                .with_file_name("notification.json")
        };

    // Expire the detached parent first so it is terminal, and therefore no
    // longer queued, before the queue scan below runs.
    assert!(
        expire_detached_cook_admission_in_store(&seeded, handoff_cook_id)
            .expect("expire the detached admission lease in the injected store")
    );

    // A budget of zero is refused before anything is inspected, and a scope
    // that matches nothing inspects nothing. Neither writes, so both are safe
    // to run before the one call that quarantines.
    let budgeted = claim_next_eligible_queued_run_with_preflight_and_filter_and_limit_in_store(
        &seeded,
        |_| true,
        0,
        |_, _| unreachable!("preflight cannot run inside an exhausted admission budget"),
    )
    .expect("exhausted budget is not an error");
    assert!(budgeted.admission_limit_reached);
    assert_eq!(budgeted.inspected, 0);
    assert!(budgeted.record.is_none());
    assert!(budgeted.skipped.is_empty());

    let scoped = claim_next_eligible_queued_run_with_preflight_and_filter_in_store(
        &seeded,
        |_| false,
        |_, _| unreachable!("preflight cannot run for a record outside the caller's scope"),
    )
    .expect("an empty scope is not an error");
    assert_eq!(scoped.inspected, 0);
    assert!(scoped.record.is_none());
    assert!(scoped.skipped.is_empty());

    // The one coordinating write. The preflight closure is never reached: the
    // pinless record fails controller-runtime validation first, which is the
    // branch that quarantines and skips.
    let claim = claim_next_eligible_queued_run_with_preflight_in_store(&seeded, |_, _| {
        unreachable!("preflight runs only after the durable plan and runtime validate")
    })
    .expect("scan the injected store's queue");
    assert!(claim.record.is_none());
    assert!(!claim.admission_limit_reached);
    assert_eq!(
        claim.inspected, 1,
        "the scan must see the injected store's own queue; an ambient observation \
         database holds none of these records and would inspect nothing"
    );
    assert_eq!(claim.skipped.len(), 1);
    assert_eq!(claim.skipped[0].run_id, queued_run_id);
    assert_eq!(
        claim.skipped[0].category,
        "queue_admission_preflight_failed"
    );
    assert_eq!(
        claim.skipped[0].error_code,
        ErrorCode::ValidationInvalidArgument.as_str()
    );

    // The quarantine landed in the scanned store, so the next scan of that same
    // store has nothing left to inspect.
    let rescan =
        claim_next_eligible_queued_run_in_store(&seeded).expect("rescan the injected store");
    assert_eq!(rescan.inspected, 0);
    assert!(rescan.skipped.is_empty());
    assert!(claim_next_queued_run_in_store(&seeded)
        .expect("the thin claim delegate shares the injected store")
        .is_none());

    // The Cook notification claim is an `O_EXCL` marker file, not a record
    // write, so it is checked at the path each store owns.
    assert!(claim_cook_terminal_notification_in_store(
        &seeded,
        notification_cook_id,
        "fixture-notifier"
    )
    .expect("win the terminal notification claim in the injected store"));
    assert!(
        !claim_cook_terminal_notification_in_store(
            &seeded,
            notification_cook_id,
            "second-notifier"
        )
        .expect("a held claim is answered, not errored"),
        "the injected store's claim is exactly-once within that store"
    );
    confirm_cook_terminal_notification_in_store(&seeded, notification_cook_id, "fixture-notifier")
        .expect("confirm the delivery in the injected store");
    assert!(!claim_cook_terminal_notification_in_store(
        &seeded,
        notification_cook_id,
        "late-notifier"
    )
    .expect("a delivered notification is answered, not errored"));
    // The empty-id guard is preserved. Without it `sanitize_run_id` would turn
    // `""` into a freshly minted `agent-task-<uuid>` and `"   "` into `___`,
    // and each would create a durable claim marker under its own directory.
    for blank in ["", "   "] {
        assert!(
            !claim_cook_terminal_notification_in_store(&seeded, blank, "fixture-notifier")
                .expect("a blank Cook id is answered, not errored")
        );
    }

    // The positive: every coordinating write is durable in the store handed in.
    let quarantined = seeded
        .read_record(queued_run_id)
        .expect("read the scanned queue's record back");
    assert_eq!(quarantined.state, AgentTaskRunState::Queued);
    assert_eq!(
        quarantined.metadata["queue_quarantine"]["category"],
        "queue_admission_preflight_failed"
    );
    let expired = seeded
        .read_record(handoff_cook_id)
        .expect("read the detached parent back");
    assert_eq!(expired.state, AgentTaskRunState::Failed);
    assert_eq!(
        expired.metadata["detached_cook_handoff"]["state"],
        "exited_before_handoff"
    );
    assert_eq!(
        expired.metadata["detached_cook_handoff"]["admission_state"],
        "failed"
    );
    assert!(notification_claim_path(&seeded).exists());
    assert!(notification_path(&seeded).exists());
    // A blank Cook id must not have minted a durable Cook directory beside the
    // one this test named.
    let cook_root = seeded
        .cook_index_path(notification_cook_id)
        .parent()
        .expect("a Cook index lives in its own directory")
        .parent()
        .expect("Cook directories share one root")
        .to_path_buf();
    let mut cook_directories: Vec<String> = std::fs::read_dir(&cook_root)
        .expect("read the injected store's Cook root")
        .map(|entry| {
            entry
                .expect("read a Cook directory entry")
                .file_name()
                .to_string_lossy()
                .into_owned()
        })
        .collect();
    cook_directories.sort();
    assert_eq!(cook_directories, vec![notification_cook_id.to_string()]);

    // The negative: the second root holds the same identities and saw none of
    // it. Absence first.
    let untouched_queue = other
        .read_record(queued_run_id)
        .expect("the second root still has its own queued record");
    assert_eq!(untouched_queue.state, AgentTaskRunState::Queued);
    assert!(
        untouched_queue.metadata.get("queue_quarantine").is_none(),
        "second root must not observe a quarantine written through the injected store"
    );
    let untouched_parent = other
        .read_record(handoff_cook_id)
        .expect("the second root still has its own detached parent");
    assert_eq!(untouched_parent.state, AgentTaskRunState::Queued);
    assert_eq!(
        untouched_parent.metadata["detached_cook_handoff"]["state"],
        "pending"
    );
    for key in ["admission_state", "reason"] {
        assert!(
            untouched_parent.metadata["detached_cook_handoff"]
                .get(key)
                .is_none(),
            "second root must not observe `{key}` written through the injected store"
        );
    }
    assert!(!notification_claim_path(&other).exists());
    assert!(!notification_path(&other).exists());

    // Then the stronger half: the second root is not merely missing the
    // evidence, it is still *eligible*. Its queued run is still inspectable,
    // its notification claim is still winnable, and its admission lease is
    // still expirable — none of which is true of state some other call already
    // consumed.
    let still_eligible = claim_next_eligible_queued_run_with_preflight_and_filter_in_store(
        &other,
        |record| record.run_id == queued_run_id,
        |_, _| unreachable!("preflight runs only after the durable plan and runtime validate"),
    )
    .expect("scan the second root's own queue");
    assert_eq!(still_eligible.inspected, 1);
    assert_eq!(still_eligible.skipped.len(), 1);
    assert_eq!(still_eligible.skipped[0].run_id, queued_run_id);
    assert!(
        claim_cook_terminal_notification_in_store(&other, notification_cook_id, "second-root")
            .expect("the second root owns its own notification claim"),
        "the injected store's claim must not consume the second root's eligibility"
    );
    assert!(
        expire_detached_cook_admission_in_store(&other, handoff_cook_id)
            .expect("the second root owns its own admission lease"),
        "the injected store's expiry must not consume the second root's lease"
    );
}

#[test]
fn detached_handoff_siblings_advance_only_the_injected_store() {
    // The detached-handoff cluster is admission state: a pending parent, the
    // durable cancellation fence a child reads before it may materialize, and
    // the child and supervisor identities that keep the admission lease live.
    // Absence in a second root is too weak a negative for that — every one of
    // these writes is equally "absent" from a root the work simply never
    // reached. So both roots are seeded with the *same* parent in the *same*
    // pre-supervisor shape, every act below is made through a sibling handed
    // `seeded`, and the second root is then asserted to be still **eligible**:
    // its handoff still pending, its fence still open, its admission still
    // live, and its parent still able to take a child and a supervisor of its
    // own. A write that leaked into an ambient root satisfies every positive
    // assertion here and fails that (#7505).
    //
    // The two admission guards are proved directionally rather than by absence:
    // the same call with the same Cook id is refused by one root and answered
    // by the other, decided only by which store was injected.
    //
    // No environment is mutated: both roots come from
    // `HermeticTestContext::path_roots()`.
    let seeded_context = homeboy_core::test_support::HermeticTestContext::new();
    let other_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(seeded_context.data_dir(), other_context.data_dir());

    let seeded =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(seeded_context.path_roots());
    let other =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(other_context.path_roots());

    let handoff_cook_id = "rooted-detached-handoff-parent";
    let indexed_cook_id = "rooted-detached-handoff-indexed";
    let collision_cook_id = "rooted-detached-handoff-collision";
    let indexed_attempt_run_id = "rooted-detached-handoff-indexed-attempt-1";
    let plan = test_plan();

    // The parent shape is reproduced rather than produced. The submit branch of
    // `record_detached_cook_handoff_parent_in_store` runs controller admission,
    // which is deliberately *not* a lifecycle root: it writes under
    // `paths::controller_runtimes_store()` and takes a machine-global admission
    // lock. Driving it here would reach the operator's real home and serialize
    // against every peer test, so this test exercises the branches that decide
    // *whether* to submit and never the submit itself. Every other function in
    // this cluster is driven end to end.
    let seed_pending_parent =
        |lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore, cook_id: &str| {
            lifecycle_store
                .submit_plan_with_runtime_admission(&plan, cook_id, |_| Ok(json!({})))
                .expect("submit a detached handoff parent identity");
            lifecycle_store
                .mutate_record(cook_id, |record| {
                    record.ensure_metadata_object().insert(
                        "detached_cook_handoff".to_string(),
                        json!({
                            "state": "pending",
                            "admission_state": "pre_supervisor",
                            "admission_deadline_at": (chrono::Utc::now()
                                + chrono::Duration::seconds(3_600))
                            .to_rfc3339(),
                            "cook_id": cook_id,
                            "cancellation_fence": { "state": "open" },
                        }),
                    );
                    true
                })
                .expect("seed the pre-supervisor handoff shape");
        };

    for lifecycle_store in [&seeded, &other] {
        seed_pending_parent(lifecycle_store, handoff_cook_id);
        seed_pending_parent(lifecycle_store, indexed_cook_id);
    }
    // The collision guard's two answers, one per root, under one Cook id: an
    // unrelated run in the first, this Cook's own handoff parent in the second.
    seeded
        .submit_plan_with_runtime_admission(&plan, collision_cook_id, |_| Ok(json!({})))
        .expect("submit an unrelated run under the Cook id in the first root");
    seed_pending_parent(&other, collision_cook_id);
    // The alias guard's evidence, published in the first root only.
    seeded
        .write_cook_index_attempt(
            indexed_cook_id,
            1,
            indexed_attempt_run_id,
            now_timestamp(),
            None,
        )
        .expect("publish a Cook attempt alias in the first root only");

    // Re-recording a live handoff parent is the idempotent branch: it answers
    // from the injected store's own record instead of submitting a second one.
    let readmitted = record_detached_cook_handoff_parent_in_store(&seeded, handoff_cook_id)
        .expect("re-record the live handoff parent in the injected store");
    assert_eq!(readmitted.run_id, handoff_cook_id);
    assert_eq!(
        readmitted.metadata["detached_cook_handoff"]["admission_state"],
        "pre_supervisor"
    );
    assert!(
        readmitted.metadata.get("cook_progress").is_none(),
        "the idempotent branch must answer from the durable parent, not submit a new one"
    );

    // The alias guard, both directions. Only the first root published the Cook
    // index, so only the first root refuses.
    let aliased = record_detached_cook_handoff_parent_in_store(&seeded, indexed_cook_id)
        .expect_err("an indexed Cook attempt refuses a fresh handoff parent");
    assert_eq!(
        aliased.code.as_str(),
        ErrorCode::ValidationInvalidArgument.as_str()
    );
    assert_eq!(aliased.details["field"], "run_id");
    assert_eq!(
        aliased.details["problem"],
        "detached Cook id already resolves to an existing Cook attempt"
    );
    assert_eq!(
        record_detached_cook_handoff_parent_in_store(&other, indexed_cook_id)
            .expect("the second root published no alias and answers from its own parent")
            .metadata["detached_cook_handoff"]["admission_state"],
        "pre_supervisor",
        "an alias published in the injected store must not decide the second root"
    );

    // The collision guard, both directions, under one Cook id.
    let collided = record_detached_cook_handoff_parent_in_store(&seeded, collision_cook_id)
        .expect_err("an unrelated run under the Cook id refuses a handoff parent");
    assert_eq!(
        collided.code.as_str(),
        ErrorCode::ValidationInvalidArgument.as_str()
    );
    assert_eq!(collided.details["field"], "run_id");
    assert_eq!(
        collided.details["problem"],
        "detached Cook id collides with an existing non-handoff run"
    );
    assert_eq!(
        record_detached_cook_handoff_parent_in_store(&other, collision_cook_id)
            .expect("the second root holds this Cook's own handoff parent")
            .run_id,
        collision_cook_id
    );

    // The fence is open in both roots, so the child may proceed in either.
    require_detached_cook_handoff_fence_open_in_store(&seeded, handoff_cook_id)
        .expect("an open fence admits the child in the injected store");

    // Attach the child, then the supervisor, through the injected store only.
    let child_pid = 424_242;
    let start_identity = homeboy_core::process::ProcessStartIdentity::Linux {
        starttime_ticks: 4_242,
    };
    let attached = record_detached_cook_handoff_child_in_store(
        &seeded,
        handoff_cook_id,
        child_pid,
        start_identity.clone(),
    )
    .expect("attach the child identity in the injected store");
    assert_eq!(
        attached.metadata["detached_cook_handoff"]["admission_state"],
        "child_attached"
    );
    assert_eq!(
        attached.metadata["detached_cook_handoff"]["child_pid"],
        child_pid
    );
    assert_eq!(
        attached.metadata["detached_cook_handoff"]["cancellation_fence"]["state"], "open",
        "attaching a child carries the durable fence forward"
    );

    record_detached_cook_supervisor_in_store(&seeded, handoff_cook_id, "daemon-job-rooted")
        .expect("attach the supervising daemon job in the injected store");

    // The positive: every write is durable in the store that was handed in.
    let supervised = seeded
        .read_record(handoff_cook_id)
        .expect("read the injected store's handoff parent back");
    assert_eq!(
        supervised.metadata["detached_cook_handoff"]["supervisor_job_id"],
        "daemon-job-rooted"
    );
    assert_eq!(
        supervised.metadata["detached_cook_handoff"]["admission_state"],
        "supervising"
    );
    assert_eq!(
        supervised.metadata["detached_cook_handoff"]["reattach_command"],
        format!("homeboy agent-task status {handoff_cook_id}")
    );
    // A supervised admission is live without consulting the process table, so
    // the predicates are decided by the record alone.
    let now = chrono::Utc::now();
    assert!(has_pending_detached_cook_handoff(&supervised));
    assert!(detached_cook_admission_is_live(&supervised, now));
    assert!(!has_expired_detached_cook_admission(&supervised, now));

    // Close the fence in the injected store only. The fence is the one piece of
    // handoff state a *child process* reads on its own, so proving it is read
    // from the store it was handed is the point of this pair.
    seeded
        .mutate_record(handoff_cook_id, |record| {
            record.metadata["detached_cook_handoff"]["cancellation_fence"]["state"] =
                json!("cancelled");
            true
        })
        .expect("close the injected store's cancellation fence");
    let fenced = require_detached_cook_handoff_fence_open_in_store(&seeded, handoff_cook_id)
        .expect_err("a cancelled fence refuses the child in the store that holds it");
    assert_eq!(
        fenced.code.as_str(),
        ErrorCode::ValidationInvalidArgument.as_str()
    );
    assert_eq!(fenced.details["field"], "cook_id");
    assert_eq!(
        fenced.details["problem"],
        "detached Cook handoff was cancelled before its attempt could materialize"
    );

    // The negative: the second root holds the same identity and saw none of it.
    // Absence first.
    let untouched = other
        .read_record(handoff_cook_id)
        .expect("the second root still has its own handoff parent");
    assert_eq!(untouched.state, AgentTaskRunState::Queued);
    assert_eq!(
        untouched.metadata["detached_cook_handoff"]["admission_state"],
        "pre_supervisor"
    );
    for key in [
        "child_pid",
        "child_start_identity",
        "child_supervisor_deadline_at",
        "supervisor_job_id",
        "reattach_command",
    ] {
        assert!(
            untouched.metadata["detached_cook_handoff"]
                .get(key)
                .is_none(),
            "second root must not observe `{key}` written through the injected store"
        );
    }

    // Then the stronger half: the second root is not merely missing the
    // evidence, it is still *eligible*. Its handoff is still pending, its fence
    // is still open, its admission lease is still live, and its parent will
    // still accept the child and supervisor the injected store already took —
    // none of which is true of state some other call already consumed.
    assert!(has_pending_detached_cook_handoff(&untouched));
    assert!(
        detached_cook_admission_is_live(&untouched, now),
        "the injected store's supervisor must not have consumed the second root's lease"
    );
    assert!(!has_expired_detached_cook_admission(&untouched, now));
    assert_eq!(
        untouched.metadata["detached_cook_handoff"]["cancellation_fence"]["state"],
        "open"
    );
    require_detached_cook_handoff_fence_open_in_store(&other, handoff_cook_id)
        .expect("the fence closed in the injected store must leave the second root's fence open");
    assert_eq!(
        record_detached_cook_handoff_child_in_store(
            &other,
            handoff_cook_id,
            child_pid,
            start_identity,
        )
        .expect("the second root's parent still accepts its own child")
        .metadata["detached_cook_handoff"]["admission_state"],
        "child_attached"
    );
    record_detached_cook_supervisor_in_store(&other, handoff_cook_id, "daemon-job-second-root")
        .expect("the second root's parent still accepts its own supervisor");
    assert_eq!(
        other
            .read_record(handoff_cook_id)
            .expect("read the second root's parent back")
            .metadata["detached_cook_handoff"]["supervisor_job_id"],
        "daemon-job-second-root"
    );
}

/// The authority fixture used by the run-outcome rooting proof.
///
/// The verifier registry is process-global by design — it is configured trust
/// material and a subprocess contract, not a durable lifecycle root — so it is
/// installed through the serializing test guard rather than by mutating any
/// environment.
struct RootedAcceptanceVerifier;

impl AgentTaskAcceptanceVerifier for RootedAcceptanceVerifier {
    fn provenance(&self) -> AgentTaskAcceptanceVerifierProvenance {
        AgentTaskAcceptanceVerifierProvenance {
            verifier: "rooted-independent-review".to_string(),
            configuration: "rooted-policy-revision-1".to_string(),
        }
    }

    fn verify_acceptance(
        &self,
        request: &AgentTaskAcceptanceVerificationRequest,
    ) -> Result<AgentTaskAcceptanceAttestation> {
        if request.token != "rooted-token" {
            return Err(Error::validation_invalid_argument(
                "token",
                "rooted fixture verifier rejected token",
                None,
                None,
            ));
        }
        Ok(AgentTaskAcceptanceAttestation {
            actor: "rooted-reviewer".to_string(),
            authority: request.requirement.authority.clone(),
            policy: request.requirement.policy.clone(),
            issued_at: "2020-01-01T00:00:00Z".to_string(),
            provider_ref: "rooted://acceptance/1".to_string(),
            signature: "rooted-signature".to_string(),
            key_id: "rooted-key".to_string(),
        })
    }

    fn revalidate_attestation(
        &self,
        request: &AgentTaskAcceptanceVerificationRequest,
        attestation: &AgentTaskAcceptanceAttestation,
    ) -> Result<()> {
        if attestation.signature != "rooted-signature"
            || attestation.authority != request.requirement.authority
            || attestation.policy != request.requirement.policy
        {
            return Err(Error::validation_invalid_argument(
                "acceptance",
                "rooted fixture attestation did not match the signed request",
                None,
                None,
            ));
        }
        Ok(())
    }
}

/// A routed placement decision that a controller-local outcome verifies.
///
/// Deliberately *not* a submission stamp: `normalize_missing_execution_placement_decision_in_store`
/// only supersedes a stamp, and the outcome recorder compares `decision_id`,
/// which is a deterministic content identity and therefore identical in both
/// roots.
fn rooted_placement_decision() -> homeboy_lab_runner_contract::ExecutionPlacementDecision {
    homeboy_lab_runner_contract::ExecutionPlacementDecision::new(
        "rooted-run-outcome-policy",
        "1",
        homeboy_lab_runner_contract::ExecutionPlacementIdentity {
            repository: "repo".to_string(),
            workspace: "workspace".to_string(),
            task: "task-a".to_string(),
            candidate: None,
            base: None,
        },
        homeboy_lab_runner_contract::Placement::Auto,
        homeboy_lab_runner_contract::ExecutionPlacementRequirement::Either,
        homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local,
        None,
        homeboy_lab_runner_contract::ExecutionPlacementFallback {
            local_allowed: false,
            reason: None,
        },
        homeboy_lab_runner_contract::ExecutionPlacementOverrideAuthorization {
            authorized: false,
            authority: None,
        },
    )
}

/// An applied promotion complete enough to open acceptance.
fn rooted_applied_promotion() -> Value {
    json!({
        "status": "applied",
        "verified_base": { "sha": "rooted-base" },
        "provenance": { "candidate": { "fingerprint": {
            "schema": "homeboy/agent-task-candidate-fingerprint/v1",
            "target_path": "/repo",
            "head": "rooted-candidate",
            "base": "rooted-candidate-parent",
            "changed_files": ["src/lib.rs"],
            "sha256": "rooted-candidate-sha",
            "tree": "rooted-candidate-tree",
        } } },
    })
}

#[test]
fn run_outcome_siblings_record_only_into_the_injected_store() {
    // The run-outcome family writes the evidence a run is *judged* by: the
    // verified placement of the execution, the authority verdict on its
    // candidate, the terminal artifact projection, the durable controller
    // failure, and the finalization that a dependency rebase re-arms. Absence
    // in a second root is far too weak a negative for that — every one of these
    // writes is equally "absent" from a root the work simply never reached.
    //
    // So both roots are seeded with the *same* run identity in the *same*
    // pre-outcome shape: the same submission stamp, the same applied promotion,
    // the same pending acceptance, the same reserved provider execution, the
    // same recorded controller failure, the same finalization, the same
    // terminal state, and the same aggregate. Every act below is made through a
    // sibling handed `seeded`. The second root is then asserted to be *still in
    // its original state* — still accepting the same verdict, still
    // un-completed, still un-projected, still un-invalidated, still bindable —
    // which is what a leak into an ambient root cannot satisfy while every
    // positive assertion here still passes (#7505).
    //
    // Two guards are proved directionally rather than by absence: the same call
    // with the same run id and the same argument is answered one way by one root
    // and the opposite way by the other, decided only by which store was
    // injected.
    //
    // No environment is mutated: both roots come from
    // `HermeticTestContext::path_roots()`. The acceptance verifier registry is
    // process-global trust material, not a lifecycle root, and is installed
    // through its own serializing guard.
    let seeded_context = homeboy_core::test_support::HermeticTestContext::new();
    let other_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(seeded_context.data_dir(), other_context.data_dir());

    let seeded =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(seeded_context.path_roots());
    let other =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(other_context.path_roots());

    let run_id = "rooted-run-outcome";
    let mut plan = test_plan();
    plan.metadata = json!({
        "acceptance": { "authority": "rooted-review", "policy": "rooted-release" },
    });
    let aggregate = succeeded_aggregate(&plan);
    let decision = rooted_placement_decision();
    let outcome = decision
        .outcome(
            homeboy_lab_runner_contract::EffectiveExecutionPlacement::Local,
            None,
        )
        .expect("a controller-local outcome verifies a local decision");

    for lifecycle_store in [&seeded, &other] {
        lifecycle_store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
            .expect("submit the same run identity into both roots");
        // A reserved provider execution, reproduced rather than produced.
        // `reserve_provider_execution_in_store` is a claim-family sibling that a
        // prior slice already rooted; what this test needs is the durable shape
        // it leaves behind, so the binding recorder has something to find.
        lifecycle_store
            .mutate_record(run_id, |record| {
                record.ensure_metadata_object().insert(
                    "provider_executions".to_string(),
                    json!([{
                        "key": "task-a:1",
                        "task_id": "task-a",
                        "attempt": 1,
                        "state": "running",
                    }]),
                );
                true
            })
            .expect("seed the same reserved provider execution into both roots");
        record_cook_controller_failure_in_store(
            lifecycle_store,
            run_id,
            &json!({ "reason": "rooted-controller-failure" }),
        )
        .expect("seed the same controller failure into both roots");
        record_cook_finalization_in_store(
            lifecycle_store,
            run_id,
            json!({ "status": "published", "pr": 1 }),
        )
        .expect("seed the same finalization into both roots");
        record_promotion_in_store(lifecycle_store, run_id, rooted_applied_promotion())
            .expect("seed the same applied promotion into both roots");
        lifecycle_store
            .mutate_record(run_id, |record| {
                set_run_state(record, AgentTaskRunState::Succeeded);
                record.updated_at = Some(now_timestamp());
                true
            })
            .expect("terminalize the same run in both roots");
        lifecycle_store
            .write_aggregate(run_id, &aggregate)
            .expect("seed the same durable aggregate into both roots");
        assert_eq!(
            lifecycle_store
                .read_record(run_id)
                .expect("read the seeded record back")
                .acceptance
                .expect("an applied promotion opens acceptance")
                .verdict,
            AgentTaskAcceptanceVerdict::Pending,
            "both roots start from the identical pending verdict"
        );
    }

    // Adopt the routed decision over the submission stamp — in the first root
    // only. The second root keeps the stamp it was submitted with, which is
    // what makes the outcome recorder answer directionally below.
    assert!(
        normalize_missing_execution_placement_decision_in_store(&seeded, run_id, &decision)
            .expect("adopt a canonical decision in the injected store"),
        "a submission stamp is superseded by a routed decision"
    );

    // DIRECTIONAL PAIR 1. The identical verified outcome, for the identical run
    // id, is accepted by the root that adopted the decision it names and refused
    // by the root that did not. Nothing about the call distinguishes them.
    record_execution_placement_outcome_in_store(&seeded, run_id, outcome.clone())
        .expect("the injected store owns the decision this outcome verifies");
    let contradicted = record_execution_placement_outcome_in_store(&other, run_id, outcome.clone())
        .expect_err("the second root's own decision contradicts this outcome");
    assert_eq!(
        contradicted.code.as_str(),
        ErrorCode::ValidationInvalidArgument.as_str()
    );
    assert_eq!(contradicted.details["field"], "execution_placement_outcome");
    assert_eq!(
        contradicted.details["problem"],
        "verified placement outcome contradicts the durable routing decision"
    );

    record_provider_execution_process_in_store(&seeded, run_id, "task-a", 1, 515_151)
        .expect("bind the provider process in the injected store");

    let _verifier = AcceptanceVerifierTestGuard::install(Box::new(RootedAcceptanceVerifier));
    let accepted = record_acceptance_verdict_in_store(
        &seeded,
        run_id,
        AgentTaskAcceptanceVerdict::Accepted,
        vec!["review://rooted/1".to_string()],
        "rooted-token".to_string(),
    )
    .expect("record the authority verdict into the injected store");
    assert_eq!(
        accepted
            .acceptance
            .as_ref()
            .expect("acceptance record")
            .verdict,
        AgentTaskAcceptanceVerdict::Accepted
    );

    clear_cook_controller_failure_in_store(&seeded, run_id)
        .expect("clear the controller failure in the injected store");
    let invalidated = invalidate_cook_finalization_for_dependency_in_store(
        &seeded,
        run_id,
        "dependency-sha",
        "homeboy agent-task cook-continue",
    )
    .expect("re-arm the finalization in the injected store");
    assert_eq!(
        invalidated.metadata["latest_promotion"]["status"],
        "verification_pending"
    );
    assert!(
        reconcile_terminal_artifact_projection_in_store(&seeded, run_id)
            .expect("reproject terminal artifacts in the injected store"),
        "a terminal record with a durable plan and aggregate is reprojectable"
    );

    // The positive: every outcome is durable in the store that was handed in.
    let persisted = seeded
        .read_record(run_id)
        .expect("read the seeded record back");
    assert_eq!(
        persisted.metadata["execution_placement_decision"]["decision_id"],
        decision.decision_id
    );
    assert_eq!(
        persisted.metadata["execution_placement_normalized"]["reason"],
        "durable run carried a submission-derived placement decision"
    );
    assert_eq!(
        persisted.metadata["execution_placement_outcome"]["decision_id"],
        decision.decision_id
    );
    assert_eq!(
        persisted.metadata["provider_executions"][0]["owner_pid"],
        515_151
    );
    let acceptance = persisted
        .acceptance
        .as_ref()
        .expect("durable acceptance record");
    assert_eq!(acceptance.verdict, AgentTaskAcceptanceVerdict::Accepted);
    assert_eq!(acceptance.actor.as_deref(), Some("rooted-reviewer"));
    assert_eq!(acceptance.signature.as_deref(), Some("rooted-signature"));
    assert!(persisted.metadata.get("cook_controller_failure").is_none());
    assert!(persisted.metadata.get("cook_finalization").is_none());
    assert_eq!(
        persisted.metadata["cook_recovery_source_checkpoint"]["dependency_revision"],
        "dependency-sha"
    );
    assert_eq!(
        persisted.metadata["latest_promotion"]["status"],
        "verification_pending"
    );
    assert_eq!(
        persisted.metadata["artifact_projection"]["status"],
        "complete"
    );

    // The negative, absence first: the second root holds the same run identity
    // and observed none of it.
    let untouched = other
        .read_record(run_id)
        .expect("the second root still has its own record");
    assert_eq!(
        untouched.metadata["execution_placement_decision"]["policy_id"],
        homeboy_lab_runner_contract::CONTROLLER_LOCAL_SUBMISSION_POLICY_ID,
        "the second root's decision must still be the stamp it was submitted with"
    );
    for key in [
        "execution_placement_normalized",
        "execution_placement_outcome",
        "artifact_projection",
        "cook_recovery_source_checkpoint",
    ] {
        assert!(
            untouched.metadata.get(key).is_none(),
            "second root must not observe `{key}` written through the injected store"
        );
    }
    assert!(
        untouched.metadata["provider_executions"][0]
            .get("owner_pid")
            .is_none(),
        "second root must not observe the provider process bound through the injected store"
    );

    // Then the stronger half: the second root is not merely missing the
    // evidence, it is still in its *original* state. Its controller failure
    // still stands, its finalization is still published, its promotion is still
    // applied, and its verdict is still pending — none of which is true of
    // outcome state some other call already recorded.
    assert_eq!(
        untouched.metadata["cook_controller_failure"]["reason"], "rooted-controller-failure",
        "clearing the injected store's failure must not clear the second root's"
    );
    assert_eq!(
        untouched.metadata["cook_finalization"]["status"],
        "published"
    );
    assert_eq!(untouched.metadata["latest_promotion"]["status"], "applied");
    assert_eq!(
        untouched
            .acceptance
            .as_ref()
            .expect("the second root keeps its own acceptance record")
            .verdict,
        AgentTaskAcceptanceVerdict::Pending
    );

    // And it is still *eligible*: every operation the injected store already
    // performed can still be performed here, on the second root's own state.
    let second_verdict = record_acceptance_verdict_in_store(
        &other,
        run_id,
        AgentTaskAcceptanceVerdict::Accepted,
        vec!["review://rooted/2".to_string()],
        "rooted-token".to_string(),
    )
    .expect("the second root still accepts the same verdict")
    .acceptance
    .expect("acceptance record");
    assert_eq!(second_verdict.verdict, AgentTaskAcceptanceVerdict::Accepted);
    assert!(
        second_verdict.history.is_empty(),
        "the second root moved from pending, so the injected store's verdict was never recorded here"
    );
    record_provider_execution_process_in_store(&other, run_id, "task-a", 1, 626_262)
        .expect("the second root's reservation is still bindable");
    clear_cook_controller_failure_in_store(&other, run_id)
        .expect("the second root still owns its controller failure");
    assert!(
        reconcile_terminal_artifact_projection_in_store(&other, run_id)
            .expect("the second root is still reprojectable"),
        "the injected store's projection must not consume the second root's"
    );
    let second_invalidated = invalidate_cook_finalization_for_dependency_in_store(
        &other,
        run_id,
        "dependency-sha",
        "homeboy agent-task cook-continue",
    )
    .expect("the second root's finalization is still invalidatable");
    assert!(
        second_invalidated
            .metadata
            .get("cook_finalization")
            .is_none(),
        "the second root still had a finalization of its own to re-arm"
    );

    let second_root = other
        .read_record(run_id)
        .expect("read the second root's record back");
    assert_eq!(
        second_root.metadata["provider_executions"][0]["owner_pid"],
        626_262
    );
    assert!(second_root
        .metadata
        .get("cook_controller_failure")
        .is_none());
    assert_eq!(
        second_root.metadata["artifact_projection"]["status"],
        "complete"
    );

    // DIRECTIONAL PAIR 2. The identical adoption offer, for the identical run
    // id, is refused by the root that already holds this decision and accepted
    // by the root whose submission stamp was never superseded.
    assert!(
        !normalize_missing_execution_placement_decision_in_store(&seeded, run_id, &decision)
            .expect("re-offering the adopted decision is answered, not errored"),
        "the injected store already carries this exact decision"
    );
    assert!(
        normalize_missing_execution_placement_decision_in_store(&other, run_id, &decision)
            .expect("the second root still holds an unsuperseded submission stamp"),
        "an adoption made in the injected store must not decide the second root"
    );
}

#[test]
fn run_reentry_siblings_re_enter_only_the_injected_store() {
    // Re-entry is the family that decides whether a durable run may run *again*:
    // resume, retry, and the quarantine/re-arm pair that gates both. Absence in a
    // second root is a uselessly weak negative for that — a root the work never
    // reached is equally "not resumed", "not retried" and "not re-armed". What
    // separates a leak from a clean rooting is whether the second root is still
    // *eligible*, so both roots are seeded with the same four identities in the
    // same pre-re-entry shape: a retry source, a resumable queued run, a clean
    // queued run, and a quarantined queued run. Every act below is made through
    // a sibling handed `seeded`, and the second root is then asserted to be
    // still retryable, still resumable, still quarantinable and still
    // quarantined — none of which is true of state some other call consumed.
    //
    // Four guards are proved directionally rather than by absence: the same call
    // with the same run id is answered one way by one root and the opposite way
    // by the other, decided only by which store was injected.
    //
    // No environment is mutated: both roots come from
    // `HermeticTestContext::path_roots()`.
    let seeded_context = homeboy_core::test_support::HermeticTestContext::new();
    let other_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(seeded_context.data_dir(), other_context.data_dir());

    let seeded =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(seeded_context.path_roots());
    let other =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(other_context.path_roots());

    let retry_source = "rooted-reentry-retry-source";
    let cook_id = "rooted-reentry-cook";
    let first_successor = "rooted-reentry-cook-attempt-1-successor";
    let second_successor = "rooted-reentry-retry-second";
    let resume_target = "rooted-reentry-resume";
    let quarantine_target = "rooted-reentry-quarantine";
    let rearm_target = "rooted-reentry-rearm";
    let plan = test_plan();

    // A run submitted with a stub admission carries `controller_runtime: {}` — a
    // pin naming no immutable executable. Every lifecycle mutation validates the
    // pin before it runs, and that validation resolves
    // `paths::controller_runtimes_store()`, which is machine-global by design
    // and would be the operator's real home here. A record with no pin at all is
    // the shape the pin validators explicitly tolerate and return early on, so
    // the seed drops the stub rather than reaching a root this test does not
    // own. Controller admission itself is likewise never driven: the retry below
    // is handed a stub admission and no queue projection, exactly as the
    // detached-handoff and run-outcome proofs do.
    let seed_queued = |lifecycle_store: &crate::agent_task_lifecycle::AgentTaskLifecycleStore,
                       run_id: &str| {
        lifecycle_store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
            .expect("submit the same run identity into both roots");
        lifecycle_store
            .mutate_record(run_id, |record| {
                record
                    .ensure_metadata_object()
                    .remove(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY);
                true
            })
            .expect("seed an unpinned queued record into both roots");
    };

    for lifecycle_store in [&seeded, &other] {
        for run_id in [retry_source, resume_target, quarantine_target, rearm_target] {
            seed_queued(lifecycle_store, run_id);
        }
        // The quarantined half of the pair, identical in both roots.
        quarantine_queued_run_exact_in_store(lifecycle_store, rearm_target, "seeded quarantine")
            .expect("seed the same operator quarantine into both roots");
        assert_eq!(
            lifecycle_store
                .read_record(rearm_target)
                .expect("read the seeded quarantine back")
                .metadata["queue_quarantine"]["operator_reason"],
            "seeded quarantine",
            "both roots start from the identical quarantined state"
        );
    }

    // Reserve one retry successor, resume one run, quarantine one clean run and
    // re-arm the quarantined one — all through siblings handed `seeded`.
    let reserved = retry_with_runtime_admission_in_store(
        &seeded,
        retry_source,
        Some(first_successor),
        false,
        true,
        None,
        |_| Ok(json!({})),
    )
    .expect("reserve a retry successor in the injected store");
    assert_eq!(reserved.run_id, first_successor);

    let resumed = mark_resuming_in_store(&seeded, resume_target)
        .expect("resume the queued run in the injected store");
    assert_eq!(resumed.state, AgentTaskRunState::Running);

    quarantine_queued_run_exact_in_store(&seeded, quarantine_target, "operator halted this run")
        .expect("quarantine the clean queued run in the injected store");
    rearm_quarantined_run_in_store(&seeded, rearm_target)
        .expect("re-arm the quarantined run in the injected store");

    // The positive: every re-entry decision is durable in the store handed in.
    let reserved_record = seeded
        .read_record(first_successor)
        .expect("the injected store holds the reserved successor");
    assert_eq!(reserved_record.metadata["retry_of"], retry_source);
    assert_eq!(reserved_record.metadata["retried_from"], retry_source);
    assert_eq!(reserved_record.metadata["retry_root"], retry_source);
    let seeded_source = seeded
        .read_record(retry_source)
        .expect("read the injected store's retry source back");
    assert_eq!(seeded_source.metadata["retries"], json!([first_successor]));
    assert_eq!(seeded_source.metadata["retry_root"], retry_source);
    let seeded_resume = seeded
        .read_record(resume_target)
        .expect("read the injected store's resumed run back");
    assert_eq!(seeded_resume.state, AgentTaskRunState::Running);
    assert!(seeded_resume.metadata.get("resume_requested_at").is_some());
    assert_eq!(
        seeded
            .read_record(quarantine_target)
            .expect("read the injected store's quarantined run back")
            .metadata["queue_quarantine"]["operator_reason"],
        "operator halted this run"
    );
    assert!(
        seeded
            .read_record(rearm_target)
            .expect("read the injected store's re-armed run back")
            .metadata
            .get("queue_quarantine")
            .is_none(),
        "the re-arm must clear the marker in the store that was handed in"
    );

    // The negative, absence first: the second root holds all four identities and
    // observed none of it. The successor is absent from its records *and* from
    // its indexed retry lineage, which is the read a claim would double-book on.
    assert!(
        other.read_record(first_successor).is_err(),
        "the second root must not hold a successor reserved through the injected store"
    );
    assert!(
        other
            .read_retry_successors(retry_source)
            .expect("the second root's own retry lineage")
            .is_empty(),
        "the second root's indexed lineage must not observe the injected store's reservation"
    );
    let untouched_source = other
        .read_record(retry_source)
        .expect("the second root still has its own retry source");
    assert!(untouched_source.metadata.get("retries").is_none());
    assert!(untouched_source.metadata.get("retry_root").is_none());
    let untouched_resume = other
        .read_record(resume_target)
        .expect("the second root still has its own resumable run");
    assert!(untouched_resume
        .metadata
        .get("resume_requested_at")
        .is_none());
    assert!(
        other
            .read_record(quarantine_target)
            .expect("the second root still has its own clean queued run")
            .metadata
            .get("queue_quarantine")
            .is_none(),
        "second root must not observe the quarantine written through the injected store"
    );

    // Then the stronger half: the second root is not merely missing the
    // evidence, it is still in its *original eligible* state. Its retry source
    // is still unreserved, its resumable run is still queued, and its
    // quarantined run is still quarantined with the reason it was seeded with.
    assert_eq!(untouched_source.state, AgentTaskRunState::Queued);
    assert_eq!(untouched_resume.state, AgentTaskRunState::Queued);
    let untouched_rearm = other
        .read_record(rearm_target)
        .expect("the second root still has its own quarantined run");
    assert_eq!(untouched_rearm.state, AgentTaskRunState::Queued);
    assert_eq!(
        untouched_rearm.metadata["queue_quarantine"]["operator_reason"], "seeded quarantine",
        "a re-arm made through the injected store must not clear the second root's quarantine"
    );

    // DIRECTIONAL PAIR 1. The identical resume, for the identical run id, is
    // admitted by the root that re-armed it and refused by the root where it is
    // still quarantined. The quarantine guard lives inside `mark_running`'s
    // mutation closure, so this is the assertion that it reads the injected
    // store rather than an ambient copy of the same identity.
    assert_eq!(
        mark_resuming_in_store(&seeded, rearm_target)
            .expect("the injected store re-armed this run, so it may resume")
            .state,
        AgentTaskRunState::Running
    );
    let still_quarantined = mark_resuming_in_store(&other, rearm_target)
        .expect_err("the second root's run is still quarantined and must refuse to resume");
    assert_eq!(
        still_quarantined.code.as_str(),
        ErrorCode::ValidationInvalidArgument.as_str()
    );
    assert_eq!(still_quarantined.details["field"], "run_id");
    assert_eq!(
        still_quarantined.details["problem"],
        "agent-task run is quarantined; re-arm its exact run id after repairing durable provenance"
    );

    // DIRECTIONAL PAIR 2. The identical re-arm, for the identical run id, is
    // accepted by the root that holds the quarantine and refused by the root
    // that never observed it.
    assert!(rearm_quarantined_run_in_store(&seeded, quarantine_target)
        .expect("the injected store holds this quarantine and can clear it")
        .metadata
        .get("queue_quarantine")
        .is_none());
    let never_quarantined = rearm_quarantined_run_in_store(&other, quarantine_target)
        .expect_err("the second root never observed this quarantine");
    assert_eq!(never_quarantined.details["field"], "run_id");
    assert_eq!(
        never_quarantined.details["problem"],
        "only an exact queued quarantined run can be re-armed"
    );

    // DIRECTIONAL PAIR 3. The identical Cook retry-successor lookup, for the
    // identical source and attempt, finds the reservation in the root it was
    // made in and nothing in the other. A caller reads `None` as authority to
    // create a reservation, so this is the read that would mint a second
    // successor over a live one.
    assert_eq!(
        find_unbound_cook_retry_successor_in_store(&seeded, retry_source, cook_id, 1, &plan)
            .expect("the injected store's own retry lineage")
            .map(|record| record.run_id),
        Some(first_successor.to_string())
    );
    assert!(
        find_unbound_cook_retry_successor_in_store(&other, retry_source, cook_id, 1, &plan)
            .expect("the second root's own retry lineage")
            .is_none(),
        "a reservation made in the injected store must not be adopted by the second root"
    );

    // DIRECTIONAL PAIR 4, and the remaining eligibility proofs. The identical
    // retry request, for the identical source, is refused by the root that
    // already holds an active successor and admitted by the root that does not —
    // which is simultaneously the proof that the second root is still retryable.
    let active = retry_with_runtime_admission_in_store(
        &seeded,
        retry_source,
        Some(second_successor),
        false,
        true,
        None,
        |_| Ok(json!({})),
    )
    .expect_err("the injected store's active successor forbids a second reservation");
    assert_eq!(
        active.code.as_str(),
        ErrorCode::ValidationInvalidArgument.as_str()
    );
    assert!(
        active.message.contains(first_successor),
        "the refusal must name the injected store's own active successor: {}",
        active.message
    );
    assert_eq!(
        retry_with_runtime_admission_in_store(
            &other,
            retry_source,
            Some(second_successor),
            false,
            true,
            None,
            |_| Ok(json!({})),
        )
        .expect("the second root holds no active successor and is still retryable")
        .run_id,
        second_successor
    );

    // The second root is still resumable and still quarantinable on its own
    // state, and its own quarantine is still clearable.
    assert_eq!(
        mark_resuming_in_store(&other, resume_target)
            .expect("the second root's queued run is still resumable")
            .state,
        AgentTaskRunState::Running
    );
    assert_eq!(
        quarantine_queued_run_exact_in_store(&other, quarantine_target, "second root quarantine")
            .expect("the second root's clean queued run is still quarantinable")
            .metadata["queue_quarantine"]["operator_reason"],
        "second root quarantine"
    );
    assert!(rearm_quarantined_run_in_store(&other, rearm_target)
        .expect("the second root still owns the quarantine it was seeded with")
        .metadata
        .get("queue_quarantine")
        .is_none());
}

#[test]
fn lifecycle_read_siblings_answer_from_the_injected_store_and_not_a_second_root() {
    // The positive half of this proof is cheap; the negative half is the point.
    // A read sibling that ignored its store parameter and resolved the ambient
    // root would still satisfy every assertion below against `seeded`, because
    // the state really is there. Only a second, differently-rooted store can
    // distinguish "read the store I was handed" from "read whatever the process
    // environment points at" (#7505). No environment is mutated here: both
    // roots come from `HermeticTestContext::path_roots()`.
    let seeded_context = homeboy_core::test_support::HermeticTestContext::new();
    let empty_context = homeboy_core::test_support::HermeticTestContext::new();
    assert_ne!(seeded_context.data_dir(), empty_context.data_dir());

    let seeded =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(seeded_context.path_roots());
    let empty =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(empty_context.path_roots());

    let cook_id = "rooted-read-cook";
    let attempt_run_id = "rooted-read-cook-attempt-1-a";
    let plan = test_plan();
    seeded
        .submit_plan_with_runtime_admission(&plan, attempt_run_id, |_| Ok(json!({})))
        .expect("submit attempt into the seeded store");
    seeded
        .write_cook_index_attempt(cook_id, 1, attempt_run_id, now_timestamp(), None)
        .expect("register the Cook attempt in the seeded store");
    seeded
        .write_aggregate(attempt_run_id, &succeeded_aggregate(&plan))
        .expect("persist the attempt aggregate in the seeded store");
    let aggregate_path = seeded.aggregate_path(attempt_run_id);

    // Every read answers from the store it was handed.
    assert_eq!(
        exact_record_in_store(&seeded, attempt_run_id)
            .expect("exact record")
            .run_id,
        attempt_run_id
    );
    assert!(run_record_exists_in_store(&seeded, attempt_run_id).expect("record exists"));
    assert!(run_record_exists_readonly_in_store(&seeded, attempt_run_id)
        .expect("record exists read-only"));
    assert!(run_record_exists_resolved_in_store(&seeded, cook_id).expect("resolved record exists"));
    assert!(cook_index_exists_in_store(&seeded, cook_id).expect("cook index exists"));
    assert_eq!(
        cook_index_in_store(&seeded, cook_id)
            .expect("cook index")
            .latest_run_id,
        attempt_run_id
    );
    assert_eq!(
        resolve_run_id_in_store(&seeded, cook_id).expect("resolve cook alias"),
        attempt_run_id
    );
    assert_eq!(
        status_in_store(&seeded, cook_id)
            .expect("persisted status")
            .run_id,
        attempt_run_id
    );
    assert_eq!(
        durable_local_read_in_store(&seeded, cook_id)
            .expect("durable local read")
            .record
            .run_id,
        attempt_run_id
    );
    assert!(exact_durable_local_read_in_store(&seeded, attempt_run_id)
        .expect("exact durable local read")
        .aggregate
        .is_some());
    assert_eq!(
        load_plan_in_store(&seeded, cook_id)
            .expect("load plan")
            .plan_id,
        plan.plan_id
    );
    assert_eq!(
        load_controller_plan_in_store(&seeded, cook_id)
            .expect("load controller plan")
            .plan_id,
        plan.plan_id
    );
    assert_eq!(
        artifacts_in_store(&seeded, attempt_run_id)
            .expect("artifacts")
            .run_id,
        attempt_run_id
    );
    assert_eq!(
        read_aggregate_in_store(&seeded, cook_id)
            .expect("aggregate through the Cook alias")
            .plan_id,
        plan.plan_id
    );
    assert_eq!(
        read_attempt_aggregate_in_store(&seeded, attempt_run_id)
            .expect("attempt aggregate")
            .plan_id,
        plan.plan_id
    );
    assert_eq!(
        run_id_for_aggregate_path_in_store(&seeded, &aggregate_path)
            .expect("aggregate path lookup"),
        Some(attempt_run_id.to_string())
    );
    assert_eq!(
        running_owner_pid_in_store(&seeded, attempt_run_id).expect("owner pid"),
        None
    );
    assert!(
        !has_active_provider_execution_in_store(&seeded, attempt_run_id)
            .expect("provider execution predicate")
    );
    assert_eq!(
        reconcile_scope_run_ids_in_store(&seeded, cook_id).expect("reconcile scope"),
        vec![attempt_run_id.to_string()]
    );
    assert!(
        find_unbound_cook_retry_successor_in_store(&seeded, attempt_run_id, cook_id, 2, &plan)
            .expect("retry successor lookup")
            .is_none()
    );
    let (records, _) = read_records_with_health_in_store(&seeded).expect("registry snapshot");
    assert_eq!(
        records
            .iter()
            .map(|record| record.run_id.as_str())
            .collect::<Vec<_>>(),
        vec![attempt_run_id]
    );
    let (bounded, _) =
        read_records_with_health_bounded_in_store(&seeded, 10).expect("bounded registry snapshot");
    assert_eq!(bounded.len(), 1);
    let (all_records, _) =
        read_all_records_with_health_in_store(&seeded).expect("unbounded registry snapshot");
    assert_eq!(all_records.len(), 1);

    // The negative: an identically-named identity in a second root is absent.
    // Any sibling that reached for the ambient root would answer these from the
    // seeded state above and fail here.
    assert!(!run_record_exists_in_store(&empty, attempt_run_id).expect("no record in second root"));
    assert!(!run_record_exists_readonly_in_store(&empty, attempt_run_id)
        .expect("no record in second root"));
    assert!(!run_record_exists_resolved_in_store(&empty, cook_id)
        .expect("no resolved record in second root"));
    assert!(!cook_index_exists_in_store(&empty, cook_id).expect("no cook index in second root"));
    assert!(cook_index_in_store(&empty, cook_id).is_err());
    assert!(exact_record_in_store(&empty, attempt_run_id).is_err());
    assert_eq!(
        resolve_run_id_in_store(&empty, cook_id).expect("unresolvable alias echoes the id"),
        cook_id
    );
    assert!(status_in_store(&empty, cook_id).is_err());
    assert!(durable_local_read_in_store(&empty, cook_id).is_err());
    assert!(exact_durable_local_read_in_store(&empty, attempt_run_id).is_err());
    assert!(load_plan_in_store(&empty, cook_id).is_err());
    assert!(load_controller_plan_in_store(&empty, cook_id).is_err());
    assert!(artifacts_in_store(&empty, attempt_run_id).is_err());
    assert!(read_aggregate_in_store(&empty, cook_id).is_err());
    assert!(read_attempt_aggregate_in_store(&empty, attempt_run_id).is_err());
    assert!(running_owner_pid_in_store(&empty, attempt_run_id).is_err());
    assert!(has_active_provider_execution_in_store(&empty, attempt_run_id).is_err());
    assert_eq!(
        run_id_for_aggregate_path_in_store(&empty, &aggregate_path)
            .expect("aggregate path lookup in second root"),
        None
    );
    assert_eq!(
        reconcile_scope_run_ids_in_store(&empty, cook_id).expect("reconcile scope in second root"),
        vec![cook_id.to_string()]
    );
    assert!(
        find_unbound_cook_retry_successor_in_store(&empty, attempt_run_id, cook_id, 2, &plan)
            .expect("retry successor lookup in second root")
            .is_none()
    );
    assert!(read_records_with_health_in_store(&empty)
        .expect("registry snapshot in second root")
        .0
        .is_empty());
    assert!(read_records_with_health_bounded_in_store(&empty, 10)
        .expect("bounded registry snapshot in second root")
        .0
        .is_empty());
    assert!(read_all_records_with_health_in_store(&empty)
        .expect("unbounded registry snapshot in second root")
        .0
        .is_empty());
}

#[test]
fn a_supervision_stop_survives_an_hour_of_routine_resource_samples() {
    // #7015: the point of the evidence is to answer "why was this stopped?"
    // after the fact. If the decision shared an array with the sample stream, a
    // long run would evict the answer with the noise.
    //
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). Three hundred routine samples and the two decisions they must not
    // evict are one durable record; reading the surviving window from a home
    // other than the one they were appended to would prove nothing about the
    // eviction policy.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-supervision-evidence";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit run");

    let stopped = record_cook_supervision_in_store(
        &lifecycle_store,
        run_id,
        1,
        Some(json!({ "rss_mib": 10_500, "child_processes": 41 })),
        vec![json!({
            "metric": "rss_mib",
            "action": "stop",
            "limit": 10_240,
            "observed": 10_500
        })],
    )
    .expect("record the breach");
    assert_eq!(
        stopped.metadata["cook_supervision_events"][0]["decision"]["action"],
        "stop"
    );

    let executed = record_cook_supervision_stop_in_store(
        &lifecycle_store,
        run_id,
        1,
        json!({ "status": "terminated", "signal": "SIGTERM" }),
    )
    .expect("record the termination outcome");
    assert_eq!(
        executed.metadata["cook_supervision_events"][1]["kind"],
        "stop_executed"
    );

    // Enough routine heartbeats to overrun the bounded timeline window.
    let mut record = executed;
    for beat in 0..300 {
        record = record_cook_supervision_in_store(
            &lifecycle_store,
            run_id,
            1,
            Some(json!({ "rss_mib": beat })),
            Vec::new(),
        )
        .expect("record a routine sample");
    }

    let timeline = record.metadata["cook_resource_timeline"]
        .as_array()
        .expect("timeline is an array");
    assert_eq!(
        timeline.len(),
        240,
        "the timeline is a bounded rolling window"
    );
    assert_eq!(
        timeline.last().expect("newest sample")["sample"]["rss_mib"],
        299,
        "the window keeps the most recent samples"
    );

    let events = record.metadata["cook_supervision_events"]
        .as_array()
        .expect("events are an array");
    assert_eq!(
        events.len(),
        2,
        "routine samples never displace a supervision decision"
    );
    assert_eq!(events[1]["kind"], "stop_executed");
}

#[test]
fn a_termination_the_host_could_not_carry_out_is_recorded_as_a_failure() {
    // A policy can order a stop that the host then fails to execute. Evidence
    // that showed only the order would let a reader conclude a run was stopped
    // when it is still running.
    //
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). The returned record is the one the store committed, so the
    // failed-termination evidence is asserted about this home's run.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cook-supervision-failed-stop";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submit run");

    let record = record_cook_supervision_stop_in_store(
        &lifecycle_store,
        run_id,
        1,
        json!({
            "status": "termination_failed",
            "error": "process-tree cancellation is only supported on Unix hosts"
        }),
    )
    .expect("record the failed termination");

    assert_eq!(
        record.metadata["cook_supervision_events"][0]["outcome"]["status"],
        "termination_failed"
    );
}

#[test]
fn provider_run_result_reads_declared_output_alias() {
    let role_aliases: AgentTaskProviderRoleAliases = serde_json::from_value(json!({
        "outputs": {
            "provider_run_result": ["custom_run_result"]
        }
    }))
    .expect("role aliases");
    let outcome = AgentTaskOutcome {
        task_id: "task-a".to_string(),
        status: crate::agent_task::AgentTaskOutcomeStatus::Failed,
        outputs: json!({
            "custom_run_result": {
                "run_id": "custom-run-1"
            }
        }),
        ..Default::default()
    };

    assert_eq!(
        provider_run_result(&outcome, &role_aliases)
            .and_then(|result| result.get("run_id"))
            .and_then(Value::as_str),
        Some("custom-run-1")
    );
}

/// Stays on `with_isolated_home` (#7505). `validate_controller_runtime_in_store`
/// reaches `migrate_record_controller_runtime_in_store`, which calls
/// `controller_runtime::migrate_legacy_pin_and_persist` — that resolves
/// `runtime_root()` and takes the machine-global admission lock under
/// `paths::controller_runtimes_store()`, which is deliberately not a lifecycle
/// root. Rooted, this test would take a cross-process lock on the *real
/// operator* home. It is also a legacy-pin provenance fixture, so the pins have
/// to be real rather than a stub `{}`.
#[cfg(unix)]
#[test]
fn legacy_v1_pin_migration_failures_leave_durable_record_unchanged() {
    with_isolated_home(|_| {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary fake controller directory");
        let identity = homeboy_core::build_identity::current().display;
        let record = submit_plan(&test_plan(), Some("migration-failure")).expect("submit");
        let cases = [
            (
                "missing",
                temporary.path().join("missing-homeboy"),
                None,
                "missing",
            ),
            (
                "non-executable",
                temporary.path().join("non-executable-homeboy"),
                Some(identity.clone()),
                "not executable",
            ),
            (
                "identity-mismatch",
                temporary.path().join("wrong-identity-homeboy"),
                Some("homeboy test+wrong".to_string()),
                "build identity mismatch",
            ),
        ];

        for (name, legacy, artifact_identity, expected_error) in cases {
            if let Some(artifact_identity) = artifact_identity {
                fake_controller_artifact(&legacy, &artifact_identity, name);
                if name == "non-executable" {
                    std::fs::set_permissions(&legacy, std::fs::Permissions::from_mode(0o600))
                        .expect("remove executable permission");
                }
            }
            rewrite_record_for_test(&record.run_id, |record| {
                record.metadata
                    [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                    "originating": {
                        "build_identity": identity,
                        "pinned_executable": legacy,
                    }
                });
            })
            .expect("project v1 legacy pin");
            let before = reconcile_status(&record.run_id).expect("record before migration");

            let error =
                validate_controller_runtime_in_store(&test_lifecycle_store(), &record.run_id)
                    .expect_err("legacy migration fails closed");

            assert!(
                error.message.contains(expected_error),
                "{name}: {}",
                error.message
            );
            assert_eq!(
                reconcile_status(&record.run_id).expect("record after migration"),
                before
            );
        }
    });
}

#[test]
fn local_cook_logs_surface_running_provider_execution_before_aggregate() {
    // #8396: while a local cook runs the provider (no aggregate yet), logs must
    // advance past "task submitted" to reflect the durable running provider
    // execution, so operators can tell active execution from a hung preflight.
    //
    // Rooted in an explicit store rather than a mutated process environment
    // (#7505). The log projection is a read of exactly the record this test
    // rewrote, so both halves have to name the same home or the projection is
    // describing somebody else's provider execution.
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "local-cook-running";
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("submitted");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record.metadata["provider_executions"] = serde_json::json!([{
            "key": "task:1",
            "task_id": "task",
            "attempt": 1,
            "backend": "opencode",
            "model": "openai/gpt-5.6-sol",
            "state": "running",
            "started_at": "2026-07-24T00:00:00Z"
        }]);
    })
    .expect("running provider execution recorded");

    let log =
        logs_in_store(&lifecycle_store, run_id).expect("logs resolve for a running local cook");
    let messages: Vec<&str> = log
        .events
        .iter()
        .filter_map(|event| event.data["message"].as_str())
        .collect();
    assert!(
        messages
            .iter()
            .any(|message| message.contains("provider execution running")
                && message.contains("opencode")
                && message.contains("openai/gpt-5.6-sol")),
        "logs must surface the running provider execution, got: {messages:?}"
    );
}

#[test]
fn cancelled_local_provider_retains_runtime_evidence_in_terminal_logs() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "local-cook-cancelled-with-output";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved provider execution");

    // This simulates a provider that emitted output before controller cancellation.
    let stdout = context
        .path_roots()
        .artifacts()
        .join("provider-runtime-stdout-1.log");
    std::fs::write(&stdout, "provider emitted diagnostic output\n").expect("provider output");
    record_provider_execution_runtime_evidence_in_store(
        &lifecycle_store,
        run_id,
        &plan.tasks[0].task_id,
        1,
        Some(format!("file://{}", stdout.display())),
        None,
    )
    .expect("runtime evidence recorded before cancellation");

    super::super::cancellation::cancel_run_in_store(
        &lifecycle_store,
        run_id,
        Some("deterministic test cancellation"),
    )
    .expect("cancelled");

    let log = logs_in_store(&lifecycle_store, run_id).expect("terminal logs");
    let terminal = log
        .events
        .iter()
        .find(|event| {
            event.data["state"] == "cancelled"
                && event
                    .data
                    .get("message")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|message| message.contains("provider execution cancelled"))
        })
        .expect("cancelled provider event");
    let stdout_ref = terminal
        .artifacts
        .iter()
        .find(|reference| reference.kind == "provider-runtime-stdout")
        .expect("bounded stdout reference");
    assert_eq!(stdout_ref.uri, format!("file://{}", stdout.display()));
    assert_eq!(
        std::fs::read_to_string(stdout_ref.uri.strip_prefix("file://").expect("file uri"))
            .expect("retained provider output"),
        "provider emitted diagnostic output\n"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The liveness predicate is a read of the same durable provider
/// execution the reservation and terminalization wrote, so answering it from an
/// ambient home would report on a boundary this test never opened.
#[test]
fn terminal_provider_execution_is_inactive_while_cleanup_timing_remains_durable() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "policy-denied-terminalization";
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
        .expect("submitted");
    reserve_provider_execution_in_store(&lifecycle_store, run_id, &plan.tasks[0], 1)
        .expect("reserved");

    assert!(
        has_active_provider_execution_in_store(&lifecycle_store, run_id)
            .expect("active provider execution"),
        "the heartbeat may sample only while the provider boundary is active"
    );

    record_provider_execution_terminal_in_store(
        &lifecycle_store,
        run_id,
        &plan.tasks[0].task_id,
        1,
        "failed",
    )
    .expect("policy-denied provider terminalized");
    record_provider_execution_cleanup_elapsed_in_store(
        &lifecycle_store,
        run_id,
        &plan.tasks[0].task_id,
        1,
        17,
    )
    .expect("cleanup timing recorded");

    assert!(
        !has_active_provider_execution_in_store(&lifecycle_store, run_id)
            .expect("terminal provider execution"),
        "controller artifact cleanup must not keep provider liveness active"
    );
    let record = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("durable record")
    .record;
    let execution = &record.metadata["provider_executions"][0];
    assert_eq!(execution["state"], "failed");
    assert!(execution["finished_at"].is_string());
    assert_eq!(execution["post_provider_cleanup_elapsed_ms"], 17);
    assert!(execution["post_provider_cleanup_finished_at"].is_string());
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The mode check reads the exact `plan.json` the submission wrote and
/// the status read has to agree about that same path, so both halves name one
/// home.
#[cfg(unix)]
#[test]
fn submit_plan_persists_owner_only_plan_file_before_observation() {
    use std::os::unix::fs::PermissionsExt;

    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), "private-plan", |_| Ok(json!({})))
        .expect("submitted");

    assert_eq!(
        std::fs::metadata(&record.plan_path)
            .expect("plan metadata")
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(
        reconcile_status_in_store(
            &lifecycle_store,
            &record.run_id,
            AgentTaskStatusOptions::default(),
            false,
        )
        .expect("observation record")
        .record
        .plan_path,
        record.plan_path
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "One identity" is the claim: the acceptance, the three reads that
/// resolve it and the cancellation that terminalizes it all have to reach the
/// same durable record, which is exactly what an injected store makes checkable
/// and an ambient home does not.
///
/// The cancellation hook stays as it is. It is a `thread_local`, not a
/// process-global registry, so it needs no serializing home of its own.
#[test]
fn detached_lab_run_plan_uses_one_identity_for_status_logs_artifacts_and_cancellation() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "agent-task-detached-run-plan";
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
        "--record-run-id".to_string(),
        run_id.to_string(),
    ];
    record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-8341",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("detached run-plan is bound to the controller run");

    let status = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("controller identity resolves status")
    .record;
    let logs = logs_in_store(&lifecycle_store, run_id).expect("controller identity resolves logs");
    let artifacts = artifacts_in_store(&lifecycle_store, run_id)
        .expect("controller identity resolves artifacts");
    assert_eq!(status.run_id, run_id);
    assert_eq!(status.metadata["runner_job_id"], "job-8341");
    assert!(!logs.events.is_empty());
    assert!(artifacts.artifacts.is_empty());

    let _cancel = super::cancellation::test_cancel_hook::install(Box::new(
        |runner_id, job_id, durable_run_id| {
            assert_eq!(runner_id, "homeboy-lab");
            assert_eq!(job_id, "job-8341");
            assert_eq!(durable_run_id, "agent-task-detached-run-plan");
            Ok((
                homeboy_core::api_jobs::Job {
                    id: uuid::Uuid::new_v4(),
                    operation: "runner.exec".to_string(),
                    status: homeboy_core::api_jobs::JobStatus::Cancelled,
                    created_at_ms: 1,
                    updated_at_ms: 2,
                    started_at_ms: Some(1),
                    finished_at_ms: Some(2),
                    event_count: 0,
                    source_snapshot: None,
                    path_materialization_plan: None,
                    stale_reason: None,
                    daemon_lease_id: None,
                    target_runner_id: None,
                    target_project_id: None,
                    claim_id: None,
                    claimed_by_runner_id: None,
                    claimed_at_ms: None,
                    claim_expires_at_ms: None,
                    artifacts: Vec::new(),
                    runner_job_projection: None,
                },
                Vec::new(),
            ))
        },
    ));
    let cancelled = cancel_run_in_store(
        &lifecycle_store,
        run_id,
        Some("operator requested cancellation"),
    )
    .expect("canonical cancellation reaches the runner job");
    assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
    assert_eq!(
        cancelled.metadata["live_cancellation"]["cancellation"],
        "runner_job_cancel"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Both pins are *supplied by this test* through the admission
/// callback rather than produced by real controller admission, so rooting adds
/// no stub here and takes no assertion away: the controller pin the runner
/// submission must preserve is the one written two calls earlier, into this
/// store. The preservation is only a claim about one home — a mirrored record
/// read ambiently could carry another installation's pin and still satisfy
/// every `assert_ne!` below.
///
/// The two submissions are spelled through
/// `submit_plan_with_runtime_admission_in_store`, which is the same body the
/// ambient `submit_plan_with_runtime_admission` and its `_on_runner` sibling
/// delegate to. The one difference is the admission-status projection, which is
/// passed `None` here: it reads `paths::controller_runtimes_store()`, which is
/// machine-global by design, and it only writes the unasserted
/// `controller_admission` metadata key.
#[test]
fn accepted_lab_runner_execution_preserves_controller_runtime_pin_across_host_identity_divergence()
{
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cross-host-controller-pin";
    let controller_runtime = json!({
        "schema": "homeboy/controller-runtime-pin/v2",
        "originating": {
            "build_identity": "homeboy 0.300.0+398952a1501d",
            "pinned_executable": "/Users/chubes/.local/share/homeboy/controller-runtimes/macos/homeboy",
            "sha256": "macos-controller-sha256"
        }
    });
    let runner_runtime = json!({
        "schema": "homeboy/controller-runtime-pin/v2",
        "originating": {
            "build_identity": "homeboy 0.300.0+398952a1501d",
            "pinned_executable": "/home/chubes/.local/share/homeboy/controller-runtimes/linux/homeboy",
            "sha256": "linux-runner-sha256"
        }
    });
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
    ];

    let record =
        lifecycle_store
            .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| {
                Ok(controller_runtime.clone())
            })
            .expect("controller submits the durable run");
    record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id,
            runner_id: "linux-lab",
            runner_job_id: "runner-job-9574",
            remote_workspace: "/home/chubes/homeboy",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("runner acceptance is recorded");

    submit_plan_with_runtime_admission_in_store(
        &lifecycle_store,
        &test_plan(),
        Some(run_id),
        Some("linux-lab".to_string()),
        None,
        None,
        |_| Ok(runner_runtime.clone()),
    )
    .expect("runner records its execution identity without replacing the controller pin");
    let mirrored = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("runner-updated controller record")
    .record;
    let preserved = mirrored.metadata
        [homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY]
        .clone();

    assert_eq!(preserved, controller_runtime);
    assert_eq!(
        mirrored.metadata["runner_execution_runtime"],
        runner_runtime
    );
    assert_ne!(
        preserved["originating"]["pinned_executable"],
        runner_runtime["originating"]["pinned_executable"]
    );
    assert_ne!(
        preserved["originating"]["sha256"],
        runner_runtime["originating"]["sha256"]
    );
}

/// Stays on `with_isolated_home` (#7505). Neither
/// `pinned_runtime_for_mutation` nor `runner_pinned_runtime_for_mutation` has a
/// rooted sibling — both resolve the record through the ambient `store::` shim,
/// and the first additionally migrates the pin through the machine-global
/// admission lock. Rooting the test would mean rooting those two entry points
/// first, which is its own slice.
#[test]
fn persisted_runner_v2_pin_is_resolved_through_its_lab_authority_before_local_validation() {
    with_isolated_home(|_| {
        let run_id = "cook-10497-attempt-1";
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "run-plan".to_string(),
        ];
        submit_plan(&test_plan(), Some(run_id)).expect("persist controller-local continuation");
        record_detached_lab_run(DetachedLabRunRecord {
            run_id,
            runner_id: "homeboy-lab",
            runner_job_id: "job-10497",
            // This Lab workspace has been reaped; continuation must use durable
            // recipe and promotion evidence rather than a remote checkout.
            remote_workspace: "/home/chubes/reaped-lab-workspace",
            remote_command: &command,
        })
        .expect("persist Lab transport authority");
        rewrite_record_for_test(run_id, |record| {
            record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY] = json!({
                "schema": "homeboy/controller-runtime-pin/v2",
                "originating": {
                    "build_identity": "homeboy 0.300.0+linux",
                    "pinned_executable": "/home/chubes/.local/share/homeboy/controller-runtimes/linux/homeboy",
                    "sha256": "linux-runner-sha256"
                }
            });
            record.metadata["retained_successful_patch_candidate"] = json!({"sha256": "candidate-10497"});
        })
        .expect("persist historical v2 runner pin");

        assert!(
            pinned_runtime_for_mutation(run_id).is_err(),
            "the historical Linux path is not valid on the controller"
        );
        let pinned = runner_pinned_runtime_for_mutation(run_id)
            .expect("resolve runner-owned v2 runtime")
            .expect("runner transport takes precedence over local validation");
        assert_eq!(pinned.runner_id, "homeboy-lab");
        assert_eq!(
            pinned.executable,
            std::path::PathBuf::from(
                "/home/chubes/.local/share/homeboy/controller-runtimes/linux/homeboy"
            )
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Both pins are supplied by this test's own admission callbacks, so
/// rooting substitutes nothing the assertions depend on — see the accepted-hand
/// off proof above for the same reasoning and the same `None` admission-status
/// projection.
#[test]
fn planned_lab_runner_execution_preserves_controller_runtime_pin_before_acceptance() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "cross-host-controller-pin-planned";
    let controller_runtime = json!({
        "schema": "homeboy/controller-runtime-pin/v2",
        "originating": {
            "build_identity": "homeboy 0.300.0+macos",
            "pinned_executable": "/Users/chubes/.local/share/homeboy/controller-runtimes/macos/homeboy",
            "sha256": "macos-controller-sha256"
        }
    });
    let runner_runtime = json!({
        "schema": "homeboy/controller-runtime-pin/v2",
        "originating": {
            "build_identity": "homeboy 0.300.0+linux",
            "pinned_executable": "/home/chubes/.local/share/homeboy/controller-runtimes/linux/homeboy",
            "sha256": "linux-runner-sha256"
        }
    });
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
    ];

    lifecycle_store
        .submit_plan_with_runtime_admission(
            &test_plan(),
            run_id,
            |_| Ok(controller_runtime.clone()),
        )
        .expect("controller submits the durable run");
    record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id,
            runner_id: "linux-lab",
            remote_workspace: "/home/chubes/homeboy",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("controller records planned handoff");

    submit_plan_with_runtime_admission_in_store(
        &lifecycle_store,
        &test_plan(),
        Some(run_id),
        Some("linux-lab".to_string()),
        None,
        None,
        |_| Ok(runner_runtime.clone()),
    )
    .expect("runner preserves controller-seat runtime before acceptance");

    let record = reconcile_status_in_store(
        &lifecycle_store,
        run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("controller record")
    .record;
    assert_eq!(
        record.metadata[homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY],
        controller_runtime
    );
    assert_eq!(record.metadata["runner_execution_runtime"], runner_runtime);
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). This is the one pin test whose fixture *is* a stub pin, and it
/// stays honest under rooting because the thing it proves is an **absence**:
/// the runner submission is refused because the record carries no
/// controller-runtime key at all, and the rewrite that removes it and the
/// submission that fails on it must therefore name the same record. Whether the
/// pin removed was a real admission or the stub `{}` this store writes makes no
/// difference — `controller_runtime_for_runner_execution` accepts `{}` and
/// fails only on the missing key, so the refusal below is caused by this test's
/// own removal and not by the substitution.
#[test]
fn accepted_lab_runner_execution_rejects_missing_controller_runtime_pin() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "missing-cross-host-controller-pin";
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "run-plan".to_string(),
    ];
    lifecycle_store
        .submit_plan_with_runtime_admission(&test_plan(), run_id, |_| Ok(json!({})))
        .expect("controller submits the durable run");
    record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id,
            runner_id: "linux-lab",
            runner_job_id: "runner-job-9574",
            remote_workspace: "/home/chubes/homeboy",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("runner acceptance is recorded");
    rewrite_record_for_test_in_store(&lifecycle_store, run_id, |record| {
        record
            .metadata
            .as_object_mut()
            .expect("metadata object")
            .remove(homeboy_core::controller_runtime::CONTROLLER_RUNTIME_METADATA_KEY);
    })
    .expect("remove controller runtime pin");

    let error = submit_plan_with_runtime_admission_in_store(
        &lifecycle_store,
        &test_plan(),
        Some(run_id),
        Some("linux-lab".to_string()),
        None,
        None,
        |_| Ok(json!({ "runner": "runtime" })),
    )
    .expect_err("accepted handoff without its controller pin fails closed");

    assert_eq!(error.code, ErrorCode::ValidationInvalidArgument);
    assert!(error.message.contains("no controller runtime pin"));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The daemon identity the cancellation projects onto is read from the
/// same queued proxy it was recorded against, so both writes and the
/// cancellation that joins them name one home.
///
/// The cancellation hook is a `thread_local`, so it needs no serializing home.
#[test]
fn cancelling_queued_runner_proxy_projects_to_accepted_daemon_job() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let run_id = "agent-task-queued-runner-proxy";
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id,
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/homeboy",
            remote_command: &command,
            durable_plan: None,
        },
        &stub_lab_offload_submission,
    )
    .expect("queued controller proxy");
    record_runner_job_identity_in_store(
        &lifecycle_store,
        run_id,
        "homeboy-lab",
        "job-pre-provider",
    )
    .expect("persist accepted daemon job identity");

    let _cancel = super::cancellation::test_cancel_hook::install(Box::new(
        |runner_id, job_id, durable_run_id| {
            assert_eq!(runner_id, "homeboy-lab");
            assert_eq!(job_id, "job-pre-provider");
            assert_eq!(durable_run_id, "agent-task-queued-runner-proxy");
            Ok((
                homeboy_core::api_jobs::Job {
                    id: uuid::Uuid::new_v4(),
                    operation: "runner.exec".to_string(),
                    status: homeboy_core::api_jobs::JobStatus::Cancelled,
                    created_at_ms: 1,
                    updated_at_ms: 2,
                    started_at_ms: None,
                    finished_at_ms: Some(2),
                    event_count: 0,
                    source_snapshot: None,
                    path_materialization_plan: None,
                    stale_reason: None,
                    daemon_lease_id: None,
                    target_runner_id: None,
                    target_project_id: None,
                    claim_id: None,
                    claimed_by_runner_id: None,
                    claimed_at_ms: None,
                    claim_expires_at_ms: None,
                    artifacts: Vec::new(),
                    runner_job_projection: None,
                },
                Vec::new(),
            ))
        },
    ));

    let cancelled = cancel_run_in_store(
        &lifecycle_store,
        run_id,
        Some("controller aggregate unavailable"),
    )
    .expect("cancellation reaches queued daemon job");
    assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
    assert_eq!(
        cancelled.metadata["live_cancellation"]["cancellation"],
        "runner_job_cancel"
    );
}

/// Stays on `with_isolated_home` (#7505). This test *installs* a runner
/// continuation provider. `RunnerContinuationTestGuard` holds no mutex of its
/// own, so the hermetic home's global lock is currently the only thing
/// serializing provider installation across tests; a rooted test running beside
/// this one would observe whichever provider happened to be installed. Nothing
/// about the store injection changes that, so the provider guard is what keeps
/// this test ambient.
#[test]
fn status_expires_an_unaccepted_handoff_but_late_runner_acceptance_wins() {
    with_isolated_home(|_| {
        let _runner = RunnerContinuationTestGuard::install(Box::new(IntentReplayProvider {
            store: JobStore::default(),
            submitted: Arc::new(Mutex::new(Vec::new())),
            lookups: Arc::new(Mutex::new(Vec::new())),
            fail_after_accept_once: Arc::new(Mutex::new(false)),
        }));
        let command = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
        ];
        record_lab_offload_planned(LabOffloadProxyPlan {
            run_id: "expired-handoff-late-acceptance",
            runner_id: "homeboy-lab",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
            durable_plan: None,
        })
        .expect("controller proxy recorded before handoff");
        record_lab_offload_submission_request(
            "expired-handoff-late-acceptance",
            &replay_request("expired-handoff-late-acceptance", &command),
        )
        .expect("persist complete pending submission request");
        rewrite_record_for_test("expired-handoff-late-acceptance", |record| {
            record
                .lab_handoff
                .as_mut()
                .expect("typed handoff")
                .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
        })
        .expect("expire acceptance deadline");

        let expired = reconcile_status("expired-handoff-late-acceptance")
            .expect("status reconciles the expired controller proxy");
        assert_eq!(expired.state, AgentTaskRunState::Cancelled);
        assert_eq!(expired.metadata["handoff_acceptance"]["state"], "expired");
        assert_eq!(expired.metadata["retryable"], true);
        assert_eq!(
            expired.metadata["managed_recovery"]["command"],
            "homeboy agent-task retry expired-handoff-late-acceptance --run"
        );

        let accepted = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "expired-handoff-late-acceptance",
            runner_id: "homeboy-lab",
            runner_job_id: "job-accepted-after-deadline",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("late acceptance supersedes only the synthetic expiry cancellation");
        assert_eq!(accepted.state, AgentTaskRunState::Running);
        assert!(accepted.has_accepted_lab_handoff());
        assert_eq!(accepted.metadata["handoff_acceptance"]["state"], "accepted");
        assert_eq!(
            accepted.metadata["runner_job_id"],
            "job-accepted-after-deadline"
        );
        assert_eq!(
            accepted.metadata["runner_execution_record"]["status"],
            "running"
        );
        let accepted_with_stale_deadline =
            rewrite_record_for_test("expired-handoff-late-acceptance", |record| {
                record
                    .lab_handoff
                    .as_mut()
                    .expect("typed accepted handoff")
                    .acceptance_deadline_at = Some("2000-01-01T00:00:00+00:00".to_string());
            })
            .expect("make the historical acceptance deadline stale");
        assert!(!accepted_with_stale_deadline.has_expired_pending_lab_handoff(chrono::Utc::now()));
    });
}

#[test]
fn non_retryable_pre_execution_failure_remains_invalid_input() {
    let plan = test_plan();
    let outcome = build_pre_execution_failure_outcome(
        "cook-invalid-input",
        &plan.tasks[0],
        "controller_admission",
        &Error::validation_invalid_argument("plan", "invalid input", None, None),
    );

    assert_eq!(
        outcome.failure_classification,
        Some(AgentTaskFailureClassification::InvalidInput)
    );
    assert_eq!(outcome.diagnostics[0].data["retryable"], false);
    assert_eq!(outcome.outputs["retryable"], false);
    assert_eq!(outcome.metadata["retryable"], false);
    assert_eq!(outcome.metadata["provider_executions_consumed"], 0);
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The repair is a write: status rewrites `plan_path` back to the
/// controller-owned file. Deciding that repair from one home and committing it
/// into another is exactly the split #7505 exists to stop, and the durable
/// read-back below would still pass while it happened. The file removed in the
/// second half is the one this store wrote, so the fail-closed diagnostic is
/// about this installation's missing plan.
#[test]
fn status_repairs_runner_plan_projection_and_missing_controller_plan_fails_closed() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let record = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "runner-projected-status", |_| Ok(json!({})))
        .expect("controller plan submitted");
    rewrite_record_for_test_in_store(&lifecycle_store, &record.run_id, |projected| {
        projected.plan_path =
            "/runner/agent-task-runs/runner-projected-status/plan.json".to_string();
    })
    .expect("runner transport path projected");

    assert_eq!(
        reconcile_status_in_store(
            &lifecycle_store,
            &record.run_id,
            AgentTaskStatusOptions::default(),
            false,
        )
        .expect("controller status")
        .record
        .plan_path,
        record.plan_path
    );
    assert_eq!(
        lifecycle_store
            .read_record(&record.run_id)
            .expect("repaired record")
            .plan_path,
        record.plan_path
    );

    std::fs::remove_file(&record.plan_path).expect("remove controller plan");
    let error = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect_err("missing controller plan fails closed");
    assert_eq!(error.code, ErrorCode::InternalIoError);
    let diagnostic = error.details["error"]
        .as_str()
        .expect("structured ownership diagnostic");
    assert!(diagnostic.contains("authoritative controller-owned plan"));
    assert!(diagnostic.contains("runner execution transport"));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Idempotence is the claim, and idempotence is only meaningful within
/// one home: the second `record_lab_offload_planned` has to *find* the record
/// the first one wrote, and the persisted plan read back afterwards has to be
/// the same file. An ambient resume that found nothing would mint a second
/// staging record and the run ids would still compare equal, because the
/// requested id is fixed.
#[test]
fn slow_materialization_remains_discoverable_with_source_identity_and_is_idempotent() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec![
        "homeboy".to_string(),
        "agent-task".to_string(),
        "cook".to_string(),
    ];
    let mut durable_plan = test_plan();
    durable_plan.tasks[0].task_id = "https://github.com/example/project/issues/42".to_string();
    durable_plan.tasks[0].source_refs = vec![AgentTaskSourceRef {
        kind: "task".to_string(),
        uri: "https://github.com/example/project/issues/42".to_string(),
        revision: None,
    }];
    let started = std::time::Instant::now();
    let first = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "slow-materialization",
            runner_id: "homeboy-lab",
            remote_workspace: "pending-materialization",
            remote_command: &command,
            durable_plan: Some(&durable_plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("proxy persisted before staging");

    // Deliberately exceed a caller's short wait budget after the durable
    // write, as a workspace/dependency materializer can in production.
    std::thread::sleep(std::time::Duration::from_millis(20));
    assert!(started.elapsed() > std::time::Duration::from_millis(1));

    let visible = reconcile_status_in_store(
        &lifecycle_store,
        "slow-materialization",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("immediately discoverable")
    .record;
    assert_eq!(visible.run_id, first.run_id);
    assert_eq!(
        visible.tasks[0].task_id,
        "https://github.com/example/project/issues/42"
    );

    let resumed = record_lab_offload_planned_with_submission_in_store(
        &lifecycle_store,
        LabOffloadProxyPlan {
            run_id: "slow-materialization",
            runner_id: "homeboy-lab",
            remote_workspace: "pending-materialization",
            remote_command: &command,
            durable_plan: Some(&durable_plan),
        },
        &stub_lab_offload_submission,
    )
    .expect("resume does not duplicate staging record");
    assert_eq!(resumed.run_id, first.run_id);
    let persisted =
        load_plan_in_store(&lifecycle_store, "slow-materialization").expect("one persisted plan");
    assert_eq!(persisted.tasks.len(), 1);
    assert_eq!(
        persisted.tasks[0].source_refs[0].uri,
        "https://github.com/example/project/issues/42"
    );

    let with_child = record_lab_offload_phase_executions_in_store(
        &lifecycle_store,
        "slow-materialization",
        "hydrating",
        ["runner-job-42".to_string()],
    )
    .expect("child staging job recorded");
    assert_eq!(
        with_child.metadata["materialization_execution_ids"],
        json!(["runner-job-42"])
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The terminal projection's idempotence read decides whether the
/// child aggregate is projected *at all*, and it compares against the aggregate
/// durable in this store; answered ambiently it would either re-project a
/// result already committed here or skip one because a different installation
/// held a matching aggregate. The aggregate read back at the end is the same
/// home the reconciliation committed into.
#[test]
fn disconnected_proxy_projects_terminal_child_aggregate_once_reachable() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("running proxy");
    let mut record = reconcile_status_in_store(
        &lifecycle_store,
        "agent-task-disconnected-child",
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("status")
    .record;
    assert!(is_accepted_runner_handoff(&record));
    assert_eq!(
        record.metadata["runner_job_id"],
        "00000000-0000-0000-0000-000000000123"
    );
    assert!(record.metadata.get("pre_execution_failure").is_none());
    let mut running_snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    running_snapshot.job.status = homeboy_core::api_jobs::JobStatus::Running;
    running_snapshot.events.clear();
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &running_snapshot)
        .expect("remote process heartbeat is retained");
    assert_eq!(record.metadata["phase"], "executing");
    let heartbeat = record
        .lifecycle
        .heartbeat
        .clone()
        .expect("confirmed heartbeat");

    record.annotate_runner_disconnected();
    assert_eq!(record.metadata["runner_liveness"], "disconnected");
    assert_eq!(
        record.metadata["stale_running_reason"],
        "runner_disconnected"
    );
    assert_eq!(record.lifecycle.heartbeat, Some(heartbeat));

    let child_plan = test_plan();
    let mut child_aggregate = succeeded_aggregate(&child_plan);
    child_aggregate.outcomes[0].artifacts = vec![artifact_ref_artifact(
        "patch",
        "patch",
        None,
        Some("/runner/artifacts/patch.diff"),
    )];
    child_aggregate.outcomes[0].diagnostics = vec![AgentTaskDiagnostic {
        class: "provider.attempt".to_string(),
        message: "attempt 1 succeeded".to_string(),
        data: json!({ "attempt": 1 }),
    }];
    let snapshot = terminal_child_snapshot(&child_aggregate);

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("terminal reconciliation");
    let once = record.clone();
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("repeated reconciliation");

    assert_eq!(
        record, once,
        "repeated terminal reconciliation is idempotent"
    );
    assert_eq!(record.state, AgentTaskRunState::Succeeded);
    assert!(is_accepted_runner_handoff(&record));
    assert_eq!(record.artifact_refs[0].uri, "/runner/artifacts/patch.diff");
    assert_eq!(record.metadata["runner_job_status"], "succeeded");
    assert_eq!(record.metadata["runner_liveness"], "reachable");
    let aggregate = lifecycle_store
        .read_aggregate("agent-task-disconnected-child")
        .expect("projected child aggregate");
    assert_eq!(
        aggregate.outcomes[0].diagnostics[0].class,
        "provider.attempt"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). "The aggregate is not there yet" and "the aggregate is there now"
/// are both reads of one durable file, and the continuation status in between
/// decides from the same record. An ambient read of either would answer from an
/// installation this test never wrote to, and the `is_err()` half in particular
/// would pass for the wrong reason.
#[test]
fn terminal_daemon_status_waits_for_delayed_aggregate_then_projects_once() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-delayed-aggregate",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("accepted handoff");
    let mut terminal_without_aggregate =
        terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
    terminal_without_aggregate.events.clear();

    reconcile_runner_job_snapshot_in_store(
        &lifecycle_store,
        &mut record,
        &terminal_without_aggregate,
    )
    .expect("terminal transport awaits aggregate synchronization");
    assert_eq!(record.state, AgentTaskRunState::Running);
    assert!(lifecycle_store.read_aggregate(&record.run_id).is_err());
    let before_delayed_provider_terminal = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("continuation reconciliation observes the pending provider aggregate")
    .record;
    assert_eq!(
        before_delayed_provider_terminal.state,
        AgentTaskRunState::Running,
        "continuation must keep observing until the runner publishes the provider aggregate"
    );

    let aggregate = succeeded_aggregate(&test_plan());
    let mut terminal_with_aggregate = terminal_child_snapshot(&aggregate);
    let identity = terminal_with_aggregate.events[0]
        .data
        .as_mut()
        .and_then(|event| event.get_mut("identity"))
        .and_then(Value::as_object_mut)
        .expect("terminal lifecycle identity");
    identity.insert("persisted_run_id".to_string(), json!(record.run_id));
    identity.insert("run_id".to_string(), json!(record.run_id));
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &terminal_with_aggregate)
        .expect("delayed aggregate terminalizes the handoff");
    let terminal = record.clone();
    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &terminal_with_aggregate)
        .expect("repeated aggregate reconciliation is idempotent");

    assert_eq!(record, terminal);
    assert_eq!(record.state, AgentTaskRunState::Succeeded);
    assert_eq!(
        lifecycle_store
            .read_aggregate(&record.run_id)
            .expect("aggregate persisted"),
        aggregate
    );
    let after_delayed_provider_terminal = reconcile_status_in_store(
        &lifecycle_store,
        &record.run_id,
        AgentTaskStatusOptions::default(),
        false,
    )
    .expect("continuation reconciliation observes the projected terminal aggregate")
    .record;
    assert_eq!(
        after_delayed_provider_terminal.state,
        AgentTaskRunState::Succeeded,
        "the same durable attempt becomes terminal exactly once when delayed provider evidence arrives"
    );
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The reconciliation commits the timeout aggregate against the record
/// this store accepted, so the failed lifecycle state and the succeeded
/// transport state below describe one durable run.
#[test]
fn accepted_handoff_projects_a_remote_timeout_aggregate_even_when_daemon_transport_succeeds() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id: "agent-task-remote-timeout",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("accepted handoff");
    let mut aggregate = succeeded_aggregate(&test_plan());
    aggregate.status = AgentTaskAggregateStatus::Failed;
    aggregate.totals = AgentTaskAggregateTotals {
        timed_out: 1,
        ..Default::default()
    };
    aggregate.outcomes[0].status = AgentTaskOutcomeStatus::Timeout;
    aggregate.events[0].state = AgentTaskState::Failed;

    let mut snapshot = terminal_child_snapshot(&aggregate);
    let identity = snapshot.events[0]
        .data
        .as_mut()
        .and_then(|event| event.get_mut("identity"))
        .and_then(Value::as_object_mut)
        .expect("terminal lifecycle identity");
    identity.insert("persisted_run_id".to_string(), json!(record.run_id));
    identity.insert("run_id".to_string(), json!(record.run_id));

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("remote timeout aggregate reconciles");

    assert_eq!(record.state, AgentTaskRunState::Failed);
    assert_eq!(
        record.totals.as_ref().map(|totals| totals.timed_out),
        Some(1)
    );
    assert_eq!(record.metadata["runner_job_status"], "succeeded");
}

/// Stays on `with_isolated_home` (#7505). `store::interrupt_after_terminal_commit_for_test`
/// arms a process-global `AtomicBool`, exactly like the record-write fault
/// above; the hermetic home's global mutex is what stops a peer test from
/// consuming the injected interruption.
#[test]
fn terminal_projection_is_reader_complete_when_interrupted_after_commit_and_retry_is_idempotent() {
    with_isolated_home(|_| {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let mut record = record_detached_lab_run(DetachedLabRunRecord {
            run_id: "agent-task-disconnected-child",
            runner_id: "homeboy-lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        })
        .expect("running proxy");
        let snapshot = terminal_child_snapshot(&succeeded_aggregate(&test_plan()));
        store::interrupt_after_terminal_commit_for_test();

        reconcile_runner_job_snapshot(&mut record, &snapshot)
            .expect_err("interruption after the committed envelope is surfaced");

        assert_eq!(
            store::read_record("agent-task-disconnected-child")
                .expect("committed controller projection")
                .state,
            AgentTaskRunState::Succeeded
        );
        let (status_record, log, artifacts) = std::thread::scope(|scope| {
            let status_reader = scope.spawn(|| reconcile_status("agent-task-disconnected-child"));
            let log_reader = scope.spawn(|| logs("agent-task-disconnected-child"));
            let artifact_reader = scope.spawn(|| artifacts("agent-task-disconnected-child"));
            (
                status_reader
                    .join()
                    .expect("status reader")
                    .expect("committed status"),
                log_reader
                    .join()
                    .expect("log reader")
                    .expect("committed log"),
                artifact_reader
                    .join()
                    .expect("artifact reader")
                    .expect("committed artifacts"),
            )
        });
        assert_eq!(status_record.state, AgentTaskRunState::Succeeded);
        assert_eq!(log.events[0].data["state"], "succeeded");
        assert!(artifacts.artifacts.is_empty());

        reconcile_runner_job_snapshot(&mut record, &snapshot).expect("idempotent retry");
        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert!(store::aggregate_path(&record.run_id)
            .expect("aggregate path")
            .exists());
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The artifact report is read back for the same run id the
/// reconciliation hydrated, so the transcript evidence is this store's own.
/// Both cases share one store deliberately: they use distinct run ids, exactly
/// as they did under one isolated home.
#[test]
fn terminal_proxy_reconciliation_hydrates_persisted_dispatch_terminal_states() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    for (run_id, aggregate_status, outcome_status, expected_state) in [
        (
            "agent-task-persisted-dispatch-failure",
            AgentTaskAggregateStatus::Failed,
            AgentTaskOutcomeStatus::Failed,
            AgentTaskRunState::Failed,
        ),
        (
            "agent-task-persisted-dispatch-timeout",
            AgentTaskAggregateStatus::Failed,
            AgentTaskOutcomeStatus::Timeout,
            AgentTaskRunState::Failed,
        ),
    ] {
        let mut record = record_detached_lab_run_with_submission_in_store(
            &lifecycle_store,
            DetachedLabRunRecord {
                run_id,
                runner_id: "homeboy-lab",
                runner_job_id: "00000000-0000-0000-0000-000000000123",
                remote_workspace: "/runner/workspace/repo",
                remote_command: &command,
            },
            &stub_lab_offload_submission,
        )
        .expect("running proxy");
        let mut aggregate = succeeded_aggregate(&test_plan());
        aggregate.status = aggregate_status;
        aggregate.outcomes[0].status = outcome_status;
        aggregate.outcomes[0].evidence_refs = vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: format!("homeboy://lab/{run_id}/transcript"),
            label: Some("Provider transcript".to_string()),
        }];

        reconcile_runner_job_snapshot_in_store(
            &lifecycle_store,
            &mut record,
            &persisted_terminal_result_snapshot(&aggregate),
        )
        .expect("hydrate persisted dispatch result");

        let artifact_report =
            artifacts_in_store(&lifecycle_store, run_id).expect("controller artifacts");
        assert_eq!(record.state, expected_state);
        assert!(artifact_report
            .evidence_refs
            .iter()
            .any(|evidence| evidence.kind == "transcript"));
    }
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). Three roots have to agree here, which is exactly why it is worth
/// injecting them: the finalized fixture is seeded under
/// `lifecycle_store.artifact_root()`, the projection the reconciliation writes
/// lands under that same artifacts root, and the observation database the
/// projections are listed from is the one below this store's data root. Seeding
/// the fixture in one home and reading the projection from another is the
/// `#12618` shape — controller-owned bytes registered under one home while a
/// "complete" projection status is written into another.
///
/// The two observation reads use `open_observation_maintained()`, not
/// `open_observation_initialized()`. Only the former is the rooted equivalent
/// of the ambient `ObservationStore::open_initialized()` this test used: the
/// lifecycle opener defers startup artifact maintenance, so swapping it in
/// would quietly change what the two reads below see.
#[test]
fn recovery_preserves_terminal_runner_identity_before_projecting_runner_artifacts() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let command = vec!["homeboy".to_string(), "agent-task".to_string()];
    let run_id = "agent-task-recovered-runner-artifact";
    let mut record = record_detached_lab_run_with_submission_in_store(
        &lifecycle_store,
        DetachedLabRunRecord {
            run_id,
            runner_id: "runner/a:lab",
            runner_job_id: "00000000-0000-0000-0000-000000000123",
            remote_workspace: "/runner/workspace/repo",
            remote_command: &command,
        },
        &stub_lab_offload_submission,
    )
    .expect("detached handoff");
    record.ensure_metadata_object().remove("runner_id");
    record.ensure_metadata_object().remove("runner_job_id");

    let patch = "runner patch";
    let finalized = lifecycle_store
        .artifact_root()
        .join("executor-finalized")
        .join(run_id)
        .join("patch.diff");
    std::fs::create_dir_all(finalized.parent().expect("finalized parent"))
        .expect("create finalized parent");
    std::fs::write(&finalized, patch).expect("write finalized patch");
    let mut aggregate = succeeded_aggregate(&test_plan());
    aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "patch".to_string(),
        kind: "patch".to_string(),
        name: None,
        label: None,
        role: None,
        semantic_key: None,
        path: Some("/home/runner/.homeboy/executor-finalized/patch.diff".to_string()),
        url: Some(
            "homeboy://agent-task/run/detached-run/artifacts#task=task-a&artifact=patch"
                .to_string(),
        ),
        mime: Some("text/x-patch".to_string()),
        size_bytes: Some(patch.len() as u64),
        sha256: Some(format!("{:x}", sha2::Sha256::digest(patch.as_bytes()))),
        metadata: json!({ "executor_artifact_finalized": true }),
    });
    let mut snapshot = terminal_child_snapshot(&aggregate);
    snapshot.events[0].data.as_mut().expect("event data")["identity"]["runner_id"] =
        json!("runner/a:lab");
    snapshot.events[0].data.as_mut().expect("event data")["identity"]["run_id"] = json!(run_id);
    snapshot.events[0].data.as_mut().expect("event data")["identity"]["persisted_run_id"] =
        json!(run_id);
    let event = crate::agent_task_lifecycle::agent_task_lifecycle_event::agent_task_run_plan_lifecycle_event_from_job_events(
        Some(&snapshot.events),
    )
    .expect("terminal lifecycle event");
    assert_eq!(event.identity.runner_id, "runner/a:lab");
    assert_eq!(
        event.identity.runner_job_id,
        "00000000-0000-0000-0000-000000000123"
    );
    assert_eq!(event.identity.run_id.as_deref(), Some(run_id));
    assert_eq!(event.identity.persisted_run_id.as_deref(), Some(run_id));

    reconcile_runner_job_snapshot_in_store(&lifecycle_store, &mut record, &snapshot)
        .expect("terminal recovery");

    assert_eq!(record.metadata["runner_id"], "runner/a:lab");
    assert_eq!(
        record.metadata["runner_job_id"],
        "00000000-0000-0000-0000-000000000123"
    );
    assert_eq!(
        record.metadata["artifact_projection"]["status"], "complete",
        "{:#}",
        record.metadata
    );
    let artifacts = lifecycle_store
        .open_observation_maintained()
        .expect("store")
        .list_artifacts(run_id)
        .expect("artifact projections");
    assert_eq!(artifacts.len(), 1);
    assert_eq!(artifacts[0].artifact_type, "file");
    let projected = verified_controller_artifact_projection_path_in_store(
        &lifecycle_store
            .open_observation_maintained()
            .expect("store"),
        run_id,
        &aggregate.outcomes[0].task_id,
        &aggregate.outcomes[0].artifacts[0],
    );
    assert_eq!(
        projected.expect("verify projection"),
        Some(std::path::PathBuf::from(&artifacts[0].path))
    );
    assert_ne!(
        artifacts[0].path,
        "/home/runner/.homeboy/executor-finalized/patch.diff"
    );
}

#[test]
fn terminal_projection_preserves_persisted_observation_creator_version() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    let store = lifecycle_store
        .open_observation_maintained()
        .expect("observation store");
    for (run_id, creator_version) in [
        ("persisted-observation-version", Some("0.364.13")),
        ("persisted-observation-without-version", None),
    ] {
        let submitted = lifecycle_store
            .submit_plan_with_runtime_admission(&plan, run_id, |_| Ok(json!({})))
            .expect("submit");
        let mut persisted = store
            .get_run(&submitted.run_id)
            .expect("read observation")
            .expect("observation exists");
        persisted.homeboy_version = creator_version.map(str::to_string);
        store
            .upsert_imported_run(&persisted)
            .expect("seed persisted creator version");

        record_run_aggregate_in_store(
            &lifecycle_store,
            &submitted.run_id,
            &plan,
            &succeeded_aggregate(&plan),
        )
        .expect("project terminal observation");

        assert_eq!(
            store
                .get_run(&submitted.run_id)
                .expect("read projected observation")
                .expect("projected observation exists")
                .homeboy_version
                .as_deref(),
            creator_version
        );
    }
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The artifact is imported directly into an observation database and
/// then *reused* by terminal reconciliation, so the import, the reconciliation
/// and the resolve that recovers it must all name one database — a reuse
/// decided against another home's import would mark the projection complete
/// with no bytes behind it, and `artifact_projection.status == "complete"` would
/// still read back correct.
///
/// `open_observation_maintained()` is the rooted equivalent of the ambient
/// `ObservationStore::open_initialized()` this test used; the lifecycle opener
/// defers the startup artifact maintenance that opener performs.
///
/// The two scratch files move from the isolated home's directory to the
/// hermetic context's own root, which is the same kind of place: a temporary
/// directory owned by this test alone.
#[test]
fn terminal_reconciliation_reuses_verified_directly_imported_artifact() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let patch = b"patch bytes";
    let source = context.root().join("imported.patch");
    std::fs::write(&source, patch).expect("write imported patch");
    let plan = test_plan();
    let mut aggregate = succeeded_aggregate(&plan);
    aggregate.outcomes[0].artifacts.push(AgentTaskArtifact {
        schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
        id: "patch".to_string(),
        kind: "patch".to_string(),
        name: None,
        label: None,
        role: Some("patch".to_string()),
        semantic_key: None,
        path: Some("/runner/private/patch.diff".to_string()),
        url: None,
        mime: Some("text/x-patch".to_string()),
        size_bytes: Some(patch.len() as u64),
        sha256: Some(format!("{:x}", sha2::Sha256::digest(patch))),
        metadata: json!({ "executor_artifact_finalized": true }),
    });
    let submitted = lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "direct-import-reconciliation", |_| {
            Ok(json!({}))
        })
        .expect("submit");
    record_runner_job_identity_in_store(
        &lifecycle_store,
        &submitted.run_id,
        "homeboy-lab",
        "job-1",
    )
    .expect("runner identity");

    let mut hash = sha2::Sha256::new();
    sha2::Digest::update(&mut hash, submitted.run_id.as_bytes());
    sha2::Digest::update(&mut hash, [0]);
    sha2::Digest::update(&mut hash, aggregate.outcomes[0].task_id.as_bytes());
    sha2::Digest::update(&mut hash, [0]);
    sha2::Digest::update(&mut hash, b"patch");
    let artifact_id = format!("agent-task-{:x}", hash.finalize());
    let store = lifecycle_store
        .open_observation_maintained()
        .expect("store");
    store
        .import_artifact(&homeboy_core::observation::ArtifactRecord {
            id: artifact_id,
            run_id: submitted.run_id.clone(),
            kind: "patch".to_string(),
            artifact_type: "file".to_string(),
            path: source.display().to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: Some(format!("{:x}", sha2::Sha256::digest(patch))),
            size_bytes: Some(patch.len() as i64),
            mime: Some("text/x-patch".to_string()),
            metadata_json: json!({ "name": "patch" }),
            created_at: chrono::Utc::now().to_rfc3339(),
        })
        .expect("direct artifact import");

    record_run_aggregate_in_store(&lifecycle_store, &submitted.run_id, &plan, &aggregate)
        .expect("terminal reconciliation");
    reconcile_terminal_artifact_projection_in_store(&lifecycle_store, &submitted.run_id)
        .expect("repeated reconciliation");

    let record = lifecycle_store
        .read_record(&submitted.run_id)
        .expect("terminal record");
    assert_eq!(record.metadata["artifact_projection"]["status"], "complete");
    let artifact = homeboy_core::observation::runs_service::resolve_artifact_for_run(
        &store,
        &submitted.run_id,
        "patch",
    )
    .expect("actionable imported patch");
    let output = context.root().join("recovered.patch");
    homeboy_core::observation::runs_service::copy_local_file_artifact(
        artifact,
        Some(output.clone()),
    )
    .expect("recover patch without runner");
    assert_eq!(std::fs::read(output).expect("recovered patch bytes"), patch);
}

/// Stays on `with_isolated_home` (#7505). `mark_running_in_store` is rooted for
/// its lifecycle reads and writes but still calls
/// `migrate_record_controller_runtime_in_store` first, which resolves
/// `runtime_root()` and takes the machine-global admission lock. A run
/// submitted with a stub `{}` admission — the only submission a rooted test can
/// make without reaching that same lock — therefore cannot be marked running at
/// all. Rooting the `mark_running` family needs its pin migration threaded
/// through the injected store first, which is its own slice.
#[test]
fn running_observation_projects_each_terminal_aggregate_state() {
    with_isolated_home(|_| {
        let cases = [
            (
                "terminal-success",
                AgentTaskAggregateStatus::Succeeded,
                AgentTaskOutcomeStatus::Succeeded,
                "succeeded",
            ),
            (
                "terminal-failure",
                AgentTaskAggregateStatus::Failed,
                AgentTaskOutcomeStatus::Failed,
                "failed",
            ),
            (
                "terminal-partial",
                AgentTaskAggregateStatus::PartialFailure,
                AgentTaskOutcomeStatus::Failed,
                "partial_failure",
            ),
            (
                "terminal-cancelled",
                AgentTaskAggregateStatus::Cancelled,
                AgentTaskOutcomeStatus::Cancelled,
                "cancelled",
            ),
        ];
        for (run_id, aggregate_status, outcome_status, terminal_state) in cases {
            let plan = test_plan();
            let mut aggregate = succeeded_aggregate(&plan);
            aggregate.status = aggregate_status;
            aggregate.outcomes[0].status = outcome_status;
            submit_plan(&plan, Some(run_id)).expect("submit");
            mark_running(run_id).expect("running");
            record_run_aggregate(run_id, &plan, &aggregate).expect("terminal aggregate");

            let observation = homeboy_core::observation::ObservationStore::open_initialized()
                .expect("store")
                .get_run(run_id)
                .expect("observation")
                .expect("existing running observation transitioned");
            assert_ne!(observation.status, "running");
            assert_eq!(
                observation.metadata_json["agent_task_terminal_state"],
                terminal_state
            );
        }
    });
}

/// Stays on `with_isolated_home` (#7505). `record_pre_dispatch_failure` has no
/// rooted sibling: it resolves its own store through the ambient `status` and
/// writes through the ambient shims. The assertions also name
/// `paths::homeboy_data()` directly to reach the legacy `status.json` and the
/// aggregate file, which is the ambient data root by definition.
#[test]
fn pre_dispatch_failure_persists_failed_run_without_provider_handle() {
    with_isolated_home(|_| {
        let record = record_pre_dispatch_failure(AgentTaskPreDispatchFailure {
                identity: RunDispatchIdentity {
                    run_id: "cook-lab-predispatch",
                    runner_id: "lab-a",
                },
                local_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--run-id".to_string(),
                    "cook-lab-predispatch".to_string(),
                ],
                remote_command: vec![
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    "cook".to_string(),
                    "--cwd".to_string(),
                    "/runner/workspace/repo".to_string(),
                ],
                remote_workspace: "/runner/workspace/repo",
                failure_message: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout",
                stdout: "",
                stderr: "Invalid argument 'cwd': agent-task runtime dispatch requires --cwd to be a git checkout\n",
                exit_code: 1,
            })
            .expect("pre-dispatch failure recorded");

        let loaded = reconcile_status("cook-lab-predispatch").expect("status loaded");
        let log = logs("cook-lab-predispatch").expect("logs loaded");
        let artifact_report = artifacts("cook-lab-predispatch").expect("artifacts loaded");
        let legacy_status_path = homeboy_core::paths::homeboy_data()
            .expect("homeboy data")
            .join("agent-task-runs")
            .join("cook-lab-predispatch")
            .join("status.json");
        std::fs::remove_file(
            homeboy_core::paths::homeboy_data()
                .expect("homeboy data")
                .join("agent-task-runs")
                .join("cook-lab-predispatch")
                .join("aggregate.json"),
        )
        .expect("aggregate file removed");
        let mirrored_log = logs("cook-lab-predispatch").expect("mirrored logs loaded");
        let mirrored_artifacts =
            artifacts("cook-lab-predispatch").expect("mirrored artifacts loaded");

        assert_eq!(record.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.state, AgentTaskRunState::Failed);
        assert_eq!(loaded.tasks[0].state, AgentTaskState::Failed);
        assert!(loaded.provider_handles.is_empty());
        assert_eq!(log.events[1].data["state"], "failed");
        assert_eq!(mirrored_log.events[1].data["state"], "failed");
        assert_eq!(loaded.metadata["provider_run_ids"], serde_json::json!([]));
        assert_eq!(
            loaded.artifact_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            artifact_report.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert_eq!(
            mirrored_artifacts.evidence_refs[0].kind,
            "lab-offload-pre-dispatch-failure"
        );
        assert!(
            !legacy_status_path.exists(),
            "agent-task status.json is no longer the primary durable run record"
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The blocker this test used to carry was real: `submit_plan_in_store`
/// enqueued its controller admission against the machine-global
/// `paths::controller_runtimes_store()`, so a rooted spelling would have queued
/// against the operator's own home. That admission follows the store's root now
/// (#12862), and no stub is needed to make the submission safe.
#[test]
fn record_completed_run_exposes_logs_and_artifacts() {
    let test_context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(test_context.path_roots());
    {
        let plan = test_plan();
        let aggregate = AgentTaskAggregate {
            schema: AGENT_TASK_AGGREGATE_SCHEMA.to_string(),
            plan_id: plan.plan_id.clone(),
            status: AgentTaskAggregateStatus::Succeeded,
            totals: AgentTaskAggregateTotals {
                queued: 1,
                succeeded: 1,
                ..AgentTaskAggregateTotals::default()
            },
            outcomes: vec![AgentTaskOutcome {
                task_id: "task-a".to_string(),
                status: crate::agent_task::AgentTaskOutcomeStatus::Succeeded,
                summary: Some("ok".to_string()),
                artifacts: vec![AgentTaskArtifact {
                    schema: crate::agent_task::AGENT_TASK_ARTIFACT_SCHEMA.to_string(),
                    id: "patch".to_string(),
                    kind: "patch".to_string(),
                    name: Some("patch.diff".to_string()),
                    label: None,
                    role: None,
                    semantic_key: None,
                    path: Some("/tmp/patch.diff".to_string()),
                    url: None,
                    mime: None,
                    size_bytes: None,
                    sha256: None,
                    metadata: Value::Null,
                }],
                evidence_refs: vec![AgentTaskEvidenceRef {
                    kind: "transcript".to_string(),
                    uri: "file:///tmp/transcript.json".to_string(),
                    label: Some("provider transcript".to_string()),
                }],
                ..Default::default()
            }],
            events: vec![AgentTaskProgressEvent {
                task_id: "task-a".to_string(),
                state: AgentTaskState::Succeeded,
                attempt: 1,
                message: Some("ok".to_string()),
            }],
            artifact_lineage: Vec::new(),
            child_runs: Vec::new(),
            artifact_bindings: Vec::new(),
            queue: Default::default(),
        };

        let record = record_completed_run_in_store(
            &lifecycle_store,
            &plan,
            &aggregate,
            Some("run-complete"),
        )
        .expect("recorded");
        let log = logs_in_store(&lifecycle_store, &record.run_id).expect("logs");
        let artifacts = artifacts_in_store(&lifecycle_store, &record.run_id).expect("artifacts");

        assert_eq!(record.state, AgentTaskRunState::Succeeded);
        assert_eq!(log.events[0].data["state"], "succeeded");
        assert_eq!(artifacts.artifacts[0].id, "patch");
        assert_eq!(artifacts.evidence_refs[0].kind, "transcript");
    }
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The whole submit -> run -> complete -> read cycle names one home,
/// including the controller admission `submit_plan_in_store` takes: that queue
/// and its lock follow the store's root now (#12859) instead of the machine.
#[test]
fn submitted_run_can_be_loaded_marked_running_and_completed() {
    let test_context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(test_context.path_roots());
    {
        let plan = test_plan();
        submit_plan_in_store(&lifecycle_store, &plan, Some("run-execute")).expect("submitted");

        let loaded_plan = load_plan_in_store(&lifecycle_store, "run-execute").expect("plan loaded");
        let running =
            mark_running_in_store(&lifecycle_store, "run-execute").expect("marked running");
        let aggregate = succeeded_aggregate(&loaded_plan);

        let completed = record_run_aggregate_in_store(
            &lifecycle_store,
            "run-execute",
            &loaded_plan,
            &aggregate,
        )
        .expect("completed");
        let durable_status = reconcile_status_in_store(
            &lifecycle_store,
            "run-execute",
            AgentTaskStatusOptions::default(),
            false,
        )
        .expect("status")
        .record;

        assert_eq!(loaded_plan.plan_id, "plan-a");
        assert_eq!(running.state, AgentTaskRunState::Running);
        assert_eq!(running.tasks[0].state, AgentTaskState::Running);
        assert_eq!(
            running.lifecycle.execution.state,
            RunExecutionState::Running
        );
        assert!(running.lifecycle.heartbeat.is_some());
        assert_eq!(completed.state, AgentTaskRunState::Succeeded);
        assert_eq!(completed.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(
            completed.lifecycle.execution.state,
            RunExecutionState::Succeeded
        );
        assert_eq!(completed.totals, Some(aggregate.totals.clone()));
        assert_eq!(durable_status.state, AgentTaskRunState::Succeeded);
        assert_eq!(durable_status.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(durable_status.totals, Some(aggregate.totals.clone()));
        assert!(completed.aggregate_path.is_some());
    }
}

/// Stays on `with_isolated_home` (#7505) — `record_completed_run`; see
/// `record_completed_run_exposes_logs_and_artifacts`.
#[test]
fn completed_run_persists_opaque_provider_handles_from_outcome_metadata() {
    with_isolated_home(|_| {
        let plan = test_plan();
        let mut aggregate = succeeded_aggregate(&plan);
        aggregate.outcomes[0].metadata = json!({
            "provider_handle": AgentTaskExecutionHandle {
                kind: AgentTaskExecutionHandleKind::ProviderRun,
                task_id: "task-a".to_string(),
                backend: "sample-runtime".to_string(),
                run_id: "provider-run-123".to_string(),
                stream_uri: Some("provider://runs/provider-run-123/events".to_string()),
                metadata: json!({ "opaque": { "provider_owned": true } }),
            }
        });

        let record =
            record_completed_run(&plan, &aggregate, Some("run-provider-handle")).expect("recorded");

        assert_eq!(record.provider_handles.len(), 1);
        assert_eq!(record.provider_handles[0].task_id, "task-a");
        assert_eq!(record.provider_handles[0].backend, "sample-runtime");
        assert_eq!(
            record.provider_handles[0].provider_run_id,
            "provider-run-123"
        );
        assert_eq!(
            record.provider_handles[0].stream_uri.as_deref(),
            Some("provider://runs/provider-run-123/events")
        );
        assert_eq!(
            record.provider_handles[0].state,
            Some(AgentTaskState::Succeeded)
        );
        assert_eq!(
            record.provider_handles[0].metadata["opaque"]["provider_owned"],
            json!(true)
        );
        assert_eq!(
            record.metadata["provider_run_ids"],
            json!(["provider-run-123"])
        );
        assert_eq!(
            record.lifecycle.provider_runtime[0].state,
            ProviderRuntimeState::Succeeded
        );
        assert_eq!(
            record.lifecycle.external_runtime_ids[0].value,
            "provider-run-123"
        );
        assert_eq!(
            record.lifecycle.artifact_retention.status,
            ArtifactRetentionStatus::NotApplicable
        );
    });
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The aggregate this plants by hand and the record `status` rebuilds
/// from it are the same run in one home. `mark_running` used to pin this to an
/// ambient home through its pin migration's machine-global admission lock; that
/// lock follows the store's root now (#12852, #12859).
#[test]
fn status_recovers_terminal_state_from_durable_aggregate() {
    let test_context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(test_context.path_roots());
    {
        let plan = test_plan();
        submit_plan_in_store(&lifecycle_store, &plan, Some("run-stale-status")).expect("submitted");
        mark_running_in_store(&lifecycle_store, "run-stale-status").expect("marked running");
        let aggregate = succeeded_aggregate(&plan);
        store::write_aggregate_in_store(&lifecycle_store, "run-stale-status", &aggregate)
            .expect("aggregate written");

        let recovered = reconcile_status_in_store(
            &lifecycle_store,
            "run-stale-status",
            AgentTaskStatusOptions::default(),
            false,
        )
        .expect("status recovered")
        .record;
        let persisted = lifecycle_store
            .read_record("run-stale-status")
            .expect("record persisted");

        assert_eq!(recovered.state, AgentTaskRunState::Succeeded);
        assert_eq!(recovered.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(recovered.totals, Some(aggregate.totals.clone()));
        assert_eq!(persisted.state, AgentTaskRunState::Succeeded);
        assert_eq!(persisted.tasks[0].state, AgentTaskState::Succeeded);
        assert_eq!(persisted.totals, Some(aggregate.totals.clone()));
    }
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). `mark_running` used to pin this test to an ambient home: its pin
/// migration took the admission lock under `runtime_root()`, which is
/// machine-global, so a rooted store was not enough to isolate it. The lock now
/// lives under the root the store was built from, and the submission, the first
/// running projection and the rejection of the second all name one home.
#[test]
fn mark_running_rejects_live_running_record() {
    let test_context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store = AgentTaskLifecycleStore::new(test_context.path_roots());
    let plan = test_plan();
    submit_plan_in_store(&lifecycle_store, &plan, Some("run-live-owner")).expect("submitted");
    mark_running_in_store(&lifecycle_store, "run-live-owner").expect("marked running");

    let error =
        mark_running_in_store(&lifecycle_store, "run-live-owner").expect_err("live run rejected");

    assert!(error.message.contains("already running"));
}

/// Rooted in an explicit store rather than a mutated process environment
/// (#7505). The runner-backed record this test plants by hand is the record the
/// cancellation must read to decide it cannot signal locally, so the write and
/// the cancellation have to name one home.
#[test]
fn cancel_run_emits_recovery_commands_for_runner_backed_run() {
    let context = homeboy_core::test_support::HermeticTestContext::new();
    let lifecycle_store =
        crate::agent_task_lifecycle::AgentTaskLifecycleStore::new(context.path_roots());
    let plan = test_plan();
    lifecycle_store
        .submit_plan_with_runtime_admission(&plan, "run-cancel-runner", |_| Ok(json!({})))
        .expect("submitted");
    let mut record = lifecycle_store
        .read_record("run-cancel-runner")
        .expect("record");
    record.state = AgentTaskRunState::Running;
    record.tasks[0].state = AgentTaskState::Running;
    // Runner-backed: owner pid lives on the runner host (not running
    // here), so live cancellation must hand back recovery commands.
    record.metadata = json!({
        "runner_pid": u32::MAX,
        "runner_id": "lab-a",
        "runner_job_id": "job-123",
    });
    lifecycle_store
        .write_record(&record)
        .expect("stored runner record");

    let cancelled = cancel_run_in_store(&lifecycle_store, "run-cancel-runner", None)
        .expect("runner run cancelled");

    assert_eq!(cancelled.state, AgentTaskRunState::Cancelled);
    assert_eq!(cancelled.tasks[0].state, AgentTaskState::Cancelled);
    let unsupported = &cancelled.metadata["live_cancellation_unsupported"];
    assert!(unsupported.is_object());
    assert_eq!(unsupported["runner_id"], json!("lab-a"));
    assert_eq!(unsupported["runner_job_id"], json!("job-123"));
    let commands = unsupported["recovery_commands"]
        .as_array()
        .expect("recovery commands array");
    assert!(!commands.is_empty());
    // The first recovery command should route cancellation to the
    // owning runner so the operator can act deterministically.
    assert!(commands[0]
        .as_str()
        .expect("command string")
        .contains("homeboy runner exec lab-a"));
    // No real local process was signalled.
    assert!(cancelled.metadata.get("live_cancellation").is_none());
}

/// Stays on `with_isolated_home` because record-health reconciliation still
/// resolves its store from the ambient test home. Fault injection is scoped to
/// this thread, independently of that legacy path (#11897).
#[test]
fn record_health_recovers_after_interrupted_migration_without_changing_terminal_status() {
    with_isolated_home(|_| {
        let plan = test_plan();
        submit_plan(&plan, Some("interrupted-terminal")).expect("submitted");
        let store = homeboy_core::observation::ObservationStore::open_initialized().expect("store");
        let mut observation = store
            .get_run("interrupted-terminal")
            .expect("read")
            .expect("observation");
        observation.status = "pass".to_string();
        observation.finished_at = Some("2026-01-01T00:01:00Z".to_string());
        observation.metadata_json = json!({});
        store
            .upsert_imported_run(&observation)
            .expect("terminal malformed fixture");

        store::fail_next_record_write_for_test();
        assert!(reconcile_record_health_in_store(&test_lifecycle_store(), false).is_err());
        assert_eq!(
            record_health_summary_in_store(&test_lifecycle_store(),)
                .expect("still malformed")
                .malformed,
            1
        );
        let applied = reconcile_record_health_in_store(&test_lifecycle_store(), false)
            .expect("retry migration");
        assert_eq!(applied.migrated, 1);
        let repaired = reconcile_status("interrupted-terminal").expect("repaired");
        assert_eq!(repaired.state, AgentTaskRunState::Succeeded);
        assert_eq!(
            repaired.lifecycle.execution.finished_at.as_deref(),
            Some("2026-01-01T00:01:00Z")
        );
    });
}

#[test]
fn artifact_refs_dedup_identical_refs_across_artifacts_and_evidence() {
    let outcomes = vec![outcome_with_refs(
        "task-a",
        vec![artifact_ref_artifact(
            "transcript",
            "transcript",
            Some("file:///tmp/transcript.json"),
            None,
        )],
        vec![AgentTaskEvidenceRef {
            kind: "transcript".to_string(),
            uri: "file:///tmp/transcript.json".to_string(),
            label: Some("transcript artifact".to_string()),
        }],
    )];

    let refs = artifact_refs_for_outcomes(&outcomes);

    assert_eq!(
        refs.len(),
        1,
        "exact-duplicate refs collapse to a single entry"
    );
}
