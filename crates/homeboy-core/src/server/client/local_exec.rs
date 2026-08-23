use std::fs::OpenOptions;
use std::io::{Cursor, Read, Write};
use std::path::PathBuf;
use std::process::{ChildStdin, Command, Stdio};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    mpsc::{self, Receiver, RecvTimeoutError},
    Arc, Mutex,
};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;

use crate::engine::invocation;

#[cfg(not(target_os = "linux"))]
use super::super::process_cleanup::configure_process_group_cleanup;
#[cfg(target_os = "linux")]
use super::super::process_cleanup::prepare_process_scope_cleanup;
use super::super::process_cleanup::{
    active_cleanup_signal, interrupted_exit_code, stderr_with_interruption,
    ProcessGroupCleanupGuard,
};
use super::delegated::{
    stderr_with_delegated_failure, DelegatedRunFailureMonitor, DelegatedRunTerminalFailure,
};
use super::resource_monitor::ChildResourceMonitor;
use super::CommandOutput;

const PIPED_STDIN_FINISH_GRACE: Duration = Duration::from_millis(100);
const OUTPUT_READER_CLEANUP_GRACE: Duration = Duration::from_millis(250);

pub fn execute_local_command(command: &str) -> CommandOutput {
    execute_local_command_in_dir(command, None, None)
}

/// Run a local command, streaming `stdin` bytes to the child's standard input.
///
/// Used by the SSH transport's localhost fast path to feed a secret-env block
/// to the command over stdin instead of interpolating secret values into the
/// `sh -c` argv (where they would be visible in `ps`). The bytes are written on
/// a dedicated thread and stdin is closed (EOF) once they are flushed.
pub(crate) fn execute_local_command_with_stdin(command: &str, stdin: &[u8]) -> CommandOutput {
    execute_local_command_in_dir_impl(
        command,
        None,
        None,
        None,
        Some(StdinSource::Reader(Box::new(Cursor::new(stdin.to_vec())))),
    )
}

pub(crate) fn execute_local_command_with_piped_stdin(command: &str) -> CommandOutput {
    let stdin = match piped_stdin_file() {
        Ok(stdin) => stdin,
        Err(error) => return stdin_source_error(error),
    };
    execute_local_command_in_dir_impl(command, None, None, None, Some(StdinSource::Piped(stdin)))
}

pub(crate) fn execute_local_command_with_piped_stdin_and_timeout(
    command: &str,
    timeout: Duration,
) -> CommandOutput {
    let stdin = match piped_stdin_file() {
        Ok(stdin) => stdin,
        Err(error) => return stdin_source_error(error),
    };
    execute_local_command_in_dir_impl(
        command,
        None,
        None,
        Some(timeout),
        Some(StdinSource::Piped(stdin)),
    )
}

pub(crate) fn execute_local_command_with_stdin_and_timeout(
    command: &str,
    stdin: &[u8],
    timeout: Duration,
) -> CommandOutput {
    execute_local_command_in_dir_impl(
        command,
        None,
        None,
        Some(timeout),
        Some(StdinSource::Reader(Box::new(Cursor::new(stdin.to_vec())))),
    )
}

/// Run a local command, capturing stdout/stderr.
///
/// All locally-spawned commands run in their own process group with guaranteed
/// descendant teardown on exit, panic, or signal. Verbs that genuinely need a
/// background process use `std::process::Command` directly and manage the pid.
pub fn execute_local_command_in_dir(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    execute_local_command_in_dir_impl(command, current_dir, env, None, None)
}

pub fn execute_local_command_in_dir_with_timeout(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
    timeout: Duration,
) -> CommandOutput {
    execute_local_command_in_dir_impl(command, current_dir, env, Some(timeout), None)
}

#[derive(Clone, Copy)]
enum StreamMode {
    Capture,
    Passthrough,
    StderrPassthrough,
}

fn execute_local_command_in_dir_impl(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
    stdin: Option<StdinSource>,
) -> CommandOutput {
    run_local_command(
        command,
        current_dir,
        env,
        timeout,
        stdin,
        StreamMode::Capture,
    )
}

