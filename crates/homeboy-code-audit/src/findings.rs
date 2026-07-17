//! Convert check results into actionable findings for the report.

use super::checks::{CheckResult, CheckStatus};
// The finding value types + helpers moved to homeboy-audit-contract (#8425).
// Re-export them so existing `crate::findings::*` paths keep resolving.
pub use homeboy_audit_contract::finding::{
    finding_confidence, finding_kind_key, homeboy_finding_from_audit,
    normalized_finding_description_for_fingerprint, Finding, FindingConfidence, Severity,
};
pub use homeboy_audit_contract::AuditFinding;

/// Build findings from check results.
///
/// Fragmented conventions are suppressed — the convention metadata still appears
/// in the report, but individual findings are noise when the pattern itself is
/// uncertain.
pub fn build_findings(results: &[CheckResult]) -> Vec<Finding> {
    let mut findings = Vec::new();

    for result in results {
        let severity = match result.status {
            CheckStatus::Clean => continue,
            CheckStatus::Drift => Severity::Warning,
            // Suppress individual findings from fragmented conventions.
            // The convention is still reported; just the per-file findings are omitted.
            CheckStatus::Fragmented => continue,
        };

        for outlier in &result.outliers {
            for deviation in &outlier.deviations {
                let severity =
                    if outlier.noisy || matches!(deviation.kind, AuditFinding::NamingMismatch) {
                        Severity::Info
                    } else {
                        severity.clone()
                    };

                findings.push(Finding {
                    convention: result.convention_name.clone(),
                    severity,
                    file: outlier.file.clone(),
                    description: deviation.description.clone(),
                    suggestion: deviation.suggestion.clone(),
                    kind: deviation.kind.clone(),
                });
            }
        }
    }

    findings
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::checks::CheckResult;
    use crate::conventions::{Deviation, Outlier};

    #[test]
    fn clean_result_produces_no_findings() {
        let results = vec![CheckResult {
            convention_name: "Test".to_string(),
            status: CheckStatus::Clean,
            conforming_count: 3,
            total_count: 3,
            outliers: vec![],
        }];

        let findings = build_findings(&results);
        assert!(findings.is_empty());
    }

    #[test]
    fn drift_produces_warning_findings() {
        let results = vec![CheckResult {
            convention_name: "Step Types".to_string(),
            status: CheckStatus::Drift,
            conforming_count: 2,
            total_count: 3,
            outliers: vec![Outlier {
                file: "agent-ping.php".to_string(),
                noisy: false,
                deviations: vec![Deviation {
                    kind: AuditFinding::MissingMethod,
                    description: "Missing method: validate".to_string(),
                    suggestion: "Add validate()".to_string(),
                }],
            }],
        }];

        let findings = build_findings(&results);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Warning);
        assert_eq!(findings[0].convention, "Step Types");
        assert_eq!(findings[0].file, "agent-ping.php");
    }

    #[test]
    fn test_build_findings() {
        let results = vec![CheckResult {
            convention_name: "Step Types".to_string(),
            status: CheckStatus::Drift,
            conforming_count: 2,
            total_count: 3,
            outliers: vec![Outlier {
                file: "agent-ping.php".to_string(),
                noisy: false,
                deviations: vec![Deviation {
                    kind: AuditFinding::MissingMethod,
                    description: "Missing method: validate".to_string(),
                    suggestion: "Add validate()".to_string(),
                }],
            }],
        }];

        assert_eq!(build_findings(&results).len(), 1);
    }

    #[test]
    fn fragmented_produces_no_findings() {
        // Fragmented conventions (< 50% confidence) are suppressed.
        // The convention metadata still appears in the report, but individual
        // findings are noise when the pattern itself is uncertain.
        let results = vec![CheckResult {
            convention_name: "Misc".to_string(),
            status: CheckStatus::Fragmented,
            conforming_count: 1,
            total_count: 3,
            outliers: vec![
                Outlier {
                    file: "a.php".to_string(),
                    noisy: false,
                    deviations: vec![Deviation {
                        kind: AuditFinding::MissingMethod,
                        description: "Missing".to_string(),
                        suggestion: "Fix".to_string(),
                    }],
                },
                Outlier {
                    file: "b.php".to_string(),
                    noisy: false,
                    deviations: vec![Deviation {
                        kind: AuditFinding::MissingMethod,
                        description: "Missing".to_string(),
                        suggestion: "Fix".to_string(),
                    }],
                },
            ],
        }];

        let findings = build_findings(&results);
        assert!(
            findings.is_empty(),
            "Fragmented conventions should not produce findings"
        );
    }

    #[test]
    fn naming_mismatch_is_downgraded_to_info() {
        let results = vec![CheckResult {
            convention_name: "Abilities".to_string(),
            status: CheckStatus::Drift,
            conforming_count: 2,
            total_count: 3,
            outliers: vec![Outlier {
                file: "abilities/helpers.php".to_string(),
                noisy: true,
                deviations: vec![Deviation {
                    kind: AuditFinding::NamingMismatch,
                    description:
                        "Helper-like name does not match convention suffix 'Ability': Helpers"
                            .to_string(),
                    suggestion: "Treat this as a utility/helper or rename it".to_string(),
                }],
            }],
        }];

        let findings = build_findings(&results);
        assert_eq!(findings.len(), 1);
        assert_eq!(findings[0].severity, Severity::Info);
        assert_eq!(findings[0].kind, AuditFinding::NamingMismatch);
    }

    #[test]
    fn finding_serializes_confidence_from_kind() {
        let finding = Finding {
            convention: "compiler".to_string(),
            severity: Severity::Warning,
            file: "src/lib.rs".to_string(),
            description: "unused import".to_string(),
            suggestion: "remove it".to_string(),
            kind: AuditFinding::CompilerWarning,
        };

        let json = serde_json::to_value(&finding).expect("serialize finding");

        assert_eq!(json["metadata"]["confidence"], "structural");
        assert_eq!(json["rule"], "compiler_warning");
        assert_eq!(json["message"], "unused import");
        assert!(
            json.get("raw").is_none(),
            "canonical audit findings should not serialize a typed raw payload"
        );
    }

    #[test]
    fn finding_deserializes_canonical_homeboy_finding() {
        let value = serde_json::json!({
            "tool": "audit",
            "rule": "compiler_warning",
            "category": "compiler",
            "severity": "warning",
            "file": "src/lib.rs",
            "message": "unused import",
            "metadata": {
                "suggestion": "remove it"
            }
        });

        let finding: Finding = serde_json::from_value(value).expect("deserialize finding");

        assert_eq!(finding.kind, AuditFinding::CompilerWarning);
        assert_eq!(finding.convention, "compiler");
        assert_eq!(finding.severity, Severity::Warning);
        assert_eq!(finding.file, "src/lib.rs");
        assert_eq!(finding.description, "unused import");
        assert_eq!(finding.suggestion, "remove it");
    }

    #[test]
    fn finding_confidence_tiers_classify_known_risk_levels() {
        assert_eq!(
            finding_confidence(&AuditFinding::CompilerWarning),
            FindingConfidence::Structural
        );
        assert_eq!(
            finding_confidence(&AuditFinding::UnreferencedExport),
            FindingConfidence::Graph
        );
        assert_eq!(
            finding_confidence(&AuditFinding::OrphanedTest),
            FindingConfidence::Heuristic
        );
        assert_eq!(
            finding_confidence(&AuditFinding::RedirectValidation),
            FindingConfidence::Heuristic
        );
        assert!(finding_confidence(&AuditFinding::CompilerWarning).allows_automated_refactor());
        assert!(!finding_confidence(&AuditFinding::OrphanedTest).allows_automated_refactor());
    }

    #[test]
    fn test_confidence() {
        assert_eq!(
            finding_confidence(&AuditFinding::CompilerWarning),
            FindingConfidence::Structural
        );
        assert_eq!(
            finding_confidence(&AuditFinding::OrphanedTest),
            FindingConfidence::Heuristic
        );
    }

    #[test]
    fn test_allows_automated_refactor() {
        assert!(FindingConfidence::Structural.allows_automated_refactor());
        assert!(!FindingConfidence::Graph.allows_automated_refactor());
        assert!(!FindingConfidence::Heuristic.allows_automated_refactor());
    }
}
