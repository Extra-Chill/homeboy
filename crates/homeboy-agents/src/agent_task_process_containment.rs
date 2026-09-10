//! Process-tree containment for agent-task provider and gate command spawns.
//!
//! A provider agent is not a leaf process. It spawns its own tooling — build
//! systems, test runners, linkers — and those grandchildren are what actually
//! consume the host. Spawning a provider as a plain [`Command`] child leaves
//! every one of those descendants unowned: `Child::kill` signals the direct
//! child only, and a controller that dies (cancel, operator kill, OOM) reaps
//! nothing at all. Extra-Chill/homeboy#11477 observed the full chain surviving
//! a run Homeboy had already marked terminal: cook → provider → build driver →
//! three compiler processes at 90-95% CPU, and later an orphaned linked test
//! binary four minutes past termination. Every level had to be killed by hand,
//! by pid.
//!
//! This module is the single place the agent-task execution paths obtain
//! containment, so provider execution, provider readiness probes, and
//! repo-local gate execution all terminalize the same way:
//!
//! * the child leads its own process group, so termination signals the whole
//!   group instead of one pid;
//! * a controller death guard (a `fork()`ed watcher holding the read end of a
//!   pipe this process owns) SIGKILLs that group if this process exits for any
//!   reason, including an untrappable SIGKILL;
//! * [`AgentTaskProcessSupervisor`] owns the direct child, so every exit path —
//!   timeout, liveness kill, clean exit, early `return`, and unwind — terminates
//!   the group and waits the leader.
//!
//! Reaping is deliberately unconditional rather than "only on failure". The
//! orphaned test binary in #11477 outlived a provider that had already exited,
//! so a clean provider exit is exactly the case that leaked.
//!
//! The forked controller-death guard closes every inherited descriptor except
//! its own liveness pipe, so overlapping containments do not keep each other's
//! guards alive.

use std::ops::{Deref, DerefMut};
use std::process::{Child, Command};

use homeboy_core::engine::command::{
    terminate_process_tree_and_reap, terminate_remaining_process_group, ControllerChildGuard,
};

/// Owns the process group of one spawned agent-task child.
///
/// Construct with [`AgentTaskProcessContainment::prepare`] *before* spawning
/// (the process-group isolation is installed on the [`Command`]) and call
/// [`AgentTaskProcessContainment::attach`] immediately after spawning.
pub(crate) struct AgentTaskProcessContainment {
    /// Also held for its `Drop`, which closes the liveness pipe the forked
    /// death guard watches — that closure is what makes controller death kill
    /// the tree.
    guard: ControllerChildGuard,
    leader_pid: Option<u32>,
    reaped: bool,
}

impl AgentTaskProcessContainment {
    /// Install process-group isolation and the controller death guard on
    /// `command`. Must be called before `command.spawn()`.
    pub(crate) fn prepare(command: &mut Command) -> std::io::Result<Self> {
        Ok(Self {
            guard: ControllerChildGuard::prepare(command)?,
            leader_pid: None,
            reaped: false,
        })
    }

    /// Start the death guard for `child`. Called after spawn so the guard
    /// cannot inherit the standard library's private spawn error pipe.
    pub(crate) fn attach(&mut self, child: &Child) -> std::io::Result<()> {
        self.leader_pid = Some(child.id());
        self.guard.attach(child)
    }

    /// Transfer the spawned child into an unwind-safe supervisor and attach the
    /// controller death guard. An attach failure still drops the supervisor,
    /// which terminates the group and waits the child before returning.
    pub(crate) fn supervise(self, child: Child) -> std::io::Result<AgentTaskProcessSupervisor> {
        let mut supervisor = AgentTaskProcessSupervisor {
            containment: self,
            child,
            cleanup_complete: false,
        };
        supervisor.containment.attach(&supervisor.child)?;
        Ok(supervisor)
    }

    /// The pid of the contained group leader, once attached.
    pub(crate) fn leader_pid(&self) -> Option<u32> {
        self.leader_pid
    }

