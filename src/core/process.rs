use crate::core::error::{Error, Result};
use std::path::{Path, PathBuf};
#[cfg(unix)]
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// Generic process step shape shared by command/runner adapters.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessStep {
    pub program: String,
    pub args: Vec<String>,
    pub working_dir: Option<PathBuf>,
}

impl ProcessStep {
    pub fn new(program: impl Into<String>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            working_dir: None,
        }
    }

    pub fn args(mut self, args: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.args = args.into_iter().map(Into::into).collect();
        self
    }

    pub fn working_dir(mut self, working_dir: impl Into<PathBuf>) -> Self {
        self.working_dir = Some(working_dir.into());
        self
    }
}

/// Resolve a process step's working directory inside a containment root.
///
/// When a step omits a working directory, the normalized root becomes the
/// effective working directory. This gives local commands, extensions, and rigs
/// one reusable path policy without requiring filesystem canonicalization.
pub(crate) fn prepare_contained_process_step(
    root: impl AsRef<Path>,
    step: ProcessStep,
) -> Result<ProcessStep> {
    let root = crate::core::paths::normalize_local_path(root);
    let working_dir = match step.working_dir.as_deref() {
        Some(working_dir) => {
            crate::core::paths::resolve_contained_local_path(&root, working_dir, "working_dir")?
        }
        None => root,
    };

    Ok(ProcessStep {
        working_dir: Some(working_dir),
        ..step
    })
}

