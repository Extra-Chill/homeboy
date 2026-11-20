//! Small, admission-only CPU policy. Running work is never preempted.
use serde::Serialize;
use std::collections::BTreeMap;
use std::time::{Duration, Instant};

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

/// Reserve the last CPUs, leaving at least one for steps.
/// Oversized requests are clamped to preserve step progress.
/// Unset, invalid, or zero requests preserve the legacy admission policy.
pub fn gate_reserve(allowed: &[usize]) -> Vec<usize> {
    let requested = std::env::var("HOMEBOY_LAB_GATE_RESERVE")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let count = requested.min(allowed.len().saturating_sub(1));
    allowed[allowed.len() - count..].to_vec()
}

/// Gate concurrency is independent of CPU count; an empty reserve disables it.
pub fn gate_slots(reserve: &[usize]) -> usize {
    std::env::var("HOMEBOY_LAB_GATE_SLOTS")
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(1)
        .max(1)
        .min(reserve.len())
}

/// A gate always pins to its slot's disjoint share, even with just one slot.
pub fn gate_cpu_slice(reserve: &[usize], slot: usize) -> Vec<usize> {
    cpu_share(reserve, Some(gate_slots(reserve)), slot, true).unwrap_or_default()
}

/// Direct runner executions are the gate lane; workflow steps use step permits.
pub fn is_gate(job: &crate::api_jobs::Job) -> bool {
    job.runner_job_projection
        .as_ref()
        .is_some_and(|projection| projection.kind == "runner.exec")
}

/// Reserved gates ignore bulk CPU/IO pressure, but retain the memory guard.
pub fn gate_admission_reason(
    pressure: &crate::runner_pressure::RunnerPressure,
    thresholds: &crate::runner_pressure::PressureThresholds,
    active: usize,
    slots: usize,
) -> Option<String> {
    if pressure
        .memory
        .is_some_and(|value| value > thresholds.memory.saturating_mul(100))
    {
        Some("pressure".into())
    } else if active >= slots {
        Some("gate_slot".into())
    } else {
        None
    }
}

/// Admission lane a step declares through [`LAB_PRIORITY_ENV`]. Ordered so
/// `High` sorts before `Normal`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum LabPriority {
    /// Short follow-ups (publish, small fixes) admitted ahead of normal work.
    High,
    #[default]
    Normal,
}

impl LabPriority {
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Self::High),
            "normal" => Some(Self::Normal),
            _ => None,
        }
    }

    /// Unset or unknown values fall back to the normal lane.
    pub fn from_env_value(value: Option<&str>) -> Self {
        value.and_then(Self::parse).unwrap_or_default()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Normal => "normal",
        }
    }
}

/// Step environment naming the admission lane (`high` or `normal`).
pub const LAB_PRIORITY_ENV: &str = "HOMEBOY_LAB_PRIORITY";
/// Step environment declaring its expected run time in seconds; queue ETAs
/// are derived from holders' declared budgets.
pub const LAB_BUDGET_ENV: &str = "HOMEBOY_LAB_BUDGET_SECONDS";
/// Daemon environment bounding the percent of slots one owner may hold while
/// another owner waits (1-100, default 50).
pub const LAB_OWNER_MAX_SHARE_ENV: &str = "HOMEBOY_LAB_OWNER_MAX_SHARE";
const DEFAULT_OWNER_MAX_SHARE_PERCENT: usize = 50;

pub fn owner_max_share_percent() -> usize {
    std::env::var(LAB_OWNER_MAX_SHARE_ENV)
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .filter(|percent| (1..=100).contains(percent))
        .unwrap_or(DEFAULT_OWNER_MAX_SHARE_PERCENT)
}

/// Most slots one owner may hold while another owner waits; never below one.
pub fn owner_slot_cap(budget: usize, share_percent: usize) -> usize {
    (budget * share_percent / 100).max(1)
}

/// A step waiting for a slot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LabWaiter {
    pub ticket: usize,
    pub owner: String,
    pub priority: LabPriority,
}

