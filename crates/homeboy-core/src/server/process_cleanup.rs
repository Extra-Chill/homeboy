use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

#[cfg(unix)]
static ACTIVE_CLEANUP_SIGNAL: AtomicI32 = AtomicI32::new(0);

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

pub(crate) struct ProcessCleanupReport {
    pub(crate) incomplete: Option<String>,
    pub(crate) warning: Option<String>,
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

    pub(crate) fn cleanup(mut self) -> Option<ProcessCleanupReport> {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            let detail = cleanup_process_group(
                pgid,
                #[cfg(target_os = "linux")]
                self.containment.as_ref(),
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
            );
        }
    }
}

#[cfg(unix)]
fn install_process_cleanup_signal_handlers() {
    CLEANUP_SIGNALS_INSTALLED.call_once(|| unsafe {
        libc::signal(
            libc::SIGINT,
            cleanup_signal_handler as *const () as libc::sighandler_t,
        );
        libc::signal(
            libc::SIGTERM,
            cleanup_signal_handler as *const () as libc::sighandler_t,
        );
    });
}

#[cfg(unix)]
extern "C" fn cleanup_signal_handler(signal: libc::c_int) {
    ACTIVE_CLEANUP_SIGNAL.store(signal, Ordering::SeqCst);
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
) -> Option<ProcessCleanupReport> {
    #[cfg(target_os = "linux")]
    if let Some(containment) = containment {
        match containment.cleanup_with_grace(Duration::from_millis(200), false) {
            Ok(cleanup) if cleanup.complete => {
                return cleanup.warning.map(|warning| ProcessCleanupReport {
                    incomplete: None,
                    warning: Some(warning),
                });
            }
            Ok(cleanup) => {
                return cleanup.detail.map(|incomplete| ProcessCleanupReport {
                    incomplete: Some(incomplete),
                    warning: cleanup.warning,
                });
            }
            Err(error) => {
                cleanup_process_group_fallback(pgid);
                return Some(ProcessCleanupReport {
                    incomplete: Some(format!("process containment cleanup failed: {error}")),
                    warning: None,
                });
            }
        }
    }

    cleanup_process_group_fallback(pgid);
    None
}

#[cfg(unix)]
fn cleanup_process_group_fallback(pgid: libc::pid_t) {
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(200));
    if crate::process::process_group_is_running(pgid) {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
    // Reuse the engine's bounded group reaper so orphaned descendants are
    // adopted and reaped rather than left as zombies under a non-reaping PID 1.
    let _ = crate::engine::command::terminate_remaining_process_group(pgid as u32);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interrupted_command_output_records_signal() {
        let stderr = stderr_with_interruption("runner output".to_string(), Some(15));

        assert_eq!(interrupted_exit_code(Some(15), 0), 143);
        assert!(stderr.contains("runner output"));
        assert!(stderr.contains("Homeboy interrupted by signal 15"));
        assert!(stderr.contains("terminated child process group"));
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
