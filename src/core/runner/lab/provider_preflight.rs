//! AgentTask provider preflight for Lab offload.
//!
//! Before dispatching an `agent-task cook` command to a Lab
//! runner, Homeboy verifies that the requested executor backend/selector is
//! actually selectable *on that runner* — not just locally. This module owns
//! that command-specific bridge end to end:
//!
//! - parsing the backend/selector out of the offload argv
//!   (`agent_task_provider_selection_from_args`),
//! - probing `agent-task providers` on the runner and parsing its (possibly
//!   chatter-wrapped) JSON envelope,
//! - resolving the requested backend/selector against the runner-reported
//!   provider list using the same `resolve_provider_for_backend` contract that
//!   execution-time selection uses,
//! - optionally refreshing a safe runner session once and re-probing, and
//! - turning an unselectable provider into a precise, operator-actionable
//!   validation error.
//!
//! Keeping this off the `offload` request path makes the offload module's
//! surface narrower: it only calls `preflight_agent_task_provider_on_runner`
//! and never reaches into provider discovery internals.

use std::path::Path;

use crate::core::agent_task_provider::{resolve_provider_for_backend, ProviderResolution};
use crate::core::agent_tasks::provider::{
    default_backend_for_component, AgentTaskExecutorProvider, ExtensionProviderAgentTaskExecutor,
};
use crate::core::engine::shell;
use crate::core::source_snapshot::SourceSnapshot;
use crate::core::{Error, Result};

