//! What one job, workflow step or Cook phase consumed, recorded when it ends.
//!
//! The kernel keeps exact accounting for a runner job in its own cgroup
//! (`cpu.stat`, `memory.peak`, `io.stat`). Work without one is measured from
//! `getrusage` deltas around the phase and marked `estimated`: the calling
//! thread's own CPU is Homeboy's (`homeboy_cpu_usec`), and the CPU of children
//! reaped meanwhile is the workload's. Reaped-child CPU is process-wide, so
//! phases running at once on other threads can share it.
//!
//! Phases nest. Each [`UsageScope`] records its *self* usage: CPU a nested
//! scope on the same thread measured is subtracted from its parent, so the
//! usage of every span in a run sums without double counting.

use std::cell::RefCell;
use std::time::Instant;

use serde::{Deserialize, Serialize};

/// Usage read from the job's own cgroup: exact.
pub const USAGE_SOURCE_JOB_CGROUP: &str = "job_cgroup";
/// Usage summed over the process tree's `/proc` totals at finish: estimated.
pub const USAGE_SOURCE_PROCESS_TREE: &str = "process_tree";
/// Usage from `getrusage` deltas around a phase: estimated.
pub const USAGE_SOURCE_RUSAGE: &str = "rusage";

/// The usage block one step or phase records on its run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceUsage {
    /// `cpu.stat` `usage_usec`: user plus system CPU.
    pub cpu_usage_usec: u64,
    /// `cpu.stat` `user_usec`.
    pub cpu_user_usec: u64,
    /// `cpu.stat` `system_usec`.
    pub cpu_system_usec: u64,
    /// The part of `cpu_usage_usec` Homeboy itself spent (daemon, controller),
    /// as opposed to the workload it ran.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub homeboy_cpu_usec: u64,
    pub wall_ms: u64,
    /// `memory.peak`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory_peak_bytes: Option<u64>,
    /// `io.stat` `rbytes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_read_bytes: Option<u64>,
    /// `io.stat` `wbytes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub io_write_bytes: Option<u64>,
    /// [`USAGE_SOURCE_JOB_CGROUP`], [`USAGE_SOURCE_PROCESS_TREE`] or
    /// [`USAGE_SOURCE_RUSAGE`].
    pub source: String,
    /// Not the kernel's exact per-job accounting.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

fn is_zero(value: &u64) -> bool {
    *value == 0
}

impl ResourceUsage {
    /// Usage of a process tree from its user and system CPU totals at finish.
    pub fn from_process_tree(cpu_user_usec: u64, cpu_system_usec: u64, wall_ms: u64) -> Self {
        Self {
            cpu_usage_usec: cpu_user_usec.saturating_add(cpu_system_usec),
            cpu_user_usec,
            cpu_system_usec,
            wall_ms,
            source: USAGE_SOURCE_PROCESS_TREE.to_string(),
            estimated: true,
            ..Self::default()
        }
    }
}

/// CPU in microseconds: the calling thread's own and its reaped children's.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct CpuTimes {
    own_user: u64,
    own_system: u64,
    children_user: u64,
    children_system: u64,
}

impl CpuTimes {
    fn since(self, start: CpuTimes) -> CpuTimes {
        CpuTimes {
            own_user: self.own_user.saturating_sub(start.own_user),
            own_system: self.own_system.saturating_sub(start.own_system),
            children_user: self.children_user.saturating_sub(start.children_user),
            children_system: self.children_system.saturating_sub(start.children_system),
        }
    }

    fn plus(self, other: CpuTimes) -> CpuTimes {
        CpuTimes {
            own_user: self.own_user.saturating_add(other.own_user),
            own_system: self.own_system.saturating_add(other.own_system),
            children_user: self.children_user.saturating_add(other.children_user),
            children_system: self.children_system.saturating_add(other.children_system),
        }
    }
}

#[cfg(unix)]
fn cpu_times() -> Option<CpuTimes> {
    fn read(who: libc::c_int) -> Option<(u64, u64)> {
        let mut usage = std::mem::MaybeUninit::<libc::rusage>::uninit();
        // SAFETY: getrusage initializes the supplied rusage on success.
        if unsafe { libc::getrusage(who, usage.as_mut_ptr()) } != 0 {
            return None;
        }
        // SAFETY: initialized by the successful call above.
        let usage = unsafe { usage.assume_init() };
        let micros = |time: libc::timeval| {
            (time.tv_sec as u64)
                .saturating_mul(1_000_000)
                .saturating_add(time.tv_usec as u64)
        };
        Some((micros(usage.ru_utime), micros(usage.ru_stime)))
    }
    #[cfg(any(target_os = "linux", target_os = "android"))]
    let own = libc::RUSAGE_THREAD;
    #[cfg(not(any(target_os = "linux", target_os = "android")))]
    let own = libc::RUSAGE_SELF;
    let (own_user, own_system) = read(own)?;
    let (children_user, children_system) = read(libc::RUSAGE_CHILDREN)?;
    Some(CpuTimes {
        own_user,
        own_system,
        children_user,
        children_system,
    })
}

