//! On-demand daemons own durable work, not an indefinite detached process.

use std::time::{Duration, Instant};

use super::generation_store;
use crate::error::Result;

pub const IDLE_TIMEOUT_ENV: &str = "HOMEBOY_DAEMON_IDLE_TIMEOUT_SECS";
pub(super) const DEFAULT_IDLE_TIMEOUT_SECS: u64 = 300;
/// Idle time before a daemon whose binary was replaced stops itself.
const REPLACED_BINARY_STOP_AFTER: Duration = Duration::from_secs(30);
const LAUNCHER_ENV: &str = "HOMEBOY_DAEMON_LAUNCHER_IDENTITY";

#[derive(serde::Serialize, serde::Deserialize)]
pub(super) struct LauncherIdentity {
    pid: u32,
    start: crate::process::ProcessStartIdentity,
}

impl LauncherIdentity {
    pub(super) fn configure(command: &mut std::process::Command) {
        // A very short configured idle window must not expire while its
        // launcher is still validating and returning the startup handoff.
        command.env_remove(LAUNCHER_ENV);
        let pid = std::process::id();
        if let Ok(Some(start)) = crate::process::process_start_identity(pid) {
            let identity = Self { pid, start };
            command.env(
                LAUNCHER_ENV,
                serde_json::to_string(&identity).expect("launcher identity"),
            );
        }
    }

    pub(super) fn inherited() -> Option<Self> {
        serde_json::from_str(&std::env::var(LAUNCHER_ENV).ok()?).ok()
    }

    pub(super) fn is_live(&self) -> bool {
        crate::process::process_identity_state_with_start_identity(
            self.pid,
            None,
            Some(&self.start),
        ) == crate::process::ProcessIdentityState::Live
    }
}

pub(super) fn launch_idle_timeout(default: u64) -> Result<u64> {
    Ok(configured_idle_timeout()?.map_or_else(
        || {
            if std::env::var_os(IDLE_TIMEOUT_ENV).is_some() {
                0
            } else {
                default
            }
        },
        |timeout| timeout.as_secs(),
    ))
}

/// Explicit foreground/service launches remain resident. Automatic launchers
/// supply the bounded default; zero explicitly selects a resident daemon.
pub(super) fn configured_idle_timeout() -> Result<Option<Duration>> {
    std::env::var(IDLE_TIMEOUT_ENV)
        .ok()
        .map(|value| {
            value.trim().parse::<u64>().map_err(|_| {
                crate::error::Error::validation_invalid_argument(
                    IDLE_TIMEOUT_ENV,
                    "daemon idle timeout must be a non-negative number of seconds",
                    Some(value),
                    None,
                )
            })
        })
        .transpose()
        .map(|seconds| {
            seconds
                .filter(|seconds| *seconds > 0)
                .map(Duration::from_secs)
        })
}

/// Global schedules, mirrored observations, and orchestration belong to the
/// admission owner. Draining generations only maintain their own durable work.
pub(super) fn owns_global_work(lease_id: &str) -> bool {
    generation_store::admitting()
        .is_ok_and(|endpoint| endpoint.is_some_and(|endpoint| endpoint.lease_id == lease_id))
}

/// Suffix Linux appends to `/proc/self/exe` (and `std::env::current_exe()`)
/// once the running executable file has been unlinked or replaced.
const REPLACED_EXECUTABLE_SUFFIX: &str = " (deleted)";

/// The executable path to launch Homeboy subcommands from this process.
///
/// After an upgrade or installer replaces the running binary, Linux reports
/// `current_exe()` as `<path> (deleted)`. That path does not exist, so
/// launching it fails and a supervisor could never spawn its own `daemon stop`
/// (#15403). The installed file at the original path is the binary to run.
pub(super) fn launchable_executable() -> std::io::Result<std::path::PathBuf> {
    Ok(launchable_path(std::env::current_exe()?))
}

fn launchable_path(executable: std::path::PathBuf) -> std::path::PathBuf {
    if executable.exists() {
        return executable;
    }
    match executable
        .to_str()
        .and_then(|rendered| rendered.strip_suffix(REPLACED_EXECUTABLE_SUFFIX))
    {
        Some(installed) => std::path::PathBuf::from(installed),
        None => executable,
    }
}

