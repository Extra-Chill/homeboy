use super::super::helpers::version_is_newer;
use super::super::types::InstallMethod;
use super::super::types::RunnerUpgradeEntry;
use super::*;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerExecOptions;
use crate::core::runner::RunnerKind;
use crate::core::runner::RunnerStatusReport;
use crate::core::upgrade::ExtensionUpgradeEntry;
use crate::core::Result;
use std::path::Path;

pub fn upgrade_configured_runners(
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

pub fn runner_upgrade_targets(runner_targets: &[String]) -> Result<Vec<Runner>> {
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

pub fn upgrade_runners_with_executor(
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

pub fn upgrade_runners_with_executor_and_source_materializer(
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

pub fn upgrade_runners_with_executor_source_materializer_and_path_updater(
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
    upgrade_runners_with_executor_source_materializer_path_updater_and_reconnector(
        runners,
        force,
        method_override,
        source_path,
        extension_updates,
        &mut exec,
        status,
        reconnect_runner_daemon,
        &mut materialize_source_path,
        &mut update_homeboy_path,
    )
}

pub fn upgrade_runners_with_executor_source_materializer_path_updater_and_reconnector(
    runners: &[Runner],
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    mut exec: impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: impl Fn(&str) -> Result<RunnerStatusReport>,
    mut reconnect_stale_daemon: impl FnMut(&str) -> Result<String>,
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
            &mut reconnect_stale_daemon,
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

pub fn upgrade_runner_with_executor(
    runner: &Runner,
    force: bool,
    method_override: Option<InstallMethod>,
    source_path: Option<&Path>,
    extension_updates: &[ExtensionUpgradeEntry],
    exec: &mut impl FnMut(&str, RunnerExecOptions) -> Result<(runner::RunnerExecOutput, i32)>,
    status: &impl Fn(&str) -> Result<RunnerStatusReport>,
    reconnect_stale_daemon: &mut impl FnMut(&str) -> Result<String>,
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
    let expected_source_identity = if method_override == Some(InstallMethod::Source) {
        source_path.and_then(source_checkout_build_identity)
    } else {
        None
    };
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
    if let Some(err) = prepare_runner_source_checkout_for_upgrade(
        runner,
        method_override,
        command_source_path.as_deref(),
        exec,
    ) {
        return runner_upgrade_failure_entry(
            &runner.id,
            original_homeboy_path,
            previous_version,
            1,
            err,
        );
    }
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
    let mut source_path_realigned = false;
    if let Some(realignment) = source_upgrade_homeboy_path_realignment(
        runner,
        &original_homeboy_path,
        method_override,
        command_source_path.as_deref(),
        &upgrade_homeboy_path,
        new_version.as_deref(),
        expected_source_identity.as_deref(),
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
    let local_version_drift = runner_local_version_drift(
        &runner.id,
        &homeboy_path,
        previous_version.as_deref(),
        new_version.as_deref(),
    );
    if path_drift.is_none() {
        path_drift = local_version_drift;
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
    let mut stale_daemon_repair_detail = None;
    let mut stale_daemon = runner_stale_daemon(runner, status);
    if stale_daemon.is_some() && path_drift.is_none() {
        match reconnect_stale_daemon(&runner.id) {
            Ok(detail) => {
                stale_daemon = None;
                stale_daemon_repair_detail = Some(detail);
            }
            Err(err) => {
                stale_daemon_repair_detail = Some(format!(
                    "automatic stale runner daemon restart failed: {}",
                    err.message
                ));
            }
        }
    }
    let upgraded = source_path_realigned
        || match (previous_version.as_deref(), new_version.as_deref()) {
            (Some(previous), Some(new)) => new != previous,
            _ => false,
        };
    let success = new_version.is_some()
        && path_drift.is_none()
        && extensions_failed.is_empty()
        && stale_daemon.is_none();
    let detail =
        runner_version_report_detail(detail, previous_version.as_deref(), new_version.as_deref());
    let detail = runner_upgrade_final_detail(
        &runner.id,
        detail,
        path_update_detail.as_deref(),
        stale_daemon_repair_detail.as_deref(),
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
