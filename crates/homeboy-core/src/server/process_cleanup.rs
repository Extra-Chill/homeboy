use std::process::Command;
use std::sync::atomic::{AtomicI32, AtomicUsize, Ordering};
use std::time::Duration;

/// How long a terminated process group gets to exit on SIGTERM before it is
/// sent SIGKILL.
#[cfg(unix)]
const PROCESS_GROUP_CLEANUP_GRACE: Duration = Duration::from_millis(200);

#[cfg(unix)]
static ACTIVE_CLEANUP_SIGNAL: AtomicI32 = AtomicI32::new(0);

/// Every SIGINT/SIGTERM the cleanup handler has observed. A child wait
/// consumes [`ACTIVE_CLEANUP_SIGNAL`] (and a new child resets it), so a wait
/// that is not inside a child wait, such as a foreground client polling a
/// queued Lab job, never saw the request and kept waiting (#18042). This count
/// is never consumed: [`crate::foreground_wait`] compares it with the count at
/// scope entry.
static TERMINATION_SIGNALS: AtomicUsize = AtomicUsize::new(0);
static LAST_TERMINATION_SIGNAL: AtomicI32 = AtomicI32::new(0);

#[cfg(unix)]
static CLEANUP_SIGNALS_INSTALLED: std::sync::Once = std::sync::Once::new();

