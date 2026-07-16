use crate::agent_task_dispatch_service::ResolvedAgentTaskProviderPolicy;
use crate::error::{Error, Result};
use crate::lab_contract::{
    RunnerWorkload, RunnerWorkloadAgentTask, RunnerWorkloadAgentTaskDispatchKind,
    RunnerWorkloadAgentTaskLifecycleMirrorPolicy, RunnerWorkloadAssignment,
    RunnerWorkloadCapability, RunnerWorkloadCommandFamily, RunnerWorkloadExtensionRevision,
    RunnerWorkloadKind, RunnerWorkloadMutationPolicy, RunnerWorkloadResultRefs,
    RunnerWorkloadSecrets, RunnerWorkloadState, RunnerWorkloadWorkspaceMappings,
    RUNNER_WORKLOAD_SCHEMA,
};
use crate::plan::HomeboyPlan;
use crate::secret_env_plan::SecretEnvPlan;
use clap::Parser;

use super::{
    LabOffloadCommand, LabOffloadSourcePathMode, LabOffloadWorkspaceModePolicy,
    RunnerCapabilityPreflight,
};

pub(crate) struct RunnerWorkloadBuildInput<'a> {
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

#[cfg(test)]
pub(crate) fn build_runner_workload(input: RunnerWorkloadBuildInput<'_>) -> RunnerWorkload {
    let command_label = input.command.hot_label.to_string();
    build_runner_workload_with_command_label(input, command_label)
}

pub(crate) fn build_runner_workload_for_dispatched_command(
    input: RunnerWorkloadBuildInput<'_>,
    dispatched_command: &[String],
) -> RunnerWorkload {
    let identity = dispatched_command_identity(dispatched_command)
        .unwrap_or_else(|| RunnerWorkloadCommandIdentity::from_label(input.command.hot_label));
    build_runner_workload_with_command_identity(input, identity)
}

fn build_runner_workload_with_command_label(
    input: RunnerWorkloadBuildInput<'_>,
    command_label: String,
) -> RunnerWorkload {
    build_runner_workload_with_command_identity(
        input,
        RunnerWorkloadCommandIdentity::from_label(&command_label),
    )
}