fn run_local_command(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
    stdin: Option<StdinSource>,
    stream_mode: StreamMode,
) -> CommandOutput {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_pairs) = env {
        cmd.envs(env_pairs.iter().copied());
    }

    cmd.stdout(Stdio::piped());
    cmd.stderr(Stdio::piped());
    if stdin.is_some() {
        cmd.stdin(Stdio::piped());
    }
    #[cfg(target_os = "linux")]
    let containment = match prepare_process_scope_cleanup(&mut cmd) {
        Ok(containment) => containment,
        Err(error) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command containment error: {error}"),
                success: false,
                exit_code: -1,
                timed_out: false,
                observation: super::CommandObservation::SpawnFailed,
                child_resource: None,
            };
        }
    };
    #[cfg(not(target_os = "linux"))]
    configure_process_group_cleanup(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command error: {}", e),
                success: false,
                exit_code: -1,
                timed_out: false,
                observation: super::CommandObservation::SpawnFailed,
                child_resource: None,
            };
        }
    };
    #[cfg(target_os = "linux")]
    let mut cleanup_guard = {
        let mut containment = containment;
        if let Err(error) = containment.attach(&child) {
            let containment_error = containment
                .terminate_on_failure_bounded(Duration::from_secs(2), false)
                .err();
            let _ = ProcessGroupCleanupGuard::new(child.id()).cleanup();
            let _ = child.wait();
            return CommandOutput {
                stdout: String::new(),
                stderr: format!(
                    "Command containment error: {error}{}",
                    containment_error
                        .map(|cleanup| format!("; scope cleanup also failed: {cleanup}"))
                        .unwrap_or_default()
                ),
                success: false,
                exit_code: -1,
                timed_out: false,
                observation: super::CommandObservation::SpawnFailed,
                child_resource: None,
            };
        }
        Some(ProcessGroupCleanupGuard::with_containment(
            child.id(),
            containment,
        ))
    };
    #[cfg(not(target_os = "linux"))]
    let mut cleanup_guard = Some(ProcessGroupCleanupGuard::new(child.id()));
    let mut supervision = ChildSupervision::start(env, command, current_dir, child.id());
    let _invocation_child_guard = invocation_child_guard(
        env,
        child.id(),
        cleanup_guard.as_ref().and_then(|guard| guard.pgid()),
        command,
    );
    let monitor = ChildResourceMonitor::start(child.id(), command.to_string());

    // Stream stdin independently while stdout/stderr are drained, so a large
    // producer cannot deadlock against a verbose child command.
    let stdin_handle = stdin.and_then(|source| {
        child
            .stdin
            .take()
            .map(|pipe| spawn_stdin_pump(pipe, source))
    });

    let stdout_reader = child.stdout.take().map(|pipe| {
        spawn_output_reader(
            pipe,
            matches!(stream_mode, StreamMode::Passthrough).then_some(OutputSink::Stdout),
        )
    });
    let stderr_reader = child.stderr.take().map(|pipe| {
        spawn_output_reader(
            pipe,
            matches!(
                stream_mode,
                StreamMode::Passthrough | StreamMode::StderrPassthrough
            )
            .then_some(OutputSink::Stderr),
        )
    });

    let (status, delegated_failure, timed_out, interrupted_signal) =
        wait_for_child_or_delegated_failure(
            &mut child,
            env,
            &mut cleanup_guard,
            timeout,
            supervision.as_mut(),
        );
    // The child was spawned successfully. A failed wait cannot be presented as
    // a spawn failure or a confirmed timeout/cancellation terminal result.
    let transport_observation_failed = status.is_err();
    // Descendants can inherit these pipes after the shell exits. Tear down the
    // process group before joining readers so they cannot hold this command open.
    let cleanup_report = cleanup_guard
        .take()
        .map(ProcessGroupCleanupGuard::cleanup)
        .unwrap_or_default();
    let interrupted_signal = interrupted_signal.or_else(active_cleanup_signal);

    let stdin_failed = stdin_handle
        .and_then(StdinPump::finish_after_child)
        .is_some_and(|result| result.is_err());
    let reader_deadline = Instant::now() + OUTPUT_READER_CLEANUP_GRACE;
    let (stdout, stdout_stalled) = collect_output_reader(stdout_reader, reader_deadline);
    let (mut stderr, stderr_stalled) = collect_output_reader(stderr_reader, reader_deadline);
    let streams_stalled = stdout_stalled || stderr_stalled;
    if streams_stalled || !cleanup_report.is_empty() {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        if let Some(detail) = cleanup_report.incomplete {
            stderr.push_str(&format!(
                "Homeboy containment cleanup was incomplete: {detail}.\n"
            ));
        }
        // Evidence only. Reduced discovery confidence is not a producer result,
        // so it is recorded without touching success or the exit code (#13128).
        if let Some(warning) = cleanup_report.warning {
            stderr.push_str(&format!(
                "Homeboy containment cleanup completed with reduced confidence: {warning}.\n"
            ));
        }
        if streams_stalled {
            stderr.push_str("Homeboy command output pipes remained open after cleanup; returning partial output instead of waiting for an unverified descendant.");
        }
    }
    let timed_out = timed_out || streams_stalled;

    let output = match status {
        Ok(status) => CommandOutput {
            stdout,
            stderr: stdin_failure_message(
                stderr_with_delegated_failure(
                    stderr_with_timeout(
                        stderr_with_interruption(stderr, interrupted_signal),
                        timed_out,
                        timeout,
                    ),
                    delegated_failure.as_ref(),
                ),
                stdin_failed,
            ),
            success: status.success()
                && interrupted_signal.is_none()
                && delegated_failure.is_none()
                && !timed_out
                && !stdin_failed,
            exit_code: timed_out_exit_code(
                timed_out,
                stdin_failure_exit_code(
                    stdin_failed,
                    interrupted_exit_code(interrupted_signal, status.code().unwrap_or(-1)),
                ),
            ),
            timed_out,
            observation: post_spawn_observation(
                transport_observation_failed,
                stdin_failed,
                interrupted_signal,
                timed_out,
            ),
            child_resource: Some(monitor.finish()),
        },
        Err(e) => CommandOutput {
            stdout,
            stderr: stderr_with_delegated_failure(
                stderr_with_interruption(
                    stderr_with_timeout(
                        format!("{stderr}\nCommand error: {}", e),
                        timed_out,
                        timeout,
                    ),
                    interrupted_signal,
                ),
                delegated_failure.as_ref(),
            ),
            success: false,
            exit_code: timed_out_exit_code(
                timed_out,
                interrupted_exit_code(interrupted_signal, -1),
            ),
            timed_out,
            observation: super::CommandObservation::TransportObservationFailed,
            child_resource: Some(monitor.finish()),
        },
    };
    if let Some(supervision) = supervision.as_mut() {
        supervision.finish(&output, interrupted_signal, timed_out, timeout);
    }
    output
}

