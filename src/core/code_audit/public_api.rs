//! public_api — extracted from mod.rs.

use std::path::Path;
use crate::{component, is_zero, Result};
use docs_audit::claims::ClaimConfidence;
use std::fs;
use crate::core::code_audit::types::CodeAuditResult;
use crate::core::code_audit::audit_internal;
use crate::core::code_audit::classify_broken_doc_ref;
use crate::core::*;


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
pub(crate) fn read_reference_paths_from_env() -> Vec<String> {
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
        return Err(crate::Error::validation_invalid_argument(
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
    audit_internal(component_id, source_path, None, None, &ref_paths)
}

/// Detect documentation drift — broken and stale references in markdown files.
///
/// Scans all `.md` files in common docs directories, extracts verifiable claims
/// (file paths, directory paths, class names), and checks each against the
/// codebase. Broken claims become `Finding` entries in the unified audit pipeline.
pub(crate) fn detect_doc_drift(root: &Path, component_id: &str) -> Vec<Finding> {
    use docs_audit::claims::ClaimConfidence;

    let mut findings = Vec::new();

    // Find docs directory
    let docs_dirs = ["docs", "doc", "documentation"];
    let docs_entry = docs_dirs.iter().find_map(|d| {
        let p = root.join(d);
        if p.is_dir() {
            Some((p, *d))
        } else {
            None
        }
    });

    let Some((docs_path, docs_dir_name)) = docs_entry else {
        return findings;
    };

    let doc_excludes = if let Ok(comp) = component::load(component_id) {
        crate::component::scope::resolve_component_scope(
            &comp,
            crate::component::scope::ScopeCommand::Audit,
        )
        .exclude
    } else {
        Vec::new()
    };

    let doc_files = docs_audit::find_doc_files(&docs_path, &doc_excludes);
    if doc_files.is_empty() {
        return findings;
    }

    // Load extension-configured ignore patterns if component is registered
    let ignore_patterns = if let Ok(comp) = component::load(component_id) {
        docs_audit::collect_extension_ignore_patterns(&comp)
    } else {
        Vec::new()
    };

    for relative_doc in &doc_files {
        let abs_doc = docs_path.join(relative_doc);
        let content = match std::fs::read_to_string(&abs_doc) {
            Ok(c) => c,
            Err(_) => continue,
        };

        let finding_file = format!("{}/{}", docs_dir_name, relative_doc);
        let claims = docs_audit::claims::extract_claims(&content, &finding_file, &ignore_patterns);

        for claim in claims {
            // Skip example/placeholder paths — they're illustrative, not real references
            if claim.confidence == ClaimConfidence::Example {
                continue;
            }

            let result = docs_audit::verify::verify_claim(&claim, root, &docs_path, None);

            match result {
                docs_audit::VerifyResult::Broken { suggestion } => {
                    let suggestion_text = suggestion.unwrap_or_default();
                    let (kind, description) = classify_broken_doc_ref(
                        &claim.claim_type,
                        &claim.value,
                        claim.line,
                        &suggestion_text,
                    );

                    findings.push(Finding {
                        convention: "docs".to_string(),
                        severity: match claim.confidence {
                            ClaimConfidence::Real => Severity::Warning,
                            ClaimConfidence::Example | ClaimConfidence::Unclear => Severity::Info,
                        },
                        file: finding_file.clone(),
                        description,
                        suggestion: suggestion_text,
                        kind,
                    });
                }
                docs_audit::VerifyResult::Verified
                | docs_audit::VerifyResult::NeedsVerification { .. } => {}
            }
        }
    }

    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });

    findings
}

/// Fingerprint external reference paths for cross-reference analysis.
///
/// Walks each reference path and fingerprints all source files found.
/// These fingerprints provide the call/import data that dead code detection
/// uses to determine whether a function is referenced externally (e.g. by
/// WordPress core calling a hook callback, or a parent plugin importing a class).
///
/// Reference fingerprints are NOT used for convention discovery, duplication
/// detection, or structural analysis — they only enrich the cross-reference set.
pub(crate) fn fingerprint_reference_paths(reference_paths: &[String]) -> Vec<fingerprint::FileFingerprint> {
    if reference_paths.is_empty() {
        return Vec::new();
    }

    let mut ref_fps = Vec::new();
    let mut total_files = 0;

    for ref_path in reference_paths {
        let root = Path::new(ref_path);
        if !root.is_dir() {
            continue;
        }

        if let Ok(walker_iter) = walker::walk_source_files(root) {
            for path in walker_iter {
                if let Some(fp) = fingerprint::fingerprint_file(&path, root) {
                    ref_fps.push(fp);
                    total_files += 1;
                }
            }
        }
    }

    if total_files > 0 {
        log_status!(
            "audit",
            "Reference dependencies: {} file(s) fingerprinted from {} path(s)",
            total_files,
            reference_paths.len()
        );
    }

    ref_fps
}
