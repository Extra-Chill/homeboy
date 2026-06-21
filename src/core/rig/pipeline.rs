//! Rig pipeline executor.
//!
//! Optional step IDs plus `depends_on` edges are topologically ordered before
//! sequential execution. Caching and parallelism are later #1464 phases.
//!
//! Every step emits a `PipelineStepOutcome`. The runner aggregates them into
//! a `PipelineOutcome` with overall success/failure.

use std::collections::{BTreeSet, HashMap, VecDeque};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use super::check;
use super::expand::expand_vars;
use super::service;
use super::spec::{
    ComponentSpec, GitOp, PatchOp, PipelineStep, RigSpec, ServiceOp, SharedPathOp, SharedPathSpec,
    StackOp, SymlinkOp, SymlinkSpec,
};
use super::stack as rig_stack;
use super::state::{now_rfc3339, RigState, SharedPathState};
use super::toolchain;
use crate::core::component::Component;
use crate::core::error::{Error, Result};

/// Result of one pipeline step.
#[derive(Debug, Clone, Serialize)]
pub struct PipelineStepOutcome {
    /// Step kind (`service`, `command`, `symlink`, `check`).
    pub kind: String,
    /// Human-readable label for the step.
    pub label: String,
    /// `"pass"`, `"fail"`, or `"skip"`.
    pub status: String,
    /// Error message when `status = "fail"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PipelineOutcome {
    pub name: String,
    pub steps: Vec<PipelineStepOutcome>,
    pub passed: usize,
    pub failed: usize,
}

impl PipelineOutcome {
    pub fn is_success(&self) -> bool {
        self.failed == 0
    }
}

pub fn run_pipeline(rig: &RigSpec, name: &str, fail_fast: bool) -> Result<PipelineOutcome> {
    run_pipeline_with_settings(rig, name, fail_fast, &[])
}

pub fn run_pipeline_with_settings(
    rig: &RigSpec,
    name: &str,
    fail_fast: bool,
    settings: &[(String, String)],
) -> Result<PipelineOutcome> {
    let steps = rig.pipeline.get(name).cloned().unwrap_or_default();
    let ordered_indices = order_pipeline_steps(rig, name, &steps)?;
    run_ordered_steps(rig, name, &steps, ordered_indices, fail_fast, settings)
}

pub fn run_pipeline_check_groups(
    rig: &RigSpec,
    groups: &[String],
    fail_fast: bool,
) -> Result<PipelineOutcome> {
    let wanted: BTreeSet<&str> = groups.iter().map(String::as_str).collect();
    let steps = rig
        .pipeline
        .get("check")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|step| step_matches_groups(step, &wanted))
        .collect::<Vec<_>>();
    let ordered_indices = order_pipeline_steps(rig, "check", &steps)?;
    run_ordered_steps(rig, "check", &steps, ordered_indices, fail_fast, &[])
}

fn run_ordered_steps(
    rig: &RigSpec,
    name: &str,
    steps: &[PipelineStep],
    ordered_indices: Vec<usize>,
    fail_fast: bool,
    settings: &[(String, String)],
) -> Result<PipelineOutcome> {
    let mut outcomes = Vec::with_capacity(ordered_indices.len());
    let mut failed = 0;
    let mut passed = 0;
    let mut aborted = false;

    for idx in ordered_indices {
        let step = &steps[idx];
        if aborted {
            outcomes.push(PipelineStepOutcome {
                kind: step_kind(step).to_string(),
                label: step_label(rig, step, idx),
                status: "skip".to_string(),
                error: None,
            });
            continue;
        }

        let label = step_label(rig, step, idx);
        crate::log_status!("rig", "{}: {}", name, label);

        let result = run_step(rig, name, step, settings);

        let outcome = match &result {
            Ok(()) => PipelineStepOutcome {
                kind: step_kind(step).to_string(),
                label: label.clone(),
                status: "pass".to_string(),
                error: None,
            },
            Err(e) => PipelineStepOutcome {
                kind: step_kind(step).to_string(),
                label: label.clone(),
                status: "fail".to_string(),
                error: Some(e.to_string()),
            },
        };

        match &result {
            Ok(()) => passed += 1,
            Err(_) => {
                failed += 1;
                if fail_fast {
                    aborted = true;
                }
            }
        }

        outcomes.push(outcome);
    }

    Ok(PipelineOutcome {
        name: name.to_string(),
        steps: outcomes,
        passed,
        failed,
    })
}

fn step_matches_groups(step: &PipelineStep, wanted: &BTreeSet<&str>) -> bool {
    match step {
        PipelineStep::Check { groups, .. } => {
            groups.iter().any(|group| wanted.contains(group.as_str()))
        }
        _ => false,
    }
}