#[cfg(unix)]
pub(crate) fn configure_process_group_cleanup(cmd: &mut Command) {
    install_process_cleanup_signal_handlers();
    use std::os::unix::process::CommandExt;
    unsafe {
        cmd.pre_exec(|| {
            if libc::setpgid(0, 0) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
}

#[cfg(target_os = "linux")]
pub(crate) fn prepare_process_scope_cleanup(
    cmd: &mut Command,
) -> crate::error::Result<crate::process::ProcessContainment> {
    install_process_cleanup_signal_handlers();
    crate::process::ProcessContainment::prepare(cmd)
}

#[cfg(not(unix))]
pub(crate) fn configure_process_group_cleanup(_cmd: &mut Command) {}

pub(crate) struct ProcessGroupCleanupGuard {
    #[cfg(unix)]
    pgid: Option<libc::pid_t>,
    #[cfg(target_os = "linux")]
    containment: Option<crate::process::ProcessContainment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ProcessCleanupReport {
    pub(crate) incomplete: String,
}

impl ProcessGroupCleanupGuard {
    pub fn new(root_pid: u32) -> Self {
        #[cfg(unix)]
        {
            let pgid = Some(root_pid as libc::pid_t);
            ACTIVE_CLEANUP_SIGNAL.store(0, Ordering::SeqCst);
            Self {
                pgid,
                #[cfg(target_os = "linux")]
                containment: None,
            }
        }

        #[cfg(not(unix))]
        {
            let _ = root_pid;
            Self {}
        }
    }

    #[cfg(target_os = "linux")]
    pub(crate) fn with_containment(
        root_pid: u32,
        containment: crate::process::ProcessContainment,
    ) -> Self {
        ACTIVE_CLEANUP_SIGNAL.store(0, Ordering::SeqCst);
        Self {
            pgid: Some(root_pid as libc::pid_t),
            containment: Some(containment),
        }
    }

    pub(crate) fn cleanup(
        mut self,
        context: crate::process::ProcessContainmentCleanupContext,
    ) -> Option<ProcessCleanupReport> {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let detail = cleanup_process_group(
                pgid,
                #[cfg(target_os = "linux")]
                self.containment.as_ref(),
                context,
            );
            self.pgid = None;
            return detail;
        }

        None
    }

    #[cfg(unix)]
    pub(crate) fn pgid(&self) -> Option<i32> {
        self.pgid
    }

    #[cfg(not(unix))]
    pub(crate) fn pgid(&self) -> Option<i32> {
        None
    }
}

impl Drop for ProcessGroupCleanupGuard {
    fn drop(&mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid.take() {
            cleanup_process_group(
                pgid,
                #[cfg(target_os = "linux")]
                self.containment.as_ref(),
                crate::process::ProcessContainmentCleanupContext::LeaderMayBeRunning,
            );
        }
    }
}

#[cfg(unix)]
static PREVIOUS_SIGINT: crate::signal_chain::ChainedSignal =
    crate::signal_chain::ChainedSignal::new();

#[cfg(unix)]
static PREVIOUS_SIGTERM: crate::signal_chain::ChainedSignal =
    crate::signal_chain::ChainedSignal::new();

/// Record SIGINT/SIGTERM for child cleanup while chaining to any handler that
/// was already installed (the daemon's drain handlers, #18468), so spawning a
/// local child never steals the process's own shutdown signals.
#[cfg(unix)]
fn install_process_cleanup_signal_handlers() {
    CLEANUP_SIGNALS_INSTALLED.call_once(|| {
        // Best effort, as with the previous signal(2) install: failure only
        // loses interruption evidence, never correctness of the child run.
        let _ = PREVIOUS_SIGINT.install(libc::SIGINT, cleanup_signal_handler);
        let _ = PREVIOUS_SIGTERM.install(libc::SIGTERM, cleanup_signal_handler);
    });
}

#[cfg(unix)]
extern "C" fn cleanup_signal_handler(
    signal: libc::c_int,
    info: *mut libc::siginfo_t,
    context: *mut libc::c_void,
) {
    ACTIVE_CLEANUP_SIGNAL.store(signal, Ordering::SeqCst);
    LAST_TERMINATION_SIGNAL.store(signal, Ordering::SeqCst);
    TERMINATION_SIGNALS.fetch_add(1, Ordering::SeqCst);
    match signal {
        libc::SIGINT => PREVIOUS_SIGINT.forward(signal, info, context),
        libc::SIGTERM => PREVIOUS_SIGTERM.forward(signal, info, context),
        _ => {}
    }
}

/// Route SIGINT/SIGTERM through the chained cleanup handler so they are
/// counted in [`termination_signals`]. Installing it replaces the default
/// terminate disposition, so only callers that poll the count may install it.
pub(crate) fn install_termination_observer() {
    #[cfg(unix)]
    install_process_cleanup_signal_handlers();
}

/// `(count, last signal)` of every SIGINT/SIGTERM observed so far.
pub(crate) fn termination_signals() -> (usize, i32) {
    (
        TERMINATION_SIGNALS.load(Ordering::SeqCst),
        LAST_TERMINATION_SIGNAL.load(Ordering::SeqCst),
    )
}

#[cfg(unix)]
pub(crate) fn active_cleanup_signal() -> Option<i32> {
    let signal = ACTIVE_CLEANUP_SIGNAL.swap(0, Ordering::SeqCst);
    (signal > 0).then_some(signal)
}

#[cfg(not(unix))]
pub(crate) fn active_cleanup_signal() -> Option<i32> {
    None
}

pub(crate) fn interrupted_exit_code(signal: Option<i32>, fallback: i32) -> i32 {
    signal.map(|value| 128 + value).unwrap_or(fallback)
}

pub(crate) fn stderr_with_interruption(mut stderr: String, signal: Option<i32>) -> String {
    if let Some(signal) = signal {
        if !stderr.is_empty() && !stderr.ends_with('\n') {
            stderr.push('\n');
        }
        stderr.push_str(&format!(
            "Homeboy interrupted by signal {signal}; terminated child process group before returning failure evidence."
        ));
    }
    stderr
}

#[cfg(unix)]
fn cleanup_process_group(
    pgid: libc::pid_t,
    #[cfg(target_os = "linux")] containment: Option<&crate::process::ProcessContainment>,
    context: crate::process::ProcessContainmentCleanupContext,
) -> Option<ProcessCleanupReport> {
    #[cfg(not(target_os = "linux"))]
    let _ = context;
    #[cfg(target_os = "linux")]
    if let Some(containment) = containment {
        match containment.cleanup_with_grace(PROCESS_GROUP_CLEANUP_GRACE, context) {
            Ok(cleanup) if cleanup.complete => return None,
            Ok(cleanup) => {
                return Some(ProcessCleanupReport {
                    incomplete: cleanup.detail.unwrap_or_else(|| {
                        "process containment cleanup could not prove the owned scope empty"
                            .to_string()
                    }),
                });
            }
            Err(error) => {
                cleanup_process_group_fallback(pgid);
                return Some(ProcessCleanupReport {
                    incomplete: format!("process containment cleanup failed: {error}"),
                });
            }
        }
    }

    cleanup_process_group_fallback(pgid);
    None
}

#[cfg(unix)]
fn cleanup_process_group_fallback(pgid: libc::pid_t) {
    cleanup_process_group_fallback_within(pgid, PROCESS_GROUP_CLEANUP_GRACE);
}

#[cfg(unix)]
fn cleanup_process_group_fallback_within(pgid: libc::pid_t, grace: Duration) {
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    // Give the group up to the grace period to exit, but return as soon as it
    // has: a command that already finished must not pay the whole grace.
    let deadline = std::time::Instant::now() + grace;
    while homeboy_engine_primitives::command::process_group_is_running(pgid as u32).unwrap_or(false)
        && std::time::Instant::now() < deadline
    {
        std::thread::sleep(Duration::from_millis(5));
    }
    if homeboy_engine_primitives::command::process_group_is_running(pgid as u32).unwrap_or(false) {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    // Reuse the engine's bounded group reaper so orphaned descendants are
    // adopted and reaped rather than left as zombies under a non-reaping PID 1.
    let _ = crate::engine::command::terminate_remaining_process_group(pgid as u32);
}

/// Serializes tests that raise SIGINT/SIGTERM in the test process with tests
/// that read [`termination_signals`], so a raised signal cannot stop another
/// test's foreground wait scope.
#[cfg(test)]
pub(crate) static SIGNAL_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn cleanup_of_an_already_exited_group_does_not_wait_out_the_grace() {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("sh")
            .args(["-c", "true"])
            .process_group(0)
            .spawn()
            .expect("spawn group leader");
        let pgid = child.id() as libc::pid_t;
        child.wait().expect("leader exits");

        // The grace is deliberately huge. Cleanup of an exited group still
        // scans /proc and signals the group, and a starved CI runner can
        // stretch that past any bound a few times smaller than the real 200ms
        // grace (#15756). Waiting out this grace would take a full minute, so
        // returning in under half of it proves the grace was skipped, with a
        // margin no scheduling delay can cross.
        let grace = Duration::from_secs(60);
        let started = std::time::Instant::now();
        cleanup_process_group_fallback_within(pgid, grace);
        let elapsed = started.elapsed();
        assert!(
            elapsed < grace / 2,
            "a finished command paid {elapsed:?} of a {grace:?} cleanup grace"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_still_terminates_a_running_group() {
        use std::os::unix::process::CommandExt;
        let mut child = Command::new("sh")
            .args(["-c", "sleep 30"])
            .process_group(0)
            .spawn()
            .expect("spawn group leader");
        let pgid = child.id() as libc::pid_t;

        cleanup_process_group_fallback(pgid);
        let status = child.wait().expect("leader is reaped");
        assert!(!status.success(), "the running group was terminated");
    }

    #[test]
    fn interrupted_command_output_records_signal() {
        let stderr = stderr_with_interruption("runner output".to_string(), Some(15));

        assert_eq!(interrupted_exit_code(Some(15), 0), 143);
        assert!(stderr.contains("runner output"));
        assert!(stderr.contains("Homeboy interrupted by signal 15"));
        assert!(stderr.contains("terminated child process group"));
    }

    /// Raise SIGTERM (only once handlers are installed, so it cannot kill
    /// the test process) and wait for both observers to see it.
    #[cfg(unix)]
    fn raise_sigterm_and_assert_both_observers(stop: &std::sync::atomic::AtomicBool) {
        let _signals = super::SIGNAL_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        assert_eq!(unsafe { libc::raise(libc::SIGTERM) }, 0);
        let deadline = std::time::Instant::now() + Duration::from_secs(10);
        while !stop.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(
            stop.load(Ordering::SeqCst),
            "the daemon's terminate request was swallowed by the cleanup handler"
        );
        assert_eq!(active_cleanup_signal(), Some(libc::SIGTERM));
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_handler_installed_after_terminate_handler_chains_sigterm() {
        crate::process::reset_terminate_request_for_test();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::process::install_terminate_handler(stop.clone()).expect("terminate handler");
        // The daemon order: drain handlers first, then a local child spawn.
        let mut command = Command::new("true");
        configure_process_group_cleanup(&mut command);

        raise_sigterm_and_assert_both_observers(&stop);
    }

    #[cfg(unix)]
    #[test]
    fn terminate_handler_installed_after_cleanup_handler_chains_sigterm() {
        crate::process::reset_terminate_request_for_test();
        install_process_cleanup_signal_handlers();
        let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::process::install_terminate_handler(stop.clone()).expect("terminate handler");

        raise_sigterm_and_assert_both_observers(&stop);
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn containment_guard_clears_a_stale_cleanup_signal() {
        ACTIVE_CLEANUP_SIGNAL.store(libc::SIGTERM, Ordering::SeqCst);
        let mut command = Command::new("sh");
        let containment =
            crate::process::ProcessContainment::prepare(&mut command).expect("prepare containment");
        let mut guard = ProcessGroupCleanupGuard::with_containment(0, containment);

        assert_eq!(active_cleanup_signal(), None);

        // This is a construction-only test; avoid signaling a process group on drop.
        guard.pgid = None;
    }
}
