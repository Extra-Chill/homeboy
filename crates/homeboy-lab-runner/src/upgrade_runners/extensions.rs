use super::*;
use crate as runner;
use crate::execution::{RunnerExtensionRevisionOutcome, RunnerExtensionRevisionTarget};
use crate::extension_materialization::runner_extension_git_install_command;
use crate::Runner;
use crate::RunnerExecOptions;
use homeboy_core::Result;
use homeboy_upgrade::upgrade::ExtensionUpgradeEntry;
use homeboy_upgrade::upgrade::RunnerExtensionSyncEntry;

/// Build the desired extension set from the upgrade entries, hand it to the
/// extension-parity owner, and map its outcomes onto upgrade reporting.
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
        progress::begin_item(sync_progress_item(&options.command));
        exec(runner_id, convergence_admission(options))
    };

    // Unsupported and unrefreshable extensions never reach the parity owner.
    let mut desired: Vec<(&ExtensionUpgradeEntry, RunnerExtensionRevisionTarget<'_>)> = Vec::new();
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
        desired.push((
            extension,
            RunnerExtensionRevisionTarget {
                id: &extension.extension_id,
                source_url,
                source_revision,
            },
        ));
    }

    let targets: Vec<_> = desired.iter().map(|(_, target)| *target).collect();
    let outcomes =
        crate::execution::ensure_runner_extension_revisions(runner, homeboy_path, &targets, exec);
    for ((extension, target), outcome) in desired.into_iter().zip(outcomes) {
        match outcome {
            RunnerExtensionRevisionOutcome::AlreadyCurrent => synced.push(synced_entry(
                runner,
                extension,
                target.source_revision,
                true,
            )),
            RunnerExtensionRevisionOutcome::Installed => synced.push(synced_entry(
                runner,
                extension,
                target.source_revision,
                false,
            )),
            RunnerExtensionRevisionOutcome::Failed { detail } => {
                failed.push(failed_entry(runner, homeboy_path, &target, &detail))
            }
        }
    }

    (synced, skipped, failed)
}

/// Name the runner job extension sync is waiting on: the inventory probe or
/// the extensions one install job replaces.
fn sync_progress_item(command: &[String]) -> String {
    if command.get(2).map(String::as_str) != Some("install") {
        return "inventory".to_string();
    }
    let ids: Vec<&str> = command
        .windows(2)
        .filter(|pair| pair[0] == "--id")
        .map(|pair| pair[1].as_str())
        .collect();
    format!("install {}", ids.join(", "))
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
    target: &RunnerExtensionRevisionTarget<'_>,
    err: &str,
) -> RunnerExtensionSyncEntry {
    RunnerExtensionSyncEntry {
        extension_id: target.id.to_string(),
        source_revision: target.source_revision.to_string(),
        synced: false,
        detail: Some(format!("sync failed: {err}")),
        recovery_commands: runner_exec_recovery_commands(
            runner,
            &runner_extension_git_install_command(
                homeboy_path,
                target.source_url,
                &[target.id],
                target.source_revision,
            ),
        ),
    }
}
