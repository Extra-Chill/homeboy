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