fn run_step(
    rig: &RigSpec,
    pipeline_name: &str,
    step: &PipelineStep,
    settings: &[(String, String)],
) -> Result<()> {
    match step {
        PipelineStep::Service { id, op, .. } => run_service_step(rig, id, *op),
        PipelineStep::Build { component, .. } => run_build_step(rig, component),
        PipelineStep::Extension { component, op, .. } => run_extension_step(rig, component, op),
        PipelineStep::Git {
            component,
            op,
            args,
            ..
        } => run_git_step(rig, component, *op, args),
        PipelineStep::Stack {
            component,
            op,
            dry_run,
            ..
        } => run_stack_step(rig, component, *op, *dry_run),
        PipelineStep::Command { .. } | PipelineStep::CommandIfMissing { .. } => {
            run_command_pipeline_step(rig, step, settings)
        }
        PipelineStep::Requirement { .. } => {
            run_requirement_step(rig, pipeline_name, step, settings)
        }
        PipelineStep::Symlink { op, .. } => run_symlink_step(rig, *op),
        PipelineStep::SharedPath { op, .. } => run_shared_path_step(rig, *op),
        PipelineStep::Patch {
            component,
            file,
            marker,
            after,
            content,
            op,
            ..
        } => run_patch_step(rig, component, file, marker, after.as_deref(), content, *op),
        PipelineStep::Check { spec, .. } => check::evaluate(rig, spec),
    }
}

fn order_pipeline_steps(
    rig: &RigSpec,
    pipeline_name: &str,
    steps: &[PipelineStep],
) -> Result<Vec<usize>> {
    if steps.is_empty() {
        return Ok(Vec::new());
    }

    let mut id_to_index = HashMap::new();
    for (idx, step) in steps.iter().enumerate() {
        if let Some(id) = step_id(step) {
            if let Some(previous_idx) = id_to_index.insert(id, idx) {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    pipeline_name,
                    format!(
                        "duplicate pipeline step id '{}' at positions {} and {}",
                        id, previous_idx, idx
                    ),
                ));
            }
        }
    }

    let mut indegree = vec![0usize; steps.len()];
    let mut dependents = vec![Vec::<usize>::new(); steps.len()];

    for (idx, step) in steps.iter().enumerate() {
        for dependency_id in step_dependencies(step) {
            let Some(&dependency_idx) = id_to_index.get(dependency_id.as_str()) else {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    pipeline_name,
                    format!(
                        "pipeline step {} depends on missing step id '{}'",
                        step_node_label(step, idx),
                        dependency_id
                    ),
                ));
            };
            indegree[idx] += 1;
            dependents[dependency_idx].push(idx);
        }
    }

    for child_indices in &mut dependents {
        child_indices.sort_unstable();
    }

    let mut ready = VecDeque::new();
    for (idx, count) in indegree.iter().enumerate() {
        if *count == 0 {
            ready.push_back(idx);
        }
    }

    let mut ordered = Vec::with_capacity(steps.len());
    while let Some(idx) = ready.pop_front() {
        ordered.push(idx);
        for dependent_idx in dependents[idx].iter().copied() {
            indegree[dependent_idx] -= 1;
            if indegree[dependent_idx] == 0 {
                ready.push_back(dependent_idx);
            }
        }
    }

    if ordered.len() != steps.len() {
        let cycle_members = steps
            .iter()
            .enumerate()
            .filter(|&(idx, _step)| indegree[idx] > 0)
            .map(|(idx, step)| step_node_label(step, idx))
            .collect::<Vec<_>>()
            .join(", ");
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            pipeline_name,
            format!(
                "pipeline dependency cycle detected involving {}",
                cycle_members
            ),
        ));
    }

    Ok(ordered)
}

fn step_id(step: &PipelineStep) -> Option<&str> {
    match step {
        PipelineStep::Service { step_id, .. }
        | PipelineStep::Build { step_id, .. }
        | PipelineStep::Extension { step_id, .. }
        | PipelineStep::Git { step_id, .. }
        | PipelineStep::Stack { step_id, .. }
        | PipelineStep::Command { step_id, .. }
        | PipelineStep::CommandIfMissing { step_id, .. }
        | PipelineStep::Requirement { step_id, .. }
        | PipelineStep::Symlink { step_id, .. }
        | PipelineStep::SharedPath { step_id, .. }
        | PipelineStep::Patch { step_id, .. }
        | PipelineStep::Check { step_id, .. } => step_id.as_deref(),
    }
}

fn step_dependencies(step: &PipelineStep) -> &[String] {
    match step {
        PipelineStep::Service { depends_on, .. }
        | PipelineStep::Build { depends_on, .. }
        | PipelineStep::Extension { depends_on, .. }
        | PipelineStep::Git { depends_on, .. }
        | PipelineStep::Stack { depends_on, .. }
        | PipelineStep::Command { depends_on, .. }
        | PipelineStep::CommandIfMissing { depends_on, .. }
        | PipelineStep::Requirement { depends_on, .. }
        | PipelineStep::Symlink { depends_on, .. }
        | PipelineStep::SharedPath { depends_on, .. }
        | PipelineStep::Patch { depends_on, .. }
        | PipelineStep::Check { depends_on, .. } => depends_on,
    }
}

fn step_node_label(step: &PipelineStep, idx: usize) -> String {
    step_id(step)
        .map(|id| format!("'{}'", id))
        .unwrap_or_else(|| format!("at position {}", idx))
}

pub fn cleanup_shared_paths(rig: &RigSpec) -> Result<()> {
    run_shared_path_step(rig, SharedPathOp::Cleanup)
}

