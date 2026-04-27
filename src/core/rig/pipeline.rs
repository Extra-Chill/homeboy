//! Rig pipeline executor.
//!
//! Optional step IDs plus `depends_on` edges are topologically ordered before
//! sequential execution. Caching and parallelism are later #1464 phases.
//!
//! Every step emits a `PipelineStepOutcome`. The runner aggregates them into
//! a `PipelineOutcome` with overall success/failure.

use std::collections::{HashMap, VecDeque};
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::Serialize;

use super::check;
use super::expand::expand_vars;
use super::service;
use super::spec::{
    ComponentSpec, GitOp, PatchOp, PipelineStep, RigSpec, ServiceOp, SharedPathOp, SharedPathSpec,
    SymlinkOp, SymlinkSpec,
};
use super::state::{now_rfc3339, RigState, SharedPathState};
use crate::error::{Error, Result};

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
    let steps = rig.pipeline.get(name).cloned().unwrap_or_default();
    let ordered_indices = order_pipeline_steps(rig, name, &steps)?;
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

        let result = run_step(rig, step);

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

fn run_step(rig: &RigSpec, step: &PipelineStep) -> Result<()> {
    match step {
        PipelineStep::Service { id, op, .. } => run_service_step(rig, id, *op),
        PipelineStep::Build { component, .. } => run_build_step(rig, component),
        PipelineStep::Git {
            component,
            op,
            args,
            ..
        } => run_git_step(rig, component, *op, args),
        PipelineStep::Command {
            cmd,
            cwd,
            env,
            label: _,
            ..
        } => run_command_step(rig, cmd, cwd.as_deref(), env),
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
        | PipelineStep::Git { step_id, .. }
        | PipelineStep::Command { step_id, .. }
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
        | PipelineStep::Git { depends_on, .. }
        | PipelineStep::Command { depends_on, .. }
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

fn run_build_step(rig: &RigSpec, component_id: &str) -> Result<()> {
    let (_, path) = resolve_component_path(rig, component_id)?;
    let (result, exit_code) = crate::build::run_with_path(component_id, &path)?;

    if exit_code != 0 {
        let detail = match &result {
            crate::build::BuildResult::Single(output) => {
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
            crate::build::BuildResult::Bulk(_) => format!("exit {}", exit_code),
        };
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "build",
            format!("build {} failed: {}", component_id, detail),
        ));
    }
    Ok(())
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

    let output = crate::git::execute_git_for_release(&path, &arg_refs).map_err(|e| {
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
    Ok(())
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
) -> Result<()> {
    let expanded = expand_vars(rig, cmd);
    let mut command = Command::new("sh");
    command.arg("-c").arg(&expanded);

    if let Some(cwd) = cwd {
        let resolved = expand_vars(rig, cwd);
        command.current_dir(PathBuf::from(resolved));
    }
    for (k, v) in env {
        command.env(k, expand_vars(rig, v));
    }

    let status = command.status().map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!("spawn failed for `{}`: {}", expanded, e),
        )
    })?;

    if !status.success() {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "command",
            format!("`{}` exited {}", expanded, status.code().unwrap_or(-1)),
        ));
    }
    Ok(())
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
        PipelineStep::Git { .. } => "git",
        PipelineStep::Command { .. } => "command",
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
        PipelineStep::Command { cmd, label, .. } => label
            .clone()
            .unwrap_or_else(|| truncate(&expand_vars(rig, cmd), 80)),
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
