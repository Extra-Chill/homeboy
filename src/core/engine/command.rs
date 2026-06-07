//! Command execution primitives with consistent error handling.

use std::io::{self, Read};
use std::process::{Child, Command, ExitStatus, Output};
use std::thread;
use std::time::Duration;

use crate::core::error::{Error, Result};
use serde::Serialize;

pub const DEFAULT_CAPTURE_LIMIT_BYTES: usize = 64 * 1024;

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

#[derive(Debug, Clone, Copy, Default, Serialize, PartialEq, Eq)]
pub struct CaptureMetadata {
    pub bytes_seen: u64,
    pub bytes_retained: usize,
    pub byte_limit: usize,
    pub truncated: bool,
}

#[derive(Debug, Clone, Default, Serialize, PartialEq, Eq)]
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
    mut is_cancelled: impl FnMut() -> bool,
) -> io::Result<BoundedCommandOutput> {
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();
    let stdout_handle =
        stdout.map(|stream| thread::spawn(move || capture_tail(stream, byte_limit)));
    let stderr_handle =
        stderr.map(|stream| thread::spawn(move || capture_tail(stream, byte_limit)));

    let status = loop {
        if let Some(status) = child.try_wait()? {
            break status;
        }
        if is_cancelled() {
            terminate_process_tree(child.id())?;
            break child.wait()?;
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

fn terminate_process_tree(root_pid: u32) -> io::Result<()> {
    #[cfg(unix)]
    unsafe {
        let pgid = -(root_pid as libc::pid_t);
        if libc::kill(pgid, libc::SIGTERM) != 0 {
            let error = io::Error::last_os_error();
            if error.raw_os_error() != Some(libc::ESRCH) {
                return Err(error);
            }
        }
        thread::sleep(Duration::from_millis(500));
        if libc::kill(pgid, 0) == 0 {
            let _ = libc::kill(pgid, libc::SIGKILL);
        }
        Ok(())
    }

    #[cfg(not(unix))]
    {
        let _ = root_pid;
        Err(io::Error::other(
            "process tree cancellation is not implemented on this platform",
        ))
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
}
