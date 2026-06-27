use super::super::types::RunnerDaemonDriftEntry;
use super::super::types::RunnerExtensionSyncEntry;
use super::super::types::RunnerUpgradeEntry;
use super::*;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerStatusReport;
use crate::core::Result;

pub fn runner_version_report_detail(
    detail: String,
    previous_version: Option<&str>,
    new_version: Option<&str>,
) -> String {
    format!(
        "{detail}\nrunner version check: local/current {}; runner before {}; runner after {}",
        effective_local_version(),
        previous_version.unwrap_or("unknown"),
        new_version.unwrap_or("unknown")
    )
}

pub fn runner_stale_daemon(
    runner: &Runner,
    status: &impl Fn(&str) -> Result<RunnerStatusReport>,
) -> Option<RunnerDaemonDriftEntry> {
    let warning = status(&runner.id).ok()?.stale_daemon?;
    Some(RunnerDaemonDriftEntry {
        session_homeboy_version: warning.session_homeboy_version,
        current_homeboy_version: warning.current_homeboy_version,
        recovery_commands: warning.recovery_commands,
    })
}

pub fn runner_upgrade_final_detail(
    runner_id: &str,
    detail: String,
    path_update_detail: Option<&str>,
    stale_daemon_repair_detail: Option<&str>,
    path_drift: Option<&str>,
    stale_daemon: Option<&RunnerDaemonDriftEntry>,
    extensions_skipped: &[RunnerExtensionSyncEntry],
    extensions_failed: &[RunnerExtensionSyncEntry],
) -> String {
    let mut parts = vec![detail];

    if let Some(path_update_detail) = path_update_detail {
        parts.push(path_update_detail.to_string());
    }

    if let Some(stale_daemon_repair_detail) = stale_daemon_repair_detail {
        parts.push(stale_daemon_repair_detail.to_string());
    }

    if !extensions_skipped.is_empty() {
        parts.push(format!(
            "{} runner extension sync(s) skipped: {}",
            extensions_skipped.len(),
            extensions_skipped
                .iter()
                .map(|entry| format!(
                    "{}@{} ({})",
                    entry.extension_id,
                    entry.source_revision,
                    entry.detail.as_deref().unwrap_or("no detail")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
    }

    if !extensions_failed.is_empty() {
        parts.push(format!(
            "{} runner extension sync(s) failed: {}",
            extensions_failed.len(),
            extensions_failed
                .iter()
                .map(|entry| format!(
                    "{}@{} ({})",
                    entry.extension_id,
                    entry.source_revision,
                    entry.detail.as_deref().unwrap_or("no detail")
                ))
                .collect::<Vec<_>>()
                .join("; ")
        ));
        parts.push(format!(
            "retry failed runner sync with `{}` or retry an individual failed extension using its recovery_commands entry",
            runner_upgrade_recovery_commands(runner_id).join(" && ")
        ));
    }

    if let Some(path_drift) = path_drift {
        parts.push(format!("runner PATH drift detected: {path_drift}"));
    }

    if let Some(stale_daemon) = stale_daemon {
        let remediation = stale_daemon.recovery_commands.join(" && ");
        let remediation = if remediation.is_empty() {
            "homeboy runner disconnect <runner> && homeboy runner connect <runner>".to_string()
        } else {
            remediation
        };
        parts.push(format!(
            "connected runner daemon is stale: active daemon control plane reports {}, job command binary reports {}; refresh with `{}`",
            stale_daemon.session_homeboy_version,
            stale_daemon.current_homeboy_version,
            remediation
        ));
    }

    parts.join("\n")
}

pub fn runner_upgrade_detail(output: &runner::RunnerExecOutput) -> String {
    let stdout = output.stdout.trim();
    let stderr = output.stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{}\n{}", stdout, stderr),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (true, true) => "runner upgrade produced no output".to_string(),
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
