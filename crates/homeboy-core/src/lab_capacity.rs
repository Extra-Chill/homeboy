//! Small, admission-only CPU policy. Running work is never preempted.
use serde::Serialize;
use std::collections::BTreeMap;

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
    pub active_slots_by_owner: BTreeMap<String, usize>,
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
        active_slots_by_owner: STEP_STATE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .active_slots_by_owner(),
    }
}

/// Queue and occupied slots share a lock so ranking and admission are atomic.
#[derive(Default)]
struct StepState {
    slots: Vec<Option<String>>,
    queue: Vec<(usize, String)>,
}

impl StepState {
    fn active_slots_by_owner(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for owner in self.slots.iter().flatten() {
            *counts.entry(owner.clone()).or_default() += 1;
        }
        counts
    }
}

static STEP_STATE: std::sync::Mutex<StepState> = std::sync::Mutex::new(StepState {
    slots: Vec::new(),
    queue: Vec::new(),
});
static NEXT_TICKET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// An admitted step's slot. The slot owns a fixed share of the CPUs, so
/// concurrent steps never size themselves against the same cores.
pub struct StepPermit {
    slot: usize,
}
impl StepPermit {
    pub fn slot(&self) -> usize {
        self.slot
    }
}
impl Drop for StepPermit {
    fn drop(&mut self) {
        let mut state = STEP_STATE.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(owner) = state.slots.get_mut(self.slot) {
            *owner = None;
        }
    }
}

/// The CPUs a step in `slot` may use: an equal, disjoint share of `allowed`
/// for each of the `limit` concurrent steps. Tools that size themselves by
/// CPU count (`nproc`, test runners, `make -j`) then fit their share. No
/// limit, or a share that would cover every CPU, leaves the step unpinned.
pub fn cpu_slice(allowed: &[usize], limit: Option<usize>, slot: usize) -> Option<Vec<usize>> {
    let limit = limit.filter(|limit| *limit > 1)?;
    let width = (allowed.len() / limit).max(1);
    if width >= allowed.len() {
        return None;
    }
    let start = (slot % limit) * width % allowed.len();
    Some(allowed[start..(start + width).min(allowed.len())].to_vec())
}

/// Admit the least-occupied owner first, breaking ties by ticket arrival.
pub fn acquire_step(
    limit: Option<usize>,
    owner: &str,
    waiting: impl FnMut(usize, &LabCapacity) -> crate::Result<bool>,
) -> crate::Result<Option<StepPermit>> {
    acquire_step_with_observation(limit, owner, waiting, observe)
}

