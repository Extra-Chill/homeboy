//! Command execution primitives with consistent error handling.

use std::io::{self, Read, Write};
#[cfg(unix)]
use std::os::fd::RawFd;
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const DEFAULT_CAPTURE_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const MAX_OBSERVED_LINE_BYTES: usize = 64 * 1024;
#[cfg(all(unix, not(target_os = "linux")))]
type AdoptedGuard = Option<(u32, Option<u32>, u64)>;
#[cfg(unix)]
const PROCESS_TREE_TERM_GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const PROCESS_TREE_KILL_GRACE: Duration = Duration::from_secs(2);
#[cfg(target_os = "linux")]
const CONTROLLER_LOSS_TERM_GRACE: Duration = Duration::from_millis(200);
#[cfg(target_os = "linux")]
const CONTROLLER_LOSS_KILL_GRACE: Duration = Duration::from_millis(400);
#[cfg(any(unix, windows))]
const PROCESS_TREE_POLL_INTERVAL: Duration = Duration::from_millis(25);
const CAPTURE_JOIN_GRACE: Duration = Duration::from_secs(2);
const PROCESS_TREE_CLEANUP_DEADLINE: Duration = Duration::from_secs(4);

#[cfg(target_os = "linux")]
static SUPERVISOR_TEARDOWN: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(0);

pub type StdoutLineObserver = Arc<dyn Fn(&str) + Send + Sync + 'static>;
pub type StreamChunkObserver = Arc<dyn Fn(&[u8], bool) + Send + Sync + 'static>;

/// Stream a supervised child's chunks to the matching parent stream.
pub fn parent_stream_passthrough() -> StreamChunkObserver {
    Arc::new(|chunk, stderr| {
        if stderr {
            let mut sink = io::stderr();
            let _ = sink.write_all(chunk);
            let _ = sink.flush();
        } else {
            let mut sink = io::stdout();
            let _ = sink.write_all(chunk);
            let _ = sink.flush();
        }
    })
}

/// Whether this build can isolate a spawned command into a terminable process
/// tree. Callers that require fail-closed child identity persistence must check
/// this before spawning.
pub const fn supports_process_tree_isolation() -> bool {
    cfg!(any(unix, windows))
}

/// Whether a PID still names a process that can execute work.
///
/// Linux retains an exited process as a zombie until its parent reaps it, and
/// `kill(pid, 0)` still succeeds during that interval. Zombies cannot execute
/// work, so lifecycle supervision and descendant cleanup treat them as exited.
pub fn process_is_running(pid: u32) -> bool {
    if pid > i32::MAX as u32 {
        return false;
    }

    #[cfg(target_os = "linux")]
    if let Some(state) = linux_process_state(pid) {
        return state != 'Z';
    }

    #[cfg(unix)]
    unsafe {
        libc::kill(pid as libc::pid_t, 0) == 0
    }

    #[cfg(not(unix))]
    {
        pid == std::process::id()
    }
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct LinuxProcStat {
    state: char,
    parent_pid: u32,
    starttime_ticks: u64,
}

#[cfg(target_os = "linux")]
fn linux_process_stat(pid: u32) -> Option<LinuxProcStat> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_linux_proc_stat(&stat)
}

#[cfg(target_os = "linux")]
fn parse_linux_proc_stat(stat: &str) -> Option<LinuxProcStat> {
    let after_command = stat.rsplit_once(") ")?.1;
    let mut fields = after_command.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent_pid = fields.next()?.parse().ok()?;
    let starttime_ticks = after_command.split_whitespace().nth(19)?.parse().ok()?;
    Some(LinuxProcStat {
        state,
        parent_pid,
        starttime_ticks,
    })
}

#[cfg(target_os = "linux")]
fn linux_process_state(pid: u32) -> Option<char> {
    linux_process_stat(pid).map(|stat| stat.state)
}

/// Keeps an isolated child tree owned by the controller that spawned it.
///
/// On Linux, the spawned child is an execution-scoped supervisor: it is the
/// parent and subreaper for only that command, waits the root separately, and
/// reaps every descendant — including never-observed adopted zombies — without
/// consuming unrelated `Child` statuses. Controller exit closes a pipe whose
/// write end is held only by this guard; the supervisor then tears the tree
/// down. Other Unix platforms keep a sibling death watcher over the isolated
/// process group. Windows uses a kill-on-close job object.
pub struct ControllerChildGuard {
    #[cfg(unix)]
    controller_liveness_read_fd: RawFd,
    #[cfg(unix)]
    controller_liveness_fd: RawFd,
    #[cfg(unix)]
    child_registration_read_fd: RawFd,
    #[cfg(unix)]
    child_registration_fd: RawFd,
    #[cfg(all(unix, not(target_os = "linux")))]
    owned_processes: Mutex<Vec<UnixProcessIdentity>>,
    #[cfg(windows)]
    job: Mutex<windows_sys::Win32::Foundation::HANDLE>,
}

impl ControllerChildGuard {
    /// Configure a command so its complete process tree dies if this guard's
    /// controller process exits, then retain the returned guard while waiting.
    pub fn prepare(command: &mut Command) -> io::Result<Self> {
        #[cfg(unix)]
        {
            let mut fds = [-1; 2];
            if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error());
            }
            for fd in fds {
                if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                    let error = io::Error::last_os_error();
                    unsafe {
                        libc::close(fds[0]);
                        libc::close(fds[1]);
                    }
                    return Err(error);
                }
            }
            #[cfg(target_os = "linux")]
            {
                configure_isolated_process_tree(command, -1, fds[0]);
                return Ok(Self {
                    controller_liveness_read_fd: fds[0],
                    controller_liveness_fd: fds[1],
                    child_registration_read_fd: -1,
                    child_registration_fd: -1,
                });
            }
            #[cfg(not(target_os = "linux"))]
            {
                let mut registration_fds = [-1; 2];
                if unsafe { libc::pipe(registration_fds.as_mut_ptr()) } != 0 {
                    let error = io::Error::last_os_error();
                    unsafe {
                        libc::close(fds[0]);
                        libc::close(fds[1]);
                    }
                    return Err(error);
                }
                for fd in registration_fds {
                    if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                        let error = io::Error::last_os_error();
                        unsafe {
                            libc::close(fds[0]);
                            libc::close(fds[1]);
                            libc::close(registration_fds[0]);
                            libc::close(registration_fds[1]);
                        }
                        return Err(error);
                    }
                }
                // The child reports its PID from pre_exec, after it became its own
                // process-group leader but before it can execute workload code.
                // That lets the already-running guard cover spawn-to-attach.
                configure_isolated_process_tree(command, registration_fds[1], -1);
                let guard = Self {
                    controller_liveness_read_fd: fds[0],
                    controller_liveness_fd: fds[1],
                    child_registration_read_fd: registration_fds[0],
                    child_registration_fd: registration_fds[1],
                    owned_processes: Mutex::new(Vec::new()),
                };
                match unsafe { libc::fork() } {
                    -1 => Err(io::Error::last_os_error()),
                    0 => controller_death_guard_loop(
                        guard.controller_liveness_read_fd,
                        guard.controller_liveness_fd,
                        guard.child_registration_read_fd,
                        guard.child_registration_fd,
                    ),
                    _guard_pid => Ok(guard),
                }
            }
        }

        #[cfg(not(unix))]
        {
            isolate_process_tree(command);
            Ok(Self {
                #[cfg(windows)]
                job: Mutex::new(std::ptr::null_mut()),
            })
        }
    }

    /// Record the child after spawn. The guard already runs from `prepare`, so
    /// controller death during this handoff cannot leave workload descendants.
    pub fn attach(&self, child: &Child) -> io::Result<()> {
        #[cfg(all(unix, not(target_os = "linux")))]
        {
            self.observe_owned_processes(child.id())?;
            Ok(())
        }

        #[cfg(target_os = "linux")]
        {
            let _ = child;
            Ok(())
        }

        #[cfg(not(unix))]
        {
            #[cfg(windows)]
            return assign_windows_kill_on_close_job(child, &self.job);

            #[cfg(not(windows))]
            {
                let _ = child;
                Ok(())
            }
        }
    }

    fn terminate_and_reap(
        &mut self,
        child: &mut Child,
        deadline: std::time::Instant,
    ) -> io::Result<ExitStatus> {
        #[cfg(target_os = "linux")]
        {
            self.close_liveness_write();
            signal_pid(child.id(), libc::SIGTERM)?;
            match reap_child_until(child, deadline) {
                Ok(status) => Ok(status),
                Err(_) => {
                    signal_pid(child.id(), libc::SIGKILL)?;
                    reap_child_until(child, deadline)
                }
            }
        }

        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let _ = deadline;
            self.observe_owned_processes(child.id())?;
            terminate_owned_processes_and_reap(child, &self.owned_processes, self.adopted_guard()?)
        }

        #[cfg(windows)]
        {
            self.close_windows_job(true, deadline)?;
            return reap_child_until(child, deadline);
        }
        #[cfg(not(any(unix, windows)))]
        terminate_process_tree_and_reap(child)
    }

    /// Synchronously clean up after a supervision infrastructure error before
    /// the caller releases mutation ownership.
    pub fn terminate_and_reap_bounded(&mut self, child: &mut Child) -> io::Result<ExitStatus> {
        let deadline = std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE;
        match self.terminate_and_reap(child, deadline) {
            Ok(status) => Ok(status),
            Err(primary) => {
                #[cfg(all(unix, not(target_os = "linux")))]
                let _ = signal_process_group(child.id(), libc::SIGKILL);
                #[cfg(target_os = "linux")]
                self.close_liveness_write();
                let _ = child.kill();
                reap_child_until(child, deadline).map_err(|fallback| {
                    io::Error::other(format!(
                        "{primary}; direct process-group fallback also failed: {fallback}"
                    ))
                })
            }
        }
    }

    fn close_after_root_exit(
        &mut self,
        root_pid: u32,
        deadline: std::time::Instant,
    ) -> io::Result<()> {
        #[cfg(target_os = "linux")]
        {
            let _ = (root_pid, deadline);
            Ok(())
        }

        #[cfg(all(unix, not(target_os = "linux")))]
        {
            let _ = deadline;
            self.observe_owned_processes(root_pid)?;
            // A short-lived root can spawn and exit between identity snapshots.
            // Its ordinary descendants remain in the isolated process group;
            // drain that group before handling tracked session escapees.
            terminate_remaining_process_group(root_pid)?;
            terminate_owned_processes_after_root_exit(
                root_pid,
                &self.owned_processes,
                self.adopted_guard()?,
            )
        }

        #[cfg(not(unix))]
        {
            let _ = root_pid;
            #[cfg(windows)]
            return self.close_windows_job(true, deadline);
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = root_pid;
            Ok(())
        }
    }

    #[cfg(target_os = "linux")]
    fn close_liveness_write(&mut self) {
        if self.controller_liveness_fd >= 0 {
            unsafe {
                libc::close(self.controller_liveness_fd);
            }
            self.controller_liveness_fd = -1;
        }
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    fn observe_owned_processes(&self, root_pid: u32) -> io::Result<()> {
        let snapshot = unix_process_snapshot()?;
        let mut owned = self
            .owned_processes
            .lock()
            .map_err(|_| io::Error::other("owned process identity lock poisoned"))?;
        extend_owned_processes(&mut owned, root_pid, &snapshot, self.adopted_guard()?)?;
        Ok(())
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    fn adopted_guard(&self) -> io::Result<AdoptedGuard> {
        Ok(None)
    }

    #[cfg(windows)]
    fn close_windows_job(
        &mut self,
        terminate: bool,
        deadline: std::time::Instant,
    ) -> io::Result<()> {
        use windows_sys::Win32::Foundation::CloseHandle;
        use windows_sys::Win32::System::JobObjects::{
            JobObjectBasicAccountingInformation, QueryInformationJobObject, TerminateJobObject,
            JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        };

        let mut job = self
            .job
            .lock()
            .map_err(|_| io::Error::other("controller job handle lock poisoned"))?;
        if job.is_null() {
            return Ok(());
        }
        let termination_error = (terminate && unsafe { TerminateJobObject(*job, 1) } == 0)
            .then(io::Error::last_os_error);
        let mut drain_error = None;
        if termination_error.is_none() {
            loop {
                let mut accounting = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
                let queried = unsafe {
                    QueryInformationJobObject(
                        *job,
                        JobObjectBasicAccountingInformation,
                        (&mut accounting as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(),
                        std::mem::size_of_val(&accounting) as u32,
                        std::ptr::null_mut(),
                    )
                };
                if queried == 0 {
                    drain_error = Some(io::Error::last_os_error());
                    break;
                }
                if accounting.ActiveProcesses == 0 {
                    break;
                }
                if std::time::Instant::now() >= deadline {
                    drain_error = Some(io::Error::new(
                        io::ErrorKind::TimedOut,
                        "Windows process job remained active at cleanup deadline",
                    ));
                    break;
                }
                thread::sleep(PROCESS_TREE_POLL_INTERVAL);
            }
        }
        unsafe {
            CloseHandle(*job);
        }
        *job = std::ptr::null_mut();
        termination_error.or(drain_error).map_or(Ok(()), Err)
    }
}

impl Drop for ControllerChildGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            if self.controller_liveness_read_fd >= 0 {
                libc::close(self.controller_liveness_read_fd);
                self.controller_liveness_read_fd = -1;
            }
            if self.controller_liveness_fd >= 0 {
                libc::close(self.controller_liveness_fd);
                self.controller_liveness_fd = -1;
            }
            if self.child_registration_read_fd >= 0 {
                libc::close(self.child_registration_read_fd);
                self.child_registration_read_fd = -1;
            }
            if self.child_registration_fd >= 0 {
                libc::close(self.child_registration_fd);
                self.child_registration_fd = -1;
            }
        }
        #[cfg(windows)]
        if let Ok(mut job) = self.job.lock() {
            if !job.is_null() {
                unsafe {
                    windows_sys::Win32::Foundation::CloseHandle(*job);
                }
                *job = std::ptr::null_mut();
            }
        }
    }
}

