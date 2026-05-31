use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{collections::BTreeMap, path::Path};

use crate::core::code_audit;
use crate::core::finding::{FindingProducer, FindingSource, HomeboyFinding};

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

impl NewFindingRecord {
    pub fn from_homeboy_finding(run_id: impl Into<String>, finding: HomeboyFinding) -> Self {
        let metadata_json = finding.metadata_json();
        Self {
            run_id: run_id.into(),
            tool: finding.tool,
            rule: finding.rule,
            file: finding.location.file,
            line: finding.location.line,
            severity: finding.severity,
            fingerprint: finding.fingerprint,
            message: finding.message,
            fixable: finding.fix.fixable,
            metadata_json,
        }
    }
}

pub fn finding_records_from_homeboy_findings(
    run_id: &str,
    findings: impl IntoIterator<Item = HomeboyFinding>,
) -> Vec<NewFindingRecord> {
    let run_id = run_id.to_string();
    findings
        .into_iter()
        .map(|finding| NewFindingRecord::from_homeboy_finding(run_id.clone(), finding))
        .collect()
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecordedHomeboyFinding {
    pub id: String,
    pub run_id: String,
    #[serde(flatten)]
    pub finding: HomeboyFinding,
    pub created_at: String,
}

impl From<FindingRecord> for RecordedHomeboyFinding {
    fn from(record: FindingRecord) -> Self {
        let mut metadata = record
            .metadata_json
            .as_object()
            .cloned()
            .unwrap_or_default();
        let category = take_optional_string(&mut metadata, "category");
        let column = take_optional_i64(&mut metadata, "column");
        let producer = take_optional_json::<FindingProducer>(&mut metadata, "producer");
        let source = take_optional_json::<FindingSource>(&mut metadata, "source");
        let raw = metadata.remove("raw");
        let finding = HomeboyFinding {
            tool: record.tool,
            rule: record.rule,
            category,
            severity: record.severity,
            location: crate::core::finding::FindingLocation {
                file: record.file,
                line: record.line,
                column,
            },
            message: record.message,
            fingerprint: record.fingerprint,
            fix: crate::core::finding::FindingFix {
                fixable: record.fixable,
            },
            producer,
            source,
            metadata,
            raw,
        };
        Self {
            id: record.id,
            run_id: record.run_id,
            finding,
            created_at: record.created_at,
        }
    }
}

impl From<RecordedHomeboyFinding> for FindingRecord {
    fn from(recorded: RecordedHomeboyFinding) -> Self {
        let metadata_json = recorded.finding.metadata_json();
        Self {
            id: recorded.id,
            run_id: recorded.run_id,
            tool: recorded.finding.tool,
            rule: recorded.finding.rule,
            file: recorded.finding.location.file,
            line: recorded.finding.location.line,
            severity: recorded.finding.severity,
            fingerprint: recorded.finding.fingerprint,
            message: recorded.finding.message,
            fixable: recorded.finding.fix.fixable,
            metadata_json,
            created_at: recorded.created_at,
        }
    }
}

fn take_optional_string(
    metadata: &mut serde_json::Map<String, Value>,
    key: &str,
) -> Option<String> {
    metadata
        .remove(key)
        .and_then(|value| value.as_str().map(str::to_string))
}

fn take_optional_i64(metadata: &mut serde_json::Map<String, Value>, key: &str) -> Option<i64> {
    metadata.remove(key).and_then(|value| value.as_i64())
}

fn take_optional_json<T: for<'de> Deserialize<'de>>(
    metadata: &mut serde_json::Map<String, Value>,
    key: &str,
) -> Option<T> {
    metadata
        .remove(key)
        .and_then(|value| serde_json::from_value(value).ok())
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
pub(crate) struct AnnotationSidecarItem {
    pub(crate) file: Option<String>,
    pub(crate) line: Option<i64>,
    pub(crate) message: String,
    pub(crate) source: Option<String>,
    pub(crate) severity: Option<String>,
    pub(crate) code: Option<String>,
    pub(crate) fixable: Option<bool>,
    #[serde(flatten, default, skip_serializing_if = "BTreeMap::is_empty")]
    pub(crate) extra: BTreeMap<String, Value>,
}

pub fn finding_record_from_lint(run_id: &str, finding: HomeboyFinding) -> NewFindingRecord {
    NewFindingRecord::from_homeboy_finding(run_id, finding)
}

pub fn finding_records_from_lint(
    run_id: &str,
    findings: &[HomeboyFinding],
) -> Vec<NewFindingRecord> {
    finding_records_from_homeboy_findings(run_id, findings.iter().cloned())
}

pub fn homeboy_finding_from_audit(finding: &code_audit::Finding) -> HomeboyFinding {
    HomeboyFinding::from(finding)
}

pub fn finding_record_from_audit(run_id: &str, finding: &code_audit::Finding) -> NewFindingRecord {
    NewFindingRecord::from_homeboy_finding(run_id, homeboy_finding_from_audit(finding))
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
    Ok(finding_records_from_homeboy_findings(
        run_id,
        homeboy_findings_from_annotation_file(path)?,
    ))
}

fn homeboy_findings_from_annotation_file(
    path: &Path,
) -> crate::core::error::Result<Vec<HomeboyFinding>> {
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

    let annotations: Vec<AnnotationSidecarItem> = serde_json::from_str(&content).map_err(|e| {
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
        .map(|annotation| homeboy_finding_from_annotation(annotation, source_file))
        .collect())
}

pub(crate) fn homeboy_finding_from_annotation(
    annotation: &AnnotationSidecarItem,
    source_file: &str,
) -> HomeboyFinding {
    let tool = annotation
        .source
        .clone()
        .unwrap_or_else(|| annotation_file_stem(source_file));
    let mut normalized = HomeboyFinding::builder(tool, annotation.message.clone())
        .source(FindingSource::new("annotation").path(source_file))
        .metadata("source_sidecar", "annotations")
        .metadata("annotation_file", source_file)
        .raw(annotation)
        .build();
    normalized.rule = annotation.code.clone();
    normalized.location.file = annotation.file.clone();
    normalized.location.line = annotation.line;
    normalized.severity = annotation.severity.clone();
    normalized.fingerprint = annotation_fingerprint(annotation);
    normalized.fix.fixable = annotation.fixable;
    normalized
}

fn annotation_file_stem(source_file: &str) -> String {
    Path::new(source_file)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("annotations")
        .to_string()
}

fn annotation_fingerprint(annotation: &AnnotationSidecarItem) -> Option<String> {
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
