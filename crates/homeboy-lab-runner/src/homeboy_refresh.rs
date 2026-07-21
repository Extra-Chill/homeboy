use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use homeboy_core::engine::shell::{quote_arg, quote_path};
use homeboy_core::error::{Error, Result};
use homeboy_core::git::{run_git, run_git_output};
use homeboy_core::output::MergeOutput;

use super::connection::{
    active_jobs_before_daemon_replacement, disconnect_with_session, rotate_daemon_generation,
};
use super::execution::exec_with_status_snapshot;
use super::{
    connect_with_orphan_adoption, exec, load, materialize_runner_extension_with_env, merge,
    normalize_runner_command_env_for_homeboy_path, plan_controller_snapshot_extension,
    RunnerCapabilityPreflight, RunnerExecOptions, RunnerExecOutput,
    RunnerExtensionMaterializationRequest, RunnerExtensionMaterializationSource,
    RunnerFileTransfer, RunnerKind,
};

#[cfg(test)]
pub(super) use super::{extension_materialization, Runner};

const DEFAULT_HOMEBOY_REMOTE: &str = "https://github.com/Extra-Chill/homeboy.git";
const DEFAULT_HOMEBOY_REF: &str = "main";
const DISCONNECTED_SSH_REFRESH_TIMEOUT: Duration = Duration::from_secs(20 * 60);
const RECONNECT_VERIFICATION_WINDOW: Duration = Duration::from_secs(3);
const RECONNECT_VERIFICATION_RETRY_INTERVAL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeboyBinaryRefreshMode {
    Materialize,
    Select { binary_path: String },
}