/// True when the file at this process's executable path is no longer the image
/// this process is running, because an upgrade or installer replaced it.
///
/// A daemon on a replaced binary serves stale code and fails admissions that
/// pin its own executable, so an idle one should exit promptly and let the next
/// command start the installed binary instead of waiting out its idle window.
/// A binary removed without a replacement is not "replaced": there is nothing
/// newer to run, so the daemon keeps serving.
pub(super) fn running_executable_replaced() -> bool {
    #[cfg(target_os = "linux")]
    {
        let Ok(link) = std::fs::read_link("/proc/self/exe") else {
            return false;
        };
        let installed = launchable_path(link);
        replaced_relative_to(std::path::Path::new("/proc/self/exe"), &installed)
    }
    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Whether `installed` now names a different file than `running_image`.
#[cfg(unix)]
fn replaced_relative_to(running_image: &std::path::Path, installed: &std::path::Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let (Ok(running), Ok(installed)) = (
        std::fs::metadata(running_image),
        std::fs::metadata(installed),
    ) else {
        return false;
    };
    (running.dev(), running.ino()) != (installed.dev(), installed.ino())
}

pub(super) struct IdleLifetime {
    timeout: Option<Duration>,
    last_activity: Instant,
}

impl IdleLifetime {
    pub(super) fn new(timeout: Option<Duration>) -> Self {
        Self {
            timeout,
            last_activity: Instant::now(),
        }
    }

    pub(super) fn touch(&mut self) {
        self.last_activity = Instant::now();
    }

    /// True once a daemon on a replaced binary has been idle long enough to
    /// stop. The short bound still spaces out retries of a fenced stop, which
    /// re-touches this lifetime, instead of spawning one every poll.
    pub(super) fn replaced_and_settled(&self, replaced: bool) -> bool {
        replaced && self.last_activity.elapsed() >= REPLACED_BINARY_STOP_AFTER
    }

    pub(super) fn expired(&self, draining: bool) -> bool {
        // Even an explicitly resident generation becomes bounded once replaced.
        let timeout = if draining {
            Some(
                self.timeout
                    .unwrap_or(Duration::from_secs(DEFAULT_IDLE_TIMEOUT_SECS)),
            )
        } else {
            self.timeout
        };
        timeout.is_some_and(|timeout| self.last_activity.elapsed() >= timeout)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn launchable_path_strips_the_replaced_suffix_only_when_the_path_is_missing() {
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("homeboy");
        std::fs::write(&installed, b"new").unwrap();
        let replaced = temp.path().join("homeboy (deleted)");
        assert_eq!(launchable_path(replaced), installed);

        // A file genuinely named with the suffix is launched as-is.
        let literal = temp.path().join("odd (deleted)");
        std::fs::write(&literal, b"x").unwrap();
        assert_eq!(launchable_path(literal.clone()), literal);

        let present = temp.path().join("present");
        std::fs::write(&present, b"x").unwrap();
        assert_eq!(launchable_path(present.clone()), present);
    }

    #[test]
    fn replacement_is_detected_by_file_identity() {
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("homeboy");
        std::fs::write(&installed, b"old build").unwrap();
        // A hard link pins the old image the way /proc/self/exe does.
        let running = temp.path().join("running-image");
        std::fs::hard_link(&installed, &running).unwrap();
        assert!(!replaced_relative_to(&running, &installed));

        // Installers replace by writing a new file and renaming it over the path.
        let staged = temp.path().join(".homeboy.new");
        std::fs::write(&staged, b"new build").unwrap();
        std::fs::rename(&staged, &installed).unwrap();
        assert!(replaced_relative_to(&running, &installed));

        // Removed without a replacement: nothing newer to run.
        std::fs::remove_file(&installed).unwrap();
        assert!(!replaced_relative_to(&running, &installed));
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn the_test_binary_itself_is_not_replaced() {
        assert!(!running_executable_replaced());
    }
}
