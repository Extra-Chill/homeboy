use std::collections::BTreeMap;

use homeboy::core::runner::RunnerActiveJobState;
use homeboy::core::runners::{self as runner, RunnerSession, RunnerStatusReport, RunnerTunnelMode};

use super::super::CmdResult;
use super::types::{
    LabFollowup, LabRunnerHomeboyOutput, LabSelectedRunnerOutput, RunnerArtifactFeatureDiagnostics,
    RunnerConnectionOutput, RunnerExtra, RunnerOperatorCommand, RunnerOutput,
    RunnerToolDiagnostics,
};

pub(super) fn status(id: Option<&str>) -> CmdResult<RunnerOutput> {
    let preferred_lab_runner = runner::resolve_default_lab_runner()?;
    if let Some(id) = id {
        let report = runner::status(id)?;
        let operator_hints = runner_status_operator_hints(&report);
        let operator_commands = runner_status_operator_commands(&report);
        let selected_lab_runner = selected_lab_runner_status(Some(id), Some(report.clone()))?;
        return Ok((
            RunnerOutput {
                command: "runner.status".to_string(),
                id: Some(id.to_string()),
                extra: RunnerExtra {
                    connection: Some(RunnerConnectionOutput::Status(report)),
                    preferred_lab_runner,
                    selected_lab_runner,
                    managed_followups: runner_followups(Some(id)),
                    operator_hints,
                    operator_commands,
                    ..Default::default()
                },
                ..Default::default()
            },
            0,
        ));
    }

    let sessions = runner::statuses()?;
    let operator_hints = sessions
        .iter()
        .flat_map(runner_status_operator_hints)
        .collect();
    let operator_commands = sessions
        .iter()
        .flat_map(runner_status_operator_commands)
        .collect();
    let selected_lab_runner = selected_lab_runner_status(preferred_lab_runner.as_deref(), None)?;
    let managed_followups = runner_followups(preferred_lab_runner.as_deref());
    Ok((
        RunnerOutput {
            command: "runner.status".to_string(),
            extra: RunnerExtra {
                sessions,
                preferred_lab_runner,
                selected_lab_runner,
                managed_followups,
                operator_hints,
                operator_commands,
                ..Default::default()
            },
            ..Default::default()
        },
        0,
    ))
}

fn selected_lab_runner_status(
    runner_id: Option<&str>,
    status: Option<RunnerStatusReport>,
) -> homeboy::core::Result<Option<LabSelectedRunnerOutput>> {
    let Some(runner_id) = runner_id else {
        return Ok(None);
    };
    let runner_config = runner::load(runner_id)?;
    let status = match status {
        Some(status) => status,
        None => runner::status(runner_id)?,
    };
    let configured_executable = runner_config
        .settings
        .homeboy_path
        .clone()
        .unwrap_or_else(|| "homeboy".to_string());
    Ok(Some(LabSelectedRunnerOutput {
        runner_id: runner_id.to_string(),
        kind: format!("{:?}", runner_config.kind).to_ascii_lowercase(),
        configured_executable: configured_executable.clone(),
        runner_homeboy: lab_runner_homeboy_output(runner_id, &configured_executable, &status),
        daemon_enabled: runner_config.settings.daemon,
        workspace_root: runner_config.workspace_root.clone(),
        readiness_state: format!("{:?}", status.state).to_ascii_lowercase(),
        connected: status.connected,
        status,
    }))
}

fn lab_runner_homeboy_output(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
) -> LabRunnerHomeboyOutput {
    let controller_version = env!("CARGO_PKG_VERSION").to_string();
    let controller_build_identity = homeboy::core::build_identity::current().display;
    let active_daemon_version = status
        .session
        .as_ref()
        .map(|session| session.homeboy_version.clone());
    let version_drift = active_daemon_version
        .as_ref()
        .is_some_and(|version| version != &controller_version);
    let stale_daemon = status.stale_daemon.as_ref();
    LabRunnerHomeboyOutput {
        controller_version,
        controller_build_identity,
        configured_executable: configured_executable.to_string(),
        active_daemon_version,
        active_daemon_build_identity: status
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.clone()),
        job_command_binary_version: stale_daemon
            .map(|warning| warning.job_command_binary_version.clone()),
        job_command_binary_build_identity: stale_daemon
            .and_then(|warning| warning.job_command_binary_build_identity.clone()),
        stale_daemon_severity: stale_daemon.map(|warning| warning.severity.to_string()),
        stale_daemon_refresh_command: stale_daemon.map(|warning| warning.refresh_command.clone()),
        stale_daemon: stale_daemon.and_then(|warning| serde_json::to_value(warning).ok()),
        version_drift,
        command_availability_checks: lab_command_availability_checks(configured_executable),
        artifact_features: runner_artifact_feature_diagnostics(
            runner_id,
            configured_executable,
            status,
            version_drift,
        ),
        refresh_commands: lab_runner_homeboy_refresh_commands(runner_id),
        upgrade_command: format!(
            "homeboy upgrade --force --upgrade-runner {}",
            shell_arg(runner_id)
        ),
    }
}

