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
    // Classify first: unsupported, unrefreshable, and already-current
    // extensions never cost an install job.
    let mut pending: Vec<(&ExtensionUpgradeEntry, &str, &str)> = Vec::new();
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
        match installed.as_ref().expect("sync candidate has inventory") {
            Ok(rows) => {
                let already_current = rows.iter().any(|row| {
                    row["id"].as_str() == Some(&extension.extension_id)
                        && row["source_revision"].as_str().is_some_and(|revision| {
                            !revision.is_empty() && source_revision.starts_with(revision)
                        })
                });
                if already_current {
                    synced.push(synced_entry(runner, extension, source_revision, true));
                } else {
                    pending.push((extension, source_url, source_revision));
                }
            }
            Err(detail) => failed.push(failed_entry(
                runner,
                homeboy_path,
                extension,
                source_url,
                source_revision,
                detail,
            )),
        }
    }

    // Changed extensions sharing one source and revision install from a
    // single checkout in one runner job instead of one job (and one fetch)
    // apiece. A batch failure — including a runner that predates repeated
    // `--id` — falls back to per-extension installs so failures stay exact.
    let mut groups: Vec<(&str, &str, Vec<&ExtensionUpgradeEntry>)> = Vec::new();
    for (extension, source_url, source_revision) in pending {
        match groups
            .iter_mut()
            .find(|(url, revision, _)| *url == source_url && *revision == source_revision)
        {
            Some((_, _, members)) => members.push(extension),
            None => groups.push((source_url, source_revision, vec![extension])),
        }
    }
    for (source_url, source_revision, members) in groups {
        if members.len() > 1 {
            let ids: Vec<&str> = members
                .iter()
                .map(|extension| extension.extension_id.as_str())
                .collect();
            progress::begin_item(format!("install {}", ids.join(", ")));
            let batch = crate::extension_materialization::run_materialization_command(
                runner,
                None,
                runner_extension_install_recovery_command(
                    homeboy_path,
                    source_url,
                    &ids,
                    source_revision,
                ),
                exec,
            );
            if batch.is_ok() {
                synced.extend(
                    members
                        .iter()
                        .map(|extension| synced_entry(runner, extension, source_revision, false)),
                );
                continue;
            }
        }
        for extension in members {
            let id = extension.extension_id.as_str();
            match crate::extension_materialization::run_materialization_command(
                runner,
                None,
                runner_extension_install_recovery_command(
                    homeboy_path,
                    source_url,
                    &[id],
                    source_revision,
                ),
                exec,
            ) {
                Ok(()) => synced.push(synced_entry(runner, extension, source_revision, false)),
                Err(err) => failed.push(failed_entry(
                    runner,
                    homeboy_path,
                    extension,
                    source_url,
                    source_revision,
                    &err.message,
                )),
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

fn synced_entry(
    runner: &Runner,
    extension: &ExtensionUpgradeEntry,
    source_revision: &str,
    already_current: bool,
) -> RunnerExtensionSyncEntry {
    homeboy_core::log_status!(
        "upgrade",
        "  {} extension {} {} at {}",
        runner.id,
        extension.extension_id,
        if already_current {
            "unchanged"
        } else {
            "synced"
        },
        source_revision.chars().take(7).collect::<String>()
    );
    RunnerExtensionSyncEntry {
        extension_id: extension.extension_id.clone(),
        source_revision: source_revision.to_string(),
        synced: true,
        detail: already_current.then(|| "already at revision".to_string()),
        recovery_commands: Vec::new(),
    }
}

fn failed_entry(
    runner: &Runner,
    homeboy_path: &str,
    extension: &ExtensionUpgradeEntry,
    source_url: &str,
    source_revision: &str,
    err: &str,
) -> RunnerExtensionSyncEntry {
    RunnerExtensionSyncEntry {
        extension_id: extension.extension_id.clone(),
        source_revision: source_revision.to_string(),
        synced: false,
        detail: Some(format!("sync failed: {err}")),
        recovery_commands: runner_exec_recovery_commands(
            runner,
            &runner_extension_install_recovery_command(
                homeboy_path,
                source_url,
                &[extension.extension_id.as_str()],
                source_revision,
            ),
        ),
    }
}

/// `extension install --replace` for one or more extensions of one source at
/// one revision. Repeated `--id` installs them all from a single checkout.
fn runner_extension_install_recovery_command(
    homeboy_path: &str,
    source_url: &str,
    extension_ids: &[&str],
    source_revision: &str,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "install".to_string(),
        source_url.to_string(),
    ];
    for extension_id in extension_ids {
        command.extend(["--id".to_string(), extension_id.to_string()]);
    }
    command.extend([
        "--ref".to_string(),
        source_revision.to_string(),
        "--replace".to_string(),
    ]);
    command
}
