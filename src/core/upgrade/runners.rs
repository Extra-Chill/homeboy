use regex::Regex;

use crate::core::build_identity;
use crate::core::runner::{
    self, Runner, RunnerCapabilityPreflight, RunnerExecOptions, RunnerKind, RunnerRequiredTool,
    RunnerStatusReport, RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOptions,
};
use crate::core::upgrade::ExtensionUpgradeEntry;
use crate::core::Result;
use std::path::Path;

use super::helpers::{current_version, version_is_newer};
use super::types::{
    InstallMethod, RunnerDaemonDriftEntry, RunnerExtensionSyncEntry, RunnerUpgradeEntry,
};

pub(super) fn upgrade_configured_runners(
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
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
        force,
        method_override,
        source_path,
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
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    upgrade_runners_with_executor_and_source_materializer(
        runners,
        force,
        method_override,
        source_path,
        extension_updates,
        &mut exec,
        status,
        materialize_runner_source_path,
    )
}

fn upgrade_runners_with_executor_and_source_materializer(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    mut materialize_source_path: impl FnMut(&Runner, &Path) -> Result<String>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    upgrade_runners_with_executor_source_materializer_and_path_updater(
        runners,
        force,
        method_override,
        source_path,
        extension_updates,
        &mut exec,
        status,
        &mut materialize_source_path,
        update_runner_homeboy_path,
    )
}

