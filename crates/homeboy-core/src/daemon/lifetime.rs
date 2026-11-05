//! On-demand daemons own durable work, not an indefinite detached process.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
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
/// path that no longer exists (Linux reports `<path> (deleted)`), so nothing
/// could be launched from it (#15403). The recorded startup path is where the
/// installed binary lives.
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

/// How often a supervised `serve` re-evaluates its own lifetime.
const LIFETIME_POLL: Duration = Duration::from_secs(1);

/// True only when the daemon's state directory is provably gone.
///
/// A missing or unreadable `state.json` is normal during startup, before the
/// daemon publishes its lease, so only the directory's absence counts. An
/// existence check that errors (permissions, I/O) is not proof of removal and
/// keeps the daemon alive.
pub(super) fn state_dir_removed(state_path: &Path) -> bool {
    state_path
        .parent()
        .is_some_and(|dir| matches!(dir.try_exists(), Ok(false)))
}

/// A `serve` owns its lifetime exactly when a supervisor launched it: the
/// launch carries a startup token. A hand-started foreground `serve` has no
/// token and stays resident. A service-managed `serve` may carry a token but
/// its manager owns its lifetime; an idle exit under `Restart=always` would
/// only loop.
pub(super) fn owns_lifetime(
    startup_token: &str,
    service_manager: Option<super::DaemonServiceManager>,
) -> bool {
    service_manager.is_none() && !startup_token.is_empty()
}

/// The idle policy this `serve` enforces on itself, or `None` when it does not
/// own its lifetime. Read before the lease is published, so an invalid idle
/// window fails startup instead of leaving a lease behind.
pub(super) fn owned_idle_lifetime(
    service_manager: Option<super::DaemonServiceManager>,
) -> Result<Option<IdleLifetime>> {
    let startup_token = std::env::var(super::DAEMON_STARTUP_TOKEN_ENV).unwrap_or_default();
    if !owns_lifetime(&startup_token, service_manager) {
        return Ok(None);
    }
    Ok(Some(IdleLifetime::new(
        configured_idle_timeout()?,
        Instant::now(),
    )))
}

/// One poll's view of this daemon's lifetime inputs.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct LifetimeObservation {
    /// Nothing can reach this daemon any more: no client can discover its
    /// lease, read its jobs, or run a lease-scoped stop (#15590).
    pub(super) state_dir_removed: bool,
    /// The published lease is this process's own.
    pub(super) own_lease: bool,
    /// Active jobs in this daemon's scope (or an unreadable count), or a live
    /// launcher still returning the startup handoff.
    pub(super) busy: bool,
    /// The executable this daemon started from was replaced (#15403).
    pub(super) binary_replaced: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum LifetimeDecision {
    Continue,
    /// Stop without the lifecycle lock, which lived in the removed directory.
    StopNow,
    /// Stop only if the lifecycle lock is free and the daemon is still idle.
    StopIfStillIdle,
}

pub(super) struct IdleLifetime {
    timeout: Option<Duration>,
    last_activity: Instant,
}

impl IdleLifetime {
    pub(super) fn new(timeout: Option<Duration>, now: Instant) -> Self {
        Self {
            timeout,
            last_activity: now,
        }
    }

    pub(super) fn decide(
        &mut self,
        now: Instant,
        observation: &LifetimeObservation,
    ) -> LifetimeDecision {
        if observation.state_dir_removed {
            return LifetimeDecision::StopNow;
        }
        // An unpublished or foreign lease is not this daemon idling.
        if !observation.own_lease || observation.busy {
            self.last_activity = now;
            return LifetimeDecision::Continue;
        }
        let idle_for = now.saturating_duration_since(self.last_activity);
        let expired = self.timeout.is_some_and(|timeout| idle_for >= timeout);
        // An idle daemon whose binary was replaced stops after a short settle
        // instead of serving stale code until its idle window lapses; the next
        // command starts the installed binary (#15403).
        let replaced = observation.binary_replaced && idle_for >= REPLACED_BINARY_STOP_AFTER;
        if !expired && !replaced {
            return LifetimeDecision::Continue;
        }
        // A stop that cannot proceed now is retried after another complete
        // idle window, not on every poll.
        self.last_activity = now;
        LifetimeDecision::StopIfStillIdle
    }
}

