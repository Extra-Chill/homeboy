//! Shared process-table observation and cycle-safe ancestry traversal.
use std::{collections::HashSet, io, process::Command};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProcessRow {
    pub pid: u32,
    pub parent_pid: u32,
    pub state: Option<String>,
    pub rss_kib: Option<u64>,
    pub cpu_percent: Option<f64>,
    pub elapsed_seconds: u64,
    pub uid: u32,
    pub start: String,
    pub command: String,
    pub executable: String,
    /// Kernel identity and cumulative CPU counters, when available.
    pub started_at: (u64, u64),
    pub cpu_time: Option<u64>,
}

/// One wide observation, shared by diagnostics and lifecycle callers. Environment
/// is requested only by containment cleanup; it can contain sensitive values.
pub fn snapshot(include_environment: bool) -> io::Result<Vec<ProcessRow>> {
    let mut command = Command::new("ps");
    command.arg("-axww");
    if include_environment {
        #[cfg(target_os = "macos")]
        command.arg("-E");
        #[cfg(not(target_os = "macos"))]
        command.arg("e");
    }
    command.args([
        "-o",
        "pid=,ppid=,stat=,rss=,%cpu=,etime=,uid=,lstart=,comm=",
        "-o",
        "args=",
    ]);
    let output = command.env("LC_ALL", "C").output()?;
    if !output.status.success() {
        return Err(io::Error::other(
            String::from_utf8_lossy(&output.stderr).trim().to_string(),
        ));
    }
    let rows = parse_snapshot(&String::from_utf8_lossy(&output.stdout));
    #[cfg(target_os = "macos")]
    let rows = rows
        .into_iter()
        .map(|mut row| {
            enrich_native_identity(&mut row);
            row
        })
        .collect();
    Ok(rows)
}

pub fn parse_snapshot(output: &str) -> Vec<ProcessRow> {
    output
        .lines()
        .filter_map(|line| {
            let fields: Vec<_> = line.split_whitespace().collect();
            Some(ProcessRow {
                pid: fields.first()?.parse().ok()?,
                parent_pid: fields.get(1)?.parse().ok()?,
                state: Some(fields.get(2)?.to_string()),
                rss_kib: fields.get(3)?.parse().ok(),
                cpu_percent: fields.get(4)?.parse().ok(),
                elapsed_seconds: parse_ps_elapsed_seconds(fields.get(5)?)?,
                uid: fields.get(6)?.parse().ok()?,
                start: fields.get(7..12)?.join(" "),
                executable: fields.get(12)?.to_string(),
                command: remainder_after_fields(line, 13).trim().to_string(),
                ..ProcessRow::default()
            })
        })
        .collect()
}

/// Descendants with hop depth, excluding the root, in breadth-first order.
/// A visited set makes torn snapshots (including cycles through root) safe.
pub fn descendants(root: u32, parents: impl IntoIterator<Item = (u32, u32)>) -> Vec<(u32, usize)> {
    let parents: Vec<_> = parents.into_iter().collect();
    let mut seen = HashSet::from([root]);
    let mut queue = vec![(root, 0)];
    let mut cursor = 0;
    while cursor < queue.len() {
        let (parent, depth) = queue[cursor];
        for &(pid, ppid) in &parents {
            if ppid == parent && seen.insert(pid) {
                queue.push((pid, depth + 1));
            }
        }
        cursor += 1;
    }
    queue.remove(0);
    queue
}

/// Ancestors nearest first, excluding PID 0 and stopping on cycles.
pub fn ancestors(root: u32, parents: impl IntoIterator<Item = (u32, u32)>) -> Vec<u32> {
    let parents: std::collections::HashMap<_, _> = parents.into_iter().collect();
    let mut seen = HashSet::from([root]);
    let mut current = root;
    let mut found = Vec::new();
    while let Some(&parent) = parents.get(&current) {
        if parent == 0 || !seen.insert(parent) {
            break;
        }
        found.push(parent);
        current = parent;
    }
    found
}

pub fn parse_ps_elapsed_seconds(etime: &str) -> Option<u64> {
    let (days, clock) = match etime.trim().split_once('-') {
        Some((days, clock)) => (days.parse::<u64>().ok()?, clock),
        None => (0, etime.trim()),
    };
    let mut parts = clock.split(':').rev();
    let seconds: u64 = parts.next()?.parse().ok()?;
    let minutes: u64 = parts.next().map(str::parse).transpose().ok()?.unwrap_or(0);
    let hours: u64 = parts.next().map(str::parse).transpose().ok()?.unwrap_or(0);
    if parts.next().is_some() {
        return None;
    }
    days.checked_mul(86400)?
        .checked_add(hours.checked_mul(3600)?)?
        .checked_add(minutes.checked_mul(60)?)?
        .checked_add(seconds)
}

