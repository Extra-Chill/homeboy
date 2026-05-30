use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

use crate::core::code_audit::{self, report::finding_kind_key};
use crate::core::extension::lint::LintFinding;
use crate::core::finding::HomeboyFinding;

use super::records::NewFindingRecord;

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
    let mut builder = HomeboyFinding::builder(
        finding.tool.clone().unwrap_or_else(|| "lint".to_string()),
        finding.message.clone(),
    )
    .category(finding.category.clone())
    .fingerprint(finding.id.clone())
    .source_sidecar("lint-findings")
    .raw(finding);

    if let Some(rule) =
        lint_extra_string(finding, "rule").or_else(|| Some(finding.category.clone()))
    {
        builder = builder.rule(rule);
    }
    if let Some(file) = &finding.file {
        builder = builder.file(file.clone());
    }
    if let Some(line) = lint_extra_i64(finding, "line") {
        builder = builder.line(line);
    }
    if let Some(severity) = &finding.severity {
        builder = builder.severity(severity.clone());
    }
    if let Some(fixable) = lint_extra_bool(finding, "fixable") {
        builder = builder.fixable(fixable);
    }

    NewFindingRecord::from_homeboy_finding(run_id, &builder.build())
}

pub fn finding_records_from_lint(run_id: &str, findings: &[LintFinding]) -> Vec<NewFindingRecord> {
    findings
        .iter()
        .map(|finding| finding_record_from_lint(run_id, finding))
        .collect()
}

pub fn finding_record_from_audit(run_id: &str, finding: &code_audit::Finding) -> NewFindingRecord {
    let kind = finding_kind_key(&finding.kind);
    let normalized = HomeboyFinding::builder("audit", finding.description.clone())
        .rule(kind.clone())
        .category("code_audit")
        .file(finding.file.clone())
        .severity(audit_severity_key(&finding.severity))
        .fingerprint(audit_finding_fingerprint(finding))
        .source_sidecar("audit-findings")
        .metadata(serde_json::json!({
            "convention": finding.convention,
            "suggestion": finding.suggestion,
            "confidence": finding.kind.confidence(),
            "kind": kind,
        }))
        .raw(finding)
        .build();

    NewFindingRecord::from_homeboy_finding(run_id, &normalized)
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
    let mut builder = HomeboyFinding::builder(tool, annotation.message.clone())
        .category("annotation")
        .source_sidecar("annotations")
        .source_artifact(source_file)
        .metadata(serde_json::json!({ "annotation_file": source_file }))
        .raw(annotation);

    if let Some(rule) = &annotation.code {
        builder = builder.rule(rule.clone());
    }
    if let Some(file) = &annotation.file {
        builder = builder.file(file.clone());
    }
    if let Some(line) = annotation.line {
        builder = builder.line(line);
    }
    if let Some(severity) = &annotation.severity {
        builder = builder.severity(severity.clone());
    }
    if let Some(fingerprint) = annotation_fingerprint(annotation) {
        builder = builder.fingerprint(fingerprint);
    }
    if let Some(fixable) = annotation.fixable {
        builder = builder.fixable(fixable);
    }

    NewFindingRecord::from_homeboy_finding(run_id, &builder.build())
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