fn resolve_component_path(rig: &RigSpec, component_id: &str) -> Result<(ComponentSpec, String)> {
    let component = rig.components.get(component_id).ok_or_else(|| {
        Error::rig_pipeline_failed(
            &rig.id,
            "build",
            format!(
                "component '{}' not declared in rig `components` map",
                component_id
            ),
        )
    })?;
    let path = expand_vars(rig, &component.path);
    Ok((component.clone(), path))
}

fn resolve_rig_component(rig: &RigSpec, component_id: &str) -> Result<Component> {
    let (component, path) = resolve_component_path(rig, component_id)?;
    let mut resolved = Component {
        id: component_id.to_string(),
        local_path: path,
        remote_url: component.remote_url,
        triage_remote_url: component.triage_remote_url,
        extensions: component.extensions,
        ..Component::default()
    };
    resolved.resolve_remote_path();
    Ok(resolved)
}

fn run_build_step(rig: &RigSpec, component_id: &str) -> Result<()> {
    let component = resolve_rig_component(rig, component_id)?;
    let (result, exit_code) = crate::core::build::run_component(&component)?;

    if exit_code != 0 {
        let detail = match &result {
            crate::core::build::BuildResult::Single(output) => {
                let tail = output
                    .output
                    .stderr
                    .lines()
                    .rev()
                    .take(5)
                    .collect::<Vec<_>>()
                    .into_iter()
                    .rev()
                    .collect::<Vec<_>>()
                    .join("\n");
                if tail.trim().is_empty() {
                    format!("exit {}", exit_code)
                } else {
                    format!("exit {} — {}", exit_code, tail)
                }
            }
            crate::core::build::BuildResult::Bulk(_) => format!("exit {}", exit_code),
        };
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "build",
            format!("build {} failed: {}", component_id, detail),
        ));
    }
    Ok(())
}

fn run_extension_step(rig: &RigSpec, component_id: &str, op: &str) -> Result<()> {
    match op {
        "build" => run_build_step(rig, component_id),
        other => Err(Error::rig_pipeline_failed(
            &rig.id,
            "extension",
            format!(
                "extension op '{}' is not supported for component '{}'; supported ops: build",
                other, component_id
            ),
        )),
    }
}

fn run_git_step(rig: &RigSpec, component_id: &str, op: GitOp, extra_args: &[String]) -> Result<()> {
    let (_, path) = resolve_component_path(rig, component_id)?;

    let base_args: Vec<String> = match op {
        GitOp::Status => vec!["status".into(), "--porcelain=v1".into()],
        GitOp::Pull => vec!["pull".into()],
        GitOp::Push => vec!["push".into()],
        GitOp::Fetch => vec!["fetch".into()],
        GitOp::Checkout => vec!["checkout".into()],
        GitOp::CurrentBranch => vec!["rev-parse".into(), "--abbrev-ref".into(), "HEAD".into()],
        GitOp::Rebase => vec!["rebase".into()],
        GitOp::CherryPick => vec!["cherry-pick".into()],
    };
    let mut full_args: Vec<String> = base_args;
    for arg in extra_args {
        full_args.push(expand_vars(rig, arg));
    }
    let arg_refs: Vec<&str> = full_args.iter().map(String::as_str).collect();

    let output = crate::core::git::execute_git_for_release(&path, &arg_refs).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!("spawn `git {}` in {}: {}", full_args.join(" "), path, e),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!(
                "`git {}` in {} exited {}{}",
                full_args.join(" "),
                path,
                code,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            ),
        ));
    }
    fail_if_git_index_unmerged(rig, &path, &full_args)?;
    Ok(())
}

fn fail_if_git_index_unmerged(rig: &RigSpec, path: &str, completed_args: &[String]) -> Result<()> {
    let output = crate::core::git::execute_git_for_release(
        path,
        &["diff", "--name-only", "--diff-filter=U"],
    )
    .map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!(
                "spawn `git diff --name-only --diff-filter=U` in {}: {}",
                path, e
            ),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let code = output.status.code().unwrap_or(-1);
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "git",
            format!(
                "`git diff --name-only --diff-filter=U` in {} exited {}{}",
                path,
                code,
                if stderr.trim().is_empty() {
                    String::new()
                } else {
                    format!(": {}", stderr.trim())
                }
            ),
        ));
    }

    let unresolved = String::from_utf8_lossy(&output.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_string)
        .collect::<Vec<_>>();

    if unresolved.is_empty() {
        return Ok(());
    }

    Err(Error::rig_pipeline_failed(
        &rig.id,
        "git",
        format!(
            "`git {}` in {} completed but left unresolved conflicts: {}",
            completed_args.join(" "),
            path,
            unresolved.join(", ")
        ),
    ))
}

fn run_stack_step(rig: &RigSpec, component_id: &str, op: StackOp, dry_run: bool) -> Result<()> {
    match op {
        StackOp::Sync => {
            rig_stack::run_component_sync(rig, component_id, dry_run)?;
            Ok(())
        }
    }
}

