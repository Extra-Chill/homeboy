//! On-demand daemons own durable work, not an indefinite detached process.

use std::time::{Duration, Instant};

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

/// The file identity of an executable: the path it was launched from and the
/// `(dev, ino)` that path resolved to at the time.
///
/// Installers replace a binary by renaming a new file over its path, so the
/// path keeps resolving while its `(dev, ino)` changes. Comparing the two is a
/// portable replacement check that needs no `/proc`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ExecutableIdentity {
    pub path: std::path::PathBuf,
    pub dev: u64,
    pub ino: u64,
}

impl ExecutableIdentity {
    pub fn of(path: &std::path::Path) -> std::io::Result<Self> {
        let (dev, ino) = file_id(&std::fs::metadata(path)?);
        Ok(Self {
            path: path.to_path_buf(),
            dev,
            ino,
        })
    }

    /// True when the recorded path now names a different file. A path that is
    /// missing with no replacement is not "replaced": there is nothing newer
    /// to run.
    pub fn replaced(&self) -> bool {
        std::fs::metadata(&self.path)
            .is_ok_and(|metadata| file_id(&metadata) != (self.dev, self.ino))
    }
}

#[cfg(unix)]
fn file_id(metadata: &std::fs::Metadata) -> (u64, u64) {
    use std::os::unix::fs::MetadataExt;
    (metadata.dev(), metadata.ino())
}

/// std exposes no stable file identity here, so replacement is never detected.
#[cfg(not(unix))]
fn file_id(_metadata: &std::fs::Metadata) -> (u64, u64) {
    (0, 0)
}

static RUNNING_EXECUTABLE: std::sync::OnceLock<ExecutableIdentity> = std::sync::OnceLock::new();

/// This process's executable identity, recorded on first use. Daemon and
/// supervisor startup call this before serving, so the record names the
/// binary they started from even after an upgrade replaces it.
pub(super) fn running_executable() -> std::io::Result<&'static ExecutableIdentity> {
    if let Some(identity) = RUNNING_EXECUTABLE.get() {
        return Ok(identity);
    }
    let identity = ExecutableIdentity::of(&std::env::current_exe()?)?;
    Ok(RUNNING_EXECUTABLE.get_or_init(|| identity))
}

/// The executable path to launch Homeboy subcommands from this process.
///
/// After an upgrade replaces the running binary, `current_exe()` can name a
/// path that no longer exists (Linux reports `<path> (deleted)`), so a
/// supervisor could never spawn its own `daemon stop` (#15403). The recorded
/// startup path is where the installed binary lives.
pub fn launchable_executable() -> std::io::Result<std::path::PathBuf> {
    Ok(running_executable()?.path.clone())
}

/// True when the file at this process's executable path is no longer the image
/// this process started from, because an upgrade or installer replaced it.
///
/// A daemon on a replaced binary serves stale code and fails admissions that
/// pin its own executable, so an idle one should exit promptly and let the next
/// command start the installed binary instead of waiting out its idle window.
pub(super) fn running_executable_replaced() -> bool {
    RUNNING_EXECUTABLE
        .get()
        .is_some_and(ExecutableIdentity::replaced)
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

    pub(super) fn expired(&self) -> bool {
        self.timeout
            .is_some_and(|timeout| self.last_activity.elapsed() >= timeout)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_new_file_renamed_over_the_recorded_path_is_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("homeboy");
        std::fs::write(&installed, b"old build").unwrap();
        let recorded = ExecutableIdentity::of(&installed).unwrap();
        assert!(!recorded.replaced());

        // Installers replace by writing a new file and renaming it over the path.
        let staged = temp.path().join(".homeboy.new");
        std::fs::write(&staged, b"new build").unwrap();
        std::fs::rename(&staged, &installed).unwrap();
        assert!(recorded.replaced());
    }

    #[test]
    fn a_recorded_path_deleted_without_replacement_is_not_replaced() {
        let temp = tempfile::tempdir().unwrap();
        let installed = temp.path().join("homeboy");
        std::fs::write(&installed, b"old build").unwrap();
        let recorded = ExecutableIdentity::of(&installed).unwrap();
        std::fs::remove_file(&installed).unwrap();
        assert!(!recorded.replaced());
    }

    #[test]
    fn the_test_binary_itself_is_not_replaced() {
        let recorded = running_executable().unwrap();
        assert_eq!(launchable_executable().unwrap(), recorded.path);
        assert!(!running_executable_replaced());
    }
}
