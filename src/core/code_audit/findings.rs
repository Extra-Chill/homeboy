//! Convert check results into actionable findings for the report.

use super::checks::{CheckResult, CheckStatus};
use super::conventions::AuditFinding;
use crate::core::finding::{FindingSource, HomeboyFinding};
use regex::Regex;
use serde::{Deserializer, Serializer};
use serde_json::Value;
use std::str::FromStr;

/// An actionable finding from the code audit.
#[derive(Debug, Clone)]
pub struct Finding {
    /// The convention this finding relates to.
    pub convention: String,
    /// Severity of the finding.
    pub severity: Severity,
    /// The file with the issue.
    pub file: String,
    /// Human-readable description.
    pub description: String,
    /// Suggested action.
    pub suggestion: String,
    /// The kind of deviation.
    pub kind: AuditFinding,
}

impl serde::Serialize for Finding {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        HomeboyFinding::from(self).serialize(serializer)
    }
}

impl<'de> serde::Deserialize<'de> for Finding {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = Value::deserialize(deserializer)?;

        let normalized: HomeboyFinding =
            serde_json::from_value(value).map_err(serde::de::Error::custom)?;
        let kind = normalized
            .rule
            .as_deref()
            .or_else(|| normalized.metadata.get("kind").and_then(Value::as_str))
            .ok_or_else(|| serde::de::Error::custom("missing audit finding kind"))?;
        let severity = normalized
            .severity
            .as_deref()
            .map(severity_from_key)
            .transpose()
            .map_err(serde::de::Error::custom)?
            .unwrap_or(Severity::Warning);

        Ok(Finding {
            convention: normalized
                .metadata
                .get("convention")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or(normalized.category)
                .unwrap_or_default(),
            severity,
            file: normalized.location.file.unwrap_or_default(),
            description: normalized.message,
            suggestion: normalized
                .metadata
                .get("suggestion")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
            kind: AuditFinding::from_str(kind).map_err(serde::de::Error::custom)?,
        })
    }
}

pub fn homeboy_finding_from_audit(finding: &Finding) -> HomeboyFinding {
    HomeboyFinding::from(finding)
}

impl From<&Finding> for HomeboyFinding {
    fn from(finding: &Finding) -> Self {
        let kind = finding_kind_key(&finding.kind);
        HomeboyFinding::builder("audit", finding.description.clone())
            .rule(kind.clone())
            .category(finding.convention.clone())
            .file(finding.file.clone())
            .severity(audit_severity_key(&finding.severity))
            .fingerprint(audit_finding_fingerprint(finding))
            .source(FindingSource::new("sidecar").label("audit-findings"))
            .metadata("source_sidecar", "audit-findings")
            .metadata("convention", finding.convention.clone())
            .metadata("suggestion", finding.suggestion.clone())
            .metadata("confidence", finding.kind.confidence())
            .metadata("kind", kind)
            .build()
    }
}

pub(crate) fn finding_kind_key(finding: &AuditFinding) -> String {
    serde_json::to_value(finding)
        .ok()
        .and_then(|v| v.as_str().map(String::from))
        .unwrap_or_else(|| format!("{:?}", finding).to_lowercase())
}

fn audit_finding_fingerprint(finding: &Finding) -> String {
    format!(
        "{}:{}:{}:{}",
        finding.file,
        finding_kind_key(&finding.kind),
        finding.convention,
        normalized_finding_description_for_fingerprint(&finding.description)
    )
}

pub(in crate::core::code_audit) fn normalized_finding_description_for_fingerprint(
    description: &str,
) -> String {
    let line_number = Regex::new(r" at line \d+").expect("line-number fingerprint regex compiles");
    line_number
        .replace_all(description, " at line <line>")
        .to_string()
}

fn audit_severity_key(severity: &Severity) -> String {
    serde_json::to_value(severity)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{severity:?}").to_lowercase())
}

fn severity_from_key(value: &str) -> Result<Severity, String> {
    match value {
        "warning" => Ok(Severity::Warning),
        "info" => Ok(Severity::Info),
        other => Err(format!("unknown audit severity: {other}")),
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    /// Convention violation — should be fixed.
    Warning,
    /// Pattern is unclear — needs investigation.
    Info,
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
#[derive(Default)]
pub enum FindingConfidence {
    /// Derived from parser output, compiler output, or explicit file-system facts.
    Structural,
    /// Derived from whole-codebase reference or ownership graph analysis.
    Graph,
    /// Derived from naming, shape, similarity, or convention heuristics.
    #[default]
    Heuristic,
}

impl FindingConfidence {
    /// Only structural findings are eligible for unattended mutation by default.
    pub fn allows_automated_refactor(self) -> bool {
        matches!(self, Self::Structural)
    }
}

impl AuditFinding {
    /// Confidence tier for downstream enforcement and autofix policy.
    pub fn confidence(&self) -> FindingConfidence {
        match self {
            // Direct facts from parser/compiler/filesystem output.
            AuditFinding::MissingImport
            | AuditFinding::CompilerWarning
            | AuditFinding::BrokenDocReference
            | AuditFinding::StaleDocReference
            | AuditFinding::UnwiredNestedRustTest
            | AuditFinding::NonPortableArtifactPath
            | AuditFinding::CommandStatusContractViolation => FindingConfidence::Structural,

            // Depends on cross-file reference resolution or declared ownership maps.
            AuditFinding::UnusedParameter
            | AuditFinding::IgnoredParameter
            | AuditFinding::UnreferencedExport
            | AuditFinding::OrphanedInternal
            | AuditFinding::LayerOwnershipViolation
            | AuditFinding::DeprecationAge
            | AuditFinding::DeadGuard
            | AuditFinding::MutatingResourceAccess => FindingConfidence::Graph,

            // Convention, naming, body-shape, and similarity findings require judgment.
            _ => FindingConfidence::Heuristic,
        }
    }
}

/// Build findings from check results.
///
/// Fragmented conventions (< 50% confidence) are suppressed — the convention
/// metadata still appears in the report, but individual findings are noise
/// when the pattern itself is uncertain.
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
    use crate::core::code_audit::checks::CheckResult;
    use crate::core::code_audit::conventions::{Deviation, Outlier};

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
            AuditFinding::CompilerWarning.confidence(),
            FindingConfidence::Structural
        );
        assert_eq!(
            AuditFinding::UnreferencedExport.confidence(),
            FindingConfidence::Graph
        );
        assert_eq!(
            AuditFinding::OrphanedTest.confidence(),
            FindingConfidence::Heuristic
        );
        assert_eq!(
            AuditFinding::RedirectValidation.confidence(),
            FindingConfidence::Heuristic
        );
        assert!(AuditFinding::CompilerWarning
            .confidence()
            .allows_automated_refactor());
        assert!(!AuditFinding::OrphanedTest
            .confidence()
            .allows_automated_refactor());
    }

    #[test]
    fn test_confidence() {
        assert_eq!(
            AuditFinding::CompilerWarning.confidence(),
            FindingConfidence::Structural
        );
        assert_eq!(
            AuditFinding::OrphanedTest.confidence(),
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