#[derive(Debug, Clone)]
pub struct HomeboyBinaryRefreshOptions {
    pub runner_id: String,
    pub mode: HomeboyBinaryRefreshMode,
    pub source: Option<String>,
    pub git_ref: Option<String>,
    pub target_dir: Option<String>,
    pub reconnect: bool,
    pub force: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HomeboyBinaryRefreshPlan {
    pub runner_id: String,
    pub mode: String,
    pub source: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    pub target_dir: Option<String>,
    pub binary_path: String,
    pub script: String,
    pub reconnect: bool,
    pub followup_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyBinaryRefreshOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub dry_run: bool,
    pub plan: HomeboyBinaryRefreshPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub identity: Option<Value>,
    pub updated_fields: Vec<String>,
    pub daemon_refreshed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub interrupted_job_ids: Vec<String>,
    pub selected_binary_path: String,
    pub reconnect_required: bool,
    pub followup_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reconnect_deferred: Option<HomeboyReconnectDeferred>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<HomeboyBinaryRefreshFailure>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub bootstrap_provenance: Option<HomeboyBootstrapProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyReconnectDeferred {
    pub reason: &'static str,
    pub active_job_ids: Vec<String>,
    pub selected_binary_path: String,
    pub followup_commands: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ownership_contention: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyBootstrapProvenance {
    pub transport: &'static str,
    pub requested_ref: Option<String>,
    pub resolved_source_sha: Option<String>,
    pub binary_commit: Option<String>,
    pub binary_identity: Value,
    pub timeout_ms: Option<u128>,
    pub config_fields_changed: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HomeboyBinaryRefreshFailure {
    pub exit_code: i32,
    pub failed_command: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(rename = "ref", skip_serializing_if = "Option::is_none")]
    pub git_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_sha: Option<String>,
    pub build_path: String,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub capture: Option<homeboy_core::engine::command::CommandCaptureMetadata>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub execution_record: Option<homeboy_core::runner_execution_envelope::RunnerExecutionRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub job_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub mirror_run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verification: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RunnerDevSyncOptions {
    pub runner_id: String,
    pub homeboy_source: Option<String>,
    pub homeboy_binary: Option<String>,
    pub extensions: Vec<String>,
    pub reconnect: bool,
    pub dry_run: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunnerDevSyncBinaryProvenance {
    pub sha256: String,
    pub hash: String,
    pub local_binary: String,
    pub remote_binary: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    pub dirty: bool,
}

pub type RunnerDevSyncExtensionProvenance =
    super::extension_materialization::RunnerExtensionMaterializationProvenance;

pub type RunnerDevSyncExtensionPlan =
    super::extension_materialization::RunnerExtensionMaterializationPlan;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerDevSyncPlan {
    pub runner_id: String,
    pub workspace_root: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_binary: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_binary: Option<String>,
    pub extensions: Vec<RunnerDevSyncExtensionPlan>,
    pub reconnect: bool,
    pub followup_commands: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerDevSyncOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub dry_run: bool,
    pub plan: RunnerDevSyncPlan,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub binary: Option<RunnerDevSyncBinaryProvenance>,
    pub extensions: Vec<RunnerDevSyncExtensionProvenance>,
    pub extensions_deferred: Vec<String>,
    pub updated_fields: Vec<String>,
    pub daemon_refreshed: bool,
    pub reconnect_required: bool,
    pub next_actions: Vec<String>,
}

pub fn plan_homeboy_binary_refresh(
    options: &HomeboyBinaryRefreshOptions,
) -> Result<HomeboyBinaryRefreshPlan> {
    let runner = load(&options.runner_id)?;
    let runner_id = runner.id;
    match &options.mode {
        HomeboyBinaryRefreshMode::Select { binary_path } => {
            let binary_path = non_empty("select", binary_path)?;
            let script = identity_probe_script(binary_path);
            Ok(HomeboyBinaryRefreshPlan {
                runner_id: runner_id.clone(),
                mode: "select".to_string(),
                source: None,
                git_ref: None,
                target_dir: None,
                binary_path: binary_path.to_string(),
                script,
                reconnect: options.reconnect,
                followup_commands: refresh_followups(&runner_id, options.reconnect),
            })
        }
        HomeboyBinaryRefreshMode::Materialize => {
            let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "target_dir",
                    "runner refresh-homeboy requires --target-dir when the runner has no workspace_root",
                    None,
                    None,
                )
            })?;
            let source = options
                .source
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_HOMEBOY_REMOTE);
            let git_ref = options
                .git_ref
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .unwrap_or(DEFAULT_HOMEBOY_REF);
            let target_dir = options
                .target_dir
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .unwrap_or_else(|| default_target_dir(workspace_root, git_ref));
            let binary_path = format!(
                "{}/target/release/homeboy",
                target_dir.trim_end_matches('/')
            );
            let script = materialize_script(source, git_ref, &target_dir, &binary_path);
            Ok(HomeboyBinaryRefreshPlan {
                runner_id: runner_id.clone(),
                mode: "materialize".to_string(),
                source: Some(source.to_string()),
                git_ref: Some(git_ref.to_string()),
                target_dir: Some(target_dir),
                binary_path,
                script,
                reconnect: options.reconnect,
                followup_commands: refresh_followups(&runner_id, options.reconnect),
            })
        }
    }
}

pub fn refresh_homeboy_binary(
    options: HomeboyBinaryRefreshOptions,
) -> Result<(HomeboyBinaryRefreshOutput, i32)> {
    let promotion_lease = homeboy_core::runtime_promotion::acquire_for_generation_rotation(
        "runner binary promotion",
        options.runner_id.clone(),
    )?;
    let plan = plan_homeboy_binary_refresh(&options)?;
    // A refresh owns the lease it observed before changing the runner binary.
    // If its local session disappears during that transition, reconnect can
    // explicitly reconcile only that exact proven-dead lease.
    let refresh_session = options
        .reconnect
        .then(|| super::connection::recorded_session(&plan.runner_id))
        .transpose()?
        .flatten();
    let refresh_owned_lease = refresh_session.clone().and_then(refresh_owned_lease);
    if options.dry_run {
        return Ok((
            HomeboyBinaryRefreshOutput {
                variant: "refresh_homeboy",
                command: "runner.refresh_homeboy",
                runner_id: plan.runner_id.clone(),
                dry_run: true,
                identity: None,
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                interrupted_job_ids: Vec::new(),
                selected_binary_path: plan.binary_path.clone(),
                reconnect_required: !plan.reconnect,
                followup_commands: plan.followup_commands.clone(),
                reconnect_deferred: None,
                failure: None,
                bootstrap_provenance: None,
                plan,
            },
            0,
        ));
    }

    let required_commands = match &options.mode {
        HomeboyBinaryRefreshMode::Materialize => {
            vec!["bash".to_string(), "git".to_string(), "cargo".to_string()]
        }
        HomeboyBinaryRefreshMode::Select { .. } => vec!["bash".to_string()],
    };

    promotion_lease.assert_generation()?;
    let runner = load(&plan.runner_id)?;
    let previous_homeboy_path = runner.settings.homeboy_path.clone();
    let connection_status = super::status(&plan.runner_id)?;
    let disconnected_ssh = runner.kind == RunnerKind::Ssh && !connection_status.connected;
    let exec_options = refresh_execution_options(&plan, required_commands, disconnected_ssh);
    let (exec_output, exit_code) =
        exec_with_status_snapshot(&plan.runner_id, exec_options, Some(connection_status))?;
    if exit_code != 0 {
        return Ok((
            HomeboyBinaryRefreshOutput {
                variant: "refresh_homeboy",
                command: "runner.refresh_homeboy",
                runner_id: plan.runner_id.clone(),
                dry_run: false,
                identity: None,
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                interrupted_job_ids: Vec::new(),
                selected_binary_path: plan.binary_path.clone(),
                reconnect_required: !plan.reconnect,
                followup_commands: plan.followup_commands.clone(),
                reconnect_deferred: None,
                failure: Some(refresh_failure(&plan, exec_output, exit_code)),
                bootstrap_provenance: None,
                plan,
            },
            exit_code,
        ));
    }

    // A reconnect replaces the daemon control plane. Prove it is admissible
    // before selecting the new global binary: a deferred refresh must leave
    // the active daemon and its configured control binary untouched.
    // Active jobs are now a rotation case, not a global reconnect barrier. The
    // candidate is materialized and validated before it gets an independent
    // daemon lease; the old lease remains the owner of its existing work.
    let deferred_active_jobs: Option<Vec<homeboy_core::api_jobs::ActiveRunnerJobSummary>> = None;
    if let Some(active_jobs) = deferred_active_jobs {
        let active_job_ids = active_jobs
            .iter()
            .map(|job| job.job_id.clone())
            .collect::<Vec<_>>();
        let followup_commands = active_job_followups(&plan.runner_id, &active_job_ids);
        return Ok((
            HomeboyBinaryRefreshOutput {
                variant: "refresh_homeboy",
                command: "runner.refresh_homeboy",
                runner_id: plan.runner_id.clone(),
                dry_run: false,
                plan: plan.clone(),
                identity: parse_identity(&exec_output.stdout).ok(),
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                interrupted_job_ids: Vec::new(),
                selected_binary_path: plan.binary_path.clone(),
                reconnect_required: true,
                followup_commands: followup_commands.clone(),
                reconnect_deferred: Some(HomeboyReconnectDeferred {
                    reason: "active_daemon_jobs",
                    active_job_ids,
                    selected_binary_path: plan.binary_path.clone(),
                    followup_commands,
                    ownership_contention: None,
                }),
                failure: None,
                bootstrap_provenance: None,
            },
            1,
        ));
    }

    promotion_lease.assert_generation()?;
    // Only a reconnect replaces the daemon, so it is the only mode allowed to
    // change the runner-global daemon control binary. A non-reconnecting
    // refresh materializes a command build for a workflow without creating
    // stale-daemon drift for unrelated jobs.
    let promote_daemon_binary = options.reconnect;
    let bootstrap = if disconnected_ssh {
        ssh_bootstrap_promote_with(
            &plan,
            || Ok(exec_output.stdout.clone()),
            |homeboy_path| {
                if !promote_daemon_binary {
                    return Ok(Vec::new());
                }
                homeboy_core::config::with_config_lock(|| {
                    let patch = refreshed_runner_patch(&plan.runner_id, homeboy_path)?;
                    match merge(Some(&plan.runner_id), &patch.to_string(), &[])? {
                        MergeOutput::Single(result) => Ok(result.updated_fields),
                        MergeOutput::Bulk(_) => Ok(Vec::new()),
                    }
                })
            },
        )
    } else {
        let identity = parse_identity(&exec_output.stdout)?;
        verify_materialized_identity(&plan, &exec_output.stdout, &identity).map_err(|message| {
            Error::validation_invalid_argument(
                "identity",
                message,
                Some(plan.runner_id.clone()),
                None,
            )
        })?;
        let updated_fields = if promote_daemon_binary {
            homeboy_core::config::with_config_lock(|| {
                let patch = refreshed_runner_patch(&plan.runner_id, &plan.binary_path)?;
                match merge(Some(&plan.runner_id), &patch.to_string(), &[])? {
                    MergeOutput::Single(result) => Ok(result.updated_fields),
                    MergeOutput::Bulk(_) => Ok(Vec::new()),
                }
            })?
        } else {
            Vec::new()
        };
        Ok(SshBootstrapPromotion {
            identity,
            source_sha: source_sha_from_output(&exec_output.stdout),
            updated_fields,
        })
    };
    let bootstrap = match bootstrap {
        Ok(bootstrap) => bootstrap,
        Err(error) => {
            let verification = error.message;
            return Ok((
                HomeboyBinaryRefreshOutput {
                    variant: "refresh_homeboy",
                    command: "runner.refresh_homeboy",
                    runner_id: plan.runner_id.clone(),
                    dry_run: false,
                    identity: parse_identity(&exec_output.stdout).ok(),
                    updated_fields: Vec::new(),
                    daemon_refreshed: false,
                    interrupted_job_ids: Vec::new(),
                    selected_binary_path: plan.binary_path.clone(),
                    reconnect_required: !plan.reconnect,
                    followup_commands: plan.followup_commands.clone(),
                    reconnect_deferred: None,
                    failure: Some(refresh_verification_failure(
                        &plan,
                        exec_output,
                        verification,
                    )),
                    bootstrap_provenance: None,
                    plan,
                },
                1,
            ));
        }
    };
    let identity = bootstrap.identity;
    let updated_fields = bootstrap.updated_fields;

    let mut daemon_refreshed = false;
    let interrupted_job_ids;
    if options.reconnect {
        promotion_lease.assert_generation()?;
        let active_jobs = active_jobs_before_daemon_replacement(&plan.runner_id)?;
        if !active_jobs.is_empty() && !options.force {
            let candidate_identity = identity
                .get("data")
                .unwrap_or(&identity)
                .get("display")
                .and_then(Value::as_str)
                .unwrap_or("candidate")
                .to_string();
            let draining_job_ids = active_jobs
                .iter()
                .map(|job| job.job_id.clone())
                .collect::<Vec<_>>();
            if let Err(error) = rotate_daemon_generation(
                &plan.runner_id,
                &plan.binary_path,
                &candidate_identity,
                &draining_job_ids,
            ) {
                restore_runner_homeboy_path_if_selected(
                    &plan.runner_id,
                    &plan.binary_path,
                    previous_homeboy_path.as_deref(),
                )?;
                return Err(error);
            }
            return Ok((
                HomeboyBinaryRefreshOutput {
                    variant: "refresh_homeboy",
                    command: "runner.refresh_homeboy",
                    runner_id: plan.runner_id.clone(),
                    dry_run: false,
                    plan: plan.clone(),
                    identity: Some(identity.clone()),
                    updated_fields: updated_fields.clone(),
                    daemon_refreshed: true,
                    interrupted_job_ids: Vec::new(),
                    selected_binary_path: plan.binary_path.clone(),
                    reconnect_required: false,
                    followup_commands: Vec::new(),
                    reconnect_deferred: None,
                    failure: None,
                    bootstrap_provenance: None,
                },
                0,
            ));
        }
        interrupted_job_ids = match protect_active_jobs_before_reconnect(
            &plan.runner_id,
            &active_jobs,
            options.force,
        ) {
            Ok(job_ids) => job_ids,
            Err(_) => {
                let deferred = defer_reconnect_after_promotion_race(
                    &plan.runner_id,
                    &plan.binary_path,
                    previous_homeboy_path.as_deref(),
                    &active_jobs,
                )?;
                return Ok((
                    HomeboyBinaryRefreshOutput {
                        variant: "refresh_homeboy",
                        command: "runner.refresh_homeboy",
                        runner_id: plan.runner_id.clone(),
                        dry_run: false,
                        plan: plan.clone(),
                        identity: Some(identity),
                        updated_fields: Vec::new(),
                        daemon_refreshed: false,
                        interrupted_job_ids: Vec::new(),
                        selected_binary_path: plan.binary_path.clone(),
                        reconnect_required: true,
                        followup_commands: deferred.followup_commands.clone(),
                        reconnect_deferred: Some(deferred),
                        failure: None,
                        bootstrap_provenance: None,
                    },
                    1,
                ));
            }
        };
        if let Err(error) =
            disconnect_with_session(&plan.runner_id, refresh_session.as_ref(), options.force)
        {
            return rollback_refresh_error_with(error, || {
                restore_runner_homeboy_path_if_selected(
                    &plan.runner_id,
                    &plan.binary_path,
                    previous_homeboy_path.as_deref(),
                )
                .map(|_| ())
            });
        }
        let (report, connect_exit_code) = match connect_with_orphan_adoption(
            &plan.runner_id,
            refresh_owned_lease.as_deref(),
            &[],
            false,
            None,
            None,
            None,
        ) {
            Ok(result) => result,
            Err(error) => {
                return rollback_refresh_connect_error_with(
                    error,
                    || {
                        restore_runner_homeboy_path_if_selected(
                            &plan.runner_id,
                            &plan.binary_path,
                            previous_homeboy_path.as_deref(),
                        )
                        .map(|_| ())
                    },
                    || {
                        let (report, exit_code) = connect_with_orphan_adoption(
                            &plan.runner_id,
                            None,
                            &[],
                            false,
                            None,
                            None,
                            None,
                        )?;
                        if exit_code != 0 || !report.connected {
                            return Err(Error::validation_invalid_argument(
                                "reconnect",
                                report.failure_message.unwrap_or_else(|| {
                                    "rollback reconnect did not persist an active daemon session".to_string()
                                }),
                                Some(plan.runner_id.clone()),
                                None,
                            ));
                        }
                        Ok(())
                    },
                );
            }
        };
        let daemon_identity_verification = (connect_exit_code == 0)
            .then(|| verify_refreshed_daemon_identity(&plan.runner_id, &identity))
            .transpose()
            .map_err(|error| error.message);
        daemon_refreshed = daemon_identity_verification.is_ok();
        if !daemon_refreshed {
            let reconnect_exit_code = if connect_exit_code == 0 {
                1
            } else {
                connect_exit_code
            };
            if let Err(rollback_error) = rollback_refreshed_daemon(
                &plan.runner_id,
                previous_homeboy_path.as_deref(),
                options.force,
            ) {
                return Err(reconnect_rollback_error(&report, rollback_error));
            }
            return Ok((
                HomeboyBinaryRefreshOutput {
                    variant: "refresh_homeboy",
                    command: "runner.refresh_homeboy",
                    runner_id: plan.runner_id.clone(),
                    dry_run: false,
                    plan: plan.clone(),
                    identity: Some(identity),
                    updated_fields: Vec::new(),
                    daemon_refreshed: false,
                    interrupted_job_ids,
                    selected_binary_path: plan.binary_path.clone(),
                    reconnect_required: true,
                    followup_commands: plan.followup_commands.clone(),
                    reconnect_deferred: None,
                    failure: Some(refresh_reconnect_failure(
                        &plan,
                        &exec_output,
                        &report,
                        daemon_identity_verification.err().as_deref(),
                    )),
                    bootstrap_provenance: None,
                },
                reconnect_exit_code,
            ));
        }
    } else {
        interrupted_job_ids = Vec::new();
    }

    Ok((
        HomeboyBinaryRefreshOutput {
            variant: "refresh_homeboy",
            command: "runner.refresh_homeboy",
            runner_id: plan.runner_id.clone(),
            dry_run: false,
            plan: plan.clone(),
            identity: Some(identity.clone()),
            updated_fields: updated_fields.clone(),
            daemon_refreshed,
            interrupted_job_ids,
            selected_binary_path: plan.binary_path.clone(),
            reconnect_required: !daemon_refreshed,
            followup_commands: plan.followup_commands,
            reconnect_deferred: None,
            failure: None,
            bootstrap_provenance: Some(HomeboyBootstrapProvenance {
                transport: if disconnected_ssh {
                    "ssh_bootstrap"
                } else {
                    "daemon"
                },
                requested_ref: plan.git_ref.clone(),
                resolved_source_sha: bootstrap.source_sha,
                binary_commit: identity
                    .get("data")
                    .unwrap_or(&identity)
                    .get("git_commit")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                binary_identity: identity,
                timeout_ms: disconnected_ssh
                    .then_some(DISCONNECTED_SSH_REFRESH_TIMEOUT.as_millis()),
                config_fields_changed: updated_fields.clone(),
            }),
        },
        0,
    ))
}

fn refresh_owned_lease(session: super::RunnerSession) -> Option<String> {
    (session.mode == super::RunnerTunnelMode::DirectSsh
        && session.role == super::RunnerSessionRole::Controller)
        .then_some(session.remote_daemon_lease_id)
        .flatten()
}

fn refresh_failure(
    plan: &HomeboyBinaryRefreshPlan,
    execution: RunnerExecOutput,
    exit_code: i32,
) -> HomeboyBinaryRefreshFailure {
    HomeboyBinaryRefreshFailure {
        exit_code,
        failed_command: execution.argv.clone(),
        source: plan.source.clone(),
        git_ref: plan.git_ref.clone(),
        source_sha: source_sha_from_output(&execution.stdout),
        build_path: plan.binary_path.clone(),
        stdout: execution.stdout.clone(),
        stderr: execution.stderr.clone(),
        capture: execution.capture.clone(),
        execution_record: execution.execution_record.clone(),
        job_id: execution.job_id.clone(),
        mirror_run_id: execution.mirror_run_id.clone(),
        verification: None,
    }
}

fn refresh_verification_failure(
    plan: &HomeboyBinaryRefreshPlan,
    execution: RunnerExecOutput,
    verification: String,
) -> HomeboyBinaryRefreshFailure {
    let mut failure = refresh_failure(plan, execution, 1);
    failure.verification = Some(verification);
    failure
}

fn verify_refreshed_daemon_identity(runner_id: &str, identity: &Value) -> Result<()> {
    let expected_commit = identity
        .get("data")
        .unwrap_or(identity)
        .get("git_commit")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "runner `{runner_id}` refreshed binary did not report a git commit"
            ))
        })?;
    verify_refreshed_daemon_identity_with(
        runner_id,
        expected_commit,
        || super::status(runner_id),
        || std::thread::sleep(RECONNECT_VERIFICATION_RETRY_INTERVAL),
    )
}