pub fn pid_is_running(pid: u32) -> bool {
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

/// Install a Ctrl-C / SIGINT handler that flips `stop` to `true` on the first
/// signal, giving long-running loops a cooperative shutdown flag. The `context`
/// label is woven into the error message so callers (reverse runner worker,
/// preview client, ...) surface a distinct diagnostic on failure (#5092).
pub fn install_shutdown_handler(stop: Arc<AtomicBool>, context: &str) -> Result<()> {
    let context = context.to_string();
    ctrlc::set_handler(move || {
        stop.store(true, Ordering::SeqCst);
    })
    .map_err(|err| Error::internal_unexpected(format!("install {context} signal handler: {err}")))
}

#[cfg(test)]
mod containment_tests {
    use super::*;

    #[test]
    fn process_step_defaults_to_root_working_dir() {
        let step = prepare_contained_process_step("/repo/./worktree", ProcessStep::new("cargo"))
            .expect("prepared step");

        assert_eq!(step.working_dir, Some(PathBuf::from("/repo/worktree")));
    }

    #[test]
    fn process_step_accepts_relative_working_dir_inside_root() {
        let step = prepare_contained_process_step(
            "/repo/worktree",
            ProcessStep::new("cargo")
                .args(["test"])
                .working_dir("crates/core/.."),
        )
        .expect("prepared step");

        assert_eq!(step.program, "cargo");
        assert_eq!(step.args, vec!["test".to_string()]);
        assert_eq!(
            step.working_dir,
            Some(PathBuf::from("/repo/worktree/crates"))
        );
    }

    #[test]
    fn process_step_rejects_working_dir_escape() {
        let err = prepare_contained_process_step(
            "/repo/worktree",
            ProcessStep::new("cargo").working_dir("../outside"),
        )
        .expect_err("cwd escape should fail");

        assert!(err.to_string().contains("escapes root '/repo/worktree'"));
    }
}

pub fn process_group_is_running(pgid: i32) -> bool {
    if pgid <= 0 {
        return false;
    }

    #[cfg(target_os = "linux")]
    if let Some(running) = linux_process_group_has_running_member(pgid) {
        return running;
    }

    #[cfg(unix)]
    unsafe {
        libc::kill(-(pgid as libc::pid_t), 0) == 0
    }

    #[cfg(not(unix))]
    {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTreeTermination {
    pub owner_pid: u32,
    pub descendant_pids: Vec<u32>,
    pub signalled_pids: Vec<u32>,
    pub signal: &'static str,
    pub recovery_commands: Vec<String>,
}

pub fn terminate_process_tree(owner_pid: u32) -> Result<ProcessTreeTermination> {
    if owner_pid > i32::MAX as u32 {
        return Err(Error::validation_invalid_argument(
            "pid",
            format!("pid {} is outside the supported Unix pid range", owner_pid),
            Some(owner_pid.to_string()),
            None,
        ));
    }

    #[cfg(unix)]
    {
        let descendant_pids = unix_descendant_pids(owner_pid)?;
        let current_pid = std::process::id();
        let mut targets = descendant_pids.clone();
        targets.push(owner_pid);
        targets.retain(|pid| *pid != current_pid);
        targets.sort_unstable();
        targets.dedup();

        for pid in targets.iter().rev() {
            unsafe {
                if libc::kill(*pid as libc::pid_t, libc::SIGTERM) != 0 {
                    let err = std::io::Error::last_os_error();
                    if err.raw_os_error() != Some(libc::ESRCH) {
                        return Err(Error::internal_unexpected(format!(
                            "failed to signal pid {} with SIGTERM: {}",
                            pid, err
                        )));
                    }
                }
            }
        }

        let recovery_commands = if targets.is_empty() {
            Vec::new()
        } else {
            vec![format!(
                "kill -TERM {}",
                targets
                    .iter()
                    .map(u32::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            )]
        };

        return Ok(ProcessTreeTermination {
            owner_pid,
            descendant_pids,
            signalled_pids: targets,
            signal: "SIGTERM",
            recovery_commands,
        });
    }

    #[cfg(not(unix))]
    {
        let _ = owner_pid;
        Err(Error::validation_invalid_argument(
            "pid",
            "process-tree cancellation is only supported on Unix hosts",
            None,
            None,
        ))
    }
}

#[cfg(unix)]
fn unix_descendant_pids(owner_pid: u32) -> Result<Vec<u32>> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid="])
        .output()
        .map_err(|error| {
            Error::internal_unexpected(format!("failed to inspect process tree with ps: {error}"))
        })?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "failed to inspect process tree with ps: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(descendant_pids_from_ps(&stdout, owner_pid))
}

#[cfg(unix)]
fn descendant_pids_from_ps(ps_output: &str, owner_pid: u32) -> Vec<u32> {
    let rows: Vec<(u32, u32)> = ps_output
        .lines()
        .filter_map(|line| {
            let mut fields = line.split_whitespace();
            let pid = fields.next()?.parse().ok()?;
            let ppid = fields.next()?.parse().ok()?;
            Some((pid, ppid))
        })
        .collect();
    let mut descendants = Vec::new();
    let mut frontier = vec![owner_pid];
    while let Some(parent) = frontier.pop() {
        for (pid, ppid) in &rows {
            if *ppid == parent && !descendants.contains(pid) {
                descendants.push(*pid);
                frontier.push(*pid);
            }
        }
    }
    descendants
}

#[cfg(target_os = "linux")]
fn linux_process_state(pid: u32) -> Option<char> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_linux_stat(&stat).map(|process| process.state)
}

#[cfg(target_os = "linux")]
fn linux_process_group_has_running_member(pgid: i32) -> Option<bool> {
    let entries = std::fs::read_dir("/proc").ok()?;
    for entry in entries.flatten() {
        let file_name = entry.file_name();
        let Some(name) = file_name.to_str() else {
            continue;
        };
        if !name.bytes().all(|byte| byte.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) else {
            continue;
        };
        let Some(process) = parse_linux_stat(&stat) else {
            continue;
        };
        if process.process_group_id != pgid {
            continue;
        }
        if process.state != 'Z' {
            return Some(true);
        }
    }
    Some(false)
}

#[cfg(target_os = "linux")]
struct LinuxProcessStat {
    state: char,
    process_group_id: i32,
}

#[cfg(target_os = "linux")]
fn parse_linux_stat(stat: &str) -> Option<LinuxProcessStat> {
    let after_command = stat.rsplit_once(") ")?.1;
    let mut fields = after_command.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let _parent_pid = fields.next()?;
    let process_group_id = fields.next()?.parse().ok()?;
    Some(LinuxProcessStat {
        state,
        process_group_id,
    })
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn parse_linux_stat_handles_command_names_with_spaces() {
        let stat = "123 (name with spaces) Z 1 456 456 0 -1 0 0 0";
        let process = parse_linux_stat(stat).expect("process stat");

        assert_eq!(process.state, 'Z');
        assert_eq!(process.process_group_id, 456);
    }
}

#[cfg(all(test, unix))]
mod process_tree_tests {
    use super::*;

    #[test]
    fn descendant_pids_from_ps_walks_nested_children() {
        let ps = "10 1\n11 10\n12 11\n13 10\n20 1\n";

        let mut descendants = descendant_pids_from_ps(ps, 10);
        descendants.sort_unstable();

        assert_eq!(descendants, vec![11, 12, 13]);
    }
}
