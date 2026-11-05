//! Which service manager, if any, owns the running `daemon serve` (#17387).
//!
//! A service manager (a systemd unit with `Restart=always`, a launchd job)
//! restarts the daemon itself whenever it exits. Homeboy must never start a
//! second daemon beside it: that one takes the owner lock and the manager's
//! own restart then crash-loops. The serving process records its manager in
//! the lease, so every CLI path can see who owns the restart.
//!
//! Detection uses what the manager itself sets. systemd sets `INVOCATION_ID`
//! for every process it starts for a unit, and launchd sets
//! `XPC_SERVICE_NAME`. Both are inherited by descendants (a shell in a
//! terminal that runs as a user service, a test harness running in a CI
//! unit), so the variable only counts when the manager is also the serving
//! process's direct parent. Homeboy's own `daemon supervise` launcher is the
//! parent of every daemon it spawns, so those are never mistaken for managed.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DaemonServiceManager {
    Systemd,
    Launchd,
}

impl DaemonServiceManager {
    /// The manager that started this process directly, if any.
    pub fn detect() -> Option<Self> {
        Self::detect_from(
            |key| std::env::var(key).ok().filter(|value| !value.is_empty()),
            parent_process_name().as_deref(),
        )
    }

    fn detect_from(var: impl Fn(&str) -> Option<String>, parent: Option<&str>) -> Option<Self> {
        match parent? {
            "systemd" if var("INVOCATION_ID").is_some() => Some(Self::Systemd),
            // macOS terminal sessions carry `XPC_SERVICE_NAME=0`.
            "launchd" if var("XPC_SERVICE_NAME").is_some_and(|name| name != "0") => {
                Some(Self::Launchd)
            }
            _ => None,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Systemd => "systemd",
            Self::Launchd => "launchd",
        }
    }
}

/// Basename of this process's parent executable.
fn parent_process_name() -> Option<String> {
    #[cfg(unix)]
    {
        // SAFETY: getppid has no preconditions and cannot fail.
        let ppid = unsafe { libc::getppid() };
        let output = std::process::Command::new("ps")
            .args(["-o", "comm=", "-p", &ppid.to_string()])
            .output()
            .ok()
            .filter(|output| output.status.success())?;
        let comm = String::from_utf8_lossy(&output.stdout).trim().to_string();
        std::path::Path::new(&comm)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
    }
    #[cfg(not(unix))]
    {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::DaemonServiceManager;

    fn env<'a>(pairs: &'a [(&'a str, &'a str)]) -> impl Fn(&str) -> Option<String> + 'a {
        move |key| {
            pairs
                .iter()
                .find(|(name, _)| *name == key)
                .map(|(_, value)| value.to_string())
        }
    }

    #[test]
    fn a_systemd_unit_process_is_systemd_managed() {
        assert_eq!(
            DaemonServiceManager::detect_from(env(&[("INVOCATION_ID", "abc")]), Some("systemd")),
            Some(DaemonServiceManager::Systemd)
        );
    }

    #[test]
    fn a_launchd_job_is_launchd_managed() {
        assert_eq!(
            DaemonServiceManager::detect_from(
                env(&[("XPC_SERVICE_NAME", "com.extrachill.homeboy")]),
                Some("launchd")
            ),
            Some(DaemonServiceManager::Launchd)
        );
    }

    /// The manager variables leak into descendants; only a direct child of the
    /// manager is owned by it. A daemon under `daemon supervise`, a shell, or a
    /// test harness is not.
    #[test]
    fn an_inherited_manager_variable_without_the_manager_as_parent_is_unmanaged() {
        let vars = [("INVOCATION_ID", "abc"), ("XPC_SERVICE_NAME", "com.x")];
        for parent in [Some("homeboy"), Some("bash"), Some("cargo"), None] {
            assert_eq!(DaemonServiceManager::detect_from(env(&vars), parent), None);
        }
    }

    #[test]
    fn a_manager_parent_without_its_variable_is_unmanaged() {
        assert_eq!(
            DaemonServiceManager::detect_from(env(&[]), Some("systemd")),
            None
        );
        assert_eq!(
            DaemonServiceManager::detect_from(env(&[("XPC_SERVICE_NAME", "0")]), Some("launchd")),
            None
        );
    }
}
