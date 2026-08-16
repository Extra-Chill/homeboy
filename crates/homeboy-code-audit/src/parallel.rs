//! Deterministic scoped-thread fan-out for the audit's independent detectors.
//!
//! Audit detectors are independent by construction: each one is a pure function
//! of an immutable corpus (`&[&FileFingerprint]`, a `CodebaseSnapshot`, the
//! resolved `AuditConfig`) and returns an owned `Vec<Finding>`. Nothing in the
//! detector phase mutates shared state, so the phase was serial only because it
//! was written as a loop.
//!
//! `std::thread::scope` is used rather than a work-stealing crate deliberately:
//! it needs no new dependency (so no `Cargo.lock` churn on a `--locked` CI
//! build), it lets the detectors keep borrowing the corpora that live on the
//! caller's stack, and the fan-out shape here is one flat batch of independent
//! jobs — the case scoped threads handle without ceremony.
//!
//! The contract that matters for the audit is DETERMINISM: [`map_parallel`]
//! returns its outputs in job order, never completion order, so no caller can
//! observe thread scheduling in a findings vector or a timing report.
//!
//! One thing the fan-out does change is progress LOGGING. `time_audit_detector`'s
//! `Running …` / `Completed … in Nms` lines are emitted from the worker threads as
//! work happens, so they interleave: each `eprintln!` is atomic, but a `Running`
//! line no longer sits next to its own `Completed` line.
//! `AuditTiming::log_detector_summary` is the ordered, ranked view that replaces
//! reading those lines top to bottom.

use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread::ScopedJoinHandle;

/// Operator override for the detector worker count. `1` forces the original
/// serial execution, which is the first thing to try when a detector is
/// suspected of being order- or concurrency-sensitive.
const WORKER_COUNT_ENV: &str = "HOMEBOY_AUDIT_DETECTOR_THREADS";

/// Run `run` over every job concurrently and return the outputs **in job
/// order**, regardless of completion order.
///
/// Jobs are handed out through a shared cursor rather than pre-partitioned
/// because detector costs are wildly uneven — one duplication pass can outweigh
/// thirty per-file scanners — and a static split would leave workers idle behind
/// the slowest chunk.
///
/// The caller participates as one of the workers, so an `n`-job batch on `n`-way
/// parallelism spawns `n - 1` threads rather than `n`.
///
/// Bounds worth stating explicitly, because scoped threads only compile when
/// they hold: `J: Sync` is what makes `&jobs` sendable to a worker, `F: Sync` is
/// what makes `&run` sendable, and `R: Send` is what lets a completed output
/// move back to the caller through the collector.
pub(crate) fn map_parallel<J, R, F>(jobs: &[J], run: F) -> Vec<R>
where
    J: Sync,
    R: Send,
    F: Fn(&J) -> R + Sync,
{
    let workers = worker_count(jobs.len());
    if workers <= 1 {
        return jobs.iter().map(run).collect();
    }

    let cursor = AtomicUsize::new(0);
    let collected: Mutex<Vec<(usize, R)>> = Mutex::new(Vec::with_capacity(jobs.len()));

    std::thread::scope(|scope| {
        let helpers: Vec<_> = (1..workers)
            .map(|_| scope.spawn(|| drain_jobs(jobs, &run, &cursor, &collected)))
            .collect();
        // The caller is a worker too, so a batch never costs an idle thread.
        drain_jobs(jobs, &run, &cursor, &collected);
        for helper in helpers {
            join_scoped(helper);
        }
    });

    let mut collected = collected
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    // The single point where completion order is discarded. Job indices are
    // unique, so this total order is exact — not a tie-broken approximation.
    collected.sort_by_key(|(index, _)| *index);
    collected.into_iter().map(|(_, output)| output).collect()
}

/// Take jobs from the shared cursor until the batch is exhausted, tagging each
/// output with the index it came from.
///
/// `run` is called OUTSIDE the collector lock: the lock is held only for the
/// push, so two detectors never serialize against each other.
fn drain_jobs<J, R, F>(
    jobs: &[J],
    run: &F,
    cursor: &AtomicUsize,
    collected: &Mutex<Vec<(usize, R)>>,
) where
    J: Sync,
    R: Send,
    F: Fn(&J) -> R + Sync,
{
    loop {
        let index = cursor.fetch_add(1, Ordering::Relaxed);
        let Some(job) = jobs.get(index) else {
            break;
        };
        let output = run(job);
        lock(collected).push((index, output));
    }
}

