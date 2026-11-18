//! Kernel-enforced per-job cgroups (#18418).
//!
//! The runner unit renders `Delegate=yes`, so the daemon owns its service
//! cgroup. At startup it moves itself into the `daemon/` leaf (cgroup v2 keeps
//! processes out of inner nodes) and prepares `jobs/`, whose `memory.max` is
//! the host budget: runner jobs together can never OOM the host, and an OOM
//! stays inside `jobs/`. Each job joins its own `jobs/<job-id>/` cgroup before
//! exec, so no descendant escapes it by changing session or process group, and
//! stopping the job is `cgroup.kill`.
//!
//! When delegation is unavailable the runner keeps the sampled process-tree
//! guard.

use std::fs;
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;
use std::time::{Duration, Instant};

const CONTROLLERS: &str = "+memory +pids";
const DAEMON_LEAF: &str = "daemon";
const JOBS_DIR: &str = "jobs";
const DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const DRAIN_POLL: Duration = Duration::from_millis(10);
/// The `pids` controller counts tasks (threads), while the guard limit counts
/// processes, and a headless browser or Node process holds 10 to 30 threads.
/// `pids.max` is a fork-bomb backstop at this many tasks per allowed process;
/// the sampler enforces the process count itself.
pub(crate) const TASKS_PER_PROCESS: u64 = 32;

static DELEGATION: OnceLock<Result<PathBuf, String>> = OnceLock::new();

/// Move this daemon into its `daemon/` leaf and prepare `jobs/`, once per
/// process. Returns the `jobs/` directory, or why runner jobs fall back to the
/// sampled process-tree guard.
pub(crate) fn initialize(jobs_memory_max: u64) -> Result<&'static Path, String> {
    DELEGATION
        .get_or_init(|| delegate(jobs_memory_max))
        .as_deref()
        .map_err(String::clone)
}

fn jobs_dir() -> Option<&'static Path> {
    DELEGATION.get()?.as_deref().ok()
}

fn delegate(jobs_memory_max: u64) -> Result<PathBuf, String> {
    let own = own_cgroup_dir()
        .ok_or("cgroup v2 is not mounted or this process's cgroup is unreadable")?;
    let service = if own.file_name() == Some(DAEMON_LEAF.as_ref()) {
        own.parent().map(Path::to_path_buf).unwrap_or_default()
    } else {
        own.clone()
    };
    let controllers = read(&service.join("cgroup.controllers"))?;
    for controller in ["memory", "pids"] {
        if !controllers
            .split_whitespace()
            .any(|name| name == controller)
        {
            return Err(format!(
                "the {controller} controller is not delegated to {}",
                service.display()
            ));
        }
    }
    let leaf = service.join(DAEMON_LEAF);
    if own != leaf {
        let me = std::process::id().to_string();
        if read(&service.join("cgroup.procs"))?
            .lines()
            .any(|pid| pid.trim() != me)
        {
            return Err(format!(
                "{} holds processes other than the daemon, so it is not a delegated service cgroup",
                service.display()
            ));
        }
        create_dir(&leaf)?;
        write(&leaf.join("cgroup.procs"), &me)?;
    }
    write(&service.join("cgroup.subtree_control"), CONTROLLERS)?;
    let jobs = service.join(JOBS_DIR);
    create_dir(&jobs)?;
    write(&jobs.join("cgroup.subtree_control"), CONTROLLERS)?;
    write(&jobs.join("memory.max"), &limit_value(jobs_memory_max))?;
    match fs::write(jobs.join("memory.swap.max"), "0") {
        Err(error) if error.kind() != io::ErrorKind::NotFound => {
            return Err(format!(
                "write {}: {error}",
                jobs.join("memory.swap.max").display()
            ))
        }
        _ => {}
    }
    // Remove empty job cgroups a previous daemon left; populated ones belong
    // to jobs that outlived it and refuse removal.
    for entry in fs::read_dir(&jobs).into_iter().flatten().flatten() {
        if entry.path().is_dir() {
            let _ = fs::remove_dir(entry.path());
        }
    }
    Ok(jobs)
}

fn own_cgroup_dir() -> Option<PathBuf> {
    let membership =
        super::cgroup_v2_membership_path(&fs::read_to_string("/proc/self/cgroup").ok()?)?;
    let (mount_root, mount_point) =
        super::cgroup_v2_mount(&fs::read_to_string("/proc/self/mountinfo").ok()?)?;
    Some(Path::new(&mount_point).join(super::cgroup_relative_to_mount(&membership, &mount_root)?))
}

fn read(path: &Path) -> Result<String, String> {
    fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))
}

fn write(path: &Path, value: &str) -> Result<(), String> {
    fs::write(path, value).map_err(|error| format!("write {}: {error}", path.display()))
}

fn create_dir(path: &Path) -> Result<(), String> {
    match fs::create_dir(path) {
        Err(error) if error.kind() != io::ErrorKind::AlreadyExists => {
            Err(format!("create {}: {error}", path.display()))
        }
        _ => Ok(()),
    }
}

/// A limit of `0` disables the guard, so the kernel limit is `max`.
fn limit_value(limit: u64) -> String {
    if limit == 0 {
        "max".to_string()
    } else {
        limit.to_string()
    }
}

/// Kernel counters of one job cgroup.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct JobCgroupCounters {
    pub memory_limit: Option<u64>,
    pub memory_current: Option<u64>,
    pub memory_peak: Option<u64>,
    pub oom_kill: Option<u64>,
    pub pids_limit: Option<u64>,
    pub pids_current: Option<u64>,
    pub pids_peak: Option<u64>,
    pub pids_max_events: Option<u64>,
}

