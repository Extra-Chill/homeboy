use regex::Regex;

use crate::core::runner::{
    self, Runner, RunnerCapabilityPreflight, RunnerExecOptions, RunnerKind, RunnerRequiredTool,
    RunnerStatusReport,
};
use crate::core::upgrade::ExtensionUpgradeEntry;
use crate::core::Result;

use super::types::{RunnerDaemonDriftEntry, RunnerExtensionSyncEntry, RunnerUpgradeEntry};

pub(super) fn upgrade_configured_runners(
    runner_targets: &[String],
    extension_updates: &[ExtensionUpgradeEntry],
) -> Result<(Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>)> {
    let runners = runner_upgrade_targets(runner_targets)?;
    if runners.is_empty() {
        return Ok((vec![], vec![]));
    }

    crate::log_status!(
        "upgrade",
        "Updating {} configured runner(s)...",
        runners.len()
    );
    Ok(upgrade_runners_with_executor(
        &runners,
        extension_updates,
        runner::exec,
        runner::status,
    ))
}

fn runner_upgrade_targets(runner_targets: &[String]) -> Result<Vec<Runner>> {
    if !runner_targets.is_empty() {
        return runner_targets
            .iter()
            .map(|runner_id| runner::load(runner_id))
            .collect();
    }

    Ok(runner::list()?
        .into_iter()
        .filter(|runner| runner.kind == RunnerKind::Ssh)
        .collect())
}

fn upgrade_runners_with_executor(
    runners: &[Runner],
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for runner in runners {
        let entry = upgrade_runner_with_executor(runner, extension_updates, &mut exec, &status);
        if entry.success {
            crate::log_status!(
                "upgrade",
                "  {} {}",
                entry.runner_id,
                runner_upgrade_summary(&entry)
            );
            updated.push(entry);
        } else {
            crate::log_status!("upgrade", "  {} skipped: {}", entry.runner_id, entry.detail);
            skipped.push(entry);
        }
    }

    (updated, skipped)
}

fn upgrade_runner_with_executor(
    runner: &Runner,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: &impl Fn(&str) -> Result<RunnerStatusReport>,
) -> RunnerUpgradeEntry {
    let homeboy_path = runner
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    let previous_version = runner_homeboy_version(runner, &homeboy_path, exec)
        .ok()
        .flatten();
    let upgrade = exec(
        &runner.id,
        runner_exec_options(
            runner,
            vec![
                homeboy_path.clone(),
                "upgrade".to_string(),
                "--no-restart".to_string(),
                "--skip-runners".to_string(),
            ],
        ),
    );

    let (exit_code, detail) = match upgrade {
        Ok((output, exit_code)) if exit_code == 0 => (exit_code, runner_upgrade_detail(&output)),
        Ok((output, exit_code)) => {
            return RunnerUpgradeEntry {
                runner_id: runner.id.clone(),
                homeboy_path,
                success: false,
                upgraded: false,
                previous_version,
                new_version: None,
                bare_homeboy_version: None,
                path_drift: None,
                extensions_synced: Vec::new(),
                extensions_failed: Vec::new(),
                stale_daemon: None,
                exit_code,
                detail: runner_upgrade_detail(&output),
            };
        }
        Err(err) => {
            return RunnerUpgradeEntry {
                runner_id: runner.id.clone(),
                homeboy_path,
                success: false,
                upgraded: false,
                previous_version,
                new_version: None,
                bare_homeboy_version: None,
                path_drift: None,
                extensions_synced: Vec::new(),
                extensions_failed: Vec::new(),
                stale_daemon: None,
                exit_code: 1,
                detail: err.message,
            };
        }
    };

    let new_version = runner_homeboy_version(runner, &homeboy_path, exec)
        .ok()
        .flatten();
    let (extensions_synced, extensions_failed) =
        sync_runner_extensions(runner, &homeboy_path, extension_updates, exec);
    let bare_homeboy_version = runner_bare_homeboy_version(runner, &homeboy_path, exec);
    let path_drift = runner_path_drift(
        &homeboy_path,
        new_version.as_deref(),
        bare_homeboy_version.as_deref(),
    );
    let stale_daemon = runner_stale_daemon(runner, status);
    let upgraded = match (previous_version.as_deref(), new_version.as_deref()) {
        (Some(previous), Some(new)) => new != previous,
        _ => false,
    };
    let success = new_version.is_some() && extensions_failed.is_empty() && stale_daemon.is_none();
    let detail = runner_upgrade_final_detail(
        detail,
        path_drift.as_deref(),
        stale_daemon.as_ref(),
        &extensions_failed,
    );

    RunnerUpgradeEntry {
        runner_id: runner.id.clone(),
        homeboy_path,
        success,
        upgraded,
        previous_version,
        new_version,
        bare_homeboy_version,
        path_drift,
        extensions_synced,
        extensions_failed,
        stale_daemon,
        exit_code,
        detail,
    }
}

