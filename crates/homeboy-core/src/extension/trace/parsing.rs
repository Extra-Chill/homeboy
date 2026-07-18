//! Trace runner JSON output parsing.

use std::collections::BTreeMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::observation::timeline::{
    ObservationEvent, ObservationSpanDefinition, ObservationSpanResult, ObservationSpanStatus,
};
use crate::structured_sidecar;
use homeboy_lifecycle_contract::RigStateSnapshot;

use super::preview::TracePreviewMetadata;
pub use homeboy_extension_contract::trace_parsing::{
    TraceArtifact, TraceAssertion, TraceAssertionStatus, TraceCanonicalCheck,
    TraceComponentsProvenance, TraceDependencyProvenance, TraceEvent, TraceEvidenceMetadata,
    TraceGitProvenance, TraceList, TraceRuntimeAssetProvenance, TraceScenario, TraceSpanDefinition,
    TraceSpanResult, TraceSpanStatus, TraceStatus, TraceTemporalAssertionDefinition,
    TraceToolchainProvenance,
};
pub use homeboy_extension_contract::trace_results::TraceResults;

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
    let mut deserializer = serde_json::Deserializer::from_str(raw);
    let parsed = serde_path_to_error::deserialize(&mut deserializer).map_err(|e| {
        let path = e.path().to_string();
        let path = if path == "." { "$".to_string() } else { path };
        Error::internal_json(
            format!(
                "Failed to parse trace results JSON at `{}`: {}",
                path,
                e.inner()
            ),
            Some("trace.parsing.deserialize".to_string()),
        )
    })?;

    let payload = serde_json::to_value(&parsed).map_err(|e| {
        Error::internal_json(
            format!("Failed to validate trace results JSON: {}", e),
            Some("trace.parsing.deserialize".to_string()),
        )
    })?;
    structured_sidecar::validate_payload("trace.results", &payload)?;

    Ok(parsed)
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
                "metrics":{"assertion_count":1,"producer":"custom-provider"},
                "artifacts":[{"label":"main log","path":"artifacts/main.log","kind":"log"}]
            }"#,
        )
        .expect("minimal trace envelope should parse");

        assert_eq!(parsed.component_id, "studio");
        assert_eq!(parsed.status, TraceStatus::Fail);
        assert_eq!(parsed.timeline[0].t_ms, 0);
        assert_eq!(parsed.span_definitions[0].id, "close_to_assertion");
        assert_eq!(parsed.assertions[0].id, "no-window-reopen");
        assert_eq!(parsed.metrics["assertion_count"], serde_json::json!(1));
        assert_eq!(parsed.artifacts[0].path, "artifacts/main.log");
        assert_eq!(parsed.artifacts[0].kind.as_deref(), Some("log"));
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

        let detail = err.details["error"].as_str().expect("JSON error detail");
        assert!(detail.contains("`status`"), "{}", detail);
    }

    #[test]
    fn trace_json_parser_rejects_malformed_timeline_shape() {
        let err = parse_trace_results_str(
            r#"{"component_id":"studio","scenario_id":"x","status":"pass","timeline":[{"source":"desktop","event":"x"}],"assertions":[],"artifacts":[]}"#,
        )
        .unwrap_err();

        let detail = err.details["error"].as_str().expect("JSON error detail");
        assert!(detail.contains("`timeline[0]`"), "{}", detail);
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
