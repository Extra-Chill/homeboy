//! Temporal trace assertion evaluation over timeline events.

use crate::core::extension::bench::distribution::percentile;
use serde_json::json;

use super::parsing::{
    TraceAssertion, TraceAssertionStatus, TraceResults, TraceStatus,
    TraceTemporalAssertionDefinition,
};
use super::spans::{event_matches_key, reporting_timeline};

pub(crate) fn apply_temporal_assertions(results: &mut TraceResults) -> bool {
    if results.temporal_assertions.is_empty() {
        return false;
    }

    let definitions = std::mem::take(&mut results.temporal_assertions);
    let timeline = reporting_timeline(&results.timeline);
    let mut has_failure = false;
    for definition in &definitions {
        let assertion = evaluate_temporal_assertion(definition, &timeline);
        if assertion.status != TraceAssertionStatus::Pass {
            has_failure = true;
        }
        results.assertions.push(assertion);
    }

    if has_failure {
        results.status = TraceStatus::Fail;
    }
    has_failure
}

fn evaluate_temporal_assertion(
    definition: &TraceTemporalAssertionDefinition,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    match definition {
        TraceTemporalAssertionDefinition::Count {
            id,
            events,
            min,
            max,
            message,
        } => evaluate_count(id, events, *min, *max, message.as_deref(), timeline),
        TraceTemporalAssertionDefinition::ForbiddenEvent {
            id,
            pattern,
            message,
        } => evaluate_forbidden_event(id, pattern, message.as_deref(), timeline),
        TraceTemporalAssertionDefinition::MaxConcurrent {
            id,
            track,
            max,
            message,
        } => evaluate_max_concurrent(id, track, *max, message.as_deref(), timeline),
        TraceTemporalAssertionDefinition::NoOverlap {
            id,
            events,
            by,
            window_ms,
            message,
        } => evaluate_no_overlap(id, events, by, *window_ms, message.as_deref(), timeline),
        TraceTemporalAssertionDefinition::Ordering {
            id,
            before,
            after,
            within_ms,
            by,
            message,
        } => evaluate_ordering(
            id,
            before,
            after,
            *within_ms,
            by.as_deref(),
            message.as_deref(),
            timeline,
        ),
        TraceTemporalAssertionDefinition::LatencyBound {
            id,
            from,
            to,
            p50_ms,
            p95_ms,
            p99_ms,
            message,
        } => evaluate_latency_bound(
            id,
            from,
            to,
            *p50_ms,
            *p95_ms,
            *p99_ms,
            message.as_deref(),
            timeline,
        ),
        TraceTemporalAssertionDefinition::RequiredSequence {
            id,
            sequence,
            message,
        } => evaluate_required_sequence(id, sequence, message.as_deref(), timeline),
    }
}

fn evaluate_count(
    id: &str,
    events: &[String],
    min: Option<usize>,
    max: Option<usize>,
    message: Option<&str>,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    let matches = timeline
        .iter()
        .filter(|event| events.iter().any(|key| event_matches_key(event, key)))
        .collect::<Vec<_>>();
    let actual = matches.len();
    let passed = min.is_none_or(|value| actual >= value) && max.is_none_or(|value| actual <= value);

    TraceAssertion {
        id: id.to_string(),
        status: if passed {
            TraceAssertionStatus::Pass
        } else {
            TraceAssertionStatus::Fail
        },
        message: Some(
            message
                .map(str::to_string)
                .unwrap_or_else(|| count_message(events, min, max, actual, passed)),
        ),
        details: Some(json!({
            "kind": "count",
            "events": events,
            "min": min,
            "max": max,
            "actual": actual,
            "matches": event_details(&matches),
        })),
    }
}

