use super::*;

/// Best-effort compatibility ingestion for a declared producer sidecar. Missing
/// or malformed sidecars never replace the command's authoritative gate result.
pub(crate) fn ingest_gate_diagnostic_sidecars(
    cwd: &Path,
    mappings: &[AgentTaskGateDiagnosticSidecarMapping],
    report: &mut AgentTaskGateReport,
    full_evidence_ref: &str,
) {
    let Some(evidence) = report.failure_evidence.as_mut() else {
        return;
    };
    for mapping in mappings {
        if evidence.diagnostics.len() >= MAX_GATE_DIAGNOSTICS
            || mapping.target_schema != AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA
            || mapping.source_schema.trim().is_empty()
            || mapping.producer.id.trim().is_empty()
            || mapping.producer.schema.trim().is_empty()
        {
            continue;
        }
        let Ok(path) = homeboy_core::resolve_contained_local_path(
            cwd,
            &mapping.path,
            "gate_diagnostic_sidecars.path",
        ) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_GATE_DIAGNOSTIC_SIDECAR_BYTES {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(records) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else {
            continue;
        };
        for value in records {
            if evidence.diagnostics.len() >= MAX_GATE_DIAGNOSTICS {
                break;
            }
            let Some(record) = normalize_gate_diagnostic_record(value, mapping, full_evidence_ref)
            else {
                continue;
            };
            if !evidence.diagnostics.iter().any(|existing| {
                existing.identity == record.identity && existing.producer == record.producer
            }) {
                evidence.diagnostics.push(record);
            }
        }
    }
}

pub(super) fn normalize_gate_diagnostic_record(
    value: serde_json::Value,
    mapping: &AgentTaskGateDiagnosticSidecarMapping,
    full_evidence_ref: &str,
) -> Option<AgentTaskGateDiagnosticRecord> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != mapping.source_schema {
        return None;
    }
    let identity = bounded_diagnostic_text(object.get("identity")?.as_str()?);
    let summary = bounded_diagnostic_text(object.get("summary")?.as_str()?);
    if identity.is_empty() || summary.is_empty() {
        return None;
    }
    let source_location = object
        .get("source_location")
        .and_then(serde_json::Value::as_str)
        .map(bounded_diagnostic_text)
        .filter(|value| !value.is_empty());
    let suggested_actions = object
        .get("suggested_actions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(bounded_diagnostic_text)
        .filter(|value| !value.is_empty())
        .take(MAX_GATE_DIAGNOSTIC_ACTIONS)
        .collect();
    Some(AgentTaskGateDiagnosticRecord {
        schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
        identity,
        summary,
        source_location,
        suggested_actions,
        producer: mapping.producer.clone(),
        full_evidence_ref: full_evidence_ref.to_string(),
    })
}

pub(super) fn bounded_diagnostic_text(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        if result.len() + character.len_utf8() > MAX_GATE_DIAGNOSTIC_FIELD_BYTES {
            break;
        }
        result.push(character);
    }
    result
}

pub(super) fn gate_diagnostic_record_schema() -> String {
    AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string()
}
