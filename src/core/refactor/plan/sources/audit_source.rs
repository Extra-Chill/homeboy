use crate::core::refactor::auto as fixer;

pub(super) fn filtered_audit_source_result(
    result: &crate::core::code_audit::CodeAuditResult,
    policy: &fixer::FixPolicy,
) -> crate::core::code_audit::CodeAuditResult {
    let mut filtered = result.clone();
    filtered.findings.retain(|finding| {
        let allowed = policy
            .only
            .as_ref()
            .is_none_or(|only| only.contains(&finding.kind));
        let denied = policy.exclude.contains(&finding.kind);
        allowed && !denied
    });
    filtered.summary.outliers_found = filtered.findings.len();
    filtered
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::code_audit::{AuditFinding, AuditSummary, CodeAuditResult, Finding, Severity};

    fn audit_result_with(kinds: Vec<AuditFinding>) -> CodeAuditResult {
        CodeAuditResult {
            component_id: "component".to_string(),
            source_path: "/tmp/component".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: kinds.len(),
                alignment_score: None,
                files_skipped: 0,
                warnings: Vec::new(),
            },
            conventions: Vec::new(),
            directory_conventions: Vec::new(),
            findings: kinds
                .into_iter()
                .map(|kind| Finding {
                    convention: "fixture".to_string(),
                    severity: Severity::Warning,
                    file: "src/lib.rs".to_string(),
                    description: "fixture finding".to_string(),
                    suggestion: "fix it".to_string(),
                    kind,
                })
                .collect(),
            duplicate_groups: Vec::new(),
        }
    }

    #[test]
    fn filtered_audit_source_result_applies_only_and_exclude() {
        let result = audit_result_with(vec![
            AuditFinding::DuplicateFunction,
            AuditFinding::MissingMethod,
            AuditFinding::GodFile,
        ]);
        let policy = fixer::FixPolicy {
            only: Some(vec![AuditFinding::DuplicateFunction, AuditFinding::GodFile]),
            exclude: vec![AuditFinding::GodFile],
        };

        let filtered = filtered_audit_source_result(&result, &policy);

        assert_eq!(filtered.findings.len(), 1);
        assert_eq!(filtered.findings[0].kind, AuditFinding::DuplicateFunction);
        assert_eq!(filtered.summary.outliers_found, 1);
        assert_eq!(result.findings.len(), 3);
    }
}
