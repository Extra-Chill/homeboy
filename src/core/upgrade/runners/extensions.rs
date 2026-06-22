use super::super::types::RunnerExtensionSyncEntry;
use super::*;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerExecOptions;
use crate::core::upgrade::ExtensionUpgradeEntry;
use crate::core::Result;

pub fn sync_runner_extensions(
    runner: &Runner,
    homeboy_path: &str,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> (
    Vec<RunnerExtensionSyncEntry>,
    Vec<RunnerExtensionSyncEntry>,
    Vec<RunnerExtensionSyncEntry>,
) {
    let mut synced = Vec::new();
    let mut skipped = Vec::new();
    let mut failed = Vec::new();
    for extension in extension_updates {
        let Some(source_url) = extension.source_url.as_deref() else {
            continue;
        };
        let Some(source_revision) = extension.source_revision.as_deref() else {
            continue;
        };
        if !runner_supports_extension_sync(runner, &extension.extension_id) {
            skipped.push(RunnerExtensionSyncEntry {
                extension_id: extension.extension_id.clone(),
                source_revision: source_revision.to_string(),
                synced: false,
                detail: Some("skipped by runner supported_extensions policy".to_string()),
                recovery_commands: Vec::new(),
            });
            continue;
        }

        let exists =
            match runner_extension_exists(runner, homeboy_path, &extension.extension_id, exec) {
                Ok(exists) => exists,
                Err(detail) => {
                    failed.push(RunnerExtensionSyncEntry {
                        extension_id: extension.extension_id.clone(),
                        source_revision: source_revision.to_string(),
                        synced: false,
                        detail: Some(detail),
                        recovery_commands: runner_extension_recovery_commands(runner, homeboy_path),
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
                    recovery_commands: Vec::new(),
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
                    recovery_commands: runner_exec_recovery_commands(runner, &command),
                });
            }
            Err(err) => {
                failed.push(RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.to_string(),
                    synced: false,
                    detail: Some(format!("sync failed: {}", err.message)),
                    recovery_commands: runner_exec_recovery_commands(runner, &command),
                });
            }
        }
    }

    (synced, skipped, failed)
}

pub fn runner_supports_extension_sync(runner: &Runner, extension_id: &str) -> bool {
    runner.policy.supported_extensions.is_empty()
        || runner
            .policy
            .supported_extensions
            .iter()
            .any(|supported| supported == extension_id)
}

pub fn defer_extension_failures_for_path_drift(
    path_drift: Option<&str>,
    skipped: &mut Vec<RunnerExtensionSyncEntry>,
    failed: &mut Vec<RunnerExtensionSyncEntry>,
) {
    let Some(path_drift) = path_drift else {
        return;
    };
    if failed.is_empty() {
        return;
    }

    skipped.extend(failed.drain(..).map(|mut entry| {
        let original_detail = entry
            .detail
            .take()
            .unwrap_or_else(|| "no failure detail".to_string());
        entry.detail = Some(format!(
            "deferred because runner executable drift was detected after binary refresh: {path_drift}; original failure: {original_detail}"
        ));
        entry
    }));
}

pub fn runner_extension_exists(
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

pub fn runner_extension_recovery_commands(runner: &Runner, homeboy_path: &str) -> Vec<String> {
    let command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "list".to_string(),
    ];
    runner_exec_recovery_commands(runner, &command)
}