fn evaluate_forbidden_event(
    id: &str,
    pattern: &str,
    message: Option<&str>,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    let matches = timeline
        .iter()
        .filter(|event| event_matches_key(event, pattern))
        .collect::<Vec<_>>();
    let passed = matches.is_empty();

    TraceAssertion {
        id: id.to_string(),
        status: if passed {
            TraceAssertionStatus::Pass
        } else {
            TraceAssertionStatus::Fail
        },
        message: Some(
            message
                .map(str::to_string)
                .unwrap_or_else(|| forbidden_event_message(pattern, matches.len(), passed)),
        ),
        details: Some(json!({
            "kind": "forbidden-event",
            "pattern": pattern,
            "actual": matches.len(),
            "matches": event_details(&matches),
        })),
    }
}

fn evaluate_max_concurrent(
    id: &str,
    track: &[String],
    max: usize,
    message: Option<&str>,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    if track.len() != 2 {
        return TraceAssertion {
            id: id.to_string(),
            status: TraceAssertionStatus::Error,
            message: Some(
                "max-concurrent requires exactly two track events: start and end".to_string(),
            ),
            details: Some(json!({
                "kind": "max-concurrent",
                "track": track,
                "max": max,
            })),
        };
    }

    let mut changes = timeline
        .iter()
        .filter_map(|event| {
            if event_matches_key(event, &track[0]) {
                Some((event.t_ms, 1_i64, event))
            } else if event_matches_key(event, &track[1]) {
                Some((event.t_ms, -1_i64, event))
            } else {
                None
            }
        })
        .collect::<Vec<_>>();
    changes.sort_by_key(|(t_ms, delta, _)| (*t_ms, *delta));

    let mut current = 0_i64;
    let mut max_observed = 0_i64;
    let mut max_t_ms = None;
    for (t_ms, delta, _) in &changes {
        current = (current + delta).max(0);
        if current > max_observed {
            max_observed = current;
            max_t_ms = Some(*t_ms);
        }
    }

    let passed = max_observed <= max as i64;
    TraceAssertion {
        id: id.to_string(),
        status: if passed {
            TraceAssertionStatus::Pass
        } else {
            TraceAssertionStatus::Fail
        },
        message: Some(
            message
                .map(str::to_string)
                .unwrap_or_else(|| max_concurrent_message(&track[0], max, max_observed, passed)),
        ),
        details: Some(json!({
            "kind": "max-concurrent",
            "track": track,
            "max": max,
            "max_observed": max_observed,
            "at_t_ms": max_t_ms,
            "events": event_details(&changes.iter().map(|(_, _, event)| *event).collect::<Vec<_>>()),
        })),
    }
}

fn evaluate_no_overlap(
    id: &str,
    events: &[String],
    by: &str,
    window_ms: u64,
    message: Option<&str>,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    let matches = timeline
        .iter()
        .filter(|event| events.iter().any(|key| event_matches_key(event, key)))
        .collect::<Vec<_>>();
    let mut overlaps = Vec::new();
    for (index, first) in matches.iter().enumerate() {
        let Some(first_group) = event_data_string(first, by) else {
            continue;
        };
        for second in matches.iter().skip(index + 1) {
            if second.t_ms.saturating_sub(first.t_ms) > window_ms {
                break;
            }
            let Some(second_group) = event_data_string(second, by) else {
                continue;
            };
            if first_group != second_group {
                overlaps.push(json!({
                    "delta_ms": second.t_ms.saturating_sub(first.t_ms),
                    "first": event_detail(first),
                    "second": event_detail(second),
                    "first_group": first_group,
                    "second_group": second_group,
                }));
            }
        }
    }

    let passed = overlaps.is_empty();
    TraceAssertion {
        id: id.to_string(),
        status: if passed {
            TraceAssertionStatus::Pass
        } else {
            TraceAssertionStatus::Fail
        },
        message: Some(
            message.map(str::to_string).unwrap_or_else(|| {
                no_overlap_message(events, by, window_ms, overlaps.len(), passed)
            }),
        ),
        details: Some(json!({
            "kind": "no-overlap",
            "events": events,
            "by": by,
            "window_ms": window_ms,
            "overlap_count": overlaps.len(),
            "overlaps": overlaps,
        })),
    }
}

