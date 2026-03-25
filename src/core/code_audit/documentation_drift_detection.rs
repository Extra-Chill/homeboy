//! documentation_drift_detection — extracted from mod.rs.

use std::path::Path;
use crate::{component, is_zero, Result};


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

/// Classify a broken reference as stale (moved target) or truly broken.
pub(crate) fn classify_broken_doc_ref(
    claim_type: &docs_audit::ClaimType,
    value: &str,
    line: usize,
    suggestion: &str,
) -> (AuditFinding, String) {
    let s = suggestion.to_lowercase();
    let label = match claim_type {
        docs_audit::ClaimType::FilePath => "file reference",
        docs_audit::ClaimType::DirectoryPath => "directory reference",
        docs_audit::ClaimType::CodeExample => "code example",
        docs_audit::ClaimType::ClassName => "class reference",
    };

    if s.contains("did you mean")
        || s.contains("moved to")
        || s.contains("similar")
        || s.contains("renamed")
    {
        (
            AuditFinding::StaleDocReference,
            format!(
                "Stale {} `{}` (line {}) — target has moved",
                label, value, line
            ),
        )
    } else {
        (
            AuditFinding::BrokenDocReference,
            format!(
                "Broken {} `{}` (line {}) — target does not exist",
                label, value, line
            ),
        )
    }
}