fn verify_refreshed_daemon_identity_with<Status, Wait>(
    runner_id: &str,
    expected_commit: &str,
    mut status: Status,
    mut wait: Wait,
) -> Result<()>
where
    Status: FnMut() -> Result<super::RunnerStatusReport>,
    Wait: FnMut(),
{
    let deadline = Instant::now() + RECONNECT_VERIFICATION_WINDOW;
    loop {
        match verify_refreshed_daemon_status(runner_id, expected_commit, &status()?) {
            Ok(()) => return Ok(()),
            Err(_) if Instant::now() < deadline => wait(),
            Err(error) => return Err(error),
        }
    }
}

fn verify_refreshed_daemon_status(
    runner_id: &str,
    expected_commit: &str,
    status: &super::RunnerStatusReport,
) -> Result<()> {
    if !status.is_connected() {
        return Err(Error::internal_unexpected(format!(
            "runner `{runner_id}` reconnect did not persist an active daemon session"
        )));
    }
    if let Some(stale_daemon) = &status.stale_daemon {
        return Err(Error::internal_unexpected(format!(
            "runner `{runner_id}` reconnect retained a stale daemon: {}",
            stale_daemon.message
        )));
    }
    let actual_identity = status
        .session
        .as_ref()
        .and_then(|session| session.homeboy_build_identity.as_deref())
        .ok_or_else(|| {
            Error::internal_unexpected(format!(
                "runner `{runner_id}` reconnect did not persist a daemon build identity"
            ))
        })?;
    if build_identity_commit(actual_identity) != Some(expected_commit) {
        return Err(Error::internal_unexpected(format!(
            "runner `{runner_id}` reconnect started daemon `{actual_identity}`, expected commit `{expected_commit}`"
        )));
    }
    Ok(())
}