fn acquire_step_with_observation(
    limit: Option<usize>,
    owner: &str,
    mut waiting: impl FnMut(usize, &LabCapacity) -> crate::Result<bool>,
    observation: impl Fn(Option<usize>) -> LabCapacity,
) -> crate::Result<Option<StepPermit>> {
    use std::sync::atomic::Ordering;
    let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
    STEP_STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .queue
        .push((ticket, owner.to_owned()));
    struct Queued(usize);
    impl Drop for Queued {
        fn drop(&mut self) {
            STEP_STATE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .queue
                .retain(|(ticket, _)| *ticket != self.0);
        }
    }
    let _queued = Queued(ticket);
    loop {
        let mut observation = observation(limit);
        let position = {
            let mut state = STEP_STATE.lock().unwrap_or_else(|e| e.into_inner());
            let counts = state.active_slots_by_owner();
            observation.active_slots_by_owner = counts.clone();
            let rank = (counts.get(owner).copied().unwrap_or(0), ticket);
            let position = 1 + state
                .queue
                .iter()
                .filter(|(candidate, owner)| {
                    (counts.get(owner).copied().unwrap_or(0), *candidate) < rank
                })
                .count();
            let active = state.slots.iter().flatten().count();
            if position == 1 && active < observation.effective_budget {
                let slot = state
                    .slots
                    .iter()
                    .position(Option::is_none)
                    .unwrap_or(state.slots.len());
                if slot == state.slots.len() {
                    state.slots.push(Some(owner.to_owned()));
                } else {
                    state.slots[slot] = Some(owner.to_owned());
                }
                state.queue.retain(|(candidate, _)| *candidate != ticket);
                return Ok(Some(StepPermit { slot }));
            }
            position
        };
        if !waiting(position, &observation)? {
            return Ok(None);
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// The CPUs this process may run on, in order.
#[cfg(target_os = "linux")]
pub fn allowed_cpus() -> Vec<usize> {
    // SAFETY: `set` is a plain bitmask owned by this frame and sized for the call.
    unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        if libc::sched_getaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &mut set) != 0 {
            return Vec::new();
        }
        (0..libc::CPU_SETSIZE as usize)
            .filter(|cpu| libc::CPU_ISSET(*cpu, &set))
            .collect()
    }
}

#[cfg(not(target_os = "linux"))]
pub fn allowed_cpus() -> Vec<usize> {
    Vec::new()
}

/// Pin the spawned process, and so its whole tree, to `cpus`.
#[cfg(target_os = "linux")]
pub fn pin_command(command: &mut std::process::Command, cpus: &[usize]) {
    use std::os::unix::process::CommandExt;
    // SAFETY: the mask is built before fork; the hook only issues the
    // async-signal-safe `sched_setaffinity` syscall.
    let set = unsafe {
        let mut set: libc::cpu_set_t = std::mem::zeroed();
        for cpu in cpus {
            libc::CPU_SET(*cpu, &mut set);
        }
        set
    };
    unsafe {
        command.pre_exec(move || {
            if libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(target_os = "linux"))]
pub fn pin_command(_command: &mut std::process::Command, _cpus: &[usize]) {}

pub fn queued_steps() -> usize {
    STEP_STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .queue
        .len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn waiting_owner_gets_a_slot_before_saturated_owners_queued_tickets() {
        crate::test_support::with_isolated_home(|_| {
            use std::sync::mpsc;
            use std::time::Duration;

            // Keep this independent of host load and CPU count.
            fn observation(limit: Option<usize>) -> LabCapacity {
                LabCapacity {
                    cpus: 2,
                    load_average_1m: Some(0.0),
                    concurrency_limit: limit,
                    effective_budget: 2,
                    active_slots_by_owner: BTreeMap::new(),
                }
            }
            let first = acquire_step_with_observation(Some(2), "A", |_, _| Ok(true), observation)
                .unwrap()
                .unwrap();
            let second = acquire_step_with_observation(Some(2), "A", |_, _| Ok(true), observation)
                .unwrap()
                .unwrap();
            assert_eq!(observe(Some(2)).active_slots_by_owner["A"], 2);
            std::thread::scope(|scope| {
                let (queued_tx, queued_rx) = mpsc::channel();
                let (admitted_tx, admitted_rx) = mpsc::channel();
                let (release_tx, release_rx) = mpsc::channel();
                // Start each ticket only after its predecessor is actually queued.
                for expected_position in 1..=2 {
                    let queued_tx = queued_tx.clone();
                    let admitted_tx = admitted_tx.clone();
                    scope.spawn(move || {
                        let mut reported = false;
                        let permit = acquire_step_with_observation(
                            Some(2),
                            "A",
                            |position, capacity| {
                                if !reported {
                                    assert_eq!(position, expected_position);
                                    assert_eq!(capacity.active_slots_by_owner["A"], 2);
                                    queued_tx.send(()).unwrap();
                                    reported = true;
                                }
                                Ok(true)
                            },
                            observation,
                        )
                        .unwrap();
                        admitted_tx.send("A").unwrap();
                        drop(permit);
                    });
                    queued_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                }
                scope.spawn(move || {
                    let mut reported = false;
                    let permit = acquire_step_with_observation(
                        Some(2),
                        "B",
                        |position, _| {
                            if !reported {
                                assert_eq!(position, 1);
                                queued_tx.send(()).unwrap();
                                reported = true;
                            }
                            Ok(true)
                        },
                        observation,
                    )
                    .unwrap();
                    admitted_tx.send("B").unwrap();
                    release_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                    drop(permit);
                });
                queued_rx.recv_timeout(Duration::from_secs(10)).unwrap();
                drop(first);
                assert_eq!(
                    admitted_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
                    "B"
                );
                assert_eq!(observe(Some(2)).active_slots_by_owner["B"], 1);
                release_tx.send(()).unwrap();
                drop(second);
                assert_eq!(
                    admitted_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
                    "A"
                );
                assert_eq!(
                    admitted_rx.recv_timeout(Duration::from_secs(10)).unwrap(),
                    "A"
                );
            });
            assert!(observe(Some(2)).active_slots_by_owner.is_empty());
            assert_eq!(queued_steps(), 0);
        });
    }

    #[test]
    fn cpu_slices_are_disjoint_shares() {
        let cpus = (0..32).collect::<Vec<_>>();
        for (limit, slot, expected) in [
            (Some(4), 0, Some((0..8).collect::<Vec<_>>())),
            (Some(4), 3, Some((24..32).collect())),
            (Some(3), 2, Some((20..30).collect())),
            (Some(64), 40, Some(vec![8])),
            (Some(1), 0, None),
            (None, 0, None),
        ] {
            assert_eq!(cpu_slice(&cpus, limit, slot), expected, "{limit:?} {slot}");
        }
    }

    /// A pinned step's tree sees only its share: `nproc` sizes to the slice.
    #[test]
    #[cfg(target_os = "linux")]
    fn pinned_child_nproc_reports_its_slice() {
        let allowed = allowed_cpus();
        let Some(slice) = cpu_slice(&allowed, Some(2), 1) else {
            return; // a single-CPU host has nothing to divide
        };
        let mut command = std::process::Command::new("sh");
        command.args(["-c", "nproc"]);
        pin_command(&mut command, &slice);
        let output = command.output().expect("run nproc");
        assert_eq!(
            String::from_utf8_lossy(&output.stdout).trim(),
            slice.len().to_string()
        );
    }

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
