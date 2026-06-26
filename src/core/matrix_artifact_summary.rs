use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::core::artifact_ref::{ArtifactRef, EvidenceRef, ARTIFACT_REF_SCHEMA};
use crate::core::observation::{ArtifactRecord, FindingRecord};

pub const MATRIX_ARTIFACT_SUMMARY_SCHEMA: &str = "homeboy/matrix-artifact-summary/v1";
pub const GENERIC_MATRIX_SUMMARY_SCHEMA: &str = "homeboy/matrix-summary/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GenericMatrixSummary {
    pub schema: String,
    pub run_id: String,
    pub status: String,
    pub case_count: usize,
    pub failed_count: usize,
    pub needs_review_count: usize,
    pub source_artifact: ArtifactRef,
    pub artifact_refs: Vec<EvidenceRef>,
    pub preview_refs: Vec<EvidenceRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatrixArtifactSummary {
    pub schema: String,
    pub run_id: String,
    pub fixture_count: usize,
    pub finding_count: usize,
    pub group_counts: Vec<MatrixSummaryCount>,
    pub top_diagnostic_kinds: Vec<MatrixSummaryCount>,
    pub top_fixtures: Vec<MatrixSummaryCount>,
    pub result_refs: Vec<ArtifactRef>,
    pub finding_packet_refs: Vec<ArtifactRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MatrixSummaryCount {
    pub key: String,
    pub count: usize,
}

#[derive(Default)]
struct SummaryAccumulator {
    fixture_count: usize,
    finding_count: usize,
    group_counts: BTreeMap<String, usize>,
    diagnostic_kind_counts: BTreeMap<String, usize>,
    fixture_counts: BTreeMap<String, usize>,
    result_ref_ids: BTreeSet<String>,
    result_refs: Vec<ArtifactRef>,
    finding_packet_ref_ids: BTreeSet<String>,
    finding_packet_refs: Vec<ArtifactRef>,
}

pub fn summarize_matrix_artifacts(
    run_id: &str,
    artifacts: &[ArtifactRecord],
    findings: &[FindingRecord],
) -> Option<MatrixArtifactSummary> {
    let mut acc = SummaryAccumulator::default();
    let mut saw_matrix_signal = false;

    for artifact in artifacts {
        if artifact.run_id != run_id {
            continue;
        }
        let class = classify_artifact(artifact);
        if class.is_matrix {
            saw_matrix_signal = true;
        }
        if class.is_result {
            push_ref(&mut acc.result_refs, &mut acc.result_ref_ids, artifact);
        }
        if class.is_finding_packet {
            push_ref(
                &mut acc.finding_packet_refs,
                &mut acc.finding_packet_ref_ids,
                artifact,
            );
        }
        if let Some(value) = read_json_artifact(artifact) {
            saw_matrix_signal |= value_has_matrix_signal(&value);
            collect_from_value(&mut acc, &value);
        }
    }

    if !findings.is_empty() {
        saw_matrix_signal = true;
        acc.finding_count = acc.finding_count.max(findings.len());
        for finding in findings {
            collect_finding_record(&mut acc, finding);
        }
    }

    if !saw_matrix_signal {
        return None;
    }

    Some(MatrixArtifactSummary {
        schema: MATRIX_ARTIFACT_SUMMARY_SCHEMA.to_string(),
        run_id: run_id.to_string(),
        fixture_count: acc.fixture_count,
        finding_count: acc.finding_count,
        group_counts: top_counts(acc.group_counts, 20),
        top_diagnostic_kinds: top_counts(acc.diagnostic_kind_counts, 10),
        top_fixtures: top_counts(acc.fixture_counts, 10),
        result_refs: acc.result_refs,
        finding_packet_refs: acc.finding_packet_refs,
    })
}

pub fn generic_matrix_summary_from_artifacts(
    run_id: &str,
    artifacts: &[ArtifactRecord],
) -> Option<GenericMatrixSummary> {
    for artifact in artifacts {
        let Some(value) = read_json_artifact(artifact) else {
            continue;
        };
        if schema_at(&value) != Some(GENERIC_MATRIX_SUMMARY_SCHEMA) {
            continue;
        }
        return Some(generic_matrix_summary_from_value(run_id, artifact, &value));
    }
    None
}

fn generic_matrix_summary_from_value(
    run_id: &str,
    artifact: &ArtifactRecord,
    value: &Value,
) -> GenericMatrixSummary {
    GenericMatrixSummary {
        schema: GENERIC_MATRIX_SUMMARY_SCHEMA.to_string(),
        run_id: run_id.to_string(),
        status: first_string(
            value,
            &[
                &["status"][..],
                &["overall_status"][..],
                &["summary", "status"][..],
                &["matrix", "status"][..],
            ],
        )
        .unwrap_or("unknown")
        .to_string(),
        case_count: first_usize(
            value,
            &[
                &["case_count"][..],
                &["total_cases"][..],
                &["summary", "case_count"][..],
                &["matrix", "case_count"][..],
            ],
        )
        .or_else(|| array_len(value, &["cases"]))
        .or_else(|| array_len(value, &["results"]))
        .unwrap_or(0),
        failed_count: first_usize(
            value,
            &[
                &["failed_count"][..],
                &["fail_count"][..],
                &["summary", "failed_count"][..],
                &["status_counts", "failed"][..],
                &["status_counts", "fail"][..],
            ],
        )
        .unwrap_or(0),
        needs_review_count: first_usize(
            value,
            &[
                &["needs_review_count"][..],
                &["review_count"][..],
                &["summary", "needs_review_count"][..],
                &["status_counts", "needs_review"][..],
            ],
        )
        .unwrap_or(0),
        source_artifact: artifact_ref_with_metadata(artifact),
        artifact_refs: collect_evidence_refs(value, &["artifact_refs"], "artifact"),
        preview_refs: collect_evidence_refs(value, &["preview_refs"], "preview"),
    }
}

fn schema_at(value: &Value) -> Option<&str> {
    string_at(value, &["schema"]).or_else(|| string_at(value, &["$schema"]))
}

fn first_string<'a>(value: &'a Value, paths: &[&[&str]]) -> Option<&'a str> {
    paths.iter().find_map(|path| string_at(value, path))
}

