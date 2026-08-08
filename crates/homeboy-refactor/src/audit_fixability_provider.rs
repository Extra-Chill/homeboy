//! Refactor-side implementation of the audit fixability provider.
//!
//! The audit engine (`code_audit`) defines `AuditFixabilityProvider` and calls
//! it to compute its fixability summary without depending on refactor behavior.
//! This module implements that trait by running the real fix planner
//! (`plan::generate::generate_audit_fixes*`), applying the dry-run policy
//! annotation (`auto::apply_fix_policy`), and projecting each planned insertion
//! and new file into the slim `(finding, auto_apply)` verdict audit needs. It is
//! registered at binary startup by the CLI, mirroring the extension-manifest /
//! runner-evidence / tunnel provider hooks.

use homeboy_code_audit::fingerprint::FileFingerprint;
use homeboy_code_audit::fixability_provider::{
    register_audit_fixability_provider, AuditFixabilityProvider, FixabilityVerdict,
};
use homeboy_code_audit::CodeAuditResult;

struct RefactorFixabilityProvider;

impl AuditFixabilityProvider for RefactorFixabilityProvider {
    fn plan(
        &self,
        result: &CodeAuditResult,
        source_path: &str,
        fingerprints: &[FileFingerprint],
    ) -> Vec<FixabilityVerdict> {
        let path = std::path::Path::new(source_path);

        // Generate the fix plan (dry-run — never writes). Reuse the audit run's
        // fingerprints when present to avoid re-fingerprinting.
        let fix_policy = crate::auto::FixPolicy::default();
        let mut fix_result = if fingerprints.is_empty() {
            crate::plan::generate::generate_audit_fixes(result, path, &fix_policy)
        } else {
            crate::plan::generate::generate_audit_fixes_with_fingerprints(
                result,
                path,
                &fix_policy,
                fingerprints,
            )
        };

        if fix_result.fixes.is_empty() && fix_result.new_files.is_empty() {
            return Vec::new();
        }

        // Apply policy annotation (dry-run mode: write=false, no filtering) so
        // each insertion/new file carries its automation verdict.
        let policy = crate::auto::FixPolicy {
            only: None,
            exclude: Vec::new(),
        };
        crate::auto::apply_fix_policy(&mut fix_result, false, &policy);

        let mut verdicts = Vec::new();
        for fix in &fix_result.fixes {
            for insertion in &fix.insertions {
                verdicts.push(FixabilityVerdict {
                    finding: insertion.finding.clone(),
                    auto_apply: insertion.auto_apply,
                });
            }
        }
        for new_file in &fix_result.new_files {
            verdicts.push(FixabilityVerdict {
                finding: new_file.finding.clone(),
                auto_apply: new_file.auto_apply,
            });
        }

        verdicts
    }
}

/// Register the refactor-backed audit fixability provider. Called once at binary
/// startup by the CLI.
pub fn register() {
    register_audit_fixability_provider(Box::new(RefactorFixabilityProvider));
}

#[cfg(all(test, feature = "slow-tests"))]
mod tests {
    use super::register;
    use homeboy_code_audit::{
        report::compute_fixability, AuditFinding, AuditSummary, CodeAuditResult, Finding, Severity,
    };
    use std::fs;

    #[test]
    fn computes_fixability_through_the_registered_audit_provider() {
        register();

        let dir = tempfile::tempdir().expect("temp dir");
        let root = dir.path();
        fs::write(
            root.join("todo.rs"),
            "// TODO: add helper\npub fn run() {}\n",
        )
        .expect("write TODO fixture");
        let result = CodeAuditResult {
            component_id: "fixability-test".to_string(),
            source_path: root.to_string_lossy().to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 0,
                outliers_found: 1,
                alignment_score: None,
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings: vec![Finding {
                convention: "comment_hygiene".to_string(),
                severity: Severity::Info,
                file: "todo.rs".to_string(),
                description: "Comment marker 'TODO' found on line 1: TODO: add helper".to_string(),
                suggestion: "Resolve the TODO".to_string(),
                kind: AuditFinding::TodoMarker,
                line: None,
            }],
            duplicate_groups: vec![],
        };
        let fixability = compute_fixability(&result).unwrap_or_else(|| {
            panic!(
                "registered fixability provider should produce a plan; audit findings: {:#?}",
                result.findings
            )
        });

        assert_eq!(fixability.fixable_count, 1);
        assert_eq!(fixability.automated_count, 0);
        assert_eq!(fixability.manual_only_count, 1);
        assert_eq!(fixability.by_kind.len(), 1);
        let todo_marker = fixability
            .by_kind
            .get("todo_marker")
            .expect("fixability summary should include the TODO marker");
        assert_eq!(todo_marker.total, 1);
        assert_eq!(todo_marker.automated, 0);
        assert_eq!(todo_marker.manual_only, 1);
    }
}
