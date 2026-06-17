use crate::core::error::Result;
use std::path::{Path, PathBuf};

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
pub fn prepare_contained_process_step(
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