fn evaluate_ordering(
    id: &str,
    before: &str,
    after: &str,
    within_ms: Option<u64>,
    by: Option<&str>,
    message: Option<&str>,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    let before_events = timeline
        .iter()
        .filter(|event| event_matches_key(event, before))
        .collect::<Vec<_>>();
    let after_events = timeline
        .iter()
        .filter(|event| event_matches_key(event, after))
        .collect::<Vec<_>>();
    let mut violations = Vec::new();

    for before_event in &before_events {
        let matched = after_events.iter().any(|after_event| {
            after_event.t_ms >= before_event.t_ms
                && within_ms.is_none_or(|limit| after_event.t_ms - before_event.t_ms <= limit)
                && same_group(before_event, after_event, by)
        });
        if !matched {
            violations.push(event_detail(before_event));
        }
    }

    let passed = violations.is_empty();
    TraceAssertion {
        id: id.to_string(),
        status: if passed {
            TraceAssertionStatus::Pass
        } else {
            TraceAssertionStatus::Fail
        },
        message: Some(
            message
                .map(str::to_string)
                .unwrap_or_else(|| ordering_message(before, after, violations.len(), passed)),
        ),
        details: Some(json!({
            "kind": "ordering",
            "before": before,
            "after": after,
            "within_ms": within_ms,
            "by": by,
            "checked": before_events.len(),
            "violation_count": violations.len(),
            "violations": violations,
        })),
    }
}

fn evaluate_latency_bound(
    id: &str,
    from: &str,
    to: &str,
    p50_ms: Option<u64>,
    p95_ms: Option<u64>,
    p99_ms: Option<u64>,
    message: Option<&str>,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    let durations = paired_durations(timeline, from, to);
    let samples = durations
        .iter()
        .map(|duration| *duration as f64)
        .collect::<Vec<_>>();
    let actual_p50 = (!samples.is_empty()).then(|| percentile(&samples, 50.0));
    let actual_p95 = (!samples.is_empty()).then(|| percentile(&samples, 95.0));
    let actual_p99 = (!samples.is_empty()).then(|| percentile(&samples, 99.0));
    let passed = !samples.is_empty()
        && p50_ms.is_none_or(|limit| actual_p50.is_some_and(|actual| actual <= limit as f64))
        && p95_ms.is_none_or(|limit| actual_p95.is_some_and(|actual| actual <= limit as f64))
        && p99_ms.is_none_or(|limit| actual_p99.is_some_and(|actual| actual <= limit as f64));

    TraceAssertion {
        id: id.to_string(),
        status: if passed {
            TraceAssertionStatus::Pass
        } else {
            TraceAssertionStatus::Fail
        },
        message: Some(
            message
                .map(str::to_string)
                .unwrap_or_else(|| latency_bound_message(from, to, durations.len(), passed)),
        ),
        details: Some(json!({
            "kind": "latency-bound",
            "from": from,
            "to": to,
            "p50_ms": p50_ms,
            "p95_ms": p95_ms,
            "p99_ms": p99_ms,
            "actual_p50_ms": actual_p50,
            "actual_p95_ms": actual_p95,
            "actual_p99_ms": actual_p99,
            "sample_count": durations.len(),
            "durations_ms": durations,
        })),
    }
}

fn evaluate_required_sequence(
    id: &str,
    sequence: &[String],
    message: Option<&str>,
    timeline: &[super::parsing::TraceEvent],
) -> TraceAssertion {
    let mut position = 0;
    let mut matched = Vec::new();
    for event in timeline {
        if sequence
            .get(position)
            .is_some_and(|key| event_matches_key(event, key))
        {
            matched.push(event_detail(event));
            position += 1;
            if position == sequence.len() {
                break;
            }
        }
    }
    let passed = position == sequence.len();

    TraceAssertion {
        id: id.to_string(),
        status: if passed {
            TraceAssertionStatus::Pass
        } else {
            TraceAssertionStatus::Fail
        },
        message: Some(
            message
                .map(str::to_string)
                .unwrap_or_else(|| required_sequence_message(sequence, position, passed)),
        ),
        details: Some(json!({
            "kind": "required-sequence",
            "sequence": sequence,
            "matched_count": position,
            "missing": sequence.get(position),
            "matches": matched,
        })),
    }
}