fn first_usize(value: &Value, paths: &[&[&str]]) -> Option<usize> {
    paths
        .iter()
        .find_map(|path| value_at(value, path).and_then(Value::as_u64))
        .map(|count| count as usize)
}

fn array_len(value: &Value, path: &[&str]) -> Option<usize> {
    value_at(value, path)
        .and_then(Value::as_array)
        .map(Vec::len)
}

fn collect_evidence_refs(value: &Value, path: &[&str], default_kind: &str) -> Vec<EvidenceRef> {
    value_at(value, path)
        .and_then(Value::as_array)
        .map(|items| {
            items
                .iter()
                .filter_map(|item| evidence_ref_from_value(item, default_kind))
                .collect()
        })
        .unwrap_or_default()
}

fn evidence_ref_from_value(value: &Value, default_kind: &str) -> Option<EvidenceRef> {
    if let Some(target) = value.as_str() {
        return Some(EvidenceRef::new(default_kind, target, target));
    }
    let target = string_at(value, &["ref"])
        .or_else(|| string_at(value, &["target"]))
        .or_else(|| string_at(value, &["url"]))
        .or_else(|| string_at(value, &["path"]))?;
    let kind = string_at(value, &["kind"]).unwrap_or(default_kind);
    let label = string_at(value, &["label"])
        .or_else(|| string_at(value, &["id"]))
        .unwrap_or(target);
    Some(EvidenceRef::new(kind, target, label))
}

pub fn render_matrix_artifact_summary_markdown(summary: &MatrixArtifactSummary) -> String {
    let mut lines = vec![
        format!("# Matrix Artifact Summary"),
        String::new(),
        format!("Run: `{}`", summary.run_id),
        format!("Fixtures: {}", summary.fixture_count),
        format!("Findings: {}", summary.finding_count),
    ];
    push_count_section(&mut lines, "Group Counts", &summary.group_counts);
    push_count_section(
        &mut lines,
        "Top Diagnostic Kinds",
        &summary.top_diagnostic_kinds,
    );
    push_count_section(&mut lines, "Top Fixtures", &summary.top_fixtures);
    push_ref_section(&mut lines, "Result Artifacts", &summary.result_refs);
    push_ref_section(
        &mut lines,
        "Finding Packet Artifacts",
        &summary.finding_packet_refs,
    );
    lines.push(String::new());
    lines.join("\n")
}

