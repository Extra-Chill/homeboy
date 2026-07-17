use std::collections::HashMap;

use serde_json::Value;

use homeboy::core::redaction::RedactionPolicy;
use homeboy::core::server::{RunnerPolicy, RunnerSettings};
use homeboy::core::MergeOutput;
use homeboy::runner::runners::{self as runner, ReverseRunnerConnectOptions, Runner, RunnerKind};

use super::super::{CmdResult, DynamicSetArgs};
use super::cli::RunnerKindArg;
use super::types::{RunnerConnectionOutput, RunnerExtra, RunnerOutput, REDACTED_ENV_VALUE};

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

pub(super) fn list() -> CmdResult<RunnerOutput> {
    Ok((
        RunnerOutput {
            command: "runner.list".to_string(),
            entities: runner::list()?,
            extra: RunnerExtra {
                sessions: runner::statuses()?,
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
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
    pub(super) confirm_pid_dead: bool,
    pub(super) confirm_untracked_child_dead: Vec<uuid::Uuid>,
    pub(super) reconcile_leaseless_orphans: bool,
    pub(super) confirm_no_daemon_owner: bool,
    pub(super) recover_missing_lease_state: Option<String>,
    pub(super) recorded_pid: Option<u32>,
    pub(super) recorded_endpoint: Option<String>,
    pub(super) confirm_control_plane_lost: bool,
}

pub(super) fn connect(id: &str, input: RunnerConnectInput) -> CmdResult<RunnerOutput> {
    let RunnerConnectInput {
        reverse,
        runner_id,
        broker_url,
        adopt_orphan_lease,
        confirm_pid_dead,
        confirm_untracked_child_dead,
        reconcile_leaseless_orphans,
        confirm_no_daemon_owner,
        recover_missing_lease_state,
        recorded_pid,
        recorded_endpoint,
        confirm_control_plane_lost,
    } = input;
    if adopt_orphan_lease.is_some() && !confirm_pid_dead {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "confirm_pid_dead",
            "--adopt-orphan-lease requires --confirm-pid-dead",
            None,
            Some(vec!["Inspect `homeboy daemon status` on the runner before adopting its exact dead lease.".to_string()]),
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
    if reverse && adopt_orphan_lease.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "adopt_orphan_lease",
            "orphan daemon adoption only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
    if reconcile_leaseless_orphans != confirm_no_daemon_owner {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "--reconcile-leaseless-orphans requires --confirm-no-daemon-owner, and vice versa",
            None,
            None,
        ));
    }
    if reverse && reconcile_leaseless_orphans {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "reconcile_leaseless_orphans",
            "lease-less recovery only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
    let recovery_mode_count = usize::from(adopt_orphan_lease.is_some())
        + usize::from(reconcile_leaseless_orphans)
        + usize::from(recover_missing_lease_state.is_some());
    if recovery_mode_count > 1 {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "recovery_mode",
            "--adopt-orphan-lease, --reconcile-leaseless-orphans, and --recover-missing-lease-state are mutually exclusive",
            None,
            None,
        ));
    }
    if recover_missing_lease_state.is_some()
        && (!confirm_pid_dead
            || !confirm_control_plane_lost
            || recorded_pid.is_none()
            || recorded_endpoint.is_none())
    {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "recover_missing_lease_state",
            "--recover-missing-lease-state requires --recorded-pid, --recorded-endpoint, --confirm-pid-dead, and --confirm-control-plane-lost",
            None,
            None,
        ));
    }
    if reverse && recover_missing_lease_state.is_some() {
        return Err(homeboy::core::Error::validation_invalid_argument(
            "recover_missing_lease_state",
            "state-loss recovery only applies to direct SSH runner connections",
            None,
            None,
        ));
    }
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
        runner::connect_with_orphan_adoption(
            id,
            adopt_orphan_lease.as_deref(),
            &confirm_untracked_child_dead,
            reconcile_leaseless_orphans,
            recover_missing_lease_state.as_deref(),
            recorded_pid,
            recorded_endpoint.as_deref(),
        )?
    };
    Ok((
        RunnerOutput {
            command: "runner.connect".to_string(),
            id: Some(report.runner_id.clone()),
            extra: RunnerExtra {
                connection: Some(RunnerConnectionOutput::Connect(report)),
                ..Default::default()
            },
            ..Default::default()
        },
        exit_code,
    ))
}

