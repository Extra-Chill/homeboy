//! Audit finding value types.
//!
//! `Finding` (a single reported issue: kind, severity, file, description,
//! suggestion), its `Severity` / `FindingConfidence` companions, the
//! confidence policy (`finding_confidence`), and the projection into the shared
//! `homeboy_finding::HomeboyFinding`. These are the audit *output* vocabulary —
//! produced by the audit engine, consumed by refactor/report/CLI — so they live
//! in the shared contract alongside `AuditFinding`.

use std::str::FromStr;

use homeboy_finding::{FindingSource, HomeboyFinding};
use regex::Regex;
use serde::{Deserializer, Serializer};
use serde_json::Value;

use crate::AuditFinding;

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
    /// 1-based line the finding refers to, when the detector knows it.
    ///
    /// Detectors have always known this — a dozen of them format `at line {}`
    /// into `description` — but it lived only as prose. Nothing downstream
    /// could sort by it, jump to it, or render `file.rs:148`, and the audit
    /// fingerprint had to regex the number back out of English to stay stable
    /// across line shifts (#11320).
    pub line: Option<u32>,
}

impl Finding {
    /// Attach the line this finding refers to.
    pub fn at_line(mut self, line: usize) -> Self {
        self.line = Some(line as u32);
        self
    }
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
            line: normalized.location.line.map(|line| line as u32),
        })
    }
}

pub fn homeboy_finding_from_audit(finding: &Finding) -> HomeboyFinding {
    HomeboyFinding::from(finding)
}

impl From<&Finding> for HomeboyFinding {
    fn from(finding: &Finding) -> Self {
        let kind = finding_kind_key(&finding.kind);
        let mut builder = HomeboyFinding::builder("audit", finding.description.clone());
        if let Some(line) = finding.line {
            builder = builder.line(line as i64);
        }
        builder
            .rule(kind.clone())
            .category(finding.convention.clone())
            .file(finding.file.clone())
            .severity(audit_severity_key(&finding.severity))
            .fingerprint(audit_finding_fingerprint(finding))
            .source(FindingSource::new("sidecar").label("audit-findings"))
            .metadata("source_sidecar", "audit-findings")
            .metadata("convention", finding.convention.clone())
            .metadata("suggestion", finding.suggestion.clone())
            .metadata("confidence", finding_confidence(&finding.kind))
            .metadata("kind", kind)
            .build()
    }
}

pub fn finding_kind_key(finding: &AuditFinding) -> String {
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

/// Replace line numbers in a description so a finding keeps one identity as
/// surrounding code shifts.
///
/// This regexes English because, until [`Finding::line`] existed, the line
/// number had nowhere else to live. It stays for the descriptions that still
/// embed one; new detectors should set the field and leave the number out of
/// the prose.
pub fn normalized_finding_description_for_fingerprint(description: &str) -> String {
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

/// Confidence tier for downstream enforcement and autofix policy.
///
/// A free function rather than an inherent method because `AuditFinding` now
/// lives in the `homeboy-audit-contract` crate (the orphan rule forbids an
/// inherent `impl` on a foreign type), while `FindingConfidence` and the audit
/// policy that consumes it are core-side.
pub fn finding_confidence(finding: &AuditFinding) -> FindingConfidence {
    {
        match finding {
            // Direct facts from parser/compiler/filesystem output.
            AuditFinding::MissingImport
            | AuditFinding::CompilerWarning
            | AuditFinding::BrokenDocReference
            | AuditFinding::StaleDocReference
            | AuditFinding::UnwiredNestedRustTest
            | AuditFinding::NonPortableArtifactPath
            | AuditFinding::CommandStatusContractViolation
            | AuditFinding::CommandStatusFixtureMissing => FindingConfidence::Structural,

            // Depends on cross-file reference resolution or declared ownership maps.
            AuditFinding::UnusedParameter
            | AuditFinding::IgnoredParameter
            | AuditFinding::UnreferencedExport
            | AuditFinding::OrphanedInternal
            | AuditFinding::LayerOwnershipViolation
            | AuditFinding::DeprecationAge
            | AuditFinding::DeadGuard
            | AuditFinding::MutatingResourceAccess
            | AuditFinding::LossyPolicyProjection => FindingConfidence::Graph,

            // Convention, naming, body-shape, and similarity findings require judgment.
            _ => FindingConfidence::Heuristic,
        }
    }
}

#[cfg(test)]
mod line_tests {
    use super::*;

    fn finding(line: Option<u32>) -> Finding {
        Finding {
            convention: "core_boundary_leak:core-agnostic-source".to_string(),
            severity: Severity::Warning,
            file: "crates/homeboy-core/src/thing.rs".to_string(),
            description: "configured ecosystem term `node` appears at line 148".to_string(),
            suggestion: "move it".to_string(),
            kind: AuditFinding::CoreBoundaryLeak,
            line,
        }
    }

    #[test]
    fn the_line_reaches_the_normalized_finding_location() {
        // The point of the field: downstream consumers get a structured
        // location instead of having to parse it back out of English.
        let normalized = HomeboyFinding::from(&finding(Some(148)));

        assert_eq!(normalized.location.line, Some(148));
        assert_eq!(
            normalized.location.file.as_deref(),
            Some("crates/homeboy-core/src/thing.rs")
        );
    }

    #[test]
    fn a_finding_without_a_line_leaves_the_location_line_unset() {
        assert!(HomeboyFinding::from(&finding(None)).location.line.is_none());
    }

    #[test]
    fn at_line_attaches_a_line_to_an_existing_finding() {
        assert_eq!(finding(None).at_line(148).line, Some(148));
    }

    #[test]
    fn the_line_survives_a_serialize_deserialize_round_trip() {
        let json = serde_json::to_value(finding(Some(148))).expect("serialize");
        let restored: Finding = serde_json::from_value(json).expect("deserialize");

        assert_eq!(restored.line, Some(148));
    }

    #[test]
    fn the_fingerprint_still_ignores_line_numbers() {
        // Identity must not move when surrounding code shifts, whether the line
        // is carried structurally, in the prose, or both.
        let first = audit_finding_fingerprint(&finding(Some(148)));
        let second = audit_finding_fingerprint(&finding(Some(902)));

        assert_eq!(first, second);
        assert!(first.contains("at line <line>"));
    }

    #[test]
    fn a_line_only_in_the_prose_does_not_reach_the_location() {
        // Documents the remaining gap for detectors that have not adopted the
        // field: the number is visible to a reader and invisible to a consumer.
        let normalized = HomeboyFinding::from(&finding(None));

        assert!(normalized.message.contains("at line 148"));
        assert!(normalized.location.line.is_none());
    }
}