fn run_service_step(rig: &RigSpec, service_id: &str, op: ServiceOp) -> Result<()> {
    match op {
        ServiceOp::Start => {
            service::start(rig, service_id)?;
            Ok(())
        }
        ServiceOp::Stop => service::stop(rig, service_id),
        ServiceOp::Health => {
            let spec = rig.services.get(service_id).ok_or_else(|| {
                Error::rig_service_failed(&rig.id, service_id, "service not declared in rig spec")
            })?;
            if let Some(health) = &spec.health {
                check::evaluate(rig, health)?;
            }
            match service::status(&rig.id, service_id)? {
                service::ServiceStatus::Running(_) => Ok(()),
                service::ServiceStatus::Stopped => Err(Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    "service is stopped",
                )),
                service::ServiceStatus::Stale(pid) => Err(Error::rig_service_failed(
                    &rig.id,
                    service_id,
                    format!("recorded PID {} is not alive", pid),
                )),
            }
        }
    }
}

fn run_command_step(
    rig: &RigSpec,
    cmd: &str,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
    settings: &[(String, String)],
) -> Result<()> {
    let expanded = expand_vars(rig, cmd);
    let mut command = Command::new(command_step_shell());
    command.arg("-c").arg(&expanded);

    if let Some(cwd) = cwd {
        let resolved = expand_vars(rig, cwd);
        command.current_dir(PathBuf::from(resolved));
    }

    if !env.contains_key("PATH") {
        if let Some(path) = toolchain::command_step_path() {
            command.env("PATH", path);
        }
    }

    for (k, v) in env {
        command.env(k, expand_vars(rig, v));
    }

    for (k, v) in settings_env(settings) {
        command.env(k, v);
    }

    let status = command.status().map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!("spawn failed for `{}`: {}", expanded, e),
        )
    })?;

    if !status.success() {
        let code = status.code().unwrap_or(-1);
        let hint = if code == 127 {
            ": command not found. Rig command steps bootstrap common toolchain PATHs automatically; if the tool lives elsewhere, set env.PATH for this step or prefer a typed build/git/check step"
        } else {
            ""
        };
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!("`{}` exited {}{}", expanded, code, hint),
        ));
    }
    Ok(())
}

fn settings_env(settings: &[(String, String)]) -> Vec<(String, String)> {
    settings
        .iter()
        .map(|(key, value)| {
            (
                format!("HOMEBOY_SETTINGS_{}", key.to_uppercase()),
                value.clone(),
            )
        })
        .collect()
}

fn run_command_pipeline_step(
    rig: &RigSpec,
    step: &PipelineStep,
    settings: &[(String, String)],
) -> Result<()> {
    match step {
        PipelineStep::Command { cmd, cwd, env, .. } => {
            run_command_step(rig, cmd, cwd.as_deref(), env, settings)
        }
        PipelineStep::CommandIfMissing {
            missing,
            cmd,
            cwd,
            env,
            ..
        } => run_command_if_missing_step(rig, missing, cmd, cwd.as_deref(), env, settings),
        _ => unreachable!("command pipeline helper only accepts command steps"),
    }
}

fn run_command_if_missing_step(
    rig: &RigSpec,
    missing: &str,
    cmd: &str,
    cwd: Option<&str>,
    env: &HashMap<String, String>,
    settings: &[(String, String)],
) -> Result<()> {
    let expanded_missing = expand_vars(rig, missing);
    let missing_path = PathBuf::from(&expanded_missing);
    let resolved_missing = if missing_path.is_absolute() {
        missing_path
    } else if let Some(cwd) = cwd {
        PathBuf::from(expand_vars(rig, cwd)).join(missing_path)
    } else {
        missing_path
    };

    if resolved_missing.exists() {
        return Ok(());
    }

    run_command_step(rig, cmd, cwd, env, settings)
}

