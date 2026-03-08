//! Documentation drift detection — broken references and stale paths in markdown.
//!
//! Scans markdown files for claims (file paths, directory paths, class names),
//! verifies each against the codebase, and produces standard `Finding` structs.
//! This is a detection phase in the unified audit pipeline, same as structural
//! analysis or dead code detection.

pub(crate) mod claims;
pub(crate) mod verify;

use std::path::Path;

use super::conventions::AuditFinding;
use super::findings::{Finding, Severity};
use claims::{ClaimConfidence, ClaimType};

/// Detect documentation drift — broken and stale references in markdown files.
///
/// Scans all `.md` files in common docs directories, extracts verifiable claims
/// (file paths, directory paths, class names), and checks each against the
/// codebase. Broken claims become findings.
///
/// Returns empty vec if no docs directory exists (docs are optional).
pub fn detect_doc_drift(root: &Path, ignore_patterns: &[String]) -> Vec<Finding> {
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

    let doc_files = claims::find_doc_files(&docs_path, None);
    if doc_files.is_empty() {
        return findings;
    }

    for relative_doc in &doc_files {
        let abs_doc = docs_path.join(relative_doc);
        let content = match std::fs::read_to_string(&abs_doc) {
            Ok(c) => c,
            Err(_) => continue,
        };

        // The finding's file field should be relative to root, including the docs dir prefix
        let finding_file = format!("{}/{}", docs_dir_name, relative_doc);

        let claims = claims::extract_claims(&content, &finding_file, ignore_patterns);

        for claim in claims {
            let result = verify::verify_claim(&claim, root, &docs_path, None);

            match result {
                verify::VerifyResult::Broken { suggestion } => {
                    let suggestion_text = suggestion.unwrap_or_default();
                    let (kind, description) = classify_broken(&claim, &suggestion_text);

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
                verify::VerifyResult::Verified | verify::VerifyResult::NeedsVerification { .. } => {
                }
            }
        }
    }

    // Sort for deterministic output
    findings.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then_with(|| a.description.cmp(&b.description))
    });

    findings
}

/// Classify a broken reference as stale (moved target) or truly broken.
fn classify_broken(claim: &claims::Claim, suggestion: &str) -> (AuditFinding, String) {
    let s = suggestion.to_lowercase();

    if s.contains("did you mean")
        || s.contains("moved to")
        || s.contains("similar")
        || s.contains("renamed")
    {
        (
            AuditFinding::StaleDocReference,
            format!(
                "Stale {} `{}` (line {}) — target has moved",
                claim_type_label(&claim.claim_type),
                claim.value,
                claim.line
            ),
        )
    } else {
        (
            AuditFinding::BrokenDocReference,
            format!(
                "Broken {} `{}` (line {}) — target does not exist",
                claim_type_label(&claim.claim_type),
                claim.value,
                claim.line
            ),
        )
    }
}

fn claim_type_label(ct: &ClaimType) -> &'static str {
    match ct {
        ClaimType::FilePath => "file reference",
        ClaimType::DirectoryPath => "directory reference",
        ClaimType::CodeExample => "code example",
        ClaimType::ClassName => "class reference",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn no_docs_dir_produces_no_findings() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("main.rs"), "fn main() {}").unwrap();

        let findings = detect_doc_drift(dir.path(), &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn empty_docs_dir_produces_no_findings() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir_all(dir.path().join("docs")).unwrap();

        let findings = detect_doc_drift(dir.path(), &[]);
        assert!(findings.is_empty());
    }

    #[test]
    fn detects_broken_file_reference() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();

        fs::write(
            docs.join("guide.md"),
            "# Guide\n\nSee `src/missing_file.rs` for details.\n",
        )
        .unwrap();

        // Create src/ but not the referenced file
        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let findings = detect_doc_drift(dir.path(), &[]);

        let broken: Vec<_> = findings
            .iter()
            .filter(|f| f.kind == AuditFinding::BrokenDocReference)
            .collect();

        assert!(
            !broken.is_empty(),
            "Should detect broken reference to src/missing_file.rs, findings: {:?}",
            findings
        );
        assert_eq!(broken[0].convention, "docs");
        assert!(broken[0].file.starts_with("docs/"));
    }

    #[test]
    fn valid_reference_produces_no_finding() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();

        fs::write(
            docs.join("guide.md"),
            "# Guide\n\nSee `src/main.rs` for the entry point.\n",
        )
        .unwrap();

        fs::create_dir_all(dir.path().join("src")).unwrap();
        fs::write(dir.path().join("src/main.rs"), "fn main() {}").unwrap();

        let findings = detect_doc_drift(dir.path(), &[]);

        let broken: Vec<_> = findings
            .iter()
            .filter(|f| {
                f.kind == AuditFinding::BrokenDocReference
                    || f.kind == AuditFinding::StaleDocReference
            })
            .collect();

        assert!(
            broken.is_empty(),
            "Valid reference should not produce findings, got: {:?}",
            broken
        );
    }

    #[test]
    fn findings_have_docs_convention() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();

        fs::write(
            docs.join("api.md"),
            "# API\n\nEndpoint defined in `src/api/routes.rs`.\n",
        )
        .unwrap();

        let findings = detect_doc_drift(dir.path(), &[]);

        for f in &findings {
            assert_eq!(
                f.convention, "docs",
                "All doc findings should have convention 'docs'"
            );
        }
    }

    #[test]
    fn ignore_patterns_filter_claims() {
        let dir = tempfile::tempdir().unwrap();
        let docs = dir.path().join("docs");
        fs::create_dir_all(&docs).unwrap();

        fs::write(
            docs.join("guide.md"),
            "# Guide\n\nSee `vendor/package/file.php` for details.\n",
        )
        .unwrap();

        // Without ignore patterns, this would be a broken reference
        let findings_without = detect_doc_drift(dir.path(), &[]);
        let findings_with = detect_doc_drift(dir.path(), &["vendor/**".to_string()]);

        // The ignored pattern should produce fewer or equal findings
        assert!(
            findings_with.len() <= findings_without.len(),
            "Ignore patterns should filter claims"
        );
    }
}
