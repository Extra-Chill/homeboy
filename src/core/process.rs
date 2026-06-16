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