pub(crate) enum StdinSource {
    Reader(Box<dyn Read + Send>),
    Piped(std::fs::File),
}

pub(crate) struct StdinPump {
    cancelled: Arc<AtomicBool>,
    cancellable: bool,
    completed: std::sync::mpsc::Receiver<()>,
    handle: thread::JoinHandle<std::io::Result<()>>,
}

impl StdinPump {
    pub(crate) fn finish_after_child(self) -> Option<std::io::Result<()>> {
        if self.cancellable {
            if self
                .completed
                .recv_timeout(PIPED_STDIN_FINISH_GRACE)
                .is_err()
            {
                self.cancelled.store(true, Ordering::Release);
            }
        }
        self.handle.join().ok()
    }
}

pub(crate) fn spawn_stdin_pump(pipe: ChildStdin, source: StdinSource) -> StdinPump {
    let cancelled = Arc::new(AtomicBool::new(false));
    let pump_cancelled = Arc::clone(&cancelled);
    // A regular file is finite. Let it drain after a fast child exit so a
    // localhost SSH envelope cannot observe EOF before its piped payload.
    let cancellable = matches!(&source, StdinSource::Piped(reader) if !reader
        .metadata()
        .map(|metadata| metadata.file_type().is_file())
        .unwrap_or(false));
    let (completed_tx, completed) = std::sync::mpsc::channel();
    let handle = thread::spawn(move || {
        let result = match source {
            StdinSource::Reader(mut reader) => copy_stdin_to_child(reader.as_mut(), pipe),
            StdinSource::Piped(reader) => copy_piped_stdin_to_child(reader, pipe, pump_cancelled),
        };
        let _ = completed_tx.send(());
        result
    });
    StdinPump {
        cancelled,
        cancellable,
        completed,
        handle,
    }
}

fn copy_stdin_to_child(reader: &mut dyn Read, mut pipe: ChildStdin) -> std::io::Result<()> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => pipe.write_all(&buffer[..count])?,
            Err(error) => return Err(error),
        }
    }
}

#[cfg(unix)]
fn copy_piped_stdin_to_child(
    mut reader: std::fs::File,
    mut pipe: ChildStdin,
    cancelled: Arc<AtomicBool>,
) -> std::io::Result<()> {
    use std::os::fd::AsRawFd;

    let mut descriptor = libc::pollfd {
        fd: reader.as_raw_fd(),
        events: libc::POLLIN,
        revents: 0,
    };
    let mut buffer = [0u8; 64 * 1024];
    while !cancelled.load(Ordering::Acquire) {
        let result = unsafe { libc::poll(&mut descriptor, 1, 20) };
        if result < 0 {
            let error = std::io::Error::last_os_error();
            if error.kind() != std::io::ErrorKind::Interrupted {
                return Err(error);
            }
            continue;
        }
        if result == 0 {
            continue;
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => pipe.write_all(&buffer[..count])?,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn copy_piped_stdin_to_child(
    mut reader: std::fs::File,
    mut pipe: ChildStdin,
    cancelled: Arc<AtomicBool>,
) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetFileType, FILE_TYPE_PIPE};
    use windows_sys::Win32::System::Pipes::PeekNamedPipe;

    let handle = reader.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
    if unsafe { GetFileType(handle) } != FILE_TYPE_PIPE {
        return copy_stdin_to_child(&mut reader, pipe);
    }

    let mut buffer = [0u8; 64 * 1024];
    while !cancelled.load(Ordering::Acquire) {
        let mut available = 0;
        let result = unsafe {
            PeekNamedPipe(
                handle,
                std::ptr::null_mut(),
                0,
                std::ptr::null_mut(),
                &mut available,
                std::ptr::null_mut(),
            )
        };
        if result == 0 {
            let error = std::io::Error::last_os_error();
            if windows_pipe_error_is_eof(error.raw_os_error(), available) {
                return Ok(());
            }
            return Err(error);
        }
        if available == 0 {
            thread::sleep(Duration::from_millis(20));
            continue;
        }
        match reader.read(&mut buffer) {
            Ok(0) => return Ok(()),
            Ok(count) => pipe.write_all(&buffer[..count])?,
            Err(error) => return Err(error),
        }
    }
    Ok(())
}

/// Windows reports a closed anonymous pipe through `PeekNamedPipe` as an
/// error rather than a zero-byte read. Treat only the no-bytes-remaining form
/// as EOF so an empty pipeline keeps normal no-input command semantics.
/// Production reaches this only from the `#[cfg(windows)]` `PeekNamedPipe`
/// path, but it is a pure predicate and its test runs on every host. Gate it
/// on `windows` OR `test` so a Linux lib build does not carry an unreachable
/// function, and the cross-platform test still compiles.
#[cfg(any(windows, test))]
pub(super) fn windows_pipe_error_is_eof(error_code: Option<i32>, available: u32) -> bool {
    available == 0 && matches!(error_code, Some(109 | 233))
}

#[cfg(all(not(unix), not(windows)))]
fn copy_piped_stdin_to_child(
    mut reader: std::fs::File,
    pipe: ChildStdin,
    _cancelled: Arc<AtomicBool>,
) -> std::io::Result<()> {
    copy_stdin_to_child(&mut reader, pipe)
}

#[cfg(unix)]
pub(crate) fn piped_stdin_file() -> std::io::Result<std::fs::File> {
    use std::os::fd::{AsRawFd, FromRawFd};

    let descriptor = unsafe { libc::dup(std::io::stdin().as_raw_fd()) };
    if descriptor < 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::fs::File::from_raw_fd(descriptor) })
    }
}