fn build_runner_workload_with_command_identity(
    input: RunnerWorkloadBuildInput<'_>,
    identity: RunnerWorkloadCommandIdentity,
) -> RunnerWorkload {
    let workspace_mapping_ref = input.workspace_mapping_ref.map(str::to_string);
    let required_secret_categories = required_secret_categories(&identity.label);
    RunnerWorkload {
        schema: RUNNER_WORKLOAD_SCHEMA.to_string(),
        workload_id: format!("{}.runner_workload", input.plan.id),
        kind: RunnerWorkloadKind {
            command_label: identity.label,
            command_family: identity.family,
        },
        agent_task: None,
        notification_route: crate::notification_route::current(),
        workspace_mappings: RunnerWorkloadWorkspaceMappings {
            source_path_mode: source_path_mode_label(input.command.source_path_mode).to_string(),
            workspace_mode_policy: workspace_mode_policy_label(input.command.workspace_mode_policy)
                .to_string(),
            mapping_ref: workspace_mapping_ref.clone(),
        },
        required_capabilities: required_capabilities(input.command),
        required_secrets: RunnerWorkloadSecrets {
            categories: required_secret_categories,
            secret_env_plan: SecretEnvPlan::default(),
        },
        required_extensions: input.command.required_extensions.clone(),
        required_extension_revisions: required_extension_revisions(
            &input.command.required_extensions,
        ),
        mutation_policy: RunnerWorkloadMutationPolicy {
            capture_patch: input.capture_patch,
            mutation_flag: input.mutation_flag.map(str::to_string),
            allow_dirty_lab_workspace: input.allow_dirty_lab_workspace,
        },
        assignment: RunnerWorkloadAssignment {
            runner_id: Some(input.runner_id.to_string()),
            runner_mode: Some(input.runner_mode.to_string()),
            source: Some(input.assignment_source.to_string()),
        },
        state: RunnerWorkloadState {
            status: input.status.to_string(),
            remote_workspace: input.remote_workspace.map(str::to_string),
            fallback_reason: input.fallback_reason.map(str::to_string),
        },
        result_refs: RunnerWorkloadResultRefs {
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
struct RunnerWorkloadCommandIdentity {
    label: String,
    family: RunnerWorkloadCommandFamily,
}

impl RunnerWorkloadCommandIdentity {
    fn from_label(label: &str) -> Self {
        Self {
            label: label.to_string(),
            family: RunnerWorkloadCommandFamily::from_command_label(label),
        }
    }
}

/// Parse the dispatched argv through the CLI contract so target positionals do
/// not become part of a command label.
///
/// The actual CLI parse lives in the CLI layer and is invoked through the
/// `command_label` resolver hook, so core does not depend on the full CLI
/// parser (`cli_surface::Cli`).
fn dispatched_command_identity(command: &[String]) -> Option<RunnerWorkloadCommandIdentity> {
    let argv = cli_argv_from_dispatched_command(command)?;
    let label = super::resolve_command_label(&argv)?;
    Some(RunnerWorkloadCommandIdentity::from_label(&label))
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

pub(crate) fn runner_workload_agent_task_from_command(
    args: &[String],
    run_id: Option<&str>,
) -> Option<RunnerWorkloadAgentTask> {
    let agent_task_index = args.iter().position(|arg| arg == "agent-task")?;
    let action = args.get(agent_task_index + 1)?.as_str();
    match action {
        "cook" => run_id.map(|run_id| RunnerWorkloadAgentTask {
            run_id: run_id.to_string(),
            plan_ref: None,
            resolved_provider_policy: resolved_provider_policy_from_command(args),
            dispatch_kind: RunnerWorkloadAgentTaskDispatchKind::Cook,
            lifecycle_mirror_policy: RunnerWorkloadAgentTaskLifecycleMirrorPolicy::None,
        }),
        "dispatch" => run_id.map(|run_id| RunnerWorkloadAgentTask {
            run_id: run_id.to_string(),
            plan_ref: None,
            resolved_provider_policy: resolved_provider_policy_from_command(args),
            dispatch_kind: RunnerWorkloadAgentTaskDispatchKind::Dispatch,
            lifecycle_mirror_policy: RunnerWorkloadAgentTaskLifecycleMirrorPolicy::None,
        }),
        "run-plan" => {
            let plan_ref = option_value_after(args, agent_task_index + 1, "--plan")?;
            let record_run_id = run_id
                .map(str::to_string)
                .or_else(|| option_value_after(args, agent_task_index + 1, "--record-run-id"))?;
            Some(RunnerWorkloadAgentTask {
                run_id: record_run_id,
                plan_ref: Some(plan_ref),
                resolved_provider_policy: None,
                dispatch_kind: RunnerWorkloadAgentTaskDispatchKind::RunPlan,
                lifecycle_mirror_policy:
                    RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate,
            })
        }
        _ => None,
    }
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
) -> Vec<RunnerWorkloadExtensionRevision> {
    required_extension_revisions_with(required_extensions, crate::extension::read_source_revision)
}

fn required_extension_revisions_with(
    required_extensions: &[String],
    mut read_revision: impl FnMut(&str) -> Option<String>,
) -> Vec<RunnerWorkloadExtensionRevision> {
    required_extensions
        .iter()
        .filter_map(|extension_id| {
            read_revision(extension_id).map(|source_revision| RunnerWorkloadExtensionRevision {
                extension_id: extension_id.clone(),
                source_revision,
            })
        })
        .collect()
}

pub(crate) fn validate_runner_workload_dispatch(
    workload: Option<&RunnerWorkload>,
    runner_id: &str,
    cwd: Option<&str>,
    command: &[String],
    secret_env_plan: &SecretEnvPlan,
    capture_patch: bool,
) -> Result<()> {
    let Some(workload) = workload else {
        return Ok(());
    };
    validate_runner_workload_dispatch_identity(workload, runner_id, cwd)?;
    validate_runner_workload_mutation_policy(workload, capture_patch)?;
    validate_runner_workload_required_secrets(workload, secret_env_plan)?;
    validate_runner_workload_required_capabilities(workload)?;
    validate_runner_workload_command_present(command)?;
    validate_runner_workload_command(workload, command)?;
    Ok(())
}

fn validate_runner_workload_dispatch_identity(
    workload: &RunnerWorkload,
    runner_id: &str,
    cwd: Option<&str>,
) -> Result<()> {
    if workload.schema != RUNNER_WORKLOAD_SCHEMA {
        return Err(workload_error(
            "runner_workload.schema",
            format!("runner workload schema must be `{RUNNER_WORKLOAD_SCHEMA}`"),
        ));
    }
    validate_runner_workload_result_refs(workload)?;
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

fn validate_runner_workload_mutation_policy(
    workload: &RunnerWorkload,
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

fn validate_runner_workload_required_secrets(
    workload: &RunnerWorkload,
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

fn validate_runner_workload_required_capabilities(workload: &RunnerWorkload) -> Result<()> {
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

fn validate_runner_workload_command_present(command: &[String]) -> Result<()> {
    if command.is_empty() {
        return Err(workload_error(
            "runner_workload.command",
            "runner workload dispatch requires a command".to_string(),
        ));
    }
    Ok(())
}

fn validate_runner_workload_command(workload: &RunnerWorkload, command: &[String]) -> Result<()> {
    if let Some(dispatched_identity) = dispatched_command_identity(command) {
        return validate_runner_workload_command_identity(workload, dispatched_identity);
    }

    // Some direct runner validations use incomplete argv that cannot be parsed
    // as a full CLI invocation. Preserve their strict label validation rather
    // than accepting an unrecognized command.
    validate_runner_workload_command_fallback(workload, command)
}

fn validate_runner_workload_command_identity(
    workload: &RunnerWorkload,
    dispatched_identity: RunnerWorkloadCommandIdentity,
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

    if dispatched_identity.family != RunnerWorkloadCommandFamily::Unknown
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

fn validate_runner_workload_command_fallback(
    workload: &RunnerWorkload,
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
        // both Quality) canonicalize to one runner-workload identity, so a
        // labelled dispatch that resolves to the workload's family is a match
        // even when the surface label differs (#7972). When the dispatched
        // family is Unknown we cannot canonicalize, so fall back to strict
        // label matching.
        let canonical_same_family = dispatched_family != RunnerWorkloadCommandFamily::Unknown
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

    if dispatched_family != RunnerWorkloadCommandFamily::Unknown
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

fn validate_runner_workload_result_refs(workload: &RunnerWorkload) -> Result<()> {
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

fn runner_workload_command_label_value_option(arg: &str) -> bool {
    matches!(arg, "--extension" | "--path")
}

fn runner_workload_command_label_flag_option(arg: &str) -> bool {
    arg.starts_with("--extension=") || arg.starts_with("--path=")
}

fn dispatched_command_family(command_args: &[String]) -> RunnerWorkloadCommandFamily {
    let positional_parts = dispatched_positional_command_parts(command_args);
    let max_parts = positional_parts.len().min(3);
    for part_count in (1..=max_parts).rev() {
        let label = positional_parts[..part_count].join(" ");
        let family = RunnerWorkloadCommandFamily::from_command_label(&label);
        if family != RunnerWorkloadCommandFamily::Unknown {
            return family;
        }
    }
    RunnerWorkloadCommandFamily::Unknown
}

fn dispatched_positional_command_parts(command_args: &[String]) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut iter = command_args.iter().peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if runner_workload_command_label_value_option(arg) {
            let _ = iter.next();
            continue;
        }
        if runner_workload_command_label_flag_option(arg) {
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

pub(crate) fn merge_runner_workload_capability_preflight(
    preflight: Option<RunnerCapabilityPreflight>,
    workload: Option<&RunnerWorkload>,
) -> Result<Option<RunnerCapabilityPreflight>> {
    let Some(workload) = workload else {
        return Ok(preflight);
    };
    let _ = workload;
    Ok(preflight)
}

pub(crate) fn merge_runner_workload_required_extensions(
    mut required_extensions: Vec<String>,
    workload: Option<&RunnerWorkload>,
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

pub(crate) fn runner_workload_with_result_refs(
    mut workload: RunnerWorkload,
    job_id: Option<&str>,
    mirror_run_id: Option<&str>,
    artifacts: &[crate::api_jobs::JobArtifactMetadata],
) -> RunnerWorkload {
    workload.result_refs.job_id = job_id.map(str::to_string);
    workload.result_refs.mirror_run_id = mirror_run_id.map(str::to_string);
    workload.result_refs.artifacts = artifacts
        .iter()
        .map(|artifact| crate::lab_contract::RunnerWorkloadArtifactRef {
            id: artifact.id.clone(),
            name: artifact.name.clone(),
            path: artifact.path.clone(),
            url: artifact.url.clone(),
        })
        .collect();
    workload
}

fn required_capabilities(command: &LabOffloadCommand) -> Vec<RunnerWorkloadCapability> {
    let mut capabilities = Vec::new();
    if command.routing_policy.requires_extension_parity || !command.required_extensions.is_empty() {
        capabilities.push(RunnerWorkloadCapability {
            name: "extension_parity".to_string(),
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
    use crate::api_jobs::JobArtifactMetadata;
    use crate::plan::{HomeboyPlan, PlanKind};

    /// Register a stub command-label resolver for tests.
    ///
    /// Production wires this hook to the real CLI parser (`cli_surface::Cli`) at
    /// startup; the parser's correctness is covered by the CLI layer's own
    /// tests. Here we only exercise core's workload-building behaviour given a
    /// resolved label, so the stub maps each test argv to its expected label via
    /// the shared `command_label_for_test_argv` table (keeping core free of any
    /// dependency on the CLI parser).
    fn register_test_command_label_resolver() {
        crate::runner::set_command_label_resolver(command_label_for_test_argv);
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
                "agent-task run/run-next/status/logs/artifacts/review/list/active/latest"
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
            command: crate::lab_contract::LabCommandContract::portable("trace", None, true, &[]),
            required_extensions: vec!["browser".to_string()],
            required_capabilities: vec![RunnerWorkloadCapability {
                name: "playwright".to_string(),
                required: true,
            }],
            workload: None,
        }
    }

    fn workload() -> RunnerWorkload {
        let plan = plan();
        let command = command();
        build_runner_workload(RunnerWorkloadBuildInput {
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
    fn runner_workload_kind_comes_from_dispatched_command_for_lab_shapes() {
        struct Case {
            name: &'static str,
            argv: Vec<&'static str>,
            expected_label: &'static str,
            expected_family: RunnerWorkloadCommandFamily,
        }

        let cases = [
            Case {
                name: "review lint component",
                argv: vec!["/srv/homeboy/bin/homeboy", "review", "lint", "homeboy"],
                expected_label: "review lint",
                expected_family: RunnerWorkloadCommandFamily::Quality,
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
                expected_family: RunnerWorkloadCommandFamily::AgentTask,
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
                expected_family: RunnerWorkloadCommandFamily::Unknown,
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
                expected_family: RunnerWorkloadCommandFamily::Workspace,
            },
            Case {
                name: "resident-path command",
                argv: vec!["/srv/homeboy/bin/homeboy", "agent-task", "status", "run-1"],
                expected_label:
                    "agent-task run/run-next/status/logs/artifacts/review/list/active/latest",
                expected_family: RunnerWorkloadCommandFamily::AgentTask,
            },
        ];

        for case in cases {
            let plan = plan();
            let mut command = command();
            command.hot_label = "trace";
            let dispatched_command = case
                .argv
                .iter()
                .map(|arg| (*arg).to_string())
                .collect::<Vec<_>>();

            register_test_command_label_resolver();
            let workload = build_runner_workload_for_dispatched_command(
                RunnerWorkloadBuildInput {
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

    #[test]
    fn runner_core_builds_complete_workload_payload() {
        let plan = plan();
        let command = command();
        let mut workload = build_runner_workload(RunnerWorkloadBuildInput {
            plan: &plan,
            command: &command,
            capture_patch: true,
            mutation_flag: Some("--keep-overlay"),
            allow_dirty_lab_workspace: true,
            runner_id: "lab-a",
            runner_mode: "direct_ssh",
            assignment_source: "explicit",
            status: "offloaded",
            remote_workspace: Some("/srv/homeboy/work"),
            fallback_reason: None,
            workspace_mapping_ref: Some("workspace_mapping"),
            proof_id: Some("proof-1"),
        });
        workload.required_secrets.secret_env_plan = secret_plan(&["HOMEBODY_TRACE_SECRET"]);

        assert_eq!(workload.schema, RUNNER_WORKLOAD_SCHEMA);
        assert_eq!(workload.workload_id, "lab_offload.test.runner_workload");
        assert_eq!(workload.kind.command_label, "trace");
        assert_eq!(
            workload.kind.command_family,
            RunnerWorkloadCommandFamily::Quality
        );
        assert_eq!(
            workload.workspace_mappings.mapping_ref.as_deref(),
            Some("workspace_mapping")
        );
        assert_eq!(workload.required_capabilities.len(), 2);
        assert_eq!(workload.required_capabilities[0].name, "extension_parity");
        assert_eq!(workload.required_capabilities[1].name, "playwright");
        assert_eq!(workload.required_secrets.categories, vec!["trace"]);
        assert_eq!(workload.required_extensions, vec!["browser"]);
        assert!(workload.required_extension_revisions.is_empty());
        assert_eq!(
            workload.mutation_policy.mutation_flag.as_deref(),
            Some("--keep-overlay")
        );
        assert_eq!(workload.assignment.runner_id.as_deref(), Some("lab-a"));
        assert_eq!(
            workload.state.remote_workspace.as_deref(),
            Some("/srv/homeboy/work")
        );
        assert_eq!(workload.result_refs.plan_id, "lab_offload.test");
        assert_eq!(workload.result_refs.proof_id.as_deref(), Some("proof-1"));
    }

    #[test]
    fn runner_workload_extension_revisions_pin_installed_required_extensions() {
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
    fn runner_workload_validation_rejects_dispatch_drift() {
        let plan = plan();
        let command = command();
        let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
            workspace_mapping_ref: Some("workspace_mapping"),
            proof_id: None,
        });

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "test".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("matching dispatch is valid");

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "other-runner",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &SecretEnvPlan::default(),
            true,
        )
        .expect_err("drifted runner id must fail");
        assert_eq!(err.details["field"], "runner_workload.assignment.runner_id");
    }

    #[test]
    fn runner_workload_validation_rejects_remote_workspace_mismatch() {
        let workload = workload();

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/other-workspace"),
            &["homeboy".to_string(), "trace".to_string()],
            &SecretEnvPlan::default(),
            true,
        )
        .expect_err("mismatched remote workspace must fail");

        assert_eq!(
            err.details["field"],
            "runner_workload.state.remote_workspace"
        );
        assert!(err
            .to_string()
            .contains("remote workspace does not match dispatch cwd"));
    }

    #[test]
    fn runner_workload_validation_rejects_blank_result_refs_plan_id() {
        let mut workload = workload();
        workload.result_refs.plan_id = " ".to_string();

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect_err("blank result refs plan id must fail");
        assert_eq!(err.details["field"], "runner_workload.result_refs.plan_id");
    }

    #[test]
    fn runner_workload_validation_rejects_result_refs_plan_id_mismatched_to_workload_id() {
        let mut workload = workload();
        workload.result_refs.plan_id = "lab_offload.other".to_string();

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect_err("mismatched result refs plan id must fail");
        assert_eq!(err.details["field"], "runner_workload.result_refs.plan_id");
    }

    #[test]
    fn runner_workload_validation_rejects_blank_result_refs_artifact_ids() {
        let workload = runner_workload_with_result_refs(
            workload(),
            Some("job-1"),
            Some("mirror-1"),
            &[JobArtifactMetadata {
                id: " ".to_string(),
                name: Some("trace.zip".to_string()),
                path: Some("/tmp/trace.zip".to_string()),
                url: None,
                mime: None,
                size_bytes: Some(42),
                sha256: None,
                content_base64: None,
                metadata: None,
            }],
        );

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect_err("blank result refs artifact id must fail");
        assert_eq!(
            err.details["field"],
            "runner_workload.result_refs.artifacts"
        );
    }

    #[test]
    fn runner_workload_validation_rejects_command_label_drift() {
        let plan = plan();
        let command = command();
        let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });

        // A dispatch whose label belongs to no known workload family (here
        // `status`) cannot be canonicalized to this `trace` (Quality) workload,
        // so the strict label check still rejects genuine command drift. (A
        // same-family alias like `test` is intentionally accepted — see
        // `runner_workload_validation_rejects_dispatch_drift`.)
        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "status".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect_err("drifted command label must fail");
        assert_eq!(err.details["field"], "runner_workload.kind.command_label");
    }

    #[test]
    fn runner_workload_validation_uses_runtime_refresh_command_identity() {
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
        let workload = build_runner_workload_for_dispatched_command(
            RunnerWorkloadBuildInput {
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
        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &dispatched_command,
            &SecretEnvPlan::default(),
            true,
        )
        .expect("runtime refresh target positional must not alter the command identity");

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &SecretEnvPlan::default(),
            true,
        )
        .expect_err("a genuinely different dispatched command must fail");
        assert_eq!(err.details["field"], "runner_workload.kind.command_label");
        assert!(err.message.contains("runtime refresh"));
        assert!(err.message.contains("trace"));
    }

    #[test]
    fn runner_workload_validation_rejects_workspace_mapping_reference_drift() {
        let plan = plan();
        let command = command();
        let mut workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });
        workload.result_refs.workspace_mapping_ref = Some("workspace_mapping".to_string());

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &SecretEnvPlan::default(),
            true,
        )
        .expect_err("drifted workspace mapping reference must fail");
        assert_eq!(
            err.details["field"],
            "runner_workload.result_refs.workspace_mapping_ref"
        );
    }

    #[test]
    fn runner_workload_validation_accepts_matching_full_and_short_command_argv() {
        let plan = plan();
        let command = command();
        let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("matching full argv is valid");

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("matching short argv is valid");

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["/srv/homeboy/bin/homeboy".to_string(), "trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("matching configured homeboy executable argv is valid");

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["/srv/homeboy/bin/homeboy".to_string(), "trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("matching runner-injected homeboy argv is valid");
    }

    #[test]
    fn runner_workload_validation_accepts_nested_review_quality_labels() {
        for label in [
            "review audit",
            "review lint",
            "review test",
            "review build",
            "review ci",
        ] {
            let plan = plan();
            let mut command = command();
            command.hot_label = label;
            command.required_extensions.clear();
            command.required_capabilities.clear();
            let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
                RunnerWorkloadCommandFamily::Quality
            );
            validate_runner_workload_dispatch(
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
            validate_runner_workload_dispatch(
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
    fn runner_workload_validation_accepts_fanout_cook_batch_command_label() {
        let plan = plan();
        let mut command = command();
        command.hot_label = "agent-task fanout cook-batch";
        let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "fanout".to_string(),
                "cook-batch".to_string(),
                "--repo".to_string(),
                "homeboy".to_string(),
                "--run-plan".to_string(),
            ],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("matching fanout cook-batch argv is valid");
    }

    #[test]
    fn runner_workload_validation_rejects_required_secret_plan_without_named_handoff() {
        let plan = plan();
        let command = command();
        let mut workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });
        workload.required_secrets.secret_env_plan = secret_plan(&["HOMEBODY_TRACE_SECRET"]);

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &[
                "homeboy".to_string(),
                "trace".to_string(),
                "--secret-env=HOMEBODY_TRACE_SECRET".to_string(),
            ],
            &SecretEnvPlan::default(),
            true,
        )
        .expect_err("required secret plan without named handoff must fail");
        assert_eq!(err.details["field"], "runner_workload.required_secrets");
        assert!(err.message.contains("HOMEBODY_TRACE_SECRET"));
    }

    #[test]
    fn runner_workload_validation_rejects_wrong_trace_secret_handoff_name() {
        let mut workload = workload();
        workload.required_secrets.secret_env_plan = secret_plan(&["HOMEBODY_TRACE_SECRET"]);

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &[
                "homeboy".to_string(),
                "trace".to_string(),
                "--secret-env=HOMEBODY_TRACE_SECRET".to_string(),
            ],
            &secret_plan(&["WRONG_SECRET"]),
            true,
        )
        .expect_err("wrong trace secret handoff name must fail");
        assert_eq!(err.details["field"], "runner_workload.required_secrets");
        assert!(err.message.contains("HOMEBODY_TRACE_SECRET"));
    }

    #[test]
    fn runner_workload_validation_rejects_wrong_agent_task_secret_handoff_name() {
        let plan = plan();
        let mut command = command();
        command.hot_label = "agent-task dispatch";
        let mut workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });
        workload.required_secrets.secret_env_plan = secret_plan(&["AGENT_TASK_SECRET"]);

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "dispatch".to_string(),
                "--secret-env=AGENT_TASK_SECRET".to_string(),
            ],
            &secret_plan(&["WRONG_SECRET"]),
            true,
        )
        .expect_err("wrong agent-task secret handoff name must fail");
        assert_eq!(err.details["field"], "runner_workload.required_secrets");
        assert!(err.message.contains("AGENT_TASK_SECRET"));
    }

    #[test]
    fn runner_workload_validation_rejects_missing_tunnel_implicit_secret_handoff_name() {
        let plan = plan();
        let mut command = command();
        command.hot_label = "tunnel preview-client start";
        let mut workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });
        workload.required_secrets.secret_env_plan = secret_plan(&["HOMEBOY_PREVIEW_TUNNEL_TOKEN"]);

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &[
                "homeboy".to_string(),
                "tunnel".to_string(),
                "preview-client".to_string(),
                "start".to_string(),
                "--ingress".to_string(),
                "https://preview-broker.example.test".to_string(),
                "--public-host".to_string(),
                "preview.example.test".to_string(),
                "--local-origin".to_string(),
                "http://127.0.0.1:8888".to_string(),
            ],
            &SecretEnvPlan::default(),
            true,
        )
        .expect_err("missing implicit tunnel secret handoff name must fail");
        assert_eq!(err.details["field"], "runner_workload.required_secrets");
        assert!(err.message.contains("HOMEBOY_PREVIEW_TUNNEL_TOKEN"));
    }

    #[test]
    fn runner_workload_validation_accepts_required_secret_categories_with_named_handoff() {
        let plan = plan();
        let command = command();
        let mut workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });
        workload.required_secrets.secret_env_plan = secret_plan(&["HOMEBODY_TRACE_SECRET"]);

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &secret_plan(&["HOMEBODY_TRACE_SECRET"]),
            true,
        )
        .expect("required secret category with named handoff is valid");
    }

    #[test]
    fn runner_workload_validation_accepts_empty_secret_handoff_without_required_categories() {
        let plan = plan();
        let mut command = command();
        command.hot_label = "lint";
        let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        });

        validate_runner_workload_dispatch(
            Some(&workload),
            "lab-a",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "lint".to_string()],
            &SecretEnvPlan::default(),
            true,
        )
        .expect("empty secret handoff is valid when no categories are required");
    }

    #[test]
    fn runner_workload_agent_task_contract_extracts_run_plan_lifecycle_fields() {
        let agent_task = runner_workload_agent_task_from_command(
            &[
                "homeboy".to_string(),
                "agent-task".to_string(),
                "run-plan".to_string(),
                "--plan".to_string(),
                "@/runner/plan.json".to_string(),
                "--record-run-id=run-typed".to_string(),
            ],
            None,
        )
        .expect("agent task workload contract");

        assert_eq!(agent_task.run_id, "run-typed");
        assert_eq!(agent_task.plan_ref.as_deref(), Some("@/runner/plan.json"));
        assert_eq!(
            agent_task.dispatch_kind,
            RunnerWorkloadAgentTaskDispatchKind::RunPlan
        );
        assert_eq!(
            agent_task.lifecycle_mirror_policy,
            RunnerWorkloadAgentTaskLifecycleMirrorPolicy::RunPlanAggregate
        );
    }

    #[test]
    fn runner_workload_result_refs_use_actual_artifacts() {
        let plan = plan();
        let command = command();
        let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
        let workload = runner_workload_with_result_refs(
            workload,
            Some("job-1"),
            Some("mirror-1"),
            &[JobArtifactMetadata {
                id: "artifact-1".to_string(),
                name: Some("trace.zip".to_string()),
                path: Some("/tmp/trace.zip".to_string()),
                url: None,
                mime: None,
                size_bytes: Some(42),
                sha256: None,
                content_base64: None,
                metadata: None,
            }],
        );

        assert_eq!(workload.result_refs.job_id.as_deref(), Some("job-1"));
        assert_eq!(
            workload.result_refs.mirror_run_id.as_deref(),
            Some("mirror-1")
        );
        assert_eq!(workload.result_refs.artifacts[0].id, "artifact-1");
        assert_eq!(
            workload.result_refs.artifacts[0].name.as_deref(),
            Some("trace.zip")
        );
    }

    #[test]
    fn legacy_agent_task_workload_decodes_without_provider_policy() {
        let agent_task: RunnerWorkloadAgentTask = serde_json::from_value(serde_json::json!({
            "run_id": "legacy-run",
            "plan_ref": null,
            "dispatch_kind": "cook",
            "lifecycle_mirror_policy": "none"
        }))
        .expect("legacy workload agent-task contract");

        assert!(agent_task.resolved_provider_policy.is_none());
    }
}
