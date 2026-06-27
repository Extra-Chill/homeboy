use serde_json::json;

use crate::core::fuzz::{
    parse_fuzz_hotspot_set_value, rank_fuzz_observation_set_hotspots, FuzzObservationSet,
    FUZZ_HOTSPOT_SET_SCHEMA, FUZZ_OBSERVATION_SET_SCHEMA,
};

#[test]
fn parses_typed_hotspot_set_from_result_envelope_metadata() {
    let envelope = json!({
        "schema": "homeboy/fuzz-result-envelope/v1",
        "hotspots": {
            "schema": FUZZ_HOTSPOT_SET_SCHEMA,
            "id": "production-measurement-hotspots",
            "label": "Production measurement hotspots",
            "items": [
                {
                    "id": "action:checkout",
                    "dimension": "action",
                    "kind": "handler",
                    "metric": "duration",
                    "value": 481.5,
                    "unit": "ms",
                    "basis": "p95_per_case",
                    "sample_count": 144,
                    "rank": 1,
                    "relative_score": 0.98,
                    "label": "Checkout action",
                    "labels": ["production", "p95"],
                    "evidence_refs": ["case-log:case-1"],
                    "artifact_refs": ["profile.json"],
                    "metadata": { "bucket": "top" }
                }
            ]
        }
    });

    let set = parse_fuzz_hotspot_set_value(&envelope).expect("typed hotspot set");

    assert_eq!(set.id, "production-measurement-hotspots");
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].id, "action:checkout");
    assert_eq!(set.items[0].dimension, "action");
    assert_eq!(set.items[0].kind.as_deref(), Some("handler"));
    assert_eq!(set.items[0].metric, "duration");
    assert_eq!(set.items[0].value, 481.5);
    assert_eq!(set.items[0].unit, "ms");
    assert_eq!(set.items[0].basis.as_deref(), Some("p95_per_case"));
    assert_eq!(set.items[0].sample_count, Some(144));
    assert_eq!(set.items[0].rank, Some(1));
    assert_eq!(set.items[0].relative_score, Some(0.98));
    assert_eq!(set.items[0].labels, vec!["production", "p95"]);
    assert_eq!(set.items[0].evidence_refs, vec!["case-log:case-1"]);
    assert_eq!(set.items[0].artifact_refs, vec!["profile.json"]);
}

#[test]
fn rejects_invalid_hotspot_metric_values() {
    let invalid = json!({
        "schema": FUZZ_HOTSPOT_SET_SCHEMA,
        "id": "hotspots",
        "items": [
            {
                "id": "query:slow",
                "dimension": "query",
                "metric": "duration",
                "value": "not-a-number",
                "unit": "ms"
            }
        ]
    });

    assert!(parse_fuzz_hotspot_set_value(&invalid).is_none());
}

#[test]
fn rejects_unsupported_hotspot_set_versions() {
    let invalid = json!({
        "schema": FUZZ_HOTSPOT_SET_SCHEMA,
        "version": 999,
        "id": "hotspots",
        "items": []
    });

    assert!(parse_fuzz_hotspot_set_value(&invalid).is_none());
}

#[test]
fn ranks_observation_set_hotspots_deterministically() {
    let observations = FuzzObservationSet::from_value(json!({
        "schema": FUZZ_OBSERVATION_SET_SCHEMA,
        "version": 1,
        "id": "candidate-observations",
        "observations": [
            {
                "id": "slow-query",
                "family": "query",
                "subject": "query-a",
                "metric": "duration",
                "value": 25.0,
                "unit": "ms",
                "fingerprint": "query-a:duration"
            },
            {
                "id": "counter-spike",
                "family": "counter",
                "subject": "counter-a",
                "metric": "count",
                "value": -50.0,
                "unit": "count"
            },
            {
                "id": "same-score-action",
                "family": "action",
                "subject": "action-a",
                "metric": "duration",
                "value": 25.0,
                "unit": "ms"
            }
        ]
    }))
    .expect("observation set");

    let hotspots = rank_fuzz_observation_set_hotspots(&observations);

    assert_eq!(hotspots.schema, FUZZ_HOTSPOT_SET_SCHEMA);
    assert_eq!(hotspots.id, "candidate-observations-hotspots");
    assert_eq!(hotspots.items.len(), 3);
    assert_eq!(hotspots.items[0].id, "counter:counter-a:count");
    assert_eq!(hotspots.items[0].rank, Some(1));
    assert_eq!(hotspots.items[0].relative_score, Some(1.0));
    assert_eq!(hotspots.items[1].id, "action:action-a:duration");
    assert_eq!(hotspots.items[1].rank, Some(2));
    assert_eq!(hotspots.items[1].relative_score, Some(0.5));
    assert_eq!(hotspots.items[2].id, "query-a:duration");
    assert_eq!(hotspots.items[2].rank, Some(3));
    assert_eq!(hotspots.items[2].relative_score, Some(0.5));
    assert_eq!(hotspots.items[2].evidence_refs, vec!["slow-query"]);
}
