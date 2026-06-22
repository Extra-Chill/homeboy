use std::process::{Command, Stdio};

use crate::core::engine::invocation;

use super::super::process_cleanup::{
    active_cleanup_signal, configure_process_group_cleanup, interrupted_exit_code,
    stderr_with_interruption, ProcessGroupCleanupGuard,
};
use super::delegated::{
    stderr_with_delegated_failure, DelegatedRunFailureMonitor, DelegatedRunTerminalFailure,
};
use super::resource_monitor::ChildResourceMonitor;
use super::CommandOutput;

pub fn execute_local_command(command: &str) -> CommandOutput {
    execute_local_command_in_dir(command, None, None)
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
    execute_local_command_in_dir_impl(command, current_dir, env)
}

fn execute_local_command_in_dir_impl(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    use std::io::Read;
    use std::thread;

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
    configure_process_group_cleanup(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(child) => child,
        Err(e) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command error: {}", e),
                success: false,
                exit_code: -1,
                child_resource: None,
            };
        }
    };
    let mut cleanup_guard = Some(ProcessGroupCleanupGuard::new(child.id()));
    let _invocation_child_guard = invocation_child_guard(
        env,
        child.id(),
        cleanup_guard.as_ref().and_then(|guard| guard.pgid()),
        command,
    );
    let monitor = ChildResourceMonitor::start(child.id(), command.to_string());

    fn read_all<R: Read>(mut src: R) -> String {
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => captured.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).to_string()
    }

    let stdout_handle = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || read_all(pipe)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || read_all(pipe)));

    let (status, delegated_failure) =
        wait_for_child_or_delegated_failure(&mut child, env, &mut cleanup_guard);
    let interrupted_signal = active_cleanup_signal();

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    let output = match status {
        Ok(status) => CommandOutput {
            stdout,
            stderr: stderr_with_delegated_failure(
                stderr_with_interruption(stderr, interrupted_signal),
                delegated_failure.as_ref(),
            ),
            success: status.success()
                && interrupted_signal.is_none()
                && delegated_failure.is_none(),
            exit_code: interrupted_exit_code(interrupted_signal, status.code().unwrap_or(-1)),
            child_resource: Some(monitor.finish()),
        },
        Err(e) => CommandOutput {
            stdout,
            stderr: stderr_with_delegated_failure(
                stderr_with_interruption(
                    format!("{stderr}\nCommand error: {}", e),
                    interrupted_signal,
                ),
                delegated_failure.as_ref(),
            ),
            success: false,
            exit_code: interrupted_exit_code(interrupted_signal, -1),
            child_resource: Some(monitor.finish()),
        },
    };
    if let Some(cleanup_guard) = cleanup_guard.take() {
        cleanup_guard.cleanup();
    }
    output
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
    execute_local_command_passthrough_impl(command, current_dir, env)
}

fn execute_local_command_passthrough_impl(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    use std::io::{Read, Write};
    use std::thread;

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
    configure_process_group_cleanup(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command error: {}", e),
                success: false,
                exit_code: -1,
                child_resource: None,
            };
        }
    };
    let mut cleanup_guard = Some(ProcessGroupCleanupGuard::new(child.id()));
    let _invocation_child_guard = invocation_child_guard(
        env,
        child.id(),
        cleanup_guard.as_ref().and_then(|guard| guard.pgid()),
        command,
    );
    let monitor = ChildResourceMonitor::start(child.id(), command.to_string());

    // Tee each stream: copy every chunk to the parent's stdout/stderr as it
    // arrives (preserving the live-progress UX) while buffering it for the
    // caller. Using 4 KiB reads keeps latency low without excess syscalls.
    fn tee_to<R, W>(mut src: R, mut sink: W) -> String
    where
        R: Read,
        W: Write,
    {
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = sink.write_all(&buf[..n]);
                    let _ = sink.flush();
                    captured.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).to_string()
    }

    let stdout_handle = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || tee_to(pipe, std::io::stdout())));
    let stderr_handle = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || tee_to(pipe, std::io::stderr())));

    let (status, delegated_failure) =
        wait_for_child_or_delegated_failure(&mut child, env, &mut cleanup_guard);
    let interrupted_signal = active_cleanup_signal();

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    let output = match status {
        Ok(status) => CommandOutput {
            stdout,
            stderr: stderr_with_delegated_failure(
                stderr_with_interruption(stderr, interrupted_signal),
                delegated_failure.as_ref(),
            ),
            success: status.success()
                && interrupted_signal.is_none()
                && delegated_failure.is_none(),
            exit_code: interrupted_exit_code(interrupted_signal, status.code().unwrap_or(-1)),
            child_resource: Some(monitor.finish()),
        },
        Err(e) => CommandOutput {
            stdout,
            stderr: stderr_with_delegated_failure(
                stderr_with_interruption(
                    format!("{stderr}\nCommand error: {}", e),
                    interrupted_signal,
                ),
                delegated_failure.as_ref(),
            ),
            success: false,
            exit_code: interrupted_exit_code(interrupted_signal, -1),
            child_resource: Some(monitor.finish()),
        },
    };
    if let Some(cleanup_guard) = cleanup_guard.take() {
        cleanup_guard.cleanup();
    }
    output
}

