use std::process::Command;
use std::sync::atomic::{AtomicI32, Ordering};
use std::time::Duration;

#[cfg(unix)]
static ACTIVE_CLEANUP_PGID: AtomicI32 = AtomicI32::new(0);

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

#[cfg(not(unix))]
pub(crate) fn configure_process_group_cleanup(_cmd: &mut Command) {}

pub(crate) struct ProcessGroupCleanupGuard {
    #[cfg(unix)]
    pgid: Option<libc::pid_t>,
}

impl ProcessGroupCleanupGuard {
    pub(crate) fn new(root_pid: u32) -> Self {
        #[cfg(unix)]
        {
            let pgid = Some(root_pid as libc::pid_t);
            if let Some(pgid) = pgid {
                ACTIVE_CLEANUP_PGID.store(pgid, Ordering::SeqCst);
            }
            Self { pgid }
        }

        #[cfg(not(unix))]
        {
            let _ = root_pid;
            Self {}
        }
    }

    pub(crate) fn cleanup(mut self) {
        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            cleanup_process_group(pgid);
            ACTIVE_CLEANUP_PGID
                .compare_exchange(pgid, 0, Ordering::SeqCst, Ordering::SeqCst)
                .ok();
            self.pgid = None;
        }
    }

    #[cfg(unix)]
    pub(crate) fn pgid(&self) -> Option<i32> {
        self.pgid.map(|pgid| pgid)
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
            cleanup_process_group(pgid);
            ACTIVE_CLEANUP_PGID
                .compare_exchange(pgid, 0, Ordering::SeqCst, Ordering::SeqCst)
                .ok();
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
    let pgid = ACTIVE_CLEANUP_PGID.load(Ordering::SeqCst);
    ACTIVE_CLEANUP_SIGNAL.store(signal, Ordering::SeqCst);
    if pgid > 0 {
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
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
fn cleanup_process_group(pgid: libc::pid_t) {
    unsafe {
        libc::kill(-pgid, libc::SIGTERM);
    }
    std::thread::sleep(Duration::from_millis(200));
    if crate::core::process::process_group_is_running(pgid) {
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
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
}
