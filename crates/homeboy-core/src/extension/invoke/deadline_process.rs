use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::time::{Duration, Instant};

#[cfg(not(windows))]
use homeboy_engine_primitives::command::terminate_process_tree_and_reap;
use homeboy_engine_primitives::command::{terminate_remaining_process_group, ControllerChildGuard};
use tempfile::NamedTempFile;

use crate::process::{force_terminate_process_tree_bounded, ProcessContainment};

pub(crate) struct DeadlineProcessOutput {
    pub status: ExitStatus,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

/// `Debug` so a test asserting on a deadline-process result can name the
/// failure it got. Without it `homeboy-core`'s test target does not compile at
/// all, which takes every other test in the crate down with it.
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
    let mut containment = ProcessContainment::prepare(&mut command)
        .map_err(|error| failure(format!("{label} containment setup failed: {error}")))?;
    let mut guard = Some(
        ControllerChildGuard::prepare(&mut command)
            .map_err(|error| failure(format!("{label} containment setup failed: {error}")))?,
    );
    let mut child = command.spawn().map_err(|error| {
        failure(format!(
            "{label} spawn failed; capture files will be removed: {error}"
        ))
    })?;
    if let Err(error) = containment.attach(&child) {
        let errors = cleanup(&containment, &mut child, &mut guard, false, cleanup_budget);
        return Err(failure(format!(
            "{label} containment attach failed: {error}{}",
            cleanup_diagnostic(&errors)
        )));
    }
    if let Err(error) = guard.as_ref().expect("guard exists").attach(&child) {
        let errors = cleanup(&containment, &mut child, &mut guard, false, cleanup_budget);
        return Err(failure(format!(
            "{label} containment attach failed: {error}{}",
            cleanup_diagnostic(&errors)
        )));
    }
    let mut stdin = child.stdin.take().ok_or_else(|| {
        let errors = cleanup(&containment, &mut child, &mut guard, false, cleanup_budget);
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
        let errors = cleanup(&containment, &mut child, &mut guard, false, cleanup_budget);
        return Err(failure(format!(
            "{label} stdin write failed.{}",
            cleanup_diagnostic(&errors)
        )));
    }
    drop(stdin);

    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                let errors = cleanup(&containment, &mut child, &mut guard, true, cleanup_budget);
                if !errors.is_empty() {
                    return Err(failure(format!(
                        "{label} exited but cleanup could not be verified: {}",
                        errors.join("; ")
                    )));
                }
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
                let mut errors =
                    cleanup(&containment, &mut child, &mut guard, false, cleanup_budget);
                errors.extend(capture_snapshot_errors(&captures, label));
                return Err(failure(format!(
                    "{label} timed out; capture files were snapshotted without waiting for inherited handles{}.",
                    cleanup_diagnostic(&errors)
                )));
            }
            Err(error) => {
                let mut errors =
                    cleanup(&containment, &mut child, &mut guard, false, cleanup_budget);
                errors.extend(capture_snapshot_errors(&captures, label));
                return Err(failure(format!(
                    "{label} wait failed: {error}; capture files were snapshotted without waiting for inherited handles{}.",
                    cleanup_diagnostic(&errors)
                )));
            }
        }
    }
}

fn cleanup(
    containment: &ProcessContainment,
    child: &mut std::process::Child,
    guard: &mut Option<ControllerChildGuard>,
    leader_has_exited: bool,
    cleanup_budget: Duration,
) -> Vec<String> {
    drop(guard.take());
    let mut errors = Vec::new();
    if !leader_has_exited {
        if let Err(error) = containment.terminate_on_failure_bounded(cleanup_budget, false) {
            errors.push(format!("containment termination: {error}"));
        }
        if let Err(error) = force_terminate_process_tree_bounded(child.id(), cleanup_budget) {
            errors.push(format!("process-tree termination: {error}"));
        }
        if let Err(error) = reap_child_after_bounded_cleanup(child, cleanup_budget) {
            errors.push(format!("child reap: {error}"));
        }
    }
    #[cfg(target_os = "linux")]
    if let Err(error) = containment.cleanup_after_leader_exit_bounded(cleanup_budget) {
        errors.push(format!("descendant cleanup: {error}"));
    }
    if let Err(error) = terminate_remaining_process_group(child.id()) {
        errors.push(format!("remaining process-group cleanup: {error}"));
    }
    errors
}

#[cfg(not(windows))]
fn reap_child_after_bounded_cleanup(
    child: &mut std::process::Child,
    _cleanup_budget: Duration,
) -> std::io::Result<()> {
    terminate_process_tree_and_reap(child).map(|_| ())
}

#[cfg(windows)]
fn reap_child_after_bounded_cleanup(
    child: &mut std::process::Child,
    cleanup_budget: Duration,
) -> std::io::Result<()> {
    let pid = child.id();
    let deadline = Instant::now() + cleanup_budget;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) if Instant::now() >= deadline => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!(
                        "child {pid} remained alive after bounded cleanup for {} ms",
                        cleanup_budget.as_millis()
                    ),
                ));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(error) => return Err(error),
        }
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
