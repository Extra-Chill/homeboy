//! Trace span post-processing over shared observation timeline primitives.

use super::parsing::{TraceEvent, TraceResults, TraceSpanDefinition, TraceSpanResult};
use homeboy_core::observation::timeline;

pub type TracePhaseMilestone = timeline::ObservationPhaseMilestone;

pub fn parse_span_definition(raw: &str) -> Result<TraceSpanDefinition, String> {
    timeline::parse_span_definition(raw)
}

pub fn parse_phase_milestone(raw: &str) -> Result<TracePhaseMilestone, String> {
    timeline::parse_phase_milestone(raw)
}

pub fn phase_span_definitions(
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
    use crate::extension::trace::parsing::{TraceAssertion, TraceStatus};

    fn event(t_ms: u64, source: &str, event: &str) -> TraceEvent {
        TraceEvent {
            t_ms,
            source: source.to_string(),
            event: event.to_string(),
            data: Default::default(),
        }
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
            assertions: Vec::<TraceAssertion>::new(),
            temporal_assertions: Vec::new(),
            artifacts: Vec::new(),
            metrics: Default::default(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
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
        assert_eq!(results.span_results.len(), 1);
        assert_eq!(results.span_results[0].duration_ms, Some(25));
    }
}