fn build_identity_commit(identity: &str) -> Option<&str> {
    let (_, commit) = identity.strip_prefix("homeboy ")?.split_once('+')?;
    Some(commit.strip_suffix("-dirty").unwrap_or(commit))
}

fn refresh_execution_options(
    plan: &HomeboyBinaryRefreshPlan,
    required_commands: Vec<String>,
    disconnected_ssh: bool,
) -> RunnerExecOptions {
    let options = if disconnected_ssh {
        RunnerExecOptions::diagnostic_raw_shell(plan.script.clone())
            .with_diagnostic_ssh_timeout(DISCONNECTED_SSH_REFRESH_TIMEOUT)
    } else {
        RunnerExecOptions::raw_command(vec![
            "bash".to_string(),
            "-lc".to_string(),
            plan.script.clone(),
        ])
    };
    options.with_capability_preflight(RunnerCapabilityPreflight {
        command: "runner.refresh-homeboy".to_string(),
        required_commands,
        timeout: disconnected_ssh.then_some(DISCONNECTED_SSH_REFRESH_TIMEOUT),
        ..Default::default()
    })
}

#[derive(Debug, Clone)]
struct SshBootstrapPromotion {
    identity: Value,
    source_sha: Option<String>,
    updated_fields: Vec<String>,
}

/// Own the disconnected bootstrap boundary so transport and config mutation are
/// independently testable. Promotion is deliberately after exact identity
/// verification; a failed command or mismatched binary cannot touch config.
fn ssh_bootstrap_promote_with<Execute, Promote>(
    plan: &HomeboyBinaryRefreshPlan,
    execute: Execute,
    promote: Promote,
) -> Result<SshBootstrapPromotion>
where
    Execute: FnOnce() -> Result<String>,
    Promote: FnOnce(&str) -> Result<Vec<String>>,
{
    let stdout = execute()?;
    let identity = parse_identity(&stdout)?;
    verify_materialized_identity(plan, &stdout, &identity).map_err(|message| {
        Error::validation_invalid_argument("identity", message, Some(plan.runner_id.clone()), None)
    })?;
    let source_sha = source_sha_from_output(&stdout);
    let updated_fields = promote(&plan.binary_path)?;
    Ok(SshBootstrapPromotion {
        identity,
        source_sha,
        updated_fields,
    })
}

fn verify_materialized_identity(
    plan: &HomeboyBinaryRefreshPlan,
    stdout: &str,
    identity: &Value,
) -> std::result::Result<(), String> {
    if plan.mode != "materialize" {
        return Ok(());
    }
    let source_sha = source_sha_from_output(stdout)
        .ok_or_else(|| "materialized refresh did not report its resolved source SHA".to_string())?;
    let identity = identity.get("data").unwrap_or(identity);
    let built_commit = identity
        .get("git_commit")
        .and_then(Value::as_str)
        .ok_or_else(|| "materialized refresh identity did not report git_commit".to_string())?;
    if !source_sha.starts_with(built_commit) {
        return Err(format!(
            "materialized refresh built identity commit `{built_commit}` does not match resolved ref `{source_sha}`"
        ));
    }
    match identity.get("git_dirty").and_then(Value::as_bool) {
        Some(false) => {}
        Some(true) => return Err("materialized refresh identity is not a clean build".to_string()),
        None => {
            let version = identity
                .get("version")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    "materialized refresh identity without git_dirty did not report version"
                        .to_string()
                })?;
            let expected_display = format!("homeboy {version}+{built_commit}");
            if identity.get("display").and_then(Value::as_str) != Some(&expected_display) {
                return Err(
                    "materialized refresh identity without git_dirty is not a canonical clean build"
                        .to_string(),
                );
            }
        }
    }
    Ok(())
}