#[cfg(windows)]
pub(crate) fn piped_stdin_file() -> std::io::Result<std::fs::File> {
    use std::os::windows::io::{AsRawHandle, FromRawHandle};
    use windows_sys::Win32::Foundation::{DuplicateHandle, DUPLICATE_SAME_ACCESS, HANDLE};
    use windows_sys::Win32::System::Threading::GetCurrentProcess;

    let process = unsafe { GetCurrentProcess() };
    let mut duplicate: HANDLE = std::ptr::null_mut();
    let result = unsafe {
        DuplicateHandle(
            process,
            std::io::stdin().as_raw_handle() as HANDLE,
            process,
            &mut duplicate,
            0,
            0,
            DUPLICATE_SAME_ACCESS,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(unsafe { std::fs::File::from_raw_handle(duplicate) })
    }
}

#[cfg(all(not(unix), not(windows)))]
pub(crate) fn piped_stdin_file() -> std::io::Result<std::fs::File> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "piped stdin forwarding is not available on this platform",
    ))
}

fn stdin_source_error(error: std::io::Error) -> CommandOutput {
    CommandOutput {
        stdout: String::new(),
        stderr: format!("Homeboy cannot forward piped stdin: {error}"),
        success: false,
        exit_code: -1,
        timed_out: false,
        observation: super::CommandObservation::StdinDeliveryFailed,
        child_resource: None,
    }
}

fn stdin_failure_message(mut stderr: String, stdin_failed: bool) -> String {
    if stdin_failed {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str("Homeboy stdin delivery failed before command completion.");
    }
    stderr
}

fn stdin_failure_exit_code(stdin_failed: bool, exit_code: i32) -> i32 {
    if stdin_failed && exit_code == 0 {
        1
    } else {
        exit_code
    }
}

struct OutputReader {
    captured: Arc<Mutex<Vec<u8>>>,
    completed: Receiver<()>,
}

#[derive(Clone, Copy)]
enum OutputSink {
    Stdout,
    Stderr,
}

fn spawn_output_reader<R>(mut pipe: R, sink: Option<OutputSink>) -> OutputReader
where
    R: Read + Send + 'static,
{
    let captured = Arc::new(Mutex::new(Vec::new()));
    let reader_capture = Arc::clone(&captured);
    let (completed_tx, completed) = mpsc::channel();
    thread::spawn(move || {
        let mut buffer = [0u8; 4096];
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) | Err(_) => break,
                Ok(count) => {
                    match sink {
                        Some(OutputSink::Stdout) => {
                            let _ = std::io::stdout().write_all(&buffer[..count]);
                            let _ = std::io::stdout().flush();
                        }
                        Some(OutputSink::Stderr) => {
                            let _ = std::io::stderr().write_all(&buffer[..count]);
                            let _ = std::io::stderr().flush();
                        }
                        None => {}
                    }
                    if let Ok(mut captured) = reader_capture.lock() {
                        captured.extend_from_slice(&buffer[..count]);
                    }
                }
            }
        }
        let _ = completed_tx.send(());
    });
    OutputReader {
        captured,
        completed,
    }
}

fn collect_output_reader(reader: Option<OutputReader>, deadline: Instant) -> (String, bool) {
    let Some(reader) = reader else {
        return (String::new(), false);
    };
    let stalled = matches!(
        reader
            .completed
            .recv_timeout(deadline.saturating_duration_since(Instant::now())),
        Err(RecvTimeoutError::Timeout)
    );
    let captured = reader
        .captured
        .lock()
        .map(|captured| captured.clone())
        .unwrap_or_default();
    (String::from_utf8_lossy(&captured).to_string(), stalled)
}

