use regex::Regex;

use crate::core::runner::{
    self, Runner, RunnerCapabilityPreflight, RunnerExecOptions, RunnerKind, RunnerRequiredTool,
};
use crate::core::upgrade::{ExtensionUpgradeEntry, RunnerUpgradeEntry};
use crate::core::Result;

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
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for runner in runners {
        let entry = upgrade_runner_with_executor(runner, extension_updates, &mut exec);
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
                exit_code: 1,
                detail: err.message,
            };
        }
    };

    let new_version = runner_homeboy_version(runner, &homeboy_path, exec)
        .ok()
        .flatten();
    if let Err(detail) = sync_runner_extensions(runner, &homeboy_path, extension_updates, exec) {
        return RunnerUpgradeEntry {
            runner_id: runner.id.clone(),
            homeboy_path,
            success: false,
            upgraded: false,
            previous_version,
            new_version,
            exit_code: 1,
            detail,
        };
    }
    let upgraded = match (previous_version.as_deref(), new_version.as_deref()) {
        (Some(previous), Some(new)) => new != previous,
        _ => false,
    };

    RunnerUpgradeEntry {
        runner_id: runner.id.clone(),
        homeboy_path,
        success: new_version.is_some(),
        upgraded,
        previous_version,
        new_version,
        exit_code,
        detail,
    }
}

fn sync_runner_extensions(
    runner: &Runner,
    homeboy_path: &str,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> std::result::Result<(), String> {
    for extension in extension_updates {
        let Some(source_url) = extension.source_url.as_deref() else {
            continue;
        };
        let Some(source_revision) = extension.source_revision.as_deref() else {
            continue;
        };

        let command = vec![
            homeboy_path.to_string(),
            "extension".to_string(),
            "install".to_string(),
            source_url.to_string(),
            "--id".to_string(),
            extension.extension_id.clone(),
            "--ref".to_string(),
            source_revision.to_string(),
            "--replace".to_string(),
        ];

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
            }
            Ok((output, exit_code)) => {
                return Err(format!(
                    "extension {} sync failed with exit code {}: {}",
                    extension.extension_id,
                    exit_code,
                    runner_upgrade_detail(&output)
                ));
            }
            Err(err) => {
                return Err(format!(
                    "extension {} sync failed: {}",
                    extension.extension_id, err.message
                ));
            }
        }
    }

    Ok(())
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
    use crate::core::runner::{RunnerExecMode, RunnerExecOutput};
    use crate::core::server::RunnerSettings;
    use std::collections::HashMap;

    #[test]
    fn upgrades_configured_runner_with_homeboy_path_and_skip_runner_guard() {
        let runner = ssh_runner("lab", Some("/home/chubes/.local/bin/homeboy"));
        let mut commands = Vec::new();
        let (updated, skipped) =
            upgrade_runners_with_executor(&[runner], &[], |runner_id, options| {
                commands.push((
                    runner_id.to_string(),
                    options.command.clone(),
                    options.allow_diagnostic_ssh,
                ));
                let stdout = match commands.len() {
                    1 => "homeboy 0.199.1\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.199.2\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            });

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
        let (updated, skipped) =
            upgrade_runners_with_executor(&runners, &[], |runner_id, options| {
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
            });

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

        let (updated, skipped) =
            upgrade_runners_with_executor(&[runner], &extension_updates, |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.228.4\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.228.5\n",
                    4 => "{\"success\":true}\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            });

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert_eq!(
            commands[3],
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
