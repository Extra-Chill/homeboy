use super::super::types::InstallMethod;
use super::super::types::RunnerUpgradeEntry;
use super::*;
use crate::core::runner;
use crate::core::runner::Runner;
use crate::core::runner::RunnerExecOptions;
use crate::core::Result;

/// Captures the exit code and detail produced by a failed runner upgrade attempt.
pub struct FailedUpgradeOutcome {
    pub exit_code: i32,
    pub detail: String,
}

/// Builds a non-recoverable runner upgrade failure entry, preserving every
/// `RunnerUpgradeEntry` field at its canonical "failed before completion" value.
///
/// The caller is responsible for assigning `previous_version`, which is left as
/// `None` here so the surrounding flow can move it in once.
pub fn runner_upgrade_failure_entry(
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
pub fn recover_and_retry_failed_upgrade(
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
pub fn runner_upgrade_retry_failure_entry(
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
