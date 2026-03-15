use super::{CodeAuditResult, Finding, Severity};

#[derive(Debug, Clone, Copy)]
pub struct AuditConvergenceScoring {
    pub warning_weight: usize,
    pub info_weight: usize,
}

impl Default for AuditConvergenceScoring {
    fn default() -> Self {
        Self {
            warning_weight: 3,
            info_weight: 1,
        }
    }
}

impl AuditConvergenceScoring {
    fn severity_weight(&self, severity: &Severity) -> usize {
        match severity {
            Severity::Warning => self.warning_weight,
            Severity::Info => self.info_weight,
        }
    }

    fn weighted_finding_score(&self, result: &CodeAuditResult) -> usize {
        result
            .findings
            .iter()
            .map(|finding| self.severity_weight(&finding.severity))
            .sum()
    }
}

pub fn weighted_finding_score_with(
    result: &CodeAuditResult,
    scoring: AuditConvergenceScoring,
) -> usize {
    scoring.weighted_finding_score(result)
}

pub fn score_delta(
    before: &CodeAuditResult,
    after: &CodeAuditResult,
    scoring: AuditConvergenceScoring,
) -> isize {
    weighted_finding_score_with(before, scoring) as isize
        - weighted_finding_score_with(after, scoring) as isize
}

pub fn finding_fingerprint(finding: &Finding) -> String {
    format!(
        "{}::{:?}::{}::{}",
        finding.file, finding.kind, finding.convention, finding.description
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::{AuditFinding, AuditSummary};

    fn mk_result_with_findings(findings: Vec<Finding>) -> CodeAuditResult {
        CodeAuditResult {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: findings.len(),
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: vec![],
            },
            conventions: vec![],
            directory_conventions: vec![],
            findings,
            duplicate_groups: vec![],
        }
    }

    #[test]
    fn finding_fingerprint_same_for_identical() {
        let f1 = Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Existing finding".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::NamingMismatch,
        };
        let f2 = f1.clone();
        assert_eq!(finding_fingerprint(&f1), finding_fingerprint(&f2));
    }

    #[test]
    fn finding_fingerprint_different_for_distinct() {
        let f1 = Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Existing finding".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::NamingMismatch,
        };
        let f2 = Finding {
            convention: "duplication".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Duplicate function `foo`".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::DuplicateFunction,
        };
        assert_ne!(finding_fingerprint(&f1), finding_fingerprint(&f2));
    }

    #[test]
    fn finding_fingerprint_filters_new_findings() {
        let baseline = Finding {
            convention: "naming".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Existing finding".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::NamingMismatch,
        };
        let new = Finding {
            convention: "duplication".to_string(),
            severity: Severity::Warning,
            file: "src/target.rs".to_string(),
            description: "Duplicate function `foo`".to_string(),
            suggestion: String::new(),
            kind: AuditFinding::DuplicateFunction,
        };

        let baseline_set: std::collections::HashSet<String> =
            vec![finding_fingerprint(&baseline)].into_iter().collect();
        let post = [&baseline, &new];
        let new_findings: Vec<_> = post
            .iter()
            .filter(|f| !baseline_set.contains(&finding_fingerprint(f)))
            .collect();

        assert_eq!(new_findings.len(), 1);
        assert_eq!(new_findings[0].kind, AuditFinding::DuplicateFunction);
    }

    #[test]
    fn score_delta_zero_means_no_progress() {
        let result = mk_result_with_findings(vec![Finding {
            convention: "Test".to_string(),
            severity: Severity::Warning,
            file: "src/a.rs".to_string(),
            description: "Warning finding".to_string(),
            suggestion: "Fix it".to_string(),
            kind: AuditFinding::MissingMethod,
        }]);
        assert_eq!(
            score_delta(&result, &result, AuditConvergenceScoring::default()),
            0
        );
    }

    #[test]
    fn score_delta_uses_configured_weights() {
        let before = mk_result_with_findings(vec![
            Finding {
                convention: "Test".to_string(),
                severity: Severity::Warning,
                file: "src/a.rs".to_string(),
                description: "Warning finding".to_string(),
                suggestion: "Fix it".to_string(),
                kind: AuditFinding::MissingMethod,
            },
            Finding {
                convention: "Test".to_string(),
                severity: Severity::Info,
                file: "src/b.rs".to_string(),
                description: "Info finding".to_string(),
                suggestion: "Investigate".to_string(),
                kind: AuditFinding::MissingImport,
            },
        ]);
        let after = mk_result_with_findings(vec![Finding {
            convention: "Test".to_string(),
            severity: Severity::Info,
            file: "src/b.rs".to_string(),
            description: "Info finding".to_string(),
            suggestion: "Investigate".to_string(),
            kind: AuditFinding::MissingImport,
        }]);

        assert_eq!(
            score_delta(
                &before,
                &after,
                AuditConvergenceScoring {
                    warning_weight: 5,
                    info_weight: 1,
                }
            ),
            5
        );
    }
}