use super::super::command_path::preflight_remote_argv_path_translation;
use super::super::{
    connect, disconnect, exec, status, RunnerActiveJobState, RunnerCapabilityPreflight,
    RunnerExecOptions, RunnerStatusReport, RunnerTunnelMode,
};
use super::offload::runner_homeboy_daemon_display;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentTaskProviderSelection {
    backend: String,
    selector: Option<String>,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn preflight_agent_task_provider_on_runner(
    runner_id: &str,
    command_prefix: &[String],
    remote_cwd: &str,
    source_path: &Path,
    args: &[String],
    env: std::collections::HashMap<String, String>,
    source_snapshot: SourceSnapshot,
    required_extensions: Vec<String>,
    capability_preflight: Option<RunnerCapabilityPreflight>,
    runner_homeboy: &serde_json::Value,
    runner_status: &RunnerStatusReport,
) -> Result<()> {
    let Some(selection) = agent_task_provider_selection_from_args(args)? else {
        return Ok(());
    };

    let mut command = command_prefix.to_vec();
    command.extend([
        "agent-task".to_string(),
        "providers".to_string(),
        "--backend".to_string(),
        selection.backend.clone(),
        "--validate-readiness".to_string(),
    ]);
    if let Some(selector) = &selection.selector {
        command.extend(["--selector".to_string(), selector.clone()]);
    }
    // The probe argv is a fixed `agent-task providers` listing built from the
    // runner command prefix, not forwarded caller args — but assert the same
    // path-translation contract the offload dispatch site enforces so no
    // controller-local path can ever ride this preflight to the remote runner.
    preflight_remote_argv_path_translation(
        "Lab agent-task provider preflight",
        runner_id,
        &command,
        source_path,
        remote_cwd,
    )?;
    let probe = probe_agent_task_providers_on_runner(
        runner_id,
        remote_cwd,
        &command,
        env.clone(),
        source_snapshot.clone(),
        required_extensions.clone(),
        capability_preflight.clone(),
    )?;

    let local_providers = ExtensionProviderAgentTaskExecutor::discover()
        .providers()
        .to_vec();
    let local_available = provider_available(
        &local_providers,
        &selection.backend,
        selection.selector.as_deref(),
    );

    if probe.exit_code != 0 {
        return Err(agent_task_provider_selection_preflight_error(
            runner_id,
            &selection,
            local_available,
            None,
            runner_homeboy,
            Some(format!(
                "runner provider preflight command exited with {}: {}",
                probe.exit_code,
                first_non_empty_line(&probe.stderr)
                    .or_else(|| first_non_empty_line(&probe.stdout))
                    .unwrap_or_else(|| "no output".to_string())
            )),
            &command,
            None,
        ));
    }

    let runner_providers = parse_agent_task_providers_output(&probe.stdout).map_err(|message| {
        agent_task_provider_selection_preflight_error(
            runner_id,
            &selection,
            local_available,
            None,
            runner_homeboy,
            Some(message),
            &command,
            None,
        )
    })?;
    // Resolve against the SAME provider list that `agent-task providers
    // --runner` shows the operator, using the SAME resolution contract
    // (`resolve_provider_for_backend`) that execution-time selection uses. This
    // guarantees discovery and preflight can never disagree about whether a
    // listed provider is selectable for the requested backend/selector.
    let mut runner_unavailable_reason =
        runner_provider_unavailable_reason(&runner_providers, &selection);
    let mut refresh_result = None;

    if local_available
        && runner_unavailable_reason.is_some()
        && runner_provider_refresh_is_safe(runner_status)
    {
        refresh_result = Some(refresh_runner_provider_session_if_still_safe(runner_id));
        if refresh_result.as_ref().is_some_and(|result| result.is_ok()) {
            let probe = probe_agent_task_providers_on_runner(
                runner_id,
                remote_cwd,
                &command,
                env,
                source_snapshot,
                required_extensions,
                capability_preflight,
            )?;
            if probe.exit_code == 0 {
                if let Ok(providers) = parse_agent_task_providers_output(&probe.stdout) {
                    runner_unavailable_reason =
                        runner_provider_unavailable_reason(&providers, &selection);
                }
            }
        }
    }

    if let Some(reason) = runner_unavailable_reason {
        return Err(agent_task_provider_selection_preflight_error(
            runner_id,
            &selection,
            local_available,
            Some(false),
            runner_homeboy,
            Some(reason),
            &command,
            refresh_result,
        ));
    }

    Ok(())
}

/// Resolve the requested backend/selector against the runner-reported provider
/// list and, when it is not selectable, return a precise reason that names the
/// exact mapping mismatch in terms of the listed providers. Returns `None` when
/// the provider resolves cleanly.
///
/// This is what keeps the cook preflight aligned with `agent-task providers
/// --runner`: the same provider documents discovery shows are run through the
/// shared `resolve_provider_for_backend` contract, so a provider that discovery
/// lists is either accepted here or rejected with a specific selector/backend
/// explanation — never a bare "availability is false".
fn runner_provider_unavailable_reason(
    providers: &[AgentTaskExecutorProvider],
    selection: &AgentTaskProviderSelection,
) -> Option<String> {
    match resolve_provider_for_backend(
        providers,
        &selection.backend,
        selection.selector.as_deref(),
    ) {
        ProviderResolution::Resolved(_) => None,
        ProviderResolution::NotFound => Some(format!(
            "runner provider discovery returned {} provider(s) ({}), but none declare backend `{}`{}",
            providers.len(),
            provider_inventory_summary(providers),
            selection.backend,
            selection
                .selector
                .as_deref()
                .map(|selector| format!(" with selector/id `{selector}`"))
                .unwrap_or_default()
        )),
        ProviderResolution::AmbiguousExtensionAlias { candidate_ids } => Some(format!(
            "backend `{}` matches extension alias for {} runner providers ({}); re-run with `--selector <id>` to pick one",
            selection.backend,
            candidate_ids.len(),
            candidate_ids.join(", ")
        )),
        ProviderResolution::SelectorMismatch { available_ids } => Some(format!(
            "backend `{}` matches runner providers ({}), but selector/id `{}` does not match any of them",
            selection.backend,
            available_ids.join(", "),
            selection.selector.as_deref().unwrap_or("<default>")
        )),
    }
}

/// Compact `id (backend)` inventory of discovered providers for diagnostics.
fn provider_inventory_summary(providers: &[AgentTaskExecutorProvider]) -> String {
    if providers.is_empty() {
        return "<none>".to_string();
    }
    providers
        .iter()
        .map(|provider| format!("{} (backend `{}`)", provider.id, provider.backend))
        .collect::<Vec<_>>()
        .join(", ")
}

struct AgentTaskProviderProbeOutput {
    stdout: String,
    stderr: String,
    exit_code: i32,
}

fn probe_agent_task_providers_on_runner(
    runner_id: &str,
    remote_cwd: &str,
    command: &[String],
    env: std::collections::HashMap<String, String>,
    source_snapshot: SourceSnapshot,
    required_extensions: Vec<String>,
    capability_preflight: Option<RunnerCapabilityPreflight>,
) -> Result<AgentTaskProviderProbeOutput> {
    let (output, exit_code) = exec(
        runner_id,
        RunnerExecOptions {
            cwd: Some(remote_cwd.to_string()),
            project_id: None,
            allow_diagnostic_ssh: false,
            command: command.to_vec(),
            env,
            secret_env_names: Vec::new(),
            capture_patch: false,
            raw_exec: false,
            source_snapshot: Some(source_snapshot),
            capability_preflight,
            required_extensions,
            require_paths: Vec::new(),
            detach_after_handoff: false,
        },
    )?;

    Ok(AgentTaskProviderProbeOutput {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code,
    })
}

fn runner_provider_refresh_is_safe(status: &RunnerStatusReport) -> bool {
    status.active_job_state == RunnerActiveJobState::Available
        && status.active_jobs.is_empty()
        && status
            .session
            .as_ref()
            .is_some_and(|session| session.mode == RunnerTunnelMode::DirectSsh)
}

fn refresh_runner_provider_session_if_still_safe(
    runner_id: &str,
) -> std::result::Result<(), String> {
    let current_status = status(runner_id).map_err(|err| err.message)?;
    if !runner_provider_refresh_is_safe(&current_status) {
        return Err(
            "runner session is no longer safe to refresh automatically; active jobs may be running or the session is not direct SSH"
                .to_string(),
        );
    }
    disconnect(runner_id).map_err(|err| err.message)?;
    let (report, exit_code) = connect(runner_id).map_err(|err| err.message)?;
    if exit_code == 0 && report.connected {
        Ok(())
    } else {
        Err(report.failure_message.unwrap_or_else(|| {
            format!("runner reconnect exited with {exit_code} without establishing a session")
        }))
    }
}

fn agent_task_provider_selection_from_args(
    args: &[String],
) -> Result<Option<AgentTaskProviderSelection>> {
    let action_index = super::args_util::subcommand_index(args, "agent-task").and_then(|index| {
        args.get(index + 1)
            .filter(|arg| matches!(arg.as_str(), "dispatch" | "cook" | "loop"))
            .map(|_| index + 1)
    });
    let Some(action_index) = action_index else {
        return Ok(None);
    };

    let mut backend = None;
    let mut selector = None;
    let mut repo = None;
    let mut iter = args.iter().skip(action_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--backend" => backend = iter.next().cloned(),
            "--selector" => selector = iter.next().cloned(),
            "--repo" => repo = iter.next().cloned(),
            _ => {
                if let Some(value) = arg.strip_prefix("--backend=") {
                    backend = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--selector=") {
                    selector = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--repo=") {
                    repo = Some(value.to_string());
                }
            }
        }
    }

    let backend = match backend.filter(|backend| !backend.trim().is_empty()) {
        Some(backend) => Some(backend),
        None => default_backend_for_component(repo.as_deref())?,
    };

    Ok(backend.map(|backend| AgentTaskProviderSelection { backend, selector }))
}