/// A unit of work that either already ran on the caller's thread or is running on
/// a scoped worker.
///
/// This exists so a fan-out site keeps ONE code path across the concurrent mode
/// and the `HOMEBOY_AUDIT_DETECTOR_THREADS=1` serial mode: the site always writes
/// `spawn_or_run(...)` and later `join()`, and only the scheduling differs. A
/// hand-written second serial branch would be a place for the two modes to drift
/// apart — and the whole value of the escape hatch is that the serial mode is a
/// faithful reference for the concurrent one.
pub(crate) enum ScopedUnit<'scope, T> {
    /// Already computed inline, because concurrency is disabled.
    Ready(T),
    /// Running on a scoped worker thread.
    Running(ScopedJoinHandle<'scope, T>),
}

impl<T> ScopedUnit<'_, T> {
    /// Take the unit's output, waiting for its worker when there is one.
    pub(crate) fn join(self) -> T {
        match self {
            Self::Ready(value) => value,
            Self::Running(handle) => join_scoped(handle),
        }
    }
}

/// Start `run` on `scope`, or run it inline when detector concurrency is
/// disabled.
///
/// In the inline case the work happens at the call site, so a site that starts
/// its units in a fixed order and joins them in that same order executes exactly
/// the sequence it did before this fan-out existed.
pub(crate) fn spawn_or_run<'scope, 'env, T, F>(
    scope: &'scope std::thread::Scope<'scope, 'env>,
    run: F,
) -> ScopedUnit<'scope, T>
where
    F: FnOnce() -> T + Send + 'scope,
    T: Send + 'scope,
{
    if concurrency_enabled() {
        ScopedUnit::Running(scope.spawn(run))
    } else {
        ScopedUnit::Ready(run())
    }
}

/// Whether the detector phase may fan out at all.
///
/// Honors the same `HOMEBOY_AUDIT_DETECTOR_THREADS` override as
/// [`map_parallel`], so setting it to `1` restores fully serial execution across
/// every fan-out site rather than just this module's job pool.
pub(crate) fn concurrency_enabled() -> bool {
    worker_count(2) > 1
}

/// Join a scoped worker, re-raising its panic in the caller.
///
/// A detector panic used to abort the audit immediately; propagating the payload
/// keeps that observable behavior (same panic, same message, non-zero exit)
/// instead of silently swallowing it into a `Result`.
pub(crate) fn join_scoped<T>(handle: ScopedJoinHandle<'_, T>) -> T {
    match handle.join() {
        Ok(value) => value,
        Err(payload) => std::panic::resume_unwind(payload),
    }
}

/// Lock the collector, tolerating poisoning.
///
/// A poisoned collector means another worker panicked while pushing. That panic
/// is already on its way to the caller via [`join_scoped`], so refusing the lock
/// here would only replace a real diagnosis with a secondary one.
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Worker count for a batch of `job_count` jobs: never more workers than jobs,
/// never more than the machine's available parallelism, and always at least one
/// for a non-empty batch.
fn worker_count(job_count: usize) -> usize {
    if job_count <= 1 {
        return job_count;
    }

    let requested = std::env::var(WORKER_COUNT_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|value| *value > 0)
        .unwrap_or_else(|| {
            std::thread::available_parallelism()
                .map(NonZeroUsize::get)
                .unwrap_or(1)
        });

    requested.clamp(1, job_count)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The one property every caller depends on: outputs come back in job order
    /// even though the jobs finish in a different order. The reversed sleep makes
    /// late jobs finish first, so a collection-order implementation fails this
    /// rather than passing by luck.
    #[test]
    fn outputs_are_returned_in_job_order_not_completion_order() {
        let jobs: Vec<u64> = (0..16).collect();
        let outputs = map_parallel(&jobs, |job| {
            std::thread::sleep(std::time::Duration::from_millis(16 - *job));
            *job * 10
        });

        assert_eq!(
            outputs,
            (0..16).map(|job| job * 10).collect::<Vec<u64>>(),
            "map_parallel must discard completion order"
        );
    }

    /// Repeating the fan-out must produce identical output. A single run can pass
    /// by scheduling luck; a hundred cannot.
    #[test]
    fn repeated_runs_are_identical() {
        let jobs: Vec<u32> = (0..32).collect();
        let expected: Vec<u32> = jobs.iter().map(|job| job * job).collect();

        for _ in 0..100 {
            assert_eq!(map_parallel(&jobs, |job| job * job), expected);
        }
    }

    #[test]
    fn empty_and_single_job_batches_run_inline() {
        let empty: Vec<usize> = Vec::new();
        assert!(map_parallel(&empty, |job: &usize| *job).is_empty());
        assert_eq!(worker_count(0), 0);
        assert_eq!(worker_count(1), 1);

        assert_eq!(map_parallel(&[7usize], |job| *job + 1), vec![8]);
    }

    #[test]
    fn worker_count_never_exceeds_job_count() {
        for job_count in 2..8 {
            assert!(worker_count(job_count) <= job_count);
            assert!(worker_count(job_count) >= 1);
        }
    }
}
