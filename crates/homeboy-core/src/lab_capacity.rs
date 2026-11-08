//! Small, admission-only CPU policy. Running work is never preempted.
use serde::Serialize;

/// Leave one logical CPU per admitted heavy job, with one slot for forward progress.
/// Missing load data falls back to the CPU bound, never above the configured cap.
pub fn admission_budget(cpus: usize, load: Option<f64>, limit: usize) -> usize {
    let load = load
        .filter(|load| load.is_finite() && *load >= 0.0)
        .unwrap_or(0.0);
    ((cpus as f64 - load).floor().max(1.0) as usize).min(limit)
}

#[derive(Debug, Clone, Serialize)]
pub struct LabCapacity {
    pub cpus: usize,
    pub load_average_1m: Option<f64>,
    pub concurrency_limit: Option<usize>,
    pub effective_budget: usize,
}

pub fn observe(limit: Option<usize>) -> LabCapacity {
    let cpus = std::thread::available_parallelism()
        .map(usize::from)
        .unwrap_or(1);
    let load_average_1m = std::fs::read_to_string("/proc/loadavg")
        .ok()
        .and_then(|text| text.split_whitespace().next()?.parse::<f64>().ok());
    LabCapacity {
        cpus,
        load_average_1m,
        concurrency_limit: limit,
        effective_budget: admission_budget(cpus, load_average_1m, limit.unwrap_or(usize::MAX)),
    }
}

static ACTIVE_STEPS: std::sync::Mutex<usize> = std::sync::Mutex::new(0);
static NEXT_TICKET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
static STEP_QUEUE: std::sync::Mutex<Vec<usize>> = std::sync::Mutex::new(Vec::new());

pub struct StepPermit;
impl Drop for StepPermit {
    fn drop(&mut self) {
        let mut active = ACTIVE_STEPS.lock().unwrap_or_else(|e| e.into_inner());
        *active = active.saturating_sub(1);
    }
}

/// FIFO admission for detached provider/gate steps owned by this daemon.
pub fn acquire_step(
    limit: Option<usize>,
    mut waiting: impl FnMut(usize, &LabCapacity) -> crate::Result<bool>,
) -> crate::Result<Option<StepPermit>> {
    use std::sync::atomic::Ordering;
    let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
    STEP_QUEUE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(ticket);
    struct Queued(usize);
    impl Drop for Queued {
        fn drop(&mut self) {
            STEP_QUEUE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .retain(|ticket| *ticket != self.0);
        }
    }
    let _queued = Queued(ticket);
    loop {
        let observation = observe(limit);
        let position = STEP_QUEUE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .position(|candidate| *candidate == ticket)
            .unwrap_or(0)
            + 1;
        {
            let mut active = ACTIVE_STEPS.lock().unwrap_or_else(|e| e.into_inner());
            if position == 1 && *active < observation.effective_budget {
                *active += 1;
                return Ok(Some(StepPermit));
            }
        }
        if !waiting(position, &observation)? {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

pub fn queued_steps() -> usize {
    STEP_QUEUE.lock().unwrap_or_else(|e| e.into_inner()).len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn budget_table() {
        for (cpus, load, limit, expected) in [
            (32, Some(0.0), 4, 4),
            (32, Some(30.0), 4, 2),
            (32, Some(31.5), 4, 1),
            (32, Some(40.0), 4, 1),
            (1, Some(0.0), 8, 1),
            (8, None, 3, 3),
            (8, Some(f64::NAN), 3, 3),
            (8, Some(-1.0), 3, 3),
            (8, Some(0.0), 0, 0),
        ] {
            assert_eq!(
                admission_budget(cpus, load, limit),
                expected,
                "cpus={cpus} load={load:?} limit={limit}"
            );
        }
    }
}