fn parse_agent_task_providers_output(
    output: &str,
) -> std::result::Result<Vec<AgentTaskExecutorProvider>, String> {
    let value = parse_json_from_mixed_output(output)
        .ok_or_else(|| "runner provider preflight did not return JSON".to_string())?;
    let providers = value
        .get("providers")
        .or_else(|| value.get("data").and_then(|data| data.get("providers")))
        .cloned()
        .ok_or_else(|| "runner provider preflight JSON did not include providers".to_string())?;

    serde_json::from_value::<Vec<AgentTaskExecutorProvider>>(providers).map_err(|error| {
        format!("runner provider preflight returned invalid providers JSON: {error}")
    })
}

fn parse_json_from_mixed_output(output: &str) -> Option<serde_json::Value> {
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(output) {
        return Some(value);
    }
    for (index, _) in output.match_indices('{') {
        let mut stream = serde_json::Deserializer::from_str(&output[index..]).into_iter();
        if let Some(Ok(value)) = stream.next() {
            return Some(value);
        }
    }
    None
}

fn provider_available(
    providers: &[AgentTaskExecutorProvider],
    backend: &str,
    selector: Option<&str>,
) -> bool {
    matches!(
        resolve_provider_for_backend(providers, backend, selector),
        ProviderResolution::Resolved(_)
    )
}