pub fn execute_local_command_interactive(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> i32 {
    #[cfg(windows)]
    let mut cmd = {
        let mut cmd = Command::new("cmd");
        cmd.args(["/C", command]);
        cmd
    };

    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = Command::new("sh");
        cmd.args(["-c", command]);
        cmd
    };

    if let Some(dir) = current_dir {
        cmd.current_dir(dir);
    }

    if let Some(env_pairs) = env {
        cmd.envs(env_pairs.iter().copied());
    }

    let status = cmd
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status();

    match status {
        Ok(s) => s.code().unwrap_or(-1),
        Err(_) => -1,
    }
}

/// Execute local command with stdout/stderr tee'd to terminal *and* captured.
///
/// Originally this function just inherited stdout/stderr and returned empty
/// strings — which meant callers like the test runner had no way to surface
/// PHPUnit output when tests failed (#1143). We now pipe both streams, copy
/// each chunk to the parent's stdout/stderr as it arrives (so the user still
/// sees live progress), and retain the full text in `CommandOutput` for
/// downstream processing.
pub fn execute_local_command_passthrough(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    execute_local_command_passthrough_impl(command, current_dir, env, None)
}

pub fn execute_local_command_passthrough_with_timeout(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
    timeout: Duration,
) -> CommandOutput {
    execute_local_command_passthrough_impl(command, current_dir, env, Some(timeout))
}

fn execute_local_command_passthrough_impl(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> CommandOutput {
    run_local_command(
        command,
        current_dir,
        env,
        timeout,
        None,
        StreamMode::Passthrough,
    )
}

pub fn execute_local_command_stderr_passthrough(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    execute_local_command_stderr_passthrough_impl(command, current_dir, env, None)
}

pub fn execute_local_command_stderr_passthrough_with_timeout(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
    timeout: Duration,
) -> CommandOutput {
    execute_local_command_stderr_passthrough_impl(command, current_dir, env, Some(timeout))
}

fn execute_local_command_stderr_passthrough_impl(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
    timeout: Option<Duration>,
) -> CommandOutput {
    run_local_command(
        command,
        current_dir,
        env,
        timeout,
        None,
        StreamMode::StderrPassthrough,
    )
}

fn timed_out_exit_code(timed_out: bool, fallback: i32) -> i32 {
    if timed_out {
        124
    } else {
        fallback
    }
}

fn post_spawn_observation(
    transport_observation_failed: bool,
    stdin_failed: bool,
    interrupted_signal: Option<i32>,
    timed_out: bool,
) -> super::CommandObservation {
    if transport_observation_failed {
        super::CommandObservation::TransportObservationFailed
    } else if stdin_failed {
        super::CommandObservation::StdinDeliveryFailed
    } else if interrupted_signal.is_some() {
        super::CommandObservation::Cancelled
    } else if timed_out {
        super::CommandObservation::StreamDrainTimedOut
    } else {
        super::CommandObservation::Complete
    }
}

fn stderr_with_timeout(mut stderr: String, timed_out: bool, timeout: Option<Duration>) -> String {
    if timed_out {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        match timeout {
            Some(timeout) => stderr.push_str(&format!(
                "Homeboy command timed out after {}ms; terminated child process group before returning failure evidence.",
                timeout.as_millis()
            )),
            None => stderr.push_str(
                "Homeboy command timed out; terminated child process group before returning failure evidence.",
            ),
        }
    }
    stderr
}

fn invocation_child_guard(
    env: Option<&[(&str, &str)]>,
    root_pid: u32,
    pgid: Option<i32>,
    command_label: &str,
) -> Option<invocation::InvocationChildGuard> {
    let invocation_id = env.and_then(|pairs| {
        pairs
            .iter()
            .find_map(|(key, value)| (*key == "HOMEBOY_INVOCATION_ID").then_some(*value))
    })?;

    invocation::register_child_process(invocation_id, root_pid, pgid, command_label.to_string())
        .ok()
}

fn wait_for_child_or_delegated_failure(
    child: &mut std::process::Child,
    env: Option<&[(&str, &str)]>,
    cleanup_guard: &mut Option<ProcessGroupCleanupGuard>,
    timeout: Option<Duration>,
    mut supervision: Option<&mut ChildSupervision>,
) -> (
    std::io::Result<std::process::ExitStatus>,
    Option<DelegatedRunTerminalFailure>,
    bool,
    Option<i32>,
) {
    let monitor = DelegatedRunFailureMonitor::from_env(env);
    let deadline = timeout.map(|timeout| Instant::now() + timeout);

    loop {
        if let Some(supervision) = supervision.as_deref_mut() {
            supervision.heartbeat();
        }
        if let Some(signal) = active_cleanup_signal() {
            if let Some(cleanup_guard) = cleanup_guard.take() {
                cleanup_guard.cleanup();
            } else {
                let _ = child.kill();
            }
            return (child.wait(), None, false, Some(signal));
        }
        match child.try_wait() {
            Ok(Some(status)) => return (Ok(status), None, false, None),
            Ok(None) => {}
            Err(error) => return (Err(error), None, false, None),
        }

        if let Some(failure) = monitor
            .as_ref()
            .and_then(|monitor| monitor.terminal_failure())
        {
            if let Some(cleanup_guard) = cleanup_guard.take() {
                cleanup_guard.cleanup();
            }
            return (child.wait(), Some(failure), false, None);
        }

        if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
            if let Some(cleanup_guard) = cleanup_guard.take() {
                cleanup_guard.cleanup();
            } else {
                let _ = child.kill();
            }
            return (child.wait(), None, true, None);
        }

        let poll_interval = monitor
            .as_ref()
            .map(|monitor| monitor.poll_interval)
            .unwrap_or_else(|| Duration::from_millis(50));
        std::thread::sleep(poll_interval);
    }
}

