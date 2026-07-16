//! Shared observation timeline and span primitives.
//!
//! Trace uses these today, and other command families can reuse the same
//! event/span contract without reimplementing selector parsing or phase math.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservationEvent {
    pub t_ms: u64,
    pub source: String,
    pub event: String,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub data: BTreeMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ObservationSpanDefinition {
    pub id: String,
    pub from: String,
    pub to: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ObservationSpanStatus {
    Ok,
    Skipped,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ObservationSpanResult {
    pub id: String,
    pub from: String,
    pub to: String,
    pub status: ObservationSpanStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub from_t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub to_t_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl ObservationSpanResult {
    pub fn is_ok(&self) -> bool {
        self.status == ObservationSpanStatus::Ok
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationPhaseMilestone {
    pub label: String,
    pub key: String,
}

pub fn parse_span_definition(raw: &str) -> Result<ObservationSpanDefinition, String> {
    let parts: Vec<&str> = raw.split(':').collect();
    if parts.len() != 3 {
        return Err("expected id:from:to".to_string());
    }
    let definition = ObservationSpanDefinition {
        id: parts[0].trim().to_string(),
        from: parts[1].trim().to_string(),
        to: parts[2].trim().to_string(),
    };
    if definition.id.is_empty() || definition.from.is_empty() || definition.to.is_empty() {
        return Err("span id, from, and to must be non-empty".to_string());
    }
    Ok(definition)
}

pub fn parse_phase_milestone(raw: &str) -> Result<ObservationPhaseMilestone, String> {
    let raw = raw.trim();
    if raw.is_empty() {
        return Err("phase milestone must be non-empty".to_string());
    }

    let (label, key) = match raw.split_once(':') {
        Some((label, key)) => (label.trim(), key.trim()),
        None => (raw, raw),
    };
    if label.is_empty() || key.is_empty() {
        return Err("expected [label:]source.event".to_string());
    }

    Ok(ObservationPhaseMilestone {
        label: label.to_string(),
        key: key.to_string(),
    })
}

pub fn phase_span_definitions(
    phases: &[ObservationPhaseMilestone],
) -> Result<Vec<ObservationSpanDefinition>, String> {
    if phases.is_empty() {
        return Ok(Vec::new());
    }
    if phases.len() < 2 {
        return Err("at least two --phase milestones are required".to_string());
    }

    let mut definitions = phases
        .windows(2)
        .map(|pair| ObservationSpanDefinition {
            id: format!(
                "phase.{}_to_{}",
                phase_id_part(&pair[0].label),
                phase_id_part(&pair[1].label)
            ),
            from: pair[0].key.clone(),
            to: pair[1].key.clone(),
        })
        .collect::<Vec<_>>();

    definitions.push(ObservationSpanDefinition {
        id: "phase.total".to_string(),
        from: phases.first().expect("checked non-empty").key.clone(),
        to: phases.last().expect("checked non-empty").key.clone(),
    });

    Ok(definitions)
}

fn phase_id_part(label: &str) -> String {
    let mut out = String::new();
    let mut last_was_separator = false;
    for ch in label.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            out.push('_');
            last_was_separator = true;
        }
    }
    let trimmed = out.trim_matches('_').to_string();
    if trimmed.is_empty() {
        "phase".to_string()
    } else {
        trimmed
    }
}

pub fn merge_span_definitions(
    runner_definitions: &[ObservationSpanDefinition],
    cli_definitions: &[ObservationSpanDefinition],
) -> Vec<ObservationSpanDefinition> {
    let mut merged = Vec::new();
    for definition in runner_definitions.iter().chain(cli_definitions.iter()) {
        if let Some(position) = merged
            .iter()
            .position(|existing: &ObservationSpanDefinition| existing.id == definition.id)
        {
            merged[position] = definition.clone();
        } else {
            merged.push(definition.clone());
        }
    }
    merged
}

pub fn reporting_timeline(timeline: &[ObservationEvent]) -> Vec<ObservationEvent> {
    let mut events = Vec::new();
    for event in timeline {
        push_reporting_event(&mut events, event.clone());
    }
    events.sort_by_key(|event| event.t_ms);
    events
}

fn push_reporting_event(events: &mut Vec<ObservationEvent>, event: ObservationEvent) {
    let nested = nested_detail_events(&event);
    events.push(event);
    for nested_event in nested {
        push_reporting_event(events, nested_event);
    }
}

fn nested_detail_events(event: &ObservationEvent) -> Vec<ObservationEvent> {
    let Some(serde_json::Value::Array(items)) = event.data.get("events") else {
        return Vec::new();
    };

    items
        .iter()
        .filter_map(|item| nested_detail_event(event.t_ms, item))
        .collect()
}

fn nested_detail_event(parent_t_ms: u64, item: &serde_json::Value) -> Option<ObservationEvent> {
    let source = item.get("source")?.as_str()?.to_string();
    let event = item.get("event")?.as_str()?.to_string();
    let relative_t_ms = json_millis(item.get("t")?)?;
    let data = item
        .get("data")
        .and_then(|value| match value {
            serde_json::Value::Object(map) => Some(
                map.iter()
                    .map(|(key, value)| (key.clone(), value.clone()))
                    .collect(),
            ),
            _ => None,
        })
        .unwrap_or_default();

    Some(ObservationEvent {
        t_ms: parent_t_ms.saturating_add(relative_t_ms),
        source,
        event,
        data,
    })
}

fn json_millis(value: &serde_json::Value) -> Option<u64> {
    match value {
        serde_json::Value::Number(number) => number.as_u64().or_else(|| {
            number
                .as_f64()
                .filter(|value| value.is_finite() && *value >= 0.0)
                .map(|value| value.round() as u64)
        }),
        _ => None,
    }
}

pub fn summarize_spans(
    timeline: &[ObservationEvent],
    definitions: &[ObservationSpanDefinition],
) -> Vec<ObservationSpanResult> {
    definitions
        .iter()
        .map(|definition| summarize_span(definition, timeline))
        .collect()
}

pub fn event_matches_key(event: &ObservationEvent, key: &str) -> bool {
    event.source.len() + 1 + event.event.len() == key.len()
        && key.starts_with(&event.source)
        && key.as_bytes().get(event.source.len()) == Some(&b'.')
        && key[event.source.len() + 1..] == event.event
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventSelector {
    key: String,
    filters: Vec<EventFieldFilter>,
    occurrence: EventOccurrence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct EventFieldFilter {
    path: Vec<String>,
    value: serde_json::Value,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventOccurrence {
    Any,
    Nth(usize),
    Last,
}

impl EventSelector {
    fn parse(raw: &str) -> Result<Self, String> {
        let raw = raw.trim();
        let Some((key, selector)) = raw.split_once('[') else {
            return Ok(Self {
                key: raw.to_string(),
                filters: Vec::new(),
                occurrence: EventOccurrence::Any,
            });
        };
        let Some(selector) = selector.strip_suffix(']') else {
            return Err(format!(
                "invalid span endpoint selector `{raw}`: missing closing `]`"
            ));
        };

        let key = key.trim();
        if key.is_empty() {
            return Err(format!(
                "invalid span endpoint selector `{raw}`: missing event key"
            ));
        }

        let mut filters = Vec::new();
        let mut occurrence = EventOccurrence::Any;
        for part in selector.split(',') {
            let part = part.trim();
            if part.is_empty() {
                return Err(format!(
                    "invalid span endpoint selector `{raw}`: empty selector term"
                ));
            }
            if part == "last" || part == "occurrence=last" {
                if occurrence != EventOccurrence::Any {
                    return Err(format!(
                        "invalid span endpoint selector `{raw}`: occurrence specified more than once"
                    ));
                }
                occurrence = EventOccurrence::Last;
                continue;
            }
            if let Some(value) = part.strip_prefix("occurrence=") {
                if occurrence != EventOccurrence::Any {
                    return Err(format!(
                        "invalid span endpoint selector `{raw}`: occurrence specified more than once"
                    ));
                }
                let occurrence_number = value.parse::<usize>().map_err(|_| {
                    format!(
                        "invalid span endpoint selector `{raw}`: occurrence must be a positive integer or `last`"
                    )
                })?;
                if occurrence_number == 0 {
                    return Err(format!(
                        "invalid span endpoint selector `{raw}`: occurrence must be 1 or greater"
                    ));
                }
                occurrence = EventOccurrence::Nth(occurrence_number);
                continue;
            }

            let Some((path, value)) = part.split_once('=') else {
                return Err(format!(
                    "invalid span endpoint selector `{raw}`: expected `data.FIELD=value`, `occurrence=N`, or `last`"
                ));
            };
            let Some(path) = path.trim().strip_prefix("data.") else {
                return Err(format!(
                    "invalid span endpoint selector `{raw}`: field filters must start with `data.`"
                ));
            };
            let path = path
                .split('.')
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .map(ToString::to_string)
                .collect::<Vec<_>>();
            if path.is_empty() {
                return Err(format!(
                    "invalid span endpoint selector `{raw}`: data field path must be non-empty"
                ));
            }
            filters.push(EventFieldFilter {
                path,
                value: parse_selector_value(value.trim()),
            });
        }

        Ok(Self {
            key: key.to_string(),
            filters,
            occurrence,
        })
    }

    fn select<'a>(&self, timeline: &'a [ObservationEvent]) -> Vec<&'a ObservationEvent> {
        let matches = timeline
            .iter()
            .filter(|event| event_matches_key(event, &self.key))
            .filter(|event| self.filters.iter().all(|filter| filter.matches(event)))
            .collect::<Vec<_>>();

        match self.occurrence {
            EventOccurrence::Any => matches,
            EventOccurrence::Nth(n) => matches
                .get(n - 1)
                .map(|event| vec![*event])
                .unwrap_or_default(),
            EventOccurrence::Last => matches.last().map(|event| vec![*event]).unwrap_or_default(),
        }
    }
}

impl EventFieldFilter {
    fn matches(&self, event: &ObservationEvent) -> bool {
        let Some((first, rest)) = self.path.split_first() else {
            return false;
        };
        let Some(mut current) = event.data.get(first) else {
            return false;
        };
        for segment in rest {
            let Some(next) = current.get(segment) else {
                return false;
            };
            current = next;
        }
        current == &self.value
    }
}

fn parse_selector_value(raw: &str) -> serde_json::Value {
    serde_json::from_str(raw).unwrap_or_else(|_| serde_json::Value::String(raw.to_string()))
}

fn first_event_time(timeline: &[ObservationEvent], selector: &EventSelector) -> Option<u64> {
    selector.select(timeline).first().map(|event| event.t_ms)
}

fn nearest_valid_pair(
    timeline: &[ObservationEvent],
    from_selector: &EventSelector,
    to_selector: &EventSelector,
) -> Option<(u64, u64)> {
    let mut best: Option<(u64, u64)> = None;

    let from_events = from_selector.select(timeline);
    let to_events = to_selector.select(timeline);
    for from in from_events {
        for to in to_events.iter().filter(|event| event.t_ms >= from.t_ms) {
            match best {
                Some((best_from, best_to))
                    if to.t_ms - from.t_ms >= best_to.saturating_sub(best_from) => {}
                _ => best = Some((from.t_ms, to.t_ms)),
            }
        }
    }

    best
}

fn out_of_order_span_message(
    definition: &ObservationSpanDefinition,
    from_t_ms: u64,
    to_t_ms: u64,
) -> String {
    if definition.id.starts_with("phase.") {
        format!(
            "phase milestone `{}` occurred at {}ms before previous milestone `{}` at {}ms; phase chain is non-monotonic",
            definition.to, to_t_ms, definition.from, from_t_ms
        )
    } else {
        "span end occurred before span start".to_string()
    }
}

fn summarize_span(
    definition: &ObservationSpanDefinition,
    timeline: &[ObservationEvent],
) -> ObservationSpanResult {
    let from_selector = match EventSelector::parse(&definition.from) {
        Ok(selector) => selector,
        Err(message) => return skipped_span_result(definition, None, None, Vec::new(), message),
    };
    let to_selector = match EventSelector::parse(&definition.to) {
        Ok(selector) => selector,
        Err(message) => return skipped_span_result(definition, None, None, Vec::new(), message),
    };

    let from_t_ms = first_event_time(timeline, &from_selector);
    let to_t_ms = first_event_time(timeline, &to_selector);
    let mut missing = Vec::new();
    if from_t_ms.is_none() {
        missing.push(definition.from.clone());
    }
    if to_t_ms.is_none() {
        missing.push(definition.to.clone());
    }
    if !missing.is_empty() {
        return skipped_span_result(
            definition,
            from_t_ms,
            to_t_ms,
            missing,
            "span endpoint missing from timeline".to_string(),
        );
    }

    let Some((from_value, to_value)) = nearest_valid_pair(timeline, &from_selector, &to_selector)
    else {
        let message = match (from_t_ms, to_t_ms) {
            (Some(from_value), Some(to_value)) => {
                out_of_order_span_message(definition, from_value, to_value)
            }
            _ => "span end occurred before span start".to_string(),
        };
        return skipped_span_result(definition, from_t_ms, to_t_ms, Vec::new(), message);
    };

    ObservationSpanResult {
        id: definition.id.clone(),
        from: definition.from.clone(),
        to: definition.to.clone(),
        status: ObservationSpanStatus::Ok,
        duration_ms: Some(to_value - from_value),
        from_t_ms: Some(from_value),
        to_t_ms: Some(to_value),
        missing: Vec::new(),
        message: None,
    }
}

fn skipped_span_result(
    definition: &ObservationSpanDefinition,
    from_t_ms: Option<u64>,
    to_t_ms: Option<u64>,
    missing: Vec<String>,
    message: String,
) -> ObservationSpanResult {
    ObservationSpanResult {
        id: definition.id.clone(),
        from: definition.from.clone(),
        to: definition.to.clone(),
        status: ObservationSpanStatus::Skipped,
        duration_ms: None,
        from_t_ms,
        to_t_ms,
        missing,
        message: Some(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(t_ms: u64, source: &str, event: &str) -> ObservationEvent {
        ObservationEvent {
            t_ms,
            source: source.to_string(),
            event: event.to_string(),
            data: Default::default(),
        }
    }

    fn event_with_data(
        t_ms: u64,
        source: &str,
        event: &str,
        data: serde_json::Value,
    ) -> ObservationEvent {
        let serde_json::Value::Object(data) = data else {
            panic!("test data must be a JSON object");
        };

        ObservationEvent {
            t_ms,
            source: source.to_string(),
            event: event.to_string(),
            data: data.into_iter().collect(),
        }
    }

    #[test]
    fn test_reporting_timeline() {
        let timeline = reporting_timeline(&[
            event(50, "runner", "later"),
            event_with_data(
                10,
                "runner",
                "details",
                serde_json::json!({
                    "events": [
                        { "t": 5, "source": "nested", "event": "one" },
                        { "t": 15.4, "source": "nested", "event": "two", "data": { "ok": true } }
                    ]
                }),
            ),
        ]);

        let keys = timeline
            .iter()
            .map(|event| format!("{}:{}", event.t_ms, event.event))
            .collect::<Vec<_>>();
        assert_eq!(keys, vec!["10:details", "15:one", "25:two", "50:later"]);
        assert_eq!(timeline[1].source, "nested");
        assert_eq!(timeline[2].data.get("ok"), Some(&serde_json::json!(true)));
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
        let labeled = parse_phase_milestone("ready:runner.ready").unwrap();
        assert_eq!(labeled.label, "ready");
        assert_eq!(labeled.key, "runner.ready");

        let unlabeled = parse_phase_milestone("runner.done").unwrap();
        assert_eq!(unlabeled.label, "runner.done");
        assert_eq!(unlabeled.key, "runner.done");
    }

    #[test]
    fn test_phase_span_definitions() {
        let phases = vec![
            ObservationPhaseMilestone {
                label: "Start".to_string(),
                key: "runner.start".to_string(),
            },
            ObservationPhaseMilestone {
                label: "Ready".to_string(),
                key: "runner.ready".to_string(),
            },
            ObservationPhaseMilestone {
                label: "Done".to_string(),
                key: "runner.done".to_string(),
            },
        ];

        let definitions = phase_span_definitions(&phases).unwrap();
        assert_eq!(definitions.len(), 3);
        assert_eq!(definitions[0].id, "phase.start_to_ready");
        assert_eq!(definitions[1].id, "phase.ready_to_done");
        assert_eq!(definitions[2].id, "phase.total");
    }

    #[test]
    fn test_summarize_spans() {
        let results = summarize_spans(
            &[event(10, "runner", "start"), event(35, "runner", "done")],
            &[ObservationSpanDefinition {
                id: "total".to_string(),
                from: "runner.start".to_string(),
                to: "runner.done".to_string(),
            }],
        );

        assert_eq!(results[0].status, ObservationSpanStatus::Ok);
        assert_eq!(results[0].duration_ms, Some(25));
        assert!(results[0].is_ok());
    }

    #[test]
    fn test_summarize_spans_reports_missing_endpoints() {
        let results = summarize_spans(
            &[event(10, "runner", "start")],
            &[ObservationSpanDefinition {
                id: "total".to_string(),
                from: "runner.start".to_string(),
                to: "runner.done".to_string(),
            }],
        );

        assert_eq!(results[0].status, ObservationSpanStatus::Skipped);
        assert_eq!(results[0].missing, vec!["runner.done"]);
    }

    #[test]
    fn test_event_matches_key() {
        let event = event(0, "desktop", "window.closed");

        assert!(event_matches_key(&event, "desktop.window.closed"));
        assert!(!event_matches_key(&event, "desktop.window"));
        assert!(!event_matches_key(&event, "runner.window.closed"));
        assert!(!event_matches_key(&event, "desktop.window.closed.extra"));
    }

    #[test]
    fn test_merge_span_definitions() {
        let runner = ObservationSpanDefinition {
            id: "total".to_string(),
            from: "runner.start".to_string(),
            to: "runner.done".to_string(),
        };
        let cli = ObservationSpanDefinition {
            id: "total".to_string(),
            from: "cli.start".to_string(),
            to: "cli.done".to_string(),
        };

        let merged = merge_span_definitions(&[runner], &[cli]);

        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].from, "cli.start");
        assert_eq!(merged[0].to, "cli.done");
    }
}