fn count_message(
    events: &[String],
    min: Option<usize>,
    max: Option<usize>,
    actual: usize,
    passed: bool,
) -> String {
    if passed {
        return format!(
            "event count for `{}` satisfied: observed {actual}",
            events.join(", ")
        );
    }

    match (min, max) {
        (Some(min), Some(max)) => format!(
            "event count for `{}` outside range {min}..={max}: observed {actual}",
            events.join(", ")
        ),
        (Some(min), None) => format!(
            "event count for `{}` below minimum {min}: observed {actual}",
            events.join(", ")
        ),
        (None, Some(max)) => format!(
            "event count for `{}` exceeded maximum {max}: observed {actual}",
            events.join(", ")
        ),
        (None, None) => format!("event count for `{}` observed {actual}", events.join(", ")),
    }
}

fn forbidden_event_message(pattern: &str, actual: usize, passed: bool) -> String {
    if passed {
        format!("forbidden event `{pattern}` did not occur")
    } else {
        format!("forbidden event `{pattern}` occurred {actual} time(s)")
    }
}

fn max_concurrent_message(start: &str, max: usize, max_observed: i64, passed: bool) -> String {
    if passed {
        format!("max concurrency for `{start}` stayed within {max}: observed {max_observed}")
    } else {
        format!("max concurrency for `{start}` exceeded {max}: observed {max_observed}")
    }
}

fn no_overlap_message(
    events: &[String],
    by: &str,
    window_ms: u64,
    overlap_count: usize,
    passed: bool,
) -> String {
    if passed {
        format!(
            "events `{}` did not overlap across `{by}` within {window_ms}ms",
            events.join(", ")
        )
    } else {
        format!(
            "events `{}` overlapped across `{by}` within {window_ms}ms {overlap_count} time(s)",
            events.join(", ")
        )
    }
}

fn ordering_message(before: &str, after: &str, violations: usize, passed: bool) -> String {
    if passed {
        format!("ordering `{before}` before `{after}` satisfied")
    } else {
        format!("ordering `{before}` before `{after}` failed for {violations} event(s)")
    }
}

fn latency_bound_message(from: &str, to: &str, samples: usize, passed: bool) -> String {
    if passed {
        format!("latency bound `{from}` to `{to}` satisfied across {samples} sample(s)")
    } else {
        format!("latency bound `{from}` to `{to}` failed across {samples} sample(s)")
    }
}

fn required_sequence_message(sequence: &[String], matched: usize, passed: bool) -> String {
    if passed {
        format!("required sequence `{}` occurred", sequence.join(" -> "))
    } else {
        format!(
            "required sequence `{}` stopped after {matched} matched event(s)",
            sequence.join(" -> ")
        )
    }
}

fn same_group(
    first: &super::parsing::TraceEvent,
    second: &super::parsing::TraceEvent,
    by: Option<&str>,
) -> bool {
    by.is_none_or(|key| {
        event_data_string(first, key).is_some_and(|first_value| {
            event_data_string(second, key).is_some_and(|second_value| first_value == second_value)
        })
    })
}

