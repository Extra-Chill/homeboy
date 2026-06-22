//! Public audit entry points and component audit-config resolution.
//!
//! Mechanically split out of `mod.rs`; the public API is preserved by the
//! re-export in the module root.

use std::path::Path;

use super::detectors::source_policy;
use super::engine::audit_internal;
use super::execution_plan::AuditExecutionPlan;
use super::findings::Finding;
use super::types::{AuditWithAnalysis, CodeAuditResult};
use super::{fingerprint, walker};
use crate::core::component::AuditConfig;
use crate::core::{component, Result};

/// Audit a registered component by ID.
pub fn audit_component(component_id: &str) -> Result<CodeAuditResult> {
    let comp = component::resolve_effective(Some(component_id), None, None)?;
    component::validate_local_path(&comp)?;
    audit_path_with_id(component_id, &comp.local_path)
}

/// Read reference dependency paths from HOMEBOY_AUDIT_REFERENCE_PATHS env var.
///
/// Reference dependencies are external codebases (e.g. WordPress core, plugin
/// dependencies) whose fingerprints are included in cross-reference analysis
/// (dead code detection) but excluded from convention discovery and duplication
/// detection. This eliminates false positives for functions called via framework
/// hooks, callbacks, or inherited methods.
fn read_reference_paths_from_env() -> Vec<String> {
    std::env::var("HOMEBOY_AUDIT_REFERENCE_PATHS")
        .unwrap_or_default()
        .lines()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty() && Path::new(s).is_dir())
        .collect()
}

/// Audit a filesystem path directly (no registered component needed).
pub fn audit_path(path: &str) -> Result<CodeAuditResult> {
    let p = Path::new(path);
    if !p.is_dir() {
        return Err(crate::core::Error::validation_invalid_argument(
            "path",
            format!("Not a directory: {}", path),
            None,
            None,
        ));
    }

    // Use directory name as component_id
    let name = p
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    audit_path_with_id(&name, path)
}

/// Core audit logic shared by both entry points.
/// Also available for callers that have a component ID and an overridden path.
pub fn audit_path_with_id(component_id: &str, source_path: &str) -> Result<CodeAuditResult> {
    let ref_paths = read_reference_paths_from_env();
    audit_internal(
        component_id,
        source_path,
        None,
        None,
        &ref_paths,
        &AuditExecutionPlan::full(),
        &[],
    )
    .map(|audit| audit.result)
}

/// Run only configured source policies for a component path.
pub fn source_policy_findings_for_path(
    component_id: &str,
    source_path: &str,
) -> Result<Vec<Finding>> {
    let root = Path::new(source_path);
    if !root.is_dir() {
        return Err(crate::core::Error::validation_invalid_argument(
            "path",
            format!("Not a directory: {source_path}"),
            None,
            None,
        ));
    }

    let audit_config = audit_config_for(component_id, root, &[]);
    let snapshot = walker::walk_all_source_files_snapshot(root);
    let fingerprints = snapshot
        .iter()
        .filter_map(|(path, content)| fingerprint::fingerprint_content(path, root, content))
        .collect::<Vec<_>>();
    let fingerprint_refs = fingerprints.iter().collect::<Vec<_>>();

    Ok(source_policy::run(
        &fingerprint_refs,
        &audit_config.source_policies,
    ))
}

pub(crate) fn audit_path_with_id_with_plan_and_analysis(
    component_id: &str,
    source_path: &str,
    plan: &AuditExecutionPlan,
    reference_paths: &[String],
    extension_overrides: &[String],
) -> Result<AuditWithAnalysis> {
    audit_internal(
        component_id,
        source_path,
        None,
        None,
        reference_paths,
        plan,
        extension_overrides,
    )
}

/// Audit only specific files within a component path.
///
/// Used for PR-scoped audits (`--changed-since`) where only changed files
/// should be checked. Conventions are discovered from the full codebase,
/// but findings are scoped to changed files + their affected call sites.
///
/// When `git_ref` is provided, the engine diffs fingerprints of changed files
/// against their base-ref versions to detect symbol changes (renames, removals,
/// signature changes), then fans out to find all files that reference those
/// changed symbols. This catches breakage at call sites, not just in changed files.
pub fn audit_path_scoped(
    component_id: &str,
    source_path: &str,
    file_filter: &[String],
    git_ref: Option<&str>,
) -> Result<CodeAuditResult> {
    let ref_paths = read_reference_paths_from_env();
    audit_internal(
        component_id,
        source_path,
        Some(file_filter),
        git_ref,
        &ref_paths,
        &AuditExecutionPlan::full(),
        &[],
    )
    .map(|audit| audit.result)
}

pub(crate) fn audit_path_scoped_with_plan_and_analysis(
    component_id: &str,
    source_path: &str,
    file_filter: &[String],
    git_ref: Option<&str>,
    plan: &AuditExecutionPlan,
    reference_paths: &[String],
    extension_overrides: &[String],
) -> Result<AuditWithAnalysis> {
    audit_internal(
        component_id,
        source_path,
        Some(file_filter),
        git_ref,
        reference_paths,
        plan,
        extension_overrides,
    )
}

pub(super) fn audit_config_for(
    component_id: &str,
    root: &Path,
    extension_overrides: &[String],
) -> AuditConfig {
    let component =
        component::discover_from_portable(root).or_else(|| component::load(component_id).ok());
    let mut audit_config = AuditConfig::default();

    if let Some(component) = &component {
        if let Some(extensions) = &component.extensions {
            for extension_id in extensions.keys() {
                if let Ok(manifest) = crate::core::extension::load_extension(extension_id) {
                    if let Some(rules) = manifest.audit_detector_rules() {
                        audit_config.merge(rules);
                    }
                }
            }
        }

        if let Some(component_rules) = &component.audit {
            audit_config.merge(component_rules);
        }
    }

    for extension_id in extension_overrides {
        if let Ok(manifest) = crate::core::extension::load_extension(extension_id) {
            if let Some(rules) = manifest.audit_detector_rules() {
                audit_config.merge(rules);
            }
        }
    }

    audit_config
}