const CHILD_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(1);
const CHILD_PROGRESS_REPORT_INTERVAL: Duration = Duration::from_secs(30);
const CHILD_OUTPUT_TAIL_BYTES: usize = 16 * 1024;
pub const CHILD_SECRET_ENV_NAMES_ENV: &str = "HOMEBOY_CHILD_SECRET_ENV_NAMES";
/// Opt in to concise lifecycle messages for a supervised local child.
///
/// The value is a generic operation label. Callers that own machine-readable
/// stdout can use it to make long-running child work visible on stderr.
pub const CHILD_PROGRESS_LABEL_ENV: &str = "HOMEBOY_CHILD_PROGRESS_LABEL";
static CHILD_SUPERVISION_TEMP_SEQUENCE: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

#[derive(Serialize)]
struct ChildSupervisionRecord {
    schema: &'static str,
    status: String,
    phase: &'static str,
    command: String,
    child_pid: u32,
    started_at: String,
    heartbeat_at: String,
    finished_at: Option<String>,
    timeout_ms: Option<u128>,
    cancellation_reason: Option<String>,
    exit_code: Option<i32>,
    duration_ms: Option<u128>,
    termination: Option<String>,
    stdout_tail: String,
    stderr_tail: String,
}

struct ChildSupervision {
    path: PathBuf,
    record: ChildSupervisionRecord,
    redaction_values: Vec<String>,
    last_heartbeat: Instant,
    last_progress_report: Instant,
    started: Instant,
    progress_label: Option<String>,
}

impl ChildSupervision {
    fn start(
        env: Option<&[(&str, &str)]>,
        command: &str,
        current_dir: Option<&str>,
        child_pid: u32,
    ) -> Option<Self> {
        let run_dir = env?.iter().find_map(|(key, value)| {
            (*key == crate::engine::run_dir::run_dir_env()).then_some(*value)
        })?;
        let now = Utc::now().to_rfc3339();
        let redaction_values = child_secret_values(env);
        let progress_label = env.and_then(|pairs| {
            pairs.iter().find_map(|(key, value)| {
                (*key == CHILD_PROGRESS_LABEL_ENV && !value.trim().is_empty())
                    .then_some((*value).to_string())
            })
        });
        if let Some(label) = &progress_label {
            eprintln!(
                "[{label}] started command={} cwd={}",
                redact_child_secret_values(command, &redaction_values),
                redact_child_secret_values(current_dir.unwrap_or("."), &redaction_values)
            );
        }
        let supervision = Self {
            path: PathBuf::from(run_dir).join(crate::engine::run_dir::files::CHILD_SUPERVISION),
            record: ChildSupervisionRecord {
                schema: "homeboy.child_supervision.v1",
                status: "running".to_string(),
                phase: "child",
                command: redact_child_secret_values(command, &redaction_values),
                child_pid,
                started_at: now.clone(),
                heartbeat_at: now,
                finished_at: None,
                timeout_ms: None,
                cancellation_reason: None,
                exit_code: None,
                duration_ms: None,
                termination: None,
                stdout_tail: String::new(),
                stderr_tail: String::new(),
            },
            redaction_values,
            last_heartbeat: Instant::now(),
            last_progress_report: Instant::now(),
            started: Instant::now(),
            progress_label,
        };
        supervision.persist();
        Some(supervision)
    }

    fn heartbeat(&mut self) {
        if self.last_heartbeat.elapsed() < CHILD_HEARTBEAT_INTERVAL {
            return;
        }
        self.record.heartbeat_at = Utc::now().to_rfc3339();
        self.last_heartbeat = Instant::now();
        if progress_report_due(self.last_progress_report, Instant::now()) {
            if let Some(label) = &self.progress_label {
                eprintln!(
                    "[{label}] still running elapsed={}s",
                    self.started.elapsed().as_secs()
                );
                self.last_progress_report = Instant::now();
            }
        }
        self.persist();
    }

    fn finish(
        &mut self,
        output: &CommandOutput,
        signal: Option<i32>,
        timed_out: bool,
        timeout: Option<Duration>,
    ) {
        self.record.status = if timed_out || signal.is_some() {
            "interrupted".to_string()
        } else {
            "completed".to_string()
        };
        self.record.heartbeat_at = Utc::now().to_rfc3339();
        self.record.finished_at = Some(self.record.heartbeat_at.clone());
        self.record.timeout_ms =
            timed_out.then(|| timeout.map(|timeout| timeout.as_millis()).unwrap_or(0));
        self.record.cancellation_reason = if timed_out {
            Some("timeout".to_string())
        } else {
            signal.map(|signal| format!("signal:{signal}"))
        };
        self.record.exit_code = Some(output.exit_code);
        self.record.duration_ms = Some(self.started.elapsed().as_millis());
        self.record.termination = Some(if timed_out {
            "timeout".to_string()
        } else if signal.is_some() {
            "cancelled".to_string()
        } else {
            "completed".to_string()
        });
        self.record.stdout_tail = bounded_redacted_tail(&output.stdout, &self.redaction_values);
        self.record.stderr_tail = bounded_redacted_tail(&output.stderr, &self.redaction_values);
        self.persist();
    }

