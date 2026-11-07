//! Best-effort release of env-inheriting, orphaned run processes (not a sweep).
use std::{
    collections::{HashMap, HashSet},
    path::Path,
    time::Duration,
};

#[derive(Clone, Debug)]
struct Process {
    pid: u32,
    parent: u32,
    uid: u32,
    start: String,
    command: String,
    attributed: bool,
}

#[cfg(any(target_os = "linux", test))]
fn environ_matches(bytes: &[u8], run: &str) -> bool {
    let expected = format!("HOMEBOY_RUN_ID={run}");
    bytes
        .split(|b| *b == 0)
        .any(|entry| entry == expected.as_bytes())
}

// comm can contain spaces and parentheses. Fields after its final ')' start at 3.
#[cfg(target_os = "linux")]
fn parse_stat(stat: &str) -> Option<(u32, String)> {
    let fields: Vec<_> = stat.rsplit_once(')')?.1.split_whitespace().collect();
    Some((fields.get(1)?.parse().ok()?, fields.get(19)?.to_string()))
}

#[cfg(target_os = "linux")]
fn inspect(pid: u32, run: &str) -> Option<Process> {
    use std::os::unix::fs::MetadataExt;
    let root = std::path::PathBuf::from(format!("/proc/{pid}"));
    let uid = root.metadata().ok()?.uid();
    let (parent, start) = parse_stat(&std::fs::read_to_string(root.join("stat")).ok()?)?;
    let args = std::fs::read(root.join("cmdline")).ok()?;
    let command = String::from_utf8_lossy(&args).replace('\0', " ");
    let attributed =
        std::fs::read(root.join("environ")).is_ok_and(|bytes| environ_matches(&bytes, run));
    if parse_stat(&std::fs::read_to_string(root.join("stat")).ok()?)?.1 != start {
        return None;
    }
    Some(Process {
        pid,
        parent,
        uid,
        start,
        command,
        attributed,
    })
}

// ps emits uid, ppid, pid, five lstart fields, then command and environment.
#[cfg(any(target_os = "macos", test))]
fn parse_ps(line: &str, run: &str) -> Option<Process> {
    let fields: Vec<_> = line.split_whitespace().collect();
    let command = fields.get(8..)?.join(" ");
    let expected = format!("HOMEBOY_RUN_ID={run}");
    Some(Process {
        uid: fields.first()?.parse().ok()?,
        parent: fields.get(1)?.parse().ok()?,
        pid: fields.get(2)?.parse().ok()?,
        start: fields.get(3..8)?.join(" "),
        attributed: command.split_whitespace().any(|entry| entry == expected),
        command,
    })
}

#[cfg(target_os = "macos")]
fn ps(args: &[&str], run: &str) -> Vec<Process> {
    let Ok(output) = std::process::Command::new("/bin/ps")
        .args(args)
        .args(["-ww", "-o", "uid=,ppid=,pid=,lstart=,command="])
        .output()
    else {
        return Vec::new();
    };
    if !output.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&output.stdout)
        .lines()
        .filter_map(|line| parse_ps(line, run))
        .collect()
}

#[cfg(target_os = "macos")]
fn inspect(pid: u32, run: &str) -> Option<Process> {
    ps(&["-E", "-p", &pid.to_string()], run)
        .into_iter()
        .find(|p| p.pid == pid)
}

fn snapshot(run: &str) -> HashMap<u32, Process> {
    #[cfg(target_os = "linux")]
    let processes: Vec<_> = std::fs::read_dir("/proc")
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok())
        .filter_map(|pid| inspect(pid, run))
        .collect();
    // One bulk scan, not one ps spawn per system PID.
    #[cfg(target_os = "macos")]
    let processes = ps(&["-axE"], run);
    processes.into_iter().map(|p| (p.pid, p)).collect()
}

fn homeboy(command: &str) -> bool {
    let executable = Path::new(command.split_whitespace().next().unwrap_or(""))
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("");
    // All subcommands, including Cook and continuation/execution workers.
    executable == "homeboy" || executable.starts_with("homeboy-")
}

fn excluded(process: &Process, uid: u32, protected: &HashSet<u32>) -> bool {
    process.pid <= 1
        || process.uid != uid
        || protected.contains(&process.pid)
        || homeboy(&process.command)
}

// Conservatively release only orphan trees. A live Homeboy ancestor may be
// finalizing (git push/gh pr create), even when it is a sibling of this caller.
// Unknown ancestry fails closed. Live provider/gate trees are left alone too;
// their background leftovers become eligible once reparented to the system root.
fn orphaned(
    process: &Process,
    mut lookup: impl FnMut(u32) -> Option<Process>,
    protected: &HashSet<u32>,
) -> bool {
    let mut parent = process.parent;
    let mut seen = HashSet::from([process.pid]);
    while parent > 1 {
        if !seen.insert(parent) || protected.contains(&parent) {
            return false;
        }
        let Some(ancestor) = lookup(parent) else {
            return false;
        };
        if homeboy(&ancestor.command) {
            return false;
        }
        parent = ancestor.parent;
    }
    true
}

fn same_process(before: &Process, now: &Process) -> bool {
    before.pid == now.pid && before.start == now.start && before.uid == now.uid
}

