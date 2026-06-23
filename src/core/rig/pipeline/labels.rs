//! Step kind/label rendering plus op serialization helpers.

use super::super::expand::expand_vars;
use super::super::spec::{
    GitOp, PatchOp, PipelineStep, RigSpec, ServiceOp, SharedPathOp, StackOp, SymlinkOp,
};

pub(super) fn step_kind(step: &PipelineStep) -> &'static str {
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

pub(super) fn step_label(rig: &RigSpec, step: &PipelineStep, idx: usize) -> String {
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
