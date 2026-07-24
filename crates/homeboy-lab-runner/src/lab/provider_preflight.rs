//! Agent-task provider readiness on a selected Lab runner.

use std::path::Path;
use std::time::Duration;

use homeboy_agents::agent_task_provider::{resolve_provider_for_backend, ProviderResolution};
use homeboy_agents::agent_tasks::provider::{
    default_backend_for_component, AgentTaskExecutorProvider, ExtensionProviderAgentTaskExecutor,
};
use homeboy_core::engine::shell;
use homeboy_core::redaction::redact_argv_shell_display;
use homeboy_core::source_snapshot::SourceSnapshot;
use homeboy_core::{Error, ErrorCode, Result};

use super::super::command_path::preflight_remote_argv_path_translation;
use super::super::connection::disconnect_with_session;
use super::super::execution::exec_with_status_snapshot;
use super::super::{
    connect, status, RunnerActiveJobState, RunnerCapabilityPreflight, RunnerExecOptions,
    RunnerSession, RunnerStatusReport, RunnerTunnelMode,
};
use super::offload::runner_homeboy_daemon_display;

#[derive(Debug, Clone, PartialEq, Eq)]
struct AgentTaskProviderSelection {
    backend: String,
    selector: Option<String>,
    runtime_identity: Option<homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity>,
}

const PROVIDER_SESSION_REFRESH_RETRY_ATTEMPTS: usize = 20;
const PROVIDER_SESSION_REFRESH_RETRY_DELAY: Duration = Duration::from_millis(250);

#[allow(clippy::too_many_arguments)]
pub(super) fn preflight_agent_task_provider_on_runner(
    runner_id: &str,
    command_prefix: &[String],
    remote_cwd: &str,
    source_path: &Path,
    args: &[String],
    env: std::collections::HashMap<String, String>,
    source_snapshot: Option<SourceSnapshot>,
    required_extensions: Vec<String>,
    capability_preflight: Option<RunnerCapabilityPreflight>,
    runner_homeboy: &serde_json::Value,
    runner_status: &RunnerStatusReport,
) -> Result<()> {
    let Some(selection) = agent_task_provider_selection_from_args(args)? else {
        return Ok(());
    };
    if let Some(identity) = selection.runtime_identity.as_ref() {
        validate_controller_runtime_identity(identity, &selection)?;
    }

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
    if source_snapshot.is_some() {
        preflight_remote_argv_path_translation(
            "Lab agent-task provider preflight",
            runner_id,
            &command,
            source_path,
            remote_cwd,
        )?;
    }
    let probe = probe_agent_task_providers_on_runner(
        runner_id,
        remote_cwd,
        &command,
        env.clone(),
        source_snapshot.clone(),
        required_extensions.clone(),
        capability_preflight.clone(),
        runner_status.clone(),
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
                "runner provider preflight command exited with {}; inspect details.command_output for full stdout/stderr",
                probe.exit_code
            )),
            &command,
            Some(&probe),
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
            None,
        )
    })?;
    // Resolve against the SAME provider list that `agent-task providers
    // --runner` shows the operator, using the SAME resolution contract
    // (`resolve_provider_for_backend`) that execution-time selection uses. This
    // guarantees discovery and preflight can never disagree about whether a
    // listed provider is selectable for the requested backend/selector.
    let mut runner_unavailable_reason = selection
        .runtime_identity
        .as_ref()
        .and_then(|identity| {
            (!resolved_materialized_runtime(identity))
                .then(|| exact_runtime_requirement_reason(&runner_providers, identity))
                .flatten()
        })
        .or_else(|| runner_provider_unavailable_reason(&runner_providers, &selection));
    let mut refresh_result = None;

    if local_available
        && runner_unavailable_reason.is_some()
        && runner_provider_refresh_is_safe(runner_status)
    {
        let refresh = refresh_runner_provider_session_if_still_safe(
            runner_id,
            runner_status.session.as_ref(),
        );
        let replacement_status = refresh
            .as_ref()
            .ok()
            .map(ProviderSessionRefresh::status)
            .cloned();
        refresh_result = Some(refresh.map(|_| ()));
        if let Some(replacement_status) = replacement_status {
            let probe = probe_agent_task_providers_on_runner(
                runner_id,
                remote_cwd,
                &command,
                env,
                source_snapshot,
                required_extensions,
                capability_preflight,
                replacement_status,
            )?;
            if probe.exit_code == 0 {
                if let Ok(providers) = parse_agent_task_providers_output(&probe.stdout) {
                    runner_unavailable_reason = selection
                        .runtime_identity
                        .as_ref()
                        .and_then(|identity| {
                            (!resolved_materialized_runtime(identity))
                                .then(|| exact_runtime_requirement_reason(&providers, identity))
                                .flatten()
                        })
                        .or_else(|| runner_provider_unavailable_reason(&providers, &selection));
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
            None,
            refresh_result,
        ));
    }

    Ok(())
}