fn sync_runner_extensions(
    runner: &Runner,
    homeboy_path: &str,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> (Vec<RunnerExtensionSyncEntry>, Vec<RunnerExtensionSyncEntry>) {
    let mut synced = Vec::new();
    let mut failed = Vec::new();
    for extension in extension_updates {
        let Some(source_url) = extension.source_url.as_deref() else {
            continue;
        };
        let Some(source_revision) = extension.source_revision.as_deref() else {
            continue;
        };

        let exists =
            match runner_extension_exists(runner, homeboy_path, &extension.extension_id, exec) {
                Ok(exists) => exists,
                Err(detail) => {
                    failed.push(RunnerExtensionSyncEntry {
                        extension_id: extension.extension_id.clone(),
                        source_revision: source_revision.to_string(),
                        synced: false,
                        detail: Some(detail),
                    });
                    continue;
                }
            };
        let mut command = vec![
            homeboy_path.to_string(),
            "extension".to_string(),
            "install".to_string(),
            source_url.to_string(),
            "--id".to_string(),
            extension.extension_id.clone(),
            "--ref".to_string(),
            source_revision.to_string(),
        ];
        if exists {
            command.push("--replace".to_string());
        }

        let result = exec(&runner.id, runner_exec_options(runner, command.clone()));
        match result {
            Ok((output, 0)) => {
                crate::log_status!(
                    "upgrade",
                    "  {} extension {} synced at {}",
                    runner.id,
                    extension.extension_id,
                    source_revision
                );
                let _ = output;
                synced.push(RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.to_string(),
                    synced: true,
                    detail: None,
                });
            }
            Ok((output, exit_code)) => {
                failed.push(RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.to_string(),
                    synced: false,
                    detail: Some(format!(
                        "sync failed with exit code {}: {}",
                        exit_code,
                        runner_upgrade_detail(&output)
                    )),
                });
            }
            Err(err) => {
                failed.push(RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.to_string(),
                    synced: false,
                    detail: Some(format!("sync failed: {}", err.message)),
                });
            }
        }
    }

    (synced, failed)
}

