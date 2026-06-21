use std::fmt::Write as _;

use homeboy::core::code_audit::{AuditCommandOutput, AuditFinding, Finding, Severity};
use homeboy::core::finding::HomeboyFinding;

use super::TOP_N;

/// Render the audit stage body as actionable finding snippets. Empty bodies
/// mean no findings; we say nothing.
pub(super) fn render_audit_body(out: &mut String, output: &AuditCommandOutput) {
    let findings = audit_findings(output);
    if findings.is_empty() {
        return;
    }

    for finding in findings.iter().take(TOP_N) {
        render_audit_finding(out, finding);
    }
    if findings.len() > TOP_N {
        let _ = writeln!(
            out,
            "- _… {} more audit finding(s)_",
            findings.len() - TOP_N
        );
    }
    let _ = writeln!(out, "- _Total: {} finding(s)_", findings.len());
}

fn render_audit_finding(out: &mut String, finding: &AuditFindingLine) {
    let _ = write!(
        out,
        "- `{}` — **{}**: {}",
        finding.file, finding.kind_label, finding.description
    );
    if !finding.suggestion.is_empty() {
        let _ = write!(out, "; {}", finding.suggestion);
    }
    if finding.severity.as_deref() == Some("info") {
        let _ = write!(out, " _(info)_");
    }
    out.push('\n');
}

#[derive(Clone)]
struct AuditFindingLine {
    file: String,
    kind_label: String,
    severity: Option<String>,
    description: String,
    suggestion: String,
}

impl From<&Finding> for AuditFindingLine {
    fn from(finding: &Finding) -> Self {
        Self {
            file: finding.file.clone(),
            kind_label: audit_kind_label(&finding.kind),
            severity: Some(audit_severity_label(&finding.severity)),
            description: finding.description.clone(),
            suggestion: finding.suggestion.clone(),
        }
    }
}

impl From<&HomeboyFinding> for AuditFindingLine {
    fn from(finding: &HomeboyFinding) -> Self {
        Self {
            file: finding.location.file.clone().unwrap_or_default(),
            kind_label: finding
                .rule
                .as_deref()
                .or_else(|| {
                    finding
                        .metadata
                        .get("kind")
                        .and_then(serde_json::Value::as_str)
                })
                .unwrap_or("audit")
                .to_string(),
            severity: finding.severity.clone(),
            description: finding.message.clone(),
            suggestion: finding
                .metadata
                .get("suggestion")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string(),
        }
    }
}

/// Pull actionable audit finding details. PR comments should answer "where and
/// why?" without forcing the reviewer to run the deep-dive command first.
fn audit_findings(output: &AuditCommandOutput) -> Vec<AuditFindingLine> {
    match output {
        AuditCommandOutput::Full { result, .. } => {
            result.findings.iter().map(AuditFindingLine::from).collect()
        }
        AuditCommandOutput::Compared { result, .. } => {
            result.findings.iter().map(AuditFindingLine::from).collect()
        }
        AuditCommandOutput::Summary(summary) => summary
            .top_findings
            .iter()
            .map(AuditFindingLine::from)
            .collect(),
        AuditCommandOutput::BaselineSaved { .. } => Vec::new(),
        AuditCommandOutput::Conventions { .. } => Vec::new(),
    }
}

fn audit_kind_label(kind: &AuditFinding) -> String {
    serde_json::to_value(kind)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{:?}", kind).to_lowercase())
}

fn audit_severity_label(severity: &Severity) -> String {
    serde_json::to_value(severity)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| format!("{severity:?}").to_lowercase())
}