pub(crate) fn wp_codebox_tool_diagnostics(
    runner_id: Option<&str>,
    env: &BTreeMap<String, String>,
) -> RunnerToolDiagnostics {
    let configured = env
        .get("HOMEBOY_WP_CODEBOX_BIN")
        .cloned()
        .or_else(|| env.get("HOMEBOY_SETTINGS_WP_CODEBOX_BIN").cloned());
    let configured_binary_source = if env.contains_key("HOMEBOY_WP_CODEBOX_BIN") {
        "HOMEBOY_WP_CODEBOX_BIN"
    } else if env.contains_key("HOMEBOY_SETTINGS_WP_CODEBOX_BIN") {
        "HOMEBOY_SETTINGS_WP_CODEBOX_BIN"
    } else {
        "unset"
    };
    let install_dir = env
        .get("HOMEBOY_WP_CODEBOX_INSTALL_DIR")
        .cloned()
        .unwrap_or_else(|| "${HOME}/.cache/homeboy/wp-codebox".to_string());
    let managed_cache_source = format!("{}/source", install_dir.trim_end_matches('/'));
    let managed_cache_binary = format!("{managed_cache_source}/packages/cli/dist/index.js");
    RunnerToolDiagnostics {
        tool: "wp-codebox",
        configured_binary: configured,
        configured_binary_source,
        managed_cache_source,
        managed_cache_binary,
        effective_binary_rule:
            "managed cache binary wins when executable; otherwise configured binary, then PATH",
        diagnostic_command: wp_codebox_effective_binary_command(runner_id),
    }
}

pub(crate) fn wp_codebox_effective_binary_command(runner_id: Option<&str>) -> String {
    let script = "configured=${HOMEBOY_WP_CODEBOX_BIN:-${HOMEBOY_SETTINGS_WP_CODEBOX_BIN:-}}; install_dir=${HOMEBOY_WP_CODEBOX_INSTALL_DIR:-$HOME/.cache/homeboy/wp-codebox}; managed_source=$install_dir/source; managed_binary=$managed_source/packages/cli/dist/index.js; if [ -x \"$managed_binary\" ]; then effective=$managed_binary; source=managed_cache; elif [ -n \"$configured\" ]; then effective=$configured; source=configured; else effective=$(command -v wp-codebox 2>/dev/null || true); source=path; fi; revision=$(git -C \"$managed_source\" rev-parse --short HEAD 2>/dev/null || true); printf 'configured_binary=%s\nmanaged_cache_source=%s\nmanaged_cache_binary=%s\neffective_binary=%s\neffective_source=%s\nmanaged_cache_revision=%s\n' \"${configured:-}\" \"$managed_source\" \"$managed_binary\" \"${effective:-}\" \"$source\" \"${revision:-}\"";
    match runner_id {
        Some(runner_id) => format!(
            "homeboy runner exec {} --raw -- bash -lc {}",
            shell_arg(runner_id),
            shell_arg(script)
        ),
        None => format!("bash -lc {}", shell_arg(script)),
    }
}

fn lab_command_availability_checks(homeboy_path: &str) -> Vec<String> {
    let binary = shell_arg(homeboy_path);
    vec![
        format!("{binary} --version"),
        format!("{binary} runner exec --help"),
        format!("{binary} runs artifact --help"),
        format!("{binary} fuzz --help"),
        format!("{binary} runs evidence --help"),
        format!("{binary} extension list"),
    ]
}

