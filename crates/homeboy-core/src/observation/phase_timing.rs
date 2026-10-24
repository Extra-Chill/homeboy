//! Run-bound timing context for synchronous controller work and probe workers.
use std::{cell::RefCell, path::PathBuf};

use super::ObservationStore;
use crate::Result;

pub type Context = (PathBuf, String);

thread_local! {
    static CURRENT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

pub fn current() -> Option<Context> {
    CURRENT.with(|slot| slot.borrow().clone())
}

pub struct Scope(Option<Context>);

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|slot| *slot.borrow_mut() = self.0.take());
    }
}

pub fn enter(context: Option<Context>) -> Scope {
    Scope(CURRENT.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), context)))
}

pub fn for_run(store: &ObservationStore, run_id: &str) -> Scope {
    enter(Some((
        store.database_path().to_path_buf(),
        run_id.to_string(),
    )))
}

/// Finish work whose lifetime cannot be enclosed in a closure.
pub fn finish(phase: &str, started: std::time::Instant, failed: bool) {
    if let Some((path, run_id)) = current() {
        let started_at = (chrono::Utc::now() - started.elapsed()).to_rfc3339();
        if let Err(error) = ObservationStore::open_initialized_at(path)
            .and_then(|store| store.finish_run_phase(&run_id, phase, &started_at, failed))
        {
            crate::log_status!("observe", "phase timing persistence failed: {}", error);
        }
    }
}

pub fn time<T>(phase: &str, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    time_with_outcome(phase, operation, |_| false)
}

pub fn time_with_outcome<T>(
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
                crate::log_status!("observe", "phase timing persistence failed: {}", error)
            }
        }
    }
    operation()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nested_scopes_restore_context_and_workers_explicitly_inherit_it() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("timing.db")).unwrap();
        let run = store
            .start_run(super::super::NewRunRecord::builder("test").build())
            .unwrap();
        let _scope = for_run(&store, &run.id);
        let context = current();
        {
            let _disabled = enter(None);
            assert!(current().is_none());
        }
        assert_eq!(current(), context);
        std::thread::spawn(move || {
            assert!(current().is_none());
            let _scope = enter(context);
            time_with_outcome("worker", || Ok(false), |ok| !ok).unwrap();
        })
        .join()
        .unwrap();
        let spans = store.phase_spans(&run.id).unwrap().spans;
        assert_eq!(spans[0].id, "worker");
        assert_eq!(spans[0].status, crate::phase_timing::PhaseStatus::Failed);
        assert!(spans[0].duration_ms.is_some());
    }

    #[test]
    fn persistence_failure_preserves_success_and_workflow_error() {
        let dir = tempfile::tempdir().unwrap();
        let _scope = enter(Some((dir.path().to_path_buf(), "invalid".into())));
        assert_eq!(time("success", || Ok(42)).unwrap(), 42);
        let error = time::<()>("failure", || {
            Err(crate::Error::internal_unexpected("work failed"))
        });
        assert!(error.unwrap_err().to_string().contains("work failed"));
        finish("finished", std::time::Instant::now(), true);
    }
}
