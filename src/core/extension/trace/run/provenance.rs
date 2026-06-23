//! Git/toolchain provenance probes for trace canonicality evidence.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::error::Result;
use crate::core::extension::ExtensionExecutionContext;

use super::super::canonicality::trace_toolchain_provenance_requirements;
use super::super::parsing::{
    TraceComponentsProvenance, TraceEvidenceMetadata, TraceGitProvenance,
    TraceRuntimeAssetProvenance, TraceToolchainProvenance,
};
use super::types::TraceRunWorkflowArgs;

pub(super) fn non_canonical_evidence_hints(evidence: &TraceEvidenceMetadata) -> Vec<String> {
    if evidence.canonical {
        return Vec::new();
    }
    vec![format!(
        "Non-canonical local evidence mode `{}` is active; do not use this run as reviewer-facing proof until the reported canonicality reasons are fixed.",
        evidence.mode
    )]
}

pub(super) fn trace_provenance(
    execution_context: Option<&ExtensionExecutionContext>,
    component_path: &str,
    args: &TraceRunWorkflowArgs,
) -> Result<(TraceToolchainProvenance, TraceComponentsProvenance)> {
    let homeboy = homeboy_git_provenance();
    let target = git_provenance(Path::new(component_path), Some("target"));
    let toolchain_requirements = trace_toolchain_provenance_requirements(execution_context)?;
    let mut reasons = Vec::new();
    let mut toolchains = BTreeMap::new();

    for requirement in &toolchain_requirements {
        if let Some((key, value)) = declared_toolchain_env_value(requirement, args) {
            let mut provenance = git_provenance(Path::new(value.as_str()), Some(&requirement.id));
            provenance.source = Some(format!("env:{key}"));
            toolchains.insert(requirement.id.clone(), provenance);
        } else {
            reasons.push(format!(
                "{} checkout was not resolved for this trace run",
                requirement.label
            ));
        }
    }
    if execution_context.is_none() {
        reasons.push("trace runner used generic local workload discovery".to_string());
    }
    for (label, provenance) in [("homeboy", &homeboy), ("target", &target)] {
        push_git_provenance_reasons(label, provenance, &mut reasons);
    }
    for (id, provenance) in &toolchains {
        let label = toolchain_requirements
            .iter()
            .find(|requirement| requirement.id == *id)
            .map(|requirement| requirement.label.as_str())
            .unwrap_or(id.as_str());
        push_git_provenance_reasons(label, provenance, &mut reasons);
    }
    let canonical = reasons.is_empty();
    let mut runtime_assets = BTreeMap::new();
    runtime_assets.insert(
        "browser_runtime".to_string(),
        browser_runtime_asset_provenance(),
    );

    Ok((
        TraceToolchainProvenance {
            canonical,
            mode: if canonical {
                "canonical"
            } else {
                "development"
            }
            .to_string(),
            reasons,
            homeboy,
            toolchains,
            node: command_version("node", &["--version"]),
            runtime_assets,
        },
        TraceComponentsProvenance {
            target,
            dependencies: Vec::new(),
        },
    ))
}

pub(super) fn push_git_provenance_reasons(
    label: &str,
    provenance: &TraceGitProvenance,
    reasons: &mut Vec<String>,
) {
    if provenance.sha.is_none() || provenance.dirty.is_none() {
        reasons.push(format!(
            "{label} checkout git provenance is incomplete for {}",
            provenance.path
        ));
    } else if provenance.dirty == Some(true) {
        reasons.push(format!(
            "{label} checkout is dirty for trace toolchain provenance: {}",
            provenance.path
        ));
    }
}

fn declared_toolchain_env_value(
    requirement: &crate::core::extension::manifest_config::TraceToolchainProvenanceConfig,
    args: &TraceRunWorkflowArgs,
) -> Option<(String, String)> {
    requirement.env_keys.iter().find_map(|key| {
        args.runner_inputs
            .env
            .iter()
            .find_map(|(name, value)| (name == key).then_some((key.clone(), value.clone())))
            .or_else(|| std::env::var(key).ok().map(|value| (key.clone(), value)))
    })
}

pub(super) fn mark_non_canonical(toolchain: &mut TraceToolchainProvenance, reason: &str) {
    toolchain.canonical = false;
    toolchain.mode = "development".to_string();
    if !toolchain.reasons.iter().any(|item| item == reason) {
        toolchain.reasons.push(reason.to_string());
    }
}

pub(super) fn git_provenance(path: &Path, source: Option<&str>) -> TraceGitProvenance {
    let probe_path = crate::core::git::git_probe_path(path);
    let git_root = crate::core::git::get_git_root(&probe_path.to_string_lossy())
        .ok()
        .map(PathBuf::from)
        .unwrap_or(probe_path);
    TraceGitProvenance {
        path: git_root.to_string_lossy().to_string(),
        sha: git_stdout(&git_root, &["rev-parse", "HEAD"]),
        branch: crate::core::git::current_branch(&git_root),
        dirty: git_dirty_state(&git_root),
        source: source.map(ToString::to_string),
    }
}

fn homeboy_git_provenance() -> TraceGitProvenance {
    let exe_path = std::env::current_exe().ok();
    let exe_parent = exe_path.as_deref().and_then(Path::parent);
    let manifest_dir = env!(concat!("CARGO", "_MANIFEST_DIR"));
    let provenance = exe_parent
        .map(|path| git_provenance(path, Some("homeboy")))
        .filter(|provenance| provenance.sha.is_some());

    provenance.unwrap_or_else(|| git_provenance(Path::new(manifest_dir), Some("homeboy")))
}

fn git_stdout(path: &Path, args: &[&str]) -> Option<String> {
    crate::core::git::run_git(path, args, "trace provenance git probe")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub(super) fn git_dirty_state(path: &Path) -> Option<bool> {
    crate::core::git::run_git(
        path,
        &["status", "--porcelain=v1"],
        "trace provenance git status",
    )
    .ok()
    .map(|status| !status.trim().is_empty())
}

fn command_version(command: &str, args: &[&str]) -> Option<String> {
    Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
}

fn browser_runtime_asset_provenance() -> TraceRuntimeAssetProvenance {
    TraceRuntimeAssetProvenance {
        present: false,
        mode: Some("jspi".to_string()),
        version: None,
        path: None,
    }
}