fn run_requirement_step(
    rig: &RigSpec,
    pipeline_name: &str,
    step: &PipelineStep,
    settings: &[(String, String)],
) -> Result<()> {
    let PipelineStep::Requirement {
        path,
        file,
        dir,
        component,
        component_path_contains,
        executable,
        executable_env,
        executable_env_aliases,
        prepare_command,
        prepare_phases,
        cwd,
        env,
        remediation,
        ..
    } = step
    else {
        unreachable!("requirement helper only accepts requirement steps")
    };

    if component.is_some() != component_path_contains.is_some() {
        return Err(Error::validation_invalid_argument(
            "requirement.component_path_contains",
            "Requirement must specify both `component` and `component_path_contains` or neither",
            None,
            None,
        ));
    }

    if let (Some(component_id), Some(required)) = (component, component_path_contains) {
        let (_, component_path) = resolve_component_path(rig, component_id)?;
        if !component_path.contains(required) {
            return Err(requirement_failed(
                rig,
                format!(
                    "component `{}` path `{}` does not contain `{}`",
                    component_id, component_path, required
                ),
                remediation.as_deref(),
            ));
        }
    }

    let mut path_specs = Vec::new();
    if let Some(value) = path {
        path_specs.push(("path", value));
    }
    if let Some(value) = file {
        path_specs.push(("file", value));
    }
    if let Some(value) = dir {
        path_specs.push(("dir", value));
    }

    if path_specs.is_empty() && component_path_contains.is_none() && executable.is_none() {
        return Err(Error::validation_invalid_argument(
            "requirement",
            "Requirement must specify at least one of `path`, `file`, `dir`, `component_path_contains`, or `executable`",
            None,
            None,
        ));
    }

    let missing_before_prepare = path_specs
        .iter()
        .filter_map(|(kind, declared)| {
            requirement_path_failure(rig, cwd.as_deref(), kind, declared)
        })
        .chain(requirement_executable_failure(
            rig,
            executable.as_deref(),
            executable_env.as_deref(),
            executable_env_aliases,
            env,
        ))
        .collect::<Vec<_>>();

    if !missing_before_prepare.is_empty()
        && prepare_command.is_some()
        && prepare_phases.iter().any(|phase| phase == pipeline_name)
    {
        run_command_step(
            rig,
            prepare_command.as_deref().unwrap(),
            cwd.as_deref(),
            env,
            settings,
        )
        .map_err(|error| {
            requirement_failed(
                rig,
                format!(
                    "{}; prepare command failed: {}",
                    missing_before_prepare.join("; "),
                    error
                ),
                remediation.as_deref(),
            )
        })?;
    }

    let missing = path_specs
        .iter()
        .filter_map(|(kind, declared)| {
            requirement_path_failure(rig, cwd.as_deref(), kind, declared)
        })
        .chain(requirement_executable_failure(
            rig,
            executable.as_deref(),
            executable_env.as_deref(),
            executable_env_aliases,
            env,
        ))
        .collect::<Vec<_>>();

    if !missing.is_empty() {
        return Err(requirement_failed(
            rig,
            missing.join("; "),
            remediation.as_deref(),
        ));
    }

    Ok(())
}

fn requirement_path_failure(
    rig: &RigSpec,
    cwd: Option<&str>,
    kind: &str,
    declared: &str,
) -> Option<String> {
    let resolved = resolve_requirement_path(rig, cwd, declared);
    let ok = match kind {
        "file" => resolved.is_file(),
        "dir" => resolved.is_dir(),
        _ => resolved.exists(),
    };

    (!ok).then(|| {
        let display = resolved.display();
        if declared == display.to_string() {
            format!("{} does not exist: {}", kind, display)
        } else {
            format!(
                "{} does not exist: {} (declared: {})",
                kind, display, declared
            )
        }
    })
}

fn requirement_executable_failure(
    rig: &RigSpec,
    executable: Option<&str>,
    executable_env: Option<&str>,
    executable_env_aliases: &[String],
    env: &HashMap<String, String>,
) -> Option<String> {
    let executable = executable?;
    let executable = expand_vars(rig, executable);
    let mut attempts = Vec::new();

    for name in executable_env_names(executable_env, executable_env_aliases) {
        match env_value(name, env) {
            Some(value) if !value.trim().is_empty() => {
                let candidate = expand_vars(rig, &value);
                if find_executable(&candidate, env).is_some() {
                    return None;
                }
                attempts.push(format!("{}={} (not executable)", name, candidate));
            }
            _ => attempts.push(format!("{} is unset or empty", name)),
        }
    }

    if find_executable(&executable, env).is_some() {
        return None;
    }

    attempts.push(declared_executable_attempt(&executable));

    Some(format!(
        "executable `{}` could not be resolved; tried {}",
        executable,
        attempts.join(", ")
    ))
}

fn executable_env_names<'a>(
    executable_env: Option<&'a str>,
    executable_env_aliases: &'a [String],
) -> Vec<&'a str> {
    executable_env
        .into_iter()
        .chain(executable_env_aliases.iter().map(String::as_str))
        .filter(|name| !name.trim().is_empty())
        .collect()
}

fn declared_executable_attempt(declared: &str) -> String {
    let path = Path::new(declared);
    if declared.contains(std::path::MAIN_SEPARATOR) || path.is_absolute() {
        format!("declared executable `{}`", declared)
    } else {
        format!("PATH lookup for `{}`", declared)
    }
}

fn env_value(name: &str, env: &HashMap<String, String>) -> Option<String> {
    env.get(name).cloned().or_else(|| std::env::var(name).ok())
}

fn find_executable(candidate: &str, env: &HashMap<String, String>) -> Option<PathBuf> {
    let path = Path::new(candidate);
    if candidate.contains(std::path::MAIN_SEPARATOR) || path.is_absolute() {
        return is_executable_file(path).then(|| path.to_path_buf());
    }

    let path_var = env
        .get("PATH")
        .map(OsString::from)
        .or_else(|| std::env::var_os("PATH"));
    let path_var = path_var?;

    std::env::split_paths(&path_var)
        .map(|dir| dir.join(candidate))
        .find(|path| is_executable_file(path))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;

    path.is_file()
        && path
            .metadata()
            .map(|metadata| metadata.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

fn resolve_requirement_path(rig: &RigSpec, cwd: Option<&str>, declared: &str) -> PathBuf {
    let expanded = expand_vars(rig, declared);
    let path = PathBuf::from(&expanded);
    if path.is_absolute() {
        path
    } else if let Some(cwd) = cwd {
        PathBuf::from(expand_vars(rig, cwd)).join(path)
    } else {
        path
    }
}

fn requirement_failed(rig: &RigSpec, message: String, remediation: Option<&str>) -> Error {
    let detail = remediation
        .map(|text| format!("{}. Remediation: {}", message, expand_vars(rig, text)))
        .unwrap_or(message);
    Error::rig_pipeline_failed(&rig.id, "requirement", detail)
}

#[cfg(unix)]
fn command_step_shell() -> &'static str {
    "/bin/sh"
}

#[cfg(not(unix))]
fn command_step_shell() -> &'static str {
    "sh"
}