pub(super) fn runner_artifact_feature_diagnostics(
    runner_id: &str,
    homeboy_path: &str,
    status: &RunnerStatusReport,
    version_drift: bool,
) -> RunnerArtifactFeatureDiagnostics {
    let binary = shell_arg(homeboy_path);
    let runner_arg = shell_arg(runner_id);
    let mut hints = Vec::new();
    if version_drift || status.stale_daemon.is_some() {
        hints.push(format!(
            "Runner `{runner_id}` reports Homeboy version/build drift. If artifact commands are missing on runner jobs, restart the active daemon with `homeboy runner disconnect {runner_arg}` then `homeboy runner connect {runner_arg}`."
        ));
    }
    if status.connected
        && status
            .session
            .as_ref()
            .and_then(|session| session.local_url.as_ref())
            .is_none()
    {
        hints.push(format!(
            "Runner `{runner_id}` has no direct daemon URL in the active session; verify artifact command support through managed exec instead of assuming the controller binary matches the runner binary."
        ));
    }

    RunnerArtifactFeatureDiagnostics {
        required_features: vec!["runner_exec_artifact_output", "runs_artifact_attach"],
        controller_commands: vec![
            "homeboy runner exec <runner-id> --run-id <run-id> --artifact <path> -- <command>"
                .to_string(),
            "homeboy runs artifact attach <run-id> --runner <runner-id> --path <path> --name <name>"
                .to_string(),
        ],
        runner_command_checks: vec![
            format!("{binary} runner exec --help"),
            format!("{binary} runs artifact attach --help"),
            format!("homeboy runner exec {runner_arg} -- {binary} runner exec --help"),
            format!("homeboy runner exec {runner_arg} -- {binary} runs artifact attach --help"),
        ],
        hints,
    }
}

fn lab_runner_homeboy_refresh_commands(runner_id: &str) -> Vec<String> {
    let runner_arg = shell_arg(runner_id);
    vec![
        format!("homeboy runner disconnect {runner_arg}"),
        format!("homeboy runner connect {runner_arg}"),
    ]
}

pub(super) fn runner_followups(runner_id: Option<&str>) -> Vec<LabFollowup> {
    let mut followups = vec![
        LabFollowup {
            label: "recent_runs",
            command: "homeboy runs list --limit 5".to_string(),
            purpose: "Find recent persisted run records before digging into runner state.",
        },
        LabFollowup {
            label: "latest_bench_run",
            command: "homeboy runs latest-run --kind bench".to_string(),
            purpose: "Resolve the latest benchmark run id for evidence inspection.",
        },
        LabFollowup {
            label: "latest_fuzz_run",
            command: "homeboy runs latest-run --kind fuzz".to_string(),
            purpose: "Resolve the latest fuzz run id for evidence inspection.",
        },
        LabFollowup {
            label: "run_artifacts",
            command: "homeboy runs artifacts <run-id>".to_string(),
            purpose: "List recorded run artifacts through Homeboy.",
        },
        LabFollowup {
            label: "run_evidence",
            command: "homeboy runs evidence <run-id>".to_string(),
            purpose: "Show stable evidence summary and reviewer-facing commands for one run.",
        },
        LabFollowup {
            label: "run_refs",
            command: "homeboy runs refs --kind bench --limit 10".to_string(),
            purpose: "List recent benchmark run and artifact refs.",
        },
        LabFollowup {
            label: "fuzz_run_refs",
            command: "homeboy runs refs --kind fuzz --limit 10".to_string(),
            purpose: "List recent fuzz run and artifact refs.",
        },
    ];
    let Some(runner_id) = runner_id else {
        return followups;
    };
    let runner_arg = shell_arg(runner_id);
    followups.extend([
        LabFollowup {
            label: "doctor",
            command: format!("homeboy runner doctor {runner_arg} --scope lab-offload"),
            purpose: "Probe runner tools, workspace writability, artifact storage, and Lab offload readiness.",
        },
        LabFollowup {
            label: "env",
            command: format!("homeboy runner env {runner_arg}"),
            purpose: "Show the redacted environment Homeboy injects into runner jobs.",
        },
        LabFollowup {
            label: "homeboy_binary_refresh",
            command: format!(
                "homeboy runner disconnect {runner_arg} && homeboy runner connect {runner_arg}"
            ),
            purpose: "Restart the runner daemon so offload uses the currently configured Homeboy binary.",
        },
        LabFollowup {
            label: "homeboy_binary_upgrade",
            command: format!("homeboy upgrade --force --upgrade-runner {runner_arg}"),
            purpose: "Upgrade the Homeboy binary configured for this runner before reconnecting stale runs.",
        },
        LabFollowup {
            label: "wp_codebox_effective_binary",
            command: wp_codebox_effective_binary_command(Some(runner_id)),
            purpose: "Print the configured WP Codebox binary, managed cache binary/source, effective binary/source, and managed cache revision used by runner workloads.",
        },
        LabFollowup {
            label: "exec",
            command: format!("homeboy runner exec {runner_arg} -- <command>"),
            purpose: "Run a managed follow-up command through Homeboy instead of opening an ad-hoc shell.",
        },
    ]);
    if let Ok(path) = std::env::current_dir() {
        followups.push(LabFollowup {
            label: "workspace_sync",
            command: format!(
                "homeboy runner workspace sync {runner_arg} --path {} --mode snapshot",
                shell_arg(&path.display().to_string())
            ),
            purpose: "Materialize the current checkout into the runner workspace before a replay or follow-up run.",
        });
    }
    followups
}

