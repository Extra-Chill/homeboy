//! Supervision of one claimed controller job: the thread that prepares,
//! executes or resumes it through its driver, honors durable cancellation at
//! every gate, and persists exactly one terminal outcome.

use super::controller_job_driver::{
    self, ControllerJobDriver, ControllerJobExecutionOwner, ControllerJobHandle,
};
use super::*;
use crate::api_jobs::JobHandle;
use serde_json::Value;

/// Run work which has already been claimed by either explicit start or startup
/// recovery. The durable claim is the exactly-once execution authority.
pub(super) fn dispatch_claimed_controller_job(
    job_store: JobStore,
    job_id: Uuid,
    state: ControllerJobState,
    recovery: bool,
) {
    let Ok(driver) = controller_job_driver::driver(&state.job_type, state.version) else {
        let _ = job_store.fail_controller_error(
            job_id,
            "controller recovery driver is unavailable".to_string(),
            json!({ "phase": "recovery_driver_missing" }),
        );
        return;
    };
    if (recovery || state.cancellation_requested)
        && matches!(
            driver.execution_owner(&state.request, state.checkpoint.as_ref()),
            Err(_) | Ok(Some(ControllerJobExecutionOwner::Unavailable))
        )
    {
        // Recovery/cancellation must preserve unknown external ownership.
        // A fresh admitted start establishes its checkpoint through prepare.
        return;
    }
    // Cancellation is durable first. This channel only wakes the owner quickly;
    // every gate re-reads the durable flag before doing work.
    let (cancel_tx, cancel_rx) = mpsc::channel();
    let runtime_key = controller_job_runtime_key(&job_store, job_id);
    // Register ownership before allowing a fast driver to terminalize itself.
    let (start_tx, start_rx) = mpsc::channel();
    let registration = RuntimeRegistration(runtime_key.clone());
    // Detached: nothing joins this handle, so it is not retained. Drivers run
    // registered agent-task hooks (a workflow resolves a Cook's canonical run
    // projection) whose recursion overflows the default spawned-thread stack;
    // the 32 MiB working stack matches the orchestration tick (homeboy#15010).
    std::thread::Builder::new()
        .name(format!("controller-job-{job_id}"))
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            let _ = start_rx.recv();
            let _registration = registration;
            supervise(job_store.handle(job_id), state, recovery, driver, cancel_rx);
        })
        .expect("spawn controller job driver");
    controller_job_runtimes()
        .lock()
        .expect("controller runtimes lock")
        .insert(runtime_key, ControllerJobRuntime { cancel: cancel_tx });
    let _ = start_tx.send(());
}

/// This daemon's ownership of a running controller job. The supervisor holds
/// it for its whole life, so every way it ends releases the runtime entry.
struct RuntimeRegistration(ControllerJobRuntimeKey);

impl Drop for RuntimeRegistration {
    fn drop(&mut self) {
        controller_job_runtimes()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.0);
    }
}

fn supervise(
    job: JobHandle,
    state: ControllerJobState,
    recovery: bool,
    driver: Arc<dyn ControllerJobDriver>,
    cancel_rx: mpsc::Receiver<()>,
) {
    let timing_store = crate::observation::ObservationStore::open_initialized()
        .map_err(|error| {
            crate::log_status!("observe", "phase timing persistence failed: {}", error);
        })
        .ok();
    let timing_run_id = driver.linked_durable_run_id(&state.request);
    let _execution = timing_store
        .as_ref()
        .zip(timing_run_id.as_deref())
        .map(|(store, run_id)| store.run_phase_guard(run_id, "controller.job"));
    if job.is_cancelled() {
        cancel_before_prepare(&job, &state, recovery, driver.as_ref());
        return;
    }
    let prepared = if recovery {
        state
            .checkpoint
            .clone()
            .expect("claimed recovery has checkpoint")
    } else {
        let Some(prepared) = prepare(&job, &state, driver.as_ref()) else {
            return;
        };
        prepared
    };
    // The checkpoint write and this durable read form the final gate before
    // any execute/resume side effect can start.
    if job.is_cancelled() {
        if driver_cancelled(&job, driver.as_ref(), &prepared) {
            persist_controller_cancellation(&job);
        }
        return;
    }
    run(&job, recovery, driver, prepared, cancel_rx);
}

