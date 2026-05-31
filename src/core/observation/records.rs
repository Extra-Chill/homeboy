use serde::{Deserialize, Serialize};

#[path = "finding_records.rs"]
mod finding_records;
mod run_builder;
mod run_status;
mod trace_run_builder;
mod trace_span_builder;
mod triage_items;

pub use finding_records::*;
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

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ArtifactCleanupFilter {
    pub created_before: Option<String>,
    pub run_id: Option<String>,
    pub kind: Option<String>,
    pub artifact_type: Option<String>,
    pub run_kind: Option<String>,
    pub component_id: Option<String>,
    pub limit: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactCleanupCandidateRecord {
    pub artifact: ArtifactRecord,
    pub run_kind: String,
    pub component_id: Option<String>,
    pub run_started_at: String,
    pub run_status: String,
}

fn default_artifact_type() -> String {
    "file".to_string()
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
    use crate::core::code_audit;
    use crate::core::finding::{FindingSource, HomeboyFinding};
    use std::collections::BTreeMap;

    #[test]
    fn test_builder() {
        let record = NewRunRecord::builder("lint").build();

        assert_eq!(record.kind, "lint");
        assert_eq!(record.metadata_json, serde_json::json!({}));
    }

    #[test]
    fn test_finding_record_from_lint() {
        let finding = HomeboyFinding::builder("phpcs", "escape output")
            .category("security")
            .rule("WordPress.Security")
            .file("src/lib.rs")
            .line(10)
            .severity("error")
            .fingerprint("src/lib.rs:10:lint/security")
            .fixable(true)
            .source(FindingSource::new("sidecar").label("lint-findings"))
            .metadata("source_sidecar", "lint-findings")
            .build();

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
        let annotation = AnnotationSidecarItem {
            file: Some("src/lib.rs".to_string()),
            line: Some(33),
            message: "escape output".to_string(),
            source: None,
            severity: Some("notice".to_string()),
            code: Some("WordPress.Security".to_string()),
            fixable: Some(false),
            extra: BTreeMap::from([("id".to_string(), serde_json::json!("custom-id"))]),
        };

        let record = NewFindingRecord::from_homeboy_finding(
            "run-1",
            homeboy_finding_from_annotation(&annotation, "phpcs.json"),
        );

        assert_eq!(record.run_id, "run-1");
        assert_eq!(record.tool, "phpcs");
        assert_eq!(record.rule.as_deref(), Some("WordPress.Security"));
        assert_eq!(record.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(record.line, Some(33));
        assert_eq!(record.severity.as_deref(), Some("notice"));
        assert_eq!(record.fingerprint.as_deref(), Some("custom-id"));
        assert_eq!(record.fixable, Some(false));
    }

    #[test]
    fn recorded_homeboy_finding_projects_normalized_shape() {
        let record = FindingRecord {
            id: "finding-1".to_string(),
            run_id: "run-1".to_string(),
            tool: "phpcs".to_string(),
            rule: Some("WordPress.Security".to_string()),
            file: Some("src/lib.php".to_string()),
            line: Some(12),
            severity: Some("warning".to_string()),
            fingerprint: Some("src/lib.php:12:WordPress.Security".to_string()),
            message: "escape output".to_string(),
            fixable: Some(true),
            metadata_json: serde_json::json!({
                "category": "security",
                "column": 4,
                "source_sidecar": "lint-findings",
                "source": { "kind": "sidecar", "path": "lint-findings.json" }
            }),
            created_at: "2026-05-30T16:00:00Z".to_string(),
        };

        let recorded = RecordedHomeboyFinding::from(record.clone());

        assert_eq!(recorded.id, "finding-1");
        assert_eq!(recorded.finding.category.as_deref(), Some("security"));
        assert_eq!(recorded.finding.location.column, Some(4));
        assert_eq!(recorded.finding.metadata["source_sidecar"], "lint-findings");
        assert_eq!(FindingRecord::from(recorded), record);
    }

    fn lint_finding(id: &str, category: &str, tool: Option<&str>) -> HomeboyFinding {
        HomeboyFinding::builder(tool.unwrap_or("lint"), format!("{category} finding"))
            .category(category)
            .file("src/lib.rs")
            .severity("error")
            .fingerprint(id)
            .build()
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
