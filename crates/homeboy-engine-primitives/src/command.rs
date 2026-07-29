//! Command execution primitives with consistent error handling.

use std::io::{self, Read};
#[cfg(unix)]
use std::os::fd::RawFd;
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::Arc;
use std::sync::Mutex;
use std::thread;
use std::time::Duration;

use homeboy_error::{Error, Result};
use serde::{Deserialize, Serialize};

pub const DEFAULT_CAPTURE_LIMIT_BYTES: usize = 4 * 1024 * 1024;
const MAX_OBSERVED_LINE_BYTES: usize = 64 * 1024;
#[cfg(unix)]
const PROCESS_TREE_TERM_GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const PROCESS_TREE_KILL_GRACE: Duration = Duration::from_secs(2);
#[cfg(unix)]
const PROCESS_TREE_POLL_INTERVAL: Duration = Duration::from_millis(25);

pub type StdoutLineObserver = Arc<dyn Fn(&str) + Send + Sync + 'static>;

/// Whether this build can isolate a spawned command into a terminable process
/// tree. Callers that require fail-closed child identity persistence must check
/// this before spawning.
pub const fn supports_process_tree_isolation() -> bool {
    cfg!(unix)
}

/// Keeps an isolated child tree owned by the controller that spawned it.
///
/// On Unix, a small guard process watches a pipe whose write end is held only
/// by the controller. Controller exit closes the pipe and the guard kills the
/// complete child process group, including descendants which ignore SIGTERM.
pub struct ControllerChildGuard {
    #[cfg(unix)]
    controller_liveness_read_fd: RawFd,
    #[cfg(unix)]
    controller_liveness_fd: RawFd,
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
                    unsafe {
                        libc::close(fds[0]);
                        libc::close(fds[1]);
                    }
                    return Err(io::Error::last_os_error());
                }
            }
            isolate_process_tree(command);
            return Ok(Self {
                controller_liveness_read_fd: fds[0],
                controller_liveness_fd: fds[1],
            });
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

    /// Start the guard after the command is spawned so it cannot inherit the
    /// standard library's private spawn error pipe.
    pub fn attach(&self, child: &Child) -> io::Result<()> {
        #[cfg(unix)]
        match unsafe { libc::fork() } {
            -1 => Err(io::Error::last_os_error()),
            0 => controller_death_guard_loop(
                self.controller_liveness_read_fd,
                self.controller_liveness_fd,
                child.id(),
            ),
            _ => Ok(()),
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
}

impl Drop for ControllerChildGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        unsafe {
            libc::close(self.controller_liveness_read_fd);
            libc::close(self.controller_liveness_fd);
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

#[cfg(unix)]
fn controller_death_guard_loop(read_fd: RawFd, write_fd: RawFd, process_group: u32) -> ! {
    unsafe {
        libc::close(write_fd);
    }
    if unsafe { libc::setpgid(0, process_group as libc::pid_t) } != 0 {
        unsafe {
            libc::_exit(1);
        }
    }
    let mut byte = 0_u8;
    loop {
        let read = unsafe { libc::read(read_fd, (&mut byte as *mut u8).cast(), 1) };
        if read == 0 {
            unsafe {
                libc::kill(-(process_group as libc::pid_t), libc::SIGKILL);
                libc::_exit(0);
            }
        }
        if read < 0 && io::Error::last_os_error().kind() != io::ErrorKind::Interrupted {
            unsafe {
                libc::_exit(1);
            }
        }
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

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CaptureMetadata {
    pub bytes_seen: u64,
    pub bytes_retained: usize,
    pub byte_limit: usize,
    pub truncated: bool,
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
                capture_tail_with_live_snapshot(stream, byte_limit, true, live_output)
            })
        }
    });
    let stderr_handle = stderr.map({
        let live_output = Arc::clone(&live_output);
        move |stream| {
            thread::spawn(move || {
                capture_tail_with_live_snapshot(stream, byte_limit, false, live_output)
            })
        }
    });
    let started = std::time::Instant::now();
    let mut last_heartbeat = started;
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
        if last_heartbeat.elapsed() >= heartbeat_interval {
            let tail = live_output
                .lock()
                .map(|tail| tail.render())
                .unwrap_or_default();
            if let Err(error) = on_heartbeat(started.elapsed(), &tail) {
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
#[cfg(unix)]
fn terminate_remaining_process_group(root_pid: u32) -> io::Result<()> {
    if !process_group_is_running(root_pid) {
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
fn terminate_remaining_process_group(_root_pid: u32) -> io::Result<()> {
    Ok(())
}

struct LiveOutputTail {
    stdout: TailCapture,
    stderr: TailCapture,
}

impl LiveOutputTail {
    fn new(byte_limit: usize) -> Self {
        Self {
            stdout: TailCapture::new(byte_limit),
            stderr: TailCapture::new(byte_limit),
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
}

fn capture_tail_with_live_snapshot(
    mut reader: impl Read,
    byte_limit: usize,
    stdout: bool,
    live_output: Arc<Mutex<LiveOutputTail>>,
) -> io::Result<BoundedStreamCapture> {
    let mut capture = TailCapture::new(byte_limit);
    let mut buffer = [0_u8; 8_192];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        capture.push(&buffer[..count]);
        if let Ok(mut live) = live_output.lock() {
            let destination = if stdout {
                &mut live.stdout
            } else {
                &mut live.stderr
            };
            destination.push(&buffer[..count]);
        }
    }
    Ok(capture.finish())
}

pub fn isolate_process_tree(command: &mut Command) {
    #[cfg(unix)]
    unsafe {
        use std::os::unix::process::CommandExt;

        command.pre_exec(|| {
            if libc::setpgid(0, 0) != 0 {
                return Err(io::Error::last_os_error());
            }
            Ok(())
        });
    }

    #[cfg(not(unix))]
    {
        let _ = command;
    }
}

fn signal_process_group(root_pid: u32, signal: libc::c_int) -> io::Result<()> {
    #[cfg(unix)]
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

    #[cfg(not(unix))]
    {
        let _ = (root_pid, signal);
        Err(io::Error::other(
            "process tree cancellation is not implemented on this platform",
        ))
    }
}

#[cfg(unix)]
fn descendant_pids(root_pid: u32) -> io::Result<Vec<u32>> {
    let output = Command::new("ps").args(["-axo", "pid=,ppid="]).output()?;
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

#[cfg(unix)]
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

#[cfg(unix)]
fn wait_for_process_group_exit(
    child: &mut Child,
    root_pid: u32,
    grace: Duration,
    status: &mut Option<ExitStatus>,
) -> io::Result<bool> {
    let deadline = std::time::Instant::now() + grace;
    while process_group_is_running(root_pid) {
        if status.is_none() {
            *status = child.try_wait()?;
        }
        reap_exited_process_group_children(root_pid);
        if !process_group_is_running(root_pid) {
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
    while process_group_is_running(root_pid) {
        reap_exited_process_group_children(root_pid);
        if !process_group_is_running(root_pid) {
            return true;
        }
        if std::time::Instant::now() >= deadline {
            return false;
        }
        thread::sleep(PROCESS_TREE_POLL_INTERVAL);
    }
    true
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

        let guard = ControllerChildGuard::prepare(&mut command).expect("prepare guard");
        let mut child = command.spawn().expect("spawn child");
        guard.attach(&child).expect("attach guard");

        let started = Instant::now();
        let supervised = wait_with_bounded_output_supervised(
            &mut child,
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

/// Terminate an isolated child process tree and reap its direct child process.
/// On platforms without process groups, `Child::kill` still provides portable
/// termination and reaping of the spawned process.
pub fn terminate_process_tree_and_reap(child: &mut Child) -> io::Result<ExitStatus> {
    #[cfg(unix)]
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
        return status.map(Ok).unwrap_or_else(|| child.wait());
    }

    #[cfg(not(unix))]
    {
        if let Err(error) = child.kill() {
            if error.kind() != io::ErrorKind::InvalidInput {
                return Err(error);
            }
        }
        child.wait()
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
        }
    }

    fn push(&mut self, chunk: &[u8]) {
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
                truncated: self.bytes_seen > bytes_retained as u64,
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

    #[cfg(unix)]
    #[test]
    fn cancellation_reaps_the_entire_isolated_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let script = format!(
            "trap '' TERM; sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; wait",
            shell_quote_path(&pid_file)
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
        assert_ne!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }

    #[cfg(unix)]
    #[test]
    fn controller_loss_reaps_the_release_mutation_process_group() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("upload-child.pid");
        let script = format!(
            "trap '' TERM; sh -c 'trap \"\" TERM; while :; do :; done' & echo $! > {}; wait",
            shell_quote_path(&pid_file)
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
            if unsafe { libc::kill(descendant_pid, 0) } != 0 {
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
            shell_quote_path(&pid_file)
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
        assert_ne!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }

    #[cfg(unix)]
    fn shell_quote_path(path: &std::path::Path) -> String {
        format!(
            "'{}'",
            path.display().to_string().replace('\'', "'\\\"'\\\"'")
        )
    }
}