/// Order in which waiters take freed slots: priority lane first, then the
/// least-occupied owner, then arrival. While another owner waits, an owner
/// already holding `owner_cap` slots is deferred behind every other waiter.
/// Returns tickets, head first.
pub fn admission_order(
    waiters: &[LabWaiter],
    held: &BTreeMap<String, usize>,
    owner_cap: usize,
) -> Vec<usize> {
    let held_by = |owner: &str| held.get(owner).copied().unwrap_or(0);
    let deferred = |waiter: &LabWaiter| {
        held_by(&waiter.owner) >= owner_cap
            && waiters.iter().any(|other| other.owner != waiter.owner)
    };
    let mut order = waiters.iter().collect::<Vec<_>>();
    order.sort_by_key(|waiter| {
        (
            deferred(waiter),
            waiter.priority,
            held_by(&waiter.owner),
            waiter.ticket,
        )
    });
    order.into_iter().map(|waiter| waiter.ticket).collect()
}

/// Upper-bound seconds until a waiter at `position` can start, assuming each
/// holder ends by its declared budget, the budget stays fixed, and no
/// higher-ranked waiter arrives. `None` when too few holders declared budgets.
pub fn eta_upper_bound_seconds(
    remaining: &[Option<Duration>],
    position: usize,
    budget: usize,
) -> Option<u64> {
    let releases = (remaining.len() + position).saturating_sub(budget);
    if releases == 0 {
        return Some(0);
    }
    let mut known = remaining.iter().flatten().copied().collect::<Vec<_>>();
    known.sort();
    known
        .get(releases - 1)
        .map(|left| left.as_secs() + u64::from(left.subsec_nanos() > 0))
}

/// What a waiting step reports about its place in the queue.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct QueueStanding {
    pub queue_position: usize,
    pub waiters_ahead: usize,
    pub active_slots: usize,
    pub priority: LabPriority,
    pub owner_slot_cap: usize,
    /// Upper-bound estimate from holders' declared budgets; see
    /// [`eta_upper_bound_seconds`].
    pub eta_seconds: Option<u64>,
}

#[derive(Debug, Clone)]
struct Holder {
    owner: String,
    priority: LabPriority,
    started: Instant,
    budget: Option<Duration>,
}

impl Holder {
    fn remaining(&self) -> Option<Duration> {
        self.budget
            .map(|budget| budget.saturating_sub(self.started.elapsed()))
    }
}

/// Queue and occupied slots share a lock so ranking and admission are atomic.
#[derive(Default)]
struct StepState {
    slots: Vec<Option<Holder>>,
    queue: Vec<(LabWaiter, Instant)>,
    last_budget: Option<usize>,
}

impl StepState {
    fn active_slots_by_owner(&self) -> BTreeMap<String, usize> {
        let mut counts = BTreeMap::new();
        for holder in self.slots.iter().flatten() {
            *counts.entry(holder.owner.clone()).or_default() += 1;
        }
        counts
    }

    fn waiters(&self) -> Vec<LabWaiter> {
        self.queue
            .iter()
            .map(|(waiter, _)| waiter.clone())
            .collect()
    }

    fn remaining(&self) -> Vec<Option<Duration>> {
        self.slots.iter().flatten().map(Holder::remaining).collect()
    }
}

static STEP_STATE: std::sync::Mutex<StepState> = std::sync::Mutex::new(StepState {
    slots: Vec::new(),
    queue: Vec::new(),
    last_budget: None,
});
static NEXT_TICKET: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// Holders and ranked waiters for the daemon `/capacity` view.
pub fn step_queue_snapshot() -> serde_json::Value {
    // `observe` takes the state lock itself.
    let fallback = observe(None).effective_budget;
    let state = STEP_STATE.lock().unwrap_or_else(|e| e.into_inner());
    let budget = state.last_budget.unwrap_or(fallback);
    let share = owner_max_share_percent();
    let cap = owner_slot_cap(budget, share);
    let held = state.active_slots_by_owner();
    let remaining = state.remaining();
    let holders = state
        .slots
        .iter()
        .flatten()
        .map(|holder| {
            serde_json::json!({
                "owner": holder.owner, "priority": holder.priority,
                "elapsed_seconds": holder.started.elapsed().as_secs(),
                "budget_seconds": holder.budget.map(|budget| budget.as_secs()),
            })
        })
        .collect::<Vec<_>>();
    let waiting = admission_order(&state.waiters(), &held, cap)
        .into_iter()
        .enumerate()
        .filter_map(|(index, ticket)| {
            let (waiter, queued) = state.queue.iter().find(|(w, _)| w.ticket == ticket)?;
            Some(serde_json::json!({
                "queue_position": index + 1, "owner": waiter.owner, "priority": waiter.priority,
                "waited_seconds": queued.elapsed().as_secs(),
                "eta_seconds": eta_upper_bound_seconds(&remaining, index + 1, budget),
            }))
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "effective_budget": budget, "owner_max_share_percent": share, "owner_slot_cap": cap,
        "holders": holders, "waiting": waiting, "eta_kind": "upper_bound_from_declared_budgets",
    })
}

