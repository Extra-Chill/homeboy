//! Project recorded executor totals without instrumenting provider runtimes.
//!
//! Aggregates contain durations, not turn timestamps. Child spans therefore
//! occupy synthetic consecutive intervals inside the recorded command. Do not
//! invent a timeline when totals exceed that command's wall time.
use homeboy_engine_primitives::phase_timing::{PhaseSpan, PhaseTimingReport};
use serde_json::Value;

pub(super) fn project_provider_spans(report: &mut PhaseTimingReport, aggregate: &Value) {
    let Some(outcomes) = aggregate["outcomes"].as_array() else {
        return;
    };
    let commands = report
        .spans
        .iter()
        .filter(|span| span.id == "provider.command")
        .cloned()
        .collect::<Vec<_>>();
    // An aggregate has one result per task, not one per command retry. Only a
    // singleton pair is unambiguous; ordering alone cannot match parallel tasks.
    if commands.len() != 1 || outcomes.len() != 1 {
        return;
    }
    for (command, outcome) in commands.iter().zip(outcomes) {
        let metadata = &outcome["metadata"];
        let mut durations = Vec::new();
        if let Some(ms) = milliseconds(&metadata["model_ms"]) {
            durations.push(("provider.model".to_string(), ms));
        }
        if let Some(tools) = metadata["tool_ms_by_name"].as_object() {
            for (name, value) in tools {
                let id = format!("provider.tool.{name}");
                if id.len() <= 128 {
                    if let Some(ms) = milliseconds(value) {
                        durations.push((id, ms));
                    }
                }
            }
        }
        let total: f64 = durations.iter().map(|(_, ms)| ms).sum();
        if !total.is_finite() || command.duration_ms.is_none_or(|wall| total > wall) {
            continue;
        }
        let Ok(start) = chrono::DateTime::parse_from_rfc3339(&command.started_at) else {
            continue;
        };
        let mut offset = 0.0;
        for (id, ms) in durations {
            let started_at =
                (start + chrono::Duration::microseconds((offset * 1000.0) as i64)).to_rfc3339();
            offset += ms;
            // Read-time projection is repeatable, including callers that pass
            // an already projected report.
            if !report
                .spans
                .iter()
                .any(|span| span.id == id && span.started_at == started_at)
            {
                report.spans.push(PhaseSpan {
                    id,
                    started_at,
                    duration_ms: Some(ms),
                    status: command.status,
                    homeboy_version: command.homeboy_version.clone(),
                });
            }
        }
    }
}

fn milliseconds(value: &Value) -> Option<f64> {
    value.as_f64().filter(|ms| ms.is_finite() && *ms > 0.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_engine_primitives::phase_timing::PhaseStatus;

    #[test]
    fn recorded_lab_aggregate_projects_inside_command_without_double_counting() {
        let aggregate: Value = serde_json::from_str(include_str!(
            "../../../homeboy-agents/src/agent_task_service/cook_tests/fixtures/incomplete_no_patch.json"
        )).unwrap();
        let metadata = &aggregate["outcomes"][0]["metadata"];
        let model = metadata["model_ms"].as_f64().unwrap();
        let tools: f64 = metadata["tool_ms_by_name"]
            .as_object()
            .unwrap()
            .values()
            .map(|value| value.as_f64().unwrap())
            .sum();
        let mut report = PhaseTimingReport {
            spans: vec![PhaseSpan {
                id: "provider.command".into(),
                started_at: "2026-10-30T00:00:00+00:00".into(),
                duration_ms: Some(model + tools + 1000.0),
                status: PhaseStatus::Failed,
                homeboy_version: "recorded-fixture".into(),
            }],
        };
        project_provider_spans(&mut report, &aggregate);
        assert!(report
            .spans
            .iter()
            .any(|span| span.id == "provider.model" && span.duration_ms == Some(model)));
        assert!(report
            .spans
            .iter()
            .any(|span| span.id == "provider.tool.bash"));
        let parent_end = chrono::DateTime::parse_from_rfc3339(&report.spans[0].started_at).unwrap()
            + chrono::Duration::microseconds(((model + tools + 1000.0) * 1000.0) as i64);
        for pair in report.spans[1..].windows(2) {
            let end = chrono::DateTime::parse_from_rfc3339(&pair[0].started_at).unwrap()
                + chrono::Duration::microseconds((pair[0].duration_ms.unwrap() * 1000.0) as i64);
            let next = chrono::DateTime::parse_from_rfc3339(&pair[1].started_at).unwrap();
            assert!(end <= next && next <= parent_end);
        }
        // Exercise the canonical read path and hotspot attribution as well.
        let dir = tempfile::tempdir().unwrap();
        let store =
            super::super::ObservationStore::open_initialized_at(dir.path().join("o.sqlite"))
                .unwrap();
        let run = store
            .start_run(
                super::super::NewRunRecord::builder("agent-task")
                    .metadata(serde_json::json!({"agent_task_aggregate": aggregate}))
                    .build(),
            )
            .unwrap();
        store
            .record_phase_spans(
                &run.id,
                "command",
                &PhaseTimingReport {
                    spans: vec![report.spans[0].clone()],
                },
            )
            .unwrap();
        assert_eq!(
            store.phase_spans(&run.id).unwrap().spans.len(),
            report.spans.len()
        );
        let hotspots = crate::performance_hotspots::phase_hotspots(
            &store,
            &crate::performance_hotspots::PhaseHotspotQuery {
                last: 1,
                ..Default::default()
            },
        )
        .unwrap();
        let command = hotspots
            .phases
            .iter()
            .find(|phase| phase.phase == "provider.command")
            .unwrap();
        assert!((command.self_ms_total - 1000.0).abs() < 0.01);
        assert!(hotspots
            .phases
            .iter()
            .any(|phase| phase.phase == "provider.model" && phase.self_ms_total == model));

        let count = report.spans.len();
        project_provider_spans(&mut report, &aggregate);
        assert_eq!(report.spans.len(), count);
        let mut ambiguous = report.clone();
        ambiguous.spans = vec![report.spans[0].clone(); 2];
        project_provider_spans(&mut ambiguous, &aggregate);
        assert_eq!(ambiguous.spans.len(), 2);
    }
}