/// A v2 plan that Lab has rewritten to its immutable generation is already
/// source-revision validated by the materializer. Requiring discovery to
/// advertise that new path would incorrectly reject the job-scoped provider
/// copy; legacy plans retain the exact advertised-runtime requirement.
fn resolved_materialized_runtime(
    identity: &homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity,
) -> bool {
    let Ok(plan) = serde_json::from_value::<
        homeboy_core::agent_runtime_manifest::AgentRuntimeMaterializationPlan,
    >(identity.materialization_plan.clone()) else {
        return false;
    };
    plan.schema == "homeboy/agent-runtime-materialization-plan/v2"
        && plan.runtime_path.is_some()
        && plan.source_revision.as_deref() == Some(identity.source_revision.as_str())
}

/// A controller pin is only compatible when the runner advertises the same
/// provider and immutable materialization revision. Provider availability alone
/// cannot prove that a runner will execute the controller-selected runtime.
fn exact_runtime_requirement_reason(
    providers: &[AgentTaskExecutorProvider],
    identity: &homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity,
) -> Option<String> {
    let Some(provider) = providers
        .iter()
        .find(|provider| provider.id == identity.provider_id)
    else {
        return Some(format!(
            "runner does not advertise controller-selected provider `{}`",
            identity.provider_id
        ));
    };
    let Some(plan) = provider.extra.get("runtime_materialization_plan") else {
        return Some(format!(
            "runner provider `{}` has no runtime materialization declaration; it cannot prove the required runtime revision `{}`",
            identity.provider_id, identity.source_revision
        ));
    };
    let runner_revision = plan
        .get("source_revision")
        .and_then(serde_json::Value::as_str);
    if runner_revision == Some(identity.source_revision.as_str()) {
        return None;
    }
    Some(format!(
        "runner provider `{}` does not advertise required runtime revision `{}` (advertised `{}`); materialize the controller-selected runtime before retrying",
        identity.provider_id,
        identity.source_revision,
        runner_revision.unwrap_or("missing")
    ))
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
        ProviderResolution::SelectorMismatch { available_ids, .. } => Some(format!(
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
    source_snapshot: Option<SourceSnapshot>,
    required_extensions: Vec<String>,
    capability_preflight: Option<RunnerCapabilityPreflight>,
    status_snapshot: RunnerStatusReport,
) -> Result<AgentTaskProviderProbeOutput> {
    let options = provider_probe_options(
        command,
        remote_cwd,
        env,
        source_snapshot,
        required_extensions,
        capability_preflight,
    );
    let (output, exit_code) = exec_with_status_snapshot(runner_id, options, Some(status_snapshot))?;

    Ok(AgentTaskProviderProbeOutput {
        stdout: output.stdout,
        stderr: output.stderr,
        exit_code,
    })
}

fn provider_probe_options(
    command: &[String],
    remote_cwd: &str,
    env: std::collections::HashMap<String, String>,
    source_snapshot: Option<SourceSnapshot>,
    required_extensions: Vec<String>,
    capability_preflight: Option<RunnerCapabilityPreflight>,
) -> RunnerExecOptions {
    if let Some(source_snapshot) = source_snapshot {
        RunnerExecOptions::command(command.to_vec())
            .with_cwd(remote_cwd)
            .with_env(env)
            .with_source_snapshot(source_snapshot)
            .with_required_extensions(required_extensions)
            .with_optional_capability_preflight(capability_preflight)
    } else {
        RunnerExecOptions::command(command.to_vec())
            .with_cwd(remote_cwd)
            .with_env(env)
            .with_required_extensions(required_extensions)
            .with_optional_capability_preflight(capability_preflight)
    }
}

fn runner_provider_refresh_is_safe(status: &RunnerStatusReport) -> bool {
    status.active_job_state == RunnerActiveJobState::Available
        && status.active_jobs.is_empty()
        && status
            .session
            .as_ref()
            .is_some_and(|session| session.mode == RunnerTunnelMode::DirectSsh)
}

#[derive(Debug, Clone)]
enum ProviderSessionRefresh {
    Refreshed(RunnerStatusReport),
    Superseded(RunnerStatusReport),
}

impl ProviderSessionRefresh {
    fn status(&self) -> &RunnerStatusReport {
        match self {
            Self::Refreshed(status) | Self::Superseded(status) => status,
        }
    }
}

fn refresh_runner_provider_session_if_still_safe(
    runner_id: &str,
    expected_session: Option<&RunnerSession>,
) -> std::result::Result<ProviderSessionRefresh, String> {
    let expected_session = expected_session.ok_or_else(|| {
        "runner session is unavailable; refusing an unbound provider refresh".to_string()
    })?;
    let promotion_lease = acquire_provider_session_refresh_lease(runner_id)?;
    promotion_lease
        .assert_generation()
        .map_err(|err| err.message)?;
    let current_status = status(runner_id).map_err(|err| err.message)?;
    if provider_session_was_superseded(expected_session, current_status.session.as_ref()) {
        // Another controller completed the only safe refresh for this observed
        // session. Re-probe the replacement instead of stopping it again.
        return Ok(ProviderSessionRefresh::Superseded(current_status));
    }
    if !runner_provider_refresh_is_safe(&current_status) {
        return Err(
            "runner session is no longer safe to refresh automatically; active jobs may be running or the session is not direct SSH"
                .to_string(),
        );
    }
    disconnect_with_session(runner_id, Some(expected_session), false).map_err(|err| err.message)?;
    let (report, exit_code) = connect(runner_id).map_err(|err| err.message)?;
    if exit_code == 0 && report.connected {
        let replacement_status = status(runner_id).map_err(|err| err.message)?;
        Ok(ProviderSessionRefresh::Refreshed(replacement_status))
    } else {
        Err(report.failure_message.unwrap_or_else(|| {
            format!("runner reconnect exited with {exit_code} without establishing a session")
        }))
    }
}

fn acquire_provider_session_refresh_lease(
    runner_id: &str,
) -> std::result::Result<homeboy_core::runtime_promotion::RuntimePromotionLease, String> {
    for attempt in 0..PROVIDER_SESSION_REFRESH_RETRY_ATTEMPTS {
        match homeboy_core::runtime_promotion::acquire(
            "runner provider readiness refresh",
            runner_id.to_string(),
        ) {
            Ok(lease) => return Ok(lease),
            Err(error)
                if error.details.get("field").and_then(|value| value.as_str())
                    == Some("runtime_promotion_lease")
                    && attempt + 1 < PROVIDER_SESSION_REFRESH_RETRY_ATTEMPTS =>
            {
                std::thread::sleep(PROVIDER_SESSION_REFRESH_RETRY_DELAY);
            }
            Err(error) => return Err(error.message),
        }
    }
    unreachable!("bounded provider session refresh lease retries always return")
}

fn provider_session_was_superseded(
    expected_session: &RunnerSession,
    current_session: Option<&RunnerSession>,
) -> bool {
    current_session != Some(expected_session)
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
    let mut resolved_provider_policy = None;
    let mut iter = args.iter().skip(action_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        match arg.as_str() {
            "--backend" => backend = iter.next().cloned(),
            "--selector" => selector = iter.next().cloned(),
            "--repo" => repo = iter.next().cloned(),
            "--resolved-provider-policy" => resolved_provider_policy = iter.next().cloned(),
            _ => {
                if let Some(value) = arg.strip_prefix("--backend=") {
                    backend = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--selector=") {
                    selector = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--repo=") {
                    repo = Some(value.to_string());
                } else if let Some(value) = arg.strip_prefix("--resolved-provider-policy=") {
                    resolved_provider_policy = Some(value.to_string());
                }
            }
        }
    }

    let backend = match backend.filter(|backend| !backend.trim().is_empty()) {
        Some(backend) => Some(backend),
        None => default_backend_for_component(repo.as_deref())?,
    };

    let runtime_identity = resolved_provider_policy
        .map(|policy| {
            serde_json::from_str::<
                    homeboy_core::agent_task_config::ResolvedAgentTaskProviderPolicy,
                >(&policy)
                .map(|policy| policy.runtime_identity)
                .map_err(|error| {
                    Error::validation_invalid_argument(
                        "resolved-provider-policy",
                        "Lab received an invalid controller provider policy",
                        Some(error.to_string()),
                        None,
                    )
                })
        })
        .transpose()?
        .flatten();
    Ok(backend.map(|backend| AgentTaskProviderSelection {
        backend,
        selector,
        runtime_identity,
    }))
}

fn validate_controller_runtime_identity(
    identity: &homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity,
    selection: &AgentTaskProviderSelection,
) -> Result<()> {
    if !homeboy_core::agent_runtime_manifest::is_immutable_revision(&identity.source_revision) {
        return Err(Error::validation_invalid_argument(
            "resolved-provider-policy",
            "Lab requires a full immutable runtime source revision from the controller",
            Some(identity.source_revision.clone()),
            None,
        ));
    }
    let provider: AgentTaskExecutorProvider = serde_json::from_value(identity.provider.clone())
        .map_err(|error| {
            Error::validation_invalid_argument(
                "resolved-provider-policy",
                "Lab received an invalid controller-selected provider",
                Some(error.to_string()),
                None,
            )
        })?;
    if provider.id != identity.provider_id || provider.backend != selection.backend {
        return Err(Error::validation_invalid_argument(
            "resolved-provider-policy",
            "Lab controller runtime identity does not match the requested provider",
            Some(format!(
                "identity provider '{}' backend '{}'; request backend '{}'",
                identity.provider_id, provider.backend, selection.backend
            )),
            None,
        ));
    }
    // Older controller policies did not carry the v2 declaration. They remain
    // usable only through the exact runner-advertised revision check above.
    let Some(plan) = serde_json::from_value::<
        homeboy_core::agent_runtime_manifest::AgentRuntimeMaterializationPlan,
    >(identity.materialization_plan.clone())
    .ok() else {
        return Ok(());
    };
    if plan.runtime_sources.is_empty() {
        return Ok(());
    }
    homeboy_core::agent_runtime_manifest::validate_runtime_materialization_plan(&plan)?;
    // A published v2 generation has already verified and rewritten its source
    // locators on the selected runner. Re-checking those paths on the
    // controller would both be wrong and reintroduce controller-path coupling.
    if plan.runtime_path.is_some() {
        return Ok(());
    }
    for source in &plan.runtime_sources {
        if let homeboy_core::agent_runtime_manifest::AgentRuntimeSourceLocator::LocalPath { path } =
            &source.locator
        {
            let source_path = Path::new(path);
            if !source_path.is_dir() {
                return Err(Error::validation_invalid_argument(
                    "resolved-provider-policy",
                    "controller runtime source is unavailable before Lab submission",
                    Some(path.clone()),
                    None,
                ));
            }
            if homeboy_core::git::head_sha(source_path).as_deref()
                != Some(source.content_identity.as_str())
            {
                return Err(Error::validation_invalid_argument(
                    "resolved-provider-policy",
                    "controller runtime source no longer matches its declared content identity",
                    Some(source.id.clone()),
                    None,
                ));
            }
        }
    }
    Ok(())
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
    probe_output: Option<&AgentTaskProviderProbeOutput>,
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
        format!("Preflight command: `{}`.", redact_argv_shell_display(command)),
    ];
    hints.retain(|hint| !hint.is_empty());

    let problem = format!(
        "Lab runner `{runner_id}` cannot execute agent-task backend `{}` selector `{selector}` before dispatch: {reason}. No task cells were queued. This points to extension/runtime sync drift between the controller and selected runner, not a task failure.",
        selection.backend
    );
    let mut details = serde_json::json!({
        "field": "backend",
        "problem": problem,
        "id": selection.backend,
        "tried": hints,
        "provider_availability": {
            "controller": local_available,
            "runner": runner_available,
            "backend": selection.backend,
            "selector": selection.selector,
        },
        "preflight_command": command,
        "runner_remediation_command": refresh_command(runner_id, runner_homeboy),
        "runner_homeboy": runner_homeboy,
    });
    if let Some(output) = probe_output {
        details["command_output"] = serde_json::json!({
            "exit_code": output.exit_code,
            "stdout": output.stdout,
            "stderr": output.stderr,
        });
    }

    Error::new(
        ErrorCode::ValidationInvalidArgument,
        format!("Invalid argument 'backend': {problem}"),
        details,
    )
}

fn provider_preflight_homeboy_identity_hint(
    runner_id: &str,
    runner_homeboy: &serde_json::Value,
) -> String {
    let controller = homeboy_core::build_identity::current().display;
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
        "Controller Homeboy is `{controller}`, but runner `{runner_id}` active daemon is `{runner}`. Refresh the active binaries/session before treating this as provider drift."
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
            "Refresh runner `{runner_id}` with `{}` before retrying the cook.",
            refresh_command(runner_id, runner_homeboy)
        ),
    }
}