#[allow(clippy::too_many_arguments)]
fn agent_task_provider_selection_preflight_error(
    runner_id: &str,
    selection: &AgentTaskProviderSelection,
    local_available: bool,
    runner_available: Option<bool>,
    runner_homeboy: &serde_json::Value,
    reason: Option<String>,
    command: &[String],
    refresh_result: Option<std::result::Result<(), String>>,
) -> Error {
    let selector = selection.selector.as_deref().unwrap_or("<default>");
    let runner_available = runner_available.unwrap_or(false);
    let reason = reason.unwrap_or_else(|| {
        format!(
            "runner provider availability is {runner_available} while local provider availability is {local_available}"
        )
    });
    let mut hints = vec![
        format!(
            "Local provider availability: {local_available}; runner provider availability: {runner_available}."
        ),
        provider_preflight_homeboy_identity_hint(runner_id, runner_homeboy),
        refresh_hint(runner_id, runner_homeboy, refresh_result),
        format!(
            "Upgrade or sync the runner runtime/extensions with `{}`.",
            runner_homeboy["upgrade_command"]
                .as_str()
                .unwrap_or("homeboy upgrade --force --upgrade-runner <runner>")
        ),
        format!("Preflight command: `{}`.", command.join(" ")),
    ];
    hints.retain(|hint| !hint.is_empty());

    Error::validation_invalid_argument(
        "backend",
        format!(
            "Lab runner `{runner_id}` cannot execute agent-task backend `{}` selector `{selector}` before dispatch: {reason}. No task cells were queued. This points to extension/runtime sync drift between the controller and selected runner, not a task failure.",
            selection.backend
        ),
        Some(selection.backend.clone()),
        Some(hints),
    )
}

fn provider_preflight_homeboy_identity_hint(
    runner_id: &str,
    runner_homeboy: &serde_json::Value,
) -> String {
    let controller = crate::core::build_identity::current().display;
    let runner = runner_homeboy_daemon_display(runner_homeboy);
    if runner == "<not connected>" {
        return format!(
            "Controller Homeboy is `{controller}`; runner `{runner_id}` has no active daemon identity in Lab status."
        );
    }

    if runner == controller {
        return format!(
            "Controller and runner `{runner_id}` are both using Homeboy `{controller}`."
        );
    }

    format!(
        "Controller Homeboy is `{controller}`, but runner `{runner_id}` active daemon is `{runner}`. Refresh the active binaries/session before treating this as provider drift: {}.",
        runner_homeboy["upgrade_command"]
            .as_str()
            .unwrap_or("homeboy upgrade --force --upgrade-runner <runner>")
    )
}

fn refresh_hint(
    runner_id: &str,
    runner_homeboy: &serde_json::Value,
    refresh_result: Option<std::result::Result<(), String>>,
) -> String {
    match refresh_result {
        Some(Ok(())) => "Homeboy refreshed the runner session automatically, but the selected provider was still unavailable after reconnect.".to_string(),
        Some(Err(message)) => format!(
            "Homeboy tried to refresh runner `{runner_id}` automatically, but reconnect failed: {message}. Refresh manually with `{}`.",
            refresh_command(runner_id, runner_homeboy)
        ),
        None => format!(
            "Refresh runner `{runner_id}` with `{}`.",
            refresh_command(runner_id, runner_homeboy)
        ),
    }
}

fn refresh_command(runner_id: &str, runner_homeboy: &serde_json::Value) -> String {
    runner_homeboy["refresh_commands"]
        .as_array()
        .map(|commands| {
            commands
                .iter()
                .filter_map(|command| command.as_str())
                .collect::<Vec<_>>()
                .join(" && ")
        })
        .filter(|command| !command.is_empty())
        .unwrap_or_else(|| {
            format!(
                "homeboy runner disconnect {} && homeboy runner connect {}",
                shell::quote_arg(runner_id),
                shell::quote_arg(runner_id)
            )
        })
}

