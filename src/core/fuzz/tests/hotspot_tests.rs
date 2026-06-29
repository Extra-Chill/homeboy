use serde_json::json;

use crate::core::fuzz::{
    compare_fuzz_hotspot_sets, parse_fuzz_hotspot_set_value, rank_fuzz_observation_set_hotspots,
    FuzzHotspotSet, FuzzObservationSet, FUZZ_HOTSPOT_SET_SCHEMA, FUZZ_OBSERVATION_SET_SCHEMA,
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

#[test]
fn compares_fixture_hotspot_sets_for_relative_convergence() {
    let baseline = FuzzHotspotSet::from_value(json!({
        "schema": FUZZ_HOTSPOT_SET_SCHEMA,
        "version": 1,
        "id": "baseline-hotspots",
        "items": [
            {
                "id": "route:checkout",
                "dimension": "route",
                "metric": "duration",
                "value": 900.0,
                "unit": "ms",
                "rank": 1,
                "relative_score": 1.0,
                "label": "Checkout route"
            },
            {
                "id": "route:search",
                "dimension": "route",
                "metric": "duration",
                "value": 450.0,
                "unit": "ms",
                "rank": 2,
                "relative_score": 0.5,
                "label": "Search route"
            },
            {
                "id": "route:account",
                "dimension": "route",
                "metric": "duration",
                "value": 225.0,
                "unit": "ms",
                "rank": 3,
                "relative_score": 0.25,
                "label": "Account route"
            }
        ]
    }))
    .expect("baseline hotspot set");
    let candidate = FuzzHotspotSet::from_value(json!({
        "schema": FUZZ_HOTSPOT_SET_SCHEMA,
        "version": 1,
        "id": "candidate-hotspots",
        "items": [
            {
                "id": "route:search",
                "dimension": "route",
                "metric": "duration",
                "value": 400.0,
                "unit": "ms",
                "rank": 1,
                "relative_score": 1.0,
                "label": "Search route"
            },
            {
                "id": "route:checkout",
                "dimension": "route",
                "metric": "duration",
                "value": 120.0,
                "unit": "ms",
                "rank": 2,
                "relative_score": 0.3,
                "label": "Checkout route"
            },
            {
                "id": "route:feed",
                "dimension": "route",
                "metric": "duration",
                "value": 100.0,
                "unit": "ms",
                "rank": 3,
                "relative_score": 0.25,
                "label": "Feed route"
            }
        ]
    }))
    .expect("candidate hotspot set");

    let comparison = compare_fuzz_hotspot_sets(&baseline, &candidate);

    assert_eq!(comparison.baseline_drift.baseline_score_total, 1.75);
    assert_eq!(comparison.baseline_drift.candidate_score_total, 1.55);
    assert!((comparison.baseline_drift.score_total_delta + 0.2).abs() < f64::EPSILON);
    assert_eq!(comparison.new_items, 1);
    assert_eq!(comparison.resolved_items, 1);
    assert_eq!(
        comparison.collapsed_top_items,
        vec!["route:account", "route:checkout"]
    );
    assert_eq!(
        comparison.emerging_top_items,
        vec!["route:feed", "route:search"]
    );

    let checkout = comparison
        .items
        .iter()
        .find(|item| item.key == "route:checkout")
        .expect("checkout delta");
    assert_eq!(checkout.change_kind, "decreased");
    assert_eq!(checkout.rank_movement, Some(-1));
    assert_eq!(checkout.relative_score_delta, Some(-0.7));

    let feed = comparison
        .items
        .iter()
        .find(|item| item.key == "route:feed")
        .expect("emerging delta");
    assert_eq!(feed.change_kind, "new");
    assert_eq!(feed.candidate_rank, Some(3));

    let serialized = serde_json::to_value(&comparison).expect("serialize comparison");
    assert!(serialized.get("threshold").is_none());
    assert!(serialized.get("status").is_none());
}