    /// Terminate the contained tree while its leader is still running, then
    /// reap the leader. Used by Homeboy-initiated kills (per-attempt timeout,
    /// liveness watchdog, execution deadline).
    ///
    /// This replaces `Child::kill`, which signals only the direct child and
    /// leaves its build/test descendants running.
    pub(crate) fn terminate_live(&mut self, child: &mut Child) -> Result<(), String> {
        match terminate_process_tree_and_reap(child) {
            Ok(_) => {
                self.reaped = true;
                Ok(())
            }
            Err(primary_error) => {
                // A supervision error must not become an early return that
                // leaves the direct child unreaped. Retry group cleanup, then
                // independently kill and wait for the child as a final fallback.
                let group_error = self
                    .leader_pid
                    .and_then(|pid| terminate_remaining_process_group(pid).err());
                let kill_error = child
                    .kill()
                    .err()
                    .filter(|error| error.kind() != std::io::ErrorKind::InvalidInput);
                let wait_error = child.wait().err();
                // Reaping the direct child does not prove descendants in its
                // process group are gone. Leave the containment live when
                // group cleanup failed so Drop retries the group.
                self.reaped = group_error.is_none() && wait_error.is_none();

                let mut details = vec![primary_error.to_string()];
                if let Some(error) = group_error {
                    details.push(format!("fallback group cleanup failed: {error}"));
                }
                if let Some(error) = kill_error {
                    details.push(format!("fallback child kill failed: {error}"));
                }
                if let Some(error) = wait_error {
                    details.push(format!("fallback child reap failed: {error}"));
                }
                Err(format!(
                    "could not terminate the contained provider process group{}: {}",
                    self.leader_suffix(),
                    details.join("; ")
                ))
            }
        }
    }

    /// Reap whatever is still alive in the group after its leader exited on its
    /// own. Idempotent, and a no-op once the tree has already been terminated.
    ///
    /// Call this before joining output-capture readers: a background descendant
    /// still holding an inherited stdout/stderr pipe keeps those readers from
    /// ever seeing EOF.
    pub(crate) fn reap_after_exit(&mut self) -> Result<(), String> {
        if self.reaped {
            return Ok(());
        }
        let Some(leader_pid) = self.leader_pid else {
            self.reaped = true;
            return Ok(());
        };
        match terminate_remaining_process_group(leader_pid) {
            Ok(()) => {
                self.reaped = true;
                Ok(())
            }
            Err(primary_error) => {
                let retry_error = terminate_remaining_process_group(leader_pid).err();
                self.reaped = retry_error.is_none();
                Err(format!(
                    "contained provider process group{} outlived its leader and did not exit: {primary_error}{}",
                    self.leader_suffix(),
                    retry_error
                        .map(|error| format!("; cleanup retry failed: {error}"))
                        .unwrap_or_default()
                ))
            }
        }
    }

    fn leader_suffix(&self) -> String {
        self.leader_pid
            .map(|pid| format!(" (leader pid {pid})"))
            .unwrap_or_default()
    }
}

impl Drop for AgentTaskProcessContainment {
    fn drop(&mut self) {
        // Keep raw containment users from leaking descendants. Only the
        // child-owning supervisor can additionally guarantee waiting the
        // direct child on an early return or unwind.
        let _ = self.reap_after_exit();
    }
}

/// Owns a contained direct child for the full post-spawn lifetime.
///
/// Normal paths explicitly clean up before joining pipe workers. If control
/// instead returns early or unwinds, `Drop` terminates the process group and
/// waits the direct child, without attempting to drain descendant-held pipes.
pub(crate) struct AgentTaskProcessSupervisor {
    containment: AgentTaskProcessContainment,
    child: Child,
    cleanup_complete: bool,
}

impl AgentTaskProcessSupervisor {
    pub(crate) fn leader_pid(&self) -> Option<u32> {
        self.containment.leader_pid()
    }

    pub(crate) fn terminate_live(&mut self) -> Result<(), String> {
        let result = self.containment.terminate_live(&mut self.child);
        if result.is_ok() {
            self.cleanup_complete = true;
        }
        result
    }

    pub(crate) fn reap_after_exit(&mut self) -> Result<(), String> {
        let group_result = self.containment.reap_after_exit();
        let wait_result = self.child.wait().map(|_| ()).map_err(|error| {
            format!(
                "could not reap contained provider child{}: {error}",
                self.containment.leader_suffix()
            )
        });
        self.cleanup_complete = group_result.is_ok() && wait_result.is_ok();
        group_result.and(wait_result)
    }
}

impl Deref for AgentTaskProcessSupervisor {
    type Target = Child;

    fn deref(&self) -> &Self::Target {
        &self.child
    }
}

impl DerefMut for AgentTaskProcessSupervisor {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.child
    }
}

impl Drop for AgentTaskProcessSupervisor {
    fn drop(&mut self) {
        if !self.cleanup_complete {
            let _ = self.containment.terminate_live(&mut self.child);
        }
    }
}

