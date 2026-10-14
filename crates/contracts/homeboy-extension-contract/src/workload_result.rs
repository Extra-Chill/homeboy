//! The generic workload result envelope (#15840).
//!
//! Every extension-declared workload kind writes one of these to
//! `HOMEBOY_WORKLOAD_RESULTS_FILE`. Core reads only the generic fields:
//! status, findings (the shared [`HomeboyFinding`] shape), cases, numeric
//! metrics and artifacts. `payload` is the kind's own result and is opaque to
//! core; when the kind declares `result_schema`, `payload.schema` must match.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Component, Path};

use homeboy_finding::HomeboyFinding;
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use crate::workload_kind::WorkloadFindingsKey;

pub const WORKLOAD_RESULT_SCHEMA: &str = "homeboy/workload-result/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadResult {
    pub schema: String,
    pub kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workload_id: Option<String>,
    pub status: WorkloadStatus,
    #[serde(default)]
    pub findings: Vec<HomeboyFinding>,
    #[serde(default)]
    pub cases: Vec<WorkloadCase>,
    #[serde(default)]
    pub metrics: BTreeMap<String, f64>,
    #[serde(default)]
    pub artifacts: Vec<WorkloadArtifact>,
    #[serde(default)]
    pub payload: Value,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum WorkloadStatus {
    Pass,
    Fail,
    Error,
    Skipped,
}

impl WorkloadStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Error => "error",
            Self::Skipped => "skipped",
        }
    }

    pub fn is_pass(&self) -> bool {
        matches!(self, Self::Pass | Self::Skipped)
    }
}

/// One replayable unit of a workload run.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadCase {
    pub case_id: String,
    #[serde(
        default,
        deserialize_with = "string_or_number",
        skip_serializing_if = "Option::is_none"
    )]
    pub seed: Option<String>,
    /// Name of an entry in `artifacts`, or a path relative to the artifact
    /// directory, holding what replay needs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_ref: Option<String>,
    pub status: WorkloadStatus,
}

/// A file the runner left in `HOMEBOY_WORKLOAD_ARTIFACT_DIR`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct WorkloadArtifact {
    pub name: String,
    /// Relative to the artifact directory.
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mime: Option<String>,
}

fn string_or_number<'de, D>(deserializer: D) -> Result<Option<String>, D::Error>
where
    D: Deserializer<'de>,
{
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(Value::Number(value)) => Ok(Some(value.to_string())),
        Some(other) => Err(serde::de::Error::custom(format!(
            "seed must be a string or number, got {other}"
        ))),
    }
}

/// Whether `path` stays inside the directory it is relative to.
pub fn is_contained_relative_path(path: &str) -> bool {
    !path.trim().is_empty()
        && !path.contains('\\')
        && Path::new(path)
            .components()
            .all(|part| matches!(part, Component::Normal(_) | Component::CurDir))
}

impl WorkloadResult {
    /// Parse and validate an envelope a runner wrote for `kind`.
    pub fn parse(bytes: &[u8], kind: &str, result_schema: Option<&str>) -> Result<Self, String> {
        let result: Self = serde_json::from_slice(bytes)
            .map_err(|error| format!("invalid {WORKLOAD_RESULT_SCHEMA} envelope: {error}"))?;
        result.validate(kind, result_schema)?;
        Ok(result)
    }

    pub fn validate(&self, kind: &str, result_schema: Option<&str>) -> Result<(), String> {
        if self.schema != WORKLOAD_RESULT_SCHEMA {
            return Err(format!(
                "envelope schema must be {WORKLOAD_RESULT_SCHEMA}, got {}",
                self.schema
            ));
        }
        if self.kind != kind {
            return Err(format!(
                "envelope kind `{}` does not match the executed kind `{kind}`",
                self.kind
            ));
        }
        if self
            .workload_id
            .as_deref()
            .is_some_and(|id| id.trim().is_empty())
        {
            return Err("envelope workload_id must not be empty when present".to_string());
        }
        let mut case_ids = BTreeSet::new();
        for case in &self.cases {
            if case.case_id.trim().is_empty() {
                return Err("every case needs a non-empty case_id".to_string());
            }
            if !case_ids.insert(case.case_id.as_str()) {
                return Err(format!("duplicate case_id `{}`", case.case_id));
            }
        }
        for (name, value) in &self.metrics {
            if name.trim().is_empty() {
                return Err("metric names must not be empty".to_string());
            }
            if !value.is_finite() {
                return Err(format!("metric `{name}` must be finite"));
            }
        }
        for finding in &self.findings {
            if finding.tool.trim().is_empty() || finding.message.trim().is_empty() {
                return Err("every finding needs a tool and a message".to_string());
            }
        }
        let mut artifact_names = BTreeSet::new();
        for artifact in &self.artifacts {
            if artifact.name.trim().is_empty() {
                return Err("every artifact needs a name".to_string());
            }
            if !artifact_names.insert(artifact.name.as_str()) {
                return Err(format!("duplicate artifact name `{}`", artifact.name));
            }
            if !is_contained_relative_path(&artifact.path) {
                return Err(format!(
                    "artifact `{}` path must be relative to the artifact directory: {}",
                    artifact.name, artifact.path
                ));
            }
        }
        if let Some(expected) = result_schema {
            let actual = self.payload.get("schema").and_then(Value::as_str);
            if actual != Some(expected) {
                return Err(format!(
                    "payload.schema must be `{expected}` as declared by the kind, got {}",
                    actual.map_or_else(|| "none".to_string(), |schema| format!("`{schema}`"))
                ));
            }
        }
        Ok(())
    }

