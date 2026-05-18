use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

use crate::core::code_audit::{self, report::finding_kind_key};
use crate::core::extension::lint::LintFinding;

mod run_builder;
mod run_status;
mod trace_run_builder;
mod trace_span_builder;
mod triage_items;

pub use run_builder::NewRunRecordBuilder;
pub use run_status::RunStatus;
pub use trace_run_builder::NewTraceRunRecordBuilder;
pub use trace_span_builder::NewTraceSpanRecordBuilder;
pub use triage_items::{NewTriageItemRecord, TriageItemRecord, TriagePullRequestSignals};

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NewRunRecord {
    pub kind: String,
    pub component_id: Option<String>,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub homeboy_version: Option<String>,
    pub git_sha: Option<String>,
    pub rig_id: Option<String>,
    pub metadata_json: serde_json::Value,
}

impl NewRunRecord {
    pub fn builder(kind: impl Into<String>) -> NewRunRecordBuilder {
        NewRunRecordBuilder::new(kind)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RunRecord {
    pub id: String,
    pub kind: String,
    pub component_id: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub status: String,
    pub command: Option<String>,
    pub cwd: Option<String>,
    pub homeboy_version: Option<String>,
    pub git_sha: Option<String>,
    pub rig_id: Option<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunListFilter {
    pub kind: Option<String>,
    pub component_id: Option<String>,
    pub status: Option<String>,
    pub rig_id: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub id: String,
    pub run_id: String,
    pub kind: String,
    #[serde(rename = "type", default = "default_artifact_type")]
    pub artifact_type: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<i64>,
    pub mime: Option<String>,
    pub created_at: String,
}

fn default_artifact_type() -> String {
    "file".to_string()
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NewFindingRecord {
    pub run_id: String,
    pub tool: String,
    pub rule: Option<String>,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub severity: Option<String>,
    pub fingerprint: Option<String>,
    pub message: String,
    pub fixable: Option<bool>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FindingRecord {
    pub id: String,
    pub run_id: String,
    pub tool: String,
    pub rule: Option<String>,
    pub file: Option<String>,
    pub line: Option<i64>,
    pub severity: Option<String>,
    pub fingerprint: Option<String>,
    pub message: String,
    pub fixable: Option<bool>,
    pub metadata_json: serde_json::Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FindingListFilter {
    pub run_id: Option<String>,
    pub tool: Option<String>,
    pub file: Option<String>,
    pub fingerprint: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AnnotationFindingRecord {
    pub file: Option<String>,
    pub line: Option<i64>,
    pub message: String,
    pub source: Option<String>,
    pub severity: Option<String>,
    pub code: Option<String>,
    pub fixable: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extra: BTreeMap<String, Value>,
}

pub fn finding_record_from_lint(run_id: &str, finding: &LintFinding) -> NewFindingRecord {
    NewFindingRecord {
        run_id: run_id.to_string(),
        tool: finding.tool.clone().unwrap_or_else(|| "lint".to_string()),
        rule: lint_extra_string(finding, "rule").or_else(|| Some(finding.category.clone())),
        file: finding.file.clone(),
        line: lint_extra_i64(finding, "line"),
        severity: finding.severity.clone(),
        fingerprint: Some(finding.id.clone()),
        message: finding.message.clone(),
        fixable: lint_extra_bool(finding, "fixable"),
        metadata_json: serde_json::json!({
            "category": finding.category,
            "source_sidecar": "lint-findings",
            "raw": finding,
        }),
    }
}

pub fn finding_records_from_lint(run_id: &str, findings: &[LintFinding]) -> Vec<NewFindingRecord> {
    findings
        .iter()
        .map(|finding| finding_record_from_lint(run_id, finding))
        .collect()
}

pub fn finding_record_from_audit(run_id: &str, finding: &code_audit::Finding) -> NewFindingRecord {
    let kind = finding_kind_key(&finding.kind);
    NewFindingRecord {
        run_id: run_id.to_string(),
        tool: "audit".to_string(),
        rule: Some(kind.clone()),
        file: Some(finding.file.clone()),
        line: None,
        severity: Some(audit_severity_key(&finding.severity)),
        fingerprint: Some(audit_finding_fingerprint(finding)),
        message: finding.description.clone(),
        fixable: None,
        metadata_json: serde_json::json!({
            "source_sidecar": "audit-findings",
            "convention": finding.convention,
            "suggestion": finding.suggestion,
            "confidence": finding.kind.confidence(),
            "kind": kind,
            "raw": finding,
        }),
    }
}

pub fn finding_records_from_audit(
    run_id: &str,
    findings: &[code_audit::Finding],
) -> Vec<NewFindingRecord> {
    findings
        .iter()
        .map(|finding| finding_record_from_audit(run_id, finding))
        .collect()
}

fn audit_finding_fingerprint(finding: &code_audit::Finding) -> String {
    format!(
        "{}:{}:{}:{}",
        finding.file,
        finding_kind_key(&finding.kind),
        finding.convention,
        finding.description
    )
}

fn audit_severity_key(severity: &code_audit::Severity) -> String {
    serde_json::to_value(severity)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{severity:?}").to_lowercase())
}

pub fn finding_records_from_annotations_dir(
    run_id: &str,
    annotations_dir: &Path,
) -> crate::core::error::Result<Vec<NewFindingRecord>> {
    if !annotations_dir.exists() {
        return Ok(Vec::new());
    }

    let mut entries = std::fs::read_dir(annotations_dir)
        .map_err(|e| annotation_dir_error("read", annotations_dir, e))?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|e| annotation_dir_error("list", annotations_dir, e))?;
    entries.sort_by_key(|entry| entry.path());

    let mut records = Vec::new();
    for entry in entries {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        records.extend(finding_records_from_annotation_file(run_id, &path)?);
    }
    Ok(records)
}

fn annotation_dir_error(action: &str, path: &Path, error: std::io::Error) -> crate::core::Error {
    crate::core::Error::internal_io(
        format!(
            "Failed to {} annotations dir {}: {}",
            action,
            path.display(),
            error
        ),
        Some("observation.findings.annotations".to_string()),
    )
}

pub fn finding_records_from_annotation_file(
    run_id: &str,
    path: &Path,
) -> crate::core::error::Result<Vec<NewFindingRecord>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = std::fs::read_to_string(path).map_err(|e| {
        crate::core::Error::internal_io(
            format!("Failed to read annotations file {}: {}", path.display(), e),
            Some("observation.findings.annotations".to_string()),
        )
    })?;
    if content.trim().is_empty() {
        return Ok(Vec::new());
    }

    let annotations: Vec<AnnotationFindingRecord> =
        serde_json::from_str(&content).map_err(|e| {
            crate::core::Error::internal_io(
                format!("Malformed annotations JSON in {}: {}", path.display(), e),
                Some("observation.findings.annotations".to_string()),
            )
        })?;
    let source_file = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("annotations.json");

    Ok(annotations
        .iter()
        .map(|annotation| finding_record_from_annotation(run_id, annotation, source_file))
        .collect())
}

pub fn finding_record_from_annotation(
    run_id: &str,
    annotation: &AnnotationFindingRecord,
    source_file: &str,
) -> NewFindingRecord {
    let tool = annotation
        .source
        .clone()
        .unwrap_or_else(|| annotation_file_stem(source_file));
    NewFindingRecord {
        run_id: run_id.to_string(),
        tool,
        rule: annotation.code.clone(),
        file: annotation.file.clone(),
        line: annotation.line,
        severity: annotation.severity.clone(),
        fingerprint: annotation_fingerprint(annotation),
        message: annotation.message.clone(),
        fixable: annotation.fixable,
        metadata_json: serde_json::json!({
            "source_sidecar": "annotations",
            "annotation_file": source_file,
            "raw": annotation,
        }),
    }
}

fn annotation_file_stem(source_file: &str) -> String {
    Path::new(source_file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("annotations")
        .to_string()
}

fn annotation_fingerprint(annotation: &AnnotationFindingRecord) -> Option<String> {
    if let Some(id) = annotation.extra.get("id").and_then(Value::as_str) {
        return Some(id.to_string());
    }
    Some(format!(
        "{}:{}:{}:{}:{}",
        annotation.file.as_deref().unwrap_or_default(),
        annotation.line.unwrap_or_default(),
        annotation.source.as_deref().unwrap_or_default(),
        annotation.code.as_deref().unwrap_or_default(),
        annotation.message
    ))
}

fn lint_extra_string(finding: &LintFinding, key: &str) -> Option<String> {
    finding.extra.get(key)?.as_str().map(str::to_string)
}

fn lint_extra_i64(finding: &LintFinding, key: &str) -> Option<i64> {
    finding.extra.get(key)?.as_i64()
}

fn lint_extra_bool(finding: &LintFinding, key: &str) -> Option<bool> {
    match finding.extra.get(key)? {
        Value::Bool(value) => Some(*value),
        Value::String(value) => value.parse().ok(),
        _ => None,
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct NewTraceRunRecord {
    pub run_id: String,
    pub component_id: String,
    pub rig_id: Option<String>,
    pub scenario_id: String,
    pub status: String,
    pub baseline_status: Option<String>,
    pub metadata_json: serde_json::Value,
}

impl NewTraceRunRecord {
    pub fn builder(
        run_id: impl Into<String>,
        component_id: impl Into<String>,
        scenario_id: impl Into<String>,
        status: impl Into<String>,
    ) -> NewTraceRunRecordBuilder {
        NewTraceRunRecordBuilder::new(run_id, component_id, scenario_id, status)
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct TraceRunRecord {
    pub run_id: String,
    pub component_id: String,
    pub rig_id: Option<String>,
    pub scenario_id: String,
    pub status: String,
    pub baseline_status: Option<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct NewTraceSpanRecord {
    pub run_id: String,
    pub span_id: String,
    pub status: String,
    pub duration_ms: Option<f64>,
    pub from_event: Option<String>,
    pub to_event: Option<String>,
    pub metadata_json: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceSpanRecord {
    pub id: String,
    pub run_id: String,
    pub span_id: String,
    pub status: String,
    pub duration_ms: Option<f64>,
    pub from_event: Option<String>,
    pub to_event: Option<String>,
    pub metadata_json: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_builder() {
        let record = NewRunRecord::builder("lint").build();

        assert_eq!(record.kind, "lint");
        assert_eq!(record.metadata_json, serde_json::json!({}));
    }

    #[test]
    fn test_finding_record_from_lint() {
        let finding = LintFinding {
            id: "src/lib.rs:10:lint/security".to_string(),
            message: "escape output".to_string(),
            category: "security".to_string(),
            tool: Some("phpcs".to_string()),
            file: Some("src/lib.rs".to_string()),
            severity: Some("error".to_string()),
            extra: BTreeMap::from([
                ("line".to_string(), serde_json::json!(10)),
                ("rule".to_string(), serde_json::json!("WordPress.Security")),
                ("fixable".to_string(), serde_json::json!(true)),
            ]),
        };

        let record = finding_record_from_lint("run-1", &finding);

        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.tool, "phpcs");
        assert_eq!(record.rule.as_deref(), Some("WordPress.Security"));
        assert_eq!(record.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(record.line, Some(10));
        assert_eq!(record.severity.as_deref(), Some("error"));
        assert_eq!(
            record.fingerprint.as_deref(),
            Some("src/lib.rs:10:lint/security")
        );
        assert_eq!(record.fixable, Some(true));
        assert_eq!(record.metadata_json["category"], "security");
        assert_eq!(record.metadata_json["source_sidecar"], "lint-findings");
    }

    #[test]
    fn test_finding_records_from_lint() {
        let findings = [
            lint_finding("one", "security", Some("phpcs")),
            lint_finding("two", "i18n", None),
        ];

        let records = finding_records_from_lint("run-1", &findings);

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].fingerprint.as_deref(), Some("one"));
        assert_eq!(records[0].tool, "phpcs");
        assert_eq!(records[1].fingerprint.as_deref(), Some("two"));
        assert_eq!(records[1].tool, "lint");
    }

    #[test]
    fn test_finding_record_from_audit() {
        let finding = audit_finding();

        let record = finding_record_from_audit("run-1", &finding);

        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.tool, "audit");
        assert_eq!(record.rule.as_deref(), Some("missing_method"));
        assert_eq!(record.file.as_deref(), Some("src/commands/foo.rs"));
        assert_eq!(record.severity.as_deref(), Some("warning"));
        assert_eq!(record.message, "Missing run function");
        assert_eq!(record.metadata_json["source_sidecar"], "audit-findings");
        assert_eq!(record.metadata_json["convention"], "command modules");
        assert_eq!(record.metadata_json["kind"], "missing_method");
    }

    #[test]
    fn test_finding_records_from_audit() {
        let finding = audit_finding();

        let first = finding_records_from_audit("run-1", std::slice::from_ref(&finding));
        let second = finding_records_from_audit("run-2", &[finding]);

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert_eq!(first[0].fingerprint, second[0].fingerprint);
    }

    #[test]
    fn test_finding_records_from_annotation_file() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("phpcs.json");
        std::fs::write(
            &path,
            serde_json::to_string(&serde_json::json!([
                {
                    "file": "src/lib.rs",
                    "line": 12,
                    "message": "escape output",
                    "source": "phpcs",
                    "severity": "warning",
                    "code": "WordPress.Security.EscapeOutput",
                    "fixable": true,
                    "github_level": "warning"
                }
            ]))
            .expect("json"),
        )
        .expect("write");

        let records = finding_records_from_annotation_file("run-1", &path).expect("records");

        assert_eq!(records.len(), 1);
        let record = &records[0];
        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.tool, "phpcs");
        assert_eq!(
            record.rule.as_deref(),
            Some("WordPress.Security.EscapeOutput")
        );
        assert_eq!(record.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(record.line, Some(12));
        assert_eq!(record.severity.as_deref(), Some("warning"));
        assert_eq!(record.message, "escape output");
        assert_eq!(record.fixable, Some(true));
        assert!(record
            .fingerprint
            .as_deref()
            .expect("fingerprint")
            .contains("WordPress.Security.EscapeOutput"));
        assert_eq!(record.metadata_json["source_sidecar"], "annotations");
        assert_eq!(record.metadata_json["annotation_file"], "phpcs.json");
        assert_eq!(record.metadata_json["raw"]["github_level"], "warning");
    }

    #[test]
    fn test_finding_records_from_annotations_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        std::fs::write(temp.path().join("b.json"), annotation_json("src/b.rs", "B"))
            .expect("write b");
        std::fs::write(temp.path().join("a.json"), annotation_json("src/a.rs", "A"))
            .expect("write a");
        std::fs::write(temp.path().join("ignored.txt"), "[]").expect("write ignored");

        let records = finding_records_from_annotations_dir("run-1", temp.path()).expect("records");

        assert_eq!(records.len(), 2);
        assert_eq!(records[0].file.as_deref(), Some("src/a.rs"));
        assert_eq!(records[0].rule.as_deref(), Some("A"));
        assert_eq!(records[1].file.as_deref(), Some("src/b.rs"));
        assert_eq!(records[1].rule.as_deref(), Some("B"));
    }

    #[test]
    fn test_finding_record_from_annotation() {
        let annotation = AnnotationFindingRecord {
            file: Some("src/lib.rs".to_string()),
            line: Some(33),
            message: "escape output".to_string(),
            source: None,
            severity: Some("notice".to_string()),
            code: Some("WordPress.Security".to_string()),
            fixable: Some(false),
            extra: BTreeMap::from([("id".to_string(), serde_json::json!("custom-id"))]),
        };

        let record = finding_record_from_annotation("run-1", &annotation, "phpcs.json");

        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.tool, "phpcs");
        assert_eq!(record.rule.as_deref(), Some("WordPress.Security"));
        assert_eq!(record.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(record.line, Some(33));
        assert_eq!(record.severity.as_deref(), Some("notice"));
        assert_eq!(record.fingerprint.as_deref(), Some("custom-id"));
        assert_eq!(record.fixable, Some(false));
    }

    fn lint_finding(id: &str, category: &str, tool: Option<&str>) -> LintFinding {
        LintFinding {
            id: id.to_string(),
            message: format!("{category} finding"),
            category: category.to_string(),
            tool: tool.map(str::to_string),
            file: Some("src/lib.rs".to_string()),
            severity: Some("error".to_string()),
            extra: BTreeMap::new(),
        }
    }

    fn audit_finding() -> code_audit::Finding {
        code_audit::Finding {
            convention: "command modules".to_string(),
            severity: code_audit::Severity::Warning,
            file: "src/commands/foo.rs".to_string(),
            description: "Missing run function".to_string(),
            suggestion: "Add run()".to_string(),
            kind: code_audit::AuditFinding::MissingMethod,
        }
    }

    fn annotation_json(file: &str, code: &str) -> String {
        serde_json::to_string(&serde_json::json!([
            {
                "file": file,
                "line": 1,
                "message": "annotation",
                "code": code
            }
        ]))
        .expect("json")
    }
}
