use super::*;
use crate as runner;
use crate::materialize_runner_extension_with_exec;
use crate::Runner;
use crate::RunnerExecOptions;
use crate::{RunnerExtensionMaterializationRequest, RunnerExtensionMaterializationSource};
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

        let request = RunnerExtensionMaterializationRequest {
            id: extension.extension_id.clone(),
            revision: source_revision.to_string(),
            source: RunnerExtensionMaterializationSource::RemoteGit {
                url: source_url.to_string(),
                git_ref: source_revision.to_string(),
            },
        };
        let result =
            materialize_runner_extension_with_exec(runner, homeboy_path, None, &request, exec);
        match result {
            Ok(_provenance) => {
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
                    detail: None,
                    recovery_commands: Vec::new(),
                });
            }
            Err(err) => {
                failed.push(RunnerExtensionSyncEntry {
                    extension_id: extension.extension_id.clone(),
                    source_revision: source_revision.to_string(),
                    synced: false,
                    detail: Some(format!("sync failed: {}", err.message)),
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