fn push_count_section(lines: &mut Vec<String>, title: &str, counts: &[MatrixSummaryCount]) {
    lines.push(String::new());
    lines.push(format!("## {title}"));
    if counts.is_empty() {
        lines.push("None".to_string());
        return;
    }
    for count in counts {
        lines.push(format!("- {}: {}", count.key, count.count));
    }
}

fn push_ref_section(lines: &mut Vec<String>, title: &str, refs: &[ArtifactRef]) {
    lines.push(String::new());
    lines.push(format!("## {title}"));
    if refs.is_empty() {
        lines.push("None".to_string());
        return;
    }
    for artifact in refs {
        lines.push(format!(
            "- `{}` ({}, {})",
            artifact.id, artifact.kind, artifact.artifact_type
        ));
    }
}

#[derive(Default)]
struct ArtifactClass {
    is_matrix: bool,
    is_result: bool,
    is_finding_packet: bool,
}

fn classify_artifact(artifact: &ArtifactRecord) -> ArtifactClass {
    let mut tokens = vec![
        artifact.kind.as_str(),
        artifact.path.as_str(),
        artifact.id.as_str(),
    ];
    for key in ["role", "semantic_key", "schema", "kind"] {
        if let Some(value) = artifact.metadata_json.get(key).and_then(Value::as_str) {
            tokens.push(value);
        }
    }
    let joined = tokens.join(" ").to_ascii_lowercase();
    ArtifactClass {
        is_matrix: joined.contains("matrix")
            || joined.contains("fixture")
            || joined.contains("finding-packet")
            || joined.contains("finding_packet"),
        is_result: joined.contains("result") || joined.contains("summary"),
        is_finding_packet: (joined.contains("finding") && joined.contains("packet"))
            || joined.contains("finding-packets")
            || joined.contains("finding_packets"),
    }
}

fn push_ref(refs: &mut Vec<ArtifactRef>, seen: &mut BTreeSet<String>, artifact: &ArtifactRecord) {
    if !seen.insert(artifact.id.clone()) {
        return;
    }
    refs.push(artifact_ref_with_metadata(artifact));
}

