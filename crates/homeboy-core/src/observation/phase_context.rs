//! Run-bound timing context for synchronous controller work and probe workers.
use std::{cell::RefCell, path::PathBuf};

use super::ObservationStore;

pub type Context = (PathBuf, String);

thread_local! {
    static CURRENT: RefCell<Option<Context>> = const { RefCell::new(None) };
}

pub fn current() -> Option<Context> {
    CURRENT.with(|slot| slot.borrow().clone())
}

pub struct Scope {
    previous: Option<Context>,
    sink: Option<
        std::sync::Arc<dyn Fn(&homeboy_engine_primitives::phase_timing::PhaseSpan) + Send + Sync>,
    >,
}

impl Drop for Scope {
    fn drop(&mut self) {
        CURRENT.with(|slot| *slot.borrow_mut() = self.previous.take());
        homeboy_engine_primitives::phase_timing::replace_sink(self.sink.take());
    }
}

pub fn enter(context: Option<Context>) -> Scope {
    let sink = context.clone().map(|(path, run_id)| {
        std::sync::Arc::new(
            move |span: &homeboy_engine_primitives::phase_timing::PhaseSpan| {
                let mut span = span.clone();
                span.homeboy_version = crate::build_identity::current().version.to_string();
                static NEXT_SPAN: std::sync::atomic::AtomicU64 =
                    std::sync::atomic::AtomicU64::new(0);
                let sequence = NEXT_SPAN.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                let identity = format!(
                    "{}:{}:{}:{sequence}",
                    span.id,
                    std::process::id(),
                    span.started_at
                );
                let report = homeboy_engine_primitives::phase_timing::PhaseTimingReport {
                    spans: vec![span],
                };
                if let Err(error) = ObservationStore::open_initialized_at(&path)
                    .and_then(|store| store.record_phase_spans(&run_id, &identity, &report))
                {
                    crate::log_status!("observe", "phase timing persistence failed: {}", error);
                }
            },
        )
            as std::sync::Arc<
                dyn Fn(&homeboy_engine_primitives::phase_timing::PhaseSpan) + Send + Sync,
            >
    });
    Scope {
        previous: CURRENT.with(|slot| std::mem::replace(&mut *slot.borrow_mut(), context)),
        sink: homeboy_engine_primitives::phase_timing::replace_sink(sink),
    }
}

pub fn for_run(store: &ObservationStore, run_id: &str) -> Scope {
    enter(Some((
        store.database_path().to_path_buf(),
        run_id.to_string(),
    )))
}