/// Controller-side waiters for one runner's eligibility, ranked by lane then
/// arrival. Only a strictly higher lane holds back a waiter.
static RUNNER_WAITERS: std::sync::Mutex<Vec<(usize, String, LabPriority)>> =
    std::sync::Mutex::new(Vec::new());

/// Place in a runner-eligibility wait; dropping it leaves the queue.
pub struct RunnerWaitTicket {
    ticket: usize,
    runner: String,
    priority: LabPriority,
}

/// Standing of a runner-eligibility waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RunnerWaitStanding {
    pub queue_position: usize,
    pub waiters_ahead: usize,
    /// No higher-lane waiter for the same runner is registered.
    pub may_proceed: bool,
}

pub fn join_runner_wait(runner: &str, priority: LabPriority) -> RunnerWaitTicket {
    let ticket = NEXT_TICKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    RUNNER_WAITERS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push((ticket, runner.to_owned(), priority));
    RunnerWaitTicket {
        ticket,
        runner: runner.to_owned(),
        priority,
    }
}

impl RunnerWaitTicket {
    pub fn priority(&self) -> LabPriority {
        self.priority
    }

    pub fn standing(&self) -> RunnerWaitStanding {
        let waiters = RUNNER_WAITERS.lock().unwrap_or_else(|e| e.into_inner());
        let same = || {
            waiters
                .iter()
                .filter(|(ticket, runner, _)| *runner == self.runner && *ticket != self.ticket)
        };
        let ahead = same()
            .filter(|(ticket, _, priority)| (*priority, *ticket) < (self.priority, self.ticket))
            .count();
        RunnerWaitStanding {
            queue_position: ahead + 1,
            waiters_ahead: ahead,
            may_proceed: !same().any(|(_, _, priority)| *priority < self.priority),
        }
    }
}

impl Drop for RunnerWaitTicket {
    fn drop(&mut self) {
        RUNNER_WAITERS
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(ticket, _, _)| *ticket != self.ticket);
    }
}

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
    let reserved = gate_reserve(allowed).len();
    cpu_share(
        &allowed[..allowed.len() - reserved],
        limit,
        slot,
        reserved > 0,
    )
}

fn cpu_share(
    allowed: &[usize],
    limit: Option<usize>,
    slot: usize,
    pin_full: bool,
) -> Option<Vec<usize>> {
    if allowed.is_empty() {
        return None;
    }
    if pin_full && limit.unwrap_or(1) <= 1 {
        return Some(allowed.to_vec());
    }
    let limit = limit.filter(|limit| *limit > 1)?;
    let width = (allowed.len() / limit).max(1);
    if width >= allowed.len() {
        return pin_full.then(|| allowed.to_vec());
    }
    let start = (slot % limit) * width % allowed.len();
    Some(allowed[start..(start + width).min(allowed.len())].to_vec())
}

/// Who is asking for a step slot.
#[derive(Debug, Clone, Copy)]
pub struct StepClaim<'a> {
    /// Workflow instance (or run) the step belongs to; the fairness unit.
    pub owner: &'a str,
    pub priority: LabPriority,
    /// Declared run time, used only for waiters' ETA estimates.
    pub budget: Option<Duration>,
}

impl<'a> StepClaim<'a> {
    pub fn owner(owner: &'a str) -> Self {
        Self {
            owner,
            priority: LabPriority::Normal,
            budget: None,
        }
    }
}

/// Admit in [`admission_order`]: priority lane, then the least-occupied owner,
/// then arrival, with an owner share cap while others wait.
pub fn acquire_step(
    limit: Option<usize>,
    claim: StepClaim<'_>,
    waiting: impl FnMut(&QueueStanding, &LabCapacity) -> crate::Result<bool>,
) -> crate::Result<Option<StepPermit>> {
    acquire_step_with_observation(limit, claim, waiting, observe)
}

