use homeboy_error::{Error, Result};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

/// Prove a live process owns one exact environment value.
///
/// This is intended for persisted random ownership tokens. Callers should
/// re-check immediately before signaling because PIDs can be reused.
pub fn pid_has_environment_value(pid: u32, key: &str, value: &str) -> Result<bool> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(Error::validation_invalid_argument(
            "pid",
            "recorded process PID is invalid",
            Some(pid.to_string()),
            None,
        ));
    }
    if key.is_empty()
        || key.contains('=')
        || key.chars().any(char::is_whitespace)
        || value.chars().any(char::is_whitespace)
    {
        return Err(Error::validation_invalid_argument(
            "process_environment",
            "process environment ownership checks require a non-empty key and whitespace-free value",
            Some(key.to_string()),
            None,
        ));
    }

    #[cfg(target_os = "linux")]
    {
        let expected = format!("{key}={value}");
        match std::fs::read(format!("/proc/{pid}/environ")) {
            Ok(environment) => {
                return Ok(environment_contains_assignment(
                    &environment,
                    expected.as_bytes(),
                ));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(format!("inspect process {pid} environment")),
                ));
            }
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        Err(Error::validation_invalid_argument(
            "pid",
            "exact process environment ownership checks require Linux /proc evidence",
            Some(pid.to_string()),
            None,
        ))
    }
}

#[cfg(target_os = "linux")]
fn environment_contains_assignment(environment: &[u8], expected: &[u8]) -> bool {
    environment
        .split(|byte| *byte == 0)
        .any(|entry| entry == expected)
}

/// Send SIGTERM to a recorded PID and prove it exited within `timeout`.
/// Platform-specific signaling stays in this process abstraction so lifecycle
/// callers do not need to issue ad hoc shell commands.
pub fn terminate_pid_with_sigterm_and_wait(pid: u32, timeout: Duration) -> Result<()> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(Error::validation_invalid_argument(
            "pid",
            "recorded process PID is invalid",
            Some(pid.to_string()),
            None,
        ));
    }

    #[cfg(unix)]
    {
        unsafe {
            if libc::kill(pid as libc::pid_t, libc::SIGTERM) != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() == Some(libc::ESRCH) {
                    return Ok(());
                }
                return Err(Error::internal_io(
                    error.to_string(),
                    Some(format!("send SIGTERM to process {pid}")),
                ));
            }
        }
        let deadline = Instant::now() + timeout;
        while process_is_running_after_sigterm(pid) {
            if Instant::now() >= deadline {
                return Err(Error::internal_unexpected(format!(
                    "process {pid} remained alive for {}ms after SIGTERM",
                    timeout.as_millis()
                )));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        return Ok(());
    }

    #[cfg(not(unix))]
    {
        let _ = timeout;
        Err(Error::validation_invalid_argument(
            "pid",
            "SIGTERM process termination is unsupported on this platform",
            Some(pid.to_string()),
            None,
        ))
    }
}

#[cfg(unix)]
fn process_is_running_after_sigterm(pid: u32) -> bool {
    // A directly owned child can remain a zombie after SIGTERM. Reap it before
    // falling back to PID liveness so bounded termination works on macOS too.
    let mut status = 0;
    let reaped = unsafe { libc::waitpid(pid as libc::pid_t, &mut status, libc::WNOHANG) };
    if reaped == pid as libc::pid_t {
        return false;
    }
    pid_is_running(pid)
}

/// Read Linux `/proc/<pid>/stat` field 22, the kernel tick at which this
/// process instance started. Pairing it with a PID rejects PID reuse.
pub fn linux_process_starttime_ticks(pid: u32) -> std::result::Result<Option<u64>, String> {
    #[cfg(target_os = "linux")]
    {
        let path = format!("/proc/{pid}/stat");
        let stat = match std::fs::read_to_string(&path) {
            Ok(stat) => stat,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(format!("read {path}: {error}")),
        };
        parse_linux_process_starttime_ticks(&stat)
            .map(Some)
            .ok_or_else(|| format!("parse {path} field 22"))
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Err("Linux /proc starttime identity is unsupported on this platform".to_string())
    }
}

