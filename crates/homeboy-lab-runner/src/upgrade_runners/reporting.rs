use homeboy_upgrade::upgrade::{RunnerDaemonDriftEntry, RunnerUpgradeEntry};

pub(super) fn runner_daemon_drift_entry(
    warning: crate::RunnerStaleDaemonWarning,
) -> RunnerDaemonDriftEntry {
    let recovery_commands = warning.safe_recovery_commands();
    RunnerDaemonDriftEntry {
        session_homeboy_version: warning.session_homeboy_version,
        current_homeboy_version: warning.current_homeboy_version,
        recovery_commands,
    }
}

pub fn runner_upgrade_summary(entry: &RunnerUpgradeEntry) -> String {
    match (
        entry.previous_version.as_deref(),
        entry.new_version.as_deref(),
        entry.upgraded,
    ) {
        (Some(previous), Some(new), true) => format!("{} -> {}", previous, new),
        (_, Some(new), false) => format!("{} (up to date)", new),
        _ => "updated".to_string(),
    }
}
