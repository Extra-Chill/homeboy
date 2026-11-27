use homeboy_agents::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy;
use homeboy_agents::agent_task_gate::VerifyGateOptions;
use homeboy_agents::agent_task_scheduler::AgentTaskPlan;
use homeboy_agents::agent_task_timeout::{
    effective_provider_max_timeout_ms, effective_provider_timeout_ms,
};
use homeboy_core::error::{Error, Result};
use homeboy_core::lab_contract::{
    lab_capability_is_pipeline_enforced, LabRunnerWorkload, LabRunnerWorkloadAgentTask,
    LabRunnerWorkloadAgentTaskDispatchKind, LabRunnerWorkloadAgentTaskHandoffMirrorPolicy,
    LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy, LabRunnerWorkloadAssignment,
    LabRunnerWorkloadCapability, LabRunnerWorkloadCommandFamily,
    LabRunnerWorkloadExtensionRevision, LabRunnerWorkloadKind, LabRunnerWorkloadMutationPolicy,
    LabRunnerWorkloadResultRefs, LabRunnerWorkloadSecrets, LabRunnerWorkloadState,
    LabRunnerWorkloadWorkspaceMappings, LAB_CAPABILITY_EXTENSION_PARITY,
    LAB_RUNNER_WORKLOAD_SCHEMA,
};
use homeboy_core::plan::HomeboyPlan;
use homeboy_core::secret_env_plan::SecretEnvPlan;

use super::{
    LabOffloadCommand, LabOffloadSourcePathMode, LabOffloadWorkspaceModePolicy,
    RunnerCapabilityPreflight, RunnerRequiredTool,
};

pub(crate) struct LabRunnerWorkloadBuildInput<'a> {
    pub plan: &'a HomeboyPlan,
    pub command: &'a LabOffloadCommand,
    pub capture_patch: bool,
    pub mutation_flag: Option<&'a str>,
    pub allow_dirty_lab_workspace: bool,
    pub runner_id: &'a str,
    pub runner_mode: &'a str,
    pub assignment_source: &'a str,
    pub status: &'a str,
    pub remote_workspace: Option<&'a str>,
    pub fallback_reason: Option<&'a str>,
    pub workspace_mapping_ref: Option<&'a str>,
    pub proof_id: Option<&'a str>,
}

/// Fixture builder for tests that need a workload but do not care which command
/// dispatched it.
///
/// It routes through the production builder with an empty dispatched command,
/// which is exactly the fallback path `build_lab_runner_workload_for_dispatched_command`
/// takes when it cannot derive an identity: `from_label(hot_label)`. The former
/// implementation reached the same identity through a separate
/// `_with_command_label` entry point that production never called, so these
/// tests were asserting against a builder no dispatch used.
#[cfg(test)]
pub(crate) fn build_lab_runner_workload(
    input: LabRunnerWorkloadBuildInput<'_>,
) -> LabRunnerWorkload {
    build_lab_runner_workload_for_dispatched_command(input, &[])
}

pub(crate) fn build_lab_runner_workload_for_dispatched_command(
    input: LabRunnerWorkloadBuildInput<'_>,
    dispatched_command: &[String],
) -> LabRunnerWorkload {
    let identity = dispatched_command_identity(dispatched_command)
        .unwrap_or_else(|| LabRunnerWorkloadCommandIdentity::from_label(&input.command.hot_label));
    build_lab_runner_workload_with_command_identity(input, identity)
}