fn run_symlink_step(rig: &RigSpec, op: SymlinkOp) -> Result<()> {
    for link in &rig.symlinks {
        match op {
            SymlinkOp::Ensure => ensure_symlink(rig, link)?,
            SymlinkOp::Verify => verify_symlink(rig, link)?,
        }
    }
    Ok(())
}

fn ensure_symlink(rig: &RigSpec, link: &SymlinkSpec) -> Result<()> {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));

    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::rig_pipeline_failed(
                &rig.id,
                "symlink",
                format!("create parent of {}: {}", link_path.display(), e),
            )
        })?;
    }

    if link_path.exists() || link_path.is_symlink() {
        if let Ok(current) = std::fs::read_link(&link_path) {
            if current == target_path {
                return Ok(());
            }
        }
        std::fs::remove_file(&link_path).map_err(|e| {
            Error::rig_pipeline_failed(
                &rig.id,
                "symlink",
                format!("remove existing {}: {}", link_path.display(), e),
            )
        })?;
    }

    create_symlink(&target_path, &link_path).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!(
                "create {} → {}: {}",
                link_path.display(),
                target_path.display(),
                e
            ),
        )
    })?;
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "rig symlinks are not supported on this platform (Unix only)",
    ))
}

fn verify_symlink(rig: &RigSpec, link: &SymlinkSpec) -> Result<()> {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));

    if !link_path.is_symlink() {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!("{} is not a symlink", link_path.display()),
        ));
    }
    let current = std::fs::read_link(&link_path).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!("read {}: {}", link_path.display(), e),
        )
    })?;
    if current != target_path {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!(
                "{} points at {}, expected {}",
                link_path.display(),
                current.display(),
                target_path.display()
            ),
        ));
    }
    Ok(())
}

fn run_shared_path_step(rig: &RigSpec, op: SharedPathOp) -> Result<()> {
    if rig.shared_paths.is_empty() {
        return Ok(());
    }

    if op == SharedPathOp::Verify {
        for shared in &rig.shared_paths {
            verify_shared_path(rig, shared)?;
        }
        return Ok(());
    }

    let mut state = RigState::load(&rig.id)?;
    let mut state_changed = false;

    for shared in &rig.shared_paths {
        match op {
            SharedPathOp::Ensure => {
                ensure_shared_path(rig, shared, &mut state, &mut state_changed)?
            }
            SharedPathOp::Verify => verify_shared_path(rig, shared)?,
            SharedPathOp::Cleanup => {
                cleanup_shared_path(rig, shared, &mut state, &mut state_changed)?
            }
        }
    }

    if state_changed {
        state.save(&rig.id)?;
    }
    Ok(())
}

fn ensure_shared_path(
    rig: &RigSpec,
    shared: &SharedPathSpec,
    state: &mut RigState,
    state_changed: &mut bool,
) -> Result<()> {
    let (link_path, target_path) = resolve_shared_paths(rig, shared);
    let key = shared_path_key(&link_path);

    match std::fs::symlink_metadata(&link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            if current == target_path {
                if !target_path.exists() {
                    return Err(Error::rig_pipeline_failed(
                        &rig.id,
                        "shared-path",
                        format!("shared target {} does not exist", target_path.display()),
                    ));
                }
                return Ok(());
            }
            Err(Error::rig_pipeline_failed(
                &rig.id,
                "shared-path",
                format!(
                    "{} points at {}, expected {} — refusing to replace an existing symlink",
                    link_path.display(),
                    current.display(),
                    target_path.display()
                ),
            ))
        }
        Ok(_) => {
            if state.shared_paths.remove(&key).is_some() {
                *state_changed = true;
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !target_path.exists() {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "shared target {} does not exist for {}",
                        target_path.display(),
                        link_path.display()
                    ),
                ));
            }
            let parent = link_path.parent().ok_or_else(|| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("{} has no parent directory", link_path.display()),
                )
            })?;
            if !parent.exists() {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "parent directory {} does not exist for {}",
                        parent.display(),
                        link_path.display()
                    ),
                ));
            }

            create_symlink(&target_path, &link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "create {} → {}: {}",
                        link_path.display(),
                        target_path.display(),
                        e
                    ),
                )
            })?;
            state.shared_paths.insert(
                key,
                SharedPathState {
                    target: target_path.to_string_lossy().into_owned(),
                    created_at: now_rfc3339(),
                },
            );
            *state_changed = true;
            Ok(())
        }
        Err(e) => Err(Error::rig_pipeline_failed(
            &rig.id,
            "shared-path",
            format!("stat {}: {}", link_path.display(), e),
        )),
    }
}