fn runner_extension_exists(
    runner: &Runner,
    homeboy_path: &str,
    extension_id: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> std::result::Result<bool, String> {
    let result = exec(
        &runner.id,
        runner_exec_options(
            runner,
            vec![
                homeboy_path.to_string(),
                "extension".to_string(),
                "show".to_string(),
                extension_id.to_string(),
            ],
        ),
    );

    match result {
        Ok((_output, 0)) => Ok(true),
        Ok((_output, 4)) => Ok(false),
        Ok((output, exit_code)) => Err(format!(
            "extension {} lookup failed with exit code {}: {}",
            extension_id,
            exit_code,
            runner_upgrade_detail(&output)
        )),
        Err(err) => Err(format!(
            "extension {} lookup failed: {}",
            extension_id, err.message
        )),
    }
}

fn runner_homeboy_version(
    runner: &Runner,
    homeboy_path: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Result<Option<String>> {
    let (output, exit_code) = exec(
        &runner.id,
        runner_exec_options(
            runner,
            vec![homeboy_path.to_string(), "--version".to_string()],
        ),
    )?;
    if exit_code != 0 {
        return Ok(None);
    }

    Ok(parse_cli_version_output(&output.stdout)
        .or_else(|| parse_cli_version_output(&output.stderr)))
}

fn runner_bare_homeboy_version(
    runner: &Runner,
    homeboy_path: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Option<String> {
    if homeboy_path == "homeboy" {
        return None;
    }

    runner_homeboy_version(runner, "homeboy", exec)
        .ok()
        .flatten()
}

fn runner_path_drift(
    homeboy_path: &str,
    configured_version: Option<&str>,
    bare_version: Option<&str>,
) -> Option<String> {
    if homeboy_path == "homeboy" {
        return None;
    }
    let configured_version = configured_version?;
    let bare_version = bare_version?;
    if bare_version == configured_version {
        return None;
    }

    Some(format!(
        "configured runner executable `{}` reports {}, but bare `homeboy` reports {}",
        homeboy_path, configured_version, bare_version
    ))
}

fn runner_stale_daemon(
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

fn runner_upgrade_final_detail(
    detail: String,
    path_drift: Option<&str>,
    stale_daemon: Option<&RunnerDaemonDriftEntry>,
    extensions_failed: &[RunnerExtensionSyncEntry],
) -> String {
    let mut parts = vec![detail];

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
    }

    if let Some(path_drift) = path_drift {
        parts.push(format!("runner PATH drift detected: {path_drift}"));
    }

    if let Some(stale_daemon) = stale_daemon {
        let remediation = stale_daemon
            .recovery_commands
            .first()
            .cloned()
            .unwrap_or_else(|| "homeboy runner connect <runner>".to_string());
        parts.push(format!(
            "connected runner daemon is stale: session reports {}, configured executable reports {}; refresh with `{}`",
            stale_daemon.session_homeboy_version,
            stale_daemon.current_homeboy_version,
            remediation
        ));
    }

    parts.join("\n")
}

fn runner_exec_options(runner: &Runner, command: Vec<String>) -> RunnerExecOptions {
    RunnerExecOptions {
        cwd: None,
        project_id: None,
        allow_diagnostic_ssh: true,
        command,
        env: runner.env.clone(),
        capture_patch: false,
        raw_exec: false,
        source_snapshot: None,
        capability_preflight: Some(runner_upgrade_capability_plan()),
        required_extensions: Vec::new(),
        require_paths: Vec::new(),
    }
}

fn runner_upgrade_capability_plan() -> RunnerCapabilityPreflight {
    RunnerCapabilityPreflight {
        command: "homeboy upgrade".to_string(),
        required_tools: vec![RunnerRequiredTool::Homeboy],
        required_commands: Vec::new(),
        required_components: Vec::new(),
        required_env: Vec::new(),
    }
}

fn parse_cli_version_output(output: &str) -> Option<String> {
    let re = Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(output).map(|m| m.as_str().to_string())
}

fn runner_upgrade_detail(output: &runner::RunnerExecOutput) -> String {
    let stdout = output.stdout.trim();
    let stderr = output.stderr.trim();
    match (stdout.is_empty(), stderr.is_empty()) {
        (false, false) => format!("{}\n{}", stdout, stderr),
        (false, true) => stdout.to_string(),
        (true, false) => stderr.to_string(),
        (true, true) => "runner upgrade produced no output".to_string(),
    }
}

fn runner_upgrade_summary(entry: &RunnerUpgradeEntry) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::runner::{
        RunnerExecMode, RunnerExecOutput, RunnerSessionState, RunnerStaleDaemonWarning,
    };
    use crate::core::server::RunnerSettings;
    use std::collections::HashMap;

    #[test]
    fn upgrades_configured_runner_with_homeboy_path_and_skip_runner_guard() {
        let runner = ssh_runner("lab", Some("/home/chubes/.local/bin/homeboy"));
        let mut commands = Vec::new();
        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            &[],
            |runner_id, options| {
                commands.push((
                    runner_id.to_string(),
                    options.command.clone(),
                    options.allow_diagnostic_ssh,
                ));
                let stdout = match commands.len() {
                    1 => "homeboy 0.199.1\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.199.2\n",
                    4 => "homeboy 0.199.2\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].runner_id, "lab");
        assert!(updated[0].success);
        assert!(updated[0].upgraded);
        assert_eq!(updated[0].previous_version.as_deref(), Some("0.199.1"));
        assert_eq!(updated[0].new_version.as_deref(), Some("0.199.2"));
        assert_eq!(
            commands[1].1,
            vec![
                "/home/chubes/.local/bin/homeboy",
                "upgrade",
                "--no-restart",
                "--skip-runners"
            ]
        );
        assert!(commands.iter().all(|(_, _, allow_ssh)| *allow_ssh));

        let capability_plan = runner_upgrade_capability_plan();
        assert_eq!(
            capability_plan.required_tools,
            vec![RunnerRequiredTool::Homeboy]
        );
        assert_eq!(capability_plan.command, "homeboy upgrade");
    }

    #[test]
    fn reports_runner_upgrade_failure_without_stopping_other_runners() {
        let runners = vec![ssh_runner("lab", None), ssh_runner("bench", None)];
        let mut calls = HashMap::<String, usize>::new();
        let (updated, skipped) = upgrade_runners_with_executor(
            &runners,
            &[],
            |runner_id, options| {
                let count = calls.entry(runner_id.to_string()).or_default();
                *count += 1;
                match (runner_id, *count) {
                    ("lab", 1) => Ok((
                        exec_output(runner_id, options.command, "homeboy 0.199.1\n", "", 0),
                        0,
                    )),
                    ("lab", 2) => Ok((
                        exec_output(runner_id, options.command, "", "download failed", 1),
                        1,
                    )),
                    ("bench", 1) => Ok((
                        exec_output(runner_id, options.command, "homeboy 0.199.1\n", "", 0),
                        0,
                    )),
                    ("bench", 2) => Ok((
                        exec_output(runner_id, options.command, "already latest", "", 0),
                        0,
                    )),
                    ("bench", 3) => Ok((
                        exec_output(runner_id, options.command, "homeboy 0.199.1\n", "", 0),
                        0,
                    )),
                    _ => panic!("unexpected call {runner_id} {count}"),
                }
            },
            runner_status,
        );

        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].runner_id, "bench");
        assert!(!updated[0].upgraded);
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].runner_id, "lab");
        assert!(!skipped[0].success);
        assert_eq!(skipped[0].exit_code, 1);
        assert!(skipped[0].detail.contains("download failed"));
    }

    #[test]
    fn syncs_extension_revisions_after_runner_upgrade() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        let extension_updates = vec![ExtensionUpgradeEntry {
            extension_id: "wordpress".to_string(),
            old_version: "2.116.4".to_string(),
            new_version: "2.117.2".to_string(),
            linked: true,
            source_path: Some("/Users/chubes/Developer/homeboy-extensions/wordpress".to_string()),
            git_root: Some("/Users/chubes/Developer/homeboy-extensions".to_string()),
            source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
            source_revision: Some("48517ac3".to_string()),
            source_update: Default::default(),
        }];
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            &extension_updates,
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.228.4\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.228.5\n",
                    4 => "{\"success\":true}\n",
                    5 => "{\"success\":true}\n",
                    6 => "homeboy 0.228.5\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(
            commands[4],
            vec![
                "/home/chubes/.cargo/bin/homeboy",
                "extension",
                "install",
                "https://github.com/Extra-Chill/homeboy-extensions.git",
                "--id",
                "wordpress",
                "--ref",
                "48517ac3",
                "--replace",
            ]
        );
    }

    #[test]
    fn installs_missing_runner_extension_without_replace_flag() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        let extension_updates = vec![ExtensionUpgradeEntry {
            extension_id: "swift".to_string(),
            old_version: "2.6.1".to_string(),
            new_version: "2.6.1".to_string(),
            linked: true,
            source_path: Some("/Users/chubes/Developer/homeboy-extensions/swift".to_string()),
            git_root: Some("/Users/chubes/Developer/homeboy-extensions".to_string()),
            source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
            source_revision: Some("98a61eda".to_string()),
            source_update: Default::default(),
        }];
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            &extension_updates,
            |runner_id, options| {
                commands.push(options.command.clone());
                let (stdout, exit_code) = match commands.len() {
                    1 => ("homeboy 0.228.4\n", 0),
                    2 => ("{\"success\":true}\n", 0),
                    3 => ("homeboy 0.228.5\n", 0),
                    4 => (
                        "{\"success\":false,\"error\":{\"code\":\"extension.not_found\"}}\n",
                        4,
                    ),
                    5 => ("{\"success\":true}\n", 0),
                    6 => ("homeboy 0.228.5\n", 0),
                    _ => ("", 0),
                };
                Ok((
                    exec_output(runner_id, options.command, stdout, "", exit_code),
                    exit_code,
                ))
            },
            runner_status,
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(
            commands[4],
            vec![
                "/home/chubes/.cargo/bin/homeboy",
                "extension",
                "install",
                "https://github.com/Extra-Chill/homeboy-extensions.git",
                "--id",
                "swift",
                "--ref",
                "98a61eda",
            ]
        );
    }

    #[test]
    fn isolates_runner_extension_sync_failures_and_continues_later_extensions() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        let extension_updates = vec![
            extension_update("swift", "98a61eda"),
            extension_update("wordpress", "48517ac3"),
        ];
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            &extension_updates,
            |runner_id, options| {
                commands.push(options.command.clone());
                let (stdout, stderr, exit_code) = match commands.len() {
                    1 => ("homeboy 0.228.4\n", "", 0),
                    2 => ("{\"success\":true}\n", "", 0),
                    3 => ("homeboy 0.228.5\n", "", 0),
                    4 => ("{\"success\":true}\n", "", 0),
                    5 => ("", "swift failed", 1),
                    6 => ("{\"success\":true}\n", "", 0),
                    7 => ("{\"success\":true}\n", "", 0),
                    8 => ("homeboy 0.228.5\n", "", 0),
                    _ => ("", "", 0),
                };
                Ok((
                    exec_output(runner_id, options.command, stdout, stderr, exit_code),
                    exit_code,
                ))
            },
            runner_status,
        );

        assert!(updated.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].extensions_failed.len(), 1);
        assert_eq!(skipped[0].extensions_failed[0].extension_id, "swift");
        assert_eq!(skipped[0].extensions_synced.len(), 1);
        assert_eq!(skipped[0].extensions_synced[0].extension_id, "wordpress");
        assert!(skipped[0].detail.contains("swift@98a61eda"));
        assert!(commands
            .iter()
            .any(|command| command.contains(&"wordpress".to_string())));
    }

    #[test]
    fn reports_configured_path_and_bare_homeboy_drift() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.228.4\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.228.5\n",
                    4 => "homeboy 0.228.4\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.228.4"));
        assert!(updated[0]
            .path_drift
            .as_deref()
            .unwrap()
            .contains("bare `homeboy` reports 0.228.4"));
        assert!(updated[0].detail.contains("runner PATH drift detected"));
    }

    #[test]
    fn reports_stale_connected_daemon_after_runner_upgrade() {
        let runner = ssh_runner("lab", None);

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            &[],
            |runner_id, options| {
                let stdout = match options.command.as_slice() {
                    [_, flag] if flag == "--version" => "homeboy 0.228.5\n",
                    _ => "{\"success\":true}\n",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            stale_runner_status,
        );

        assert!(updated.is_empty());
        assert_eq!(skipped.len(), 1);
        assert!(skipped[0].stale_daemon.is_some());
        assert!(skipped[0]
            .detail
            .contains("connected runner daemon is stale"));
    }

    fn extension_update(extension_id: &str, source_revision: &str) -> ExtensionUpgradeEntry {
        ExtensionUpgradeEntry {
            extension_id: extension_id.to_string(),
            old_version: "1.0.0".to_string(),
            new_version: "1.0.0".to_string(),
            linked: true,
            source_path: Some(format!(
                "/Users/chubes/Developer/homeboy-extensions/{extension_id}"
            )),
            git_root: Some("/Users/chubes/Developer/homeboy-extensions".to_string()),
            source_url: Some("https://github.com/Extra-Chill/homeboy-extensions.git".to_string()),
            source_revision: Some(source_revision.to_string()),
            source_update: Default::default(),
        }
    }

    fn runner_status(runner_id: &str) -> Result<RunnerStatusReport> {
        Ok(RunnerStatusReport {
            runner_id: runner_id.to_string(),
            connected: false,
            state: RunnerSessionState::Disconnected,
            session: None,
            stale_daemon: None,
            session_path: "/tmp/homeboy-runner-session.json".to_string(),
        })
    }

    fn stale_runner_status(runner_id: &str) -> Result<RunnerStatusReport> {
        Ok(RunnerStatusReport {
            runner_id: runner_id.to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: None,
            stale_daemon: Some(RunnerStaleDaemonWarning::new(
                runner_id,
                "0.228.4".to_string(),
                "0.228.5".to_string(),
            )),
            session_path: "/tmp/homeboy-runner-session.json".to_string(),
        })
    }

    fn ssh_runner(id: &str, homeboy_path: Option<&str>) -> Runner {
        Runner {
            id: id.to_string(),
            kind: RunnerKind::Ssh,
            server_id: Some(format!("{id}-server")),
            workspace_root: Some("/home/chubes/workspace".to_string()),
            settings: RunnerSettings {
                homeboy_path: homeboy_path.map(str::to_string),
                ..Default::default()
            },
            env: HashMap::new(),
            resources: HashMap::new(),
            policy: Default::default(),
        }
    }

    fn exec_output(
        runner_id: &str,
        argv: Vec<String>,
        stdout: &str,
        stderr: &str,
        exit_code: i32,
    ) -> RunnerExecOutput {
        RunnerExecOutput {
            command: "runner.exec",
            runner_id: runner_id.to_string(),
            mode: RunnerExecMode::DiagnosticSsh,
            argv,
            remote_cwd: "/home/chubes/workspace".to_string(),
            exit_code,
            stdout: stdout.to_string(),
            stderr: stderr.to_string(),
            source_snapshot: None,
            job: None,
            job_id: None,
            job_events: None,
            mirror_run_id: None,
            patch: None,
            metrics: None,
            capture: None,
            diagnostics: None,
        }
    }
}
