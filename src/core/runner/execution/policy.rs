use serde_json::json;

use crate::core::error::{Error, ErrorCode, Result};

use crate::core::runner::{
    Runner, RunnerCapabilityPreflight, RunnerKind, RunnerPathMaterialization,
    RunnerRemoteDispatchWrapperFlag,
};
use crate::core::runner_execution_envelope::PathMaterializationPlan;
use crate::core::source_snapshot::SourceSnapshot;

use super::{trim_trailing_slashes, RunnerExecOptions};

/// Derive the remote capability-parity preflight for a runner dispatch.
///
/// Remote execution dispatch must validate that the runner can satisfy the
/// command's top-level executable before starting execution, so a missing tool
/// fails before remote dispatch instead of mid-run. When the caller has already
/// supplied an explicit capability preflight (e.g. rig install requiring the
/// `homeboy` tool), that contract is preserved unchanged; otherwise a contract
/// is derived from the command's first argv element. `exec` no-ops this gate for
/// local runners and SSH runners that already advertise the executable, so it is
/// behavior-preserving on a provisioned runner and fails loudly otherwise
/// (#5093, #5422).
pub(super) fn remote_execution_preflight(
    command: &[String],
    explicit: Option<&RunnerCapabilityPreflight>,
) -> Option<RunnerCapabilityPreflight> {
    if let Some(explicit) = explicit {
        return Some(explicit.clone());
    }

    let required_commands: Vec<String> = command
        .first()
        .filter(|program| !program.trim().is_empty())
        .cloned()
        .into_iter()
        .collect();
    if required_commands.is_empty() {
        return None;
    }

    Some(RunnerCapabilityPreflight {
        command: "runner.exec".to_string(),
        required_commands,
        ..Default::default()
    })
}

/// Controller-side contract prepared before a caller-derived command is sent to
/// a remote runner. The executor validates `capability_preflight` against the
/// selected runner before it starts the command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunnerRemoteDispatchPreflight {
    pub command: Vec<String>,
    pub capability_preflight: Option<RunnerCapabilityPreflight>,
}

/// Inputs for the typed remote-dispatch preflight.
pub struct RunnerRemoteDispatchPreflightRequest<'a> {
    pub context: &'a str,
    pub runner_id: &'a str,
    pub command: &'a [String],
    pub remote_cwd: &'a str,
    pub materialized_paths: &'a [RunnerPathMaterialization],
    pub local_only_wrapper_flags: &'a [RunnerRemoteDispatchWrapperFlag],
}

/// Strip controller-only wrappers, translate declared materialized paths, and
/// reject any remaining controller-local filesystem path before remote dispatch.
pub fn preflight_remote_dispatch(
    request: RunnerRemoteDispatchPreflightRequest<'_>,
) -> Result<RunnerRemoteDispatchPreflight> {
    let command = crate::core::runner::rewrite_remote_dispatch_argv(
        request.command,
        request.local_only_wrapper_flags,
        request.materialized_paths,
    );
    crate::core::runner::preflight_remote_dispatch_paths(
        request.context,
        request.runner_id,
        &command,
        request.remote_cwd,
        request.materialized_paths,
    )?;

    Ok(RunnerRemoteDispatchPreflight {
        capability_preflight: remote_execution_preflight(&command, None),
        command,
    })
}

/// Apply the final generic argv path-translation gate before remote dispatch.
/// Higher-level routes rewrite known path-bearing arguments; this shared guard
/// rejects any controller path that survives those rewrites.
pub(super) fn preflight_remote_argv(
    runner_id: &str,
    command: &[String],
    remote_cwd: &str,
    source_snapshot: Option<&SourceSnapshot>,
    path_materialization_plan: Option<&PathMaterializationPlan>,
) -> Result<()> {
    let mut mappings = path_materialization_plan
        .into_iter()
        .flat_map(|plan| plan.entries.iter())
        .filter_map(|entry| {
            entry
                .local_path
                .as_deref()
                .map(|local| (local, entry.remote_path.as_str()))
        })
        .collect::<Vec<_>>();
    if let Some(snapshot) = source_snapshot {
        if let (Some(local), Some(remote)) = (
            snapshot.local_path.as_deref(),
            snapshot.remote_path.as_deref(),
        ) {
            mappings.push((local, remote));
        }
    }
    for (local, remote) in mappings {
        crate::core::runner::preflight_remote_argv_path_translation(
            "runner exec",
            runner_id,
            command,
            std::path::Path::new(local),
            if remote.trim().is_empty() {
                remote_cwd
            } else {
                remote
            },
        )?;
    }
    Ok(())
}

pub(super) struct RunnerPolicyRequest<'a> {
    pub(super) project_id: Option<&'a str>,
    pub(super) command: &'a [String],
    pub(super) capture_patch: bool,
    pub(super) raw_exec: bool,
}

impl<'a> From<&'a RunnerExecOptions> for RunnerPolicyRequest<'a> {
    fn from(options: &'a RunnerExecOptions) -> Self {
        Self {
            project_id: options.project_id.as_deref(),
            command: &options.command,
            capture_patch: options.capture_patch,
            raw_exec: options.raw_exec,
        }
    }
}