fn artifact_ref_with_metadata(artifact: &ArtifactRecord) -> ArtifactRef {
    ArtifactRef {
        schema: ARTIFACT_REF_SCHEMA.to_string(),
        id: artifact.id.clone(),
        run_id: artifact.run_id.clone(),
        kind: artifact.kind.clone(),
        artifact_type: artifact.artifact_type.clone(),
        path: artifact.path.clone(),
        url: artifact.url.clone(),
        public_url: artifact.public_url.clone(),
        role: artifact
            .metadata_json
            .get("role")
            .and_then(Value::as_str)
            .map(str::to_string),
        semantic_key: artifact
            .metadata_json
            .get("semantic_key")
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn read_json_artifact(artifact: &ArtifactRecord) -> Option<Value> {
    if artifact.artifact_type != "file" {
        return None;
    }
    let path = Path::new(&artifact.path);
    let looks_json = artifact.mime.as_deref() == Some("application/json")
        || path.extension().and_then(|ext| ext.to_str()) == Some("json");
    if !looks_json || !path.is_file() {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    serde_json::from_slice(&bytes).ok()
}

fn value_has_matrix_signal(value: &Value) -> bool {
    string_at(value, &["schema"]).is_some_and(|schema| schema.contains("matrix"))
        || value.get("matrix").is_some()
        || value.get("fixtures").is_some()
        || value.get("fixture_count").is_some()
        || value.get("group_counts").is_some()
        || value.get("finding_packets").is_some()
}

fn collect_from_value(acc: &mut SummaryAccumulator, value: &Value) {
    collect_count_field(acc, value, &["fixture_count"], CountTarget::FixtureTotal);
    collect_count_field(
        acc,
        value,
        &["summary", "fixture_count"],
        CountTarget::FixtureTotal,
    );
    collect_count_field(acc, value, &["total_fixtures"], CountTarget::FixtureTotal);
    collect_count_field(acc, value, &["finding_count"], CountTarget::FindingTotal);
    collect_count_field(
        acc,
        value,
        &["summary", "finding_count"],
        CountTarget::FindingTotal,
    );
    collect_count_field(acc, value, &["total_findings"], CountTarget::FindingTotal);

    collect_array_len(acc, value, &["fixtures"], CountTarget::FixtureTotal);
    collect_array_len(acc, value, &["results"], CountTarget::FixtureTotal);
    collect_array_len(acc, value, &["cells"], CountTarget::FixtureTotal);
    collect_array_len(acc, value, &["findings"], CountTarget::FindingTotal);
    collect_array_len(acc, value, &["finding_packets"], CountTarget::FindingTotal);
    collect_array_len(acc, value, &["packets"], CountTarget::FindingTotal);

    collect_counts_object(&mut acc.group_counts, value_at(value, &["group_counts"]));
    collect_counts_object(
        &mut acc.group_counts,
        value_at(value, &["summary", "group_counts"]),
    );
    collect_counts_object(
        &mut acc.diagnostic_kind_counts,
        value_at(value, &["top_diagnostic_kinds"]),
    );
    collect_counts_object(
        &mut acc.diagnostic_kind_counts,
        value_at(value, &["top_finding_kinds"]),
    );
    collect_counts_object(&mut acc.fixture_counts, value_at(value, &["top_fixtures"]));

    for key in [
        "findings",
        "finding_packets",
        "packets",
        "results",
        "fixtures",
        "cells",
    ] {
        if let Some(items) = value.get(key).and_then(Value::as_array) {
            for item in items {
                collect_finding_like_value(acc, item);
            }
        }
    }
}

enum CountTarget {
    FixtureTotal,
    FindingTotal,
}

fn collect_count_field(
    acc: &mut SummaryAccumulator,
    value: &Value,
    path: &[&str],
    target: CountTarget,
) {
    if let Some(count) = value_at(value, path).and_then(Value::as_u64) {
        apply_total(acc, target, count as usize);
    }
}

fn collect_array_len(
    acc: &mut SummaryAccumulator,
    value: &Value,
    path: &[&str],
    target: CountTarget,
) {
    if let Some(items) = value_at(value, path).and_then(Value::as_array) {
        apply_total(acc, target, items.len());
    }
}

fn apply_total(acc: &mut SummaryAccumulator, target: CountTarget, count: usize) {
    match target {
        CountTarget::FixtureTotal => acc.fixture_count = acc.fixture_count.max(count),
        CountTarget::FindingTotal => acc.finding_count = acc.finding_count.max(count),
    }
}

fn collect_counts_object(counts: &mut BTreeMap<String, usize>, value: Option<&Value>) {
    let Some(value) = value else {
        return;
    };
    if let Some(object) = value.as_object() {
        for (key, value) in object {
            if let Some(count) = value.as_u64() {
                increment_by(counts, key, count as usize);
            }
        }
    } else if let Some(items) = value.as_array() {
        for item in items {
            if let (Some(key), Some(count)) = (
                string_at(item, &["key"]).or_else(|| string_at(item, &["kind"])),
                value_at(item, &["count"]).and_then(Value::as_u64),
            ) {
                increment_by(counts, key, count as usize);
            }
        }
    }
}

fn collect_finding_like_value(acc: &mut SummaryAccumulator, value: &Value) {
    for path in [
        &["group"][..],
        &["category"][..],
        &["diagnostic", "group"][..],
        &["metadata", "group"][..],
    ] {
        if let Some(group) = string_at(value, path) {
            increment(&mut acc.group_counts, group);
            break;
        }
    }
    for path in [
        &["diagnostic_kind"][..],
        &["kind"][..],
        &["rule"][..],
        &["code"][..],
        &["diagnostic", "kind"][..],
        &["metadata", "kind"][..],
    ] {
        if let Some(kind) = string_at(value, path) {
            increment(&mut acc.diagnostic_kind_counts, kind);
            break;
        }
    }
    for path in [
        &["fixture"][..],
        &["fixture_id"][..],
        &["fixture_name"][..],
        &["target"][..],
        &["path"][..],
        &["metadata", "fixture"][..],
    ] {
        if let Some(fixture) = string_at(value, path) {
            increment(&mut acc.fixture_counts, fixture);
            break;
        }
    }
}

fn collect_finding_record(acc: &mut SummaryAccumulator, finding: &FindingRecord) {
    if let Some(rule) = finding.rule.as_deref() {
        increment(&mut acc.diagnostic_kind_counts, rule);
    } else {
        increment(&mut acc.diagnostic_kind_counts, &finding.tool);
    }
    if let Some(file) = finding.file.as_deref() {
        increment(&mut acc.fixture_counts, file);
    }
    collect_finding_like_value(acc, &finding.metadata_json);
}

fn value_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for key in path {
        current = current.get(*key)?;
    }
    Some(current)
}

fn string_at<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    value_at(value, path)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn increment(counts: &mut BTreeMap<String, usize>, key: &str) {
    increment_by(counts, key, 1);
}

fn increment_by(counts: &mut BTreeMap<String, usize>, key: &str, count: usize) {
    if key.trim().is_empty() || count == 0 {
        return;
    }
    *counts.entry(key.trim().to_string()).or_default() += count;
}

fn top_counts(counts: BTreeMap<String, usize>, limit: usize) -> Vec<MatrixSummaryCount> {
    let mut rows = counts
        .into_iter()
        .map(|(key, count)| MatrixSummaryCount { key, count })
        .collect::<Vec<_>>();
    rows.sort_by(|a, b| b.count.cmp(&a.count).then_with(|| a.key.cmp(&b.key)));
    rows.truncate(limit);
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summarizes_generic_matrix_artifacts_and_refs() {
        let temp = tempfile::tempdir().expect("tempdir");
        let summary_path = temp.path().join("summary.json");
        std::fs::write(
            &summary_path,
            serde_json::to_vec(&serde_json::json!({
                "schema": "example/matrix-summary/v1",
                "fixture_count": 3,
                "finding_count": 4,
                "group_counts": { "content": 3, "layout": 1 },
                "findings": [
                    { "kind": "missing_heading", "fixture": "about" },
                    { "kind": "missing_heading", "fixture": "home" },
                    { "kind": "broken_link", "fixture": "about" }
                ]
            }))
            .expect("summary json"),
        )
        .expect("write summary");
        let packets_path = temp.path().join("finding-packets.json");
        std::fs::write(
            &packets_path,
            serde_json::to_vec(&serde_json::json!({
                "finding_packets": [
                    { "diagnostic_kind": "missing_heading", "fixture_id": "contact" }
                ]
            }))
            .expect("packet json"),
        )
        .expect("write packets");

        let artifacts = vec![
            artifact(
                "a1",
                "matrix_summary",
                &summary_path,
                serde_json::json!({ "role": "matrix-summary" }),
            ),
            artifact(
                "a2",
                "finding_packets",
                &packets_path,
                serde_json::json!({ "role": "matrix-finding-packets" }),
            ),
        ];

        let summary = summarize_matrix_artifacts("run-1", &artifacts, &[]).expect("summary");

        assert_eq!(summary.fixture_count, 3);
        assert_eq!(summary.finding_count, 4);
        assert_eq!(summary.group_counts[0].key, "content");
        assert_eq!(summary.top_diagnostic_kinds[0].key, "missing_heading");
        assert_eq!(summary.top_diagnostic_kinds[0].count, 3);
        assert_eq!(summary.top_fixtures[0].key, "about");
        assert_eq!(summary.result_refs.len(), 1);
        assert_eq!(summary.finding_packet_refs.len(), 1);
        assert_eq!(
            summary.finding_packet_refs[0].role.as_deref(),
            Some("matrix-finding-packets")
        );
    }

    fn artifact(id: &str, kind: &str, path: &Path, metadata_json: Value) -> ArtifactRecord {
        ArtifactRecord {
            id: id.to_string(),
            run_id: "run-1".to_string(),
            kind: kind.to_string(),
            artifact_type: "file".to_string(),
            path: path.display().to_string(),
            url: None,
            public_url: None,
            viewer_url: None,
            viewer_links: Vec::new(),
            sha256: None,
            size_bytes: None,
            mime: Some("application/json".to_string()),
            metadata_json,
            created_at: "2026-06-25T00:00:00Z".to_string(),
        }
    }
}