pub fn plan_runner_dev_sync(options: &RunnerDevSyncOptions) -> Result<RunnerDevSyncPlan> {
    let runner = load(&options.runner_id)?;
    let workspace_root = runner.workspace_root.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_root",
            "runner dev-sync requires the runner to configure workspace_root",
            Some(options.runner_id.clone()),
            Some(vec![
                "Set runner.workspace_root before selecting a managed dev binary slot.".to_string(),
            ]),
        )
    })?;
    Ok(RunnerDevSyncPlan {
        runner_id: runner.id.clone(),
        workspace_root: workspace_root.to_string(),
        local_binary: options.homeboy_binary.clone(),
        remote_binary: options
            .homeboy_binary
            .as_ref()
            .and_then(|path| sha256_file(Path::new(path)).ok())
            .map(|sha| dev_binary_path(workspace_root, &sha)),
        extensions: plan_extension_overlays(workspace_root, &options.extensions)?,
        reconnect: options.reconnect,
        followup_commands: dev_sync_followups(&runner.id, options),
    })
}

pub fn runner_dev_sync(options: RunnerDevSyncOptions) -> Result<(RunnerDevSyncOutput, i32)> {
    if options.homeboy_binary.is_some() && options.homeboy_source.is_some() {
        return Err(Error::validation_invalid_argument(
            "homeboy_binary",
            "Pass either --homeboy-binary or --homeboy-source, not both",
            None,
            None,
        ));
    }
    if !options.extensions.is_empty() {
        validate_extension_specs(&options.extensions)?;
    }

    let mut plan = plan_runner_dev_sync(&options)?;
    let sync_homeboy_binary = should_sync_homeboy_binary(&options);
    if options.dry_run {
        return Ok((
            RunnerDevSyncOutput {
                variant: "dev_sync",
                command: "runner.dev_sync",
                runner_id: plan.runner_id.clone(),
                dry_run: true,
                plan,
                binary: None,
                extensions: Vec::new(),
                extensions_deferred: Vec::new(),
                updated_fields: Vec::new(),
                daemon_refreshed: false,
                reconnect_required: sync_homeboy_binary && !options.reconnect,
                next_actions: dev_sync_next_actions(&options.runner_id, &options),
            },
            0,
        ));
    }

    let mut refresh_output = None;
    let mut binary = None;
    if sync_homeboy_binary {
        let source_path = options.homeboy_source.as_deref().map(expand_path);
        let (local_binary, _target_lease) = match options.homeboy_binary.as_deref() {
            Some(path) => (expand_path(path), None),
            None => build_local_homeboy_binary(source_path.as_deref())?,
        };
        let runner = load(&options.runner_id)?;
        validate_dev_sync_binary_for_runner(&runner, &local_binary)?;
        let sha256 = sha256_file(&local_binary)?;
        let hash = sha256[..16].to_string();
        let remote_binary = dev_binary_path(&plan.workspace_root, &sha256);
        plan.local_binary = Some(local_binary.display().to_string());
        plan.remote_binary = Some(remote_binary.clone());

        let transfer = RunnerFileTransfer::for_runner(&runner, None)?;
        let remote_parent = Path::new(&remote_binary)
            .parent()
            .and_then(Path::to_str)
            .ok_or_else(|| {
                Error::internal_json("invalid remote dev binary path".to_string(), None)
            })?;
        transfer.ensure_directory(remote_parent)?;
        transfer.upload_file(&local_binary.display().to_string(), &remote_binary)?;

        let chmod_script = format!("chmod 0755 {}", quote_path(&remote_binary));
        let (_chmod, chmod_exit) = exec(
            &options.runner_id,
            RunnerExecOptions::diagnostic_raw_shell(chmod_script),
        )?;
        if chmod_exit != 0 {
            return Ok((
                dev_sync_failure_output(options, plan, None, Vec::new()),
                chmod_exit,
            ));
        }

        let (refreshed, exit_code) = refresh_homeboy_binary(HomeboyBinaryRefreshOptions {
            runner_id: options.runner_id.clone(),
            mode: HomeboyBinaryRefreshMode::Select {
                binary_path: remote_binary.clone(),
            },
            source: None,
            git_ref: None,
            target_dir: None,
            reconnect: options.reconnect,
            force: false,
            dry_run: false,
        })?;
        if exit_code != 0 {
            return Ok((
                dev_sync_failure_output(options, plan, None, Vec::new()),
                exit_code,
            ));
        }

        let source_revision = source_path.as_deref().and_then(git_revision);
        let dirty = source_path.as_deref().is_some_and(git_dirty);
        binary = Some(RunnerDevSyncBinaryProvenance {
            sha256: sha256.clone(),
            hash,
            local_binary: local_binary.display().to_string(),
            remote_binary: remote_binary.clone(),
            source_path: source_path.map(|path| path.display().to_string()),
            source_revision,
            dirty,
        });
        refresh_output = Some(refreshed);
    }
    let extensions = sync_extension_overlays(&options.runner_id, &plan)?;

    let mut runner = load(&options.runner_id)?;
    let dev_sync = updated_dev_sync_resource(
        runner.resources.get("dev_sync").cloned(),
        binary.clone(),
        &extensions,
    )?;
    runner.resources.insert("dev_sync".to_string(), dev_sync);
    let patch = serde_json::json!({ "resources": runner.resources });
    let mut updated_fields = refresh_output
        .as_ref()
        .map(|output| output.updated_fields.clone())
        .unwrap_or_default();
    let replace_fields = vec!["resources".to_string()];
    if let MergeOutput::Single(result) = merge(
        Some(&options.runner_id),
        &patch.to_string(),
        &replace_fields,
    )? {
        updated_fields.extend(result.updated_fields);
    }

    Ok((
        RunnerDevSyncOutput {
            variant: "dev_sync",
            command: "runner.dev_sync",
            runner_id: options.runner_id.clone(),
            dry_run: false,
            plan,
            binary,
            extensions,
            extensions_deferred: Vec::new(),
            updated_fields,
            daemon_refreshed: refresh_output
                .as_ref()
                .is_some_and(|output| output.daemon_refreshed),
            reconnect_required: sync_homeboy_binary && !options.reconnect,
            next_actions: dev_sync_next_actions(&options.runner_id, &options),
        },
        0,
    ))
}

fn dev_sync_failure_output(
    options: RunnerDevSyncOptions,
    plan: RunnerDevSyncPlan,
    binary: Option<RunnerDevSyncBinaryProvenance>,
    extensions: Vec<RunnerDevSyncExtensionProvenance>,
) -> RunnerDevSyncOutput {
    let reconnect_required = should_sync_homeboy_binary(&options) && !options.reconnect;
    let next_actions = dev_sync_next_actions(&options.runner_id, &options);
    RunnerDevSyncOutput {
        variant: "dev_sync",
        command: "runner.dev_sync",
        runner_id: options.runner_id.clone(),
        dry_run: false,
        plan,
        binary,
        extensions,
        extensions_deferred: options.extensions,
        updated_fields: Vec::new(),
        daemon_refreshed: false,
        reconnect_required,
        next_actions,
    }
}

fn should_sync_homeboy_binary(options: &RunnerDevSyncOptions) -> bool {
    options.homeboy_binary.is_some()
        || options.homeboy_source.is_some()
        || options.extensions.is_empty()
}

fn plan_extension_overlays(
    workspace_root: &str,
    specs: &[String],
) -> Result<Vec<RunnerDevSyncExtensionPlan>> {
    specs
        .iter()
        .map(|spec| {
            let (id, source) = parse_extension_spec(spec)?;
            plan_controller_snapshot_extension(workspace_root, id, &expand_path(source))
        })
        .collect()
}