pub(crate) fn execute_local_command_stderr_passthrough(
    command: &str,
    current_dir: Option<&str>,
    env: Option<&[(&str, &str)]>,
) -> CommandOutput {
    use std::io::{Read, Write};
    use std::thread;

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
    configure_process_group_cleanup(&mut cmd);

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return CommandOutput {
                stdout: String::new(),
                stderr: format!("Command error: {}", e),
                success: false,
                exit_code: -1,
                child_resource: None,
            };
        }
    };
    let mut cleanup_guard = Some(ProcessGroupCleanupGuard::new(child.id()));
    let _invocation_child_guard = invocation_child_guard(
        env,
        child.id(),
        cleanup_guard.as_ref().and_then(|guard| guard.pgid()),
        command,
    );
    let monitor = ChildResourceMonitor::start(child.id(), command.to_string());

    fn read_all<R: Read>(mut src: R) -> String {
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => captured.extend_from_slice(&buf[..n]),
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).to_string()
    }

    fn tee_to_stderr<R: Read>(mut src: R) -> String {
        let mut captured = Vec::new();
        let mut buf = [0u8; 4096];
        let mut sink = std::io::stderr();
        loop {
            match src.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = sink.write_all(&buf[..n]);
                    let _ = sink.flush();
                    captured.extend_from_slice(&buf[..n]);
                }
                Err(_) => break,
            }
        }
        String::from_utf8_lossy(&captured).to_string()
    }

    let stdout_handle = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || read_all(pipe)));
    let stderr_handle = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || tee_to_stderr(pipe)));

    let (status, delegated_failure) =
        wait_for_child_or_delegated_failure(&mut child, env, &mut cleanup_guard);
    let interrupted_signal = active_cleanup_signal();

    let stdout = stdout_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();
    let stderr = stderr_handle
        .and_then(|h| h.join().ok())
        .unwrap_or_default();

    let output = match status {
        Ok(status) => CommandOutput {
            stdout,
            stderr: stderr_with_delegated_failure(
                stderr_with_interruption(stderr, interrupted_signal),
                delegated_failure.as_ref(),
            ),
            success: status.success()
                && interrupted_signal.is_none()
                && delegated_failure.is_none(),
            exit_code: interrupted_exit_code(interrupted_signal, status.code().unwrap_or(-1)),
            child_resource: Some(monitor.finish()),
        },
        Err(e) => CommandOutput {
            stdout,
            stderr: stderr_with_delegated_failure(
                stderr_with_interruption(
                    format!("{stderr}\nCommand error: {}", e),
                    interrupted_signal,
                ),
                delegated_failure.as_ref(),
            ),
            success: false,
            exit_code: interrupted_exit_code(interrupted_signal, -1),
            child_resource: Some(monitor.finish()),
        },
    };
    if let Some(cleanup_guard) = cleanup_guard.take() {
        cleanup_guard.cleanup();
    }
    output
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
) -> (
    std::io::Result<std::process::ExitStatus>,
    Option<DelegatedRunTerminalFailure>,
) {
    let Some(monitor) = DelegatedRunFailureMonitor::from_env(env) else {
        return (child.wait(), None);
    };

    loop {
        match child.try_wait() {
            Ok(Some(status)) => return (Ok(status), None),
            Ok(None) => {}
            Err(error) => return (Err(error), None),
        }

        if let Some(failure) = monitor.terminal_failure() {
            if let Some(cleanup_guard) = cleanup_guard.take() {
                cleanup_guard.cleanup();
            }
            return (child.wait(), Some(failure));
        }

        std::thread::sleep(monitor.poll_interval);
    }
}
