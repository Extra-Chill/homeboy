//! Trace runner JSON output parsing.

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::core::error::{Error, Result};
use crate::core::observation::timeline::{
    ObservationEvent, ObservationSpanDefinition, ObservationSpanResult, ObservationSpanStatus,
};
use crate::core::rig::RigStateSnapshot;

pub type TraceEvent = ObservationEvent;
pub type TraceSpanDefinition = ObservationSpanDefinition;
pub type TraceSpanResult = ObservationSpanResult;
pub type TraceSpanStatus = ObservationSpanStatus;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TraceStatus {
    Pass,
    Fail,
    Error,
}

impl TraceStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TraceStatus::Pass => "pass",
            TraceStatus::Fail => "fail",
            TraceStatus::Error => "error",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum TraceAssertionStatus {
    Pass,
    Fail,
    Error,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceResults {
    pub component_id: String,
    pub scenario_id: String,
    pub status: TraceStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rig: Option<RigStateSnapshot>,
    #[serde(default)]
    pub timeline: Vec<TraceEvent>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_definitions: Vec<TraceSpanDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub span_results: Vec<TraceSpanResult>,
    #[serde(default)]
    pub assertions: Vec<TraceAssertion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub temporal_assertions: Vec<TraceTemporalAssertionDefinition>,
    #[serde(default)]
    pub artifacts: Vec<TraceArtifact>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dependencies: Vec<TraceDependencyProvenance>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceDependencyProvenance {
    pub id: String,
    pub kind: String,
    pub source: String,
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub r#ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package_marker: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plugin_file: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceAssertion {
    pub id: String,
    pub status: TraceAssertionStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum TraceTemporalAssertionDefinition {
    Count {
        id: String,
        events: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        min: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max: Option<usize>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    ForbiddenEvent {
        id: String,
        pattern: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    MaxConcurrent {
        id: String,
        track: Vec<String>,
        max: usize,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    NoOverlap {
        id: String,
        events: Vec<String>,
        by: String,
        window_ms: u64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    Ordering {
        id: String,
        before: String,
        after: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        within_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        by: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    LatencyBound {
        id: String,
        from: String,
        to: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        p50_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        p95_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        p99_ms: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
    RequiredSequence {
        id: String,
        sequence: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        message: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceArtifact {
    pub label: String,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct TraceList {
    pub component_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scenario_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<TraceStatus>,
    #[serde(default)]
    pub scenarios: Vec<TraceScenario>,
    #[serde(default)]
    pub timeline: Vec<TraceEvent>,
    #[serde(default)]
    pub assertions: Vec<TraceAssertion>,
    #[serde(default)]
    pub artifacts: Vec<TraceArtifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TraceScenario {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

pub fn parse_trace_results_file(path: &Path) -> Result<TraceResults> {
    let content = std::fs::read_to_string(path).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to read trace results file {}: {}",
                path.display(),
                e
            ),
            Some("trace.parsing.read".to_string()),
        )
    })?;
    parse_trace_results_str(&content)
}

fn parse_trace_results_str(raw: &str) -> Result<TraceResults> {
    serde_json::from_str(raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse trace results JSON: {}", e),
            Some("trace.parsing.deserialize".to_string()),
        )
    })
}

pub fn parse_trace_list_str(raw: &str) -> Result<TraceList> {
    serde_json::from_str(raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse trace list JSON: {}", e),
            Some("trace.parsing.list.deserialize".to_string()),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_trace_results_str() {
        let parsed = parse_trace_results_str(
            r#"{
                "component_id":"studio",
                "scenario_id":"close-window-running-site",
                "status":"fail",
                "summary":"Window reopened after close",
                "timeline":[{"t_ms":0,"source":"desktop","event":"window.closed","data":{"id":1}}],
                "span_definitions":[{"id":"close_to_assertion","from":"desktop.window.closed","to":"assertion.checked"}],
                "assertions":[{"id":"no-window-reopen","status":"fail","message":"Window reopened"}],
                "artifacts":[{"label":"main log","path":"artifacts/main.log"}]
            }"#,
        )
        .expect("minimal trace envelope should parse");

        assert_eq!(parsed.component_id, "studio");
        assert_eq!(parsed.status, TraceStatus::Fail);
        assert_eq!(parsed.timeline[0].t_ms, 0);
        assert_eq!(parsed.span_definitions[0].id, "close_to_assertion");
        assert_eq!(parsed.assertions[0].id, "no-window-reopen");
        assert_eq!(parsed.artifacts[0].path, "artifacts/main.log");
    }

    #[test]
    fn test_parse_trace_results_file() {
        let temp = tempfile::tempdir().expect("tempdir should be created");
        let path = temp.path().join("trace-results.json");
        std::fs::write(
            &path,
            r#"{"component_id":"studio","scenario_id":"x","status":"pass","timeline":[],"span_results":[],"assertions":[],"artifacts":[]}"#,
        )
        .expect("trace results should be written");

        let parsed = parse_trace_results_file(&path).expect("trace results file should parse");
        assert_eq!(parsed.component_id, "studio");
        assert_eq!(parsed.status, TraceStatus::Pass);
    }

    #[test]
    fn trace_json_parser_rejects_invalid_status() {
        let err = parse_trace_results_str(
            r#"{"component_id":"studio","scenario_id":"x","status":"unknown","timeline":[],"assertions":[],"artifacts":[]}"#,
        )
        .unwrap_err();

        assert!(!err.message.is_empty());
    }

    #[test]
    fn trace_json_parser_rejects_malformed_timeline_shape() {
        let err = parse_trace_results_str(
            r#"{"component_id":"studio","scenario_id":"x","status":"pass","timeline":[{"source":"desktop","event":"x"}],"assertions":[],"artifacts":[]}"#,
        )
        .unwrap_err();

        assert!(!err.message.is_empty());
    }

    #[test]
    fn test_parse_trace_list_str() {
        let parsed = parse_trace_list_str(
            r#"{"component_id":"studio","scenarios":[{"id":"close-window","summary":"Close window lifecycle"}]}"#,
        )
        .expect("list envelope should parse");

        assert_eq!(parsed.scenarios[0].id, "close-window");
    }

    #[test]
    fn trace_list_parser_accepts_trace_shaped_inventory_envelope() {
        let parsed = parse_trace_list_str(
            r#"{
                "component_id":"studio",
                "scenario_id":"__list__",
                "status":"pass",
                "scenarios":[{"id":"close-window-running-site","source":"fixtures/close-window.trace.js"}],
                "timeline":[],
                "assertions":[],
                "artifacts":[]
            }"#,
        )
        .expect("trace-shaped list envelope should parse");

        assert_eq!(parsed.component_id, "studio");
        assert_eq!(parsed.scenario_id.as_deref(), Some("__list__"));
        assert_eq!(parsed.status, Some(TraceStatus::Pass));
        assert_eq!(parsed.scenarios[0].id, "close-window-running-site");
        assert_eq!(
            parsed.scenarios[0].source.as_deref(),
            Some("fixtures/close-window.trace.js")
        );
    }
}