fn build_lab_runner_workload_with_command_identity(
    input: LabRunnerWorkloadBuildInput<'_>,
    identity: LabRunnerWorkloadCommandIdentity,
) -> LabRunnerWorkload {
    let workspace_mapping_ref = input.workspace_mapping_ref.map(str::to_string);
    let required_secret_categories = required_secret_categories(&identity.label);
    LabRunnerWorkload {
        schema: LAB_RUNNER_WORKLOAD_SCHEMA.to_string(),
        workload_id: format!("{}.runner_workload", input.plan.id),
        kind: LabRunnerWorkloadKind {
            command_label: identity.label,
            command_family: identity.family,
        },
        agent_task: None,
        notification_route: homeboy_core::notification_route::current(),
        workspace_mappings: LabRunnerWorkloadWorkspaceMappings {
            source_path_mode: source_path_mode_label(input.command.source_path_mode).to_string(),
            workspace_mode_policy: workspace_mode_policy_label(input.command.workspace_mode_policy)
                .to_string(),
            mapping_ref: workspace_mapping_ref.clone(),
        },
        required_capabilities: required_capabilities(input.command),
        required_secrets: LabRunnerWorkloadSecrets {
            categories: required_secret_categories,
            secret_env_plan: SecretEnvPlan::default(),
        },
        required_extensions: input.command.required_extensions.clone(),
        required_extension_revisions: required_extension_revisions(
            &input.command.required_extensions,
        ),
        mutation_policy: LabRunnerWorkloadMutationPolicy {
            capture_patch: input.capture_patch,
            mutation_flag: input.mutation_flag.map(str::to_string),
            allow_dirty_lab_workspace: input.allow_dirty_lab_workspace,
        },
        assignment: LabRunnerWorkloadAssignment {
            runner_id: Some(input.runner_id.to_string()),
            runner_mode: Some(input.runner_mode.to_string()),
            source: Some(input.assignment_source.to_string()),
        },
        state: LabRunnerWorkloadState {
            status: input.status.to_string(),
            remote_workspace: input.remote_workspace.map(str::to_string),
            fallback_reason: input.fallback_reason.map(str::to_string),
        },
        result_refs: LabRunnerWorkloadResultRefs {
            plan_id: input.plan.id.clone(),
            proof_id: input.proof_id.map(str::to_string),
            workspace_mapping_ref,
            job_id: None,
            mirror_run_id: None,
            artifacts: Vec::new(),
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct LabRunnerWorkloadCommandIdentity {
    label: String,
    family: LabRunnerWorkloadCommandFamily,
}

impl LabRunnerWorkloadCommandIdentity {
    fn from_label(label: &str) -> Self {
        Self {
            label: label.to_string(),
            family: LabRunnerWorkloadCommandFamily::from_command_label(label),
        }
    }
}

/// Parse the dispatched argv through the CLI contract so target positionals do
/// not become part of a command label.
///
/// The actual CLI parse lives in the CLI layer and is invoked through the
/// `command_label` resolver hook, so core does not depend on the full CLI
/// parser (`cli_surface::Cli`).
fn dispatched_command_identity(command: &[String]) -> Option<LabRunnerWorkloadCommandIdentity> {
    let argv = cli_argv_from_dispatched_command(command)?;
    let label = super::resolve_command_label(&argv)?;
    Some(LabRunnerWorkloadCommandIdentity::from_label(&label))
}

fn cli_argv_from_dispatched_command(command: &[String]) -> Option<Vec<String>> {
    if command.is_empty() {
        return None;
    }
    if is_homeboy_executable(&command[0]) {
        return Some(command.to_vec());
    }
    let mut argv = Vec::with_capacity(command.len() + 1);
    argv.push("homeboy".to_string());
    argv.extend(command.iter().cloned());
    Some(argv)
}

fn is_homeboy_executable(value: &str) -> bool {
    std::path::Path::new(value)
        .file_name()
        .and_then(|name| name.to_str())
        == Some("homeboy")
}

pub(crate) fn lab_runner_workload_agent_task_from_command(
    args: &[String],
    run_id: Option<&str>,
    plan: Option<&AgentTaskPlan>,
) -> Option<LabRunnerWorkloadAgentTask> {
    let agent_task_index = args.iter().position(|arg| arg == "agent-task")?;
    let action = args.get(agent_task_index + 1)?.as_str();
    match action {
        // Cook and dispatch produce a handoff document, not a run-plan
        // aggregate, so they declare the handoff mirror policy and leave the
        // lifecycle (aggregate) mirror policy at `None`.
        "cook" => run_id.map(|run_id| LabRunnerWorkloadAgentTask {
            run_id: run_id.to_string(),
            plan_ref: None,
            resolved_provider_policy: resolved_provider_policy_from_command(args),
            dispatch_kind: LabRunnerWorkloadAgentTaskDispatchKind::Cook,
            lifecycle_mirror_policy: LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::None,
            handoff_mirror_policy: Some(
                LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::DispatchHandoff,
            ),
            wait_budget_ms: agent_task_wait_budget_ms(
                plan,
                args,
                cook_gate_budget_ms(args, agent_task_index),
            ),
        }),
        "dispatch" => run_id.map(|run_id| LabRunnerWorkloadAgentTask {
            run_id: run_id.to_string(),
            plan_ref: None,
            resolved_provider_policy: resolved_provider_policy_from_command(args),
            dispatch_kind: LabRunnerWorkloadAgentTaskDispatchKind::Dispatch,
            lifecycle_mirror_policy: LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::None,
            handoff_mirror_policy: Some(
                LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::DispatchHandoff,
            ),
            wait_budget_ms: agent_task_wait_budget_ms(plan, args, 0),
        }),
        "run-plan" => {
            let plan_ref = option_value_after(args, agent_task_index + 1, "--plan")?;
            let record_run_id = run_id
                .map(str::to_string)
                .or_else(|| option_value_after(args, agent_task_index + 1, "--record-run-id"))?;
            Some(LabRunnerWorkloadAgentTask {
                run_id: record_run_id,
                plan_ref: Some(plan_ref),
                resolved_provider_policy: None,
                dispatch_kind: LabRunnerWorkloadAgentTaskDispatchKind::RunPlan,
                lifecycle_mirror_policy:
                    LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate,
                // A run-plan declares no dispatch handoff.
                handoff_mirror_policy: Some(LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::None),
                wait_budget_ms: agent_task_wait_budget_ms(plan, args, 0),
            })
        }
        _ => None,
    }
}

/// The agent-task workload's own declared budget: every task's effective
/// provider max timeout plus the Cook gate budget.
///
/// An agent-task dispatch can reach Lab without a controller-materialized
/// plan: a fanout cook child routed by `--placement lab` selects its runner
/// inside the offload, so the controller never compiles the plan. This
/// function used to return `None` for that shape, which dropped the attached
/// `--wait` handoff onto the 20-minute transport default — the controller then
/// cancelled the runner job over its HTTP API while the provider's own
/// extended deadline (#15564: base timeout, extended in steps up to 3x while
/// there is progress) was still live, discarding the work (#16168). Without a
/// plan the same budget shape is derived from the dispatched command instead:
/// the effective provider max timeout for the command's `--timeout-ms` (or the
/// provider default) plus the gate budget.
fn agent_task_wait_budget_ms(
    plan: Option<&AgentTaskPlan>,
    args: &[String],
    gate_budget_ms: u64,
) -> Option<u64> {
    let provider_ms = match plan.filter(|plan| !plan.tasks.is_empty()) {
        Some(plan) => plan
            .tasks
            .iter()
            .map(|task| {
                effective_provider_max_timeout_ms(
                    effective_provider_timeout_ms(
                        task.limits.timeout_ms.or(plan.options.timeout_ms),
                        task.limits.max_runtime_ms,
                    ),
                    task.limits.max_timeout_ms,
                )
            })
            .fold(0, u64::saturating_add),
        None => effective_provider_max_timeout_ms(
            effective_provider_timeout_ms(command_provider_timeout_ms(args), None),
            None,
        ),
    };
    Some(provider_ms.saturating_add(gate_budget_ms))
}

/// The dispatched command's own `--timeout-ms`, when present. This is the same
/// base the remote provider resolves, so a plan-less wait budget caps where
/// the provider's extension cap does.
fn command_provider_timeout_ms(args: &[String]) -> Option<u64> {
    option_value_after(args, 0, "--timeout-ms").and_then(|value| value.parse::<u64>().ok())
}

fn cook_gate_budget_ms(args: &[String], agent_task_index: usize) -> u64 {
    option_value_after(args, agent_task_index + 1, "--gate-timeout-seconds")
        .and_then(|seconds| seconds.parse::<u64>().ok())
        .unwrap_or_else(|| VerifyGateOptions::default().gate_timeout_seconds)
        .saturating_mul(1_000)
}

fn resolved_provider_policy_from_command(
    args: &[String],
) -> Option<ResolvedAgentTaskProviderPolicy> {
    option_value_after(args, 0, "--resolved-provider-policy")
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn option_value_after(args: &[String], start_index: usize, option: &str) -> Option<String> {
    let prefix = format!("{option}=");
    let mut iter = args.iter().skip(start_index + 1);
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == option {
            return iter.next().cloned();
        }
        if let Some(value) = arg.strip_prefix(&prefix) {
            return Some(value.to_string());
        }
    }
    None
}

fn required_extension_revisions(
    required_extensions: &[String],
) -> Vec<LabRunnerWorkloadExtensionRevision> {
    required_extension_revisions_with(
        required_extensions,
        homeboy_core::extension::lifecycle::read_source_revision,
    )
}

fn required_extension_revisions_with(
    required_extensions: &[String],
    mut read_revision: impl FnMut(&str) -> Option<String>,
) -> Vec<LabRunnerWorkloadExtensionRevision> {
    required_extensions
        .iter()
        .filter_map(|extension_id| {
            read_revision(extension_id).map(|source_revision| LabRunnerWorkloadExtensionRevision {
                extension_id: extension_id.clone(),
                source_revision,
            })
        })
        .collect()
}

pub(crate) fn validate_lab_runner_workload_dispatch(
    workload: Option<&LabRunnerWorkload>,
    runner_id: &str,
    cwd: Option<&str>,
    command: &[String],
    secret_env_plan: &SecretEnvPlan,
    capture_patch: bool,
) -> Result<()> {
    let Some(workload) = workload else {
        return Ok(());
    };
    validate_lab_runner_workload_dispatch_identity(workload, runner_id, cwd)?;
    validate_lab_runner_workload_mutation_policy(workload, capture_patch)?;
    validate_lab_runner_workload_required_secrets(workload, secret_env_plan)?;
    validate_lab_runner_workload_required_capabilities(workload)?;
    validate_lab_runner_workload_command_present(command)?;
    validate_lab_runner_workload_command(workload, command)?;
    Ok(())
}

fn validate_lab_runner_workload_dispatch_identity(
    workload: &LabRunnerWorkload,
    runner_id: &str,
    cwd: Option<&str>,
) -> Result<()> {
    if workload.schema != LAB_RUNNER_WORKLOAD_SCHEMA {
        return Err(workload_error(
            "runner_workload.schema",
            format!("runner workload schema must be `{LAB_RUNNER_WORKLOAD_SCHEMA}`"),
        ));
    }
    validate_lab_runner_workload_result_refs(workload)?;
    if workload.assignment.runner_id.as_deref() != Some(runner_id) {
        return Err(workload_error(
            "runner_workload.assignment.runner_id",
            "runner workload assignment does not match dispatch runner_id".to_string(),
        ));
    }
    if let (Some(expected), Some(actual)) = (workload.state.remote_workspace.as_deref(), cwd) {
        if expected != actual {
            return Err(workload_error(
                "runner_workload.state.remote_workspace",
                "runner workload remote workspace does not match dispatch cwd".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_lab_runner_workload_mutation_policy(
    workload: &LabRunnerWorkload,
    capture_patch: bool,
) -> Result<()> {
    if workload.mutation_policy.capture_patch != capture_patch {
        return Err(workload_error(
            "runner_workload.mutation_policy.capture_patch",
            "runner workload mutation policy does not match dispatch capture_patch".to_string(),
        ));
    }
    Ok(())
}

fn validate_lab_runner_workload_required_secrets(
    workload: &LabRunnerWorkload,
    secret_env_plan: &SecretEnvPlan,
) -> Result<()> {
    let expected = workload.required_secrets.secret_env_plan.secret_env_names();
    let declared = secret_env_plan.secret_env_names();
    let missing = expected
        .iter()
        .filter(|name| !declared.contains(name))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(workload_error(
            "runner_workload.required_secrets",
            format!(
                "runner workload secret handoff is missing required env names: {}",
                missing.join(", ")
            ),
        ));
    }
    Ok(())
}

fn validate_lab_runner_workload_required_capabilities(workload: &LabRunnerWorkload) -> Result<()> {
    for capability in &workload.required_capabilities {
        if capability.name.trim().is_empty() {
            return Err(workload_error(
                "runner_workload.required_capabilities",
                "runner workload capability names must not be empty".to_string(),
            ));
        }
    }
    Ok(())
}

fn validate_lab_runner_workload_command_present(command: &[String]) -> Result<()> {
    if command.is_empty() {
        return Err(workload_error(
            "runner_workload.command",
            "runner workload dispatch requires a command".to_string(),
        ));
    }
    Ok(())
}

fn validate_lab_runner_workload_command(
    workload: &LabRunnerWorkload,
    command: &[String],
) -> Result<()> {
    if let Some(dispatched_identity) = dispatched_command_identity(command) {
        return validate_lab_runner_workload_command_identity(workload, dispatched_identity);
    }

    // Some direct runner validations use incomplete argv that cannot be parsed
    // as a full CLI invocation. Preserve their strict label validation rather
    // than accepting an unrecognized command.
    validate_lab_runner_workload_command_fallback(workload, command)
}

fn validate_lab_runner_workload_command_identity(
    workload: &LabRunnerWorkload,
    dispatched_identity: LabRunnerWorkloadCommandIdentity,
) -> Result<()> {
    if dispatched_identity.label != workload.kind.command_label {
        return Err(workload_error(
            "runner_workload.kind.command_label",
            format!(
                "runner workload command label `{}` does not match dispatched command `{}`",
                workload.kind.command_label, dispatched_identity.label
            ),
        ));
    }

    if dispatched_identity.family != LabRunnerWorkloadCommandFamily::Unknown
        && dispatched_identity.family != workload.kind.command_family
    {
        return Err(workload_error(
            "runner_workload.kind.command_family",
            format!(
                "runner workload command family `{:?}` does not match dispatched command family `{:?}`",
                workload.kind.command_family, dispatched_identity.family
            ),
        ));
    }

    Ok(())
}

fn validate_lab_runner_workload_command_fallback(
    workload: &LabRunnerWorkload,
    command: &[String],
) -> Result<()> {
    let command_args = dispatch_workload_command_args(command);
    if command_args.is_empty() {
        return Err(workload_error(
            "runner_workload.command",
            "runner workload dispatch requires a command label".to_string(),
        ));
    }

    let dispatched_family = dispatched_command_family(command_args);

    if let Some(dispatched_label) =
        dispatched_command_label(command_args, &workload.kind.command_label)
    {
        // A differing surface label is only drift when the commands are not the
        // same workload family. Same-family labels (e.g. `test` and `trace`,
        // both Quality) canonicalize to one lab-runner-workload identity, so a
        // labelled dispatch that resolves to the workload's family is a match
        // even when the surface label differs (#7972). When the dispatched
        // family is Unknown we cannot canonicalize, so fall back to strict
        // label matching.
        let canonical_same_family = dispatched_family != LabRunnerWorkloadCommandFamily::Unknown
            && dispatched_family == workload.kind.command_family;
        if dispatched_label != workload.kind.command_label && !canonical_same_family {
            return Err(workload_error(
                "runner_workload.kind.command_label",
                format!(
                    "runner workload command label `{}` does not match dispatched command `{dispatched_label}`",
                    workload.kind.command_label
                ),
            ));
        }
    }

    if dispatched_family != LabRunnerWorkloadCommandFamily::Unknown
        && dispatched_family != workload.kind.command_family
    {
        return Err(workload_error(
            "runner_workload.kind.command_family",
            format!(
                "runner workload command family `{:?}` does not match dispatched command family `{:?}`",
                workload.kind.command_family, dispatched_family
            ),
        ));
    }

    Ok(())
}

fn validate_lab_runner_workload_result_refs(workload: &LabRunnerWorkload) -> Result<()> {
    if workload.result_refs.plan_id.trim().is_empty() {
        return Err(workload_error(
            "runner_workload.result_refs.plan_id",
            "runner workload result refs require a plan_id".to_string(),
        ));
    }

    if let Some(expected_plan_id) = workload.workload_id.strip_suffix(".runner_workload") {
        if workload.result_refs.plan_id != expected_plan_id {
            return Err(workload_error(
                "runner_workload.result_refs.plan_id",
                format!(
                    "runner workload result refs plan_id `{}` does not match workload_id plan `{expected_plan_id}`",
                    workload.result_refs.plan_id
                ),
            ));
        }
    }

    if workload.workspace_mappings.mapping_ref != workload.result_refs.workspace_mapping_ref {
        return Err(workload_error(
            "runner_workload.result_refs.workspace_mapping_ref",
            "runner workload workspace mapping references do not match".to_string(),
        ));
    }

    if workload
        .result_refs
        .artifacts
        .iter()
        .any(|artifact| artifact.id.trim().is_empty())
    {
        return Err(workload_error(
            "runner_workload.result_refs.artifacts",
            "runner workload result refs artifacts require non-empty ids".to_string(),
        ));
    }

    Ok(())
}

fn dispatch_workload_command_args(command: &[String]) -> &[String] {
    let args = match command.first().map(String::as_str) {
        Some(binary) if is_homeboy_binary(binary) => &command[1..],
        _ => command,
    };
    args
}

fn is_homeboy_binary(binary: &str) -> bool {
    std::path::Path::new(binary)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == "homeboy")
}

fn dispatched_command_label(command_args: &[String], expected_label: &str) -> Option<String> {
    let expected_parts: Vec<&str> = expected_label.split_whitespace().collect();
    if expected_parts.is_empty()
        || expected_parts
            .iter()
            .any(|part| part.starts_with('-') || part.contains('/'))
    {
        return None;
    }

    let positional_parts = dispatched_positional_command_parts(command_args);
    if positional_parts.len() < expected_parts.len() {
        return None;
    }

    Some(positional_parts[..expected_parts.len()].join(" "))
}

fn lab_runner_workload_command_label_value_option(arg: &str) -> bool {
    matches!(arg, "--extension" | "--path")
}

fn lab_runner_workload_command_label_flag_option(arg: &str) -> bool {
    arg.starts_with("--extension=") || arg.starts_with("--path=")
}

fn dispatched_command_family(command_args: &[String]) -> LabRunnerWorkloadCommandFamily {
    let positional_parts = dispatched_positional_command_parts(command_args);
    let max_parts = positional_parts.len().min(3);
    for part_count in (1..=max_parts).rev() {
        let label = positional_parts[..part_count].join(" ");
        let family = LabRunnerWorkloadCommandFamily::from_command_label(&label);
        if family != LabRunnerWorkloadCommandFamily::Unknown {
            return family;
        }
    }
    LabRunnerWorkloadCommandFamily::Unknown
}

fn dispatched_positional_command_parts(command_args: &[String]) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut iter = command_args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if lab_runner_workload_command_label_value_option(arg) {
            let _ = iter.next();
            continue;
        }
        if lab_runner_workload_command_label_flag_option(arg) {
            continue;
        }
        if arg.starts_with('-') {
            if !arg.contains('=') && iter.peek().is_some_and(|next| !next.starts_with('-')) {
                let _ = iter.next();
            }
            continue;
        }
        parts.push(arg.as_str());
    }
    parts
}

/// Union the workload's declared capabilities into the dispatch capability
/// preflight so a runner that cannot satisfy them is refused before execution.
///
/// The workload is the controller's declaration of what the runner must be able
/// to do; without this merge those declarations are shipped to the runner and
/// never gated (Extra-Chill/homeboy#10287). Capability names map to runner tool
/// requirements — the declarative tool registry probes any tool id — except for
/// pipeline-enforced capabilities like `extension_parity`, which are gated by
/// their own execution-pipeline step and name no runner binary.
pub(crate) fn merge_lab_runner_workload_capability_preflight(
    mut preflight: Option<RunnerCapabilityPreflight>,
    workload: Option<&LabRunnerWorkload>,
) -> Option<RunnerCapabilityPreflight> {
    let Some(workload) = workload else {
        return preflight;
    };
    for capability in &workload.required_capabilities {
        if !capability.required {
            continue;
        }
        let name = capability.name.trim();
        if name.is_empty() || lab_capability_is_pipeline_enforced(name) {
            continue;
        }
        let tool = RunnerRequiredTool::new(name);
        let preflight = preflight.get_or_insert_with(|| RunnerCapabilityPreflight {
            command: workload.kind.command_label.clone(),
            ..Default::default()
        });
        if !preflight.required_tools.contains(&tool) {
            preflight.required_tools.push(tool);
        }
    }
    preflight
}

pub(crate) fn merge_lab_runner_workload_required_extensions(
    mut required_extensions: Vec<String>,
    workload: Option<&LabRunnerWorkload>,
) -> Vec<String> {
    if let Some(workload) = workload {
        for extension in &workload.required_extensions {
            if !required_extensions.contains(extension) {
                required_extensions.push(extension.clone());
            }
        }
    }
    required_extensions
}

fn workload_error(field: &str, message: String) -> Error {
    Error::validation_invalid_argument(field, message, None, None)
}

fn required_capabilities(command: &LabOffloadCommand) -> Vec<LabRunnerWorkloadCapability> {
    let mut capabilities = Vec::new();
    if command.routing_policy.requires_extension_parity || !command.required_extensions.is_empty() {
        capabilities.push(LabRunnerWorkloadCapability {
            name: LAB_CAPABILITY_EXTENSION_PARITY.to_string(),
            required: true,
        });
    }
    for capability in &command.required_capabilities {
        if !capabilities
            .iter()
            .any(|existing| existing.name == capability.name)
        {
            capabilities.push(capability.clone());
        }
    }
    capabilities
}

fn required_secret_categories(hot_label: &str) -> Vec<String> {
    match hot_label {
        label if label.starts_with("agent-task") => vec!["agent_task".to_string()],
        "trace" => vec!["trace".to_string()],
        label if label.starts_with("tunnel") => vec!["tunnel".to_string()],
        _ => Vec::new(),
    }
}

fn source_path_mode_label(mode: LabOffloadSourcePathMode) -> &'static str {
    match mode {
        LabOffloadSourcePathMode::CwdOrPathFlag => "cwd_or_path_flag",
        LabOffloadSourcePathMode::RunnerResident => "runner_resident",
    }
}

fn workspace_mode_policy_label(policy: LabOffloadWorkspaceModePolicy) -> &'static str {
    match policy {
        LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot => {
            "changed_since_git_else_snapshot"
        }
        LabOffloadWorkspaceModePolicy::Git => "git",
        LabOffloadWorkspaceModePolicy::GitCheckoutRequired => "git_checkout_required",
        LabOffloadWorkspaceModePolicy::RunnerResident => "runner_resident",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::plan::{HomeboyPlan, PlanKind};

    /// Register a stub command-label resolver for tests.
    ///
    /// Production wires this hook to the real CLI parser (`cli_surface::Cli`) at
    /// startup; the parser's correctness is covered by the CLI layer's own
    /// tests. Here we only exercise core's workload-building behaviour given a
    /// resolved label, so the stub maps each test argv to its expected label via
    /// the shared `command_label_for_test_argv` table (keeping core free of any
    /// dependency on the CLI parser).
    fn register_test_command_label_resolver() {
        crate::set_command_label_resolver(command_label_for_test_argv);
    }

    /// Test-only argv -> hot-command-label mapping mirroring what the production
    /// CLI parser would resolve for the argv shapes exercised in these tests.
    fn command_label_for_test_argv(argv: &[String]) -> Option<String> {
        let tokens: Vec<&str> = argv.iter().map(String::as_str).collect();
        let label = match tokens.as_slice() {
            [_, "review", "lint", ..] => "review lint",
            [_, "agent-task", "cook", ..] => "agent-task cook/run-plan/retry --run",
            [_, "extension", "update", ..] => "extension update",
            [_, "runtime", "refresh", ..] => "runtime refresh",
            [_, "agent-task", "status", ..] => {
                "agent-task run/run-next/status/review; runs logs/list/artifacts"
            }
            _ => return None,
        };
        Some(label.to_string())
    }

    fn plan() -> HomeboyPlan {
        HomeboyPlan::builder_for_description(PlanKind::LabOffload, "test").build()
    }

    fn command() -> LabOffloadCommand {
        LabOffloadCommand {
            required_extensions: vec!["browser".to_string()],
            required_capabilities: vec![LabRunnerWorkloadCapability {
                name: "playwright".to_string(),
                required: true,
            }],
            workload: None,
            ..homeboy_core::lab_routing::lab_offload_command_from_contract(
                homeboy_core::lab_contract::LabCommandContract::portable("trace", None, true, &[]),
                Vec::new(),
            )
        }
    }

    fn workload() -> LabRunnerWorkload {
        let plan = plan();
        let command = command();
        build_lab_runner_workload(LabRunnerWorkloadBuildInput {
            plan: &plan,
            command: &command,
            capture_patch: true,
            mutation_flag: None,
            allow_dirty_lab_workspace: false,
            runner_id: "lab-a",
            runner_mode: "direct_ssh",
            assignment_source: "explicit",
            status: "offloaded",
            remote_workspace: Some("/srv/homeboy/work"),
            fallback_reason: None,
            workspace_mapping_ref: None,
            proof_id: None,
        })
    }

    #[test]
    fn lab_runner_workload_kind_comes_from_dispatched_command_for_lab_shapes() {
        struct Case {
            name: &'static str,
            argv: Vec<&'static str>,
            expected_label: &'static str,
            expected_family: LabRunnerWorkloadCommandFamily,
        }

        let cases = [
            Case {
                name: "review lint component",
                argv: vec!["/srv/homeboy/bin/homeboy", "review", "lint", "homeboy"],
                expected_label: "review lint",
                expected_family: LabRunnerWorkloadCommandFamily::Quality,
            },
            Case {
                name: "agent-task cook",
                argv: vec![
                    "/srv/homeboy/bin/homeboy",
                    "agent-task",
                    "cook",
                    "--to-worktree",
                    "homeboy@cook",
                    "--goal",
                    "tighten lab dispatch metadata",
                    "--verify",
                    "cargo check --all-targets",
                    "--no-finalize",
                ],
                expected_label: "agent-task cook/run-plan/retry --run",
                expected_family: LabRunnerWorkloadCommandFamily::AgentTask,
            },
            Case {
                name: "plain offloaded command",
                argv: vec![
                    "/srv/homeboy/bin/homeboy",
                    "extension",
                    "update",
                    "wordpress",
                ],
                expected_label: "extension update",
                expected_family: LabRunnerWorkloadCommandFamily::Unknown,
            },
            Case {
                name: "runtime refresh target positional",
                argv: vec![
                    "/srv/homeboy/bin/homeboy",
                    "runtime",
                    "refresh",
                    "wp-codebox",
                    "--source",
                    "https://github.com/Automattic/wp-codebox.git",
                    "--ref",
                    "df634b9330be1f055df22fbe6d473a8ee88022ad",
                    "--runner",
                    "homeboy-lab",
                ],
                expected_label: "runtime refresh",
                expected_family: LabRunnerWorkloadCommandFamily::Workspace,
            },
            Case {
                name: "resident-path command",
                argv: vec!["/srv/homeboy/bin/homeboy", "agent-task", "status", "run-1"],
                expected_label: "agent-task run/run-next/status/review; runs logs/list/artifacts",
                expected_family: LabRunnerWorkloadCommandFamily::AgentTask,
            },
        ];

        for case in cases {
            let plan = plan();
            let mut command = command();
            command.hot_label = "trace".to_string();
            let dispatched_command = case
                .argv
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>();

            register_test_command_label_resolver();
            let workload = build_lab_runner_workload_for_dispatched_command(
                LabRunnerWorkloadBuildInput {
                    plan: &plan,
                    command: &command,
                    capture_patch: true,
                    mutation_flag: None,
                    allow_dirty_lab_workspace: false,
                    runner_id: "lab-a",
                    runner_mode: "direct_ssh",
                    assignment_source: "explicit",
                    status: "offloaded",
                    remote_workspace: Some("/srv/homeboy/work"),
                    fallback_reason: None,
                    workspace_mapping_ref: Some("path_materialization_plan"),
                    proof_id: None,
                },
                &dispatched_command,
            );

            assert_eq!(
                workload.kind.command_label, case.expected_label,
                "{}",
                case.name
            );
            assert_eq!(
                workload.kind.command_family, case.expected_family,
                "{}",
                case.name
            );
            assert_eq!(
                workload.workspace_mappings.mapping_ref.as_deref(),
                Some("path_materialization_plan"),
                "{}",
                case.name
            );
            assert_eq!(
                workload.result_refs.workspace_mapping_ref, workload.workspace_mappings.mapping_ref,
                "{}",
                case.name
            );
        }
    }

    fn secret_plan(names: &[&str]) -> SecretEnvPlan {
        SecretEnvPlan::from_secret_env_names(names.iter().map(|name| name.to_string()))
    }

    fn workload_with(
        hot_label: Option<&str>,
        workspace_mapping_ref: Option<&'static str>,
    ) -> LabRunnerWorkload {
        let plan = plan();
        let mut command = command();
        if let Some(label) = hot_label {
            command.hot_label = label.to_string();
        }
        build_lab_runner_workload(LabRunnerWorkloadBuildInput {
            plan: &plan,
            command: &command,
            capture_patch: true,
            mutation_flag: None,
            allow_dirty_lab_workspace: false,
            runner_id: "lab-a",
            runner_mode: "direct_ssh",
            assignment_source: "explicit",
            status: "offloaded",
            remote_workspace: Some("/srv/homeboy/work"),
            fallback_reason: None,
            workspace_mapping_ref,
            proof_id: None,
        })
    }

    fn argv(args: &[&str]) -> Vec<String> {
        args.iter().map(|arg| (*arg).to_string()).collect()
    }

    #[test]
    fn runner_core_builds_workload_contract_from_the_offloaded_command() {
        let workload = workload();

        assert_eq!(workload.kind.command_label, "trace");
        assert_eq!(
            workload.kind.command_family,
            LabRunnerWorkloadCommandFamily::Quality
        );
        // The pipeline-enforced parity gate is always declared ahead of the
        // command's own capabilities.
        let capabilities = workload
            .required_capabilities
            .iter()
            .map(|capability| capability.name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(capabilities, ["extension_parity", "playwright"]);
        assert_eq!(workload.required_secrets.categories, vec!["trace"]);
        assert_eq!(workload.result_refs.plan_id, "lab_offload.test");
    }

    /// A workload-declared tool capability must reach the dispatch preflight, or
    /// the runner is never gated on it (Extra-Chill/homeboy#10287).
    /// `extension_parity` names an execution-pipeline gate, not a runner binary:
    /// merging it as a required tool would refuse every parity-bearing dispatch.
    #[test]
    fn lab_runner_workload_tool_capabilities_merge_into_preflight() {
        let workload = workload();

        let preflight = merge_lab_runner_workload_capability_preflight(None, Some(&workload))
            .expect("workload capabilities create a preflight");

        assert_eq!(preflight.command, "trace");
        assert!(preflight
            .required_tools
            .contains(&RunnerRequiredTool::new("playwright")));
        assert!(!preflight
            .required_tools
            .contains(&RunnerRequiredTool::new(LAB_CAPABILITY_EXTENSION_PARITY)));
    }

    /// Merging must extend an explicit preflight rather than replace it, and must
    /// leave the caller's preflight untouched when there is no workload.
    #[test]
    fn capability_merge_preserves_the_explicit_preflight_and_no_workload_case() {
        let workload = workload();
        let explicit = RunnerCapabilityPreflight {
            command: "runner.exec".to_string(),
            required_tools: vec![RunnerRequiredTool::homeboy()],
            ..Default::default()
        };

        let merged =
            merge_lab_runner_workload_capability_preflight(Some(explicit.clone()), Some(&workload))
                .expect("explicit preflight is retained");

        assert_eq!(merged.command, "runner.exec");
        assert!(merged
            .required_tools
            .contains(&RunnerRequiredTool::homeboy()));
        assert!(merged
            .required_tools
            .contains(&RunnerRequiredTool::new("playwright")));

        assert_eq!(
            merge_lab_runner_workload_capability_preflight(Some(explicit.clone()), None),
            Some(explicit)
        );
        assert_eq!(
            merge_lab_runner_workload_capability_preflight(None, None),
            None
        );
    }

    #[test]
    fn lab_runner_workload_extension_revisions_pin_installed_required_extensions() {
        let revisions = required_extension_revisions_with(
            &["browser".to_string(), "missing".to_string()],
            |extension_id| match extension_id {
                "browser" => Some("abc1234".to_string()),
                _ => None,
            },
        );

        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].extension_id, "browser");
        assert_eq!(revisions[0].source_revision, "abc1234");
    }

    #[test]
    fn lab_runner_workload_validation_rejects_dispatch_drift() {
        let mut mapping_drift = workload_with(None, Some("path_materialization_plan"));
        mapping_drift.result_refs.workspace_mapping_ref = Some("workspace_mapping".to_string());
        // A dispatch whose label belongs to no known workload family (here
        // `status`) cannot be canonicalized to this `trace` (Quality) workload,
        // so the strict label check still rejects genuine command drift.
        for (workload, runner_id, remote_workspace, dispatched, field) in [
            (
                workload(),
                "other-runner",
                "/srv/homeboy/work",
                argv(&["homeboy", "trace"]),
                "runner_workload.assignment.runner_id",
            ),
            (
                workload(),
                "lab-a",
                "/srv/homeboy/other-workspace",
                argv(&["homeboy", "trace"]),
                "runner_workload.state.remote_workspace",
            ),
            (
                workload(),
                "lab-a",
                "/srv/homeboy/work",
                argv(&["homeboy", "status"]),
                "runner_workload.kind.command_label",
            ),
            (
                mapping_drift,
                "lab-a",
                "/srv/homeboy/work",
                argv(&["homeboy", "trace"]),
                "runner_workload.result_refs.workspace_mapping_ref",
            ),
        ] {
            let err = validate_lab_runner_workload_dispatch(
                Some(&workload),
                runner_id,
                Some(remote_workspace),
                &dispatched,
                &SecretEnvPlan::default(),
                true,
            )
            .expect_err("drifted dispatch must fail");
            assert_eq!(err.details["field"], field);
        }
    }

    #[test]
    fn lab_runner_workload_validation_rejects_invalid_result_refs() {
        let blank_plan_id: fn(&mut LabRunnerWorkload) = |w| w.result_refs.plan_id = " ".into();
        let mismatched_plan_id: fn(&mut LabRunnerWorkload) =
            |w| w.result_refs.plan_id = "lab_offload.other".into();
        let blank_artifact_id: fn(&mut LabRunnerWorkload) = |w| {
            w.result_refs.artifacts = vec![homeboy_core::lab_contract::JobArtifactMetadata {
                id: " ".to_string(),
                name: Some("trace.zip".to_string()),
                path: Some("/tmp/trace.zip".to_string()),
                ..Default::default()
            }]
        };
        for (edit, field) in [
            (blank_plan_id, "runner_workload.result_refs.plan_id"),
            (mismatched_plan_id, "runner_workload.result_refs.plan_id"),
            (blank_artifact_id, "runner_workload.result_refs.artifacts"),
        ] {
            let mut workload = workload();
            edit(&mut workload);
            let err = validate_lab_runner_workload_dispatch(
                Some(&workload),
                "lab-a",
                Some("/srv/homeboy/work"),
                &["homeboy".to_string(), "trace".to_string()],
                &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
                true,
            )
            .expect_err("invalid result refs must fail");
            assert_eq!(err.details["field"], field);
        }
    }

    #[test]
    fn lab_runner_workload_validation_uses_runtime_refresh_command_identity() {
        let plan = plan();
        let command = command();
        let dispatched_command = [
            "homeboy",
            "runtime",
            "refresh",
            "wp-codebox",
            "--source",
            "https://github.com/Automattic/wp-codebox.git",
            "--ref",
            "df634b9330be1f055df22fbe6d473a8ee88022ad",
            "--runner",
            "homeboy-lab",
        ]
        .map(str::to_string);
        register_test_command_label_resolver();
        let workload = build_lab_runner_workload_for_dispatched_command(
            LabRunnerWorkloadBuildInput {
                plan: &plan,
                command: &command,
                capture_patch: true,
                mutation_flag: None,
                allow_dirty_lab_workspace: false,
                runner_id: "lab-a",
                runner_mode: "direct_ssh",
                assignment_source: "explicit",
                status: "offloaded",
                remote_workspace: Some("/srv/homeboy/work"),
                fallback_reason: None,
                workspace_mapping_ref: None,
                proof_id: None,
            },
            &dispatched_command,
        );

        assert_eq!(workload.kind.command_label, "runtime refresh");
        validate_lab_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &dispatched_command,
            &SecretEnvPlan::default(),
            true,
        )
        .expect("runtime refresh target positional must not alter the command identity");

        let err = validate_lab_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &SecretEnvPlan::default(),
            true,
        )
        .expect_err("a genuinely different dispatched command must fail");
        // `runtime refresh` (Workspace family) and `trace` (Quality family) are
        // different workload families, so the mismatch surfaces as a command
        // family drift rather than a label drift (#7972 canonicalizes same-family
        // labels but still enforces the family).
        assert_eq!(err.details["field"], "runner_workload.kind.command_family");
        assert!(err.message.contains("Workspace"));
        assert!(err.message.contains("Quality"));
    }

    #[test]
    fn lab_runner_workload_validation_accepts_matching_command_argv_shapes() {
        let workload = workload();
        // Full, short, configured-executable and same-family alias (`test`) argv.
        for dispatched in [
            argv(&["homeboy", "trace"]),
            argv(&["trace"]),
            argv(&["/srv/homeboy/bin/homeboy", "trace"]),
            argv(&["homeboy", "test"]),
        ] {
            validate_lab_runner_workload_dispatch(
                Some(&workload),
                "lab-a",
                Some("/srv/homeboy/work"),
                &dispatched,
                &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
                true,
            )
            .unwrap_or_else(|err| panic!("{dispatched:?} must be valid: {err:?}"));
        }

        let fanout = workload_with(Some("agent-task fanout cook-batch"), None);
        validate_lab_runner_workload_dispatch(
            Some(&fanout),
            "lab-a",
            Some("/srv/homeboy/work"),
            &argv(&[
                "homeboy",
                "agent-task",
                "fanout",
                "cook-batch",
                "--repo",
                "homeboy",
                "--run-plan",
            ]),
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("matching fanout cook-batch argv is valid");
    }

    #[test]
    fn lab_runner_workload_validation_accepts_nested_review_quality_labels() {
        for label in ["review lint", "review test", "review build", "review ci"] {
            let plan = plan();
            let mut command = command();
            command.hot_label = label.to_string();
            command.required_extensions.clear();
            command.required_capabilities.clear();
            let workload = build_lab_runner_workload(LabRunnerWorkloadBuildInput {
                plan: &plan,
                command: &command,
                capture_patch: false,
                mutation_flag: None,
                allow_dirty_lab_workspace: false,
                runner_id: "lab-a",
                runner_mode: "direct_ssh",
                assignment_source: "explicit",
                status: "offloaded",
                remote_workspace: Some("/srv/homeboy/work"),
                fallback_reason: None,
                workspace_mapping_ref: None,
                proof_id: None,
            });
            let mut argv = label
                .split_whitespace()
                .map(str::to_string)
                .collect::<Vec<_>>();
            argv.push("data-machine".to_string());

            assert_eq!(
                workload.kind.command_family,
                LabRunnerWorkloadCommandFamily::Quality
            );
            validate_lab_runner_workload_dispatch(
                Some(&workload),
                "lab-a",
                Some("/srv/homeboy/work"),
                &argv,
                &SecretEnvPlan::default(),
                false,
            )
            .expect("nested review quality command label is valid");

            let mut argv_with_extension = vec![
                "review".to_string(),
                "--extension".to_string(),
                "wordpress".to_string(),
            ];
            argv_with_extension.extend(label.split_whitespace().skip(1).map(str::to_string));
            argv_with_extension.push("data-machine".to_string());
            validate_lab_runner_workload_dispatch(
                Some(&workload),
                "lab-a",
                Some("/srv/homeboy/work"),
                &argv_with_extension,
                &SecretEnvPlan::default(),
                false,
            )
            .expect("nested review quality command label tolerates interleaved options");
        }
    }

    #[test]
    fn lab_runner_workload_validation_rejects_missing_or_wrong_secret_handoff_names() {
        for (label, secret, dispatched, handoff) in [
            (
                None,
                "HOMEBODY_TRACE_SECRET",
                argv(&["homeboy", "trace", "--secret-env=HOMEBODY_TRACE_SECRET"]),
                SecretEnvPlan::default(),
            ),
            (
                None,
                "HOMEBODY_TRACE_SECRET",
                argv(&["homeboy", "trace", "--secret-env=HOMEBODY_TRACE_SECRET"]),
                secret_plan(&["WRONG_SECRET"]),
            ),
            (
                Some("agent-task dispatch"),
                "AGENT_TASK_SECRET",
                argv(&[
                    "homeboy",
                    "agent-task",
                    "dispatch",
                    "--secret-env=AGENT_TASK_SECRET",
                ]),
                secret_plan(&["WRONG_SECRET"]),
            ),
            (
                Some("tunnel preview-client start"),
                "HOMEBOY_PREVIEW_TUNNEL_TOKEN",
                argv(&[
                    "homeboy",
                    "tunnel",
                    "preview-client",
                    "start",
                    "--ingress",
                    "https://preview-broker.example.test",
                    "--public-host",
                    "preview.example.test",
                    "--local-origin",
                    "http://127.0.0.1:8888",
                ]),
                SecretEnvPlan::default(),
            ),
        ] {
            let mut workload = workload_with(label, None);
            workload.required_secrets.secret_env_plan = secret_plan(&[secret]);

            let err = validate_lab_runner_workload_dispatch(
                Some(&workload),
                "lab-a",
                Some("/srv/homeboy/work"),
                &dispatched,
                &handoff,
                true,
            )
            .expect_err("missing or wrong secret handoff name must fail");
            assert_eq!(err.details["field"], "runner_workload.required_secrets");
            assert!(err.message.contains(secret), "{secret}: {}", err.message);
        }
    }

    #[test]
    fn lab_runner_workload_validation_accepts_named_or_unneeded_secret_handoff() {
        let mut trace = workload();
        trace.required_secrets.secret_env_plan = secret_plan(&["HOMEBODY_TRACE_SECRET"]);
        validate_lab_runner_workload_dispatch(
            Some(&trace),
            "lab-a",
            Some("/srv/homeboy/work"),
            &argv(&["homeboy", "trace"]),
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("required secret category with named handoff is valid");

        let lint = workload_with(Some("lint"), None);
        validate_lab_runner_workload_dispatch(
            Some(&lint),
            "lab-a",
            Some("/srv/homeboy/work"),
            &argv(&["homeboy", "lint"]),
            &SecretEnvPlan::default(),
            true,
        )
        .expect("empty secret handoff is valid when no categories are required");
    }

    #[test]
    fn lab_runner_workload_agent_task_contract_extracts_run_plan_lifecycle_fields() {
        let agent_task = lab_runner_workload_agent_task_from_command(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
                "--plan".to_string(),
                "@/runner/plan.json".to_string(),
                "--record-run-id=run-typed".to_string(),
            ],
            None,
            None,
        )
        .expect("agent task workload contract");

        assert_eq!(agent_task.run_id, "run-typed");
        assert_eq!(agent_task.plan_ref.as_deref(), Some("@/runner/plan.json"));
        assert_eq!(
            agent_task.dispatch_kind,
            LabRunnerWorkloadAgentTaskDispatchKind::RunPlan
        );
        assert_eq!(
            agent_task.lifecycle_mirror_policy,
            LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate
        );
        // A run-plan explicitly declares no dispatch handoff.
        assert_eq!(
            agent_task.handoff_mirror_policy,
            Some(LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::None)
        );
    }

    /// Cook and dispatch are the seam #7530 is about: they produce a handoff
    /// document, not a run-plan aggregate, so they must declare the handoff
    /// mirror policy that turns on the controller's typed mirror.
    #[test]
    fn lab_runner_workload_agent_task_contract_declares_dispatch_handoff_mirroring() {
        for action in ["cook", "dispatch"] {
            let agent_task = lab_runner_workload_agent_task_from_command(
                &[
                    "homeboy".to_string(),
                    "agent-task".to_string(),
                    action.to_string(),
                    "--run-id".to_string(),
                    "run-7530".to_string(),
                ],
                Some("run-7530"),
                None,
            )
            .unwrap_or_else(|| panic!("agent task workload contract for {action}"));

            assert_eq!(agent_task.run_id, "run-7530");
            assert_eq!(agent_task.plan_ref, None);
            assert_eq!(
                agent_task.lifecycle_mirror_policy,
                LabRunnerWorkloadAgentTaskLifecycleMirrorPolicy::None,
                "{action} mirrors a handoff, not a run-plan aggregate"
            );
            assert_eq!(
                agent_task.handoff_mirror_policy,
                Some(LabRunnerWorkloadAgentTaskHandoffMirrorPolicy::DispatchHandoff)
            );
        }
    }

    /// Without a run id there is no identity to key a typed mirror on, so no
    /// section is emitted at all and the consumer keeps its output fallback.
    #[test]
    fn lab_runner_workload_agent_task_contract_is_absent_without_a_run_id() {
        assert!(lab_runner_workload_agent_task_from_command(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "cook".to_string(),
            ],
            None,
            None,
        )
        .is_none());
    }

    /// A Cook declares its own handoff wait budget from the controller plan:
    /// the provider max timeout (3x a 30 min `--timeout-ms`) plus its gate
    /// budget, well past the 20 min transport default.
    #[test]
    fn cook_wait_budget_is_provider_max_timeout_plus_gate_budget() {
        use homeboy_agents::agent_task::{
            AgentTaskExecutor, AgentTaskLimits, AgentTaskPolicy, AgentTaskRequest,
            AgentTaskWorkspace, AGENT_TASK_REQUEST_SCHEMA,
        };
        let task = AgentTaskRequest {
            schema: AGENT_TASK_REQUEST_SCHEMA.to_string(),
            task_id: "cook-task".to_string(),
            group_key: None,
            parent_plan_id: None,
            executor: AgentTaskExecutor {
                backend: "fake".to_string(),
                selector: None,
                runtime_selection: None,
                required_capabilities: Vec::new(),
                secret_env: Vec::new(),
                model: None,
                config: serde_json::Value::Null,
            },
            instructions: "cook".to_string(),
            inputs: serde_json::Value::Null,
            source_refs: Vec::new(),
            workspace: AgentTaskWorkspace::default(),
            component_contracts: Vec::new(),
            policy: AgentTaskPolicy::default(),
            limits: AgentTaskLimits {
                timeout_ms: Some(30 * 60 * 1_000),
                ..Default::default()
            },
            expected_artifacts: Vec::new(),
            artifact_declarations: Vec::new(),
            output_declarations: Vec::new(),
            runtime_tools: Vec::new(),
            metadata: serde_json::Value::Null,
        };
        let plan = AgentTaskPlan::new("cook-plan", vec![task]);
        let args = [
            "homeboy",
            "agent-task",
            "cook",
            "--gate-timeout-seconds",
            "600",
        ]
        .map(str::to_string);

        let agent_task =
            lab_runner_workload_agent_task_from_command(&args, Some("cook-run"), Some(&plan))
                .expect("cook workload contract");

        assert_eq!(
            agent_task.wait_budget_ms,
            Some(3 * 30 * 60 * 1_000 + 600 * 1_000)
        );
    }

    /// A fanout cook child reaches Lab without a controller-materialized plan
    /// (`--placement lab` selects the runner inside the offload, so the
    /// controller never compiles one). Its wait budget must still be the
    /// provider's extension cap plus the gate budget — not `None`, which let
    /// the handoff wait fall back to the 20-minute transport default and
    /// cancel the runner job while the provider's extended deadline was still
    /// live (#16168).
    #[test]
    fn plan_less_cook_wait_budget_matches_the_provider_extension_cap() {
        use homeboy_agents::agent_task_timeout::{
            DEFAULT_PROVIDER_TIMEOUT_EXTENSION_FACTOR, DEFAULT_PROVIDER_TIMEOUT_MS,
        };
        let args = [
            "homeboy",
            "agent-task",
            "cook",
            "--gate-timeout-seconds",
            "600",
        ]
        .map(str::to_string);

        assert_eq!(
            lab_runner_workload_agent_task_from_command(&args, Some("cook-run"), None)
                .expect("cook workload contract")
                .wait_budget_ms,
            Some(
                DEFAULT_PROVIDER_TIMEOUT_MS * DEFAULT_PROVIDER_TIMEOUT_EXTENSION_FACTOR
                    + 600 * 1_000
            ),
            "a plan-less cook keeps the default provider extension cap plus its gate budget"
        );

        let dispatch_args = ["homeboy", "agent-task", "dispatch"].map(str::to_string);
        assert_eq!(
            lab_runner_workload_agent_task_from_command(&dispatch_args, Some("dispatch-run"), None)
                .expect("dispatch workload contract")
                .wait_budget_ms,
            Some(DEFAULT_PROVIDER_TIMEOUT_MS * DEFAULT_PROVIDER_TIMEOUT_EXTENSION_FACTOR),
            "a plan-less dispatch keeps the default provider extension cap"
        );
    }

    /// A plan-less cook that states its own `--timeout-ms` caps where the
    /// remote provider's extension cap does: 3x that base.
    #[test]
    fn plan_less_cook_wait_budget_follows_the_commands_timeout_ms() {
        let args = [
            "homeboy",
            "agent-task",
            "cook",
            "--timeout-ms",
            "1800000",
            "--gate-timeout-seconds",
            "600",
        ]
        .map(str::to_string);

        assert_eq!(
            lab_runner_workload_agent_task_from_command(&args, Some("cook-run"), None)
                .expect("cook workload contract")
                .wait_budget_ms,
            Some(3 * 1_800_000 + 600 * 1_000)
        );
    }

    #[test]
    fn legacy_agent_task_workload_decodes_without_provider_policy() {
        let agent_task: LabRunnerWorkloadAgentTask = serde_json::from_value(serde_json::json!({
            "run_id": "legacy-run",
            "plan_ref": null,
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none"
        }))
        .expect("legacy workload agent-task contract");

        assert!(agent_task.resolved_provider_policy.is_none());
    }
}