fn event_data_string(event: &super::parsing::TraceEvent, key: &str) -> Option<String> {
    match event.data.get(key)? {
        serde_json::Value::String(value) => Some(value.clone()),
        serde_json::Value::Number(value) => Some(value.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn paired_durations(timeline: &[super::parsing::TraceEvent], from: &str, to: &str) -> Vec<u64> {
    let to_events = timeline
        .iter()
        .filter(|event| event_matches_key(event, to))
        .collect::<Vec<_>>();
    timeline
        .iter()
        .filter(|event| event_matches_key(event, from))
        .filter_map(|from_event| {
            to_events
                .iter()
                .find(|to_event| to_event.t_ms >= from_event.t_ms)
                .map(|to_event| to_event.t_ms - from_event.t_ms)
        })
        .collect()
}

fn event_detail(event: &super::parsing::TraceEvent) -> serde_json::Value {
    json!({
        "t_ms": event.t_ms,
        "source": event.source,
        "event": event.event,
        "data": event.data,
    })
}

fn event_details(events: &[&super::parsing::TraceEvent]) -> Vec<serde_json::Value> {
    events.iter().map(|event| event_detail(event)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::extension::trace::parsing::TraceEvent;
    use std::collections::BTreeMap;

    fn event(t_ms: u64, source: &str, event: &str) -> TraceEvent {
        TraceEvent {
            t_ms,
            source: source.to_string(),
            event: event.to_string(),
            data: BTreeMap::new(),
        }
    }

    fn event_with_data(
        t_ms: u64,
        source: &str,
        event: &str,
        data: serde_json::Value,
    ) -> TraceEvent {
        let serde_json::Value::Object(data) = data else {
            panic!("test event data must be an object");
        };
        TraceEvent {
            t_ms,
            source: source.to_string(),
            event: event.to_string(),
            data: data.into_iter().collect(),
        }
    }

    fn results(
        timeline: Vec<TraceEvent>,
        temporal_assertions: Vec<TraceTemporalAssertionDefinition>,
    ) -> TraceResults {
        TraceResults {
            component_id: "example".to_string(),
            scenario_id: "synthetic".to_string(),
            status: TraceStatus::Pass,
            summary: None,
            failure: None,
            rig: None,
            evidence: None,
            timeline,
            span_definitions: Vec::new(),
            span_results: Vec::new(),
            metrics: Default::default(),
            assertions: Vec::new(),
            temporal_assertions,
            artifacts: Vec::new(),
            toolchain: None,
            components: None,
            dependencies: Vec::new(),
            metrics: Default::default(),
            preview: None,
        }
    }

    #[test]
    fn test_apply_temporal_assertions() {
        let mut results = results(
            vec![event(10, "runner", "ready")],
            vec![TraceTemporalAssertionDefinition::Count {
                id: "ready-once".to_string(),
                events: vec!["runner.ready".to_string()],
                min: Some(1),
                max: Some(1),
                message: None,
            }],
        );

        assert!(!apply_temporal_assertions(&mut results));
        assert_eq!(results.status, TraceStatus::Pass);
        assert!(results.temporal_assertions.is_empty());
        assert_eq!(results.assertions.len(), 1);
        assert_eq!(results.assertions[0].id, "ready-once");
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Pass);
        assert_eq!(results.assertions[0].details.as_ref().unwrap()["actual"], 1);
    }

    #[test]
    fn count_assertion_fails_when_count_exceeds_max() {
        let mut results = results(
            vec![
                event(1, "log", "invalid_grant"),
                event(2, "log", "invalid_grant"),
            ],
            vec![TraceTemporalAssertionDefinition::Count {
                id: "no-invalid-grant".to_string(),
                events: vec!["log.invalid_grant".to_string()],
                min: None,
                max: Some(0),
                message: None,
            }],
        );

        assert!(apply_temporal_assertions(&mut results));
        assert_eq!(results.status, TraceStatus::Fail);
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Fail);
        assert_eq!(results.assertions[0].details.as_ref().unwrap()["actual"], 2);
    }

    #[test]
    fn forbidden_event_assertion_fails_on_match() {
        let mut results = results(
            vec![event(10, "desktop", "window.reopened")],
            vec![TraceTemporalAssertionDefinition::ForbiddenEvent {
                id: "no-window-reopen".to_string(),
                pattern: "desktop.window.reopened".to_string(),
                message: None,
            }],
        );

        assert!(apply_temporal_assertions(&mut results));
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Fail);
        assert_eq!(results.assertions[0].details.as_ref().unwrap()["actual"], 1);
    }

    #[test]
    fn max_concurrent_assertion_fails_when_overlap_exceeds_max() {
        let mut results = results(
            vec![
                event(0, "proc", "spawn"),
                event(5, "proc", "spawn"),
                event(10, "proc", "exit"),
                event(15, "proc", "exit"),
            ],
            vec![TraceTemporalAssertionDefinition::MaxConcurrent {
                id: "max-one-proc".to_string(),
                track: vec!["proc.spawn".to_string(), "proc.exit".to_string()],
                max: 1,
                message: None,
            }],
        );

        assert!(apply_temporal_assertions(&mut results));
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Fail);
        assert_eq!(
            results.assertions[0].details.as_ref().unwrap()["max_observed"],
            2
        );
    }

    #[test]
    fn no_overlap_assertion_fails_for_different_groups_inside_window() {
        let mut results = results(
            vec![
                event_with_data(0, "fs", "write", json!({ "pid": 1 })),
                event_with_data(50, "fs", "write", json!({ "pid": 2 })),
                event_with_data(200, "fs", "write", json!({ "pid": 3 })),
            ],
            vec![TraceTemporalAssertionDefinition::NoOverlap {
                id: "no-auth-race".to_string(),
                events: vec!["fs.write".to_string()],
                by: "pid".to_string(),
                window_ms: 100,
                message: None,
            }],
        );

        assert!(apply_temporal_assertions(&mut results));
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Fail);
        assert_eq!(
            results.assertions[0].details.as_ref().unwrap()["overlap_count"],
            1
        );
    }

    #[test]
    fn ordering_assertion_fails_when_grouped_after_event_is_missing() {
        let mut results = results(
            vec![
                event_with_data(0, "http", "response", json!({ "request_id": "a" })),
                event_with_data(20, "fs", "write", json!({ "request_id": "b" })),
            ],
            vec![TraceTemporalAssertionDefinition::Ordering {
                id: "response-before-write".to_string(),
                before: "http.response".to_string(),
                after: "fs.write".to_string(),
                within_ms: Some(100),
                by: Some("request_id".to_string()),
                message: None,
            }],
        );

        assert!(apply_temporal_assertions(&mut results));
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Fail);
        assert_eq!(
            results.assertions[0].details.as_ref().unwrap()["violation_count"],
            1
        );
    }

    #[test]
    fn latency_bound_assertion_uses_bench_percentile_logic() {
        let mut results = results(
            vec![
                event(0, "request", "start"),
                event(100, "request", "end"),
                event(200, "request", "start"),
                event(400, "request", "end"),
                event(500, "request", "start"),
                event(800, "request", "end"),
            ],
            vec![TraceTemporalAssertionDefinition::LatencyBound {
                id: "request-latency".to_string(),
                from: "request.start".to_string(),
                to: "request.end".to_string(),
                p50_ms: Some(200),
                p95_ms: Some(250),
                p99_ms: None,
                message: None,
            }],
        );

        assert!(apply_temporal_assertions(&mut results));
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Fail);
        assert_eq!(
            results.assertions[0].details.as_ref().unwrap()["actual_p50_ms"],
            200.0
        );
        assert_eq!(
            results.assertions[0].details.as_ref().unwrap()["actual_p95_ms"],
            290.0
        );
    }

    #[test]
    fn required_sequence_assertion_fails_when_sequence_is_incomplete() {
        let mut results = results(
            vec![event(0, "app", "boot"), event(10, "app", "ready")],
            vec![TraceTemporalAssertionDefinition::RequiredSequence {
                id: "boot-flow".to_string(),
                sequence: vec![
                    "app.boot".to_string(),
                    "auth.login".to_string(),
                    "app.ready".to_string(),
                ],
                message: None,
            }],
        );

        assert!(apply_temporal_assertions(&mut results));
        assert_eq!(results.assertions[0].status, TraceAssertionStatus::Fail);
        assert_eq!(
            results.assertions[0].details.as_ref().unwrap()["missing"],
            "auth.login"
        );
    }
}