/// Enforce this daemon's own lifetime until `stop` fires, ending the daemon
/// through `shutdown`, the same graceful drain SIGTERM uses.
pub(super) fn spawn_lifetime_monitor(
    mut idle: IdleLifetime,
    state_path: PathBuf,
    lease_id: String,
    jobs: PathBuf,
    shutdown: Arc<AtomicBool>,
    held: SelfStopGuards,
    stop: mpsc::Receiver<()>,
) -> std::thread::JoinHandle<()> {
    let launcher = LauncherIdentity::inherited();
    std::thread::Builder::new()
        .name("daemon-lifetime".to_string())
        .spawn(move || {
            let observe = || observe_lifetime(&state_path, &lease_id, &jobs, launcher.as_ref());
            loop {
                match stop.recv_timeout(LIFETIME_POLL) {
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    _ => return,
                }
                let stopped = match idle.decide(Instant::now(), &observe()) {
                    LifetimeDecision::Continue => false,
                    LifetimeDecision::StopNow => {
                        shutdown.store(true, Ordering::Release);
                        true
                    }
                    LifetimeDecision::StopIfStillIdle => {
                        stop_if_still_idle(&shutdown, &held, observe)
                    }
                };
                if stopped {
                    return;
                }
            }
        })
        .expect("spawn daemon lifetime monitor")
}

fn observe_lifetime(
    state_path: &Path,
    lease_id: &str,
    jobs: &Path,
    launcher: Option<&LauncherIdentity>,
) -> LifetimeObservation {
    let own_lease = super::validate_lease_file(state_path)
        .ok()
        .and_then(|validation| validation.state)
        .is_some_and(|state| state.lease_id == lease_id && state.pid == std::process::id());
    let scope = super::daemon_scope_for_state_path(state_path);
    let active_jobs =
        super::JobStore::active_count_at_path(jobs, Some(&scope)).map_or(true, |count| count > 0);
    LifetimeObservation {
        state_dir_removed: state_dir_removed(state_path),
        own_lease,
        busy: active_jobs || launcher.is_some_and(LauncherIdentity::is_live),
        binary_replaced: running_executable_replaced(),
    }
}

/// The lifecycle lock and admission fence an idle self-stop holds until
/// `serve` has drained and released its lease.
pub(super) type SelfStopGuards =
    Arc<std::sync::Mutex<Option<(super::DaemonOperationLock, super::DaemonAdmissionFence)>>>;