fn shell_arg(value: &str) -> String {
    if value
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.' | '/' | ':' | '='))
    {
        return value.to_string();
    }
    format!("'{}'", value.replace('\'', "'\\''"))
}

pub(super) fn runner_status_operator_hints(report: &RunnerStatusReport) -> Vec<String> {
    let Some(session) = report.session.as_ref().filter(|_| report.connected) else {
        return Vec::new();
    };
    let mut hints = Vec::new();
    if report.active_job_state == RunnerActiveJobState::Unavailable {
        let reason = report
            .active_job_error
            .as_ref()
            .map(|err| err.message.as_str())
            .unwrap_or("active-job status endpoint was unavailable");
        hints.push(format!(
            "Active-job status for `{}` is unavailable: {reason}. Treat active_job_count=0 as unknown, not idle.",
            report.runner_id
        ));
    }
    if report.stale_runner_job_count > 0 {
        hints.push(format!(
            "Runner `{}` has {} stale runner job(s) that are no longer active. Inspect stale_runner_jobs before retrying affected durable runs.",
            report.runner_id, report.stale_runner_job_count
        ));
    }
    if let Some(warning) = report.stale_daemon.as_ref() {
        let active_daemon = warning
            .active_daemon_control_plane_build_identity
            .as_deref()
            .unwrap_or(&warning.active_daemon_control_plane_version);
        let job_binary = warning
            .job_command_binary_build_identity
            .as_deref()
            .unwrap_or(&warning.job_command_binary_version);
        hints.push(format!(
            "Runner `{}` stale daemon severity={}: active daemon control plane is `{active_daemon}`, but the job command binary is `{job_binary}`. Refresh with `{}` before using runner/Lab status as version evidence.",
            report.runner_id, warning.severity, warning.refresh_command
        ));
    }
    match session.mode {
        RunnerTunnelMode::DirectSsh => {
            if report.active_job_count > 0 {
                hints.push(format!(
                    "Active daemon jobs for `{}` are listed from the direct daemon; inspect with `homeboy runner job logs {} <job-id> --follow` and cancel known jobs with `homeboy runner job cancel {} <job-id>`.",
                    report.runner_id, report.runner_id, report.runner_id
                ));
            }
        }
        RunnerTunnelMode::Reverse => reverse_runner_status_hints(report, session, &mut hints),
    }
    hints
}

fn reverse_runner_status_hints(
    report: &RunnerStatusReport,
    session: &RunnerSession,
    hints: &mut Vec<String>,
) {
    if session.broker_url.is_none() {
        hints.push(format!(
            "Reverse runner `{}` has no broker URL; active-job listing, logs, and cancel require reconnecting with `homeboy runner connect <controller-id> --reverse --reverse-runner {} --broker-url <url>`.",
            report.runner_id, report.runner_id
        ));
        return;
    }
    hints.push(format!(
        "Reverse runner `{}` active jobs are listed through the broker; inspect with `homeboy runner job logs {} <job-id> --follow`.",
        report.runner_id, report.runner_id
    ));
    if report.active_job_count > 0 {
        hints.push(format!(
            "Cancel known reverse broker jobs with `homeboy runner job cancel {} <job-id>`; if a claim lease expires, reconcile broker state with `homeboy runner job reconcile {}` instead of mutating the job store manually.",
            report.runner_id, report.runner_id
        ));
    }
}