fn remainder_after_fields(line: &str, count: usize) -> &str {
    let mut rest = line.trim_start();
    for _ in 0..count {
        match rest.find(char::is_whitespace) {
            Some(index) => rest = rest[index..].trim_start(),
            None => return "",
        }
    }
    rest
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LinuxProcStat {
    pub(crate) state: char,
    pub(crate) parent_pid: u32,
    pub(crate) starttime_ticks: u64,
    pub(crate) cpu_ticks: u64,
}

#[cfg(target_os = "linux")]
pub(crate) fn linux_process_stat(pid: u32) -> Option<LinuxProcStat> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    parse_linux_proc_stat(&stat)
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_linux_proc_stat(stat: &str) -> Option<LinuxProcStat> {
    let after_command = stat.rsplit_once(") ")?.1;
    let mut fields = after_command.split_whitespace();
    let state = fields.next()?.chars().next()?;
    let parent_pid = fields.next()?.parse().ok()?;
    let starttime_ticks = after_command.split_whitespace().nth(19)?.parse().ok()?;
    let user_ticks: u64 = after_command.split_whitespace().nth(11)?.parse().ok()?;
    let system_ticks: u64 = after_command.split_whitespace().nth(12)?.parse().ok()?;
    Some(LinuxProcStat {
        state,
        parent_pid,
        starttime_ticks,
        cpu_ticks: user_ticks.saturating_add(system_ticks),
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn supervisor_signal_children(signal: libc::c_int) -> bool {
    let mut path = [0u8; 64];
    let prefix = b"/proc/self/task/";
    path[..prefix.len()].copy_from_slice(prefix);
    let mut pid_buf = [0u8; 12];
    let pid = crate::command::write_u32_nul(&mut pid_buf, unsafe { libc::getpid() } as u32);
    let pid_bytes = unsafe { std::ffi::CStr::from_ptr(pid) }.to_bytes();
    let mut offset = prefix.len();
    path[offset..offset + pid_bytes.len()].copy_from_slice(pid_bytes);
    offset += pid_bytes.len();
    let suffix = b"/children";
    path[offset..offset + suffix.len()].copy_from_slice(suffix);
    path[offset + suffix.len()] = 0;
    let fd = unsafe { libc::open(path.as_ptr().cast(), libc::O_RDONLY | libc::O_CLOEXEC) };
    if fd < 0 {
        return false;
    }
    let mut delivered = false;
    let self_pid = unsafe { libc::getpid() } as u32;
    let mut buf = [0u8; 4096];
    let mut pid = 0u32;
    let mut in_pid = false;
    loop {
        let read = unsafe { libc::read(fd, buf.as_mut_ptr().cast(), buf.len()) };
        if read == 0 {
            break;
        }
        if read < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        for &byte in &buf[..read as usize] {
            if byte.is_ascii_digit() {
                in_pid = true;
                pid = pid.saturating_mul(10).saturating_add((byte - b'0') as u32);
            } else if in_pid {
                if pid != 0 && pid != self_pid {
                    delivered |= unsafe { libc::kill(pid as libc::pid_t, signal) == 0 };
                }
                pid = 0;
                in_pid = false;
            }
        }
    }
    unsafe {
        libc::close(fd);
    }
    if in_pid && pid != 0 && pid != self_pid {
        delivered |= unsafe { libc::kill(pid as libc::pid_t, signal) == 0 };
    }
    delivered
}

/// Kernel identity used to revalidate Linux cleanup targets against PID reuse.
#[cfg(target_os = "linux")]
pub fn linux_identity(pid: u32) -> Option<(u32, String, u32)> {
    use std::os::unix::fs::MetadataExt;
    let root = std::path::PathBuf::from(format!("/proc/{pid}"));
    let before = linux_process_stat(pid)?;
    let uid = root.metadata().ok()?.uid();
    let after = linux_process_stat(pid)?;
    (before.starttime_ticks == after.starttime_ticks)
        .then(|| (after.parent_pid, after.starttime_ticks.to_string(), uid))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_walks_are_cycle_safe_and_preserve_depth() {
        let parents = [(10, 12), (11, 10), (12, 11), (13, 10), (20, 1)];
        assert_eq!(descendants(10, parents), vec![(11, 1), (13, 1), (12, 2)]);
        assert_eq!(ancestors(10, parents), vec![12, 11]);
    }

    #[test]
    fn snapshot_preserves_arguments_and_resource_units() {
        let rows = parse_snapshot(
            "42 1 S 2048 2.5 2-01:02:03 1000 Wed Nov 5 12:00:00 2026 sleep sleep  30\n",
        );
        assert_eq!(rows.len(), 1);
        let row = &rows[0];
        assert_eq!(row.command, "sleep  30");
        assert_eq!(row.rss_kib, Some(2048));
        assert_eq!(row.cpu_percent, Some(2.5));
        assert_eq!(row.elapsed_seconds, 176523);
        assert_eq!(row.start, "Wed Nov 5 12:00:00 2026");
    }
}

#[cfg(target_os = "macos")]
fn enrich_native_identity(row: &mut ProcessRow) {
    unsafe {
        let mut bsd: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of_val(&bsd) as i32;
        if libc::proc_pidinfo(
            row.pid as i32,
            libc::PROC_PIDTBSDINFO,
            0,
            (&mut bsd as *mut libc::proc_bsdinfo).cast(),
            size,
        ) != size
        {
            // A process may exit (or be inaccessible) during enumeration.
            return;
        }
        let mut task: libc::proc_taskinfo = std::mem::zeroed();
        let size = std::mem::size_of_val(&task) as i32;
        let cpu_time = (libc::proc_pidinfo(
            row.pid as i32,
            libc::PROC_PIDTASKINFO,
            0,
            (&mut task as *mut libc::proc_taskinfo).cast(),
            size,
        ) == size)
            .then(|| task.pti_total_user.saturating_add(task.pti_total_system));
        row.started_at = (bsd.pbi_start_tvsec, bsd.pbi_start_tvusec);
        row.cpu_time = cpu_time;
    }
}