/// Capture the current run at submission, not when the worker starts.
/// Use this for both named and scoped thread builders.
pub fn inherit<F, T>(operation: F) -> impl FnOnce() -> T
where
    F: FnOnce() -> T,
{
    let context = current();
    move || {
        let _scope = enter(context);
        operation()
    }
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
        std::thread::spawn(inherit(move || {
            assert_eq!(current(), context);
            homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                .time_with_outcome::<_, crate::Error>("worker", || Ok(false), |ok| !ok)
                .unwrap();
        }))
        .join()
        .unwrap();
        let spans = store.phase_spans(&run.id).unwrap().spans;
        assert_eq!(spans[0].id, "worker");
        assert_eq!(
            spans[0].status,
            homeboy_engine_primitives::phase_timing::PhaseStatus::Failed
        );
        assert!(spans[0].duration_ms.is_some());
    }

    #[test]
    fn execution_boundary_records_post_terminal_recovery_leaves_and_worker_coverage() {
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("timing.db")).unwrap();
        let run = store
            .start_run(super::super::NewRunRecord::builder("agent-task").build())
            .unwrap();
        // Only the execution boundary knows about the store. Recovery and gates
        // (including a named worker) use the same context-free leaf API.
        store
            .phase_timer(&run.id)
            .time::<_, crate::Error>("cook.execution", || {
                homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                    .time::<_, crate::Error>("terminal.decision", || Ok(()))?;
                homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                    .time::<_, crate::Error>("promote.recover.candidate", || {
                        homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                            .time::<_, crate::Error>("promote.recover.controller_gates", || Ok(()))
                    })?;
                homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                    .time::<_, crate::Error>("gate.destination_setup", || Ok(()))?;
                std::thread::Builder::new()
                    .name("destination-gate".into())
                    .spawn(inherit(|| {
                        homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                            .time::<_, crate::Error>("gate.destination_execute", || {
                                assert!(current().is_some(), "worker must inherit the run context");
                                Ok(())
                            })
                    }))
                    .unwrap()
                    .join()
                    .unwrap()?;
                homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                    .time::<_, crate::Error>("mission.finalization", || {
                        store.finish_run(&run.id, super::super::RunStatus::Pass, None)
                    })?;
                Ok(())
            })
            .unwrap();
        assert!(current().is_none());
        let spans = store.phase_spans(&run.id).unwrap().spans;
        for phase in [
            "terminal.decision",
            "promote.recover.candidate",
            "promote.recover.controller_gates",
            "gate.destination_setup",
            "gate.destination_execute",
            "mission.finalization",
        ] {
            assert!(spans.iter().any(|span| span.id == phase), "missing {phase}");
        }
        // Measure the execution boundary, not the scheduling/SQLite latency
        // between admitting the run and entering that boundary. Coverage of
        // arbitrary admission gaps is tested separately in phase_coverage.
        let execution = spans
            .iter()
            .find(|span| span.id == "cook.execution")
            .expect("persisted execution boundary");
        let start = chrono::DateTime::parse_from_rfc3339(&execution.started_at).unwrap();
        let duration = execution.duration_ms.expect("completed execution span");
        let mut window = store.get_run(&run.id).unwrap().unwrap();
        assert_eq!(
            window.status, "pass",
            "finalization must persist before the boundary closes"
        );
        window.started_at = execution.started_at.clone();
        window.finished_at = Some(
            (start + chrono::Duration::milliseconds(duration.ceil() as i64))
                .to_rfc3339_opts(chrono::SecondsFormat::Millis, true),
        );
        store.upsert_imported_run(&window).unwrap();
        let coverage = store.phase_coverage(&run.id, 10).unwrap();
        assert!((coverage.wall_ms - coverage.covered_ms).abs() <= 1.0,
            "execution boundary should cover its worker and recovery leaves: {coverage:?}; spans: {spans:?}");
    }

    #[test]
    fn shared_guard_records_early_returns_panics_and_repeated_phases() {
        use homeboy_engine_primitives::phase_timing::{PhaseStatus, PhaseTimer};
        let dir = tempfile::tempdir().unwrap();
        let store = ObservationStore::open_initialized_at(dir.path().join("guards.db")).unwrap();
        let run = store
            .start_run(super::super::NewRunRecord::builder("bench").build())
            .unwrap();
        {
            let _phase = store.phase_timer(&run.id).into_guard("bench");
            let worker = inherit(|| {
                for _ in 0..3 {
                    let _guard = PhaseTimer::current().into_guard("sample");
                }
                let _ = std::panic::catch_unwind(|| {
                    let _guard = PhaseTimer::current().into_guard("panic");
                    panic!("timed failure");
                });
            });
            std::thread::spawn(worker).join().unwrap();
        }
        assert!(current().is_none());
        let report = store.phase_spans(&run.id).unwrap();
        assert_eq!(
            report
                .spans
                .iter()
                .filter(|span| span.id == "sample")
                .count(),
            3
        );
        assert_eq!(report.span("panic").unwrap().status, PhaseStatus::Failed);
        assert_eq!(report.span("bench").unwrap().status, PhaseStatus::Ok);
        assert!(report.spans.iter().all(|span| span.duration_ms.is_some()));
    }

    #[test]
    fn persistence_failure_preserves_success_and_workflow_error() {
        let dir = tempfile::tempdir().unwrap();
        let _scope = enter(Some((dir.path().to_path_buf(), "invalid".into())));
        assert_eq!(
            homeboy_engine_primitives::phase_timing::PhaseTimer::current()
                .time::<_, crate::Error>("success", || Ok(42))
                .unwrap(),
            42
        );
        let error = homeboy_engine_primitives::phase_timing::PhaseTimer::current()
            .time::<(), crate::Error>("failure", || {
                Err(crate::Error::internal_unexpected("work failed"))
            });
        assert!(error.unwrap_err().to_string().contains("work failed"));
        homeboy_engine_primitives::phase_timing::PhaseTimer::current().finish(
            "finished",
            std::time::Instant::now(),
            true,
        );
    }
}
