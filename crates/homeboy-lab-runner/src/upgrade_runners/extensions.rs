use super::*;
use crate as runner;
use crate::Runner;
use crate::RunnerExecOptions;
use homeboy_core::Result;
use homeboy_upgrade::upgrade::ExtensionUpgradeEntry;
use homeboy_upgrade::upgrade::RunnerExtensionSyncEntry;

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
    let installed = extension_updates
        .iter()
        .any(|extension| {
            extension.source_url.is_some()
                && extension.source_revision.is_some()
                && runner_supports_extension_sync(runner, &extension.extension_id)
        })
        .then(|| {
            let (output, code) = exec(
                &runner.id,
                RunnerExecOptions::raw_command(vec![
                    homeboy_path.to_string(),
                    "extension".to_string(),
                    "list".to_string(),
                ])
                .with_env(runner.env.clone()),
            )
            .map_err(|err| err.message)?;
            if code != 0 {
                return Err(format!(
                    "runner extension inventory failed ({code}): {}",
                    output.stderr
                ));
            }
            let value: serde_json::Value = serde_json::from_str(&output.stdout)
                .map_err(|err| format!("invalid runner extension inventory: {err}"))?;
            if value["success"] != true {
                return Err("runner extension inventory did not report success".to_string());
            }
            value
                .pointer("/data/entities")
                .and_then(|rows| rows.as_array())
                .cloned()
                .ok_or_else(|| "runner extension inventory omitted data.entities".to_string())
        });
    for extension in extension_updates {
        let (Some(source_url), Some(source_revision)) = (
            extension.source_url.as_deref(),
            extension.source_revision.as_deref(),
        ) else {
            skipped.push(RunnerExtensionSyncEntry {
                extension_id: extension.extension_id.clone(),
                source_revision: extension.source_revision.clone().unwrap_or_default(),
                synced: false,
                detail: Some(extension.source_update.update_note.clone().unwrap_or_else(|| {
                    "unrefreshable: installed extension lacks reproducible source URL or revision"
                        .to_string()
                })),
                recovery_commands: Vec::new(),
            });
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

        let result = match installed.as_ref().expect("sync candidate has inventory") {
            Ok(rows) => {
                let already_current = rows.iter().any(|row| {
                    row["id"].as_str() == Some(&extension.extension_id)
                        && row["source_revision"].as_str().is_some_and(|revision| {
                            !revision.is_empty() && source_revision.starts_with(revision)
                        })
                });
                if already_current {
                    Ok(true)
                } else {
                    crate::extension_materialization::run_materialization_command(
                        runner,
                        None,
                        runner_extension_install_recovery_command(
                            homeboy_path,
                            source_url,
                            &extension.extension_id,
                            source_revision,
                        ),
                        exec,
                    )
                    .map(|()| false)
                    .map_err(|err| err.message)
                }
            }
            Err(detail) => Err(detail.clone()),
        };
        match result {
            Ok(already_current) => {
                homeboy_core::log_status!(
                    "upgrade",
                    "  {} extension {} synced at {}",
                    runner.id,
                    extension.extension_id,
                    source_revision
                );
                synced.push(RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.to_string(),
                    synced: true,
                    detail: already_current.then(|| "already at revision".to_string()),
                    recovery_commands: Vec::new(),
                });
            }
            Err(err) => {
                failed.push(RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.to_string(),
                    synced: false,
                    detail: Some(format!("sync failed: {err}")),
                    recovery_commands: runner_exec_recovery_commands(
                        runner,
                        &runner_extension_install_recovery_command(
                            homeboy_path,
                            source_url,
                            &extension.extension_id,
                            source_revision,
                        ),
                    ),
                });
            }
        }
    }

    (synced, skipped, failed)
}

pub(crate) fn runner_supports_extension_sync(runner: &Runner, extension_id: &str) -> bool {
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

pub fn defer_runner_extensions_for_binary_drift(
    extension_updates: &[ExtensionUpgradeEntry],
    path_drift: &str,
) -> Vec<RunnerExtensionSyncEntry> {
    extension_updates
        .iter()
        .map(|extension| {
            let Some(source_revision) = extension.source_revision.as_ref() else {
                return RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: String::new(),
                    synced: false,
                    detail: Some("unrefreshable: installed extension lacks reproducible source URL or revision".to_string()),
                    recovery_commands: Vec::new(),
                };
            };
            if extension.source_url.is_none() {
                return RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.clone(),
                    synced: false,
                    detail: Some("unrefreshable: installed extension lacks reproducible source URL or revision".to_string()),
                    recovery_commands: Vec::new(),
                };
            }
            RunnerExtensionSyncEntry {
                extension_id: extension.extension_id.clone(),
                source_revision: source_revision.clone(),
                synced: false,
                detail: Some(format!(
                    "deferred because runner executable did not converge: {path_drift}"
                )),
                recovery_commands: Vec::new(),
            }
        })
        .collect()
}

fn runner_extension_install_recovery_command(
    homeboy_path: &str,
    source_url: &str,
    extension_id: &str,
    source_revision: &str,
) -> Vec<String> {
    vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "install".to_string(),
        source_url.to_string(),
        "--id".to_string(),
        extension_id.to_string(),
        "--ref".to_string(),
        source_revision.to_string(),
        "--replace".to_string(),
    ]
}