/// Close every descriptor this process inherited except `keep`.
///
/// Runs in a forked child, so it stays inside async-signal-safe libc calls and
/// walks a bounded descriptor range rather than allocating to enumerate
/// `/proc/self/fd`.
#[cfg(unix)]
fn close_inherited_descriptors_except(keep: &[RawFd]) {
    let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
    let max = if max > 0 { max as RawFd } else { 1024 };
    for fd in 3..max {
        if !keep.contains(&fd) {
            unsafe {
                libc::close(fd);
            }
        }
    }
    // stdio is replaced rather than simply closed so a later write in this
    // process cannot land on a descriptor the kernel has since recycled.
    let devnull = unsafe { libc::open(c"/dev/null".as_ptr(), libc::O_RDWR) };
    if devnull >= 0 {
        for fd in 0..3 {
            if !keep.contains(&fd) {
                unsafe {
                    libc::dup2(devnull, fd);
                }
            }
        }
        if devnull > 2 && !keep.contains(&devnull) {
            unsafe {
                libc::close(devnull);
            }
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn controller_death_guard_loop(
    read_fd: RawFd,
    write_fd: RawFd,
    registration_read_fd: RawFd,
    registration_fd: RawFd,
) -> ! {
    unsafe {
        libc::close(write_fd);
        libc::close(registration_fd);
    }
    // The watcher starts before the child is spawned. Close controller
    // descriptors, especially stdin-pipe writers, so it cannot keep a child
    // waiting for EOF after its controller drops them.
    close_inherited_descriptors_except(&[read_fd, registration_read_fd]);
    let mut pid_bytes = [0_u8; std::mem::size_of::<u32>()];
    let mut offset = 0;
    while offset < pid_bytes.len() {
        let read = unsafe {
            libc::read(
                registration_read_fd,
                pid_bytes[offset..].as_mut_ptr().cast(),
                pid_bytes.len() - offset,
            )
        };
        if read == 0 {
            unsafe { libc::_exit(0) };
        }
        if read < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            unsafe { libc::_exit(1) };
        }
        offset += read as usize;
    }
    let process_group = u32::from_ne_bytes(pid_bytes);
    let mut byte = 0_u8;
    loop {
        let read = unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) };
        if read == 0 {
            controller_death_cleanup(process_group);
        }
        if read < 0 && io::Error::last_os_error().raw_os_error() != Some(libc::EINTR) {
            unsafe {
                libc::_exit(1);
            }
        }
    }
}

#[cfg(unix)]
fn write_u32_nul(buf: &mut [u8; 12], mut value: u32) -> *const libc::c_char {
    buf[11] = 0;
    if value == 0 {
        buf[10] = b'0';
        return buf[10..].as_ptr().cast();
    }
    let mut i = 11;
    while value > 0 {
        i -= 1;
        buf[i] = b'0' + (value % 10) as u8;
        value /= 10;
    }
    buf[i..].as_ptr().cast()
}

/// The death watcher is already a single-threaded fork child. Exec a standalone
/// shell so process discovery can allocate safely while repeatedly tracking
/// descendants by both PID and kernel-reported start time. The root is still
/// alive when controller death closes the pipe, so session/group escapees remain
/// connected through PPID for the first snapshot.
#[cfg(all(unix, not(target_os = "linux")))]
fn controller_death_cleanup(root_pid: u32) -> ! {
    const SCRIPT: &str = concat!(
        r#"
set -eu
root=$1
state=${TMPDIR:-/tmp}/homeboy-child-guard-$$
snapshot=$state.snapshot
trap 'rm -f "$state" "$snapshot"' EXIT
: > "$state"
discover() {
  /bin/ps -axo pid=,ppid=,lstart= > "$snapshot"
  /usr/bin/awk -v root="$root" '
    BEGIN { split("", owned) }
    FILENAME==ARGV[1] { owned[$1 FS $2 FS $3 FS $4 FS $5 FS $6]=1; next }
    { pid=$1; ppid=$2; ident=$3 FS $4 FS $5 FS $6 FS $7; row[pid]=ident; parent[pid]=ppid }
    END {
      if (length(owned)==0 && row[root]!="") owned[root FS row[root]]=1
      changed=1
      while (changed) {
        changed=0
        for (pid in row) {
          for (key in owned) {
            split(key, fields, FS)
            if (parent[pid]==fields[1] && !((pid FS row[pid]) in owned)) {
              owned[pid FS row[pid]]=1; changed=1
            }
          }
        }
      }
      for (key in owned) print key
    }
  ' "$state" "$snapshot" > "$state.next"
  mv "$state.next" "$state"
}
signal_owned() {
  sig=$1
  discover
  /usr/bin/awk 'FILENAME==ARGV[1] { current[$1]=$3 FS $4 FS $5 FS $6 FS $7; next }
    { pid=$1; ident=$2 FS $3 FS $4 FS $5 FS $6; if (current[pid]==ident) print pid }
  ' "$snapshot" "$state" | while read -r pid; do kill -"$sig" "$pid" 2>/dev/null || true; done
}
for _ in 1 2 3 4 5 6 7 8; do signal_owned TERM; sleep .025; done
for _ in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16; do signal_owned KILL; sleep .025; done
discover
/usr/bin/awk 'FILENAME==ARGV[1] { current[$1]=$3 FS $4 FS $5 FS $6 FS $7; next }
  { pid=$1; ident=$2 FS $3 FS $4 FS $5 FS $6; if (current[pid]==ident) exit 1 }
' "$snapshot" "$state"
"#,
        "\0"
    );
    // This fork child must not allocate before exec: another thread may have held malloc's lock.
    let mut root_buf = [0u8; 12];
    let root = write_u32_nul(&mut root_buf, root_pid);
    unsafe {
        libc::execl(
            c"/bin/sh".as_ptr(),
            c"sh".as_ptr(),
            c"-c".as_ptr(),
            SCRIPT.as_ptr().cast::<libc::c_char>(),
            c"homeboy-child-guard".as_ptr(),
            root,
            std::ptr::null::<libc::c_char>(),
        );
        libc::kill(-(root_pid as libc::pid_t), libc::SIGKILL);
        libc::_exit(1);
    }
}