fn sync_extension_overlays(
    runner_id: &str,
    plan: &RunnerDevSyncPlan,
) -> Result<Vec<RunnerDevSyncExtensionProvenance>> {
    let runner = load(runner_id)?;
    let homeboy_env = installed_homeboy_env(&runner.env, runner.settings.homeboy_path.as_deref());
    let mut overlays = Vec::new();
    for extension in &plan.extensions {
        overlays.push(materialize_runner_extension_with_env(
            &runner,
            "homeboy",
            Some(homeboy_env.clone()),
            &RunnerExtensionMaterializationRequest {
                id: extension.id.clone(),
                revision: extension.content_hash.clone(),
                source: RunnerExtensionMaterializationSource::ControllerSnapshot {
                    local_path: PathBuf::from(&extension.source_path),
                },
            },
        )?);
    }
    Ok(overlays)
}

fn updated_dev_sync_resource(
    existing: Option<Value>,
    binary: Option<RunnerDevSyncBinaryProvenance>,
    synced_extensions: &[RunnerDevSyncExtensionProvenance],
) -> Result<Value> {
    let mut dev_sync = existing.unwrap_or_else(|| serde_json::json!({}));
    if !dev_sync.is_object() {
        dev_sync = serde_json::json!({});
    }

    dev_sync["schema"] = Value::String("homeboy/runner-dev-sync/v1".to_string());
    if let Some(binary) = binary {
        dev_sync["homeboy"] = serde_json::to_value(binary)
            .map_err(|err| Error::internal_json(err.to_string(), None))?;
    } else if dev_sync.get("homeboy").is_none() {
        dev_sync["homeboy"] = Value::Null;
    }

    let mut extensions = dev_sync
        .get("extensions")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let synced_extension_ids = synced_extensions
        .iter()
        .map(|extension| extension.id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    extensions.retain(|entry| {
        entry
            .get("id")
            .and_then(Value::as_str)
            .is_none_or(|id| !synced_extension_ids.contains(id))
    });
    for extension in synced_extensions {
        extensions.push(
            serde_json::to_value(extension)
                .map_err(|err| Error::internal_json(err.to_string(), None))?,
        );
    }
    dev_sync["extensions"] = Value::Array(dedup_extension_metadata_by_id(extensions));
    dev_sync["extensions_deferred"] = Value::Array(Vec::new());
    Ok(dev_sync)
}

fn dedup_extension_metadata_by_id(extensions: Vec<Value>) -> Vec<Value> {
    let mut deduped = Vec::new();
    for extension in extensions {
        let Some(extension_id) = extension.get("id").and_then(Value::as_str) else {
            deduped.push(extension);
            continue;
        };
        if let Some(index) = deduped
            .iter()
            .position(|entry: &Value| entry.get("id").and_then(Value::as_str) == Some(extension_id))
        {
            deduped.remove(index);
        }
        deduped.push(extension);
    }
    deduped
}

fn installed_homeboy_env(
    env: &std::collections::HashMap<String, String>,
    configured_homeboy_path: Option<&str>,
) -> std::collections::HashMap<String, String> {
    let mut env = env.clone();
    env.remove("HOMEBOY_COMMAND");
    let Some(configured_homeboy_path) = configured_homeboy_path else {
        return env;
    };
    let configured_homeboy_path = Path::new(configured_homeboy_path);
    if !configured_homeboy_path.is_absolute() {
        return env;
    }
    let Some(parent) = configured_homeboy_path.parent().and_then(Path::to_str) else {
        return env;
    };
    if let Some(path) = env.get_mut("PATH") {
        let filtered = path
            .split(':')
            .filter(|part| *part != parent)
            .collect::<Vec<_>>()
            .join(":");
        *path = filtered;
    }
    env
}

fn materialize_script(source: &str, git_ref: &str, target_dir: &str, binary_path: &str) -> String {
    format!(
        "set -e\nsource={}\nref={}\ndir={}\nbinary={}\nmkdir -p \"$(dirname \"$dir\")\"\nif [ ! -d \"$dir/.git\" ]; then\n  git clone \"$source\" \"$dir\"\nfi\ncurrent_remote=$(git -C \"$dir\" config --get remote.origin.url 2>/dev/null || true)\nif [ \"$current_remote\" != \"$source\" ]; then\n  git -C \"$dir\" remote set-url origin \"$source\" 2>/dev/null || git -C \"$dir\" remote add origin \"$source\"\nfi\ngit -C \"$dir\" fetch --prune origin\nrequested=$(git -C \"$dir\" rev-parse --verify --quiet \"origin/$ref\" || git -C \"$dir\" rev-parse --verify --quiet \"$ref\")\nif [ -z \"$requested\" ]; then\n  echo \"Homeboy ref not found: $ref\" >&2\n  exit 1\nfi\ntarget=$(git -C \"$dir\" rev-parse --verify --quiet \"${{requested}}^{{commit}}\")\ngit -C \"$dir\" checkout --quiet --force --detach \"$target\"\ngit -C \"$dir\" reset --hard \"$target\"\necho \"HOMEBOY_REFRESH_SOURCE_SHA=$target\"\ncargo build --release --bin homeboy --manifest-path \"$dir/Cargo.toml\"\n\"$binary\" self identity\n",
        quote_path(source),
        quote_path(git_ref),
        quote_path(target_dir),
        quote_path(binary_path),
    )
}

fn source_sha_from_output(stdout: &str) -> Option<String> {
    stdout.lines().find_map(|line| {
        line.strip_prefix("HOMEBOY_REFRESH_SOURCE_SHA=")
            .filter(|sha| !sha.is_empty())
            .map(str::to_string)
    })
}

fn identity_probe_script(binary_path: &str) -> String {
    format!(
        "set -e\nbinary={}\n\"$binary\" self identity\n",
        quote_path(binary_path)
    )
}

fn parse_identity(stdout: &str) -> Result<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return Err(Error::internal_json(
            "refresh-homeboy produced no identity output".to_string(),
            None,
        ));
    }
    let json_start = trimmed.rfind("\n{").map(|index| index + 1).unwrap_or(0);
    let payload = &trimmed[json_start..];
    serde_json::from_str(payload).map_err(|err| {
        Error::internal_json(
            err.to_string(),
            Some("parse runner Homeboy identity output".to_string()),
        )
    })
}

fn refreshed_runner_env(
    runner_id: &str,
    homeboy_path: &str,
) -> Result<std::collections::HashMap<String, String>> {
    let runner = load(runner_id)?;
    let mut env = runner.env;
    normalize_runner_command_env_for_homeboy_path(&mut env, Some(homeboy_path));
    Ok(env)
}

fn refreshed_runner_patch(runner_id: &str, homeboy_path: &str) -> Result<Value> {
    let _ = runner_id;
    Ok(serde_json::json!({
        "homeboy_path": homeboy_path,
    }))
}

fn restore_runner_homeboy_path(runner_id: &str, homeboy_path: Option<&str>) -> Result<()> {
    homeboy_core::config::with_config_lock(|| {
        let patch = serde_json::json!({ "homeboy_path": homeboy_path });
        match merge(Some(runner_id), &patch.to_string(), &[])? {
            MergeOutput::Single(_) | MergeOutput::Bulk(_) => Ok(()),
        }
    })
}

