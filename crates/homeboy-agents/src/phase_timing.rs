//! Run-bound timing context for synchronous controller work and probe workers.
use std::{cell::RefCell, path::PathBuf};

use homeboy_core::{observation::ObservationStore, Result};

pub(crate) type Context = (PathBuf, String);

thread_local! {
    static CURRENT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

pub(crate) fn current() -> Option<Context> {
    CURRENT.with(|slot| slot.borrow().clone())
}

pub(crate) struct Scope(Option<Context>);

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

pub(crate) fn enter(context: Option<Context>) -> Scope {
    Scope(CURRENT.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), context)))
}

pub(crate) fn for_run(store: &ObservationStore, run_id: &str) -> Scope {
    enter(Some((
        store.database_path().to_path_buf(),
        run_id.to_string(),
    )))
}

pub(crate) fn for_execution(
    execution: &crate::agent_tasks::AgentTaskExecutionContext,
) -> Option<Scope> {
    let run_id = execution.run_id.as_deref()?;
    let store = execution
        .lifecycle_store()?
        .open_observation_initialized()
        .map_err(|error| {
            homeboy_core::log_status!("observe", "phase timing persistence failed: {}", error)
        })
        .ok()?;
    Some(for_run(&store, run_id))
}

#[cfg(test)]
pub(crate) fn test_run(store: &ObservationStore) -> String {
    store
        .start_run(homeboy_core::observation::NewRunRecord::builder("agent-task").build())
        .unwrap()
        .id
}

pub(crate) fn time<T>(phase: &str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    time_with_outcome(phase, operation, |_| false)
}

pub(crate) fn time_with_outcome<T>(
    phase: &str,
    operation: impl FnOnce() -> Result<T>,
    failed: impl FnOnce(&T) -> bool,
) -> Result<T> {
    if let Some(context) = current() {
        match ObservationStore::open_initialized_at(context.0) {
            Ok(store) => {
                return store.time_run_phase_with_outcome(&context.1, phase, operation, failed);
            }
            Err(error) => {
                homeboy_core::log_status!("observe", "phase timing persistence failed: {}", error)
            }
        }
    }
    operation()
}
