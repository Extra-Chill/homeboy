use std::collections::HashMap;

use serde_json::Value;

use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::server::{RunnerPolicy, RunnerSettings};
use homeboy::core::MergeOutput;
use homeboy::runner::runners::{self as runner, ReverseRunnerConnectOptions, Runner};
use homeboy_runner_contract::RunnerKind;

use super::super::output_runtime::{CommandPresentation, CommandRun};
use super::super::{CmdResult, DynamicSetArgs};
use super::cli::RunnerKindArg;
use super::types::{
    RunnerConnectionOutput, RunnerDisconnectStatus, RunnerExtra, RunnerInventoryConcurrency,
    RunnerInventoryEvidence, RunnerInventorySummary, RunnerListOutput, RunnerListTruncation,
    RunnerOutput, REDACTED_ENV_VALUE,
};

pub(super) struct RunnerAddInput {
    pub(super) json: Option<String>,
    pub(super) skip_existing: bool,
    pub(super) id: Option<String>,
    pub(super) kind: Option<RunnerKindArg>,
    pub(super) server: Option<String>,
    pub(super) workspace_root: Option<String>,
    pub(super) settings: RunnerSettings,
}

pub(super) fn add(input: RunnerAddInput) -> CmdResult<RunnerOutput> {
    let json_spec = if let Some(spec) = input.json {
        spec
    } else {
        let id = input.id.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "id",
                "Missing required argument: id",
                None,
                None,
            )
        })?;
        let kind = input.kind.map(RunnerKind::from).unwrap_or_else(|| {
            if input.server.is_some() {
                RunnerKind::Ssh
            } else {
                RunnerKind::Local
            }
        });
        let new_runner = Runner {
            id,
            kind,
            server_id: input.server,
            workspace_root: input.workspace_root,
            settings: input.settings,
            env: HashMap::new(),
            secret_env: HashMap::new(),
            resources: HashMap::<String, Value>::new(),
            policy: RunnerPolicy::default(),
        };

        homeboy::core::config::to_json_string(&new_runner)?
    };

    match runner::create(&json_spec, input.skip_existing)? {
        homeboy::core::CreateOutput::Single(result) => Ok((
            RunnerOutput {
                command: "runner.add".to_string(),
                id: Some(result.id),
                entity: Some(result.entity),
                updated_fields: vec!["created".to_string()],
                ..Default::default()
            },
            0,
        )),
        homeboy::core::CreateOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                RunnerOutput {
                    command: "runner.add".to_string(),
                    import: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

const RUNNER_LIST_LIMIT: usize = 10;
const RUNNER_LIST_TEXT_LIMIT: usize = 256;
const RUNNER_LIST_PROJECTION_BYTES: usize = 12 * 1024;

pub(super) fn list(full: bool) -> CmdResult<RunnerListOutput> {
    let sessions = runner::statuses()?;

    if full {
        return Ok((full_list_output(runner::list()?, sessions), 0));
    }

    let descriptors = runner::RunnerDiscoveryService::list()?;
    Ok((
        bounded_list_output(compact_list_output(&descriptors, &sessions)),
        0,
    ))
}

fn full_list_output(
    mut entities: Vec<Runner>,
    sessions: Vec<runner::RunnerStatusReport>,
) -> RunnerListOutput {
    for runner in &mut entities {
        redact_runner_env(runner);
    }
    RunnerListOutput {
        command: "runner.list",
        variant: "list",
        runner_summaries: Vec::new(),
        entities,
        sessions,
        truncation: None,
    }
}

fn compact_list_output(
    descriptors: &[runner::RunnerDescriptor],
    sessions: &[runner::RunnerStatusReport],
) -> RunnerListOutput {
    let runner_summaries = descriptors
        .iter()
        .take(RUNNER_LIST_LIMIT)
        .map(|descriptor| {
            runner_inventory_summary(
                descriptor,
                sessions
                    .iter()
                    .find(|status| status.runner_id == descriptor.runner_id),
            )
        })
        .collect::<Vec<_>>();
    RunnerListOutput {
        command: "runner.list",
        variant: "list",
        truncation: Some(RunnerListTruncation {
            shown: runner_summaries.len(),
            omitted: descriptors.len().saturating_sub(runner_summaries.len()),
            evidence_ref: "runner:configured-inventory",
            full_command: "homeboy runner list --full",
        }),
        runner_summaries,
        entities: Vec::new(),
        sessions: Vec::new(),
    }
}

fn runner_inventory_summary(
    configured: &runner::RunnerDescriptor,
    status: Option<&runner::RunnerStatusReport>,
) -> RunnerInventorySummary {
    let identity = bounded_list_text(&configured.runner_id);
    let is_local = configured.kind == RunnerKind::Local;
    let operator = status.map(super::status::operator_summary);
    let admission = status.map(|status| {
        homeboy::runner::runner_admission_snapshot_for_status(status.clone())
            .map(|snapshot| snapshot.summary)
    });
    RunnerInventorySummary {
        identity: identity.clone(),
        kind: format!("{:?}", configured.kind).to_ascii_lowercase(),
        connection_state: if is_local {
            "not_applicable".to_string()
        } else {
            status
                .map(|status| format!("{:?}", status.state).to_ascii_lowercase())
                .unwrap_or_else(|| "disconnected".to_string())
        },
        admission_state: if is_local {
            "local".to_string()
        } else if let Some(admission) = admission {
            match admission {
                Ok(summary) if summary.accepting_jobs => "accepting".to_string(),
                Ok(_) => "blocked".to_string(),
                Err(_) => "unknown".to_string(),
            }
        } else {
            "blocked".to_string()
        },
        concurrency: RunnerInventoryConcurrency {
            active: status.and_then(|status| {
                (status.active_job_state == runner::RunnerActiveJobState::Available)
                    .then_some(status.active_job_count)
            }),
            limit: configured.concurrency_limit,
        },
        drift: if is_local {
            "not_applicable"
        } else {
            match status {
                Some(status) => match status.stale_daemon.as_ref() {
                    Some(warning) if warning.is_unverified() => "unverified",
                    Some(_) => "detected",
                    None if status
                        .daemon_freshness
                        .as_ref()
                        .is_some_and(|report| report.fresh) =>
                    {
                        "none"
                    }
                    None => "unverified",
                },
                None => "unverified",
            }
        }
        .to_string(),
        next_action: operator
            .as_ref()
            .map(|summary| summary.next_action.clone())
            .unwrap_or_else(|| {
                if is_local {
                    format!("homeboy runner show {}", shell_arg(&configured.runner_id))
                } else {
                    "homeboy runner status --full".to_string()
                }
            }),
        evidence: RunnerInventoryEvidence {
            environment_ref: format!("runner:{}:environment", configured.runner_id),
            environment_command: format!("homeboy runner env {}", shell_arg(&configured.runner_id)),
            full_ref: format!("runner:{}:configuration", configured.runner_id),
            full_command: "homeboy runner list --full",
        },
    }
}

fn bounded_list_output(output: RunnerListOutput) -> RunnerListOutput {
    if list_envelope_bytes(&output).is_ok_and(|bytes| bytes <= RUNNER_LIST_PROJECTION_BYTES) {
        return output;
    }
    let omitted = output
        .truncation
        .as_ref()
        .map_or(0, |truncation| truncation.shown + truncation.omitted);
    RunnerListOutput {
        command: "runner.list",
        variant: "list",
        runner_summaries: Vec::new(),
        entities: Vec::new(),
        sessions: Vec::new(),
        truncation: Some(RunnerListTruncation {
            shown: 0,
            omitted,
            evidence_ref: "runner:configured-inventory",
            full_command: "homeboy runner list --full",
        }),
    }
}

fn bounded_list_text(value: &str) -> String {
    let mut chars = value.chars();
    let bounded = chars
        .by_ref()
        .take(RUNNER_LIST_TEXT_LIMIT)
        .collect::<String>();
    if chars.next().is_some() {
        format!("{bounded}...")
    } else {
        bounded
    }
}

fn shell_arg(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\\''"))
    }
}

pub(crate) fn compact_list_command_run(
    stdout_result: homeboy::core::Result<Value>,
    exit_code: i32,
) -> CommandRun {
    let summary = stdout_result.as_ref().ok().and_then(render_list_summary);
    CommandRun::from_stdout_result(stdout_result, exit_code).with_presentation(
        CommandPresentation {
            stdout: summary,
            stderr: None,
        },
    )
}

fn list_envelope_bytes(output: &RunnerListOutput) -> homeboy::core::Result<usize> {
    let data = serde_json::to_value(output)?;
    let run = compact_list_command_run(Ok(data), 0).with_identity(
        &crate::commands::utils::response::CommandIdentity::with_operation("runner", "list"),
    );
    run.stdout_bytes()
}

pub(crate) fn render_list_summary(payload: &Value) -> Option<String> {
    let summaries = payload.get("runner_summaries").and_then(Value::as_array);
    let mut rendered = format!(
        "Runner summaries\nRunners shown: {}",
        summaries.map_or(0, Vec::len)
    );
    for summary in summaries.into_iter().flatten() {
        rendered.push_str(&format!(
            "\n{} | {} | connection={} | admission={} | concurrency={}/{} | drift={}\nNext: {}",
            summary.get("identity")?.as_str()?,
            summary.get("kind")?.as_str()?,
            summary.get("connection_state")?.as_str()?,
            summary.get("admission_state")?.as_str()?,
            summary
                .get("concurrency")?
                .get("active")
                .and_then(Value::as_u64)
                .map_or_else(|| "unknown".to_string(), |active| active.to_string()),
            summary
                .get("concurrency")?
                .get("limit")
                .and_then(Value::as_u64)
                .map_or_else(|| "default".to_string(), |limit| limit.to_string()),
            summary.get("drift")?.as_str()?,
            summary.get("next_action")?.as_str()?,
        ));
    }
    Some(rendered)
}

pub(super) fn enable(
    server_id: &str,
    workspace_root: Option<String>,
    settings: RunnerSettings,
) -> CmdResult<RunnerOutput> {
    let mut spec = serde_json::Map::new();
    if let Some(workspace_root) = workspace_root {
        spec.insert("workspace_root".to_string(), workspace_root.into());
    }
    if let Some(homeboy_path) = settings.homeboy_path {
        spec.insert("homeboy_path".to_string(), homeboy_path.into());
    }
    if settings.daemon {
        spec.insert("daemon".to_string(), true.into());
    }
    if let Some(concurrency_limit) = settings.concurrency_limit {
        spec.insert("concurrency_limit".to_string(), concurrency_limit.into());
    }
    if let Some(artifact_policy) = settings.artifact_policy {
        spec.insert("artifact_policy".to_string(), artifact_policy.into());
    }
    let runner = runner::enable_server_runner(server_id, Value::Object(spec))?;
    Ok((
        RunnerOutput {
            command: "runner.enable".to_string(),
            id: Some(runner.id.clone()),
            entity: Some(runner),
            updated_fields: vec!["runner".to_string()],
            ..Default::default()
        },
        0,
    ))
}

pub(super) fn show(id: &str) -> CmdResult<RunnerOutput> {
    let runner = runner::load(id)?;
    Ok((
        RunnerOutput {
            command: "runner.show".to_string(),
            id: Some(runner.id.clone()),
            entity: Some(runner),
            ..Default::default()
        },
        0,
    ))
}

pub(super) fn set(args: DynamicSetArgs) -> CmdResult<RunnerOutput> {
    let merged = super::super::merge_dynamic_args(&args)?.ok_or_else(|| {
        homeboy::core::Error::validation_invalid_argument(
            "spec",
            "Provide --json '<object>' or --base64 <encoded-json>",
            None,
            Some(vec![
                "Arbitrary runner updates must use explicit JSON input.".to_string(),
                "Example: homeboy runner set <id> --json '{\"workspace_root\":\"/srv/homeboy\"}'"
                    .to_string(),
            ]),
        )
    })?;
    let (json_string, replace_fields) = super::super::finalize_set_spec(&merged, &args.replace)?;

    match runner::merge(args.id.as_deref(), &json_string, &replace_fields)? {
        MergeOutput::Single(result) => {
            let entity = runner::load(&result.id)?;
            Ok((
                RunnerOutput {
                    command: "runner.set".to_string(),
                    id: Some(result.id),
                    entity: Some(entity),
                    updated_fields: result.updated_fields,
                    ..Default::default()
                },
                0,
            ))
        }
        MergeOutput::Bulk(summary) => {
            let exit_code = summary.exit_code();
            Ok((
                RunnerOutput {
                    command: "runner.set".to_string(),
                    batch: Some(summary),
                    ..Default::default()
                },
                exit_code,
            ))
        }
    }
}

pub(super) fn remove(id: &str) -> CmdResult<RunnerOutput> {
    runner::delete_safe(id)?;
    Ok((
        RunnerOutput {
            command: "runner.remove".to_string(),
            id: Some(id.to_string()),
            deleted: vec![id.to_string()],
            ..Default::default()
        },
        0,
    ))
}

pub(super) struct RunnerConnectInput {
    pub(super) reverse: bool,
    pub(super) runner_id: Option<String>,
    pub(super) broker_url: Option<String>,
    pub(super) adopt_orphan_lease: Option<String>,
    pub(super) adopt_live_lease: Option<String>,
    pub(super) expected_live_pid: Option<u32>,
    pub(super) confirm_untracked_child_dead: Vec<uuid::Uuid>,
    pub(super) reconcile_leaseless_orphans: bool,
    pub(super) reconcile_unleased_candidates: bool,
    pub(super) recover_missing_lease_state: Option<String>,
    pub(super) recorded_pid: Option<u32>,
    pub(super) recorded_endpoint: Option<String>,
}

/// Argument-shape validation for `runner connect`, split out so it can be
/// exercised without attempting an SSH connection.
pub(super) fn validate_connect_input(input: &RunnerConnectInput) -> homeboy::core::Result<()> {
    let RunnerConnectInput {
        reverse,
        runner_id: _,
        broker_url: _,
        adopt_orphan_lease,
        adopt_live_lease,
        expected_live_pid,
        confirm_untracked_child_dead,
        reconcile_leaseless_orphans,
        reconcile_unleased_candidates,
        recover_missing_lease_state,
        recorded_pid,
        recorded_endpoint,
    } = input;
    if adopt_live_lease.is_some() != expected_live_pid.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "adopt_live_lease",
            "--adopt-live-lease requires --expected-live-pid, and vice versa",
            None,
            None,
        ));
    }
    if *reverse && adopt_live_lease.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "adopt_live_lease",
            "live daemon adoption only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
    if !confirm_untracked_child_dead.is_empty() && adopt_orphan_lease.is_none() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "confirm_untracked_child_dead",
            "--confirm-untracked-child-dead requires --adopt-orphan-lease",
            None,
            None,
        ));
    }
    if *reverse && adopt_orphan_lease.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "adopt_orphan_lease",
            "orphan daemon adoption only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
    if *reverse && *reconcile_leaseless_orphans {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "lease-less recovery only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
    if *reverse && *reconcile_unleased_candidates {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "reconcile_unleased_candidates",
            "unleased candidate reconciliation only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
    let recovery_mode_count = usize::from(adopt_orphan_lease.is_some())
        + usize::from(*reconcile_leaseless_orphans)
        + usize::from(*reconcile_unleased_candidates)
        + usize::from(recover_missing_lease_state.is_some())
        + usize::from(adopt_live_lease.is_some());
    if recovery_mode_count > 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "recovery_mode",
            "--adopt-orphan-lease, --adopt-live-lease, --reconcile-leaseless-orphans, --reconcile-unleased-candidates, and --recover-missing-lease-state are mutually exclusive",
            None,
            None,
        ));
    }
    // `--recorded-pid` and `--recorded-endpoint` are evidence the runner cannot
    // reconstruct once its state record is gone, so they remain required.
    if recover_missing_lease_state.is_some()
        && (recorded_pid.is_none() || recorded_endpoint.is_none())
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "recover_missing_lease_state",
            "--recover-missing-lease-state requires --recorded-pid and --recorded-endpoint",
            None,
            None,
        ));
    }
    if *reverse && recover_missing_lease_state.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "recover_missing_lease_state",
            "state-loss recovery only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
    Ok(())
}

