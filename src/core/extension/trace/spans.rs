//! Trace span post-processing over shared observation timeline primitives.

use super::parsing::{TraceEvent, TraceResults, TraceSpanDefinition, TraceSpanResult};
use crate::core::observation::timeline;

pub type TracePhaseMilestone = timeline::ObservationPhaseMilestone;

pub(crate) fn parse_span_definition(raw: &str) -> Result<TraceSpanDefinition, String> {
    timeline::parse_span_definition(raw)
}

pub(crate) fn parse_phase_milestone(raw: &str) -> Result<TracePhaseMilestone, String> {
    timeline::parse_phase_milestone(raw)
}

pub(crate) fn phase_span_definitions(
    phases: &[TracePhaseMilestone],
) -> Result<Vec<TraceSpanDefinition>, String> {
    timeline::phase_span_definitions(phases)
}

pub(crate) fn apply_span_definitions(
    results: &mut TraceResults,
    cli_definitions: &[TraceSpanDefinition],
) {
    let definitions = timeline::merge_span_definitions(&results.span_definitions, cli_definitions);
    if definitions.is_empty() {
        return;
    }
    results.span_definitions = definitions.clone();
    let reporting_timeline = reporting_timeline(&results.timeline);
    results.span_results = summarize_spans(&reporting_timeline, &definitions);
}

pub(crate) fn reporting_timeline(timeline: &[TraceEvent]) -> Vec<TraceEvent> {
    timeline::reporting_timeline(timeline)
}

pub(crate) fn summarize_spans(
    timeline: &[TraceEvent],
    definitions: &[TraceSpanDefinition],
) -> Vec<TraceSpanResult> {
    timeline::summarize_spans(timeline, definitions)
}

pub(crate) fn event_matches_key(event: &TraceEvent, key: &str) -> bool {
    timeline::event_matches_key(event, key)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::trace::parsing::{TraceAssertion, TraceSpanStatus, TraceStatus};

    fn event(t_ms: u64, source: &str, event: &str) -> TraceEvent {
        TraceEvent {
            t_ms,
            source: source.to_string(),
            event: event.to_string(),
            data: Default::default(),
        }
    }

    #[test]
    fn trace_wrappers_preserve_span_summary_contract() {
        let results = summarize_spans(
            &[event(10, "runner", "start"), event(35, "runner", "done")],
            &[TraceSpanDefinition {
                id: "total".to_string(),
                from: "runner.start".to_string(),
                to: "runner.done".to_string(),
            }],
        );

        assert_eq!(results[0].status, TraceSpanStatus::Ok);
        assert_eq!(results[0].duration_ms, Some(25));
    }

    #[test]
    fn test_parse_span_definition() {
        let definition = parse_span_definition("total:runner.start:runner.done").unwrap();

        assert_eq!(definition.id, "total");
        assert_eq!(definition.from, "runner.start");
        assert_eq!(definition.to, "runner.done");
    }

    #[test]
    fn test_parse_phase_milestone() {
        let phase = parse_phase_milestone("ready:runner.ready").unwrap();

        assert_eq!(phase.label, "ready");
        assert_eq!(phase.key, "runner.ready");
    }

    #[test]
    fn test_phase_span_definitions() {
        let definitions = phase_span_definitions(&[
            TracePhaseMilestone {
                label: "start".to_string(),
                key: "runner.start".to_string(),
            },
            TracePhaseMilestone {
                label: "ready".to_string(),
                key: "runner.ready".to_string(),
            },
        ])
        .unwrap();

        assert_eq!(definitions.len(), 2);
        assert_eq!(definitions[0].id, "phase.start_to_ready");
        assert_eq!(definitions[1].id, "phase.total");
    }

    #[test]
    fn test_reporting_timeline() {
        let timeline =
            reporting_timeline(&[event(20, "runner", "ready"), event(0, "runner", "start")]);

        assert_eq!(timeline[0].event, "start");
        assert_eq!(timeline[1].event, "ready");
    }

    #[test]
    fn test_apply_span_definitions() {
        let mut results = TraceResults {
            component_id: "component".to_string(),
            scenario_id: "scenario".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            evidence: None,
            timeline: vec![event(10, "runner", "start"), event(35, "runner", "done")],
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            metrics: Default::default(),
            assertions: Vec::<TraceAssertion>::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
            metrics: Default::default(),
            preview: None,
        };

        apply_span_definitions(
            &mut results,
            &[TraceSpanDefinition {
                id: "total".to_string(),
                from: "runner.start".to_string(),
                to: "runner.done".to_string(),
            }],
        );

        assert_eq!(results.span_definitions.len(), 1);
        assert_eq!(results.span_results[0].duration_ms, Some(25));
    }

    #[test]
    fn test_summarize_spans() {
        trace_wrappers_preserve_span_summary_contract();
    }

    #[test]
    fn test_event_matches_key() {
        let event = event(0, "runner", "ready");

        assert!(event_matches_key(&event, "runner.ready"));
        assert!(!event_matches_key(&event, "runner.start"));
    }
}
