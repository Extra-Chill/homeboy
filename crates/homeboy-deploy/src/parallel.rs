use std::collections::VecDeque;
use std::sync::{Mutex, MutexGuard};

/// Apply owned jobs with bounded concurrency and return results in input order.
pub(crate) fn map_bounded<J, R, F>(jobs: Vec<J>, max_concurrency: usize, apply: F) -> Vec<R>
where
    J: Send,
    R: Send,
    F: Fn(J) -> R + Sync,
{
    let worker_count = max_concurrency.max(1).min(jobs.len());
    if worker_count <= 1 {
        return jobs.into_iter().map(apply).collect();
    }

    let pending = Mutex::new(jobs.into_iter().enumerate().collect::<VecDeque<_>>());
    let completed = Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        let helpers: Vec<_> = (1..worker_count)
            .map(|_| scope.spawn(|| drain(&pending, &completed, &apply)))
            .collect();
        drain(&pending, &completed, &apply);
        for helper in helpers {
            match helper.join() {
                Ok(()) => {}
                Err(payload) => std::panic::resume_unwind(payload),
            }
        }
    });

    let mut completed = into_inner(completed);
    completed.sort_by_key(|(index, _)| *index);
    completed.into_iter().map(|(_, result)| result).collect()
}

fn drain<J, R, F>(
    pending: &Mutex<VecDeque<(usize, J)>>,
    completed: &Mutex<Vec<(usize, R)>>,
    apply: &F,
) where
    J: Send,
    R: Send,
    F: Fn(J) -> R + Sync,
{
    loop {
        let Some((index, job)) = lock(pending).pop_front() else {
            return;
        };
        let result = apply(job);
        lock(completed).push((index, result));
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn into_inner<T>(mutex: Mutex<T>) -> T {
    mutex
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    use super::*;

    #[test]
    fn completion_order_does_not_change_output_order() {
        let jobs = (0..8).collect::<Vec<u64>>();
        let results = map_bounded(jobs, 4, |job| {
            std::thread::sleep(Duration::from_millis(8 - job));
            job
        });

        assert_eq!(results, (0..8).collect::<Vec<_>>());
    }

    #[test]
    fn concurrency_never_exceeds_the_requested_bound() {
        let active = AtomicUsize::new(0);
        let peak = AtomicUsize::new(0);
        map_bounded((0..12).collect::<Vec<_>>(), 3, |_| {
            let current = active.fetch_add(1, Ordering::SeqCst) + 1;
            peak.fetch_max(current, Ordering::SeqCst);
            std::thread::sleep(Duration::from_millis(5));
            active.fetch_sub(1, Ordering::SeqCst);
        });

        assert!(peak.load(Ordering::SeqCst) > 1);
        assert!(peak.load(Ordering::SeqCst) <= 3);
    }

    #[test]
    fn one_job_failure_does_not_cancel_other_jobs() {
        let completed = AtomicUsize::new(0);
        let results = map_bounded((0..6).collect::<Vec<_>>(), 2, |job| {
            completed.fetch_add(1, Ordering::SeqCst);
            if job == 2 {
                Err(job)
            } else {
                Ok(job)
            }
        });

        assert_eq!(completed.load(Ordering::SeqCst), 6);
        assert_eq!(results[2], Err(2));
        assert_eq!(results.len(), 6);
    }

    #[test]
    fn zero_or_one_worker_runs_serially() {
        let thread = std::thread::current().id();
        let zero = map_bounded(vec![1, 2], 0, |_| std::thread::current().id());
        let one = map_bounded(vec![1, 2], 1, |_| std::thread::current().id());

        assert!(zero.iter().all(|worker| *worker == thread));
        assert!(one.iter().all(|worker| *worker == thread));
    }
}