    fn persist(&self) {
        let Some(parent) = self.path.parent() else {
            return;
        };
        if std::fs::create_dir_all(parent).is_err() {
            return;
        }
        let Ok(payload) = serde_json::to_vec_pretty(&self.record) else {
            return;
        };
        let temporary = self.path.with_extension(format!(
            "tmp-{}-{}",
            std::process::id(),
            CHILD_SUPERVISION_TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed),
        ));
        let Ok(mut file) = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        else {
            return;
        };
        if file.write_all(&payload).is_ok() && file.sync_all().is_ok() {
            let _ = std::fs::rename(&temporary, &self.path);
        } else {
            let _ = std::fs::remove_file(temporary);
        }
    }
}

fn progress_report_due(last_report: Instant, now: Instant) -> bool {
    now.duration_since(last_report) >= CHILD_PROGRESS_REPORT_INTERVAL
}

fn child_secret_values(env: Option<&[(&str, &str)]>) -> Vec<String> {
    let Some(env) = env else {
        return Vec::new();
    };
    let declared_names = env
        .iter()
        .find_map(|(name, value)| (*name == CHILD_SECRET_ENV_NAMES_ENV).then_some(*value))
        .unwrap_or_default();
    let mut values = declared_names
        .lines()
        .filter_map(|declared| {
            env.iter()
                .find_map(|(name, value)| (*name == declared).then_some((*value).to_string()))
        })
        .filter(|value| !value.is_empty())
        .collect::<Vec<_>>();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.dedup();
    values
}

fn redact_child_secret_values(output: &str, redaction_values: &[String]) -> String {
    let redacted = redaction_values
        .iter()
        .fold(output.to_string(), |output, secret| {
            output.replace(secret, "[REDACTED]")
        });
    crate::redaction::redact_string(&redacted)
}

