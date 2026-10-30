use super::*;
use homeboy_upgrade::upgrade::RunnerUpgradeEntry;

/// Builds a non-recoverable runner upgrade failure entry, preserving every
/// `RunnerUpgradeEntry` field at its canonical "failed before completion" value.
///
/// The caller is responsible for assigning `previous_version`, which is left as
/// `None` here so the surrounding flow can move it in once.
pub fn runner_upgrade_failure_entry(
    runner_id: &str,
    homeboy_path: String,
    previous_version: Option<String>,
    exit_code: i32,
    detail: String,
) -> RunnerUpgradeEntry {
    let recovery_commands = vec![format!(
        "homeboy upgrade --upgrade-runner {}",
        shell_arg(runner_id)
    )];
    RunnerUpgradeEntry {
        runner_id: runner_id.to_string(),
        homeboy_path,
        success: false,
        upgraded: false,
        deferred: false,
        previous_version,
        new_version: None,
        bare_homeboy_version: None,
        path_drift: None,
        recovery_commands,
        extensions_synced: Vec::new(),
        extensions_skipped: Vec::new(),
        extensions_failed: Vec::new(),
        stale_daemon: None,
        daemon_previous_version: None,
        daemon_new_version: None,
        service_binaries: Vec::new(),
        exit_code,
        detail,
    }
}