/// Render an operator-facing recovery hint for a process group Homeboy could
/// not confirm dead. Keyed on the recorded leader pid so nobody has to go
/// hunting through `ps` output for orphaned build processes.
pub(crate) fn contained_group_recovery_commands(leader_pid: Option<u32>) -> Vec<String> {
    let Some(leader_pid) = leader_pid else {
        return Vec::new();
    };
    vec![
        format!("ps -eo pid=,pgid=,etime=,command= | awk '$2 == {leader_pid}'"),
        format!("kill -TERM -{leader_pid}"),
        format!("kill -KILL -{leader_pid}  # if it ignores SIGTERM"),
    ]
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;
    use std::time::{Duration, Instant};

    /// Poll rather than sleep a fixed interval: `kill(2)` only queues the
    /// signal, so a just-signalled pid can still read as running.
    fn wait_until_gone(pid: u32, timeout: Duration) -> bool {
        let deadline = Instant::now() + timeout;
        loop {
            if !homeboy_core::process::pid_is_running(pid) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    /// Spawn a contained shell that leaves one background descendant behind and
    /// exits immediately, returning `(child, descendant_pid)`.
    fn spawn_leaking_child(script: &str) -> (AgentTaskProcessContainment, Child, u32) {
        let mut command = Command::new("sh");
        command
            .args(["-c", script])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        let mut containment =
            AgentTaskProcessContainment::prepare(&mut command).expect("prepare containment");
        let mut child = command.spawn().expect("spawn contained child");
        containment.attach(&child).expect("attach death guard");

        // Read one line rather than to EOF: the background descendant holds the
        // inherited stdout pipe open, which is precisely the hang this
        // containment exists to prevent.
        let stdout = child.stdout.take().expect("piped stdout");
        let mut reader = BufReader::new(stdout);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read descendant pid");
        let descendant_pid: u32 = line.trim().parse().expect("descendant pid");

        (containment, child, descendant_pid)
    }

    /// The #11477 regression: the provider exits cleanly and its build/test
    /// descendant keeps running. A clean leader exit must still reap the group.
    #[test]
    fn reaping_after_leader_exit_kills_a_surviving_descendant() {
        let (mut containment, mut child, descendant_pid) =
            spawn_leaking_child("sleep 30 & echo $!; exit 0");

        child.wait().expect("leader exits on its own");
        assert!(
            homeboy_core::process::pid_is_running(descendant_pid),
            "descendant must outlive its leader for this to test anything"
        );

        containment
            .reap_after_exit()
            .expect("surviving group members are reaped");

        assert!(
            wait_until_gone(descendant_pid, Duration::from_secs(5)),
            "descendant {descendant_pid} survived cleanup after its leader exited"
        );
    }

    /// A Homeboy-initiated kill (timeout / liveness watchdog) must take the
    /// whole tree, not just the direct child.
    #[test]
    fn terminating_a_live_leader_kills_its_descendants() {
        let (mut containment, mut child, descendant_pid) =
            spawn_leaking_child("sleep 30 & echo $!; sleep 30");
        let leader_pid = child.id();

        containment
            .terminate_live(&mut child)
            .expect("contained tree terminates");

        assert!(
            wait_until_gone(descendant_pid, Duration::from_secs(5)),
            "descendant {descendant_pid} survived termination of leader {leader_pid}"
        );
        assert!(
            wait_until_gone(leader_pid, Duration::from_secs(5)),
            "leader {leader_pid} survived termination"
        );
    }

    /// Forced unwind after spawn must terminate descendants and reap the direct
    /// child, even though no explicit cleanup path runs.
    #[test]
    fn forced_unwind_terminates_group_and_reaps_direct_child() {
        let mut leader_pid = 0;
        let mut descendant_pid = 0;
        let unwind = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut command = Command::new("sh");
            command
                .args(["-c", "sleep 30 & echo $!; sleep 30"])
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::null());
            let containment =
                AgentTaskProcessContainment::prepare(&mut command).expect("prepare containment");
            let mut child = command.spawn().expect("spawn contained child");
            let stdout = child.stdout.take().expect("piped stdout");
            let mut reader = BufReader::new(stdout);
            let mut line = String::new();
            reader.read_line(&mut line).expect("read descendant pid");
            leader_pid = child.id();
            descendant_pid = line.trim().parse().expect("descendant pid");
            let _supervisor = containment.supervise(child).expect("supervise child");
            panic!("force post-spawn unwind");
        }));

        assert!(unwind.is_err());

        assert!(
            wait_until_gone(descendant_pid, Duration::from_secs(5)),
            "descendant {descendant_pid} survived unwind cleanup"
        );
        assert!(
            wait_until_gone(leader_pid, Duration::from_secs(5)),
            "leader {leader_pid} survived unwind cleanup"
        );
        let wait_result = unsafe {
            libc::waitpid(
                leader_pid as libc::pid_t,
                std::ptr::null_mut(),
                libc::WNOHANG,
            )
        };
        assert_eq!(wait_result, -1, "leader {leader_pid} remained waitable");
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD),
            "leader {leader_pid} was not reaped by the supervisor"
        );
    }

    #[test]
    fn recovery_commands_are_keyed_on_the_recorded_leader_pid() {
        let commands = contained_group_recovery_commands(Some(4242));

        assert!(commands
            .iter()
            .any(|command| command.contains("$2 == 4242")));
        assert!(commands
            .iter()
            .any(|command| command.contains("-TERM -4242")));
        assert!(contained_group_recovery_commands(None).is_empty());
    }
}
