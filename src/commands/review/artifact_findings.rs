use serde_json::Value;

use homeboy::core::ci_profile::CiRunOutput;
use homeboy::core::code_audit::{homeboy_finding_from_audit, AuditCommandOutput};
use homeboy::core::extension::lint::LintCommandOutput;
use homeboy::core::extension::test::TestCommandOutput;
use homeboy::core::finding::HomeboyFinding;

pub(super) trait ReviewArtifactFindings {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        Vec::new()
    }
}

impl ReviewArtifactFindings for AuditCommandOutput {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        match self {
            AuditCommandOutput::Full { result, .. }
            | AuditCommandOutput::Compared { result, .. } => result
                .findings
                .iter()
                .map(homeboy_finding_from_audit)
                .collect(),
            AuditCommandOutput::Summary(summary) => summary.top_findings.clone(),
            AuditCommandOutput::Conventions { .. } | AuditCommandOutput::BaselineSaved { .. } => {
                Vec::new()
            }
        }
    }
}

impl ReviewArtifactFindings for LintCommandOutput {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        self.findings.clone().unwrap_or_default()
    }
}

impl ReviewArtifactFindings for TestCommandOutput {
    fn review_artifact_findings(&self) -> Vec<HomeboyFinding> {
        self.findings.clone().unwrap_or_default()
    }
}

impl ReviewArtifactFindings for CiRunOutput {}

impl ReviewArtifactFindings for Value {}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy::core::code_audit::baseline::BaselineComparison;
    use homeboy::core::code_audit::report::AuditSummaryOutput;
    use homeboy::core::code_audit::{
        AuditFinding, AuditSummary, CodeAuditResult, Finding, Severity,
    };

    #[test]
    fn audit_full_maps_result_findings_to_review_artifact_findings() {
        let output = AuditCommandOutput::Full {
            passed: false,
            result: audit_result(vec![audit_finding("src/lib.rs", "missing method")]),
            fixability: None,
            extension_phase_timings: Vec::new(),
        };

        let findings = output.review_artifact_findings();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].tool, "audit");
        assert_eq!(findings[0].message, "missing method");
        assert_eq!(findings[0].location.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(findings[0].rule.as_deref(), Some("missing_method"));
    }

    #[test]
    fn audit_compared_maps_result_findings_to_review_artifact_findings() {
        let output = AuditCommandOutput::Compared {
            passed: false,
            result: audit_result(vec![audit_finding("src/main.rs", "naming drift")]),
            baseline_comparison: BaselineComparison {
                new_items: Vec::new(),
                resolved_fingerprints: Vec::new(),
                delta: 0,
                drift_increased: false,
            },
            changed_since: None,
            summary: None,
            fixability: None,
            extension_phase_timings: Vec::new(),
        };

        let findings = output.review_artifact_findings();

        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].message, "naming drift");
        assert_eq!(findings[0].location.file.as_deref(), Some("src/main.rs"));
    }

    #[test]
    fn audit_summary_uses_bounded_top_findings() {
        let top_finding = HomeboyFinding::builder("audit", "top finding")
            .rule("missing_method")
            .file("src/top.rs")
            .build();
        let output = AuditCommandOutput::Summary(AuditSummaryOutput {
            alignment_score: None,
            total_findings: 42,
            warnings: 42,
            info: 0,
            finding_groups: Vec::new(),
            top_findings: vec![top_finding.clone()],
            fixability: None,
            changed_since: None,
            baseline_filtering: None,
            unbaselined_findings: Vec::new(),
            extension_phase_timings: Vec::new(),
            exit_code: 1,
        });

        assert_eq!(output.review_artifact_findings(), vec![top_finding]);
    }

    fn audit_result(findings: Vec<Finding>) -> CodeAuditResult {
        CodeAuditResult {
            component_id: "homeboy".to_string(),
            source_path: "/tmp/homeboy".to_string(),
            summary: AuditSummary {
                files_scanned: 1,
                conventions_detected: 1,
                outliers_found: findings.len(),
                alignment_score: Some(0.5),
                files_skipped: 0,
                warnings: Vec::new(),
            },
            conventions: Vec::new(),
            directory_conventions: Vec::new(),
            findings,
            duplicate_groups: Vec::new(),
        }
    }

    fn audit_finding(file: &str, description: &str) -> Finding {
        Finding {
            convention: "service layout".to_string(),
            severity: Severity::Warning,
            file: file.to_string(),
            description: description.to_string(),
            suggestion: "restore the expected method".to_string(),
            kind: AuditFinding::MissingMethod,
        }
    }
}