fn daemon_pids() -> HashSet<u32> {
    let mut pids = HashSet::new();
    if let Ok(path) = homeboy_core::paths::daemon_state_file() {
        if let Ok(bytes) = std::fs::read(path) {
            if let Ok(state) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(pid) = state.get("pid").and_then(|v| v.as_u64()) {
                    if let Ok(pid) = u32::try_from(pid) {
                        pids.insert(pid);
                    }
                }
            }
        }
    }
    pids
}

fn signal(process: &Process, run: &str, uid: u32, protected: &HashSet<u32>, signal: i32) {
    let Some(now) = inspect(process.pid, run) else {
        return;
    };
    let mut protected = protected.clone();
    protected.extend(daemon_pids());
    if same_process(process, &now)
        && now.attributed
        && !excluded(&now, uid, &protected)
        && orphaned(&now, |pid| inspect(pid, run), &protected)
    {
        // Re-check identity and live ancestry before each signal.
        unsafe {
            libc::kill(now.pid as libc::pid_t, signal);
        }
    }
}

/// Missing/unreadable process data fails closed; cleanup must not prevent
/// durable terminalization. Only local env-inheriting orphan trees are released.
pub(super) fn release(run: &str) {
    if run.is_empty() {
        return;
    }
    let uid = unsafe { libc::geteuid() };
    let all = snapshot(run);
    let mut protected = daemon_pids();
    let mut pid = std::process::id();
    while protected.insert(pid) {
        let Some(process) = all.get(&pid) else {
            return;
        };
        pid = process.parent;
        if pid <= 1 {
            break;
        }
    }
    let processes: Vec<_> = all
        .values()
        .filter(|p| {
            p.attributed
                && !excluded(p, uid, &protected)
                && orphaned(p, |pid| all.get(&pid).cloned(), &protected)
        })
        .collect();
    for process in &processes {
        signal(process, run, uid, &protected, libc::SIGTERM);
    }
    if !processes.is_empty() {
        std::thread::sleep(Duration::from_millis(200));
    }
    for process in &processes {
        signal(process, run, uid, &protected, libc::SIGKILL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Process {
        parse_ps(
            "1000 1 42 Wed Nov 5 12:00:00 2026 /usr/bin/sleep 30 HOMEBOY_RUN_ID=run",
            "run",
        )
        .unwrap()
    }

    #[test]
    fn environment_and_ps_fixtures() {
        assert!(environ_matches(b"X=1\0HOMEBOY_RUN_ID=run\0", "run"));
        assert!(!environ_matches(b"HOMEBOY_RUN_ID=run-extra\0", "run"));
        assert!(!environ_matches(b"X=HOMEBOY_RUN_ID=run\0", "run"));
        let process = fixture();
        assert_eq!(process.pid, 42);
        assert_eq!(process.parent, 1);
        assert_eq!(process.start, "Wed Nov 5 12:00:00 2026");
        assert!(process.attributed);
        assert!(
            !parse_ps(
                "1000 4 42 Wed Nov 5 12:00:00 2026 sleep HOMEBOY_RUN_ID=runner",
                "run"
            )
            .unwrap()
            .attributed
        );
        assert!(parse_ps("bad", "run").is_none());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn stat_fixture() {
        assert_eq!(
            parse_stat("42 (weird ) name) S 4 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 0 123 0"),
            Some((4, "123".into()))
        );
        assert!(parse_stat("42 (truncated)").is_none());
    }

    #[test]
    fn exclusions_and_pid_reuse() {
        let process = fixture();
        assert!(!excluded(&process, 1000, &HashSet::new()));
        assert!(excluded(&process, 1001, &HashSet::new()));
        assert!(excluded(&process, 1000, &HashSet::from([42])));
        for command in [
            "/usr/bin/homeboy daemon serve",
            "homeboy supervise",
            "homeboy runner-service",
            "homeboy-abc serve",
            "homeboy cook",
            "homeboy agent-task execution-step",
        ] {
            assert!(homeboy(command));
        }
        assert!(!homeboy("python serve"));
        assert!(!homeboy("gh pr create"));
        assert!(same_process(&process, &process));
        let mut reused = process.clone();
        reused.start = "new start".into();
        assert!(!same_process(&process, &reused));
        reused = process.clone();
        reused.uid = 1001;
        assert!(!same_process(&process, &reused));
    }

    #[test]
    fn sibling_continuation_worker_and_finalization_children_are_protected() {
        let mut worker = fixture();
        worker.command = "homeboy agent-task terminal-continuation-worker".into();
        let mut git = fixture();
        git.pid = 43;
        git.parent = worker.pid;
        git.command = "git push".into();
        let all = HashMap::from([(worker.pid, worker.clone()), (git.pid, git.clone())]);
        let protected = HashSet::new(); // Neither is this caller or its ancestor.
        assert!(excluded(&worker, 1000, &protected));
        assert!(!orphaned(&git, |pid| all.get(&pid).cloned(), &protected));
        git.command = "gh pr create".into();
        assert!(!orphaned(&git, |pid| all.get(&pid).cloned(), &protected));
        assert!(!orphaned(&git, |_| None, &protected));
        git.parent = 1;
        assert!(orphaned(&git, |_| None, &protected));
        git.parent = worker.pid;
        worker.command = "bash".into();
        assert!(!orphaned(
            &git,
            |_| Some(worker.clone()),
            &HashSet::from([worker.pid])
        ));
    }
}