/// Restore only if this promotion still owns the selected value. A later
/// serialized transaction may have selected another binary after this one
/// failed; compensation must never overwrite that newer owner.
fn restore_runner_homeboy_path_if_selected(
    runner_id: &str,
    selected_homeboy_path: &str,
    previous_homeboy_path: Option<&str>,
) -> Result<bool> {
    homeboy_core::config::with_config_lock(|| {
        let runner = load(runner_id)?;
        if runner.settings.homeboy_path.as_deref() != Some(selected_homeboy_path) {
            return Ok(false);
        }
        let patch = serde_json::json!({ "homeboy_path": previous_homeboy_path });
        match merge(Some(runner_id), &patch.to_string(), &[])? {
            MergeOutput::Single(_) | MergeOutput::Bulk(_) => Ok(true),
        }
    })
}

fn defer_reconnect_after_promotion_race(
    runner_id: &str,
    selected_homeboy_path: &str,
    previous_homeboy_path: Option<&str>,
    active_jobs: &[homeboy_core::api_jobs::ActiveRunnerJobSummary],
) -> Result<HomeboyReconnectDeferred> {
    let active_job_ids = active_jobs
        .iter()
        .map(|job| job.job_id.clone())
        .collect::<Vec<_>>();
    let restored = restore_runner_homeboy_path_if_selected(
        runner_id,
        selected_homeboy_path,
        previous_homeboy_path,
    )?;
    let ownership_contention = (!restored).then(|| format!(
        "runner `{runner_id}` binary selection changed after this promotion selected `{selected_homeboy_path}`; preserving the newer owner while reconnect remains deferred"
    ));
    Ok(HomeboyReconnectDeferred {
        reason: "active_daemon_jobs",
        active_job_ids: active_job_ids.clone(),
        selected_binary_path: selected_homeboy_path.to_string(),
        followup_commands: active_job_followups(runner_id, &active_job_ids),
        ownership_contention,
    })
}

fn rollback_refreshed_daemon(
    runner_id: &str,
    previous_homeboy_path: Option<&str>,
    force: bool,
) -> Result<()> {
    // Stop the newly selected daemon before restoring configuration so all
    // persisted state converges on the previous binary before it reconnects.
    rollback_refreshed_daemon_with(
        previous_homeboy_path,
        || disconnect_with_session(runner_id, None, force).map(|_| ()),
        |homeboy_path| restore_runner_homeboy_path(runner_id, homeboy_path),
        |_| {
            let (report, exit_code) =
                connect_with_orphan_adoption(runner_id, None, &[], false, None, None, None)?;
            if exit_code != 0 || !report.connected {
                return Err(Error::validation_invalid_argument(
                    "reconnect",
                    report.failure_message.unwrap_or_else(|| {
                        "rollback reconnect did not persist an active daemon session".to_string()
                    }),
                    Some(runner_id.to_string()),
                    None,
                ));
            }
            Ok(())
        },
    )
}

fn rollback_refreshed_daemon_with<Stop, Restore, Reconnect>(
    previous_homeboy_path: Option<&str>,
    stop: Stop,
    restore: Restore,
    reconnect: Reconnect,
) -> Result<()>
where
    Stop: FnOnce() -> Result<()>,
    Restore: FnOnce(Option<&str>) -> Result<()>,
    Reconnect: FnOnce(Option<&str>) -> Result<()>,
{
    stop()?;
    restore(previous_homeboy_path)?;
    reconnect(previous_homeboy_path)
}

fn rollback_refresh_connect_error_with<T, Restore, Reconnect>(
    primary_error: Error,
    restore: Restore,
    reconnect: Reconnect,
) -> Result<T>
where
    Restore: FnOnce() -> Result<()>,
    Reconnect: FnOnce() -> Result<()>,
{
    rollback_refresh_error_with(primary_error, || {
        restore()?;
        reconnect()
    })
}

fn rollback_refresh_error_with<T, Restore>(mut primary_error: Error, restore: Restore) -> Result<T>
where
    Restore: FnOnce() -> Result<()>,
{
    if let Err(rollback_error) = restore() {
        primary_error.message = format!(
            "{}; additionally failed to restore the pre-refresh runner binary: {}",
            error_context(&primary_error),
            error_context(&rollback_error)
        );
        primary_error.details["rollback_error"] = serde_json::json!({
            "code": rollback_error.code.as_str(),
            "message": rollback_error.message,
            "details": rollback_error.details,
        });
    }
    Err(primary_error)
}

fn reconnect_rollback_error(report: &super::RunnerConnectReport, rollback_error: Error) -> Error {
    let primary_error = Error::validation_invalid_argument(
        "reconnect",
        report
            .failure_message
            .as_deref()
            .unwrap_or("runner connect returned a non-zero exit code"),
        Some(report.runner_id.clone()),
        None,
    );
    let mut error = primary_error;
    error.message = format!(
        "{}; additionally failed to restore the pre-refresh runner binary: {}",
        error.message,
        error_context(&rollback_error)
    );
    error.details["rollback_error"] = serde_json::json!({
        "code": rollback_error.code.as_str(),
        "message": rollback_error.message,
        "details": rollback_error.details,
    });
    error
}

fn error_context(error: &Error) -> String {
    if error.details.is_null() || error.details == serde_json::json!({}) {
        error.message.clone()
    } else {
        format!("{}: {}", error.message, error.details)
    }
}

fn refresh_reconnect_failure(
    plan: &HomeboyBinaryRefreshPlan,
    execution: &RunnerExecOutput,
    report: &super::RunnerConnectReport,
    daemon_identity_verification: Option<&str>,
) -> HomeboyBinaryRefreshFailure {
    let mut failure = refresh_failure(plan, execution.clone(), 1);
    failure.verification = Some(format!(
        "reconnect failed after selection and the configured binary was restored: {}",
        report
            .failure_message
            .as_deref()
            .or(daemon_identity_verification)
            .unwrap_or("runner connect returned a non-zero exit code")
    ));
    failure
}

fn build_local_homeboy_binary(
    source_path: Option<&Path>,
) -> Result<(
    PathBuf,
    Option<homeboy_core::cleanup::SharedCargoTargetLease>,
)> {
    let source_path = match source_path {
        Some(path) => path.to_path_buf(),
        None => {
            std::env::current_dir().map_err(|err| Error::internal_json(err.to_string(), None))?
        }
    };
    let manifest = source_path.join("Cargo.toml");
    if !manifest.is_file() {
        return Err(Error::validation_invalid_argument(
            "homeboy_source",
            "homeboy source path must contain Cargo.toml",
            Some(source_path.display().to_string()),
            None,
        ));
    }
    let target = homeboy_core::cleanup::acquire_shared_cargo_target(&format!(
        "runner-refresh:{}",
        source_path.display()
    ))?;
    let status = Command::new("cargo")
        .args(["build", "--release", "--bin", "homeboy", "--manifest-path"])
        .arg(&manifest)
        .env("CARGO_TARGET_DIR", target.target_dir())
        .status()
        .map_err(|err| {
            Error::internal_json(err.to_string(), Some("build local homeboy".to_string()))
        })?;
    if !status.success() {
        return Err(Error::validation_invalid_argument(
            "homeboy_source",
            format!("cargo build failed with status {status}"),
            Some(source_path.display().to_string()),
            None,
        ));
    }
    Ok((target.target_dir().join("release/homeboy"), Some(target)))
}