fn acquire_step_with_observation(
    limit: Option<usize>,
    claim: StepClaim<'_>,
    mut waiting: impl FnMut(&QueueStanding, &LabCapacity) -> crate::Result<bool>,
    observation: impl Fn(Option<usize>) -> LabCapacity,
) -> crate::Result<Option<StepPermit>> {
    use std::sync::atomic::Ordering;
    let ticket = NEXT_TICKET.fetch_add(1, Ordering::Relaxed);
    STEP_STATE
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .queue
        .push((
            LabWaiter {
                ticket,
                owner: claim.owner.to_owned(),
                priority: claim.priority,
            },
            Instant::now(),
        ));
    struct Queued(usize);
    impl Drop for Queued {
        fn drop(&mut self) {
            STEP_STATE
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .queue
                .retain(|(waiter, _)| waiter.ticket != self.0);
        }
    }
    let _queued = Queued(ticket);
    let share = owner_max_share_percent();
    loop {
        let mut observation = observation(limit);
        let standing = {
            let mut state = STEP_STATE.lock().unwrap_or_else(|e| e.into_inner());
            let budget = observation.effective_budget;
            state.last_budget = Some(budget);
            let counts = state.active_slots_by_owner();
            observation.active_slots_by_owner = counts.clone();
            let cap = owner_slot_cap(budget, share);
            let position = 1 + admission_order(&state.waiters(), &counts, cap)
                .iter()
                .position(|candidate| *candidate == ticket)
                .unwrap_or(0);
            let active = state.slots.iter().flatten().count();
            if position == 1 && active < budget {
                let holder = Holder {
                    owner: claim.owner.to_owned(),
                    priority: claim.priority,
                    started: Instant::now(),
                    budget: claim.budget,
                };
                let slot = state
                    .slots
                    .iter()
                    .position(Option::is_none)
                    .unwrap_or(state.slots.len());
                if slot == state.slots.len() {
                    state.slots.push(Some(holder));
                } else {
                    state.slots[slot] = Some(holder);
                }
                state.queue.retain(|(waiter, _)| waiter.ticket != ticket);
                return Ok(Some(StepPermit { slot }));
            }
            QueueStanding {
                queue_position: position,
                waiters_ahead: position - 1,
                active_slots: active,
                priority: claim.priority,
                owner_slot_cap: cap,
                eta_seconds: eta_upper_bound_seconds(&state.remaining(), position, budget),
            }
        };
        if !waiting(&standing, &observation)? {
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
            let a = StepClaim::owner("A");
            let first = acquire_step_with_observation(Some(2), a, |_, _| Ok(true), observation)
                .unwrap()
                .unwrap();
            let second = acquire_step_with_observation(Some(2), a, |_, _| Ok(true), observation)
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
                            a,
                            |standing, capacity| {
                                if !reported {
                                    assert_eq!(standing.queue_position, expected_position);
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
                        StepClaim::owner("B"),
                        |standing, _| {
                            if !reported {
                                assert_eq!(standing.queue_position, 1);
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
    fn admission_order_table() {
        use LabPriority::{High, Normal};
        let waiter = |ticket, owner: &str, priority| LabWaiter {
            ticket,
            owner: owner.into(),
            priority,
        };
        let held = |pairs: &[(&str, usize)]| {
            pairs
                .iter()
                .map(|(owner, count)| (owner.to_string(), *count))
                .collect::<BTreeMap<_, _>>()
        };
        for (name, waiters, held, cap, expected) in [
            (
                "high lane beats an earlier normal waiter",
                vec![waiter(1, "wf-a", Normal), waiter(2, "wf-b", High)],
                held(&[]),
                4,
                vec![2, 1],
            ),
            (
                "fifo within a lane",
                vec![waiter(1, "wf-a", High), waiter(2, "wf-b", High)],
                held(&[]),
                4,
                vec![1, 2],
            ),
            (
                "over-share owner deferred while another owner waits",
                vec![waiter(1, "fanout", High), waiter(2, "publish", Normal)],
                held(&[("fanout", 2)]),
                2,
                vec![2, 1],
            ),
            (
                "over-share owner admitted when nobody else waits",
                vec![waiter(1, "fanout", Normal), waiter(2, "fanout", Normal)],
                held(&[("fanout", 4)]),
                2,
                vec![1, 2],
            ),
            (
                "least-occupied owner first within a lane",
                vec![waiter(1, "wf-a", Normal), waiter(2, "wf-b", Normal)],
                held(&[("wf-a", 1)]),
                4,
                vec![2, 1],
            ),
        ] {
            assert_eq!(admission_order(&waiters, &held, cap), expected, "{name}");
        }
        assert_eq!(owner_slot_cap(32, 50), 16);
        assert_eq!(owner_slot_cap(1, 50), 1);
    }

    #[test]
    fn eta_upper_bound_table() {
        let secs = |s| Some(Duration::from_secs(s));
        for (remaining, position, budget, expected) in [
            (vec![secs(60), secs(10)], 1, 2, Some(10)),
            (vec![secs(60), secs(10)], 2, 2, Some(60)),
            (vec![secs(60), None], 1, 2, Some(60)),
            (vec![None, None], 1, 2, None),
            (vec![secs(60)], 1, 2, Some(0)),
            (vec![secs(5), secs(5), secs(30)], 1, 2, Some(5)),
        ] {
            assert_eq!(
                eta_upper_bound_seconds(&remaining, position, budget),
                expected,
                "{remaining:?} position={position} budget={budget}"
            );
        }
    }

    #[test]
    fn runner_wait_high_lane_holds_back_normal_waiters() {
        let normal = join_runner_wait("lab-priority-test", LabPriority::Normal);
        assert!(normal.standing().may_proceed);
        let high = join_runner_wait("lab-priority-test", LabPriority::High);
        let other = join_runner_wait("lab-other-test", LabPriority::High);
        assert_eq!(
            normal.standing(),
            RunnerWaitStanding {
                queue_position: 2,
                waiters_ahead: 1,
                may_proceed: false
            }
        );
        assert_eq!(high.standing().queue_position, 1);
        assert!(high.standing().may_proceed);
        drop((high, other));
        assert!(normal.standing().may_proceed);
    }

    #[test]
    fn cpu_slices_are_disjoint_shares() {
        crate::test_support::with_isolated_home(|_| {
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
        });
    }

    /// A pinned step's tree sees only its share: `nproc` sizes to the slice.
    #[test]
    #[cfg(target_os = "linux")]
    fn pinned_child_nproc_reports_its_slice() {
        crate::test_support::with_isolated_home(|_| {
            let allowed = allowed_cpus();
            let mut slices = cpu_slice(&allowed, Some(2), 1)
                .into_iter()
                .collect::<Vec<_>>();
            if allowed.len() >= 5 {
                std::env::set_var("HOMEBOY_LAB_GATE_RESERVE", "4");
                let reserve = gate_reserve(&allowed);
                assert_eq!(gate_slots(&reserve), 1);
                std::env::set_var("HOMEBOY_LAB_GATE_SLOTS", "0");
                assert_eq!(gate_slots(&reserve), 1);
                std::env::set_var("HOMEBOY_LAB_GATE_SLOTS", "99");
                assert_eq!(gate_slots(&reserve), 4);
                std::env::set_var("HOMEBOY_LAB_GATE_SLOTS", "2");
                let first = gate_cpu_slice(&reserve, 0);
                let second = gate_cpu_slice(&reserve, 1);
                assert_eq!(first.len(), 2);
                assert_eq!(second.len(), 2);
                assert!(first.iter().all(|cpu| !second.contains(cpu)));
                slices.extend([first, second]);
                std::env::remove_var("HOMEBOY_LAB_GATE_RESERVE");
                std::env::remove_var("HOMEBOY_LAB_GATE_SLOTS");
            }
            for slice in slices {
                let mut command = std::process::Command::new("sh");
                command.args(["-c", "nproc"]);
                pin_command(&mut command, &slice);
                let output = command.output().expect("run nproc");
                assert!(output.status.success());
                assert_eq!(
                    String::from_utf8_lossy(&output.stdout).trim(),
                    slice.len().to_string()
                );
            }
        });
    }

    #[test]
    fn reserve_excludes_cpus_even_without_a_step_limit() {
        crate::test_support::with_isolated_home(|_| {
            std::env::set_var("HOMEBOY_LAB_GATE_RESERVE", "2");
            let allowed = vec![1, 3, 5, 7, 9];
            for limit in [None, Some(1), Some(2), Some(16)] {
                for slot in 0..16 {
                    let slice = cpu_slice(&allowed, limit, slot).unwrap();
                    assert!(!slice.is_empty());
                    assert!(slice.iter().all(|cpu| ![7, 9].contains(cpu)));
                }
            }
            std::env::remove_var("HOMEBOY_LAB_GATE_RESERVE");
        });
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