pub(super) fn runner_status_operator_commands(
    report: &RunnerStatusReport,
) -> Vec<RunnerOperatorCommand> {
    let Some(session) = report.session.as_ref().filter(|_| report.connected) else {
        return Vec::new();
    };

    let mut commands = Vec::new();
    for job in report
        .active_runner_jobs
        .iter()
        .chain(report.stale_runner_jobs.iter())
    {
        commands.push(RunnerOperatorCommand {
            scope: "job_logs",
            runner_id: report.runner_id.clone(),
            job_id: Some(job.job_id.clone()),
            command: format!(
                "homeboy runner job logs {} {} --follow",
                report.runner_id, job.job_id
            ),
            description: "Follow the active runner job event stream.".to_string(),
        });
        if matches!(job.lifecycle_state.as_deref(), None | Some("active")) {
            commands.push(RunnerOperatorCommand {
                scope: "job_cancel",
                runner_id: report.runner_id.clone(),
                job_id: Some(job.job_id.clone()),
                command: format!(
                    "homeboy runner job cancel {} {}",
                    report.runner_id, job.job_id
                ),
                description: "Request cancellation for a queued or running runner job.".to_string(),
            });
        }
        if let Some(run_id) = job.durable_run_id.as_deref() {
            commands.push(RunnerOperatorCommand {
                scope: "artifact_get",
                runner_id: report.runner_id.clone(),
                job_id: Some(job.job_id.clone()),
                command: format!("homeboy runs artifact get {run_id} <artifact-id> -o <path>"),
                description: "Fetch a mirrored observation artifact after the run records one."
                    .to_string(),
            });
        }
    }

    if session.mode == RunnerTunnelMode::Reverse {
        if session.broker_url.is_some() {
            commands.push(RunnerOperatorCommand {
                scope: "broker_reconcile",
                runner_id: report.runner_id.clone(),
                job_id: None,
                command: format!(
                    "homeboy runner job reconcile {}",
                    shell_arg(&report.runner_id)
                ),
                description:
                    "Fail expired reverse-runner claims through the broker-owned lifecycle path."
                        .to_string(),
            });
            for job in &report.active_runner_jobs {
                commands.push(RunnerOperatorCommand {
                    scope: "broker_artifact_lookup",
                    runner_id: report.runner_id.clone(),
                    job_id: Some(job.job_id.clone()),
                    command: format!(
                        "homeboy runner job artifacts {} {} <artifact-id>",
                        shell_arg(&report.runner_id),
                        shell_arg(&job.job_id)
                    ),
                    description: "Inspect broker-held reverse-runner artifact metadata."
                        .to_string(),
                });
            }
        }
    }

    if let Some(warning) = report.stale_daemon.as_ref() {
        commands.push(RunnerOperatorCommand {
            scope: "daemon_refresh",
            runner_id: report.runner_id.clone(),
            job_id: None,
            command: warning.refresh_command.clone(),
            description: "Restart the active runner daemon so the control plane uses the configured job command binary.".to_string(),
        });
    }

    commands
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::runner::{
        RunnerActiveJobState, RunnerSessionRole, RunnerStaleDaemonWarning,
    };
    use homeboy::core::runners::{RunnerSessionState, RunnerTunnelMode};

    #[test]
    fn stale_daemon_status_hint_labels_control_plane_and_job_binary() {
        let report = stale_daemon_report();

        let hints = runner_status_operator_hints(&report);
        let commands = runner_status_operator_commands(&report);

        let hint = hints
            .iter()
            .find(|hint| hint.contains("stale daemon"))
            .expect("stale daemon hint");
        assert!(hint.contains("severity=warning"));
        assert!(hint.contains("active daemon control plane"));
        assert!(hint.contains("homeboy 0.259.0+daemon"));
        assert!(hint.contains("job command binary"));
        assert!(hint.contains("homeboy 0.262.0+binary"));
        assert!(hint.contains(
            "homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab"
        ));

        let refresh = commands
            .iter()
            .find(|command| command.scope == "daemon_refresh")
            .expect("daemon refresh command");
        assert_eq!(
            refresh.command,
            "homeboy runner disconnect homeboy-lab && homeboy runner connect homeboy-lab"
        );
        assert!(refresh
            .description
            .contains("configured job command binary"));
    }

    fn stale_daemon_report() -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: "homeboy-lab".to_string(),
            connected: true,
            state: RunnerSessionState::Connected,
            session: Some(RunnerSession {
                runner_id: "homeboy-lab".to_string(),
                mode: RunnerTunnelMode::DirectSsh,
                role: RunnerSessionRole::Controller,
                server_id: Some("lab-server".to_string()),
                controller_id: None,
                broker_url: None,
                remote_daemon_address: Some("127.0.0.1:7331".to_string()),
                local_port: Some(7331),
                local_url: Some("http://127.0.0.1:7331".to_string()),
                tunnel_pid: Some(12345),
                remote_daemon_pid: Some(23456),
                homeboy_version: "homeboy 0.259.0".to_string(),
                homeboy_build_identity: Some("homeboy 0.259.0+daemon".to_string()),
                connected_at: "2026-06-26T00:00:00Z".to_string(),
                worker_identity: None,
                worker_pid: None,
                last_seen_at: None,
            }),
            stale_daemon: Some(RunnerStaleDaemonWarning::new(
                "homeboy-lab",
                "homeboy 0.259.0".to_string(),
                "homeboy 0.262.0".to_string(),
                Some("homeboy 0.259.0+daemon".to_string()),
                Some("homeboy 0.262.0+binary".to_string()),
            )),
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            session_path: "/tmp/homeboy-lab.json".to_string(),
        }
    }
}