fn verify_shared_path(rig: &RigSpec, shared: &SharedPathSpec) -> Result<()> {
    let (link_path, target_path) = resolve_shared_paths(rig, shared);
    match std::fs::symlink_metadata(&link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            if current != target_path {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "{} points at {}, expected {}",
                        link_path.display(),
                        current.display(),
                        target_path.display()
                    ),
                ));
            }
            if !target_path.exists() {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("shared target {} does not exist", target_path.display()),
                ));
            }
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::rig_pipeline_failed(
            &rig.id,
            "shared-path",
            format!("{} is missing", link_path.display()),
        )),
        Err(e) => Err(Error::rig_pipeline_failed(
            &rig.id,
            "shared-path",
            format!("stat {}: {}", link_path.display(), e),
        )),
    }
}

fn cleanup_shared_path(
    rig: &RigSpec,
    shared: &SharedPathSpec,
    state: &mut RigState,
    state_changed: &mut bool,
) -> Result<()> {
    let (link_path, _target_path) = resolve_shared_paths(rig, shared);
    let key = shared_path_key(&link_path);
    let Some(owned) = state.shared_paths.get(&key).cloned() else {
        return Ok(());
    };
    let owned_target = PathBuf::from(&owned.target);

    if let Ok(metadata) = std::fs::symlink_metadata(&link_path) {
        if metadata.file_type().is_symlink() {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            if current == owned_target {
                std::fs::remove_file(&link_path).map_err(|e| {
                    Error::rig_pipeline_failed(
                        &rig.id,
                        "shared-path",
                        format!("remove {}: {}", link_path.display(), e),
                    )
                })?;
            }
        }
    }

    state.shared_paths.remove(&key);
    *state_changed = true;
    Ok(())
}

fn resolve_shared_paths(rig: &RigSpec, shared: &SharedPathSpec) -> (PathBuf, PathBuf) {
    (
        PathBuf::from(expand_vars(rig, &shared.link)),
        PathBuf::from(expand_vars(rig, &shared.target)),
    )
}

fn shared_path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

/// Apply or verify an idempotent local-only patch.
///
/// `apply` semantics:
/// - If `marker` already appears in the file → no-op (idempotent).
/// - If `after` is set and not in the file → fail with "anchor missing"
///   (file structure changed; refuse to guess where to insert).
/// - If `after` is set and present → insert `content` on the next line
///   after the first occurrence.
/// - If `after` is `None` → append `content` to the end of the file.
/// - Resulting file must contain `marker` (validated against `content`
///   at apply time so misconfigured specs error early instead of
///   double-applying on every run).
///
/// `verify` semantics: pass iff `marker` is present. Read-only — for
/// `check` pipelines that surface stale or unpatched checkouts.
fn run_patch_step(
    rig: &RigSpec,
    component_id: &str,
    file_rel: &str,
    marker: &str,
    after: Option<&str>,
    content: &str,
    op: PatchOp,
) -> Result<()> {
    let (_, component_path) = resolve_component_path(rig, component_id)?;
    let expanded_rel = expand_vars(rig, file_rel);
    let path = if PathBuf::from(&expanded_rel).is_absolute() {
        PathBuf::from(&expanded_rel)
    } else {
        PathBuf::from(&component_path).join(&expanded_rel)
    };

    let body = std::fs::read_to_string(&path).map_err(|e| {
        Error::rig_pipeline_failed(&rig.id, "patch", format!("read {}: {}", path.display(), e))
    })?;

    if body.contains(marker) {
        return Ok(()); // Already applied (apply) / present (verify).
    }

    if op == PatchOp::Verify {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "patch",
            format!(
                "marker {:?} not found in {} — patch missing or stale checkout",
                marker,
                path.display()
            ),
        ));
    }

    if !content.contains(marker) {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "patch",
            format!(
                "patch content does not contain marker {:?} — applying it would not be detectable next run, so the step would re-apply forever",
                marker
            ),
        ));
    }

    let new_body = match after {
        Some(anchor) => {
            let anchor_idx = body.find(anchor).ok_or_else(|| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "patch",
                    format!(
                        "anchor {:?} not found in {} — file structure changed, refusing to guess insertion point",
                        anchor,
                        path.display()
                    ),
                )
            })?;
            // Insert at the start of the line *after* the anchor's line.
            let after_anchor = anchor_idx + anchor.len();
            let next_newline = body[after_anchor..]
                .find('\n')
                .map(|n| after_anchor + n + 1)
                .unwrap_or(body.len());
            let mut out = String::with_capacity(body.len() + content.len() + 1);
            out.push_str(&body[..next_newline]);
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out.push_str(&body[next_newline..]);
            out
        }
        None => {
            let mut out = body.clone();
            if !out.ends_with('\n') && !out.is_empty() {
                out.push('\n');
            }
            out.push_str(content);
            if !content.ends_with('\n') {
                out.push('\n');
            }
            out
        }
    };

    std::fs::write(&path, new_body).map_err(|e| {
        Error::rig_pipeline_failed(&rig.id, "patch", format!("write {}: {}", path.display(), e))
    })?;

    Ok(())
}