    pub fn case(&self, case_id: &str) -> Option<&WorkloadCase> {
        self.cases.iter().find(|case| case.case_id == case_id)
    }

    pub fn artifact(&self, name: &str) -> Option<&WorkloadArtifact> {
        self.artifacts.iter().find(|artifact| artifact.name == name)
    }

    /// Case counts by status.
    pub fn case_totals(&self) -> BTreeMap<&'static str, usize> {
        let mut totals = BTreeMap::new();
        for case in &self.cases {
            *totals.entry(case.status.as_str()).or_insert(0) += 1;
        }
        totals
    }
}

/// The identity a finding is matched by across runs.
pub fn finding_key(finding: &HomeboyFinding, key: WorkloadFindingsKey) -> String {
    match key {
        WorkloadFindingsKey::Fingerprint => finding
            .fingerprint
            .clone()
            .filter(|fingerprint| !fingerprint.trim().is_empty())
            .unwrap_or_else(|| {
                format!(
                    "{}|{}|{}",
                    finding.tool,
                    finding.rule.as_deref().unwrap_or_default(),
                    finding.message
                )
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn envelope() -> Value {
        json!({
            "schema": WORKLOAD_RESULT_SCHEMA,
            "kind": "fuzz",
            "workload_id": "parser",
            "status": "fail",
            "findings": [{
                "tool": "fuzz",
                "rule": "parse",
                "severity": "high",
                "message": "parser throws on empty input",
                "fingerprint": "throw:parse:empty"
            }],
            "cases": [
                { "case_id": "c1", "seed": 42, "status": "pass" },
                { "case_id": "c2", "seed": "abc", "artifact_ref": "c2-input", "status": "fail" }
            ],
            "metrics": { "cases_failed": 1, "cases_total": 2 },
            "artifacts": [{ "name": "c2-input", "path": "cases/c2.json", "mime": "application/json" }],
            "payload": { "schema": "homeboy/fuzz-campaign/v1", "id": "campaign" }
        })
    }

    fn parse(value: &Value, schema: Option<&str>) -> Result<WorkloadResult, String> {
        WorkloadResult::parse(&serde_json::to_vec(value).unwrap(), "fuzz", schema)
    }

    #[test]
    fn envelope_parses_with_typed_findings_cases_metrics_and_opaque_payload() {
        let result = parse(&envelope(), Some("homeboy/fuzz-campaign/v1")).expect("valid");

        assert_eq!(result.status, WorkloadStatus::Fail);
        assert_eq!(
            result.findings[0].fingerprint.as_deref(),
            Some("throw:parse:empty")
        );
        assert_eq!(result.findings[0].severity.as_deref(), Some("high"));
        assert_eq!(result.case("c1").unwrap().seed.as_deref(), Some("42"));
        assert_eq!(
            result.case("c2").unwrap().artifact_ref.as_deref(),
            Some("c2-input")
        );
        assert_eq!(result.metrics["cases_failed"], 1.0);
        assert_eq!(result.artifact("c2-input").unwrap().path, "cases/c2.json");
        assert_eq!(result.payload["id"], "campaign");
        assert_eq!(
            result.case_totals(),
            BTreeMap::from([("fail", 1), ("pass", 1)])
        );
    }

    #[test]
    fn minimal_envelope_needs_only_schema_kind_and_status() {
        let result = parse(
            &json!({ "schema": WORKLOAD_RESULT_SCHEMA, "kind": "fuzz", "status": "pass" }),
            None,
        )
        .expect("valid");
        assert!(result.findings.is_empty() && result.cases.is_empty());
        assert!(result.payload.is_null());
        assert!(result.status.is_pass());
    }

    #[test]
    fn envelope_refuses_contract_drift() {
        let mut wrong_schema = envelope();
        wrong_schema["schema"] = json!("homeboy/fuzz-campaign/v1");
        assert!(parse(&wrong_schema, None).unwrap_err().contains("schema"));

        let mut wrong_kind = envelope();
        wrong_kind["kind"] = json!("bench");
        assert!(parse(&wrong_kind, None)
            .unwrap_err()
            .contains("does not match"));

        let mut unknown_field = envelope();
        unknown_field["extra"] = json!(true);
        assert!(parse(&unknown_field, None)
            .unwrap_err()
            .contains("unknown field"));

        let mut duplicate_case = envelope();
        duplicate_case["cases"][1]["case_id"] = json!("c1");
        assert!(parse(&duplicate_case, None)
            .unwrap_err()
            .contains("duplicate case_id"));

        let mut escaping_artifact = envelope();
        escaping_artifact["artifacts"][0]["path"] = json!("../../etc/passwd");
        assert!(parse(&escaping_artifact, None)
            .unwrap_err()
            .contains("relative"));

        let mut bad_status = envelope();
        bad_status["status"] = json!("passed");
        assert!(parse(&bad_status, None).is_err());

        assert!(parse(&envelope(), Some("homeboy/other/v1"))
            .unwrap_err()
            .contains("payload.schema"));
    }

    #[test]
    fn findings_match_by_fingerprint_then_identity() {
        let result = parse(&envelope(), None).unwrap();
        assert_eq!(
            finding_key(&result.findings[0], WorkloadFindingsKey::Fingerprint),
            "throw:parse:empty"
        );
        let mut unfingerprinted = result.findings[0].clone();
        unfingerprinted.fingerprint = None;
        assert_eq!(
            finding_key(&unfingerprinted, WorkloadFindingsKey::Fingerprint),
            "fuzz|parse|parser throws on empty input"
        );
    }
}