/// One runner job's own cgroup. Dropping it kills and removes the cgroup.
pub(crate) struct JobCgroup {
    dir: PathBuf,
    procs: fs::File,
    stopped: AtomicBool,
}

impl JobCgroup {
    /// Create `jobs/<job-id>/`, or `None` when this daemon has no delegated
    /// cgroup subtree. `jobs/` admits `concurrency` jobs at the per-job
    /// process limit.
    pub(crate) fn create(
        job_id: Option<&str>,
        process_count_limit: u64,
        concurrency: u64,
    ) -> io::Result<Option<Self>> {
        let Some(jobs) = jobs_dir() else {
            return Ok(None);
        };
        fs::write(
            jobs.join("pids.max"),
            limit_value(task_limit(process_count_limit).saturating_mul(concurrency.max(1))),
        )?;
        let name = job_id
            .filter(|id| {
                !id.is_empty()
                    && id
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
            })
            .map(str::to_string)
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let mut dir = jobs.join(&name);
        if let Err(error) = fs::create_dir(&dir) {
            if error.kind() != io::ErrorKind::AlreadyExists {
                return Err(error);
            }
            dir = jobs.join(format!("{name}-{}", uuid::Uuid::new_v4()));
            fs::create_dir(&dir)?;
        }
        let configured = (|| {
            fs::write(
                dir.join("pids.max"),
                limit_value(task_limit(process_count_limit)),
            )?;
            fs::write(dir.join("memory.oom.group"), "1")?;
            // std opens with O_CLOEXEC, so the job never inherits it.
            fs::OpenOptions::new()
                .write(true)
                .open(dir.join("cgroup.procs"))
        })();
        match configured {
            Ok(procs) => Ok(Some(Self {
                dir,
                procs,
                stopped: AtomicBool::new(false),
            })),
            Err(error) => {
                let _ = fs::remove_dir(&dir);
                Err(error)
            }
        }
    }

    /// Make the spawned child join this cgroup between fork and exec, so the
    /// job and every descendant are members from their first instruction.
    pub(crate) fn join_before_exec(&self, command: &mut Command) {
        let fd = self.procs.as_raw_fd();
        // SAFETY: only async-signal-safe write(2) runs between fork and exec;
        // writing `0` to `cgroup.procs` migrates the writing process.
        unsafe {
            command.pre_exec(move || {
                if libc::write(fd, b"0".as_ptr().cast(), 1) == 1 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
    }

    pub(crate) fn dir(&self) -> &Path {
        &self.dir
    }

    /// SIGKILL every member at once; the kernel refuses forks that race it.
    pub(crate) fn kill(&self) {
        if fs::write(self.dir.join("cgroup.kill"), "1").is_err() {
            for pid in self.members() {
                // SAFETY: plain signal delivery to a member pid.
                unsafe {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }

    /// Kill every member, wait until the cgroup is empty, and remove it.
    pub(crate) fn stop(&self) -> io::Result<()> {
        if self.stopped.swap(true, Ordering::SeqCst) {
            return Ok(());
        }
        let deadline = Instant::now() + DRAIN_TIMEOUT;
        loop {
            self.kill();
            if !self.populated() {
                break;
            }
            if Instant::now() >= deadline {
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "job cgroup {} stayed populated after cgroup.kill",
                        self.dir.display()
                    ),
                ));
            }
            std::thread::sleep(DRAIN_POLL);
        }
        loop {
            match fs::remove_dir(&self.dir) {
                Ok(()) => return Ok(()),
                Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
                Err(error) if Instant::now() >= deadline => return Err(error),
                Err(_) => std::thread::sleep(DRAIN_POLL),
            }
        }
    }

    fn populated(&self) -> bool {
        fs::read_to_string(self.dir.join("cgroup.events")).is_ok_and(|events| {
            event_count(&events, "populated").is_some_and(|populated| populated > 0)
        })
    }

    /// Every process in the job, whatever its session or process group.
    pub(crate) fn members(&self) -> Vec<i32> {
        fs::read_to_string(self.dir.join("cgroup.procs"))
            .unwrap_or_default()
            .lines()
            .filter_map(|pid| pid.trim().parse().ok())
            .collect()
    }

    pub(crate) fn counters(&self) -> JobCgroupCounters {
        let value = |name: &str| read_u64(&self.dir.join(name));
        let memory_events = fs::read_to_string(self.dir.join("memory.events")).ok();
        let pids_events = fs::read_to_string(self.dir.join("pids.events")).ok();
        JobCgroupCounters {
            // The budget is enforced on `jobs/`, the job's parent.
            memory_limit: self
                .dir
                .parent()
                .and_then(|jobs| read_u64(&jobs.join("memory.max"))),
            memory_current: value("memory.current"),
            memory_peak: value("memory.peak"),
            oom_kill: memory_events
                .as_deref()
                .and_then(|events| event_count(events, "oom_kill")),
            pids_limit: value("pids.max"),
            pids_current: value("pids.current"),
            pids_peak: value("pids.peak"),
            pids_max_events: pids_events
                .as_deref()
                .and_then(|events| event_count(events, "max")),
        }
    }
}

impl Drop for JobCgroup {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

fn task_limit(process_count_limit: u64) -> u64 {
    process_count_limit.saturating_mul(TASKS_PER_PROCESS)
}

fn read_u64(path: &Path) -> Option<u64> {
    fs::read_to_string(path).ok()?.trim().parse().ok()
}

fn event_count(events: &str, name: &str) -> Option<u64> {
    events.lines().find_map(|line| {
        let mut fields = line.split_whitespace();
        (fields.next() == Some(name))
            .then(|| fields.next()?.parse().ok())
            .flatten()
    })
}

#[cfg(test)]
#[path = "job_cgroup_tests.rs"]
mod tests;