fn step_kind(step: &PipelineStep) -> &'static str {
    match step {
        PipelineStep::Service { .. } => "service",
        PipelineStep::Build { .. } => "build",
        PipelineStep::Extension { .. } => "extension",
        PipelineStep::Git { .. } => "git",
        PipelineStep::Stack { .. } => "stack",
        PipelineStep::Command { .. } => "command",
        PipelineStep::CommandIfMissing { .. } => "command-if-missing",
        PipelineStep::Requirement { .. } => "requirement",
        PipelineStep::Symlink { .. } => "symlink",
        PipelineStep::SharedPath { .. } => "shared-path",
        PipelineStep::Patch { .. } => "patch",
        PipelineStep::Check { .. } => "check",
    }
}

fn step_label(rig: &RigSpec, step: &PipelineStep, idx: usize) -> String {
    match step {
        PipelineStep::Service { id, op, .. } => format!("service {} {}", id, serialize_op(*op)),
        PipelineStep::Build {
            component, label, ..
        } => label
            .clone()
            .unwrap_or_else(|| format!("build {}", component)),
        PipelineStep::Extension {
            component,
            op,
            label,
            ..
        } => label
            .clone()
            .unwrap_or_else(|| format!("extension {} {}", op, component)),
        PipelineStep::Git {
            component,
            op,
            args,
            label,
            ..
        } => label.clone().unwrap_or_else(|| {
            let joined = if args.is_empty() {
                String::new()
            } else {
                format!(" {}", args.join(" "))
            };
            format!("git {} {}{}", serialize_git_op(*op), component, joined)
        }),
        PipelineStep::Stack {
            component,
            op,
            dry_run,
            label,
            ..
        } => label.clone().unwrap_or_else(|| {
            format!(
                "stack {} {}{}",
                serialize_stack_op(*op),
                component,
                if *dry_run { " --dry-run" } else { "" }
            )
        }),
        PipelineStep::Command { cmd, label, .. } => label
            .clone()
            .unwrap_or_else(|| truncate(&expand_vars(rig, cmd), 80)),
        PipelineStep::CommandIfMissing { cmd, label, .. } => label
            .clone()
            .unwrap_or_else(|| truncate(&expand_vars(rig, cmd), 80)),
        PipelineStep::Requirement {
            path,
            file,
            dir,
            component,
            component_path_contains,
            executable,
            label,
            ..
        } => label.clone().unwrap_or_else(|| {
            if let Some(path) = file {
                format!("require file {}", truncate(path, 60))
            } else if let Some(path) = dir {
                format!("require dir {}", truncate(path, 60))
            } else if let Some(path) = path {
                format!("require path {}", truncate(path, 60))
            } else if let (Some(component), Some(required)) = (component, component_path_contains) {
                format!("require {} path contains {}", component, required)
            } else if let Some(executable) = executable {
                format!("require executable {}", executable)
            } else {
                format!("requirement #{}", idx + 1)
            }
        }),
        PipelineStep::Symlink { op, .. } => format!("symlink {}", serialize_symlink_op(*op)),
        PipelineStep::SharedPath { op, .. } => {
            format!("shared-path {}", serialize_shared_path_op(*op))
        }
        PipelineStep::Patch {
            component,
            file,
            op,
            label,
            ..
        } => label.clone().unwrap_or_else(|| {
            format!(
                "patch {} {} {}",
                serialize_patch_op(*op),
                component,
                truncate(file, 60)
            )
        }),
        PipelineStep::Check { label, .. } => label
            .clone()
            .unwrap_or_else(|| format!("check #{}", idx + 1)),
    }
}

fn serialize_git_op(op: GitOp) -> &'static str {
    match op {
        GitOp::Status => "status",
        GitOp::Pull => "pull",
        GitOp::Push => "push",
        GitOp::Fetch => "fetch",
        GitOp::Checkout => "checkout",
        GitOp::CurrentBranch => "current-branch",
        GitOp::Rebase => "rebase",
        GitOp::CherryPick => "cherry-pick",
    }
}

fn serialize_stack_op(op: StackOp) -> &'static str {
    match op {
        StackOp::Sync => "sync",
    }
}

fn serialize_op(op: ServiceOp) -> &'static str {
    match op {
        ServiceOp::Start => "start",
        ServiceOp::Stop => "stop",
        ServiceOp::Health => "health",
    }
}

fn serialize_symlink_op(op: SymlinkOp) -> &'static str {
    match op {
        SymlinkOp::Ensure => "ensure",
        SymlinkOp::Verify => "verify",
    }
}

fn serialize_shared_path_op(op: SharedPathOp) -> &'static str {
    match op {
        SharedPathOp::Ensure => "ensure",
        SharedPathOp::Verify => "verify",
        SharedPathOp::Cleanup => "cleanup",
    }
}

fn serialize_patch_op(op: PatchOp) -> &'static str {
    match op {
        PatchOp::Apply => "apply",
        PatchOp::Verify => "verify",
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/pipeline_test.rs"]
mod pipeline_test;
