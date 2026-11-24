//! Host-level admission of a local Cook provider attempt.
//!
//! Every local provider attempt, whatever started it (a fresh Cook, a
//! `run_next` retry, a continuation, a recovery step), passes through
//! [`admit_local_provider_attempt`] immediately before the provider runs. The
//! returned lease holds a slot in the host-wide local dispatch semaphore
//! ([`homeboy_core::local_dispatch_admission`]) until the attempt ends. Lab
//! attempts never reach this: they are placed by Lab admission.

use super::cook::*;
use homeboy_core::local_dispatch_admission::{
    admit_local_dispatch, local_dispatch_wait_evidence, sample_local_dispatch_pressure,
    ActiveLocalDispatchLease, LocalDispatchPressure, LOCAL_DISPATCH_QUEUE_POLL_INTERVAL,
};

/// Phase a Cook reports while its local dispatch waits for host pressure. The
/// phase keeps its first `phase_started_at`, so `elapsed_ms` is the whole wait.
pub const LOCAL_DISPATCH_QUEUED_PHASE: &str = "queued_local_pressure";

/// Admit `run_id`'s local provider attempt, waiting while the host is hot.
/// Each refusal is durable Cook progress carrying the wait evidence, and is
/// delivered to the observer like any other phase.
///
/// Returns `None` when host pressure cannot be sampled, so admission fails
/// open rather than guessing at evidence it does not have.
pub(super) fn admit_local_provider_attempt(
    lifecycle_store: &AgentTaskLifecycleStore,
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    attempt: u32,
) -> Result<Option<ActiveLocalDispatchLease>> {
    if sample_local_dispatch_pressure().is_none() {
        return Ok(None);
    }
    admit_local_provider_attempt_with(
        lifecycle_store,
        observer,
        cook_id,
        run_id,
        attempt,
        // A reading lost mid-wait fails open like a host that never had one.
        || {
            Ok(sample_local_dispatch_pressure()
                .unwrap_or_else(|| LocalDispatchPressure::from_load(None, 1)))
        },
        std::thread::sleep,
    )
    .map(Some)
}

pub(super) fn admit_local_provider_attempt_with(
    lifecycle_store: &AgentTaskLifecycleStore,
    observer: Option<&CookProgressObserver<'_>>,
    cook_id: &str,
    run_id: &str,
    attempt: u32,
    pressure: impl FnMut() -> Result<LocalDispatchPressure>,
    sleep: impl FnMut(Duration),
) -> Result<ActiveLocalDispatchLease> {
    let data_root = lifecycle_store.data_root();
    admit_local_dispatch(
        &data_root,
        run_id,
        pressure,
        LOCAL_DISPATCH_QUEUE_POLL_INTERVAL,
        sleep,
        |reason, reading| {
            let evidence = local_dispatch_wait_evidence(
                &data_root,
                reading,
                LOCAL_DISPATCH_QUEUE_POLL_INTERVAL,
            );
            if let Err(error) = report_cook_progress_with_evidence(
                lifecycle_store,
                observer,
                cook_id,
                run_id,
                LOCAL_DISPATCH_QUEUED_PHASE,
                attempt,
                Some(reason),
                evidence,
            ) {
                homeboy_core::log_status!("cook", "local dispatch wait not recorded: {}", error);
            }
        },
    )
    .map_err(|error| with_pre_execution_phase(error, "local_dispatch_admission"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::local_dispatch_admission::{
        acquire_local_dispatch_lease, active_local_dispatch_count,
    };

    fn reading(severity: &'static str, load_one: f64) -> LocalDispatchPressure {
        LocalDispatchPressure {
            severity,
            load_one: Some(load_one),
            load_five: Some(load_one),
            cpu_count: 32,
        }
    }

    /// A pressure wait is durable Cook progress whose start survives every
    /// poll, and the observer sees each refusal.
    #[test]
    fn local_dispatch_wait_is_durable_cook_progress() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let store = AgentTaskLifecycleStore::from_environment().unwrap();
            let plan = AgentTaskPlan::new("plan", vec![]);
            crate::agent_task_lifecycle::submit_plan(&plan, Some("cook-hot")).unwrap();
            let _holder = acquire_local_dispatch_lease(&store.data_root(), "holder").unwrap();
            let observed = std::sync::Mutex::new(Vec::new());
            let observer = |event: &CookProgressEvent<'_>| {
                observed.lock().unwrap().push((
                    event.phase.to_string(),
                    event.detail.map(str::to_string),
                    event.wait_owner.clone(),
                ));
                Ok(())
            };
            let readings = std::cell::RefCell::new(
                vec![
                    reading("ok", 1.0),
                    reading("hot", 33.1),
                    reading("hot", 40.4),
                ]
                .into_iter(),
            );
            let mut first_started_at = None;

            let lease = admit_local_provider_attempt_with(
                &store,
                Some(&observer),
                "cook",
                "cook-hot",
                2,
                || Ok(readings.borrow_mut().next_back().unwrap()),
                |_| {
                    let progress =
                        store.read_record("cook-hot").unwrap().metadata["cook_progress"].clone();
                    first_started_at.get_or_insert(progress["phase_started_at"].clone());
                    assert_eq!(
                        progress["phase_started_at"],
                        first_started_at.clone().unwrap()
                    );
                    std::thread::sleep(Duration::from_millis(20));
                },
            )
            .expect("admitted once pressure drops");

            let progress = store.read_record("cook-hot").unwrap().metadata["cook_progress"].clone();
            assert_eq!(progress["phase"], LOCAL_DISPATCH_QUEUED_PHASE);
            assert_eq!(progress["wait_owner"], "cook_queue");
            assert!(progress["detail"]
                .as_str()
                .unwrap()
                .contains("load average 33.1 across 32 CPU(s)"));
            assert!(progress["elapsed_ms"].as_u64().unwrap() >= 20);
            let wait = &progress["activity"]["local_dispatch_wait"];
            assert_eq!(wait["pressure"]["recommendation"], "hot");
            assert_eq!(wait["pressure_holders"][0]["run_id"], "holder");
            assert!(wait["next_attempt_at"].is_string());
            let observed = observed.into_inner().unwrap();
            assert_eq!(observed.len(), 2);
            assert!(observed.iter().all(|(phase, detail, owner)| {
                phase == LOCAL_DISPATCH_QUEUED_PHASE
                    && detail
                        .as_deref()
                        .is_some_and(|d| d.contains("machine is hot"))
                    && owner == "cook_queue"
            }));
            assert_eq!(active_local_dispatch_count(&store.data_root()).unwrap(), 2);
            drop(lease);
            assert_eq!(active_local_dispatch_count(&store.data_root()).unwrap(), 1);
        });
    }
}