pub(super) fn connect(id: &str, input: RunnerConnectInput) -> CmdResult<RunnerOutput> {
    validate_connect_input(&input)?;
    let RunnerConnectInput {
        reverse,
        runner_id,
        broker_url,
        adopt_orphan_lease,
        adopt_live_lease,
        expected_live_pid,
        confirm_untracked_child_dead,
        reconcile_leaseless_orphans,
        reconcile_unleased_candidates,
        recover_missing_lease_state,
        recorded_pid,
        recorded_endpoint,
    } = input;
    let (report, exit_code) = if reverse {
        let runner_id = runner_id.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "runner",
                "Provide --reverse-runner <runner-id> when using --reverse",
                None,
                None,
            )
        })?;
        runner::connect_reverse(ReverseRunnerConnectOptions {
            controller_id: id.to_string(),
            runner_id,
            broker_url,
        })?
    } else {
        match (adopt_live_lease.as_deref(), expected_live_pid) {
            (Some(lease_id), Some(pid)) => {
                runner::connect_with_live_lease_adoption(id, lease_id, pid)?
            }
            _ if reconcile_unleased_candidates => {
                runner::connect_with_unleased_candidate_reconciliation(id)?
            }
            _ => runner::connect_with_orphan_adoption(
                id,
                adopt_orphan_lease.as_deref(),
                &confirm_untracked_child_dead,
                reconcile_leaseless_orphans,
                recover_missing_lease_state.as_deref(),
                recorded_pid,
                recorded_endpoint.as_deref(),
            )?,
        }
    };
    Ok((
        RunnerOutput {
            command: "runner.connect".to_string(),
            id: Some(report.runner_id.clone()),
            extra: RunnerExtra {
                connection: Some(RunnerConnectionOutput::Connect(Box::new(report))),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

pub(super) fn disconnect(id: &str, local_recovery: bool) -> CmdResult<RunnerOutput> {
    let report = if local_recovery {
        runner::disconnect_local_recovery(id)?
    } else {
        runner::disconnect(id)?
    };
    Ok(disconnect_output(id, report, local_recovery))
}

fn disconnect_output(
    id: &str,
    report: homeboy::runner::runners::RunnerDisconnectReport,
    local_recovery: bool,
) -> (RunnerOutput, i32) {
    let status = disconnect_status(&report, local_recovery);
    (
        RunnerOutput {
            command: "runner.disconnect".to_string(),
            id: Some(id.to_string()),
            extra: RunnerExtra {
                connection: Some(RunnerConnectionOutput::Disconnect(Box::new(report))),
                disconnect_status: Some(status),
                ..Default::default()
            },
            ..Default::default()
        },
        (status == RunnerDisconnectStatus::PartialFailure) as i32,
    )
}

fn disconnect_status(
    report: &homeboy::runner::runners::RunnerDisconnectReport,
    local_recovery: bool,
) -> RunnerDisconnectStatus {
    if local_recovery {
        if report.disconnected {
            RunnerDisconnectStatus::LocalRecovery
        } else {
            RunnerDisconnectStatus::AlreadyDisconnected
        }
    } else if report.partial && !report.disconnected {
        RunnerDisconnectStatus::PartialFailure
    } else if report.disconnected {
        RunnerDisconnectStatus::Disconnected
    } else {
        RunnerDisconnectStatus::AlreadyDisconnected
    }
}

pub(super) fn redact_runner_output_env(output: &mut RunnerOutput) {
    if let Some(runner) = output.entity.as_mut() {
        redact_runner_env(runner);
    }

    for runner in &mut output.entities {
        redact_runner_env(runner);
    }
}

fn redact_runner_env(runner: &mut Runner) {
    let policy = RedactionPolicy::default();
    for (key, value) in runner.env.iter_mut() {
        if policy.is_sensitive_key(key) {
            *value = REDACTED_ENV_VALUE.to_string();
        } else {
            *value = policy.redact_string(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::runner::runners::RunnerDisconnectReport;

    fn disconnect_report(
        disconnected: bool,
        partial: bool,
        local_recovery: bool,
    ) -> RunnerDisconnectReport {
        RunnerDisconnectReport {
            runner_id: "homeboy-lab".to_string(),
            disconnected,
            partial,
            remote_error: partial.then(|| {
                if local_recovery && !disconnected {
                    "no controller-local session was present; remote daemon was not contacted"
                        .to_string()
                } else {
                    "SSH timeout".to_string()
                }
            }),
            local_recovery_command: (partial && !disconnected && !local_recovery)
                .then(|| "homeboy runner disconnect homeboy-lab --local-recovery".to_string()),
            session: None,
            session_path: "/tmp/homeboy-lab.json".to_string(),
        }
    }

    #[test]
    fn disconnect_outcomes_keep_exit_status_and_typed_state_truthful() {
        for (name, disconnected, partial, local_recovery, expected_status, expected_exit) in [
            (
                "remote timeout",
                false,
                true,
                false,
                RunnerDisconnectStatus::PartialFailure,
                1,
            ),
            (
                "local recovery",
                true,
                true,
                true,
                RunnerDisconnectStatus::LocalRecovery,
                0,
            ),
            (
                "repeated local recovery",
                false,
                true,
                true,
                RunnerDisconnectStatus::AlreadyDisconnected,
                0,
            ),
            (
                "already disconnected",
                false,
                false,
                false,
                RunnerDisconnectStatus::AlreadyDisconnected,
                0,
            ),
            (
                "remote disconnect completed",
                true,
                false,
                false,
                RunnerDisconnectStatus::Disconnected,
                0,
            ),
        ] {
            let (output, exit_code) = disconnect_output(
                "homeboy-lab",
                disconnect_report(disconnected, partial, local_recovery),
                local_recovery,
            );
            let serialized = serde_json::to_value(output).expect("serialize disconnect output");
            let envelope =
                crate::commands::utils::response::cli_response_for_json_result_for_identity(
                    &Ok(serialized),
                    exit_code,
                    &crate::commands::utils::response::CommandIdentity::with_operation(
                        "runner",
                        "disconnect",
                    ),
                    None,
                );
            let envelope = serde_json::to_value(envelope).expect("serialize command envelope");

            assert_eq!(envelope["success"], expected_exit == 0, "{name}");
            assert_eq!(envelope["exit_code"], expected_exit, "{name}");
            assert_eq!(
                envelope["status"],
                if expected_exit == 0 {
                    serde_json::json!("succeeded")
                } else {
                    serde_json::to_value(expected_status).expect("serialize expected status")
                },
                "{name}"
            );
            assert_eq!(
                envelope["data"]["status"],
                serde_json::to_value(expected_status).expect("serialize expected status"),
                "{name}"
            );
            if name == "remote timeout" {
                assert_eq!(envelope["data"]["connection"]["action"], "disconnect");
                assert_eq!(
                    envelope["data"]["connection"]["local_recovery_command"],
                    "homeboy runner disconnect homeboy-lab --local-recovery"
                );
            } else if name == "repeated local recovery" {
                assert_eq!(
                    envelope["data"]["connection"]["local_recovery_command"],
                    serde_json::Value::Null
                );
            }
        }
    }

    /// `runner list` must default to the compact view and keep the diagnostic
    /// mass behind `--full` (#9487).
    mod list_output_shape {
        use std::collections::HashMap;

        use super::super::*;
        use crate::cli_surface::Cli;
        use clap::Parser;

        fn configured_runner(id: &str) -> Runner {
            Runner {
                id: id.to_string(),
                kind: RunnerKind::Local,
                server_id: None,
                workspace_root: None,
                settings: RunnerSettings::default(),
                env: (0..50)
                    .map(|index| (format!("ENV_{index}"), "value".repeat(50)))
                    .chain(std::iter::once((
                        "PATH".to_string(),
                        "/long/path".repeat(5_000),
                    )))
                    .collect(),
                secret_env: HashMap::new(),
                resources: HashMap::new(),
                policy: RunnerPolicy::default(),
            }
        }

        fn ssh_status(
            id: &str,
            active_job_state: runner::RunnerActiveJobState,
        ) -> runner::RunnerStatusReport {
            runner::RunnerStatusReport {
                runner_id: id.to_string(),
                connected: true,
                state: runner::RunnerSessionState::Connected,
                session: None,
                stale_daemon: None,
                configured_job_binary_build_identity: None,
                daemon_freshness: None,
                active_jobs: Vec::new(),
                active_runner_jobs: Vec::new(),
                stale_runner_jobs: Vec::new(),
                active_job_count: 0,
                stale_runner_job_count: 0,
                active_job_state,
                active_job_source: None,
                active_job_error: None,
                active_job_recovery_evidence: None,
                session_path: "test".to_string(),
            }
        }

        fn descriptor(configured: &Runner) -> runner::RunnerDescriptor {
            runner::RunnerDescriptor {
                schema: runner::RUNNER_DESCRIPTOR_SCHEMA.to_string(),
                runner_id: configured.id.clone(),
                kind: configured.kind.clone(),
                server_id: configured.server_id.clone(),
                workspace_root: configured.workspace_root.clone(),
                concurrency_limit: configured.settings.concurrency_limit,
            }
        }

        fn parse(args: &[&str]) -> bool {
            let cli = Cli::try_parse_from(args).expect("parse runner list");
            let crate::cli_surface::Commands::Runner(runner) = cli.command else {
                panic!("expected a runner command");
            };
            match runner.command {
                crate::commands::runner::cli::RunnerCommand::List { full } => full,
                _ => panic!("expected a runner list command"),
            }
        }

        #[test]
        fn list_defaults_to_the_compact_view() {
            assert!(
                !parse(&["homeboy", "runner", "list"]),
                "the default must be compact; a long-lived controller's full \
                 listing truncates in agent and terminal output"
            );
        }

        #[test]
        fn full_is_available_explicitly() {
            assert!(parse(&["homeboy", "runner", "list", "--full"]));
        }

        #[test]
        fn default_projection_omits_environment_and_bounds_the_real_wire_envelope() {
            let runners = (0..25)
                .map(|index| descriptor(&configured_runner(&format!("lab-{index}"))))
                .collect::<Vec<_>>();
            let output = bounded_list_output(compact_list_output(&runners, &[]));
            let data = serde_json::to_value(super::super::super::types::RunnerCommandOutput::List(
                Box::new(output),
            ))
            .expect("list output serializes");
            let run = compact_list_command_run(Ok(data), 0).with_identity(
                &crate::commands::utils::response::CommandIdentity::with_operation(
                    "runner", "list",
                ),
            );
            let wire = run.stdout_envelope();
            let wire_json = serde_json::to_value(&wire).expect("wire serializes");

            assert!(wire.stdout_json().unwrap().len() <= RUNNER_LIST_PROJECTION_BYTES);
            assert!(wire_json["data"].get("entities").is_none());
            assert!(wire_json["data"].get("sessions").is_none());
            assert!(!wire_json["data"].to_string().contains("ENV_0"));
            assert_eq!(
                wire_json["data"]["runner_summaries"]
                    .as_array()
                    .unwrap()
                    .len(),
                10
            );
            assert_eq!(wire_json["data"]["truncation"]["shown"], 10);
            assert_eq!(wire_json["data"]["truncation"]["omitted"], 15);
            assert_eq!(
                wire_json["data"]["truncation"]["full_command"],
                "homeboy runner list --full"
            );
            assert!(wire_json["presentation"]["stdout"]
                .as_str()
                .unwrap()
                .starts_with("Runner summaries"));
        }

        #[test]
        fn projection_keeps_exact_quoted_environment_followup_or_falls_back() {
            let id = "lab 'quoted' \\ target";
            let runners = vec![descriptor(&configured_runner(id))];
            let output = bounded_list_output(compact_list_output(&runners, &[]));
            let summary = &output.runner_summaries[0];

            assert_eq!(summary.identity, id);
            assert_eq!(
                summary.evidence.environment_ref,
                format!("runner:{id}:environment")
            );
            assert_eq!(
                summary.evidence.environment_command,
                "homeboy runner env 'lab '\\''quoted'\\'' \\ target'"
            );
            assert_eq!(
                summary.next_action,
                "homeboy runner show 'lab '\\''quoted'\\'' \\ target'"
            );
        }

        #[test]
        fn oversized_exact_followups_trigger_the_bounded_fallback() {
            let runners = vec![descriptor(&configured_runner(&"\"\\\n".repeat(10_000)))];
            let output = bounded_list_output(compact_list_output(&runners, &[]));

            assert!(output.runner_summaries.is_empty());
            assert_eq!(output.truncation.as_ref().unwrap().shown, 0);
            assert_eq!(output.truncation.as_ref().unwrap().omitted, 1);
            assert!(list_envelope_bytes(&output).unwrap() <= RUNNER_LIST_PROJECTION_BYTES);
        }

        #[test]
        fn ssh_projection_does_not_turn_missing_observations_into_healthy_zeroes() {
            let mut configured = configured_runner("lab");
            configured.kind = RunnerKind::Ssh;
            let status = ssh_status("lab", runner::RunnerActiveJobState::Unavailable);
            let output = compact_list_output(&[descriptor(&configured)], &[status]);
            let value =
                serde_json::to_value(&output.runner_summaries[0]).expect("summary serializes");

            assert_eq!(value["connection_state"], "connected");
            assert_eq!(value["admission_state"], "blocked");
            assert!(value["concurrency"]["active"].is_null());
            assert_eq!(value["drift"], "unverified");
        }

        #[test]
        fn full_list_shape_preserves_configuration_except_sensitive_values() {
            let mut configured = configured_runner("lab");
            configured
                .env
                .insert("OPENCODE_API_KEY".to_string(), "secret-token".to_string());
            configured
                .env
                .insert("PUBLIC_SETTING".to_string(), "kept".to_string());
            let output = full_list_output(vec![configured], Vec::new());
            let value = serde_json::to_value(output).expect("full list serializes");

            assert_eq!(
                value["entities"][0]["env"]["OPENCODE_API_KEY"],
                REDACTED_ENV_VALUE
            );
            assert_eq!(value["entities"][0]["env"]["PUBLIC_SETTING"], "kept");
            assert_eq!(
                value["entities"][0]["env"]["PATH"],
                "/long/path".repeat(5_000)
            );
            assert!(value.get("runner_summaries").is_none());
            assert!(value.get("truncation").is_none());
        }
    }

    // Argument validation is exercised through `validate_connect_input` rather
    // than `connect` so no test can fall through to a real SSH attempt.
    use super::{validate_connect_input, RunnerConnectInput};

    fn input() -> RunnerConnectInput {
        RunnerConnectInput {
            reverse: false,
            runner_id: None,
            broker_url: None,
            adopt_orphan_lease: None,
            adopt_live_lease: None,
            expected_live_pid: None,
            confirm_untracked_child_dead: Vec::new(),
            reconcile_leaseless_orphans: false,
            reconcile_unleased_candidates: false,
            recover_missing_lease_state: None,
            recorded_pid: None,
            recorded_endpoint: None,
        }
    }

    #[test]
    fn recovery_modes_validate_with_their_required_evidence() {
        for input in [
            RunnerConnectInput {
                adopt_orphan_lease: Some("lease-dead".to_string()),
                ..input()
            },
            RunnerConnectInput {
                reconcile_leaseless_orphans: true,
                ..input()
            },
            RunnerConnectInput {
                recover_missing_lease_state: Some("lease".to_string()),
                recorded_pid: Some(42),
                recorded_endpoint: Some("127.0.0.1:7421".to_string()),
                ..input()
            },
        ] {
            validate_connect_input(&input).expect("valid recovery mode");
        }
    }

    #[test]
    fn untracked_child_confirmation_requires_exact_orphan_lease_mode() {
        let error = validate_connect_input(&RunnerConnectInput {
            confirm_untracked_child_dead: vec![uuid::Uuid::new_v4()],
            ..input()
        })
        .expect_err("untracked child confirmation is only valid for exact adoption");
        assert!(error.message.contains("requires --adopt-orphan-lease"));
    }

    #[test]
    fn reverse_connections_cannot_reconcile_leaseless_jobs() {
        let error = validate_connect_input(&RunnerConnectInput {
            reverse: true,
            reconcile_leaseless_orphans: true,
            ..input()
        })
        .expect_err("reverse recovery is unsupported");
        assert!(error.message.contains("direct SSH"));
    }

    #[test]
    fn candidate_reconciliation_is_a_direct_ssh_recovery_mode() {
        validate_connect_input(&RunnerConnectInput {
            reconcile_unleased_candidates: true,
            ..input()
        })
        .expect("candidate reconciliation selects one direct SSH recovery mode");

        let error = validate_connect_input(&RunnerConnectInput {
            reverse: true,
            reconcile_unleased_candidates: true,
            ..input()
        })
        .expect_err("reverse candidate recovery has no direct remote lifecycle boundary");
        assert!(error.message.contains("direct SSH"));
    }

    #[test]
    fn reverse_connections_cannot_adopt_live_daemons() {
        let error = validate_connect_input(&RunnerConnectInput {
            reverse: true,
            adopt_live_lease: Some("lease-live".to_string()),
            expected_live_pid: Some(42),
            ..input()
        })
        .expect_err("reverse connections have no direct SSH daemon ownership");
        assert!(error.message.contains("direct SSH"));
    }

    #[test]
    fn state_loss_recovery_still_requires_unreconstructable_evidence() {
        // Once the state record is gone the runner cannot recompute these.
        let error = validate_connect_input(&RunnerConnectInput {
            recover_missing_lease_state: Some("lease".to_string()),
            ..input()
        })
        .expect_err("partial state-loss evidence must fail before connecting");
        assert!(error.message.contains("--recorded-pid"));
    }

    #[test]
    fn recovery_modes_are_mutually_exclusive_before_connecting() {
        let conflicting_inputs = [
            RunnerConnectInput {
                adopt_orphan_lease: Some("lease".to_string()),
                reconcile_leaseless_orphans: true,
                ..input()
            },
            RunnerConnectInput {
                adopt_orphan_lease: Some("lease".to_string()),
                recover_missing_lease_state: Some("lease".to_string()),
                recorded_pid: Some(42),
                recorded_endpoint: Some("127.0.0.1:7421".to_string()),
                ..input()
            },
            RunnerConnectInput {
                reconcile_leaseless_orphans: true,
                recover_missing_lease_state: Some("lease".to_string()),
                recorded_pid: Some(42),
                recorded_endpoint: Some("127.0.0.1:7421".to_string()),
                ..input()
            },
        ];
        for input in conflicting_inputs {
            let error = validate_connect_input(&input)
                .expect_err("multiple recovery modes must fail before SSH");
            assert!(error.message.contains("mutually exclusive"));
        }
    }
}