/// Cancellation that lands before the job is prepared stops whatever work its
/// checkpoint already names.
fn cancel_before_prepare(
    job: &JobHandle,
    state: &ControllerJobState,
    recovery: bool,
    driver: &dyn ControllerJobDriver,
) {
    let checkpoint = if recovery {
        state.checkpoint.clone()
    } else {
        driver.recovery_checkpoint(&state.request).ok().flatten()
    };
    if let Some(checkpoint) = checkpoint {
        if driver_cancelled(job, driver, &checkpoint) {
            persist_controller_cancellation(job);
        }
    } else if matches!(
        driver.execution_owner(&state.request, None),
        Ok(None) | Ok(Some(ControllerJobExecutionOwner::None))
    ) {
        // A claimless in-process driver can prove it admitted no external
        // work. Missing ownership evidence must remain active/ambiguous.
        persist_controller_cancellation(job);
    }
}

/// Prepare a fresh start and persist its checkpoint, so recovery can resume
/// or cancel it. `None` when the job already ended as failed.
fn prepare(
    job: &JobHandle,
    state: &ControllerJobState,
    driver: &dyn ControllerJobDriver,
) -> Option<Value> {
    let prepared = match driver.prepare(state.request.clone()) {
        Ok(prepared) => prepared,
        Err(error) => {
            persist_driver_failure(job, driver, &error);
            return None;
        }
    };
    if job.record_controller_prepared(prepared.clone()).is_err() {
        persist_controller_failure(
            job,
            "controller checkpoint could not be persisted".to_string(),
            json!({ "phase": "checkpoint_persistence" }),
        );
        return None;
    }
    Some(prepared)
}

/// Execute or resume the prepared job on its own thread and persist its
/// outcome, stopping it through the driver when cancellation arrives.
fn run(
    job: &JobHandle,
    recovery: bool,
    driver: Arc<dyn ControllerJobDriver>,
    prepared: Value,
    cancel_rx: mpsc::Receiver<()>,
) {
    let execute_driver = Arc::clone(&driver);
    let driver_job = ControllerJobHandle::new(job.clone(), Arc::clone(&driver));
    let recovery_handle = driver_job.clone();
    let execute_prepared = prepared.clone();
    let (sender, receiver) = mpsc::sync_channel(1);
    let execute = std::thread::spawn(crate::observation::phase_timing::inherit(move || {
        let result = if recovery {
            execute_driver.resume(execute_prepared, driver_job)
        } else {
            execute_driver.execute(execute_prepared, driver_job)
        };
        let _ = sender.send(result);
    }));
    let mut cancellation_failed = false;
    loop {
        if !cancellation_failed && (job.is_cancelled() || cancel_rx.try_recv().is_ok()) {
            if driver_cancelled(job, driver.as_ref(), &prepared) {
                // A successful driver cancellation is not proof of stop.
                // Join the owned execute thread before terminalizing.
                let _ = execute.join();
                persist_controller_cancellation(job);
                return;
            }
            // The driver could not confirm stop. Keep this supervisor and
            // execute handle live until completion proves it ended; the
            // durable Failed job is explicit uncertainty, never a false
            // Cancelled terminal.
            cancellation_failed = true;
            continue;
        }
        match receiver.recv_timeout(Duration::from_millis(10)) {
            Ok(Ok(_)) if recovery_handle.recovery_yielded() => {
                // The driver stopped and persisted its checkpoint. Leave
                // the job nonterminal for the next lease's recovery.
                let _ = execute.join();
            }
            Ok(Ok(private)) => match driver.public_result(&private) {
                Ok(public) => {
                    // The driver has already produced this result. Retry only
                    // this projection, never the driver side effect.
                    if job.is_cancelled() {
                        // The driver has returned: no child can still be
                        // spawning. Cancellation must win over its receipt.
                        if driver_cancelled(job, driver.as_ref(), &prepared) {
                            persist_controller_cancellation(job);
                        }
                        let _ = execute.join();
                        break;
                    }
                    if !persist_controller_success_or_uncertainty(job, public) {
                        continue;
                    }
                    let _ = execute.join();
                }
                Err(error) => {
                    let _ = execute.join();
                    persist_driver_failure(job, driver.as_ref(), &error);
                }
            },
            Ok(Err(error)) => {
                let _ = execute.join();
                persist_driver_failure(job, driver.as_ref(), &error);
            }
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                let _ = execute.join();
                persist_controller_failure(
                    job,
                    "controller driver execution thread disconnected".to_string(),
                    json!({ "phase": "driver_disconnected" }),
                );
            }
        }
        return;
    }
}