fn upgrade_runners_with_executor_source_materializer_and_path_updater(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    mut materialize_source_path: impl FnMut(&Runner, &Path) -> Result<String>,
    mut update_homeboy_path: impl FnMut(&str, &str) -> Result<()>,
) -> (Vec<RunnerUpgradeEntry>, Vec<RunnerUpgradeEntry>) {
    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for runner in runners {
        let entry = upgrade_runner_with_executor(
            runner,
            force,
            method_override,
            source_path,
            extension_updates,
            &mut exec,
            &status,
            &mut materialize_source_path,
            &mut update_homeboy_path,
        );
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
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: &impl Fn(&str) -> Result<RunnerStatusReport>,
    materialize_source_path: &mut impl FnMut(&Runner, &Path) -> Result<String>,
    update_homeboy_path: &mut impl FnMut(&str, &str) -> Result<()>,
) -> RunnerUpgradeEntry {
    let original_homeboy_path = runner
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    let previous_version = runner_homeboy_version(runner, &original_homeboy_path, exec)
        .ok()
        .flatten();
    let command_source_path = match runner_upgrade_source_path(
        runner,
        method_override,
        source_path,
        materialize_source_path,
    ) {
        Ok(path) => path,
        Err(err) => {
            return runner_upgrade_failure_entry(
                &runner.id,
                original_homeboy_path,
                previous_version,
                1,
                err.message,
            );
        }
    };
    let mut upgrade_homeboy_path = original_homeboy_path.clone();
    let mut path_update_detail = None;
    let upgrade = exec(
        &runner.id,
        runner_exec_options(
            runner,
            runner_upgrade_command(
                &upgrade_homeboy_path,
                force,
                method_override,
                command_source_path.as_deref(),
            ),
        ),
    );

    // Both the non-zero-exit and hard-error upgrade arms attempt the same
    // recovery: realign the runner homeboy_path, then retry the upgrade.
    let recovery_outcome = match upgrade {
        Ok((output, exit_code)) if exit_code == 0 => {
            Ok((exit_code, runner_upgrade_detail(&output), None))
        }
        Ok((output, exit_code)) => recover_and_retry_failed_upgrade(
            runner,
            force,
            method_override,
            command_source_path.as_deref(),
            &original_homeboy_path,
            previous_version.as_deref(),
            FailedUpgradeOutcome {
                exit_code,
                detail: runner_upgrade_detail(&output),
            },
            update_homeboy_path,
            exec,
        ),
        Err(err) => recover_and_retry_failed_upgrade(
            runner,
            force,
            method_override,
            command_source_path.as_deref(),
            &original_homeboy_path,
            previous_version.as_deref(),
            FailedUpgradeOutcome {
                exit_code: 1,
                detail: err.message,
            },
            update_homeboy_path,
            exec,
        ),
    };

    let (exit_code, detail) = match recovery_outcome {
        Ok((exit_code, detail, recovery)) => {
            if let Some(recovery) = recovery {
                upgrade_homeboy_path = recovery.homeboy_path;
                path_update_detail = Some(recovery.detail);
            }
            (exit_code, detail)
        }
        Err(mut entry) => {
            entry.previous_version = previous_version;
            return entry;
        }
    };

    let mut homeboy_path = upgrade_homeboy_path.clone();
    let mut new_version = runner_homeboy_version(runner, &upgrade_homeboy_path, exec)
        .ok()
        .flatten();
    let configured_identity = runner_homeboy_identity(runner, &upgrade_homeboy_path, exec)
        .ok()
        .flatten();
    let mut source_path_realigned = false;
    if let Some(realignment) = source_upgrade_homeboy_path_realignment(
        runner,
        &original_homeboy_path,
        method_override,
        command_source_path.as_deref(),
        new_version.as_deref(),
        configured_identity.as_deref(),
        exec,
    ) {
        match update_homeboy_path(&runner.id, &realignment.homeboy_path) {
            Ok(()) => {
                homeboy_path = realignment.homeboy_path;
                new_version = Some(realignment.version);
                source_path_realigned = true;
                path_update_detail = Some(realignment.detail);
            }
            Err(err) => {
                path_update_detail = Some(format!(
                    "source-built runner homeboy_path realignment failed: {}",
                    err.message
                ));
            }
        }
    }
    let mut bare_homeboy_version = None;
    let alignment = if !source_path_realigned && is_auto_realignable_homeboy_path(&homeboy_path) {
        bare_homeboy_version = runner_bare_homeboy_version(runner, &upgrade_homeboy_path, exec);
        runner_homeboy_path_alignment(
            &runner.id,
            &homeboy_path,
            new_version.as_deref(),
            bare_homeboy_version.as_deref(),
        )
    } else {
        None
    };
    let mut path_drift = alignment
        .as_ref()
        .and_then(|alignment| alignment.drift.clone());

    if let Some(alignment) = alignment {
        if alignment.update_to.is_none()
            && is_disposable_lab_workspace_homeboy_path(&homeboy_path)
            && matches!(
                (new_version.as_deref(), bare_homeboy_version.as_deref()),
                (Some(configured), Some(bare)) if version_is_newer(configured, bare)
            )
        {
            let repair = repair_stale_bare_homeboy_after_upgrade(
                runner,
                force,
                method_override,
                command_source_path.as_deref(),
                new_version.as_deref().unwrap_or_default(),
                exec,
            );
            bare_homeboy_version = repair.bare_version;
            path_drift = repair.path_drift;
            path_update_detail = Some(repair.detail);
        } else {
            apply_runner_homeboy_path_alignment(
                &runner.id,
                alignment,
                &original_homeboy_path,
                bare_homeboy_version.as_deref(),
                &mut homeboy_path,
                &mut new_version,
                &mut path_drift,
                &mut path_update_detail,
                update_homeboy_path,
            );
        }
    }

    let (extensions_synced, mut extensions_skipped, mut extensions_failed) =
        sync_runner_extensions(runner, &homeboy_path, extension_updates, exec);
    if path_drift.is_some() && is_auto_realignable_homeboy_path(&homeboy_path) {
        let refreshed_bare_homeboy_version =
            runner_bare_homeboy_version(runner, &homeboy_path, exec);
        if refreshed_bare_homeboy_version.is_some() {
            bare_homeboy_version = refreshed_bare_homeboy_version;
            match runner_homeboy_path_alignment(
                &runner.id,
                &homeboy_path,
                new_version.as_deref(),
                bare_homeboy_version.as_deref(),
            ) {
                Some(alignment) => {
                    path_drift = alignment.drift.clone();
                    apply_runner_homeboy_path_alignment(
                        &runner.id,
                        alignment,
                        &original_homeboy_path,
                        bare_homeboy_version.as_deref(),
                        &mut homeboy_path,
                        &mut new_version,
                        &mut path_drift,
                        &mut path_update_detail,
                        update_homeboy_path,
                    );
                }
                None => {
                    path_drift = None;
                }
            }
        }
    }
    if bare_homeboy_version.is_none() && !source_path_realigned {
        bare_homeboy_version = runner_bare_homeboy_version(runner, &homeboy_path, exec);
    }
    if path_drift.is_none() && !source_path_realigned {
        if let Some(alignment) = runner_homeboy_path_alignment(
            &runner.id,
            &homeboy_path,
            new_version.as_deref(),
            bare_homeboy_version.as_deref(),
        ) {
            path_drift = alignment.drift.clone();
            apply_runner_homeboy_path_alignment(
                &runner.id,
                alignment,
                &original_homeboy_path,
                bare_homeboy_version.as_deref(),
                &mut homeboy_path,
                &mut new_version,
                &mut path_drift,
                &mut path_update_detail,
                update_homeboy_path,
            );
        }
    }
    defer_extension_failures_for_path_drift(
        path_drift.as_deref(),
        &mut extensions_skipped,
        &mut extensions_failed,
    );
    let recovery_commands = runner_recovery_commands(
        &runner.id,
        &homeboy_path,
        path_drift.as_ref(),
        new_version.as_deref(),
        bare_homeboy_version.as_deref(),
    );
    let stale_daemon = runner_stale_daemon(runner, status);
    let upgraded = source_path_realigned
        || match (previous_version.as_deref(), new_version.as_deref()) {
            (Some(previous), Some(new)) => new != previous,
            _ => false,
        };
    let success = new_version.is_some()
        && path_drift.is_none()
        && extensions_failed.is_empty()
        && stale_daemon.is_none();
    let detail = runner_upgrade_final_detail(
        &runner.id,
        detail,
        path_update_detail.as_deref(),
        path_drift.as_deref(),
        stale_daemon.as_ref(),
        &extensions_skipped,
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
        recovery_commands,
        extensions_synced,
        extensions_skipped,
        extensions_failed,
        stale_daemon,
        exit_code,
        detail,
    }
}

/// Captures the exit code and detail produced by a failed runner upgrade attempt.
struct FailedUpgradeOutcome {
    exit_code: i32,
    detail: String,
}

/// Builds a non-recoverable runner upgrade failure entry, preserving every
/// `RunnerUpgradeEntry` field at its canonical "failed before completion" value.
///
/// The caller is responsible for assigning `previous_version`, which is left as
/// `None` here so the surrounding flow can move it in once.
fn runner_upgrade_failure_entry(
    runner_id: &str,
    homeboy_path: String,
    previous_version: Option<String>,
    exit_code: i32,
    detail: String,
) -> RunnerUpgradeEntry {
    RunnerUpgradeEntry {
        runner_id: runner_id.to_string(),
        homeboy_path,
        success: false,
        upgraded: false,
        previous_version,
        new_version: None,
        bare_homeboy_version: None,
        path_drift: None,
        recovery_commands: runner_upgrade_recovery_commands(runner_id),
        extensions_synced: Vec::new(),
        extensions_skipped: Vec::new(),
        extensions_failed: Vec::new(),
        stale_daemon: None,
        exit_code,
        detail,
    }
}

/// Recovers a stale runner `homeboy_path` after a failed upgrade and retries the
/// upgrade through the realigned path.
///
/// Returns `Ok((exit_code, detail, recovery))` when the main upgrade flow should
/// continue (either no realignment was needed, or the retry succeeded). Returns
/// `Err(entry)` with a fully-populated failure entry (minus `previous_version`,
/// which the caller assigns) when recovery or the retry definitively fails.
#[allow(clippy::too_many_arguments)]
fn recover_and_retry_failed_upgrade(
    runner: &Runner,
    force: bool,
    method_override: Option<InstallMethod>,
    command_source_path: Option<&str>,
    original_homeboy_path: &str,
    previous_version: Option<&str>,
    failure: FailedUpgradeOutcome,
    update_homeboy_path: &mut impl FnMut(&str, &str) -> Result<()>,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> std::result::Result<(i32, String, Option<FailedUpgradePathRecovery>), RunnerUpgradeEntry> {
    match recover_runner_homeboy_path_after_failed_upgrade(
        runner,
        original_homeboy_path,
        previous_version,
        update_homeboy_path,
        exec,
    ) {
        Ok(Some(recovery)) => {
            let realigned_detail = recovery.detail.clone();
            let retry = exec(
                &runner.id,
                runner_exec_options(
                    runner,
                    runner_upgrade_command(
                        &recovery.homeboy_path,
                        force,
                        method_override,
                        command_source_path,
                    ),
                ),
            );
            match retry {
                Ok((retry_output, 0)) => {
                    Ok((0, runner_upgrade_detail(&retry_output), Some(recovery)))
                }
                Ok((retry_output, retry_exit_code)) => Err(runner_upgrade_retry_failure_entry(
                    &runner.id,
                    recovery.homeboy_path,
                    recovery.bare_version,
                    retry_exit_code,
                    &realigned_detail,
                    runner_upgrade_detail(&retry_output),
                )),
                Err(err) => Err(runner_upgrade_retry_failure_entry(
                    &runner.id,
                    recovery.homeboy_path,
                    recovery.bare_version,
                    1,
                    &realigned_detail,
                    err.message,
                )),
            }
        }
        Ok(None) => Err(runner_upgrade_failure_entry(
            &runner.id,
            original_homeboy_path.to_string(),
            None,
            failure.exit_code,
            failure.detail,
        )),
        Err(recovery_detail) => {
            let mut entry = runner_upgrade_failure_entry(
                &runner.id,
                original_homeboy_path.to_string(),
                None,
                failure.exit_code,
                format!(
                    "{}\nrunner homeboy_path recovery failed: {}",
                    failure.detail, recovery_detail
                ),
            );
            entry.path_drift = Some(recovery_detail.clone());
            entry.recovery_commands =
                runner_recovery_commands(&runner.id, "homeboy", Some(&recovery_detail), None, None);
            Err(entry)
        }
    }
}

/// Builds the failure entry emitted when the post-realignment upgrade retry
/// itself fails, preserving the realigned `homeboy_path` and bare version.
fn runner_upgrade_retry_failure_entry(
    runner_id: &str,
    homeboy_path: String,
    bare_version: Option<String>,
    exit_code: i32,
    realigned_detail: &str,
    failure_detail: String,
) -> RunnerUpgradeEntry {
    let mut entry =
        runner_upgrade_failure_entry(runner_id, homeboy_path, None, exit_code, String::new());
    entry.bare_homeboy_version = bare_version;
    entry.detail = format!(
        "{}; retry after runner homeboy_path realignment failed: {}",
        realigned_detail, failure_detail
    );
    entry
}

/// Applies a runner `homeboy_path` alignment that may update the configured path.
///
/// When the alignment carries an `update_to` target, the runner config is updated
/// and the in-flight upgrade state (`homeboy_path`, `new_version`, `path_drift`,
/// `path_update_detail`) is mutated to reflect the realignment; a failed update is
/// recorded as path drift instead.
#[allow(clippy::too_many_arguments)]
fn apply_runner_homeboy_path_alignment(
    runner_id: &str,
    alignment: RunnerHomeboyPathAlignment,
    original_homeboy_path: &str,
    bare_homeboy_version: Option<&str>,
    homeboy_path: &mut String,
    new_version: &mut Option<String>,
    path_drift: &mut Option<String>,
    path_update_detail: &mut Option<String>,
    update_homeboy_path: &mut impl FnMut(&str, &str) -> Result<()>,
) {
    let Some(new_path) = alignment.update_to.as_deref() else {
        return;
    };
    match update_homeboy_path(runner_id, new_path) {
        Ok(()) => {
            *homeboy_path = new_path.to_string();
            *new_version = bare_homeboy_version.map(str::to_string);
            *path_drift = None;
            *path_update_detail = Some(format!(
                "runner homeboy_path updated from `{}` to `{}` because bare `homeboy` reports {}",
                original_homeboy_path,
                new_path,
                bare_homeboy_version.unwrap_or("an upgraded version")
            ));
        }
        Err(err) => {
            *path_drift = Some(format!(
                "{}; automatic runner homeboy_path update failed: {}",
                alignment.drift.unwrap_or_else(|| {
                    format!(
                        "configured runner executable `{}` is stale",
                        original_homeboy_path
                    )
                }),
                err.message
            ));
        }
    }
}

struct SourceUpgradeHomeboyPathRealignment {
    homeboy_path: String,
    version: String,
    detail: String,
}

fn source_upgrade_homeboy_path_realignment(
    runner: &Runner,
    original_homeboy_path: &str,
    method_override: Option<InstallMethod>,
    command_source_path: Option<&str>,
    configured_version: Option<&str>,
    configured_identity: Option<&str>,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> Option<SourceUpgradeHomeboyPathRealignment> {
    if method_override != Some(InstallMethod::Source) {
        return None;
    }
    let current_identity = build_identity::current().display;
    if configured_version == Some(current_version())
        && configured_identity == Some(current_identity.as_str())
    {
        return None;
    }
    let source_path = command_source_path?.trim_end_matches('/');
    for build_dir in ["release", "debug"] {
        let candidate = format!("{source_path}/target/{build_dir}/homeboy");
        let Some(version) = runner_homeboy_version(runner, &candidate, exec)
            .ok()
            .flatten()
        else {
            continue;
        };
        if version != current_version() {
            continue;
        }
        let Some(identity) = runner_homeboy_identity(runner, &candidate, exec)
            .ok()
            .flatten()
        else {
            continue;
        };
        if identity != current_identity {
            continue;
        }
        return Some(SourceUpgradeHomeboyPathRealignment {
            homeboy_path: candidate.clone(),
            version: version.clone(),
            detail: format!(
                "runner homeboy_path updated from `{original_homeboy_path}` to source-built `{candidate}` because it reports {identity}"
            ),
        });
    }

    None
}

fn sync_runner_extensions(
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

fn runner_supports_extension_sync(runner: &Runner, extension_id: &str) -> bool {
    runner.policy.supported_extensions.is_empty()
        || runner
            .policy
            .supported_extensions
            .iter()
            .any(|supported| supported == extension_id)
}

fn defer_extension_failures_for_path_drift(
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

fn runner_upgrade_command(
    homeboy_path: &str,
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&str>,
) -> Vec<String> {
    let mut command = vec![
        homeboy_path.to_string(),
        "upgrade".to_string(),
        "--no-restart".to_string(),
        "--skip-extensions".to_string(),
        "--skip-runners".to_string(),
    ];

    if force {
        command.push("--force".to_string());
    }

    if let Some(method) = method_override {
        command.push("--method".to_string());
        command.push(method.as_str().to_string());
    }

    if let Some(path) = source_path {
        command.push("--source-path".to_string());
        command.push(path.to_string());
    }

    command
}

fn runner_upgrade_source_path(
    runner: &Runner,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    materialize_source_path: &mut impl FnMut(&Runner, &Path) -> Result<String>,
) -> Result<Option<String>> {
    let Some(source_path) = source_path else {
        return Ok(None);
    };

    if method_override == Some(InstallMethod::Source) && runner.kind == RunnerKind::Ssh {
        return materialize_source_path(runner, source_path).map(Some);
    }

    Ok(Some(source_path.display().to_string()))
}

fn materialize_runner_source_path(runner: &Runner, source_path: &Path) -> Result<String> {
    let (output, _) = runner::sync_workspace(
        &runner.id,
        RunnerWorkspaceSyncOptions {
            path: source_path.display().to_string(),
            mode: RunnerWorkspaceSyncMode::Git,
            controller_routed_git: true,
            changed_since_base: None,
            git_fetch_refs: Vec::new(),
            snapshot_includes: Vec::new(),
            allow_dirty_lab_workspace: false,
        },
    )?;

    Ok(output.remote_path)
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

fn runner_homeboy_identity(
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

    let output = if output.stdout.trim().is_empty() {
        output.stderr.trim()
    } else {
        output.stdout.trim()
    };

    if output.is_empty() {
        Ok(None)
    } else {
        Ok(Some(output.to_string()))
    }
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

struct RunnerHomeboyPathAlignment {
    drift: Option<String>,
    update_to: Option<String>,
}

struct FailedUpgradePathRecovery {
    homeboy_path: String,
    bare_version: Option<String>,
    detail: String,
}

struct StaleBareHomeboyRepair {
    bare_version: Option<String>,
    path_drift: Option<String>,
    detail: String,
}

fn repair_stale_bare_homeboy_after_upgrade(
    runner: &Runner,
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&str>,
    configured_version: &str,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> StaleBareHomeboyRepair {
    let repair_command = runner_upgrade_command("homeboy", force, method_override, source_path);
    let repair_result = exec(
        &runner.id,
        runner_exec_options(runner, repair_command.clone()),
    );
    let repair_detail = match repair_result {
        Ok((output, 0)) => runner_upgrade_detail(&output),
        Ok((output, exit_code)) => {
            let detail = runner_upgrade_detail(&output);
            let bare_version = runner_homeboy_version(runner, "homeboy", exec)
                .ok()
                .flatten();
            return StaleBareHomeboyRepair {
                path_drift: Some(format!(
                    "configured runner executable reports {configured_version}, but managed PATH-visible `homeboy` repair exited {exit_code}: {detail}"
                )),
                bare_version,
                detail: format!(
                    "managed PATH-visible `homeboy` repair failed with `{}`: {detail}",
                    repair_command
                        .iter()
                        .map(|arg| shell_arg(arg))
                        .collect::<Vec<_>>()
                        .join(" ")
                ),
            };
        }
        Err(err) => {
            let bare_version = runner_homeboy_version(runner, "homeboy", exec)
                .ok()
                .flatten();
            return StaleBareHomeboyRepair {
                path_drift: Some(format!(
                    "configured runner executable reports {configured_version}, but managed PATH-visible `homeboy` repair failed: {}",
                    err.message
                )),
                bare_version,
                detail: format!(
                    "managed PATH-visible `homeboy` repair failed with `{}`: {}",
                    repair_command
                        .iter()
                        .map(|arg| shell_arg(arg))
                        .collect::<Vec<_>>()
                        .join(" "),
                    err.message
                ),
            };
        }
    };

    let bare_version = runner_homeboy_version(runner, "homeboy", exec)
        .ok()
        .flatten();
    if bare_version.as_deref() == Some(configured_version) {
        return StaleBareHomeboyRepair {
            bare_version,
            path_drift: None,
            detail: format!(
                "PATH-visible `homeboy` repaired with `{}` and now reports {configured_version}: {repair_detail}",
                repair_command
                    .iter()
                    .map(|arg| shell_arg(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            ),
        };
    }

    let bare_detail = bare_version
        .as_deref()
        .unwrap_or("an unknown version")
        .to_string();
    StaleBareHomeboyRepair {
        path_drift: Some(format!(
            "configured runner executable reports {configured_version}, but managed PATH-visible `homeboy` repair left bare `homeboy` at {bare_detail}"
        )),
        bare_version,
        detail: format!(
            "managed PATH-visible `homeboy` repair completed with `{}` but bare `homeboy` still reports {bare_detail}: {repair_detail}",
            repair_command
                .iter()
                .map(|arg| shell_arg(arg))
                .collect::<Vec<_>>()
                .join(" ")
        ),
    }
}

fn recover_runner_homeboy_path_after_failed_upgrade(
    runner: &Runner,
    homeboy_path: &str,
    configured_version: Option<&str>,
    update_homeboy_path: &mut impl FnMut(&str, &str) -> Result<()>,
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
) -> std::result::Result<Option<FailedUpgradePathRecovery>, String> {
    if !is_auto_realignable_homeboy_path(homeboy_path) {
        return Ok(None);
    }

    let Some(configured_version) = configured_version else {
        return Ok(None);
    };
    let bare_version = runner_bare_homeboy_version(runner, homeboy_path, exec);
    let Some(alignment) = runner_homeboy_path_alignment(
        &runner.id,
        homeboy_path,
        Some(configured_version),
        bare_version.as_deref(),
    ) else {
        return Ok(None);
    };
    let Some(new_path) = alignment.update_to.as_deref() else {
        return Ok(None);
    };

    update_homeboy_path(&runner.id, new_path).map_err(|err| {
        format!(
            "{}; automatic runner homeboy_path update failed: {}",
            alignment.drift.unwrap_or_else(|| {
                format!("configured runner executable `{homeboy_path}` is stale")
            }),
            err.message
        )
    })?;

    let detail = format!(
        "runner homeboy_path updated from `{}` to `{}` after configured runner executable failed to upgrade because bare `homeboy` reports {}",
        homeboy_path,
        new_path,
        bare_version.as_deref().unwrap_or("an upgraded version")
    );

    Ok(Some(FailedUpgradePathRecovery {
        homeboy_path: new_path.to_string(),
        bare_version,
        detail,
    }))
}

fn runner_homeboy_path_alignment(
    runner_id: &str,
    homeboy_path: &str,
    configured_version: Option<&str>,
    bare_version: Option<&str>,
) -> Option<RunnerHomeboyPathAlignment> {
    if homeboy_path == "homeboy" {
        return None;
    }
    let configured_version = configured_version?;
    let bare_version = bare_version?;
    if bare_version == configured_version {
        return None;
    }

    let drift = format!(
        "configured runner executable `{}` reports {}, but bare `homeboy` reports {}",
        homeboy_path, configured_version, bare_version
    );

    if is_auto_realignable_homeboy_path(homeboy_path)
        && version_is_newer(bare_version, configured_version)
    {
        return Some(RunnerHomeboyPathAlignment {
            drift: Some(drift),
            update_to: Some("homeboy".to_string()),
        });
    }

    if version_is_newer(configured_version, bare_version) {
        return Some(RunnerHomeboyPathAlignment {
            drift: Some(format!(
                "{}; bare `homeboy` is older than the configured runner executable, so the runner remains degraded until PATH-visible `homeboy` is upgraded or the shadowing binary is removed. Inspect with `{}`",
                drift,
                runner_inspect_bare_homeboy_command(runner_id)
            )),
            update_to: None,
        });
    }

    Some(RunnerHomeboyPathAlignment {
        drift: Some(format!(
            "{}; automatic runner homeboy_path update is unsafe for this configured path. Remediate with `{}` after verifying bare `homeboy` is the intended runner binary",
            drift,
            runner_set_homeboy_path_command(runner_id, "homeboy")
        )),
        update_to: None,
    })
}

fn is_auto_realignable_homeboy_path(homeboy_path: &str) -> bool {
    is_versioned_homeboy_path(homeboy_path)
        || is_disposable_lab_workspace_homeboy_path(homeboy_path)
}

fn is_disposable_lab_workspace_homeboy_path(homeboy_path: &str) -> bool {
    if !homeboy_path.contains("/_lab_workspaces/") {
        return false;
    }
    let path = Path::new(homeboy_path);
    if path.file_name().and_then(|name| name.to_str()) != Some("homeboy") {
        return false;
    }

    matches!(
        path.parent()
            .and_then(|parent| parent.file_name())
            .and_then(|name| name.to_str()),
        Some("debug" | "release")
    ) && path
        .parent()
        .and_then(|parent| parent.parent())
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
        == Some("target")
}

fn is_versioned_homeboy_path(homeboy_path: &str) -> bool {
    let Some(file_name) = Path::new(homeboy_path)
        .file_name()
        .and_then(|name| name.to_str())
    else {
        return false;
    };
    Regex::new(r"^homeboy-\d+\.\d+\.\d+$")
        .map(|re| re.is_match(file_name))
        .unwrap_or(false)
}

fn update_runner_homeboy_path(runner_id: &str, homeboy_path: &str) -> Result<()> {
    let spec = serde_json::json!({ "homeboy_path": homeboy_path }).to_string();
    runner::merge(Some(runner_id), &spec, &[])?;
    Ok(())
}

fn runner_recovery_commands(
    runner_id: &str,
    homeboy_path: &str,
    path_drift: Option<&String>,
    configured_version: Option<&str>,
    bare_version: Option<&str>,
) -> Vec<String> {
    let mut commands = runner_upgrade_recovery_commands(runner_id);
    if path_drift.is_some() {
        commands.push(runner_inspect_bare_homeboy_command(runner_id));
    }
    if path_drift.is_some()
        && homeboy_path != "homeboy"
        && matches!((configured_version, bare_version), (Some(configured), Some(bare)) if version_is_newer(bare, configured))
    {
        commands.push(runner_set_homeboy_path_command(runner_id, "homeboy"));
    }
    commands
}

fn runner_inspect_bare_homeboy_command(runner_id: &str) -> String {
    let script = "type -a homeboy; command -v homeboy; homeboy --version";
    format!(
        "homeboy runner exec {} --ssh -- sh -lc {}",
        shell_arg(runner_id),
        shell_arg(script)
    )
}

fn runner_set_homeboy_path_command(runner_id: &str, homeboy_path: &str) -> String {
    format!(
        "homeboy runner set {} --json {}",
        shell_arg(runner_id),
        shell_arg(&serde_json::json!({ "homeboy_path": homeboy_path }).to_string())
    )
}

fn runner_upgrade_recovery_commands(runner_id: &str) -> Vec<String> {
    vec![format!(
        "homeboy upgrade --force --upgrade-runner {}",
        shell_arg(runner_id)
    )]
}

fn runner_extension_recovery_commands(runner: &Runner, homeboy_path: &str) -> Vec<String> {
    let command = vec![
        homeboy_path.to_string(),
        "extension".to_string(),
        "list".to_string(),
    ];
    runner_exec_recovery_commands(runner, &command)
}

fn runner_exec_recovery_commands(runner: &Runner, command: &[String]) -> Vec<String> {
    let mut args = vec![
        "homeboy".to_string(),
        "runner".to_string(),
        "exec".to_string(),
        runner.id.clone(),
        "--ssh".to_string(),
        "--".to_string(),
    ];
    args.extend(command.iter().cloned());
    vec![args
        .iter()
        .map(|arg| shell_arg(arg))
        .collect::<Vec<_>>()
        .join(" ")]
}

fn shell_arg(arg: &str) -> String {
    if !arg.is_empty()
        && arg.bytes().all(|byte| {
            byte.is_ascii_alphanumeric()
                || matches!(byte, b'/' | b'.' | b'_' | b'-' | b':' | b'@' | b'=')
        })
    {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "'\\''"))
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
    runner_id: &str,
    detail: String,
    path_update_detail: Option<&str>,
    path_drift: Option<&str>,
    stale_daemon: Option<&RunnerDaemonDriftEntry>,
    extensions_skipped: &[RunnerExtensionSyncEntry],
    extensions_failed: &[RunnerExtensionSyncEntry],
) -> String {
    let mut parts = vec![detail];

    if let Some(path_update_detail) = path_update_detail {
        parts.push(path_update_detail.to_string());
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
            "connected runner daemon is stale: session reports {}, configured executable reports {}; restart the active daemon with `{}`",
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
        secret_env_names: Vec::new(),
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
            false,
            None,
            None,
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
                "--skip-extensions",
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
    fn materializes_forced_source_upgrade_path_before_forwarding_to_runner() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        let source_path = Path::new(
            "/Users/chubes/Developer/homeboy@fix-bench-selected-duplicate-validation-1266",
        );
        let mut commands = Vec::new();
        let mut materialized = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor_and_source_materializer(
            &[runner],
            true,
            Some(InstallMethod::Source),
            Some(source_path),
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.228.13\n",
                    2 => "{\"install_method\":\"source\",\"message\":\"Upgrade command completed but active binary is still 0.228.13\",\"upgraded\":false}\n",
                    3 => "homeboy 0.228.13\n",
                    4 => "homeboy 0.228.13\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
            |runner, path| {
                materialized.push((runner.id.clone(), path.display().to_string()));
                Ok("/home/chubes/Developer/_lab_workspaces/homeboy-source".to_string())
            },
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(updated[0].success);
        assert!(!updated[0].upgraded);
        assert_eq!(
            materialized,
            vec![(
                "lab".to_string(),
                "/Users/chubes/Developer/homeboy@fix-bench-selected-duplicate-validation-1266"
                    .to_string()
            )]
        );
        assert_eq!(
            commands[1],
            vec![
                "/home/chubes/.cargo/bin/homeboy",
                "upgrade",
                "--no-restart",
                "--skip-extensions",
                "--skip-runners",
                "--force",
                "--method",
                "source",
                "--source-path",
                "/home/chubes/Developer/_lab_workspaces/homeboy-source",
            ]
        );
    }

    #[test]
    fn reports_runner_upgrade_failure_without_stopping_other_runners() {
        let runners = vec![ssh_runner("lab", None), ssh_runner("bench", None)];
        let mut calls = HashMap::<String, usize>::new();
        let (updated, skipped) = upgrade_runners_with_executor(
            &runners,
            false,
            None,
            None,
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
            false,
            None,
            None,
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
            false,
            None,
            None,
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
            false,
            None,
            None,
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
        assert_eq!(
            skipped[0].extensions_failed[0].recovery_commands,
            vec!["homeboy runner exec lab --ssh -- /home/chubes/.cargo/bin/homeboy extension install https://github.com/Extra-Chill/homeboy-extensions.git --id swift --ref 98a61eda --replace"]
        );
        assert_eq!(skipped[0].extensions_synced.len(), 1);
        assert_eq!(skipped[0].extensions_synced[0].extension_id, "wordpress");
        assert!(skipped[0].detail.contains("swift@98a61eda"));
        assert!(skipped[0]
            .detail
            .contains("homeboy upgrade --force --upgrade-runner lab"));
        assert!(commands
            .iter()
            .any(|command| command.contains(&"wordpress".to_string())));
    }

    #[test]
    fn skips_runner_extensions_outside_supported_extension_policy() {
        let mut runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        runner.policy.supported_extensions = vec!["wordpress".to_string()];
        let extension_updates = vec![
            extension_update("swift", "98a61eda"),
            extension_update("wordpress", "48517ac3"),
        ];
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            false,
            None,
            None,
            &extension_updates,
            |runner_id, options| {
                assert!(
                    !options.command.contains(&"swift".to_string()),
                    "unsupported Swift extension should not be synced"
                );
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
        assert!(updated[0].success);
        assert_eq!(updated[0].extensions_synced.len(), 1);
        assert_eq!(updated[0].extensions_synced[0].extension_id, "wordpress");
        assert_eq!(updated[0].extensions_skipped.len(), 1);
        assert_eq!(updated[0].extensions_skipped[0].extension_id, "swift");
        assert_eq!(updated[0].extensions_failed.len(), 0);
        assert!(updated[0].extensions_skipped[0]
            .detail
            .as_deref()
            .unwrap()
            .contains("supported_extensions"));
        assert!(updated[0]
            .detail
            .contains("runner extension sync(s) skipped"));
        assert!(commands
            .iter()
            .any(|command| command.contains(&"wordpress".to_string())));
    }

    #[test]
    fn defers_extension_failures_when_runner_refresh_leaves_path_drift() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        let extension_updates = vec![
            extension_update("auxiliary-extension", "98a61eda"),
            extension_update("required-extension", "48517ac3"),
        ];
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            true,
            None,
            None,
            &extension_updates,
            |runner_id, options| {
                commands.push(options.command.clone());
                let (stdout, stderr, exit_code) = match commands.len() {
                    1 => ("homeboy 0.228.18\n", "", 0),
                    2 => (
                        "{\"success\":true,\"message\":\"Upgrade command completed but active binary is still 0.228.18\"}\n",
                        "",
                        0,
                    ),
                    3 => ("homeboy 0.228.18\n", "", 0),
                    4 => ("{\"success\":true}\n", "", 0),
                    5 => ("", "extension setup failed", 1),
                    6 => ("{\"success\":true}\n", "", 0),
                    7 => ("{\"success\":true}\n", "", 0),
                    8 => ("homeboy 0.228.13\n", "", 0),
                    _ => ("", "unexpected runner command", 1),
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
        assert!(!skipped[0].success);
        assert!(!skipped[0].upgraded);
        assert_eq!(skipped[0].previous_version.as_deref(), Some("0.228.18"));
        assert_eq!(skipped[0].new_version.as_deref(), Some("0.228.18"));
        assert!(skipped[0].path_drift.is_some());
        assert!(skipped[0].extensions_failed.is_empty());
        assert_eq!(skipped[0].extensions_synced.len(), 1);
        assert_eq!(
            skipped[0].extensions_synced[0].extension_id,
            "required-extension"
        );
        assert_eq!(skipped[0].extensions_skipped.len(), 1);
        assert_eq!(
            skipped[0].extensions_skipped[0].extension_id,
            "auxiliary-extension"
        );
        let skipped_detail = skipped[0].extensions_skipped[0].detail.as_deref().unwrap();
        assert!(skipped_detail.contains("deferred because runner executable drift"));
        assert!(skipped_detail.contains("extension setup failed"));
        assert!(skipped[0]
            .detail
            .contains("runner extension sync(s) skipped"));
        assert!(!skipped[0]
            .detail
            .contains("runner extension sync(s) failed"));
        assert!(commands
            .iter()
            .any(|command| command.contains(&"required-extension".to_string())));
    }

    #[test]
    fn upgrades_runner_binary_before_controller_scoped_extension_sync() {
        let mut runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy"));
        runner.policy.supported_extensions = vec!["required-extension".to_string()];
        let extension_updates = vec![
            extension_update("irrelevant-extension", "98a61eda"),
            extension_update("required-extension", "48517ac3"),
        ];
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            false,
            None,
            None,
            &extension_updates,
            |runner_id, options| {
                commands.push(options.command.clone());
                let (stdout, stderr, exit_code) = match commands.len() {
                    1 => ("homeboy 0.228.18\n", "", 0),
                    2 => {
                        assert!(
                            options.command.contains(&"--skip-extensions".to_string()),
                            "runner binary upgrade must not run stale runner-side extension sync"
                        );
                        ("{\"success\":true}\n", "", 0)
                    }
                    3 => ("homeboy 0.228.21\n", "", 0),
                    4 => ("{\"success\":true}\n", "", 0),
                    5 => ("{\"success\":true}\n", "", 0),
                    6 => ("homeboy 0.228.21\n", "", 0),
                    _ => ("", "unexpected runner command", 1),
                };
                Ok((
                    exec_output(runner_id, options.command, stdout, stderr, exit_code),
                    exit_code,
                ))
            },
            runner_status,
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(updated[0].success);
        assert_eq!(updated[0].previous_version.as_deref(), Some("0.228.18"));
        assert_eq!(updated[0].new_version.as_deref(), Some("0.228.21"));
        assert_eq!(updated[0].extensions_synced.len(), 1);
        assert_eq!(
            updated[0].extensions_synced[0].extension_id,
            "required-extension"
        );
        assert_eq!(updated[0].extensions_skipped.len(), 1);
        assert_eq!(
            updated[0].extensions_skipped[0].extension_id,
            "irrelevant-extension"
        );
        assert!(commands
            .iter()
            .all(|command| !command.contains(&"irrelevant-extension".to_string())));
        assert!(commands
            .iter()
            .any(|command| command.contains(&"required-extension".to_string())));
    }

    #[test]
    fn repairs_stale_bare_homeboy_after_configured_runner_upgrade() {
        let runner = ssh_runner(
            "lab",
            Some("/home/chubes/Developer/_lab_workspaces/homeboy-current/target/release/homeboy"),
        );
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            false,
            None,
            None,
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.228.4\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.228.5\n",
                    4 => "homeboy 0.228.4\n",
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
        assert!(updated[0].success);
        assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.228.5"));
        assert_eq!(updated[0].path_drift, None);
        assert_eq!(commands[4][0], "homeboy");
        assert_eq!(commands[4][1], "upgrade");
        assert!(commands[4].contains(&"--skip-runners".to_string()));
        assert!(updated[0]
            .detail
            .contains("PATH-visible `homeboy` repaired"));
        assert!(!updated[0].detail.contains("runner PATH drift detected"));
    }

    #[test]
    fn fails_when_managed_bare_homeboy_repair_leaves_path_drift() {
        let runner = ssh_runner(
            "lab",
            Some("/home/chubes/Developer/_lab_workspaces/homeboy-current/target/release/homeboy"),
        );
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            false,
            None,
            None,
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.228.4\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.228.5\n",
                    4 => "homeboy 0.228.4\n",
                    5 => "{\"success\":true}\n",
                    6 => "homeboy 0.228.4\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
        );

        assert!(updated.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].bare_homeboy_version.as_deref(), Some("0.228.4"));
        assert!(skipped[0]
            .path_drift
            .as_deref()
            .unwrap()
            .contains("managed PATH-visible `homeboy` repair left bare `homeboy` at 0.228.4"));
        assert!(skipped[0]
            .recovery_commands
            .contains(&"homeboy upgrade --force --upgrade-runner lab".to_string()));
        assert!(skipped[0]
            .detail
            .contains("managed PATH-visible `homeboy` repair completed"));
        assert!(skipped[0].detail.contains("runner PATH drift detected"));
    }

    #[test]
    fn updates_versioned_runner_homeboy_path_to_bare_homeboy_when_newer() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy-0.229.1"));
        let extension_updates = vec![extension_update("required-extension", "48517ac3")];
        let mut commands = Vec::new();
        let mut path_updates = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
            &[runner],
            false,
            None,
            None,
            &extension_updates,
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.229.1\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.229.1\n",
                    4 => "homeboy 0.229.3\n",
                    5 => "{\"success\":true}\n",
                    6 => "{\"success\":true}\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
            |_runner, _path| unreachable!("source materialization not used"),
            |runner_id, homeboy_path| {
                path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
                Ok(())
            },
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(updated[0].success);
        assert!(updated[0].upgraded);
        assert_eq!(updated[0].homeboy_path, "homeboy");
        assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.1"));
        assert_eq!(updated[0].new_version.as_deref(), Some("0.229.3"));
        assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.229.3"));
        assert_eq!(updated[0].path_drift, None);
        assert_eq!(
            path_updates,
            vec![("lab".to_string(), "homeboy".to_string())]
        );
        assert_eq!(
            commands[4],
            vec!["homeboy", "extension", "show", "required-extension"]
        );
        assert!(updated[0]
            .detail
            .contains("runner homeboy_path updated from `/home/chubes/.cargo/bin/homeboy-0.229.1` to `homeboy`"));
    }

    #[test]
    fn realigns_stale_lab_workspace_homeboy_path_after_upgrade_failure() {
        let stale_path =
            "/home/chubes/Developer/_lab_workspaces/homeboy-post-4583-proof/target/debug/homeboy";
        let runner = ssh_runner("lab", Some(stale_path));
        let mut commands = Vec::new();
        let mut path_updates = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
            &[runner],
            false,
            None,
            None,
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let (stdout, stderr, exit_code) = match commands.len() {
                    1 => ("homeboy 0.229.11\n", "", 0),
                    2 => (
                        "",
                        "source upgrade failed: fatal: not a git repository (or any of the parent directories): .git\n",
                        1,
                    ),
                    3 => ("homeboy 0.230.0\n", "", 0),
                    4 => ("{\"success\":true}\n", "", 0),
                    5 => ("homeboy 0.230.0\n", "", 0),
                    _ => ("", "", 0),
                };
                Ok((
                    exec_output(runner_id, options.command, stdout, stderr, exit_code),
                    exit_code,
                ))
            },
            runner_status,
            |_runner, _path| unreachable!("source materialization not used"),
            |runner_id, homeboy_path| {
                path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
                Ok(())
            },
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(updated[0].success);
        assert!(updated[0].upgraded);
        assert_eq!(updated[0].homeboy_path, "homeboy");
        assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.11"));
        assert_eq!(updated[0].new_version.as_deref(), Some("0.230.0"));
        assert_eq!(updated[0].bare_homeboy_version, None);
        assert_eq!(updated[0].path_drift, None);
        assert_eq!(
            path_updates,
            vec![("lab".to_string(), "homeboy".to_string())]
        );
        assert_eq!(commands[1][0], stale_path);
        assert_eq!(commands[2], vec!["homeboy", "--version"]);
        assert_eq!(commands[3][0], "homeboy");
        assert!(updated[0]
            .detail
            .contains("after configured runner executable failed to upgrade"));
        assert!(updated[0].detail.contains(stale_path));
    }

    #[test]
    fn source_runner_upgrade_realigns_to_materialized_source_build_when_path_shadows_old_homeboy() {
        let stale_path = "/home/chubes/Developer/_lab_workspaces/homeboy-old/target/debug/homeboy";
        let source_path = Path::new("/Users/chubes/Developer/homeboy@current-main");
        let remote_source = "/home/chubes/Developer/_lab_workspaces/homeboy-current-main";
        let source_binary = format!("{remote_source}/target/release/homeboy");
        let runner = ssh_runner("lab", Some(stale_path));
        let mut commands = Vec::new();
        let mut path_updates = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
            &[runner],
            true,
            Some(InstallMethod::Source),
            Some(source_path),
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let (stdout, stderr, exit_code) = match commands.len() {
                    1 => (
                        format!("homeboy {}+oldbuild\n", current_version()),
                        String::new(),
                        0,
                    ),
                    2 => ("{\"success\":true}\n".to_string(), String::new(), 0),
                    3 => (
                        format!("homeboy {}+oldbuild\n", current_version()),
                        String::new(),
                        0,
                    ),
                    4 => (
                        format!("homeboy {}+oldbuild\n", current_version()),
                        String::new(),
                        0,
                    ),
                    5 if options.command[0] == source_binary => {
                        (format!("homeboy {}\n", current_version()), String::new(), 0)
                    }
                    6 if options.command[0] == source_binary => (
                        format!("{}\n", build_identity::current().display),
                        String::new(),
                        0,
                    ),
                    other => (
                        String::new(),
                        format!("unexpected command {other}: {:?}", options.command),
                        1,
                    ),
                };
                Ok((
                    exec_output(runner_id, options.command, &stdout, &stderr, exit_code),
                    exit_code,
                ))
            },
            runner_status,
            |runner, path| {
                assert_eq!(runner.id, "lab");
                assert_eq!(path, source_path);
                Ok(remote_source.to_string())
            },
            |runner_id, homeboy_path| {
                path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
                Ok(())
            },
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(updated[0].success);
        assert!(updated[0].upgraded);
        assert_eq!(updated[0].homeboy_path, source_binary);
        assert_eq!(
            updated[0].previous_version.as_deref(),
            Some(current_version())
        );
        assert_eq!(updated[0].new_version.as_deref(), Some(current_version()));
        assert_eq!(updated[0].bare_homeboy_version, None);
        assert_eq!(updated[0].path_drift, None);
        assert_eq!(path_updates, vec![("lab".to_string(), source_binary)]);
        assert_eq!(commands[1][0], stale_path);
        assert_eq!(commands[1][commands[1].len() - 2], "--source-path");
        assert_eq!(commands[1][commands[1].len() - 1], remote_source);
        assert!(updated[0].detail.contains("to source-built"));
    }

    #[test]
    fn realigns_versioned_runner_homeboy_path_using_final_bare_homeboy_state() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy-0.229.1"));
        let mut commands = Vec::new();
        let mut path_updates = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
            &[runner],
            false,
            None,
            None,
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.229.1\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.229.1\n",
                    4 => "homeboy 0.228.22\n",
                    5 => "homeboy 0.229.6\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
            |_runner, _path| unreachable!("source materialization not used"),
            |runner_id, homeboy_path| {
                path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
                Ok(())
            },
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(updated[0].success);
        assert!(updated[0].upgraded);
        assert_eq!(updated[0].homeboy_path, "homeboy");
        assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.1"));
        assert_eq!(updated[0].new_version.as_deref(), Some("0.229.6"));
        assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.229.6"));
        assert_eq!(updated[0].path_drift, None);
        assert_eq!(
            path_updates,
            vec![("lab".to_string(), "homeboy".to_string())]
        );
        assert_eq!(
            commands[3],
            vec!["homeboy", "--version"],
            "the first bare probe reproduces the stale pre-upgrade state"
        );
        assert_eq!(
            commands[4],
            vec!["homeboy", "--version"],
            "final drift detection re-checks the remote bare binary"
        );
        assert!(updated[0].detail.contains("bare `homeboy` reports 0.229.6"));
        assert!(!updated[0].detail.contains("0.228.22"));
    }

    #[test]
    fn realigns_versioned_runner_homeboy_path_when_only_final_bare_probe_succeeds() {
        let runner = ssh_runner("lab", Some("/home/chubes/.cargo/bin/homeboy-0.229.5"));
        let mut commands = Vec::new();
        let mut path_updates = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor_source_materializer_and_path_updater(
            &[runner],
            false,
            None,
            None,
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let (stdout, stderr, exit_code) = match commands.len() {
                    1 => ("homeboy 0.229.6\n", "", 0),
                    2 => ("{\"success\":true}\n", "", 0),
                    3 => ("homeboy 0.229.6\n", "", 0),
                    4 => ("", "homeboy unavailable during remote upgrade\n", 1),
                    5 => ("homeboy 0.229.7\n", "", 0),
                    _ => ("", "", 0),
                };
                Ok((
                    exec_output(runner_id, options.command, stdout, stderr, exit_code),
                    exit_code,
                ))
            },
            runner_status,
            |_runner, _path| unreachable!("source materialization not used"),
            |runner_id, homeboy_path| {
                path_updates.push((runner_id.to_string(), homeboy_path.to_string()));
                Ok(())
            },
        );

        assert!(skipped.is_empty());
        assert_eq!(updated.len(), 1);
        assert!(updated[0].success);
        assert!(updated[0].upgraded);
        assert_eq!(updated[0].homeboy_path, "homeboy");
        assert_eq!(updated[0].previous_version.as_deref(), Some("0.229.6"));
        assert_eq!(updated[0].new_version.as_deref(), Some("0.229.7"));
        assert_eq!(updated[0].bare_homeboy_version.as_deref(), Some("0.229.7"));
        assert_eq!(updated[0].path_drift, None);
        assert_eq!(
            path_updates,
            vec![("lab".to_string(), "homeboy".to_string())]
        );
        assert_eq!(commands[3], vec!["homeboy", "--version"]);
        assert_eq!(commands[4], vec!["homeboy", "--version"]);
        assert!(updated[0]
            .detail
            .contains("runner homeboy_path updated from `/home/chubes/.cargo/bin/homeboy-0.229.5` to `homeboy`"));
        assert!(!updated[0].detail.contains("runner PATH drift detected"));
    }

    #[test]
    fn reports_exact_runner_set_remediation_when_path_update_is_unsafe() {
        let runner = ssh_runner("lab", Some("/opt/homeboy/custom-homeboy"));
        let mut commands = Vec::new();

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            false,
            None,
            None,
            &[],
            |runner_id, options| {
                commands.push(options.command.clone());
                let stdout = match commands.len() {
                    1 => "homeboy 0.229.1\n",
                    2 => "{\"success\":true}\n",
                    3 => "homeboy 0.229.1\n",
                    4 => "homeboy 0.229.3\n",
                    _ => "",
                };
                Ok((exec_output(runner_id, options.command, stdout, "", 0), 0))
            },
            runner_status,
        );

        assert!(updated.is_empty());
        assert_eq!(skipped.len(), 1);
        assert_eq!(skipped[0].homeboy_path, "/opt/homeboy/custom-homeboy");
        assert!(skipped[0]
            .path_drift
            .as_deref()
            .unwrap()
            .contains("automatic runner homeboy_path update is unsafe"));
        assert!(skipped[0].recovery_commands.contains(
            &"homeboy runner exec lab --ssh -- sh -lc 'type -a homeboy; command -v homeboy; homeboy --version'".to_string()
        ));
        assert!(skipped[0].recovery_commands.contains(
            &"homeboy runner set lab --json '{\"homeboy_path\":\"homeboy\"}'".to_string()
        ));
        assert!(skipped[0]
            .detail
            .contains("homeboy runner set lab --json '{\"homeboy_path\":\"homeboy\"}'"));
    }

    #[test]
    fn reports_stale_connected_daemon_after_runner_upgrade() {
        let runner = ssh_runner("lab", None);

        let (updated, skipped) = upgrade_runners_with_executor(
            &[runner],
            false,
            None,
            None,
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
            active_jobs: Vec::new(),
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
                None,
                None,
            )),
            active_jobs: Vec::new(),
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
            secret_env: HashMap::new(),
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
            dry_run: false,
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