pub(super) fn disconnect(id: &str) -> CmdResult<RunnerOutput> {
    Ok((
        RunnerOutput {
            command: "runner.disconnect".to_string(),
            id: Some(id.to_string()),
            extra: RunnerExtra {
                connection: Some(RunnerConnectionOutput::Disconnect(runner::disconnect(id)?)),
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

pub(super) fn redact_runner_output_env(output: &mut RunnerOutput) {
    if let Some(runner) = output.entity.as_mut() {
        redact_runner_env(runner);
    }

    for runner in &mut output.entities {
        redact_runner_env(runner);
    }
}

#[cfg(test)]
mod tests {
    use super::{connect, RunnerConnectInput};

    fn input() -> RunnerConnectInput {
        RunnerConnectInput {
            reverse: false,
            runner_id: None,
            broker_url: None,
            adopt_orphan_lease: None,
            confirm_pid_dead: false,
            confirm_untracked_child_dead: Vec::new(),
            reconcile_leaseless_orphans: false,
            confirm_no_daemon_owner: false,
            recover_missing_lease_state: None,
            recorded_pid: None,
            recorded_endpoint: None,
            confirm_control_plane_lost: false,
        }
    }

    #[test]
    fn leaseless_recovery_requires_both_affirmative_flags() {
        for (recover, confirm) in [(true, false), (false, true)] {
            let error = connect(
                "runner",
                RunnerConnectInput {
                    reconcile_leaseless_orphans: recover,
                    confirm_no_daemon_owner: confirm,
                    ..input()
                },
            )
            .expect_err("partial confirmation must fail before connecting");
            assert!(error
                .message
                .contains("--reconcile-leaseless-orphans requires"));
        }
    }

    #[test]
    fn untracked_child_confirmation_requires_exact_orphan_lease_mode() {
        let error = connect(
            "runner",
            RunnerConnectInput {
                confirm_untracked_child_dead: vec![uuid::Uuid::new_v4()],
                ..input()
            },
        )
        .expect_err("untracked child confirmation is only valid for exact adoption");
        assert!(error.message.contains("requires --adopt-orphan-lease"));
    }

    #[test]
    fn reverse_connections_cannot_reconcile_leaseless_jobs() {
        let error = connect(
            "runner",
            RunnerConnectInput {
                reverse: true,
                reconcile_leaseless_orphans: true,
                confirm_no_daemon_owner: true,
                ..input()
            },
        )
        .expect_err("reverse recovery is unsupported");
        assert!(error.message.contains("direct SSH"));
    }

    #[test]
    fn state_loss_recovery_requires_exact_evidence_and_confirmations() {
        let error = connect(
            "runner",
            RunnerConnectInput {
                recover_missing_lease_state: Some("lease".to_string()),
                confirm_control_plane_lost: true,
                ..input()
            },
        )
        .expect_err("partial state-loss evidence must fail before connecting");
        assert!(error.message.contains("--recorded-pid"));
    }

    #[test]
    fn recovery_modes_are_mutually_exclusive_before_connecting() {
        let conflicting_inputs = [
            RunnerConnectInput {
                adopt_orphan_lease: Some("lease".to_string()),
                confirm_pid_dead: true,
                reconcile_leaseless_orphans: true,
                confirm_no_daemon_owner: true,
                ..input()
            },
            RunnerConnectInput {
                adopt_orphan_lease: Some("lease".to_string()),
                confirm_pid_dead: true,
                recover_missing_lease_state: Some("lease".to_string()),
                recorded_pid: Some(42),
                recorded_endpoint: Some("127.0.0.1:7421".to_string()),
                confirm_control_plane_lost: true,
                ..input()
            },
            RunnerConnectInput {
                reconcile_leaseless_orphans: true,
                confirm_no_daemon_owner: true,
                confirm_pid_dead: true,
                recover_missing_lease_state: Some("lease".to_string()),
                recorded_pid: Some(42),
                recorded_endpoint: Some("127.0.0.1:7421".to_string()),
                confirm_control_plane_lost: true,
                ..input()
            },
        ];
        for input in conflicting_inputs {
            let error =
                connect("runner", input).expect_err("multiple recovery modes must fail before SSH");
            assert!(error.message.contains("mutually exclusive"));
        }
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