/// Ask the driver to stop the work `checkpoint` names. A refusal is recorded
/// as a failed cancellation; `true` means the driver confirmed the stop and
/// the caller records the cancellation once the work has ended.
fn driver_cancelled(job: &JobHandle, driver: &dyn ControllerJobDriver, checkpoint: &Value) -> bool {
    match driver.cancel(checkpoint) {
        Ok(()) => true,
        Err(error) => {
            let public = driver.public_error(&error);
            persist_controller_cancellation_failure(job, public.message, public.data);
            false
        }
    }
}

fn persist_driver_failure(job: &JobHandle, driver: &dyn ControllerJobDriver, error: &Error) {
    let public = driver.public_error(error);
    persist_controller_failure(job, public.message, public.data);
}

/// Release the worker only after a terminal winner or write-ahead completion
/// owns its result. Without either durable proof, an unavailable index parks
/// this one owner with capped backoff rather than amplifying whole-store writes.
pub(super) fn persist_controller_completion(
    job: &crate::api_jobs::JobHandle,
    cancellation_interrupts: bool,
    mut persist: impl FnMut() -> Result<crate::api_jobs::Job>,
) -> bool {
    let mut failures = 0_u32;
    loop {
        if job.is_terminal() {
            return true;
        }
        if cancellation_interrupts && job.is_cancelled() {
            return false;
        }
        match persist() {
            Ok(_) => return true,
            Err(_) if job.is_terminal() => return true,
            Err(_) if cancellation_interrupts && job.is_cancelled() => return false,
            Err(_) if job.completion_pending() => return true,
            Err(error)
                if !matches!(
                    error.code,
                    crate::error::ErrorCode::InternalIoError
                        | crate::error::ErrorCode::ObservationStoreBusy
                ) =>
            {
                // Ownership and transition rejection is permanent, not an I/O
                // outage. Never retry it, or rerun the completed driver.
                eprintln!(
                    "controller job {} terminal persistence rejected: {}",
                    job.job_id(),
                    error.code.as_str()
                );
                return true;
            }
            Err(_) => {}
        }
        let delay = match failures {
            0 => Duration::from_millis(100),
            1 => Duration::from_millis(200),
            _ => Duration::from_secs(
                1_u64
                    .checked_shl((failures - 2).min(5))
                    .unwrap_or(30)
                    .min(30),
            ),
        };
        failures = failures.saturating_add(1);
        std::thread::sleep(delay);
    }
}

/// Retry only result persistence. The driver side effect is already finished.
pub(super) fn persist_controller_success_or_uncertainty(
    job: &crate::api_jobs::JobHandle,
    result: serde_json::Value,
) -> bool {
    persist_controller_completion(job, true, || {
        job.complete_controller_success(result.clone())
    })
}

pub(super) fn persist_controller_cancellation(job: &crate::api_jobs::JobHandle) {
    persist_controller_completion(job, false, || job.complete_controller_cancellation());
}

pub(super) fn persist_controller_cancellation_failure(
    job: &crate::api_jobs::JobHandle,
    message: String,
    data: serde_json::Value,
) {
    persist_controller_completion(job, false, || {
        job.fail_controller_cancellation(message.clone(), data.clone())
    });
}

pub(super) fn persist_controller_failure(
    job: &crate::api_jobs::JobHandle,
    message: String,
    data: serde_json::Value,
) {
    persist_controller_completion(job, false, || {
        job.fail_controller_error(message.clone(), data.clone())
    });
}