/// Serialize an idle self-stop with lifecycle commands, as `daemon stop` does.
/// A busy lock or admission fence means a lifecycle command or an admission
/// owns this moment; the caller retries after another idle window.
///
/// On success the guards move into `held`, which `serve` drops only after its
/// teardown. Like `daemon stop`, which holds both until the daemon exits, this
/// keeps a concurrent `ensure-running` or admission from finding the draining
/// daemon's still-published lease.
fn stop_if_still_idle(
    shutdown: &AtomicBool,
    held: &SelfStopGuards,
    observe: impl Fn() -> LifetimeObservation,
) -> bool {
    let Ok(Some(lock)) = super::try_acquire_daemon_operation_lock() else {
        return false;
    };
    let Ok(Some(fence)) = super::try_acquire_daemon_job_admission_fence() else {
        return false;
    };
    let observation = observe();
    if observation.state_dir_removed || !observation.own_lease || observation.busy {
        return false;
    }
    *held.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some((lock, fence));
    shutdown.store(true, Ordering::Release);
    true
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

    const IDLE: LifetimeObservation = LifetimeObservation {
        state_dir_removed: false,
        own_lease: true,
        busy: false,
        binary_replaced: false,
    };

    #[test]
    fn an_idle_daemon_with_no_launcher_and_no_jobs_stops_after_its_idle_window() {
        let start = Instant::now();
        let mut idle = IdleLifetime::new(Some(Duration::from_secs(300)), start);
        assert_eq!(
            idle.decide(start + Duration::from_secs(299), &IDLE),
            LifetimeDecision::Continue
        );
        assert_eq!(
            idle.decide(start + Duration::from_secs(300), &IDLE),
            LifetimeDecision::StopIfStillIdle
        );
        // A stop that could not take the lifecycle lock retries only after
        // another complete idle window.
        assert_eq!(
            idle.decide(start + Duration::from_secs(301), &IDLE),
            LifetimeDecision::Continue
        );
        assert_eq!(
            idle.decide(start + Duration::from_secs(600), &IDLE),
            LifetimeDecision::StopIfStillIdle
        );
    }

    #[test]
    fn active_jobs_or_a_live_launcher_keep_the_daemon_alive() {
        let start = Instant::now();
        let mut idle = IdleLifetime::new(Some(Duration::from_secs(10)), start);
        let busy = LifetimeObservation { busy: true, ..IDLE };
        for seconds in [10, 100, 1000] {
            assert_eq!(
                idle.decide(start + Duration::from_secs(seconds), &busy),
                LifetimeDecision::Continue
            );
        }
        // The idle window restarts from the last busy observation.
        assert_eq!(
            idle.decide(start + Duration::from_secs(1009), &IDLE),
            LifetimeDecision::Continue
        );
        assert_eq!(
            idle.decide(start + Duration::from_secs(1010), &IDLE),
            LifetimeDecision::StopIfStillIdle
        );
    }

    #[test]
    fn an_unpublished_or_foreign_lease_is_not_idle_time() {
        let start = Instant::now();
        let mut idle = IdleLifetime::new(Some(Duration::from_secs(10)), start);
        let foreign = LifetimeObservation {
            own_lease: false,
            binary_replaced: true,
            ..IDLE
        };
        assert_eq!(
            idle.decide(start + Duration::from_secs(100), &foreign),
            LifetimeDecision::Continue
        );
    }

    #[test]
    fn a_replaced_binary_stops_an_idle_daemon_early_but_waits_for_work() {
        let start = Instant::now();
        let mut idle = IdleLifetime::new(None, start);
        let replaced = LifetimeObservation {
            binary_replaced: true,
            ..IDLE
        };
        assert_eq!(
            idle.decide(start + REPLACED_BINARY_STOP_AFTER / 2, &replaced),
            LifetimeDecision::Continue
        );
        assert_eq!(
            idle.decide(start + REPLACED_BINARY_STOP_AFTER, &replaced),
            LifetimeDecision::StopIfStillIdle
        );
        let busy = LifetimeObservation {
            busy: true,
            ..replaced
        };
        let later = start + REPLACED_BINARY_STOP_AFTER * 10;
        assert_eq!(idle.decide(later, &busy), LifetimeDecision::Continue);
        // A resident daemon (no idle window) never stops for idleness alone.
        assert_eq!(
            idle.decide(later + REPLACED_BINARY_STOP_AFTER * 10, &IDLE),
            LifetimeDecision::Continue
        );
    }

    #[test]
    fn a_removed_state_directory_stops_the_daemon_even_with_work() {
        let start = Instant::now();
        let mut idle = IdleLifetime::new(None, start);
        let removed = LifetimeObservation {
            state_dir_removed: true,
            own_lease: false,
            busy: true,
            binary_replaced: false,
        };
        assert_eq!(idle.decide(start, &removed), LifetimeDecision::StopNow);
    }

    #[test]
    fn only_a_present_state_directory_is_not_removed() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("daemon");
        std::fs::create_dir_all(&state_dir).unwrap();
        let state_path = state_dir.join("state.json");
        // An unpublished lease is the startup window, not removal.
        assert!(!state_dir_removed(&state_path));
        std::fs::remove_dir_all(&state_dir).unwrap();
        assert!(state_dir_removed(&state_path));
    }

    #[test]
    fn the_monitor_ends_its_daemon_through_the_shutdown_flag_once_its_state_dir_is_removed() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("daemon");
        std::fs::create_dir_all(&state_dir).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (stop_tx, stop_rx) = mpsc::channel();
        let monitor = spawn_lifetime_monitor(
            IdleLifetime::new(Some(Duration::from_secs(300)), Instant::now()),
            state_dir.join("state.json"),
            "lease".to_string(),
            temp.path().join("jobs.sqlite"),
            Arc::clone(&shutdown),
            SelfStopGuards::default(),
            stop_rx,
        );
        std::thread::sleep(LIFETIME_POLL * 2);
        assert!(
            !shutdown.load(Ordering::Acquire),
            "an unpublished lease in a present directory keeps the daemon"
        );
        std::fs::remove_dir_all(&state_dir).unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        while !monitor.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
        }
        assert!(shutdown.load(Ordering::Acquire));
        assert!(monitor.is_finished());
        drop(stop_tx);
    }

    #[test]
    fn the_monitor_stops_when_serve_drains() {
        let temp = tempfile::tempdir().unwrap();
        let state_dir = temp.path().join("daemon");
        std::fs::create_dir_all(&state_dir).unwrap();
        let shutdown = Arc::new(AtomicBool::new(false));
        let (stop_tx, stop_rx) = mpsc::channel();
        let monitor = spawn_lifetime_monitor(
            IdleLifetime::new(None, Instant::now()),
            state_dir.join("state.json"),
            "lease".to_string(),
            temp.path().join("jobs.sqlite"),
            Arc::clone(&shutdown),
            SelfStopGuards::default(),
            stop_rx,
        );
        stop_tx.send(()).unwrap();
        monitor.join().unwrap();
        assert!(!shutdown.load(Ordering::Acquire));
    }

    #[test]
    fn only_a_supervised_unmanaged_serve_owns_its_lifetime() {
        use crate::daemon::DaemonServiceManager;
        assert!(owns_lifetime("token", None));
        // A hand-started foreground serve has no startup token.
        assert!(!owns_lifetime("", None));
        // A service-managed serve runs no lifetime monitor, token or not.
        for manager in [DaemonServiceManager::Systemd, DaemonServiceManager::Launchd] {
            assert!(!owns_lifetime("token", Some(manager)));
            assert!(!owns_lifetime("", Some(manager)));
        }
    }

    #[test]
    fn the_test_binary_itself_is_not_replaced() {
        let recorded = running_executable().unwrap();
        assert_eq!(launchable_executable().unwrap(), recorded.path);
        assert!(!running_executable_replaced());
    }
}
