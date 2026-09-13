use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

use homeboy_engine_primitives::command::ExecutionOwner;
use tempfile::NamedTempFile;

pub(crate) struct DeadlineProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[derive(Debug)]
pub(crate) struct DeadlineProcessFailure {
    pub message: String,
}

struct CaptureFiles {
    stdout: NamedTempFile,
    stderr: NamedTempFile,
    limit: usize,
}

impl CaptureFiles {
    fn create(limit: usize, label: &str) -> Result<Self, DeadlineProcessFailure> {
        Ok(Self {
            stdout: NamedTempFile::new().map_err(|error| {
                failure(format!(
                    "{label} stdout capture file creation failed: {error}"
                ))
            })?,
            stderr: NamedTempFile::new().map_err(|error| {
                failure(format!(
                    "{label} stderr capture file creation failed: {error}"
                ))
            })?,
            limit,
        })
    }

    fn stdio(&self, label: &str) -> Result<(Stdio, Stdio), DeadlineProcessFailure> {
        let stdout = self.stdout.reopen().map_err(|error| {
            failure(format!("{label} stdout capture file setup failed: {error}"))
        })?;
        let stderr = self.stderr.reopen().map_err(|error| {
            failure(format!("{label} stderr capture file setup failed: {error}"))
        })?;
        Ok((Stdio::from(stdout), Stdio::from(stderr)))
    }

    fn snapshot(&self, stream: &str, label: &str) -> Result<Vec<u8>, DeadlineProcessFailure> {
        let file = match stream {
            "stdout" => &self.stdout,
            "stderr" => &self.stderr,
            _ => unreachable!("capture stream is fixed"),
        };
        let length = file
            .as_file()
            .metadata()
            .map_err(|error| failure(format!("{label} {stream} capture metadata failed: {error}")))?
            .len();
        if length > self.limit as u64 {
            return Err(failure(format!(
                "{label} output exceeded the {} byte limit.",
                self.limit
            )));
        }
        let mut snapshot = file.reopen().map_err(|error| {
            failure(format!("{label} {stream} capture snapshot failed: {error}"))
        })?;
        let mut output = Vec::with_capacity(length as usize);
        Read::take(&mut snapshot, (self.limit + 1) as u64)
            .read_to_end(&mut output)
            .map_err(|error| failure(format!("{label} {stream} capture read failed: {error}")))?;
        if output.len() > self.limit {
            return Err(failure(format!(
                "{label} output exceeded the {} byte limit.",
                self.limit
            )));
        }
        Ok(output)
    }
}

pub(crate) fn execute_deadline_process(
    mut command: Command,
    input: &[u8],
    deadline: Instant,
    cleanup_budget: Duration,
    capture_limit: usize,
    label: &str,
) -> Result<DeadlineProcessOutput, DeadlineProcessFailure> {
    if Instant::now() >= deadline {
        return Err(failure(format!("{label} budget exhausted before spawn.")));
    }
    let captures = CaptureFiles::create(capture_limit, label)?;
    let (stdout, stderr) = captures.stdio(label)?;
    command.stdin(Stdio::piped()).stdout(stdout).stderr(stderr);
    let mut owner = ExecutionOwner::spawn(&mut command).map_err(|error| {
        failure(format!(
            "{label} spawn failed; capture files will be removed: {error}"
        ))
    })?;
    let mut stdin = owner.take_stdin().ok_or_else(|| {
        let errors = drain_owner(&mut owner, cleanup_budget);
        failure(format!(
            "{label} stdin was unavailable.{}",
            cleanup_diagnostic(&errors)
        ))
    })?;
    if stdin
        .write_all(input)
        .and_then(|_| stdin.write_all(b"\n"))
        .is_err()
    {
        let errors = drain_owner(&mut owner, cleanup_budget);
        return Err(failure(format!(
            "{label} stdin write failed.{}",
            cleanup_diagnostic(&errors)
        )));
    }
    drop(stdin);

    loop {
        match owner.try_wait() {
            Ok(Some(status)) => {
                return Ok(DeadlineProcessOutput {
                    status,
                    stdout: captures.snapshot("stdout", label)?,
                    stderr: captures.snapshot("stderr", label)?,
                });
            }
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let mut errors = drain_owner(&mut owner, cleanup_budget);
                errors.extend(capture_snapshot_errors(&captures, label));
                return Err(failure(format!(
                    "{label} timed out; capture files were snapshotted without waiting for inherited handles{}.",
                    cleanup_diagnostic(&errors)
                )));
            }
            Err(error) => {
                let mut errors = drain_owner(&mut owner, cleanup_budget);
                errors.extend(capture_snapshot_errors(&captures, label));
                return Err(failure(format!(
                    "{label} wait failed: {error}; capture files were snapshotted without waiting for inherited handles{}.",
                    cleanup_diagnostic(&errors)
                )));
            }
        }
    }
}

fn drain_owner(owner: &mut ExecutionOwner, _cleanup_budget: Duration) -> Vec<String> {
    match owner.drain_and_reap().and_then(|outcome| {
        outcome.into_root_status()?;
        Ok(())
    }) {
        Ok(()) => Vec::new(),
        Err(error) => vec![format!("execution owner drain: {error}")],
    }
}

fn cleanup_diagnostic(errors: &[String]) -> String {
    (!errors.is_empty())
        .then(|| format!("; cleanup evidence: {}", errors.join("; ")))
        .unwrap_or_default()
}

fn capture_snapshot_errors(captures: &CaptureFiles, label: &str) -> Vec<String> {
    ["stdout", "stderr"]
        .into_iter()
        .filter_map(|stream| {
            captures
                .snapshot(stream, label)
                .err()
                .map(|error| format!("{stream} capture snapshot: {}", error.message))
        })
        .collect()
}

fn failure(message: String) -> DeadlineProcessFailure {
    DeadlineProcessFailure { message }
}
