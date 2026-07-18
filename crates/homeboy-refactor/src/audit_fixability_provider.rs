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
