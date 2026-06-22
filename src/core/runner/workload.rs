use crate::command_contract::{
    RunnerWorkload, RunnerWorkloadAssignment, RunnerWorkloadCapability,
    RunnerWorkloadCommandFamily, RunnerWorkloadKind, RunnerWorkloadMutationPolicy,
    RunnerWorkloadResultRefs, RunnerWorkloadSecrets, RunnerWorkloadState,
    RunnerWorkloadWorkspaceMappings, RUNNER_WORKLOAD_SCHEMA,
};
use crate::core::error::{Error, Result};
use crate::core::plan::HomeboyPlan;

use super::{
    LabOffloadCommand, LabOffloadSourcePathMode, LabOffloadWorkspaceModePolicy,
    RunnerCapabilityPreflight, RunnerRequiredTool,
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

pub(crate) fn build_runner_workload(input: RunnerWorkloadBuildInput<'_>) -> RunnerWorkload {
    let workspace_mapping_ref = input.workspace_mapping_ref.map(str::to_string);
    RunnerWorkload {
        schema: RUNNER_WORKLOAD_SCHEMA.to_string(),
        workload_id: format!("{}.runner_workload", input.plan.id),
        kind: RunnerWorkloadKind {
            command_label: input.command.hot_label.to_string(),
            command_family: RunnerWorkloadCommandFamily::from_command_label(
                input.command.hot_label,
            ),
        },
        workspace_mappings: RunnerWorkloadWorkspaceMappings {
            source_path_mode: source_path_mode_label(input.command.source_path_mode).to_string(),
            workspace_mode_policy: workspace_mode_policy_label(input.command.workspace_mode_policy)
                .to_string(),
            mapping_ref: workspace_mapping_ref.clone(),
        },
        required_capabilities: required_capabilities(input.command),
        required_secrets: RunnerWorkloadSecrets {
            categories: required_secret_categories(input.command.hot_label),
        },
        required_extensions: input.command.required_extensions.clone(),
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

pub(crate) fn validate_runner_workload_dispatch(
    workload: Option<&RunnerWorkload>,
    runner_id: &str,
    cwd: Option<&str>,
    command: &[String],
    _secret_env_names: &[String],
    capture_patch: bool,
) -> Result<()> {
    let Some(workload) = workload else {
        return Ok(());
    };
    if workload.schema != RUNNER_WORKLOAD_SCHEMA {
        return Err(workload_error(
            "runner_workload.schema",
            format!("runner workload schema must be `{RUNNER_WORKLOAD_SCHEMA}`"),
        ));
    }
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
    if workload.mutation_policy.capture_patch != capture_patch {
        return Err(workload_error(
            "runner_workload.mutation_policy.capture_patch",
            "runner workload mutation policy does not match dispatch capture_patch".to_string(),
        ));
    }
    for category in &workload.required_secrets.categories {
        if !matches!(category.as_str(), "agent_task" | "trace" | "tunnel") {
            return Err(workload_error(
                "runner_workload.required_secrets",
                format!("runner workload declares unsupported secret category `{category}`"),
            ));
        }
    }
    for capability in &workload.required_capabilities {
        match capability.name.as_str() {
            "extension_parity" | "playwright" => {}
            name if !capability.required => {}
            name => {
                return Err(workload_error(
                    "runner_workload.required_capabilities",
                    format!("runner workload requires unsupported capability `{name}`"),
                ));
            }
        }
    }
    if command.is_empty() {
        return Err(workload_error(
            "runner_workload.command",
            "runner workload dispatch requires a command".to_string(),
        ));
    }
    Ok(())
}

pub(crate) fn merge_runner_workload_capability_preflight(
    mut preflight: Option<RunnerCapabilityPreflight>,
    workload: Option<&RunnerWorkload>,
) -> Result<Option<RunnerCapabilityPreflight>> {
    let Some(workload) = workload else {
        return Ok(preflight);
    };
    for capability in &workload.required_capabilities {
        if capability.name == "playwright" && capability.required {
            let preflight = preflight.get_or_insert_with(|| RunnerCapabilityPreflight {
                command: workload.kind.command_label.clone(),
                ..Default::default()
            });
            if !preflight
                .required_tools
                .contains(&RunnerRequiredTool::Playwright)
            {
                preflight
                    .required_tools
                    .push(RunnerRequiredTool::Playwright);
            }
        }
    }
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
    artifacts: &[crate::core::api_jobs::JobArtifactMetadata],
) -> RunnerWorkload {
    workload.result_refs.job_id = job_id.map(str::to_string);
    workload.result_refs.mirror_run_id = mirror_run_id.map(str::to_string);
    workload.result_refs.artifacts = artifacts
        .iter()
        .map(
            |artifact| crate::command_contract::RunnerWorkloadArtifactRef {
                id: artifact.id.clone(),
                name: artifact.name.clone(),
                path: artifact.path.clone(),
                url: artifact.url.clone(),
            },
        )
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
    if command.requires_playwright {
        capabilities.push(RunnerWorkloadCapability {
            name: "playwright".to_string(),
            required: true,
        });
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
    use crate::command_contract::LabRoutingPolicy;
    use crate::core::api_jobs::JobArtifactMetadata;
    use crate::core::plan::{HomeboyPlan, PlanKind};

    fn plan() -> HomeboyPlan {
        HomeboyPlan::builder_for_description(PlanKind::LabOffload, "test").build()
    }

    fn command() -> LabOffloadCommand {
        LabOffloadCommand {
            hot_label: "trace",
            portable: true,
            unsupported_reason: None,
            source_path_mode: LabOffloadSourcePathMode::CwdOrPathFlag,
            workspace_mode_policy: LabOffloadWorkspaceModePolicy::ChangedSinceGitElseSnapshot,
            required_extensions: vec!["browser".to_string()],
            requires_playwright: true,
            routing_policy: LabRoutingPolicy {
                requires_extension_parity: true,
                ..LabRoutingPolicy::default()
            },
        }
    }

    #[test]
    fn runner_core_builds_complete_workload_payload() {
        let plan = plan();
        let command = command();
        let workload = build_runner_workload(RunnerWorkloadBuildInput {
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
            &["homeboy".to_string(), "trace".to_string()],
            &[],
            true,
        )
        .expect("matching dispatch is valid");

        let err = validate_runner_workload_dispatch(
            Some(&workload),
            "other-runner",
            Some("/srv/homeboy/work"),
            &["homeboy".to_string(), "trace".to_string()],
            &[],
            true,
        )
        .expect_err("drifted runner id must fail");
        assert_eq!(err.details["field"], "runner_workload.assignment.runner_id");
    }

    #[test]
    fn runner_workload_capabilities_merge_into_preflight() {
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

        let preflight = merge_runner_workload_capability_preflight(None, Some(&workload))
            .expect("merge workload capabilities")
            .expect("playwright preflight");
        assert!(preflight
            .required_tools
            .contains(&RunnerRequiredTool::Playwright));
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
}