fn refresh_command(runner_id: &str, runner_homeboy: &serde_json::Value) -> String {
    runner_homeboy["primary_remediation_command"]
        .as_str()
        .map(str::to_string)
        .filter(|command| !command.is_empty())
        .or_else(|| {
            runner_homeboy["refresh_commands"]
                .as_array()
                .and_then(|commands| {
                    commands
                        .iter()
                        .filter_map(|command| command.as_str())
                        .next()
                })
                .map(str::to_string)
        })
        .or_else(|| {
            runner_homeboy["upgrade_command"]
                .as_str()
                .map(str::to_string)
        })
        .unwrap_or_else(|| {
            format!(
                "homeboy runner refresh-homeboy {} --ref {} --reconnect",
                shell::quote_arg(runner_id),
                controller_refresh_ref()
            )
        })
}

fn controller_refresh_ref() -> String {
    homeboy_product_identity::build_identity()
        .git_commit
        .unwrap_or_else(|| format!("v{}", homeboy_product_identity::product_version()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn direct_session(tunnel_pid: u32, lease_id: &str) -> RunnerSession {
        RunnerSession {
            runner_id: "homeboy-lab".to_string(),
            mode: RunnerTunnelMode::DirectSsh,
            role: crate::RunnerSessionRole::Controller,
            server_id: Some("lab".to_string()),
            controller_id: None,
            broker_url: None,
            remote_daemon_address: Some("127.0.0.1:49152".to_string()),
            local_port: Some(49152),
            local_url: Some("http://127.0.0.1:49152".to_string()),
            tunnel_pid: Some(tunnel_pid),
            remote_daemon_pid: Some(1234),
            remote_daemon_lease_id: Some(lease_id.to_string()),
            homeboy_version: "0.0.0-test".to_string(),
            homeboy_build_identity: Some("homeboy 0.0.0-test".to_string()),
            connected_at: "2026-07-16T00:00:00Z".to_string(),
            worker_identity: None,
            worker_pid: None,
            last_seen_at: None,
            leaseless_recovery_evidence: None,
        }
    }

    fn connected_status(session: RunnerSession) -> RunnerStatusReport {
        RunnerStatusReport {
            runner_id: session.runner_id.clone(),
            connected: true,
            state: crate::RunnerSessionState::Connected,
            session: Some(session),
            stale_daemon: None,
            daemon_freshness: None,
            active_jobs: Vec::new(),
            active_runner_jobs: Vec::new(),
            stale_runner_jobs: Vec::new(),
            active_job_count: 0,
            stale_runner_job_count: 0,
            active_job_state: RunnerActiveJobState::Available,
            active_job_source: None,
            active_job_error: None,
            active_job_recovery_evidence: None,
            session_path: "/tmp/homeboy-lab.json".to_string(),
        }
    }

    #[test]
    fn concurrent_provider_refresh_reprobes_the_replacement_session() {
        let observed_by_both_cooks = direct_session(100, "lease-before-refresh");
        let replacement_after_first_cook = direct_session(200, "lease-after-refresh");

        assert!(provider_session_was_superseded(
            &observed_by_both_cooks,
            Some(&replacement_after_first_cook),
        ));
        assert!(!provider_session_was_superseded(
            &observed_by_both_cooks,
            Some(&observed_by_both_cooks),
        ));
    }

    #[test]
    fn provider_reprobe_uses_the_fresh_replacement_status_snapshot() {
        let replacement = connected_status(direct_session(200, "lease-after-refresh"));

        for refresh in [
            ProviderSessionRefresh::Refreshed(replacement.clone()),
            ProviderSessionRefresh::Superseded(replacement.clone()),
        ] {
            assert_eq!(
                refresh
                    .status()
                    .session
                    .as_ref()
                    .and_then(|session| session.remote_daemon_lease_id.as_deref()),
                Some("lease-after-refresh")
            );
        }
    }

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
    fn controller_identity_bypasses_runner_discovery_with_full_selected_provider() {
        let policy = homeboy_core::agent_task_config::ResolvedAgentTaskProviderPolicy {
            backend: "controller-backend".to_string(),
            selector: Some("controller-source".to_string()),
            model: None,
            rotation: None,
            rotation_starts_with_first_entry: false,
            retry: Default::default(),
            liveness_timeout_ms: None,
            runtime_identity: Some(homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
                runtime_id: "controller-runtime".to_string(),
                provider_id: "controller-provider".to_string(),
                source_selector: "extension:controller-source".to_string(),
                source_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
                freshness:
                    homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Unverifiable,
                provider: serde_json::json!({
                    "id": "controller-provider",
                    "backend": "controller-backend",
                    "argv": ["controller-provider"]
                }),
                materialization_plan: serde_json::json!({"runtime_id": "controller-runtime"}),
            }),
        };
        let args = vec![
            "homeboy".to_string(),
            "agent-task".to_string(),
            "cook".to_string(),
            "--backend".to_string(),
            "controller-backend".to_string(),
            "--resolved-provider-policy".to_string(),
            serde_json::to_string(&policy).expect("policy"),
        ];

        let selection = agent_task_provider_selection_from_args(&args)
            .expect("selection")
            .expect("agent task selection");

        assert_eq!(
            selection
                .runtime_identity
                .as_ref()
                .expect("identity")
                .provider_id,
            "controller-provider"
        );
        validate_controller_runtime_identity(
            selection.runtime_identity.as_ref().expect("identity"),
            &selection,
        )
        .expect("controller identity is sufficient without runner discovery");
    }

    #[test]
    fn controller_identity_rejects_short_source_revision() {
        let selection = AgentTaskProviderSelection {
            backend: "controller-backend".to_string(),
            selector: None,
            runtime_identity: None,
        };
        let identity = homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
            runtime_id: "runtime".to_string(),
            provider_id: "provider".to_string(),
            source_selector: "extension:source".to_string(),
            source_revision: "0123456".to_string(),
            freshness:
                homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Unverifiable,
            provider: serde_json::json!({"id": "provider", "backend": "controller-backend"}),
            materialization_plan: serde_json::Value::Null,
        };

        let error = validate_controller_runtime_identity(&identity, &selection)
            .expect_err("short revision must fail");

        assert!(error.message.contains("full immutable"));
    }

    #[test]
    fn controller_identity_rejects_provider_backend_mismatch() {
        let selection = AgentTaskProviderSelection {
            backend: "codex".to_string(),
            selector: None,
            runtime_identity: None,
        };
        let identity = homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
            runtime_id: "opencode".to_string(),
            provider_id: "opencode.agent-task-executor".to_string(),
            source_selector: "extension:opencode".to_string(),
            source_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
            freshness:
                homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Unverifiable,
            provider: serde_json::json!({
                "id": "opencode.agent-task-executor",
                "backend": "opencode"
            }),
            materialization_plan: serde_json::Value::Null,
        };

        let error = validate_controller_runtime_identity(&identity, &selection)
            .expect_err("mismatched provider identity must fail");

        assert!(error
            .message
            .contains("does not match the requested provider"));
    }

    #[test]
    fn exact_runtime_requirement_accepts_only_the_controller_revision() {
        let identity = homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeIdentity {
            runtime_id: "runtime".to_string(),
            provider_id: "provider".to_string(),
            source_selector: "extension:provider".to_string(),
            source_revision: "0123456789abcdef0123456789abcdef01234567".to_string(),
            freshness: homeboy_core::agent_task_config::ResolvedAgentTaskRuntimeFreshness::Pinned,
            provider: serde_json::Value::Null,
            materialization_plan: serde_json::Value::Null,
        };
        let mut provider: AgentTaskExecutorProvider = serde_json::from_value(serde_json::json!({
            "id": "provider",
            "backend": "test",
            "argv": ["provider"],
        }))
        .expect("provider");
        provider.extra.insert(
            "runtime_materialization_plan".to_string(),
            serde_json::json!({ "source_revision": identity.source_revision }),
        );
        assert_eq!(
            exact_runtime_requirement_reason(&[provider.clone()], &identity),
            None
        );

        provider.extra.insert(
            "runtime_materialization_plan".to_string(),
            serde_json::json!({ "source_revision": "ffffffffffffffffffffffffffffffffffffffff" }),
        );
        let error = exact_runtime_requirement_reason(&[provider], &identity)
            .expect("stale runner runtime rejects before submission");
        assert!(error.contains("controller-selected runtime"));
    }

    #[test]
    fn runner_provider_output_parser_accepts_cli_envelope_with_chatter() {
        let stdout = concat!(
            "Preparing runtime...\n",
            "{\"success\":true,\"data\":{\"providers\":[{\"schema\":\"homeboy/agent-task-executor-provider/v1\",\"id\":\"variant-a\",\"backend\":\"primary\",\"default_backend\":true,\"argv\":[\"provider-a\",\"agent\"],\"request_schema\":\"homeboy/agent-task-request/v1\",\"outcome_schema\":\"homeboy/agent-task-outcome/v1\"}]}}\n"
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
            "{\"success\":true,\"data\":{\"providers\":[{\"schema\":\"homeboy/agent-task-executor-provider/v1\",\"id\":\"extension-a.agent-task-executor\",\"backend\":\"renamed-backend\",\"default_backend\":true,\"argv\":[\"extension-a\",\"agent\"],\"request_schema\":\"homeboy/agent-task-request/v1\",\"outcome_schema\":\"homeboy/agent-task-outcome/v1\",\"extension_id\":\"extension-a\"}]}}\n"
        );

        let providers = parse_agent_task_providers_output(stdout).expect("providers parse");

        assert!(provider_available(&providers, "extension-a", None));
        let mut selection = AgentTaskProviderSelection {
            backend: "extension-a".to_string(),
            selector: None,
            runtime_identity: None,
        };
        assert!(runner_provider_unavailable_reason(&providers, &selection).is_none());
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

        selection.selector = Some("missing".to_string());
        assert!(runner_provider_unavailable_reason(&providers, &selection)
            .expect("missing selector reason")
            .contains("does not match any"));

        let mut ambiguous = providers.clone();
        let mut second = ambiguous[0].clone();
        second.id = "extension-a.second".to_string();
        ambiguous.push(second);
        selection.selector = None;
        assert!(runner_provider_unavailable_reason(&ambiguous, &selection)
            .expect("ambiguous alias reason")
            .contains("matches extension alias"));

        selection.backend = "unknown".to_string();
        assert!(runner_provider_unavailable_reason(&ambiguous, &selection)
            .expect("unknown backend reason")
            .contains("none declare backend"));
    }

    #[test]
    fn provider_preflight_error_reports_local_runner_drift_and_no_cells_queued() {
        let selection = AgentTaskProviderSelection {
            backend: "primary".to_string(),
            selector: None,
            runtime_identity: None,
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
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("homeboy runner disconnect homeboy-lab"))));
        assert_eq!(
            err.details["runner_remediation_command"],
            serde_json::json!("homeboy runner disconnect homeboy-lab")
        );
    }

    #[test]
    fn provider_preflight_error_prioritizes_homeboy_identity_mismatch() {
        let selection = AgentTaskProviderSelection {
            backend: "primary".to_string(),
            selector: None,
            runtime_identity: None,
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
            None,
        );

        let tried = err.details["tried"].as_array().expect("tried hints");
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Controller Homeboy is `homeboy"))));
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("active daemon is `homeboy 0.0.0+old`"))));
        assert!(tried.iter().any(|hint| hint
            .as_str()
            .is_some_and(|hint| hint.contains("Refresh runner `homeboy-lab`"))));
    }

    #[test]
    fn provider_preflight_error_preserves_full_failed_command_output() {
        let selection = AgentTaskProviderSelection {
            backend: "opencode".to_string(),
            selector: Some("opencode.agent-task-executor".to_string()),
            runtime_identity: None,
        };
        let runner_homeboy = serde_json::json!({
            "refresh_commands": [
                "homeboy runner refresh-homeboy homeboy-lab --ref main --reconnect"
            ]
        });
        let output = AgentTaskProviderProbeOutput {
            stdout: "{\n  \"success\": false,\n  \"error\": {\n    \"message\": \"provider unavailable\"\n  }\n}\n".to_string(),
            stderr: "warning before json\nsecond diagnostic line\n".to_string(),
            exit_code: 2,
        };

        let err = agent_task_provider_selection_preflight_error(
            "homeboy-lab",
            &selection,
            true,
            Some(false),
            &runner_homeboy,
            Some("runner provider preflight command exited with 2; inspect details.command_output for full stdout/stderr".to_string()),
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "providers".to_string(),
            ],
            Some(&output),
            None,
        );

        assert!(err.message.contains("exited with 2"));
        assert!(!err.message.contains("exited with 2: {"));
        assert_eq!(
            err.details["command_output"]["exit_code"],
            serde_json::json!(2)
        );
        assert_eq!(
            err.details["command_output"]["stdout"],
            serde_json::json!(output.stdout)
        );
        assert_eq!(
            err.details["command_output"]["stderr"],
            serde_json::json!(output.stderr)
        );
        assert_eq!(
            err.details["runner_remediation_command"],
            serde_json::json!("homeboy runner refresh-homeboy homeboy-lab --ref main --reconnect")
        );
    }

    #[test]
    fn admission_probe_omits_source_snapshot_but_final_probe_retains_it() {
        let command = vec!["homeboy".to_string(), "agent-task".to_string()];
        let admission = provider_probe_options(
            &command,
            "/runner/workspace",
            Default::default(),
            None,
            Vec::new(),
            None,
        );
        assert!(admission.source_snapshot.is_none());

        let snapshot = SourceSnapshot {
            local_path: Some("/controller/workspace".to_string()),
            remote_path: Some("/runner/workspace".to_string()),
            ..Default::default()
        };
        let final_probe = provider_probe_options(
            &command,
            "/runner/workspace",
            Default::default(),
            Some(snapshot),
            Vec::new(),
            None,
        );
        assert!(final_probe.source_snapshot.is_some());
    }
}