#[cfg(not(unix))]
fn cpu_times() -> Option<CpuTimes> {
    None
}

#[derive(Default)]
struct Frame {
    /// Inclusive CPU of scopes nested in this one.
    nested: CpuTimes,
}

thread_local! {
    static FRAMES: RefCell<Vec<Frame>> = const { RefCell::new(Vec::new()) };
}

/// Measures one phase. Finish it (or drop it) on the thread that opened it.
pub struct UsageScope {
    started: Instant,
    start: Option<CpuTimes>,
    depth: usize,
    open: bool,
}

/// Open a scope nested in any scope already open on this thread.
pub fn usage_scope() -> UsageScope {
    let depth = FRAMES.with(|frames| {
        let mut frames = frames.borrow_mut();
        frames.push(Frame::default());
        frames.len()
    });
    UsageScope {
        started: Instant::now(),
        start: cpu_times(),
        depth,
        open: true,
    }
}

impl UsageScope {
    /// This scope's self usage: its CPU less that of nested scopes.
    pub fn finish(mut self) -> Option<ResourceUsage> {
        self.close(true)
    }

    /// Close without recording; the parent keeps this scope's CPU as its own.
    pub fn discard(mut self) {
        self.close(false);
    }

    fn close(&mut self, record: bool) -> Option<ResourceUsage> {
        if !std::mem::replace(&mut self.open, false) {
            return None;
        }
        let end = cpu_times();
        let frame = FRAMES.with(|frames| {
            let mut frames = frames.borrow_mut();
            // Scopes close in order; one left open inside this one is closed
            // with it.
            let frame = (frames.len() >= self.depth)
                .then(|| frames.drain(self.depth - 1..).next())
                .flatten()
                .unwrap_or_default();
            if let (true, Some(start), Some(end), Some(parent)) =
                (record, self.start, end, frames.last_mut())
            {
                parent.nested = parent.nested.plus(end.since(start));
            }
            frame
        });
        if !record {
            return None;
        }
        let own = end?.since(self.start?).since(frame.nested);
        let user = own.own_user.saturating_add(own.children_user);
        let system = own.own_system.saturating_add(own.children_system);
        Some(ResourceUsage {
            cpu_usage_usec: user.saturating_add(system),
            cpu_user_usec: user,
            cpu_system_usec: system,
            homeboy_cpu_usec: own.own_user.saturating_add(own.own_system),
            wall_ms: self
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            source: USAGE_SOURCE_RUSAGE.to_string(),
            estimated: true,
            ..ResourceUsage::default()
        })
    }
}

impl Drop for UsageScope {
    fn drop(&mut self) {
        self.close(true);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn burn(iterations: u64) -> u64 {
        (0..iterations).fold(0_u64, |acc, value| {
            std::hint::black_box(acc.wrapping_mul(31).wrapping_add(value))
        })
    }

    #[test]
    fn nested_scopes_record_self_usage_without_double_counting() {
        let outer = usage_scope();
        burn(100_000);
        let inner = usage_scope();
        burn(20_000_000);
        let inner = inner.finish().expect("inner usage");
        let outer = outer.finish().expect("outer usage");
        assert!(inner.estimated);
        assert_eq!(inner.source, USAGE_SOURCE_RUSAGE);
        assert!(inner.cpu_usage_usec > outer.cpu_usage_usec);
        assert_eq!(
            inner.cpu_usage_usec,
            inner.cpu_user_usec + inner.cpu_system_usec
        );
        // The thread's own CPU is Homeboy's; reaped children's are not.
        assert!(inner.homeboy_cpu_usec > 0);
        assert!(inner.homeboy_cpu_usec <= inner.cpu_usage_usec);
        assert!(FRAMES.with(|frames| frames.borrow().is_empty()));
    }

    #[test]
    fn dropped_and_discarded_scopes_leave_the_thread_clean() {
        let outer = usage_scope();
        {
            let _left_open = usage_scope();
        }
        usage_scope().discard();
        let _ = usage_scope();
        assert!(outer.finish().is_some());
        assert!(FRAMES.with(|frames| frames.borrow().is_empty()));
    }

    #[test]
    fn usage_wire_omits_unmeasured_fields() {
        let usage = ResourceUsage::from_process_tree(3_000, 1_000, 9);
        let wire = serde_json::to_value(&usage).unwrap();
        assert_eq!(wire["cpu_usage_usec"], 4_000);
        assert_eq!(wire["estimated"], true);
        assert!(wire.get("memory_peak_bytes").is_none());
        assert!(wire.get("homeboy_cpu_usec").is_none());
        assert_eq!(
            serde_json::from_value::<ResourceUsage>(wire).unwrap(),
            usage
        );
    }
}