fn dev_binary_path(workspace_root: &str, sha256: &str) -> String {
    format!(
        "{}/_homeboy_binaries/dev/{}/homeboy",
        workspace_root.trim_end_matches('/'),
        &sha256[..16]
    )
}

fn sha256_file(path: &Path) -> Result<String> {
    let mut file = fs::File::open(path).map_err(|err| {
        Error::validation_invalid_argument(
            "homeboy_binary",
            format!("could not read binary: {err}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 8192];
    loop {
        let read = file
            .read(&mut buffer)
            .map_err(|err| Error::internal_json(err.to_string(), None))?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn validate_dev_sync_binary_for_runner(runner: &super::Runner, binary: &Path) -> Result<()> {
    if runner.kind == RunnerKind::Ssh && is_macho_binary(binary)? {
        return Err(Error::validation_invalid_argument(
            "homeboy_binary",
            "runner dev-sync refuses to upload a Darwin/Mach-O Homeboy binary to an SSH runner",
            Some(binary.display().to_string()),
            Some(vec![
                format!(
                    "Build/select Homeboy on the runner with `homeboy runner refresh-homeboy {} --ref main --reconnect`.",
                    shell_arg(&runner.id)
                ),
                format!(
                    "For extension-only sync, run `homeboy runner dev-sync {} --extensions <id>=<path>` without --homeboy-binary or --homeboy-source.",
                    shell_arg(&runner.id)
                ),
            ]),
        ));
    }
    Ok(())
}

fn is_macho_binary(path: &Path) -> Result<bool> {
    let mut file = fs::File::open(path).map_err(|err| {
        Error::validation_invalid_argument(
            "homeboy_binary",
            format!("could not read binary: {err}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    let mut magic = [0_u8; 4];
    let read = file
        .read(&mut magic)
        .map_err(|err| Error::internal_json(err.to_string(), None))?;
    Ok(read == 4
        && matches!(
            magic,
            [0xfe, 0xed, 0xfa, 0xce]
                | [0xce, 0xfa, 0xed, 0xfe]
                | [0xfe, 0xed, 0xfa, 0xcf]
                | [0xcf, 0xfa, 0xed, 0xfe]
                | [0xca, 0xfe, 0xba, 0xbe]
                | [0xbe, 0xba, 0xfe, 0xca]
        ))
}

fn expand_path(path: &str) -> PathBuf {
    PathBuf::from(shellexpand::tilde(path).to_string())
}

fn git_revision(path: &Path) -> Option<String> {
    run_git(path, &["rev-parse", "HEAD"], "git rev-parse")
        .ok()
        .map(|stdout| stdout.trim().to_string())
}

fn git_dirty(path: &Path) -> bool {
    run_git_output(path, &["status", "--porcelain"], "git status --porcelain")
        .ok()
        .is_some_and(|output| !output.stdout.is_empty())
}

fn validate_extension_specs(specs: &[String]) -> Result<()> {
    for spec in specs {
        parse_extension_spec(spec)?;
    }
    Ok(())
}

fn parse_extension_spec(spec: &str) -> Result<(&str, &str)> {
    let Some((id, path)) = spec.split_once('=') else {
        return Err(Error::validation_invalid_argument(
            "extensions",
            "extension dev-sync specs must use id=path",
            Some(spec.to_string()),
            None,
        ));
    };
    let id = id.trim();
    let path = path.trim();
    if id.is_empty() || path.is_empty() {
        return Err(Error::validation_invalid_argument(
            "extensions",
            "extension dev-sync specs must include a non-empty id and path",
            Some(spec.to_string()),
            None,
        ));
    }
    Ok((id, path))
}

fn dev_sync_followups(runner_id: &str, options: &RunnerDevSyncOptions) -> Vec<String> {
    dev_sync_next_actions(runner_id, options)
}

fn dev_sync_next_actions(runner_id: &str, options: &RunnerDevSyncOptions) -> Vec<String> {
    if !should_sync_homeboy_binary(options) {
        return Vec::new();
    }

    let mut actions = refresh_followups(runner_id, options.reconnect);
    actions.push(format!(
        "homeboy runner refresh-homeboy {} --ref v{} --reconnect",
        shell_arg(runner_id),
        homeboy_product_identity::product_version()
    ));
    actions
}

fn default_target_dir(workspace_root: &str, git_ref: &str) -> String {
    format!(
        "{}/_homeboy_binaries/homeboy-{}",
        workspace_root.trim_end_matches('/'),
        sanitize_ref(git_ref)
    )
}

fn refresh_followups(runner_id: &str, reconnect: bool) -> Vec<String> {
    if reconnect {
        vec![format!("homeboy runner status {}", shell_arg(runner_id))]
    } else {
        vec![
            format!("homeboy runner disconnect {}", shell_arg(runner_id)),
            format!("homeboy runner connect {}", shell_arg(runner_id)),
            format!("homeboy runner status {}", shell_arg(runner_id)),
        ]
    }
}

fn active_job_reconnect_error(runner_id: &str, job_ids: &[String]) -> Error {
    let follow_commands = job_ids
        .iter()
        .map(|job_id| {
            format!(
                "homeboy runner job logs {} {} --follow",
                shell_arg(runner_id),
                shell_arg(job_id)
            )
        })
        .collect::<Vec<_>>();
    let mut error = Error::validation_invalid_argument(
        "reconnect",
        format!(
            "runner `{runner_id}` has active daemon jobs: {}. Wait for them to reach a terminal state before reconnecting, or rerun with --force to interrupt them",
            job_ids.join(", ")
        ),
        Some(runner_id.to_string()),
        Some(follow_commands),
    );
    error.details["active_job_ids"] = serde_json::json!(job_ids);
    error.details["force_command"] = serde_json::json!(format!(
        "homeboy runner refresh-homeboy {} --force --reconnect",
        shell_arg(runner_id)
    ));
    error
}

fn protect_active_jobs_before_reconnect(
    runner_id: &str,
    active_jobs: &[homeboy_core::api_jobs::ActiveRunnerJobSummary],
    force: bool,
) -> Result<Vec<String>> {
    let job_ids = active_jobs
        .into_iter()
        .map(|job| job.job_id.clone())
        .collect::<Vec<_>>();
    if !job_ids.is_empty() && !force {
        return Err(active_job_reconnect_error(runner_id, &job_ids));
    }
    Ok(job_ids)
}

fn active_job_followups(runner_id: &str, job_ids: &[String]) -> Vec<String> {
    job_ids
        .iter()
        .map(|job_id| {
            format!(
                "homeboy runner job logs {} {} --follow",
                shell_arg(runner_id),
                shell_arg(job_id)
            )
        })
        .chain(std::iter::once(format!(
            "homeboy runner refresh-homeboy {} --reconnect",
            shell_arg(runner_id)
        )))
        .collect()
}

fn non_empty<'a>(name: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            name,
            format!("{name} must not be empty"),
            None,
            None,
        ));
    }
    Ok(trimmed)
}

fn sanitize_ref(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
        .collect();
    let sanitized = sanitized.trim_matches('-');
    if sanitized.is_empty() {
        "main".to_string()
    } else {
        sanitized.to_string()
    }
}

fn shell_arg(value: &str) -> String {
    quote_arg(value)
}

#[cfg(test)]
mod tests;