#[cfg(windows)]
fn assign_windows_kill_on_close_job(
    child: &Child,
    job_slot: &Mutex<windows_sys::Win32::Foundation::HANDLE>,
) -> io::Result<()> {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        SetInformationJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    let job = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
    if job.is_null() || job == INVALID_HANDLE_VALUE {
        return Err(io::Error::last_os_error());
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
    let configured = unsafe {
        SetInformationJobObject(
            job,
            JobObjectExtendedLimitInformation,
            (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
            std::mem::size_of_val(&limits) as u32,
        )
    };
    let process = unsafe { OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, child.id()) };
    let assigned = !process.is_null() && unsafe { AssignProcessToJobObject(job, process) } != 0;
    if !process.is_null() {
        unsafe {
            CloseHandle(process);
        }
    }
    if configured == 0 || !assigned {
        unsafe {
            CloseHandle(job);
        }
        return Err(io::Error::last_os_error());
    }
    *job_slot
        .lock()
        .map_err(|_| io::Error::other("controller job handle lock poisoned"))? = job;
    Ok(())
}

pub fn run(program: &str, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new(program).args(args).output().map_err(|e| {
        Error::internal_io(
            format!("Failed to run {}: {}", context, e),
            Some(context.to_string()),
        )
    })?;

    if !output.status.success() {
        return Err(Error::internal_io(
            format!("{} failed: {}", context, error_text(&output)),
            Some(context.to_string()),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn run_in(dir: &str, program: &str, args: &[&str], context: &str) -> Result<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map_err(|e| {
            Error::internal_io(
                format!("Failed to run {}: {}", context, e),
                Some(context.to_string()),
            )
        })?;

    if !output.status.success() {
        return Err(Error::internal_io(
            format!("{} failed: {}", context, error_text(&output)),
            Some(context.to_string()),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

pub fn run_in_optional(dir: &str, program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if stdout.is_empty() {
        None
    } else {
        Some(stdout)
    }
}

pub fn error_text(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr);
    if !stderr.trim().is_empty() {
        stderr.trim().to_string()
    } else {
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }
}

pub fn succeeded_in(dir: &str, program: &str, args: &[&str]) -> bool {
    Command::new(program)
        .args(args)
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

pub fn require_success(success: bool, stderr: &str, operation: &str) -> Result<()> {
    if success {
        Ok(())
    } else {
        Err(Error::internal_io(
            format!("{}_FAILED: {}", operation, stderr),
            Some(operation.to_string()),
        ))
    }
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
pub struct CaptureMetadata {
    pub bytes_seen: u64,
    pub bytes_retained: usize,
    pub byte_limit: usize,
    pub bytes_truncated: u64,
    pub truncated: bool,
    #[serde(default)]
    pub sha256: String,
}

impl<'de> Deserialize<'de> for CaptureMetadata {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Record {
            bytes_seen: u64,
            bytes_retained: usize,
            byte_limit: usize,
            bytes_truncated: Option<u64>,
            truncated: bool,
            #[serde(default)]
            sha256: String,
        }

        let record = Record::deserialize(deserializer)?;
        Ok(Self {
            bytes_seen: record.bytes_seen,
            bytes_retained: record.bytes_retained,
            byte_limit: record.byte_limit,
            bytes_truncated: record.bytes_truncated.unwrap_or_else(|| {
                record
                    .bytes_seen
                    .saturating_sub(record.bytes_retained as u64)
            }),
            truncated: record.truncated,
            sha256: record.sha256,
        })
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandCaptureMetadata {
    pub stdout: CaptureMetadata,
    pub stderr: CaptureMetadata,
}

#[derive(Debug)]
pub struct BoundedCommandOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub capture: CommandCaptureMetadata,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SupervisedCommandTermination {
    Completed,
    Cancelled,
    TimedOut,
    NoProgress,
}

/// A portable command may emit `HOMEBOY_PROGRESS {"phase":"...","current":"..."}`
/// on either output stream. The marker remains ordinary command output while
/// supervision uses it to report and enforce meaningful forward progress.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CommandProgress {
    pub phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unfinished: Vec<String>,
}

#[derive(Debug, Clone, Default)]
pub struct SupervisedCommandHeartbeat {
    pub elapsed: Duration,
    pub last_progress_elapsed: Option<Duration>,
    pub progress: Option<CommandProgress>,
    pub output_tail: String,
}

#[derive(Debug)]
pub struct SupervisedCommandOutput {
    pub output: BoundedCommandOutput,
    pub termination: SupervisedCommandTermination,
}

impl BoundedCommandOutput {
    pub fn into_output(self) -> Output {
        Output {
            status: self.status,
            stdout: self.stdout,
            stderr: self.stderr,
        }
    }
}

pub fn wait_with_bounded_output(
    mut child: Child,
    byte_limit: usize,
) -> io::Result<BoundedCommandOutput> {
    wait_with_bounded_output_until_cancelled(&mut child, byte_limit, || false)
}

pub fn wait_with_bounded_output_until_cancelled(
    child: &mut Child,
    byte_limit: usize,
    is_cancelled: impl FnMut() -> bool,
) -> io::Result<BoundedCommandOutput> {
    wait_with_bounded_output_until_cancelled_with_stdout_observer(
        child,
        byte_limit,
        is_cancelled,
        None,
    )
}

pub fn wait_with_bounded_output_until_cancelled_with_stdout_observer(
    child: &mut Child,
    byte_limit: usize,
    mut is_cancelled: impl FnMut() -> bool,
    stdout_line_observer: Option<StdoutLineObserver>,
) -> io::Result<BoundedCommandOutput> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_handle = stdout.map(|stream| {
        thread::spawn(move || {
            capture_tail_with_stdout_observer(stream, byte_limit, stdout_line_observer)
        })
    });
    let stderr_handle =
        stderr.map(|stream| thread::spawn(move || capture_tail(stream, byte_limit)));

    let status = loop {
        if let Some(status) = child.try_wait()? {
            // The command exited, but a background descendant can still hold
            // the inherited stdout/stderr pipes open. Joining the capture
            // readers first would block until that descendant exits, which is
            // an unbounded wait after the work is already done (#11702). Reap
            // the remaining group before joining, exactly as the supervised
            // sibling loop does.
            terminate_remaining_process_group(child.id())?;
            break status;
        }
        if is_cancelled() {
            break terminate_process_tree_and_reap(child)?;
        }
        thread::sleep(Duration::from_millis(100));
    };
    let stdout = join_capture(stdout_handle)?;
    let stderr = join_capture(stderr_handle)?;

    Ok(BoundedCommandOutput {
        status,
        stdout: stdout.bytes,
        stderr: stderr.bytes,
        capture: CommandCaptureMetadata {
            stdout: stdout.metadata,
            stderr: stderr.metadata,
        },
    })
}

/// Wait for an isolated child while retaining bounded output and reporting
/// liveness at a caller-selected cadence. The caller owns durable state; this
/// primitive owns only child supervision and process-tree termination.
pub fn wait_with_bounded_output_supervised(
    child: &mut Child,
    byte_limit: usize,
    timeout: Duration,
    heartbeat_interval: Duration,
    is_cancelled: impl FnMut() -> bool,
    on_heartbeat: impl FnMut(Duration, &str) -> io::Result<()>,
) -> io::Result<SupervisedCommandOutput> {
    wait_with_bounded_output_supervised_with_passthrough(
        child,
        byte_limit,
        timeout,
        heartbeat_interval,
        None,
        is_cancelled,
        on_heartbeat,
    )
}

/// Like [`wait_with_bounded_output_supervised`], with an optional immediate
/// stream tee that does not change bounded capture or supervision behavior.
pub fn wait_with_bounded_output_supervised_with_passthrough(
    child: &mut Child,
    byte_limit: usize,
    timeout: Duration,
    heartbeat_interval: Duration,
    passthrough: Option<StreamChunkObserver>,
    is_cancelled: impl FnMut() -> bool,
    mut on_heartbeat: impl FnMut(Duration, &str) -> io::Result<()>,
) -> io::Result<SupervisedCommandOutput> {
    wait_with_bounded_output_supervised_with_progress_and_passthrough(
        child,
        byte_limit,
        timeout,
        None,
        heartbeat_interval,
        passthrough,
        is_cancelled,
        |heartbeat| on_heartbeat(heartbeat.elapsed, &heartbeat.output_tail),
    )
}

/// Supervise a child whose platform containment must be closed before output
/// readers are joined. This is required for Windows jobs, where descendants can
/// otherwise retain inherited pipe handles after the direct child exits.
pub fn wait_with_bounded_output_supervised_guarded(
    child: &mut Child,
    guard: &mut ControllerChildGuard,
    byte_limit: usize,
    timeout: Duration,
    heartbeat_interval: Duration,
    mut is_cancelled: impl FnMut() -> bool,
    mut on_heartbeat: impl FnMut(Duration, &str) -> io::Result<()>,
) -> io::Result<SupervisedCommandOutput> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let live_output = Arc::new(Mutex::new(LiveOutputTail::new(byte_limit)));
    let stdout_handle = stdout.map({
        let live_output = Arc::clone(&live_output);
        move |stream| {
            thread::spawn(move || {
                capture_tail_with_live_snapshot(stream, byte_limit, true, live_output, None)
            })
        }
    });
    let stderr_handle = stderr.map({
        let live_output = Arc::clone(&live_output);
        move |stream| {
            thread::spawn(move || {
                capture_tail_with_live_snapshot(stream, byte_limit, false, live_output, None)
            })
        }
    });
    let started = std::time::Instant::now();
    let mut last_heartbeat = started.checked_sub(heartbeat_interval).unwrap_or(started);
    let supervision: io::Result<_> = loop {
        #[cfg(all(unix, not(target_os = "linux")))]
        if let Err(error) = guard.observe_owned_processes(child.id()) {
            break Err(error);
        }
        let status = match child.try_wait() {
            Ok(status) => status,
            Err(error) => break Err(error),
        };
        if let Some(status) = status {
            let deadline = std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE;
            break guard
                .close_after_root_exit(child.id(), deadline)
                .map(|()| (status, SupervisedCommandTermination::Completed, deadline));
        }
        if is_cancelled() {
            let deadline = std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE;
            break guard
                .terminate_and_reap(child, deadline)
                .map(|status| (status, SupervisedCommandTermination::Cancelled, deadline));
        }
        if started.elapsed() >= timeout {
            let deadline = std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE;
            break guard
                .terminate_and_reap(child, deadline)
                .map(|status| (status, SupervisedCommandTermination::TimedOut, deadline));
        }
        if last_heartbeat.elapsed() >= heartbeat_interval {
            let heartbeat = live_output
                .lock()
                .map(|tail| tail.heartbeat(started.elapsed()))
                .unwrap_or_default();
            if let Err(error) = on_heartbeat(heartbeat.elapsed, &heartbeat.output_tail) {
                let deadline = std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE;
                break Err(match guard.terminate_and_reap(child, deadline) {
                    Ok(_) => error,
                    Err(cleanup_error) => io::Error::other(format!(
                        "{error}; failed to terminate guarded child after heartbeat failure: {cleanup_error}"
                    )),
                });
            }
            last_heartbeat = std::time::Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    };
    let (status, termination, cleanup_deadline) = match supervision {
        Ok(supervision) => supervision,
        Err(primary) => {
            let deadline = std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE;
            let cleanup = guard.terminate_and_reap_bounded(child).err();
            let capture = join_capture_pair(stdout_handle, stderr_handle, deadline).err();
            return Err(io::Error::other(format!(
                "{primary}{}{}",
                cleanup
                    .map(|error| format!("; guarded cleanup failed: {error}"))
                    .unwrap_or_default(),
                capture
                    .map(|error| format!("; output cleanup failed: {error}"))
                    .unwrap_or_default(),
            )));
        }
    };
    let capture_deadline = cleanup_deadline.min(std::time::Instant::now() + CAPTURE_JOIN_GRACE);
    let (stdout, stderr) = join_capture_pair(stdout_handle, stderr_handle, capture_deadline)?;
    Ok(SupervisedCommandOutput {
        output: BoundedCommandOutput {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            capture: CommandCaptureMetadata {
                stdout: stdout.metadata,
                stderr: stderr.metadata,
            },
        },
        termination,
    })
}

fn join_capture_pair(
    stdout: Option<thread::JoinHandle<io::Result<BoundedStreamCapture>>>,
    stderr: Option<thread::JoinHandle<io::Result<BoundedStreamCapture>>>,
    deadline: std::time::Instant,
) -> io::Result<(BoundedStreamCapture, BoundedStreamCapture)> {
    let stdout = join_capture_bounded(stdout, deadline);
    let stderr = join_capture_bounded(stderr, deadline);
    match (stdout, stderr) {
        (Ok(stdout), Ok(stderr)) => Ok((stdout, stderr)),
        (Err(stdout), Err(stderr)) => Err(io::Error::other(format!(
            "stdout capture cleanup failed: {stdout}; stderr capture cleanup failed: {stderr}"
        ))),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

/// Supervise a command with wall-clock and optional structured-progress
/// deadlines. `no_progress_timeout` is measured from spawn until the first
/// valid `HOMEBOY_PROGRESS` marker and between subsequent markers. Ordinary
/// output is retained as evidence but cannot mask a stalled test case.
pub fn wait_with_bounded_output_supervised_with_progress(
    child: &mut Child,
    byte_limit: usize,
    timeout: Duration,
    no_progress_timeout: Option<Duration>,
    heartbeat_interval: Duration,
    is_cancelled: impl FnMut() -> bool,
    on_heartbeat: impl FnMut(SupervisedCommandHeartbeat) -> io::Result<()>,
) -> io::Result<SupervisedCommandOutput> {
    wait_with_bounded_output_supervised_with_progress_and_passthrough(
        child,
        byte_limit,
        timeout,
        no_progress_timeout,
        heartbeat_interval,
        None,
        is_cancelled,
        on_heartbeat,
    )
}

/// Supervise a command with optional structured progress and an immediate
/// bounded-output stream tee.
#[allow(clippy::too_many_arguments)]
pub fn wait_with_bounded_output_supervised_with_progress_and_passthrough(
    child: &mut Child,
    byte_limit: usize,
    timeout: Duration,
    no_progress_timeout: Option<Duration>,
    heartbeat_interval: Duration,
    passthrough: Option<StreamChunkObserver>,
    mut is_cancelled: impl FnMut() -> bool,
    mut on_heartbeat: impl FnMut(SupervisedCommandHeartbeat) -> io::Result<()>,
) -> io::Result<SupervisedCommandOutput> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let live_output = Arc::new(Mutex::new(LiveOutputTail::new(byte_limit)));
    let stdout_handle = stdout.map({
        let live_output = Arc::clone(&live_output);
        let passthrough = passthrough.clone();
        move |stream| {
            thread::spawn(move || {
                capture_tail_with_live_snapshot(stream, byte_limit, true, live_output, passthrough)
            })
        }
    });
    let stderr_handle = stderr.map({
        let live_output = Arc::clone(&live_output);
        let passthrough = passthrough.clone();
        move |stream| {
            thread::spawn(move || {
                capture_tail_with_live_snapshot(stream, byte_limit, false, live_output, passthrough)
            })
        }
    });
    let started = std::time::Instant::now();
    // Persist an initial live record even when a busy runner reaches the
    // deadline before one full heartbeat interval has elapsed.
    let mut last_heartbeat = started.checked_sub(heartbeat_interval).unwrap_or(started);
    let (status, termination) = loop {
        if let Some(status) = child.try_wait()? {
            terminate_remaining_process_group(child.id())?;
            break (status, SupervisedCommandTermination::Completed);
        }
        if is_cancelled() {
            break (
                terminate_process_tree_and_reap(child)?,
                SupervisedCommandTermination::Cancelled,
            );
        }
        if started.elapsed() >= timeout {
            break (
                terminate_process_tree_and_reap(child)?,
                SupervisedCommandTermination::TimedOut,
            );
        }
        if no_progress_timeout.is_some_and(|limit| {
            live_output
                .lock()
                .map(|live| {
                    live.last_progress
                        .as_ref()
                        .map(|(_, progress_at)| progress_at.elapsed())
                        .unwrap_or_else(|| started.elapsed())
                        >= limit
                })
                .unwrap_or(false)
        }) {
            break (
                terminate_process_tree_and_reap(child)?,
                SupervisedCommandTermination::NoProgress,
            );
        }
        if last_heartbeat.elapsed() >= heartbeat_interval {
            let heartbeat = live_output
                .lock()
                .map(|tail| tail.heartbeat(started.elapsed()))
                .unwrap_or_default();
            if let Err(error) = on_heartbeat(heartbeat) {
                return match terminate_process_tree_and_reap(child) {
                    Ok(_) => Err(error),
                    Err(cleanup_error) => Err(io::Error::other(format!(
                        "{error}; failed to terminate and reap supervised child after heartbeat failure: {cleanup_error}"
                    ))),
                };
            }
            last_heartbeat = std::time::Instant::now();
        }
        thread::sleep(Duration::from_millis(50));
    };
    let stdout = join_capture(stdout_handle)?;
    let stderr = join_capture(stderr_handle)?;
    Ok(SupervisedCommandOutput {
        output: BoundedCommandOutput {
            status,
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            capture: CommandCaptureMetadata {
                stdout: stdout.metadata,
                stderr: stderr.metadata,
            },
        },
        termination,
    })
}

/// A command can exit before a background descendant closes inherited output
/// pipes. Stop that remaining process group before joining capture readers.
///
/// Public so callers that run their own wait loop (the agent-task provider
/// path supervises stdin delivery and liveness itself) can reap an isolated
/// tree with the same semantics instead of reimplementing them.
#[cfg(unix)]
pub fn terminate_remaining_process_group(root_pid: u32) -> io::Result<()> {
    if !process_group_has_live_member(root_pid) {
        return Ok(());
    }
    signal_process_group(root_pid, libc::SIGTERM)?;
    if wait_for_process_group_exit_without_child(root_pid, PROCESS_TREE_TERM_GRACE) {
        return Ok(());
    }
    signal_process_group(root_pid, libc::SIGKILL)?;
    if wait_for_process_group_exit_without_child(root_pid, PROCESS_TREE_KILL_GRACE) {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("process group {root_pid} remained alive after its root exited"),
    ))
}

#[cfg(not(unix))]
pub fn terminate_remaining_process_group(_root_pid: u32) -> io::Result<()> {
    Ok(())
}

struct LiveOutputTail {
    stdout: TailCapture,
    stderr: TailCapture,
    last_progress: Option<(CommandProgress, std::time::Instant)>,
}

impl LiveOutputTail {
    fn new(byte_limit: usize) -> Self {
        Self {
            stdout: TailCapture::new(byte_limit),
            stderr: TailCapture::new(byte_limit),
            last_progress: None,
        }
    }

    fn render(&self) -> String {
        let stdout = String::from_utf8_lossy(&self.stdout.bytes);
        let stderr = String::from_utf8_lossy(&self.stderr.bytes);
        match (stdout.trim(), stderr.trim()) {
            ("", "") => String::new(),
            (stdout, "") => format!("stdout:\n{stdout}"),
            ("", stderr) => format!("stderr:\n{stderr}"),
            (stdout, stderr) => format!("stdout:\n{stdout}\nstderr:\n{stderr}"),
        }
    }

    fn record_progress_line(&mut self, line: &[u8]) {
        const PREFIX: &[u8] = b"HOMEBOY_PROGRESS ";
        let Some(body) = line.strip_prefix(PREFIX) else {
            return;
        };
        let Ok(progress) = serde_json::from_slice::<CommandProgress>(body) else {
            return;
        };
        if progress.phase.trim().is_empty() {
            return;
        }
        self.last_progress = Some((progress, std::time::Instant::now()));
    }

    fn heartbeat(&self, elapsed: Duration) -> SupervisedCommandHeartbeat {
        let (progress, marker_elapsed) = self
            .last_progress
            .as_ref()
            .map(|(progress, at)| (Some(progress.clone()), Some(at.elapsed())))
            .unwrap_or((None, None));
        SupervisedCommandHeartbeat {
            elapsed,
            last_progress_elapsed: marker_elapsed,
            progress,
            output_tail: self.render(),
        }
    }
}

fn capture_tail_with_live_snapshot(
    mut reader: impl Read,
    byte_limit: usize,
    stdout: bool,
    live_output: Arc<Mutex<LiveOutputTail>>,
    passthrough: Option<StreamChunkObserver>,
) -> io::Result<BoundedStreamCapture> {
    let mut capture = TailCapture::new(byte_limit);
    let mut pending = Vec::new();
    let mut discard_until_newline = false;
    let mut buffer = [0_u8; 8_192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        if let Some(passthrough) = &passthrough {
            passthrough(&buffer[..count], !stdout);
        }
        capture.push(&buffer[..count]);
        if let Ok(mut live) = live_output.lock() {
            let destination = if stdout {
                &mut live.stdout
            } else {
                &mut live.stderr
            };
            destination.push(&buffer[..count]);
            for byte in &buffer[..count] {
                if discard_until_newline {
                    if *byte == b'\n' {
                        discard_until_newline = false;
                    }
                    continue;
                }
                if *byte == b'\n' {
                    live.record_progress_line(&pending);
                    pending.clear();
                } else if pending.len() < MAX_OBSERVED_LINE_BYTES {
                    pending.push(*byte);
                } else {
                    pending.clear();
                    discard_until_newline = true;
                }
            }
        }
    }
    Ok(capture.finish())
}

pub fn isolate_process_tree(command: &mut Command) {
    #[cfg(unix)]
    configure_isolated_process_tree(command, -1, -1);

    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

#[cfg(unix)]
fn configure_isolated_process_tree(
    command: &mut Command,
    registration_fd: RawFd,
    liveness_read_fd: RawFd,
) {
    unsafe {
        use std::os::unix::process::CommandExt;

        command.pre_exec(move || {
            #[cfg(target_os = "linux")]
            {
                if libc::prctl(libc::PR_SET_CHILD_SUBREAPER, 1, 0, 0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                let root = libc::fork();
                if root < 0 {
                    return Err(io::Error::last_os_error());
                }
                if root == 0 {
                    if libc::setpgid(0, 0) != 0 {
                        return Err(io::Error::last_os_error());
                    }
                    if registration_fd >= 0 {
                        let pid = libc::getpid() as u32;
                        let bytes = pid.to_ne_bytes();
                        if libc::write(registration_fd, bytes.as_ptr().cast(), bytes.len())
                            != bytes.len() as isize
                        {
                            return Err(io::Error::last_os_error());
                        }
                    }
                    return Ok(());
                }
                execution_supervisor_loop(root as u32, liveness_read_fd);
            }
            #[cfg(not(target_os = "linux"))]
            {
                let _ = liveness_read_fd;
                if libc::setpgid(0, 0) != 0 {
                    return Err(io::Error::last_os_error());
                }
                if registration_fd >= 0 {
                    let pid = libc::getpid() as u32;
                    let bytes = pid.to_ne_bytes();
                    if libc::write(registration_fd, bytes.as_ptr().cast(), bytes.len())
                        != bytes.len() as isize
                    {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok(())
            }
        });
    }
}

#[cfg(target_os = "linux")]
fn signal_pid(pid: u32, signal: libc::c_int) -> io::Result<()> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Ok(());
    }
    unsafe {
        if libc::kill(pid as libc::pid_t, signal) != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
}

#[cfg(target_os = "linux")]
extern "C" fn handle_supervisor_term(_signal: libc::c_int) {
    SUPERVISOR_TEARDOWN.store(1, std::sync::atomic::Ordering::Relaxed);
}

#[cfg(target_os = "linux")]
fn execution_supervisor_loop(root_pid: u32, liveness_read_fd: RawFd) -> ! {
    // Close the std exec-error pipe write end inherited from Command::spawn.
    // The root child still holds it, so spawn can return on root exec success
    // or receive the root exec errno. Keeping this fd open would block spawn
    // until the supervisor exits.
    let keep = [liveness_read_fd];
    close_inherited_descriptors_except(if liveness_read_fd >= 0 {
        &keep[..1]
    } else {
        &[]
    });
    SUPERVISOR_TEARDOWN.store(0, std::sync::atomic::Ordering::Relaxed);
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_IGN);
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = handle_supervisor_term as *const () as libc::sighandler_t;
        if libc::sigemptyset(&mut action.sa_mask) != 0
            || libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut()) != 0
        {
            libc::_exit(1);
        }
        if liveness_read_fd >= 0
            && libc::fcntl(liveness_read_fd, libc::F_SETFL, libc::O_NONBLOCK) == -1
        {
            libc::_exit(1);
        }
    }

    let mut root_wait_status = 0;
    let mut root_reaped = false;
    let mut tearing_down = false;
    let mut kill_phase = false;
    let mut controller_loss = false;
    let mut phase_deadline = supervisor_now();

    loop {
        loop {
            let mut status = 0;
            let reaped = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
            if reaped > 0 {
                if reaped == root_pid as libc::pid_t {
                    root_wait_status = status;
                    root_reaped = true;
                    if !tearing_down {
                        tearing_down = true;
                        kill_phase = false;
                        phase_deadline = supervisor_deadline(PROCESS_TREE_TERM_GRACE);
                    }
                }
                continue;
            }
            if reaped < 0 {
                let errno = io::Error::last_os_error().raw_os_error();
                if errno == Some(libc::EINTR) {
                    continue;
                }
                if errno == Some(libc::ECHILD) && root_reaped {
                    supervisor_exit_with_wait_status(root_wait_status);
                }
            }
            break;
        }

        let liveness_closed = supervisor_liveness_closed(liveness_read_fd);
        if !tearing_down
            && (SUPERVISOR_TEARDOWN.load(std::sync::atomic::Ordering::Relaxed) != 0
                || liveness_closed)
        {
            tearing_down = true;
            controller_loss = liveness_closed;
            kill_phase = false;
            phase_deadline = supervisor_deadline(if controller_loss {
                CONTROLLER_LOSS_TERM_GRACE
            } else {
                PROCESS_TREE_TERM_GRACE
            });
        } else if tearing_down && supervisor_deadline_reached(phase_deadline) {
            if !kill_phase {
                kill_phase = true;
                phase_deadline = supervisor_deadline(if controller_loss {
                    CONTROLLER_LOSS_KILL_GRACE
                } else {
                    PROCESS_TREE_KILL_GRACE
                });
            } else {
                unsafe { libc::_exit(1) };
            }
        }

        if tearing_down {
            supervisor_signal_remaining(
                root_pid,
                if kill_phase {
                    libc::SIGKILL
                } else {
                    libc::SIGTERM
                },
            );
        }

        supervisor_idle(liveness_read_fd);
    }
}

#[cfg(target_os = "linux")]
fn supervisor_now() -> libc::timespec {
    let mut now = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    if unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut now) } != 0 {
        unsafe { libc::_exit(1) };
    }
    now
}

#[cfg(target_os = "linux")]
fn supervisor_deadline(grace: Duration) -> libc::timespec {
    let mut deadline = supervisor_now();
    deadline.tv_sec += grace.as_secs() as libc::time_t;
    deadline.tv_nsec += grace.subsec_nanos() as libc::c_long;
    if deadline.tv_nsec >= 1_000_000_000 {
        deadline.tv_sec += 1;
        deadline.tv_nsec -= 1_000_000_000;
    }
    deadline
}

#[cfg(target_os = "linux")]
fn supervisor_deadline_reached(deadline: libc::timespec) -> bool {
    let now = supervisor_now();
    now.tv_sec > deadline.tv_sec
        || (now.tv_sec == deadline.tv_sec && now.tv_nsec >= deadline.tv_nsec)
}

#[cfg(target_os = "linux")]
fn supervisor_liveness_closed(fd: RawFd) -> bool {
    if fd < 0 {
        return false;
    }
    let mut byte = 0_u8;
    loop {
        let read = unsafe { libc::read(fd, (&mut byte as *mut u8).cast(), 1) };
        if read == 0 {
            return true;
        }
        if read > 0 {
            continue;
        }
        let errno = io::Error::last_os_error().raw_os_error();
        return !matches!(errno, Some(libc::EINTR | libc::EAGAIN));
    }
}

#[cfg(target_os = "linux")]
fn supervisor_idle(fd: RawFd) {
    if fd >= 0 {
        let mut pollfd = libc::pollfd {
            fd,
            events: libc::POLLIN | libc::POLLHUP,
            revents: 0,
        };
        unsafe {
            libc::poll(
                &mut pollfd,
                1,
                PROCESS_TREE_POLL_INTERVAL.as_millis() as libc::c_int,
            );
        }
        return;
    }
    let mut sleep_for = libc::timespec {
        tv_sec: 0,
        tv_nsec: PROCESS_TREE_POLL_INTERVAL.as_nanos() as libc::c_long,
    };
    unsafe {
        libc::nanosleep(&mut sleep_for, std::ptr::null_mut());
    }
}

#[cfg(target_os = "linux")]
fn supervisor_signal_remaining(root_pid: u32, signal: libc::c_int) {
    unsafe {
        libc::kill(-(root_pid as libc::pid_t), signal);
    }
    supervisor_signal_children(signal);
}

#[cfg(target_os = "linux")]
fn supervisor_signal_children(signal: libc::c_int) {
    let mut path = [0u8; 64];
    let prefix = b"/proc/self/task/";
    path[..prefix.len()].copy_from_slice(prefix);
    let mut pid_buf = [0u8; 12];
    let pid = write_u32_nul(&mut pid_buf, unsafe { libc::getpid() } as u32);
    let pid_bytes = unsafe { std::ffi::CStr::from_ptr(pid) }.to_bytes();
    let mut offset = prefix.len();
    path[offset..offset + pid_bytes.len()].copy_from_slice(pid_bytes);
    offset += pid_bytes.len();
    let suffix = b"/children";
    path[offset..offset + suffix.len()].copy_from_slice(suffix);
    path[offset + suffix.len()] = 0;
    let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return;
    }
    let mut buf = [0u8; 4096];
    let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
    unsafe {
        libc::close(fd);
    }
    if read <= 0 {
        return;
    }
    let self_pid = unsafe { libc::getpid() } as u32;
    let mut pid = 0u32;
    let mut in_pid = false;
    for &byte in &buf[..read as usize] {
        if byte.is_ascii_digit() {
            in_pid = true;
            pid = pid.saturating_mul(10).saturating_add((byte - b'0') as u32);
        } else if in_pid {
            if pid != 0 && pid != self_pid {
                unsafe {
                    libc::kill(pid as libc::pid_t, signal);
                }
            }
            pid = 0;
            in_pid = false;
        }
    }
    if in_pid && pid != 0 && pid != self_pid {
        unsafe {
            libc::kill(pid as libc::pid_t, signal);
        }
    }
}

#[cfg(target_os = "linux")]
fn supervisor_exit_with_wait_status(status: libc::c_int) -> ! {
    unsafe {
        if libc::WIFEXITED(status) {
            libc::_exit(libc::WEXITSTATUS(status));
        }
        if libc::WIFSIGNALED(status) {
            let signal = libc::WTERMSIG(status);
            libc::signal(signal, libc::SIG_DFL);
            libc::raise(signal);
            libc::_exit(128 + signal);
        }
        libc::_exit(1);
    }
}

#[cfg(unix)]
fn signal_process_group(root_pid: u32, signal: libc::c_int) -> io::Result<()> {
    unsafe {
        let pgid = -(root_pid as libc::pid_t);
        if libc::kill(pgid, signal) != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        Ok(())
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn descendant_pids(root_pid: u32) -> io::Result<Vec<u32>> {
    // Process-tree cleanup is controller infrastructure, not workload code. It
    // must remain available when a caller replaces the ambient PATH.
    let output = Command::new("ps")
        .env("PATH", "/usr/bin:/bin")
        .args(["-axo", "pid=,ppid="])
        .output()?;
    let parents: Vec<(u32, u32)> = String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            Some((fields.next()?.parse().ok()?, fields.next()?.parse().ok()?))
        })
        .collect();
    let mut descendants = vec![root_pid];
    let mut cursor = 0;
    while cursor < descendants.len() {
        let parent = descendants[cursor];
        descendants.extend(
            parents
                .iter()
                .filter_map(|(pid, ppid)| (*ppid == parent).then_some(*pid)),
        );
        cursor += 1;
    }
    Ok(descendants)
}

#[cfg(all(unix, not(target_os = "linux")))]
#[derive(Debug, Clone, PartialEq, Eq)]
struct UnixProcessIdentity {
    pid: u32,
    parent_pid: u32,
    started_at: String,
}

#[cfg(all(unix, not(target_os = "linux")))]
fn unix_process_snapshot() -> io::Result<Vec<UnixProcessIdentity>> {
    let output = Command::new("ps")
        .env("PATH", "/usr/bin:/bin")
        .args(["-axo", "pid=,ppid=,lstart="])
        .output()?;
    if !output.status.success() {
        return Err(io::Error::other(format!(
            "process identity discovery failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| {
            let fields = line.split_whitespace().collect::<Vec<_>>();
            if fields.len() < 7 {
                return None;
            }
            Some(UnixProcessIdentity {
                pid: fields[0].parse().ok()?,
                parent_pid: fields[1].parse().ok()?,
                started_at: fields[2..7].join(" "),
            })
        })
        .collect())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn extend_owned_processes(
    owned: &mut Vec<UnixProcessIdentity>,
    root_pid: u32,
    snapshot: &[UnixProcessIdentity],
    adopted: AdoptedGuard,
) -> io::Result<()> {
    let _ = adopted;
    if owned.is_empty() {
        if let Some(root) = snapshot.iter().find(|process| process.pid == root_pid) {
            owned.push(root.clone());
        }
    }
    loop {
        let mut changed = false;
        for process in snapshot {
            if owned.iter().any(|known| known.pid == process.pid) {
                continue;
            }
            if owned_contains_matching_parent(owned, snapshot, process.parent_pid) {
                owned.push(process.clone());
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    Ok(())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn owned_identity_matches(known: &UnixProcessIdentity, current: &UnixProcessIdentity) -> bool {
    known.pid == current.pid && known.started_at == current.started_at
}

#[cfg(all(unix, not(target_os = "linux")))]
fn owned_contains_matching_parent(
    owned: &[UnixProcessIdentity],
    snapshot: &[UnixProcessIdentity],
    parent_pid: u32,
) -> bool {
    owned.iter().any(|known| {
        known.pid == parent_pid
            && snapshot
                .iter()
                .any(|current| owned_identity_matches(known, current))
    })
}

#[cfg(all(unix, not(target_os = "linux")))]
fn terminate_owned_processes_and_reap(
    child: &mut Child,
    owned_processes: &Mutex<Vec<UnixProcessIdentity>>,
    adopted: AdoptedGuard,
) -> io::Result<ExitStatus> {
    let root_pid = child.id();
    let mut owned = owned_processes
        .lock()
        .map_err(|_| io::Error::other("owned process identity lock poisoned"))?;
    signal_process_group(root_pid, libc::SIGTERM)?;
    let mut status = child.try_wait()?;
    if !wait_for_owned_process_exit(
        child,
        root_pid,
        &mut owned,
        PROCESS_TREE_TERM_GRACE,
        libc::SIGTERM,
        &mut status,
        adopted,
    )? {
        signal_process_group(root_pid, libc::SIGKILL)?;
        if !wait_for_owned_process_exit(
            child,
            root_pid,
            &mut owned,
            PROCESS_TREE_KILL_GRACE,
            libc::SIGKILL,
            &mut status,
            adopted,
        )? {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!("owned process tree {root_pid} remained alive after SIGKILL"),
            ));
        }
    }
    status.map(Ok).unwrap_or_else(|| child.wait())
}

#[cfg(all(unix, not(target_os = "linux")))]
fn terminate_owned_processes_after_root_exit(
    root_pid: u32,
    owned_processes: &Mutex<Vec<UnixProcessIdentity>>,
    adopted: AdoptedGuard,
) -> io::Result<()> {
    let mut owned = owned_processes
        .lock()
        .map_err(|_| io::Error::other("owned process identity lock poisoned"))?;
    if wait_for_owned_process_exit_without_child(
        root_pid,
        &mut owned,
        PROCESS_TREE_TERM_GRACE,
        libc::SIGTERM,
        adopted,
    )? {
        return Ok(());
    }
    if wait_for_owned_process_exit_without_child(
        root_pid,
        &mut owned,
        PROCESS_TREE_KILL_GRACE,
        libc::SIGKILL,
        adopted,
    )? {
        return Ok(());
    }
    Err(io::Error::new(
        io::ErrorKind::TimedOut,
        format!("owned process tree {root_pid} remained alive after root exit"),
    ))
}

#[cfg(all(unix, not(target_os = "linux")))]
fn wait_for_owned_process_exit(
    child: &mut Child,
    root_pid: u32,
    owned: &mut Vec<UnixProcessIdentity>,
    grace: Duration,
    signal: libc::c_int,
    status: &mut Option<ExitStatus>,
    adopted: AdoptedGuard,
) -> io::Result<bool> {
    let deadline = std::time::Instant::now() + grace;
    loop {
        if status.is_none() {
            *status = child.try_wait()?;
        }
        if status.is_some() {
            reap_exited_process_group_children(root_pid);
        }
        if drain_owned_processes(owned, root_pid, signal, adopted)? {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(PROCESS_TREE_POLL_INTERVAL);
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn wait_for_owned_process_exit_without_child(
    root_pid: u32,
    owned: &mut Vec<UnixProcessIdentity>,
    grace: Duration,
    signal: libc::c_int,
    adopted: AdoptedGuard,
) -> io::Result<bool> {
    let deadline = std::time::Instant::now() + grace;
    loop {
        reap_exited_process_group_children(root_pid);
        if drain_owned_processes(owned, root_pid, signal, adopted)? {
            return Ok(true);
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(PROCESS_TREE_POLL_INTERVAL);
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn drain_owned_processes(
    owned: &mut Vec<UnixProcessIdentity>,
    root_pid: u32,
    signal: libc::c_int,
    adopted: AdoptedGuard,
) -> io::Result<bool> {
    let snapshot = unix_process_snapshot()?;
    extend_owned_processes(owned, root_pid, &snapshot, adopted)?;
    if signal_and_reap_owned(owned, root_pid, signal, adopted)? {
        return Ok(false);
    }
    let discovered = owned.len();
    let refresh = unix_process_snapshot()?;
    extend_owned_processes(owned, root_pid, &refresh, adopted)?;
    Ok(owned.len() == discovered)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn signal_and_reap_owned(
    owned: &[UnixProcessIdentity],
    root_pid: u32,
    signal: libc::c_int,
    adopted: AdoptedGuard,
) -> io::Result<bool> {
    let mut remaining = false;
    let snapshot = unix_process_snapshot()?;
    for known in owned {
        let Some(current) = snapshot.iter().find(|process| process.pid == known.pid) else {
            continue;
        };
        if owned_identity_matches(known, current) && process_is_running(known.pid) {
            unsafe {
                let _ = libc::kill(known.pid as libc::pid_t, signal);
            }
            remaining = true;
        }
    }
    let _ = (root_pid, adopted);
    Ok(remaining)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn signal_pids(pids: &[u32], signal: libc::c_int) {
    for pid in pids {
        unsafe {
            let _ = libc::kill(*pid as libc::pid_t, signal);
        }
    }
}

#[cfg(unix)]
fn process_group_is_running(root_pid: u32) -> bool {
    unsafe { libc::kill(-(root_pid as libc::pid_t), 0) == 0 }
}

/// Whether `root_pid`'s process group still holds a process that can run.
///
/// `process_group_is_running` asks `kill(-pgid, 0)`, which succeeds for a
/// **zombie**. `reap_exited_process_group_children` clears the zombies we are
/// the parent of, but not every zombie in the group is ours: a shell background
/// job whose parent exits first is reparented to PID 1, and where PID 1 does not
/// reap — the usual case inside a container — it stays a zombie until the
/// container ends. `waitpid` cannot touch a process we did not spawn, so the
/// group reads as alive forever.
///
/// The visible cost is not just a wrong error. Termination burns both the
/// SIGTERM and SIGKILL grace windows (4s combined) waiting for processes that
/// already exited, then reports "remained alive after SIGKILL" for a tree that
/// is dead in every sense that matters.
///
/// A zombie holds no descriptors, runs no code, and cannot be killed again. For
/// the question these wait loops ask — may this tree still act? — it is gone.
#[cfg(unix)]
fn process_group_has_live_member(root_pid: u32) -> bool {
    if !process_group_is_running(root_pid) {
        return false;
    }
    // Absent a way to inspect process state, keep the historical answer: the
    // group exists, so treat it as alive.
    process_group_live_member_count(root_pid).is_none_or(|live| live > 0)
}

/// Count non-zombie members of `root_pid`'s process group, or `None` where
/// process state cannot be read.
#[cfg(all(unix, target_os = "linux"))]
fn process_group_live_member_count(root_pid: u32) -> Option<usize> {
    let mut live = 0usize;
    for entry in std::fs::read_dir("/proc").ok()?.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if name.parse::<u32>().is_err() {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            // A process that exits mid-scan is not a live member.
            continue;
        };
        // `comm` (field 2) is parenthesised and may itself contain spaces and
        // ')', so the fields after the final ')' are the only stable ones:
        // state, ppid, pgrp.
        let Some((_, rest)) = stat.rsplit_once(')') else {
            continue;
        };
        let mut fields = rest.split_whitespace();
        let Some(state) = fields.next() else {
            continue;
        };
        let Some(_ppid) = fields.next() else {
            continue;
        };
        let Some(pgrp) = fields.next().and_then(|field| field.parse::<u32>().ok()) else {
            continue;
        };
        if pgrp != root_pid {
            continue;
        }
        if state != "Z" {
            live += 1;
        }
    }
    Some(live)
}

#[cfg(all(unix, not(target_os = "linux")))]
fn process_group_live_member_count(_root_pid: u32) -> Option<usize> {
    None
}

/// Reap any of our own already-exited children that belong to `root_pid`'s
/// process group.
///
/// `process_group_is_running` asks `kill(-pgid, 0)`, which succeeds for a
/// **zombie**. The controller death guard installed by `ControllerChildGuard`
/// is a `fork()` child of this process that deliberately joins the supervised
/// group (`setpgid(0, process_group)`), so terminating the group leaves the
/// guard as an unreaped zombie inside it. The group then looks alive forever and
/// termination reports "process group N remained alive after SIGKILL" instead of
/// a timeout.
///
/// `waitpid(-pgid, ...)` is scoped to children in that one process group, so
/// this cannot reap an unrelated child of the same process.
#[cfg(unix)]
fn reap_exited_process_group_children(root_pid: u32) {
    loop {
        let mut status = 0;
        let reaped = unsafe {
            libc::waitpid(
                -(root_pid as libc::pid_t),
                &mut status,
                libc::WNOHANG | libc::WUNTRACED,
            )
        };
        // 0 = children remain but none have exited; -1 = no such children left.
        if reaped <= 0 {
            return;
        }
    }
}

#[cfg(all(unix, not(target_os = "linux")))]
fn wait_for_process_group_exit(
    child: &mut Child,
    root_pid: u32,
    grace: Duration,
    status: &mut Option<ExitStatus>,
) -> io::Result<bool> {
    let deadline = std::time::Instant::now() + grace;
    while process_group_has_live_member(root_pid) {
        if status.is_none() {
            *status = child.try_wait()?;
        }
        // `waitpid(-pgid)` may reap the direct child behind `Child` if it
        // exits between `try_wait` and the group reap. Let `Child` establish
        // its cached status first; group reaping is then limited to guards and
        // other descendants owned by this controller.
        if status.is_some() {
            reap_exited_process_group_children(root_pid);
        }
        if !process_group_has_live_member(root_pid) {
            break;
        }
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(PROCESS_TREE_POLL_INTERVAL);
    }
    Ok(true)
}

#[cfg(unix)]
fn wait_for_process_group_exit_without_child(root_pid: u32, grace: Duration) -> bool {
    let deadline = std::time::Instant::now() + grace;
    while process_group_has_live_member(root_pid) {
        reap_exited_process_group_children(root_pid);
        if !process_group_has_live_member(root_pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        thread::sleep(PROCESS_TREE_POLL_INTERVAL);
    }
    true
}

#[cfg(all(test, target_os = "linux"))]
mod process_group_liveness_tests {
    use super::*;
    use std::process::{Command, Stdio};

    /// A process group whose only remaining member is a zombie is not alive.
    ///
    /// `kill(-pgid, 0)` succeeds for a zombie, so the group reads as running
    /// long after it can do anything. Where the zombie is not our own child we
    /// cannot reap it away, so the wait loops have to recognise the state
    /// rather than wait it out.
    #[test]
    fn a_process_group_of_only_zombies_is_not_live() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 0"]);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        isolate_process_tree(&mut command);
        let child = command.spawn().expect("spawn child");
        let pid = child.id();
        // Deliberately leak the handle: dropping `Child` without waiting is what
        // leaves the exited process as an unreaped zombie.
        std::mem::forget(child);

        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        loop {
            let live = process_group_live_member_count(pid).expect("read process state");
            if live == 0 {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "child never exited; group still reports {live} live member(s)"
            );
            thread::sleep(Duration::from_millis(10));
        }

        assert!(
            process_group_is_running(pid),
            "precondition: kill(-pgid, 0) still succeeds for the zombie, which is \
             exactly why the naive probe is wrong"
        );
        assert!(
            !process_group_has_live_member(pid),
            "a group holding only a zombie must not be reported as alive"
        );

        reap_exited_process_group_children(pid);
    }

    /// The zombie allowance must not blind the probe to a group that is running.
    #[test]
    fn a_process_group_with_a_running_member_is_live() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        command.stdout(Stdio::null()).stderr(Stdio::null());
        isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn child");
        let pid = child.id();

        assert!(
            process_group_has_live_member(pid),
            "a group running `sleep 5` must be reported as alive"
        );

        let _ = terminate_process_tree_and_reap(&mut child);
    }

    #[test]
    fn parse_linux_proc_stat_handles_command_names_with_spaces() {
        let stat = "123 (name with spaces) Z 1 456 456 0 -1 0 0 0 0 0 0 0 0 0 0 0 0 0 4242 extra";
        let parsed = parse_linux_proc_stat(stat).expect("parse");
        assert_eq!(parsed.state, 'Z');
        assert_eq!(parsed.parent_pid, 1);
        assert_eq!(parsed.starttime_ticks, 4242);
    }
}

#[cfg(all(test, unix))]
mod supervisor_zombie_guard_tests {
    use super::*;
    use std::process::{Command, Stdio};
    use std::time::Instant;

    /// A supervised command that must be terminated has to report a *timeout*,
    /// not an internal supervision failure.
    ///
    /// The controller death guard is a `fork()` child that joins the supervised
    /// process group. Killing the group left it as an unreaped zombie there, and
    /// because `kill(-pgid, 0)` succeeds for zombies the group looked alive
    /// forever — so termination returned
    /// `process group N remained alive after SIGKILL` and the caller saw
    /// `timed_out: false` with no exit code (#10356).
    #[test]
    fn timing_out_a_guarded_child_reports_a_timeout_not_a_supervision_error() {
        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());

        let mut guard = ControllerChildGuard::prepare(&mut command).expect("prepare guard");
        let mut child = command.spawn().expect("spawn child");
        guard.attach(&child).expect("attach guard");

        let started = Instant::now();
        let supervised = wait_with_bounded_output_supervised_guarded(
            &mut child,
            &mut guard,
            4096,
            Duration::from_millis(50),
            Duration::from_millis(50),
            || false,
            |_, _| Ok(()),
        )
        .expect("supervision must succeed and report a timeout");

        assert_eq!(
            supervised.termination,
            SupervisedCommandTermination::TimedOut
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "termination must not wait out the child"
        );
    }

    /// The reaper is scoped to one process group, so a sibling child of this
    /// process in a different group must survive untouched.
    #[test]
    fn reaping_is_scoped_to_the_target_process_group() {
        let mut sibling = Command::new("sh")
            .args(["-c", "sleep 3"])
            .stdout(Stdio::piped())
            .spawn()
            .expect("spawn sibling");

        let mut command = Command::new("sh");
        command.args(["-c", "sleep 5"]);
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
        let guard = ControllerChildGuard::prepare(&mut command).expect("prepare guard");
        let mut child = command.spawn().expect("spawn child");
        guard.attach(&child).expect("attach guard");

        wait_with_bounded_output_supervised(
            &mut child,
            4096,
            Duration::from_millis(50),
            Duration::from_millis(50),
            || false,
            |_, _| Ok(()),
        )
        .expect("supervision succeeds");

        assert!(
            sibling.try_wait().expect("sibling status").is_none(),
            "a child outside the supervised process group must not be reaped"
        );
        let _ = sibling.kill();
        let _ = sibling.wait();
    }
}

#[cfg(all(test, target_os = "linux"))]
mod adopted_child_reaping_tests {
    use super::*;
    use std::ffi::CString;
    use std::os::unix::process::CommandExt;
    use std::path::Path;
    use std::process::{Command, Stdio};
    use std::time::{Duration, Instant};

    #[test]
    #[ignore = "subprocess fixture for a rapidly reparented guarded descendant"]
    fn guarded_fast_double_fork_fixture() {
        let target = std::env::var("HOMEBOY_ADOPTED_DAEMON_TARGET").expect("target");
        let pid_file = std::env::var("HOMEBOY_ADOPTED_DAEMON_PID_FILE").expect("pid file");
        let script = CString::new(format!(
            "printf '%s' $$ > {}; sleep 0.25; printf stale > {}; sleep 30",
            crate::shell::quote_path(&pid_file),
            crate::shell::quote_path(&target),
        ))
        .expect("fixture script");
        let shell = c"/bin/sh";
        let shell_name = c"sh";
        let command_flag = c"-c";

        let intermediate = unsafe { libc::fork() };
        assert_ne!(intermediate, -1, "fork intermediate");
        if intermediate == 0 {
            let daemon = unsafe { libc::fork() };
            if daemon < 0 {
                unsafe { libc::_exit(2) };
            }
            if daemon > 0 {
                unsafe { libc::_exit(0) };
            }
            if unsafe { libc::setsid() } == -1 {
                unsafe { libc::_exit(3) };
            }
            let max = unsafe { libc::sysconf(libc::_SC_OPEN_MAX) };
            for fd in 0..if max > 0 { max as i32 } else { 1024 } {
                unsafe {
                    libc::close(fd);
                }
            }
            unsafe {
                libc::execl(
                    shell.as_ptr(),
                    shell_name.as_ptr(),
                    command_flag.as_ptr(),
                    script.as_ptr(),
                    std::ptr::null::<libc::c_char>(),
                );
                libc::_exit(4);
            }
        }
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(intermediate, &mut status, 0) },
            intermediate,
            "reap intermediate"
        );
    }

    #[test]
    #[ignore = "subprocess fixture for an immediately-exiting reparented descendant"]
    fn guarded_instant_double_fork_fixture() {
        let intermediate = unsafe { libc::fork() };
        assert_ne!(intermediate, -1, "fork intermediate");
        if intermediate == 0 {
            let daemon = unsafe { libc::fork() };
            if daemon < 0 {
                unsafe { libc::_exit(2) };
            }
            if daemon > 0 {
                unsafe { libc::_exit(0) };
            }
            if unsafe { libc::setsid() } == -1 {
                unsafe { libc::_exit(3) };
            }
            unsafe { libc::_exit(0) };
        }
        let mut status = 0;
        assert_eq!(
            unsafe { libc::waitpid(intermediate, &mut status, 0) },
            intermediate,
            "reap intermediate"
        );
    }

    fn wait_for_recorded_pid(path: &Path, deadline: Instant) -> u32 {
        loop {
            if let Ok(contents) = std::fs::read_to_string(path) {
                if let Ok(pid) = contents.trim().parse::<u32>() {
                    return pid;
                }
            }
            assert!(
                Instant::now() < deadline,
                "double-fork fixture did not publish its PID"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    struct RecordedLinuxProcess {
        pid: u32,
        starttime_ticks: u64,
    }

    impl Drop for RecordedLinuxProcess {
        fn drop(&mut self) {
            let Some(stat) = linux_process_stat(self.pid) else {
                return;
            };
            if stat.starttime_ticks != self.starttime_ticks {
                return;
            }
            if process_is_running(self.pid) {
                unsafe {
                    libc::kill(self.pid as libc::pid_t, libc::SIGKILL);
                }
            }
            let Some(stat) = linux_process_stat(self.pid) else {
                return;
            };
            if stat.starttime_ticks == self.starttime_ticks
                && stat.state == 'Z'
                && stat.parent_pid == std::process::id()
            {
                let mut status = 0;
                unsafe {
                    libc::waitpid(self.pid as libc::pid_t, &mut status, libc::WNOHANG);
                }
            }
        }
    }

    struct KillChildOnDrop(Option<std::process::Child>);

    impl Drop for KillChildOnDrop {
        fn drop(&mut self) {
            if let Some(mut child) = self.0.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[test]
    fn guarded_completion_reaps_double_fork_setsid_closed_fd_without_touching_sibling() {
        let mut sibling = Command::new("sh");
        sibling
            .args(["-c", "exec sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let sibling = sibling.spawn().expect("spawn unrelated child");
        let sibling_pid = sibling.id();
        let sibling_ticks = linux_process_stat(sibling_pid)
            .expect("sibling start identity")
            .starttime_ticks;
        let mut sibling = KillChildOnDrop(Some(sibling));

        let workspace = tempfile::tempdir().expect("workspace");
        let target = workspace.path().join("target");
        let pid_file = workspace.path().join("daemon.pid");
        std::fs::write(&target, b"original").expect("write target");
        let test_binary = std::env::current_exe().expect("test executable");
        let mut command = Command::new(&test_binary);
        command
            .args([
                "--ignored",
                "--exact",
                "command::adopted_child_reaping_tests::guarded_fast_double_fork_fixture",
                "--nocapture",
            ])
            .env("HOMEBOY_ADOPTED_DAEMON_TARGET", &target)
            .env("HOMEBOY_ADOPTED_DAEMON_PID_FILE", &pid_file)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let mut guard = ControllerChildGuard::prepare(&mut command).expect("prepare guard");
        let mut child = command.spawn().expect("spawn double-fork root");
        guard.attach(&child).expect("attach guard");
        let daemon_pid = wait_for_recorded_pid(&pid_file, Instant::now() + Duration::from_secs(2));
        let daemon_ticks = linux_process_stat(daemon_pid)
            .expect("daemon start identity")
            .starttime_ticks;
        let _daemon = RecordedLinuxProcess {
            pid: daemon_pid,
            starttime_ticks: daemon_ticks,
        };

        let supervised = wait_with_bounded_output_supervised_guarded(
            &mut child,
            &mut guard,
            4096,
            Duration::from_secs(2),
            Duration::from_millis(50),
            || false,
            |_, _| Ok(()),
        )
        .expect("root completion contains reparented descendant");
        assert_eq!(
            supervised.termination,
            SupervisedCommandTermination::Completed
        );
        thread::sleep(Duration::from_millis(350));
        assert_eq!(
            std::fs::read(&target).expect("read target"),
            b"original",
            "descriptor-closing daemon mutated after root completion"
        );
        let daemon_still_same =
            linux_process_stat(daemon_pid).is_some_and(|stat| stat.starttime_ticks == daemon_ticks);
        assert!(
            !daemon_still_same || !process_is_running(daemon_pid),
            "adopted double-fork descendant remained runnable"
        );

        let sibling_child = sibling.0.as_mut().expect("sibling child");
        let sibling_stat = linux_process_stat(sibling_pid);
        assert!(
            sibling_child.try_wait().expect("sibling status").is_none(),
            "an unrelated child must not be reaped"
        );
        assert_eq!(
            sibling_stat.map(|stat| stat.starttime_ticks),
            Some(sibling_ticks),
            "unrelated child identity must remain the recorded process"
        );
    }

    fn our_zombie_pids() -> Vec<u32> {
        let self_pid = std::process::id();
        let mut zombies = Vec::new();
        let Ok(entries) = std::fs::read_dir("/proc") else {
            return zombies;
        };
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else {
                continue;
            };
            let Some(stat) = linux_process_stat(pid) else {
                continue;
            };
            if stat.state == 'Z' && stat.parent_pid == self_pid {
                zombies.push(pid);
            }
        }
        zombies
    }

    fn run_instant_double_fork() -> SupervisedCommandOutput {
        let test_binary = std::env::current_exe().expect("test executable");
        let mut command = Command::new(&test_binary);
        command
            .args([
                "--ignored",
                "--exact",
                "command::adopted_child_reaping_tests::guarded_instant_double_fork_fixture",
                "--nocapture",
            ])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        let mut guard = ControllerChildGuard::prepare(&mut command).expect("prepare guard");
        let mut child = command.spawn().expect("spawn instant double-fork root");
        guard.attach(&child).expect("attach guard");
        wait_with_bounded_output_supervised_guarded(
            &mut child,
            &mut guard,
            4096,
            Duration::from_secs(2),
            Duration::from_millis(50),
            || false,
            |_, _| Ok(()),
        )
        .expect("instant double-fork root completes")
    }

    #[test]
    fn guarded_completion_reaps_instant_double_fork_without_touching_sibling() {
        let mut sibling = Command::new("sh");
        sibling
            .args(["-c", "exec sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let sibling = sibling.spawn().expect("spawn unrelated child");
        let sibling_pid = sibling.id();
        let sibling_ticks = linux_process_stat(sibling_pid)
            .expect("sibling start identity")
            .starttime_ticks;
        let mut sibling = KillChildOnDrop(Some(sibling));
        let zombies_before = our_zombie_pids();

        let supervised = run_instant_double_fork();
        assert_eq!(
            supervised.termination,
            SupervisedCommandTermination::Completed
        );
        assert_eq!(
            our_zombie_pids(),
            zombies_before,
            "never-observed adopted descendant became a controller zombie"
        );

        let sibling_child = sibling.0.as_mut().expect("sibling child");
        assert!(
            sibling_child.try_wait().expect("sibling status").is_none(),
            "an unrelated child must not be reaped"
        );
        assert_eq!(
            linux_process_stat(sibling_pid).map(|stat| stat.starttime_ticks),
            Some(sibling_ticks),
            "unrelated child identity must remain the recorded process"
        );
    }

    #[test]
    fn repeated_instant_double_forks_leave_no_adopted_zombies() {
        let mut sibling = Command::new("sh");
        sibling
            .args(["-c", "exec sleep 30"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut sibling = KillChildOnDrop(Some(sibling.spawn().expect("spawn unrelated child")));
        let sibling_pid = sibling.0.as_ref().expect("sibling").id();
        let zombies_before = our_zombie_pids();

        for _ in 0..8 {
            let supervised = run_instant_double_fork();
            assert_eq!(
                supervised.termination,
                SupervisedCommandTermination::Completed
            );
        }

        assert_eq!(
            our_zombie_pids(),
            zombies_before,
            "repeated executions left adopted zombies on the controller"
        );
        assert!(
            sibling
                .0
                .as_mut()
                .expect("sibling")
                .try_wait()
                .expect("sibling status")
                .is_none(),
            "unrelated Child status must remain available across repeated executions"
        );
        assert_eq!(
            linux_process_stat(sibling_pid).map(|stat| stat.parent_pid),
            Some(std::process::id())
        );
    }
}

/// Terminate an isolated child process tree and reap its direct child process.
/// On platforms without process groups, `Child::kill` still provides portable
/// termination and reaping of the spawned process.
pub fn terminate_process_tree_and_reap(child: &mut Child) -> io::Result<ExitStatus> {
    #[cfg(target_os = "linux")]
    {
        let supervisor_pid = child.id();
        signal_pid(supervisor_pid, libc::SIGTERM)?;
        match reap_child_until(
            child,
            std::time::Instant::now() + PROCESS_TREE_TERM_GRACE + PROCESS_TREE_KILL_GRACE,
        ) {
            Ok(status) => Ok(status),
            Err(_) => {
                signal_pid(supervisor_pid, libc::SIGKILL)?;
                reap_child_until(
                    child,
                    std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE,
                )
            }
        }
    }

    #[cfg(all(unix, not(target_os = "linux")))]
    {
        let root_pid = child.id();
        // Shells can put background jobs in a distinct process group. Snapshot
        // descendants before terminating the root so those jobs cannot retain
        // output pipes and strand capture-reader joins.
        let descendants = descendant_pids(root_pid)?;
        signal_process_group(root_pid, libc::SIGTERM)?;
        signal_pids(&descendants, libc::SIGTERM);
        let mut status = child.try_wait()?;
        if !wait_for_process_group_exit(child, root_pid, PROCESS_TREE_TERM_GRACE, &mut status)? {
            signal_process_group(root_pid, libc::SIGKILL)?;
            signal_pids(&descendants, libc::SIGKILL);
            if !wait_for_process_group_exit(child, root_pid, PROCESS_TREE_KILL_GRACE, &mut status)?
            {
                if status.is_none() {
                    let _ = child.wait()?;
                }
                return Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!("process group {root_pid} remained alive after SIGKILL"),
                ));
            }
        }
        status.map(Ok).unwrap_or_else(|| child.wait())
    }

    #[cfg(not(unix))]
    {
        if let Err(error) = child.kill() {
            if error.kind() != io::ErrorKind::InvalidInput {
                return Err(error);
            }
        }
        reap_child_until(
            child,
            std::time::Instant::now() + PROCESS_TREE_CLEANUP_DEADLINE,
        )
    }
}

fn reap_child_until(child: &mut Child, deadline: std::time::Instant) -> io::Result<ExitStatus> {
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if std::time::Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                format!(
                    "child {} was not reaped before cleanup deadline",
                    child.id()
                ),
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
}

#[derive(Debug)]
struct BoundedStreamCapture {
    bytes: Vec<u8>,
    metadata: CaptureMetadata,
}

fn join_capture(
    handle: Option<thread::JoinHandle<io::Result<BoundedStreamCapture>>>,
) -> io::Result<BoundedStreamCapture> {
    match handle {
        Some(handle) => handle
            .join()
            .map_err(|_| io::Error::other("capture thread panicked"))?,
        None => Ok(BoundedStreamCapture {
            bytes: Vec::new(),
            metadata: CaptureMetadata::default(),
        }),
    }
}

fn join_capture_bounded(
    handle: Option<thread::JoinHandle<io::Result<BoundedStreamCapture>>>,
    deadline: std::time::Instant,
) -> io::Result<BoundedStreamCapture> {
    let Some(handle) = handle else {
        return Ok(BoundedStreamCapture {
            bytes: Vec::new(),
            metadata: CaptureMetadata::default(),
        });
    };
    while !handle.is_finished() {
        if std::time::Instant::now() >= deadline {
            #[cfg(windows)]
            unsafe {
                use std::os::windows::io::AsRawHandle;
                windows_sys::Win32::System::IO::CancelSynchronousIo(handle.as_raw_handle().cast());
            }
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "capture reader did not close before cleanup deadline",
            ));
        }
        thread::sleep(Duration::from_millis(10));
    }
    handle
        .join()
        .map_err(|_| io::Error::other("capture thread panicked"))?
}

fn capture_tail(mut stream: impl Read, byte_limit: usize) -> io::Result<BoundedStreamCapture> {
    let mut capture = TailCapture::new(byte_limit);
    let mut buf = [0_u8; 8192];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        capture.push(&buf[..n]);
    }
    Ok(capture.finish())
}

fn capture_tail_with_stdout_observer(
    mut stream: impl Read,
    byte_limit: usize,
    observer: Option<StdoutLineObserver>,
) -> io::Result<BoundedStreamCapture> {
    let mut capture = TailCapture::new(byte_limit);
    let mut pending = Vec::new();
    let mut discard_until_newline = false;
    let mut buf = [0_u8; 8192];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let chunk = &buf[..n];
        capture.push(chunk);
        if let Some(observer) = observer.as_ref() {
            for byte in chunk {
                if discard_until_newline {
                    if *byte == b'\n' {
                        discard_until_newline = false;
                    }
                    continue;
                }
                if *byte == b'\n' {
                    let line = String::from_utf8_lossy(&pending);
                    observer(line.trim_end_matches('\r'));
                    pending.clear();
                } else if pending.len() < MAX_OBSERVED_LINE_BYTES {
                    pending.push(*byte);
                } else {
                    pending.clear();
                    discard_until_newline = true;
                }
            }
        }
    }
    Ok(capture.finish())
}

struct TailCapture {
    bytes: Vec<u8>,
    bytes_seen: u64,
    byte_limit: usize,
    hasher: Sha256,
}

impl Default for TailCapture {
    fn default() -> Self {
        Self::new(0)
    }
}

impl TailCapture {
    fn new(byte_limit: usize) -> Self {
        Self {
            bytes: Vec::new(),
            bytes_seen: 0,
            byte_limit,
            hasher: Sha256::new(),
        }
    }

    fn push(&mut self, chunk: &[u8]) {
        self.hasher.update(chunk);
        self.bytes_seen = self
            .bytes_seen
            .saturating_add(chunk.len().try_into().unwrap_or(u64::MAX));
        if self.byte_limit == 0 {
            self.bytes.clear();
            return;
        }
        if chunk.len() >= self.byte_limit {
            self.bytes.clear();
            self.bytes
                .extend_from_slice(&chunk[chunk.len() - self.byte_limit..]);
            return;
        }
        self.bytes.extend_from_slice(chunk);
        let overflow = self.bytes.len().saturating_sub(self.byte_limit);
        if overflow > 0 {
            self.bytes.drain(..overflow);
        }
    }

    fn finish(self) -> BoundedStreamCapture {
        let bytes_retained = self.bytes.len();
        BoundedStreamCapture {
            bytes: self.bytes,
            metadata: CaptureMetadata {
                bytes_seen: self.bytes_seen,
                bytes_retained,
                byte_limit: self.byte_limit,
                bytes_truncated: self.bytes_seen.saturating_sub(bytes_retained as u64),
                truncated: self.bytes_seen > bytes_retained as u64,
                sha256: format!("sha256:{:x}", self.hasher.finalize()),
            },
        }
    }
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct CapturedOutput {
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

impl CapturedOutput {
    pub fn new(stdout: String, stderr: String) -> Self {
        Self { stdout, stderr }
    }

    pub fn is_empty(&self) -> bool {
        self.stdout.is_empty() && self.stderr.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn tail_capture_retains_last_bytes_and_marks_truncated() {
        let mut capture = TailCapture::new(5);
        capture.push(b"hello");
        capture.push(b" world");

        let captured = capture.finish();

        assert_eq!(captured.bytes, b"world");
        assert_eq!(captured.metadata.bytes_seen, 11);
        assert_eq!(captured.metadata.bytes_retained, 5);
        assert_eq!(captured.metadata.byte_limit, 5);
        assert!(captured.metadata.truncated);
    }

    #[test]
    fn live_passthrough_keeps_the_same_bounded_capture() {
        let chunks = Arc::new(Mutex::new(Vec::<(bool, Vec<u8>)>::new()));
        let passthrough: StreamChunkObserver = {
            let chunks = Arc::clone(&chunks);
            Arc::new(move |chunk, stderr| {
                chunks
                    .lock()
                    .expect("tee sink lock")
                    .push((stderr, chunk.to_vec()));
            })
        };
        let plain = capture_tail_with_live_snapshot(
            io::Cursor::new(b"first-second".to_vec()),
            6,
            true,
            Arc::new(Mutex::new(LiveOutputTail::new(6))),
            None,
        )
        .expect("capture without passthrough");
        let tee = capture_tail_with_live_snapshot(
            io::Cursor::new(b"first-second".to_vec()),
            6,
            true,
            Arc::new(Mutex::new(LiveOutputTail::new(6))),
            Some(passthrough),
        )
        .expect("capture with passthrough");

        assert_eq!(tee.bytes, plain.bytes);
        assert_eq!(tee.metadata.bytes_seen, plain.metadata.bytes_seen);
        assert_eq!(tee.metadata.bytes_retained, plain.metadata.bytes_retained);
        assert_eq!(tee.metadata.byte_limit, plain.metadata.byte_limit);
        assert_eq!(tee.metadata.truncated, plain.metadata.truncated);
        assert_eq!(tee.metadata.sha256, plain.metadata.sha256);
        assert_eq!(tee.bytes, b"second");
        assert_eq!(
            chunks.lock().expect("tee sink lock").as_slice(),
            &[(false, b"first-second".to_vec())]
        );
    }

    #[test]
    fn supervised_wait_heartbeats_and_times_out_an_isolated_child() {
        let mut command = Command::new("sh");
        command.args(["-lc", "printf gate-output; sleep 30"]);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn controlled gate");
        let heartbeats = AtomicUsize::new(0);
        let result = wait_with_bounded_output_supervised(
            &mut child,
            64,
            Duration::from_millis(150),
            Duration::from_millis(25),
            || false,
            |_, _| {
                heartbeats.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("supervise controlled gate");
        assert_eq!(result.termination, SupervisedCommandTermination::TimedOut);
        assert!(heartbeats.load(Ordering::SeqCst) > 0);
        assert_eq!(result.output.capture.stdout.byte_limit, 64);
    }

    #[cfg(unix)]
    #[test]
    fn heartbeat_failure_terminates_and_reaps_the_supervised_child() {
        let mut command = Command::new("sh");
        command.args(["-lc", "sleep 30"]);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn controlled gate");
        let pid = child.id();
        let error = wait_with_bounded_output_supervised(
            &mut child,
            64,
            Duration::from_secs(1),
            Duration::from_millis(10),
            || false,
            |_, _| Err(io::Error::other("durable heartbeat write failed")),
        )
        .expect_err("heartbeat persistence failure stops gate");
        assert!(error.to_string().contains("durable heartbeat write failed"));
        assert_ne!(unsafe { libc::kill(pid as libc::pid_t, 0) }, 0);
    }

    #[cfg(unix)]
    #[test]
    fn isolated_spawn_returns_before_the_command_exits() {
        let mut command = Command::new("sleep");
        command
            .arg("30")
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        isolate_process_tree(&mut command);
        let started = std::time::Instant::now();
        let mut child = command
            .spawn()
            .expect("spawn must not wait on the supervisor exec-error pipe");
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "Command::spawn blocked until the isolated command finished"
        );
        let _ = terminate_process_tree_and_reap(&mut child);
    }

    #[cfg(unix)]
    #[test]
    fn isolated_spawn_reports_root_exec_failure() {
        let mut command = Command::new("/no/such/homeboy-execution-supervisor-probe");
        command
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        isolate_process_tree(&mut command);
        let error = command
            .spawn()
            .expect_err("root exec failure must fail spawn, not return a live supervisor");
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[cfg(unix)]
    #[test]
    fn guarded_nonzero_root_exit_status_is_preserved() {
        let mut command = Command::new("sh");
        command.args(["-c", "exit 7"]);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut guard = ControllerChildGuard::prepare(&mut command).expect("prepare guard");
        let mut child = command.spawn().expect("spawn nonzero root");
        guard.attach(&child).expect("attach guard");
        let supervised = wait_with_bounded_output_supervised_guarded(
            &mut child,
            &mut guard,
            4096,
            Duration::from_secs(2),
            Duration::from_millis(50),
            || false,
            |_, _| Ok(()),
        )
        .expect("nonzero root completes");
        assert_eq!(
            supervised.termination,
            SupervisedCommandTermination::Completed
        );
        assert_eq!(supervised.output.status.code(), Some(7));
    }

    #[cfg(unix)]
    #[test]
    fn guarded_root_term_signal_status_is_preserved() {
        use std::os::unix::process::ExitStatusExt;

        let mut command = Command::new("sh");
        command.args(["-c", "kill -TERM $$"]);
        command
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let mut guard = ControllerChildGuard::prepare(&mut command).expect("prepare guard");
        let mut child = command.spawn().expect("spawn signaled root");
        guard.attach(&child).expect("attach guard");
        let supervised = wait_with_bounded_output_supervised_guarded(
            &mut child,
            &mut guard,
            4096,
            Duration::from_secs(2),
            Duration::from_millis(50),
            || false,
            |_, _| Ok(()),
        )
        .expect("signaled root completes");
        assert_eq!(
            supervised.termination,
            SupervisedCommandTermination::Completed
        );
        assert_eq!(supervised.output.status.signal(), Some(libc::SIGTERM));
    }

    #[test]
    fn tail_capture_reports_untruncated_stream() {
        let mut capture = TailCapture::new(10);
        capture.push(b"ok");

        let captured = capture.finish();

        assert_eq!(captured.bytes, b"ok");
        assert_eq!(captured.metadata.bytes_seen, 2);
        assert_eq!(captured.metadata.bytes_retained, 2);
        assert!(!captured.metadata.truncated);
    }

    #[test]
    fn capture_metadata_derives_truncation_for_truncated_legacy_records() {
        let metadata: CaptureMetadata = serde_json::from_value(serde_json::json!({
            "bytes_seen": 12,
            "bytes_retained": 8,
            "byte_limit": 8,
            "truncated": true
        }))
        .expect("legacy capture metadata");

        assert_eq!(metadata.bytes_truncated, 4);
        assert!(metadata.sha256.is_empty());
    }

    #[test]
    fn capture_metadata_derives_truncation_for_untruncated_legacy_records() {
        let metadata: CaptureMetadata = serde_json::from_value(serde_json::json!({
            "bytes_seen": 8,
            "bytes_retained": 8,
            "byte_limit": 8,
            "truncated": false
        }))
        .expect("legacy capture metadata");

        assert_eq!(metadata.bytes_truncated, 0);
        assert!(metadata.sha256.is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn cancellation_reaps_the_entire_isolated_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let script = format!(
            "trap '' TERM; sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; wait",
            crate::shell::quote_path(&pid_file.display().to_string())
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn process tree");

        let status =
            wait_with_bounded_output_until_cancelled(&mut child, 1024, || pid_file.exists())
                .expect("cancel and reap process tree");
        assert!(!status.status.success());

        let descendant_pid = std::fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert!(
            !process_is_running(descendant_pid as u32),
            "cancellation left descendant {descendant_pid} runnable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn controller_loss_reaps_the_release_mutation_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("upload-child.pid");
        let script = format!(
            "trap '' TERM; sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; wait",
            crate::shell::quote_path(&pid_file.display().to_string())
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        let guard = ControllerChildGuard::prepare(&mut command).expect("prepare controller guard");
        let mut child = command.spawn().expect("spawn upload fixture");
        guard.attach(&child).expect("attach controller guard");

        for _ in 0..100 {
            if pid_file.exists() {
                break;
            }
            thread::sleep(Duration::from_millis(10));
        }
        assert!(pid_file.exists(), "upload fixture should start its child");
        let descendant_pid = std::fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");

        drop(guard);
        let root_exited = (0..100).any(|_| {
            if child.try_wait().expect("monitor release fixture").is_some() {
                true
            } else {
                thread::sleep(Duration::from_millis(10));
                false
            }
        });
        if !root_exited {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "release mutation root {} survived controller loss",
                child.id()
            );
        }
        for _ in 0..100 {
            if !process_is_running(descendant_pid as u32) {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("release mutation descendant {descendant_pid} survived controller loss");
    }

    #[cfg(unix)]
    #[test]
    fn completed_parent_does_not_deadlock_on_a_background_output_holder() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let script = format!(
            "sleep 30 & echo $! > {}; printf done",
            crate::shell::quote_path(&pid_file.display().to_string())
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn process tree");

        let started = std::time::Instant::now();
        let result = wait_with_bounded_output_supervised(
            &mut child,
            64,
            Duration::from_secs(1),
            Duration::from_millis(10),
            || false,
            |_, _| Ok(()),
        )
        .expect("completed parent is supervised");

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(result.termination, SupervisedCommandTermination::Completed);
        let descendant_pid = std::fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert!(
            !process_is_running(descendant_pid as u32),
            "completed parent left descendant {descendant_pid} runnable"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cancellable_wait_does_not_deadlock_on_a_background_output_holder() {
        // The sibling of the supervised case above, for the cancellable path
        // `review lint --fix` runs through. A fixer that leaves a background
        // descendant holding the inherited pipes used to strand the capture
        // join after the command had already exited and written its edits,
        // so the run only ended at the caller's timeout (#11702).
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let script = format!(
            "sleep 30 & echo $! > {}; printf fixed",
            crate::shell::quote_path(&pid_file.display().to_string())
        );
        let mut command = Command::new("sh");
        command.args(["-c", &script]);
        command.stdout(std::process::Stdio::piped());
        command.stderr(std::process::Stdio::piped());
        isolate_process_tree(&mut command);
        let mut child = command.spawn().expect("spawn process tree");

        let started = std::time::Instant::now();
        let output = wait_with_bounded_output_until_cancelled(&mut child, 64, || false)
            .expect("completed command returns a terminal result");

        // The point of the fix: this returns promptly instead of blocking for
        // as long as the descendant lives.
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(output.status.success());
        assert_eq!(output.stdout, b"fixed");

        let descendant_pid = std::fs::read_to_string(&pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert!(
            !process_is_running(descendant_pid as u32),
            "cancellable wait left descendant {descendant_pid} runnable"
        );
    }
}
