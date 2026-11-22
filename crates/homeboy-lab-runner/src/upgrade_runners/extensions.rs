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
    // Extension sync finishes converging the runner the upgrade just
    // refreshed, so it is admitted as convergence like that refresh. As bulk
    // workload it queued behind the saturated Lab's gates for minutes per
    // job while the upgrade reported only that it was refreshing runners.
    let exec = &mut |runner_id: &str, options: RunnerExecOptions| {
        exec(runner_id, convergence_admission(options))
    };
    let total = extension_updates.len();
    let installed = extension_updates
        .iter()
        .any(|extension| {
            extension.source_url.is_some()
                && extension.source_revision.is_some()
                && runner_supports_extension_sync(runner, &extension.extension_id)
        })
        .then(|| {
            progress::begin_item("inventory".to_string());
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
    for (index, extension) in extension_updates.iter().enumerate() {
        progress::begin_item(format!(
            "{} ({}/{total})",
            extension.extension_id,
            index + 1
        ));
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
                    source_revision.chars().take(7).collect::<String>()
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

/// Admit a runner job outside bulk pressure admission, as the daemon does for
/// a Homeboy refresh (#18376). Process capacity is still reserved.
pub(crate) fn convergence_admission(mut options: RunnerExecOptions) -> RunnerExecOptions {
    options.env.insert(
        homeboy_core::runner_pressure::ADMISSION_CLASS_ENV.to_string(),
        homeboy_core::runner_pressure::CONVERGENCE_ADMISSION_CLASS.to_string(),
    );
    options
}

pub(crate) fn runner_supports_extension_sync(runner: &Runner, extension_id: &str) -> bool {
    runner.policy.supported_extensions.is_empty()
        || runner
            .policy
            .supported_extensions
            .iter()
            .any(|supported| supported == extension_id)
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
