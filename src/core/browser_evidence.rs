use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::core::{Error, Result};

pub const BROWSER_EVIDENCE_SCHEMA_VERSION: u64 = 1;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserPerformanceProfileEnvelope {
    #[serde(default = "default_schema_version")]
    pub schema_version: u64,
    #[serde(default, alias = "url")]
    pub page_url: String,
    #[serde(default)]
    pub summary: Map<String, Value>,
    #[serde(default)]
    pub navigation: Vec<Value>,
    #[serde(default)]
    pub resources: Vec<Value>,
    #[serde(default)]
    pub network: Vec<BrowserNetworkRequestRow>,
    #[serde(default)]
    pub console_messages: Vec<Value>,
    #[serde(default)]
    pub page_errors: Vec<Value>,
    #[serde(default)]
    pub paints: Vec<Value>,
    #[serde(default)]
    pub largest_contentful_paint: Vec<Value>,
    #[serde(default)]
    pub layout_shifts: Vec<Value>,
    #[serde(default)]
    pub long_tasks: Vec<Value>,
    #[serde(default)]
    pub phase_marks: Vec<BrowserPhaseMark>,
    #[serde(default)]
    pub phases: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserNetworkRequestRow {
    #[serde(default, alias = "name")]
    pub url: String,
    #[serde(default)]
    pub method: String,
    #[serde(
        default,
        alias = "resourceType",
        alias = "initiator_type",
        alias = "initiatorType"
    )]
    pub resource_type: String,
    #[serde(
        default,
        alias = "statusCode",
        alias = "status_code",
        alias = "http_status"
    )]
    pub status: Option<u64>,
    #[serde(default)]
    pub failed: bool,
    #[serde(default, alias = "startTime", alias = "start_ms", alias = "startMs")]
    pub start_time_ms: Option<f64>,
    #[serde(default, alias = "durationMs", alias = "duration")]
    pub duration_ms: Option<f64>,
    #[serde(default, alias = "failureText")]
    pub failure_text: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserTimingRow {
    pub url: String,
    #[serde(default, alias = "normalizedUrl")]
    pub normalized_url: String,
    #[serde(default)]
    pub method: Option<String>,
    #[serde(default)]
    pub status: Option<u64>,
    #[serde(default)]
    pub failed: Option<bool>,
    #[serde(default, alias = "startTime")]
    pub start_time: Option<f64>,
    #[serde(default, alias = "ttfbMs")]
    pub ttfb_ms: Option<f64>,
    #[serde(default, alias = "durationMs")]
    pub duration_ms: Option<f64>,
    #[serde(default, alias = "initiatorType")]
    pub initiator_type: Option<String>,
    #[serde(default)]
    pub phase: Option<String>,
    #[serde(default)]
    pub raw: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserPhaseMark {
    pub name: String,
    #[serde(alias = "startTime", alias = "start_ms", alias = "startMs")]
    pub start_time_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserPhaseWindow {
    pub start_time_ms: f64,
    #[serde(default)]
    pub end_time_ms: Option<f64>,
    pub duration_ms: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct BrowserArtifactMetadata {
    pub path: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct BrowserBottleneckRow {
    pub kind: String,
    pub phase: String,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceEvent {
    pub t_ms: f64,
    pub source: String,
    pub event: String,
    #[serde(default)]
    pub data: Map<String, Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceAssertionStatus {
    Pass,
    Fail,
    Skip,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceAssertion {
    pub id: String,
    pub status: TraceAssertionStatus,
    pub message: String,
    #[serde(default)]
    pub data: Option<Value>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TraceTimeline {
    #[serde(default)]
    pub timeline: Vec<TraceEvent>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct TraceAssertions {
    #[serde(default)]
    pub assertions: Vec<TraceAssertion>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TraceEnvelopeStatus {
    Pass,
    Fail,
    Error,
    Skip,
    Unknown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TraceEnvelope {
    pub component_id: String,
    pub scenario_id: String,
    pub status: TraceEnvelopeStatus,
    pub summary: String,
    #[serde(flatten)]
    pub timeline: TraceTimeline,
    #[serde(flatten)]
    pub assertions: TraceAssertions,
    #[serde(default)]
    pub artifacts: Vec<BrowserArtifactMetadata>,
    #[serde(default)]
    pub failure: Option<Value>,
}

pub fn validate_bench_results_payload(payload: &Value) -> Result<()> {
    validate_optional_array::<BrowserPerformanceProfileEnvelope>(payload, "browser_profiles")?;
    validate_optional_array::<BrowserPerformanceProfileEnvelope>(payload, "profiles")?;
    validate_optional_array::<BrowserNetworkRequestRow>(payload, "network")?;
    validate_optional_array::<BrowserArtifactMetadata>(payload, "artifacts")?;
    validate_optional_array::<BrowserBottleneckRow>(payload, "bottlenecks")?;
    validate_optional_array::<BrowserTimingRow>(payload, "timings")?;
    Ok(())
}

pub fn validate_trace_results_payload(payload: &Value) -> Result<()> {
    validate_optional_array::<TraceEvent>(payload, "timeline")?;
    validate_optional_array::<TraceAssertion>(payload, "assertions")?;
    validate_optional_array::<BrowserArtifactMetadata>(payload, "artifacts")?;
    validate_optional_array::<TraceEnvelope>(payload, "traces")?;
    Ok(())
}

fn validate_optional_array<T>(payload: &Value, field: &str) -> Result<()>
where
    T: for<'de> Deserialize<'de>,
{
    let Some(value) = payload.get(field) else {
        return Ok(());
    };
    let Some(items) = value.as_array() else {
        return Err(Error::validation_invalid_argument(
            "browser_evidence",
            format!("browser evidence field `{field}` must be a JSON array"),
            None,
            None,
        ));
    };

    for (index, item) in items.iter().enumerate() {
        serde_json::from_value::<T>(item.clone()).map_err(|err| {
            Error::validation_invalid_argument(
                "browser_evidence",
                format!("browser evidence field `{field}` item {index} does not match the core schema: {err}"),
                None,
                None,
            )
        })?;
    }

    Ok(())
}

fn default_schema_version() -> u64 {
    BROWSER_EVIDENCE_SCHEMA_VERSION
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn validates_representative_bench_payload_shapes() {
        validate_bench_results_payload(&json!({
            "browser_profiles": [{
                "schema_version": 1,
                "page_url": "https://example.test/",
                "summary": { "ready_ms": 425.5 },
                "network": [{
                    "url": "https://example.test/app.js",
                    "method": "GET",
                    "resource_type": "script",
                    "status": 200,
                    "failed": false,
                    "start_time_ms": 12.25,
                    "duration_ms": 40.5
                }],
                "phase_marks": [{ "name": "boot", "start_time_ms": 0 }],
                "phases": { "boot": { "start_time_ms": 0, "end_time_ms": 42, "duration_ms": 42 } }
            }],
            "timings": [{
                "url": "https://example.test/app.js",
                "normalizedUrl": "/app.js",
                "method": "GET",
                "status": 200,
                "failed": false,
                "startTime": 12.25,
                "ttfbMs": 18.5,
                "durationMs": 40.5,
                "initiatorType": "script",
                "phase": "boot",
                "raw": { "source": "resource" }
            }],
            "artifacts": [{ "path": "browser/profile.json", "kind": "profile", "label": "profile" }],
            "bottlenecks": [{ "kind": "network", "phase": "boot", "message": "Slow script" }]
        })).unwrap();
    }

    #[test]
    fn validates_representative_trace_payload_shapes() {
        validate_trace_results_payload(&json!({
            "timeline": [{
                "t_ms": 1.5,
                "source": "scenario",
                "event": "loaded",
                "data": { "selector": "main" }
            }],
            "assertions": [{
                "id": "ready",
                "status": "pass",
                "message": "Page became ready"
            }],
            "artifacts": [{ "path": "trace.zip", "kind": "trace", "label": "trace" }],
            "traces": [{
                "component_id": "component",
                "scenario_id": "scenario",
                "status": "pass",
                "summary": "Trace completed",
                "timeline": [{ "t_ms": 1.5, "source": "scenario", "event": "loaded", "data": {} }],
                "assertions": [{ "id": "ready", "status": "pass", "message": "Page became ready" }],
                "artifacts": [{ "path": "trace.zip" }]
            }]
        }))
        .unwrap();
    }

    #[test]
    fn rejects_invalid_known_browser_evidence_fields() {
        let err = validate_trace_results_payload(&json!({
            "assertions": [{ "id": "ready", "status": "maybe", "message": "invalid" }]
        }))
        .expect_err("invalid assertion status should fail");

        assert!(err.to_string().contains("assertions"));
        assert!(err.to_string().contains("core schema"));
    }
}