fn bounded_redacted_tail(output: &str, redaction_values: &[String]) -> String {
    let start = output.len().saturating_sub(CHILD_OUTPUT_TAIL_BYTES);
    let start = output
        .char_indices()
        .find_map(|(index, _)| (index >= start).then_some(index))
        .unwrap_or_default();
    redact_child_secret_values(&output[start..], redaction_values)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn assert_pid_reaped(pid: libc::pid_t) {
        assert!(
            !crate::process::pid_is_running(pid as u32),
            "process {pid} was not reaped"
        );
    }

    #[cfg(unix)]
    fn pipe_files() -> (std::fs::File, std::fs::File) {
        use std::os::fd::FromRawFd;

        let mut descriptors = [0; 2];
        assert_eq!(unsafe { libc::pipe(descriptors.as_mut_ptr()) }, 0, "pipe");
        unsafe {
            (
                std::fs::File::from_raw_fd(descriptors[0]),
                std::fs::File::from_raw_fd(descriptors[1]),
            )
        }
    }

    fn supervision_output(run_dir: &tempfile::TempDir) -> serde_json::Value {
        let payload = std::fs::read_to_string(
            run_dir
                .path()
                .join(crate::engine::run_dir::files::CHILD_SUPERVISION),
        )
        .expect("child supervision artifact");
        serde_json::from_str(&payload).expect("parseable child supervision artifact")
    }

    #[test]
    fn timeout_persists_terminal_supervision_and_reaps_silent_child() {
        let run_dir = tempfile::tempdir().expect("run dir");
        let key = crate::engine::run_dir::run_dir_env();
        let value = run_dir.path().to_string_lossy().to_string();
        let env = [(key.as_str(), value.as_str())];

        let output = execute_local_command_in_dir_with_timeout(
            "sleep 30",
            None,
            Some(&env),
            Duration::from_millis(25),
        );

        assert!(output.timed_out);
        assert_eq!(output.exit_code, 124);
        let supervision = supervision_output(&run_dir);
        assert_eq!(supervision["schema"], "homeboy.child_supervision.v1");
        assert_eq!(supervision["status"], "interrupted");
        assert_eq!(supervision["cancellation_reason"], "timeout");
        assert!(supervision["started_at"].is_string());
        assert!(supervision["finished_at"].is_string());
        assert!(supervision["heartbeat_at"].is_string());
        assert!(supervision["command"].is_string());
        assert!(supervision["child_pid"].as_u64().is_some());
        #[cfg(unix)]
        {
            let pid = supervision["child_pid"].as_i64().expect("child pid") as libc::pid_t;
            assert_pid_reaped(pid);
        }
    }

    #[cfg(unix)]
    #[test]
    fn finite_piped_stdin_drains_before_child_completion_is_collected() {
        let output = tempfile::NamedTempFile::new().expect("output file");
        let output_path = output.path().to_string_lossy().to_string();
        let (reader, mut writer) = pipe_files();
        writer
            .write_all(b"finite piped stdin")
            .expect("write input");
        drop(writer);

        let result = execute_local_command_in_dir_impl(
            &format!("cat > {}", crate::engine::shell::quote_arg(&output_path)),
            None,
            None,
            None,
            Some(StdinSource::Piped(reader)),
        );

        assert!(result.success, "{}", result.stderr);
        assert_eq!(
            std::fs::read(output.path()).expect("read output"),
            b"finite piped stdin"
        );
    }

    #[cfg(unix)]
    #[test]
    fn idle_piped_stdin_is_cancelled_after_child_completion() {
        let (reader, _writer) = pipe_files();
        let started = Instant::now();

        let result = execute_local_command_in_dir_impl(
            "exit 0",
            None,
            None,
            None,
            Some(StdinSource::Piped(reader)),
        );

        assert!(result.success, "{}", result.stderr);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "idle stdin forwarding must remain bounded"
        );
    }

    #[test]
    fn progress_reports_are_rate_limited_separately_from_supervision() {
        let started = Instant::now();

        assert!(!progress_report_due(
            started,
            started + CHILD_PROGRESS_REPORT_INTERVAL - Duration::from_secs(1)
        ));
        assert!(progress_report_due(
            started,
            started + CHILD_PROGRESS_REPORT_INTERVAL
        ));
    }

    #[test]
    fn transport_wait_failure_overrides_local_timeout_and_stdin_labels() {
        for (stdin_failed, interrupted_signal, timed_out) in [
            (false, None, false),
            (false, Some(15), false),
            (false, None, true),
            (true, None, false),
        ] {
            assert_eq!(
                post_spawn_observation(true, stdin_failed, interrupted_signal, timed_out),
                super::super::CommandObservation::TransportObservationFailed
            );
        }
    }

    #[test]
    fn progress_cwd_redacts_declared_child_secrets() {
        let secret = "secret-value";
        let cwd = format!("/work/{secret}/component");

        assert_eq!(
            redact_child_secret_values(&cwd, &[secret.to_string()]),
            "/work/[REDACTED]/component"
        );
    }

    #[test]
    fn supervision_redacts_values_from_declared_child_secret_env() {
        let run_dir = tempfile::tempdir().expect("run dir");
        let run_dir_key = crate::engine::run_dir::run_dir_env();
        let run_dir_value = run_dir.path().to_string_lossy().to_string();
        let env = [
            (run_dir_key.as_str(), run_dir_value.as_str()),
            (CHILD_SECRET_ENV_NAMES_ENV, "FIXTURE_SECRET"),
            ("FIXTURE_SECRET", "supervision-fixture-secret"),
        ];

        let output = execute_local_command_in_dir(
            "printf 'received=%s\\n' \"$FIXTURE_SECRET\"",
            None,
            Some(&env),
        );

        assert!(output.success, "{}", output.stderr);
        let supervision = supervision_output(&run_dir).to_string();
        assert!(supervision.contains("[REDACTED]"));
        assert!(!supervision.contains("supervision-fixture-secret"));
    }

    #[cfg(unix)]
    #[test]
    fn sigterm_allows_child_process_group_to_exit_gracefully() {
        let run_dir = tempfile::tempdir().expect("run dir");
        let key = crate::engine::run_dir::run_dir_env();
        let value = run_dir.path().to_string_lossy().to_string();
        let marker = run_dir.path().join("graceful-termination");
        let marker_value = marker.to_string_lossy().to_string();
        let env = [
            (key.as_str(), value.as_str()),
            ("MARKER", marker_value.as_str()),
        ];
        let foreground_pid = unsafe { libc::getpid() };
        let interrupter = thread::spawn(move || {
            thread::sleep(Duration::from_millis(25));
            unsafe { libc::kill(foreground_pid, libc::SIGTERM) };
        });

        let output = execute_local_command_in_dir(
            "trap 'printf graceful > \"$MARKER\"; exit 0' TERM; while :; do sleep 1; done",
            None,
            Some(&env),
        );
        interrupter.join().expect("interrupter");

        assert_eq!(output.exit_code, 143);
        assert_eq!(
            std::fs::read_to_string(marker).expect("graceful marker"),
            "graceful"
        );
        let supervision = supervision_output(&run_dir);
        assert_eq!(supervision["status"], "interrupted");
        assert_eq!(supervision["cancellation_reason"], "signal:15");
        let pid = supervision["child_pid"].as_i64().expect("child pid") as libc::pid_t;
        assert_pid_reaped(pid);
    }

    #[cfg(unix)]
    #[test]
    fn normal_completion_reaps_descendants_before_collecting_output() {
        let started = Instant::now();
        let output = execute_local_command("sleep 30 & printf '%s' \"$!\"");

        assert!(output.success, "{}", output.stderr);
        assert!(started.elapsed() < Duration::from_secs(1));
        let pid = output
            .stdout
            .parse::<libc::pid_t>()
            .expect("descendant pid");
        assert_pid_reaped(pid);
    }
}