pub(super) fn validate_runner_policy(
    runner: &Runner,
    cwd: &str,
    request: RunnerPolicyRequest<'_>,
) -> Result<()> {
    if runner.kind != RunnerKind::Ssh {
        return Ok(());
    }

    enforce_raw_exec(runner, request.raw_exec)?;
    enforce_project(runner, request.project_id)?;
    enforce_command(runner, request.command)?;
    enforce_workspace(runner, cwd)?;
    enforce_artifacts(runner, request.capture_patch)
}

fn enforce_raw_exec(runner: &Runner, raw_exec: bool) -> Result<()> {
    if !raw_exec || runner.policy.allow_raw_exec == Some(true) {
        return Ok(());
    }

    Err(policy_denied(
        runner,
        "raw_exec",
        "remote runner raw exec is denied by default; explicitly trust this runner with allow_raw_exec=true before executing arbitrary commands",
        vec![format!(
            "Run `homeboy runner trust {} --allow-raw-exec true` for diagnostic shell access.",
            runner.id
        )],
    ))
}

fn enforce_project(runner: &Runner, project_id: Option<&str>) -> Result<()> {
    let Some(project_id) = project_id else {
        return Ok(());
    };
    if runner.policy.allowed_projects.is_empty()
        || runner
            .policy
            .allowed_projects
            .iter()
            .any(|project| project == project_id)
    {
        return Ok(());
    }

    Err(policy_denied(
        runner,
        "project",
        format!(
            "project '{project_id}' is not allowed to use runner '{}'",
            runner.id
        ),
        vec![format!(
            "Add it with `homeboy runner trust {} --project {project_id}`.",
            runner.id
        )],
    ))
}

fn enforce_command(runner: &Runner, command: &[String]) -> Result<()> {
    if runner.policy.allowed_commands.is_empty() {
        return Ok(());
    }

    let family = command_family(command);
    if runner
        .policy
        .allowed_commands
        .iter()
        .any(|allowed| allowed == family || allowed == "runner.exec")
    {
        return Ok(());
    }

    Err(policy_denied(
        runner,
        "command",
        format!("command family '{family}' is not allowed on runner '{}'", runner.id),
        vec![format!(
            "Allow it with `homeboy runner trust {} --command {family}` or use a higher-level allowed Homeboy command.",
            runner.id
        )],
    ))
}

fn enforce_workspace(runner: &Runner, cwd: &str) -> Result<()> {
    if runner.policy.workspace_roots.is_empty()
        || runner
            .policy
            .workspace_roots
            .iter()
            .any(|root| path_is_inside(root, cwd))
    {
        return Ok(());
    }

    Err(policy_denied(
        runner,
        "workspace_root",
        format!("cwd '{cwd}' is outside the runner trust policy workspace roots"),
        runner
            .policy
            .workspace_roots
            .iter()
            .map(|root| format!("Use a path under {root}."))
            .collect(),
    ))
}

fn enforce_artifacts(runner: &Runner, capture_patch: bool) -> Result<()> {
    if !capture_patch
        || !runner
            .policy
            .artifact_policy
            .as_deref()
            .is_some_and(|policy| matches!(policy, "deny" | "none"))
    {
        return Ok(());
    }

    Err(policy_denied(
        runner,
        "artifacts",
        "patch artifact capture is denied by this runner trust policy",
        vec![format!(
            "Change artifact policy with `homeboy runner trust {} --artifact-policy copy` if artifacts should be captured.",
            runner.id
        )],
    ))
}

fn policy_denied(
    runner: &Runner,
    field: &str,
    problem: impl Into<String>,
    remediation: Vec<String>,
) -> Error {
    let problem = problem.into();
    Error::new(
        ErrorCode::RunnerPolicyDenied,
        format!("Runner '{}' policy denied execution: {problem}", runner.id),
        json!({
            "runner_id": runner.id,
            "field": field,
            "problem": problem,
            "tried": remediation,
        }),
    )
}

fn command_family(command: &[String]) -> &str {
    command.first().map(String::as_str).unwrap_or("runner.exec")
}

fn path_is_inside(root: &str, cwd: &str) -> bool {
    let root = trim_trailing_slashes(root);
    let cwd = trim_trailing_slashes(cwd);
    cwd == root || cwd.starts_with(&format!("{root}/"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn typed_remote_dispatch_preflight_declares_the_remote_program_capability() {
        let preflight = preflight_remote_dispatch(RunnerRemoteDispatchPreflightRequest {
            context: "test dispatch",
            runner_id: "lab",
            command: &[
                "homeboy".to_string(),
                "rig".to_string(),
                "sources".to_string(),
                "list".to_string(),
            ],
            remote_cwd: "/runner/worktree",
            materialized_paths: &[],
            local_only_wrapper_flags: &[],
        })
        .expect("runner-local argv should satisfy controller preflight");

        assert_eq!(preflight.command[0], "homeboy");
        assert_eq!(
            preflight
                .capability_preflight
                .expect("capability preflight")
                .required_commands,
            vec!["homeboy"]
        );
    }
}
