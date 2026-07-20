//! Command execution primitives with consistent error handling.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Output};
use std::sync::Arc;
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
        if std::time::Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(PROCESS_TREE_POLL_INTERVAL);
    }
    Ok(true)
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
    fn shell_quote_path(path: &std::path::Path) -> String {
        format!(
            "'{}'",
            path.display().to_string().replace('\'', "'\\\"'\\\"'")
        )
    }
}