#[cfg(target_os = "linux")]
fn parse_linux_process_starttime_ticks(stat: &str) -> Option<u64> {
    let after_command = stat.rsplit_once(") ")?.1;
    after_command.split_whitespace().nth(19)?.parse().ok()
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

/// Capture the process group created for an isolated child before exposing it
/// as running. Unix isolation requires the child to lead its own group.
pub fn isolated_process_group_id(pid: u32) -> std::result::Result<Option<u32>, String> {
    #[cfg(unix)]
    {
        if pid == 0 || pid > i32::MAX as u32 {
            return Err(format!("invalid isolated child PID {pid}"));
        }
        let pgid = unsafe { libc::getpgid(pid as libc::pid_t) };
        if pgid < 0 {
            return Err(format!(
                "read isolated child process group {pid}: {}",
                std::io::Error::last_os_error()
            ));
        }
        if pgid as u32 != pid {
            return Err(format!(
                "child {pid} is not the leader of its isolated process group ({pgid})"
            ));
        }
        Ok(Some(pgid as u32))
    }

    #[cfg(not(unix))]
    {
        let _ = pid;
        Ok(None)
    }
}

/// Return whether a recorded isolated process group still has a member. A live
/// group always blocks recovery because its numeric ID could have been reused.
pub fn isolated_process_group_is_running(pgid: u32) -> std::result::Result<bool, String> {
    if pgid == 0 || pgid > i32::MAX as u32 {
        return Err(format!("invalid isolated process group ID {pgid}"));
    }

    #[cfg(target_os = "linux")]
    if let Some(running) = linux_process_group_has_running_member(pgid as i32) {
        return Ok(running);
    }

    #[cfg(unix)]
    unsafe {
        return Ok(libc::kill(-(pgid as libc::pid_t), 0) == 0);
    }

    #[cfg(not(unix))]
    {
        let _ = pgid;
        Err("isolated process-group liveness is unsupported on this platform".to_string())
    }
}

/// Terminate the dedicated process group created for a managed child command.
/// Callers must only pass a PID they just spawned with process-group isolation.
pub fn terminate_isolated_process_group(owner_pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        if owner_pid == 0 || owner_pid > i32::MAX as u32 {
            return Err(Error::validation_invalid_argument(
                "pid",
                "isolated process-group leader PID is invalid",
                Some(owner_pid.to_string()),
                None,
            ));
        }
        unsafe {
            if libc::kill(-(owner_pid as libc::pid_t), libc::SIGTERM) != 0
                && std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
            {
                return Err(Error::internal_unexpected(format!(
                    "terminate isolated process group {owner_pid}: {}",
                    std::io::Error::last_os_error()
                )));
            }
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
        if process_group_is_running(owner_pid as i32) {
            unsafe {
                let _ = libc::kill(-(owner_pid as libc::pid_t), libc::SIGKILL);
            }
        }
        return Ok(());
    }

    #[cfg(not(unix))]
    {
        let _ = owner_pid;
        Err(Error::validation_invalid_argument(
            "pid",
            "isolated process-group termination is unsupported on this platform",
            None,
            None,
        ))
    }
}

/// Construct the Windows-native tree termination command without coupling
/// callers to a platform-specific shell or product workflow.
pub fn windows_taskkill_process_tree_step(pid: u32) -> ProcessStep {
    ProcessStep::new("taskkill").args(["/PID", &pid.to_string(), "/T", "/F"])
}

/// Best available process-tree termination for cleanup paths that cannot leave
/// an unrecorded child behind. Callers still reap and kill the root as a final
/// fallback when this returns an error.
pub fn terminate_process_tree_best_effort(pid: u32) -> Result<()> {
    #[cfg(unix)]
    {
        terminate_process_tree(pid).map(|_| ())
    }

    #[cfg(target_os = "windows")]
    {
        let step = windows_taskkill_process_tree_step(pid);
        let status = Command::new(&step.program)
            .args(&step.args)
            .status()
            .map_err(|error| {
                Error::internal_unexpected(format!(
                    "terminate process tree {pid} with taskkill: {error}"
                ))
            })?;
        if status.success() {
            Ok(())
        } else {
            Err(Error::internal_unexpected(format!(
                "terminate process tree {pid} with taskkill exited {status}"
            )))
        }
    }

    #[cfg(all(not(unix), not(target_os = "windows")))]
    {
        let _ = pid;
        Err(Error::internal_unexpected(
            "process-tree termination is unsupported on this platform; root-process fallback is required",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTreeTermination {
    pub owner_pid: u32,
    pub descendant_pids: Vec<u32>,
    pub signalled_pids: Vec<u32>,
    /// The strongest signal actually delivered to the tree. "SIGTERM" when a
    /// graceful terminate was sufficient, "SIGKILL" when one or more processes
    /// survived the grace period and had to be force-killed.
    pub signal: &'static str,
    /// Pids that were still alive after the SIGTERM grace window and therefore
    /// received an escalated SIGKILL.
    pub killed_pids: Vec<u32>,
    /// Pids that survived even the SIGKILL escalation (e.g. uninterruptible
    /// sleep, or owned by another user). Operators may need to act on these
    /// manually; the recovery commands cover them.
    pub surviving_pids: Vec<u32>,
    pub recovery_commands: Vec<String>,
}

/// How long to wait for a process tree to exit after SIGTERM before escalating
/// to SIGKILL. Kept short so `agent-task cancel` stays responsive while still
/// giving providers a chance to flush/cleanup on a graceful terminate.
#[cfg(unix)]
const SIGTERM_GRACE: std::time::Duration = std::time::Duration::from_millis(2000);
#[cfg(unix)]
const SIGTERM_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(100);
/// How long to wait after SIGKILL for the kernel to actually tear the targets
/// down before we declare them "surviving". `kill(2)` only queues the signal,
/// so a pid can still read as running for a brief moment after the call
/// returns; polling here keeps `surviving_pids` to genuinely unkillable
/// processes instead of ones merely mid-teardown.
#[cfg(unix)]
const SIGKILL_REAP_GRACE: std::time::Duration = std::time::Duration::from_millis(2000);

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

        // Phase 1: SIGTERM the whole tree, deepest descendants first so parents
        // do not respawn or reparent children mid-teardown.
        signal_pids(&targets, libc::SIGTERM)?;

        // Phase 2: wait out a short grace period, then SIGKILL any survivors so a
        // provider that ignores SIGTERM (or is wedged) cannot keep the run alive.
        let mut killed_pids = Vec::new();
        let survivors_after_term = wait_for_exit(&targets, SIGTERM_GRACE);
        if !survivors_after_term.is_empty() {
            signal_pids(&survivors_after_term, libc::SIGKILL)?;
            killed_pids = survivors_after_term;
        }

        // `kill(2)` only queues SIGKILL, so a just-killed pid can still read as
        // running for a moment after the call returns. Poll briefly for the
        // tree to actually exit before snapshotting survivors so we don't
        // misreport processes that are merely mid-teardown.
        let surviving_pids = wait_for_exit(&targets, SIGKILL_REAP_GRACE);

        let signal = if killed_pids.is_empty() {
            "SIGTERM"
        } else {
            "SIGKILL"
        };

        let mut recovery_commands = Vec::new();
        if !targets.is_empty() {
            recovery_commands.push(format!("kill -TERM {}", join_pids(&targets)));
        }
        if !surviving_pids.is_empty() {
            recovery_commands.push(format!("kill -KILL {}", join_pids(&surviving_pids)));
        }

        return Ok(ProcessTreeTermination {
            owner_pid,
            descendant_pids,
            signalled_pids: targets,
            signal,
            killed_pids,
            surviving_pids,
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

/// Build the recovery commands an operator should run to manually terminate a
/// provider process tree when Homeboy cannot signal it itself (e.g. the recorded
/// owner pid lives on a different host/runner, or this is a non-Unix host). This
/// never signals anything — it only renders deterministic, copy-pasteable
/// commands keyed on the recorded pid so the operator does not have to spelunk.
pub fn process_tree_recovery_commands(owner_pid: u32) -> Vec<String> {
    vec![
        format!(
            "ps -axo pid=,ppid=,command= | awk -v root={owner_pid} 'function walk(p){{print p; for(i in C[p]) walk(C[p][i])}} {{C[$2][length(C[$2])+1]=$1; CMD[$1]=$0}} END{{walk(root)}}'"
        ),
        format!("pkill -TERM -P {owner_pid}"),
        format!("kill -TERM {owner_pid}"),
        format!("kill -KILL {owner_pid}  # if it ignores SIGTERM"),
    ]
}

#[cfg(unix)]
fn signal_pids(pids: &[u32], signal: libc::c_int) -> Result<()> {
    // Deepest-first: `pids` is sorted ascending and descendants generally have
    // higher pids than their owner, so iterating in reverse approximates a
    // bottom-up teardown.
    for pid in pids.iter().rev() {
        unsafe {
            if libc::kill(*pid as libc::pid_t, signal) != 0 {
                let err = std::io::Error::last_os_error();
                if err.raw_os_error() != Some(libc::ESRCH) {
                    return Err(Error::internal_unexpected(format!(
                        "failed to signal pid {} with signal {}: {}",
                        pid, signal, err
                    )));
                }
            }
        }
    }
    Ok(())
}

/// Poll the given pids until they all exit or `grace` elapses. Returns the pids
/// still running when the window closes (the SIGKILL escalation set).
#[cfg(unix)]
fn wait_for_exit(pids: &[u32], grace: std::time::Duration) -> Vec<u32> {
    let deadline = std::time::Instant::now() + grace;
    loop {
        let survivors: Vec<u32> = pids
            .iter()
            .copied()
            .filter(|pid| pid_is_running(*pid))
            .collect();
        if survivors.is_empty() || std::time::Instant::now() >= deadline {
            return survivors;
        }
        std::thread::sleep(SIGTERM_POLL_INTERVAL);
    }
}

#[cfg(unix)]
fn join_pids(pids: &[u32]) -> String {
    pids.iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(" ")
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

    #[test]
    fn force_stop_environment_ownership_requires_an_exact_assignment() {
        let environment = b"HOME=/tmp\0HOMEBOY_DAEMON_STARTUP_TOKEN=lease-token\0";

        assert!(environment_contains_assignment(
            environment,
            b"HOMEBOY_DAEMON_STARTUP_TOKEN=lease-token"
        ));
        assert!(!environment_contains_assignment(
            environment,
            b"HOMEBOY_DAEMON_STARTUP_TOKEN=lease"
        ));
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

    #[test]
    fn process_tree_recovery_commands_reference_recorded_pid() {
        let commands = process_tree_recovery_commands(4242);
        assert!(!commands.is_empty());
        assert!(commands.iter().any(|cmd| cmd.contains("4242")));
        assert!(commands.iter().any(|cmd| cmd.contains("kill -KILL 4242")));
    }

    #[test]
    fn windows_taskkill_process_tree_step_targets_the_root_and_tree() {
        assert_eq!(
            windows_taskkill_process_tree_step(4242),
            ProcessStep::new("taskkill").args(["/PID", "4242", "/T", "/F"])
        );
    }

    /// Reap a test-owned child as soon as it exits, off-thread, so the kernel
    /// clears the zombie promptly while `terminate_process_tree` is still
    /// polling. The test process is the direct parent of these children, so it
    /// alone is responsible for reaping them. On platforms without a /proc-based
    /// zombie check (e.g. macOS), `pid_is_running` answers `kill(pid, 0) == 0`,
    /// which stays `true` for an un-reaped zombie — so a child reaped only after
    /// the assertions would be misreported as a survivor. Reaping concurrently
    /// keeps the test honest about whether SIGTERM/SIGKILL actually took.
    fn reap_in_background(mut child: std::process::Child) -> std::thread::JoinHandle<()> {
        std::thread::spawn(move || {
            let _ = child.wait();
        })
    }

    #[test]
    fn terminate_process_tree_escalates_to_sigkill_on_sigterm_resistant_child() {
        // A child that ignores SIGTERM forces the SIGKILL escalation path.
        let child = Command::new("sh")
            .args(["-c", "trap '' TERM; sleep 30"])
            .spawn()
            .expect("spawn sigterm-resistant child");
        let pid = child.id();
        // Reap concurrently so the post-SIGKILL zombie is cleared during the
        // reap grace window instead of lingering as a false survivor.
        let reaper = reap_in_background(child);

        let termination = terminate_process_tree(pid).expect("terminate sigterm-resistant tree");

        // It survived SIGTERM and had to be SIGKILL'd.
        assert_eq!(termination.signal, "SIGKILL");
        assert!(termination.killed_pids.contains(&pid));
        assert!(termination.surviving_pids.is_empty());
        assert!(termination
            .recovery_commands
            .iter()
            .any(|cmd| cmd.contains(&pid.to_string())));

        let _ = reaper.join();
        assert!(!pid_is_running(pid));
    }

    #[test]
    fn terminate_process_tree_uses_sigterm_for_cooperative_child() {
        // A plain sleep exits on SIGTERM, so no escalation is needed.
        let child = Command::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn cooperative child");
        let pid = child.id();
        // Reap concurrently so the SIGTERM-exited zombie is cleared during the
        // grace window and is not mistaken for a process that ignored SIGTERM.
        let reaper = reap_in_background(child);

        let termination = terminate_process_tree(pid).expect("terminate cooperative tree");

        assert_eq!(termination.signal, "SIGTERM");
        assert!(termination.killed_pids.is_empty());
        assert!(termination.surviving_pids.is_empty());

        let _ = reaper.join();
        assert!(!pid_is_running(pid));
    }
}