fn first_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_task_provider_selection_reads_cook_backend_and_selector() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--backend".to_string(),
            "primary".to_string(),
            "--selector=variant-a".to_string(),
            "--prompt".to_string(),
            "fix it".to_string(),
        ];

        let selection = agent_task_provider_selection_from_args(&args)
            .expect("selection resolves")
            .expect("selection");

        assert_eq!(selection.backend, "primary");
        assert_eq!(selection.selector.as_deref(), Some("variant-a"));
    }

    #[test]
    fn agent_task_provider_selection_reads_loop_backend_and_selector() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "loop".to_string(),
            "--backend=primary".to_string(),
            "--selector".to_string(),
            "variant-a".to_string(),
            "--to-worktree".to_string(),
            "homeboy@candidate".to_string(),
            "--verify".to_string(),
            "homeboy test".to_string(),
            "--goal".to_string(),
            "fix it".to_string(),
        ];

        let selection = agent_task_provider_selection_from_args(&args)
            .expect("selection resolves")
            .expect("selection");

        assert_eq!(selection.backend, "primary");
        assert_eq!(selection.selector.as_deref(), Some("variant-a"));
    }

    #[test]
    fn agent_task_provider_selection_ignores_non_dispatch_commands() {
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "providers".to_string(),
            "--backend".to_string(),
            "primary".to_string(),
        ];

        assert!(agent_task_provider_selection_from_args(&args)
            .expect("selection resolves")
            .is_none());
    }

    #[test]
    fn runner_provider_output_parser_accepts_cli_envelope_with_chatter() {
        let stdout = concat!(
            "Preparing runtime...\n",
            "{\"success\":true,\"data\":{\"providers\":[{\"schema\":\"homeboy/agent-task-executor-provider/v1\",\"id\":\"variant-a\",\"backend\":\"primary\",\"default_backend\":true,\"command\":\"provider-a agent\",\"request_schema\":\"homeboy/agent-task-request/v1\",\"outcome_schema\":\"homeboy/agent-task-outcome/v1\"}]}}\n"
        );

        let providers = parse_agent_task_providers_output(stdout).expect("providers parse");

        assert!(provider_available(&providers, "primary", None));
        assert!(provider_available(&providers, "primary", Some("variant-a")));
        assert!(!provider_available(&providers, "primary", Some("missing")));
    }

    #[test]
    fn parsed_provider_availability_accepts_unique_extension_alias() {
        let stdout = concat!(
            "Preparing runtime...\n",
            "{\"success\":true,\"data\":{\"providers\":[{\"schema\":\"homeboy/agent-task-executor-provider/v1\",\"id\":\"extension-a.agent-task-executor\",\"backend\":\"renamed-backend\",\"default_backend\":true,\"command\":\"extension-a agent\",\"request_schema\":\"homeboy/agent-task-request/v1\",\"outcome_schema\":\"homeboy/agent-task-outcome/v1\",\"extension_id\":\"extension-a\"}]}}\n"
        );

        let providers = parse_agent_task_providers_output(stdout).expect("providers parse");

        assert!(provider_available(&providers, "extension-a", None));
        assert!(provider_available(
            &providers,
            "extension-a",
            Some("extension-a.agent-task-executor")
        ));
        assert!(!provider_available(
            &providers,
            "extension-a",
            Some("missing")
        ));
    }

    #[test]
    fn provider_preflight_error_reports_local_runner_drift_and_no_cells_queued() {
        let selection = AgentTaskProviderSelection {
            backend: "primary".to_string(),
            selector: None,
        };
        let runner_homeboy = serde_json::json!({
            "refresh_commands": [
                "homeboy runner disconnect homeboy-lab",
                "homeboy runner connect homeboy-lab"
            ],
            "upgrade_command": "homeboy upgrade --force --upgrade-runner homeboy-lab"
        });

        let err = agent_task_provider_selection_preflight_error(
            "homeboy-lab",
            &selection,
            true,
            Some(false),
            &runner_homeboy,
            None,
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "providers".to_string(),
            ],
            None,
        );

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("homeboy-lab"));
        assert!(err.message.contains("backend `primary`"));
        assert!(err.message.contains("No task cells were queued"));
        assert!(err.message.contains("extension/runtime sync drift"));
        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Local provider availability: true"))));
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("runner provider availability: false"))));
        assert!(tried.iter().any(|hint| hint.as_str().is_some_and(
            |hint| hint.contains("homeboy upgrade --force --upgrade-runner homeboy-lab")
        )));
    }

    #[test]
    fn provider_preflight_error_prioritizes_homeboy_identity_mismatch() {
        let selection = AgentTaskProviderSelection {
            backend: "primary".to_string(),
            selector: None,
        };
        let runner_homeboy = serde_json::json!({
            "active_daemon_build_identity": "homeboy 0.0.0+old",
            "refresh_commands": [
                "homeboy runner disconnect homeboy-lab",
                "homeboy runner connect homeboy-lab"
            ],
            "upgrade_command": "homeboy upgrade --force --upgrade-runner homeboy-lab"
        });

        let err = agent_task_provider_selection_preflight_error(
            "homeboy-lab",
            &selection,
            true,
            Some(false),
            &runner_homeboy,
            None,
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "providers".to_string(),
            ],
            None,
        );

        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Controller Homeboy is `homeboy"))));
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("active daemon is `homeboy 0.0.0+old`"))));
        assert!(tried.iter().any(|hint| hint.as_str().is_some_and(
            |hint| hint.contains("homeboy upgrade --force --upgrade-runner homeboy-lab")
        )));
    }
}
