use serde_json::json;

use crate::{
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
                    "dimensions": [
                        { "name": "operation", "value": "checkout" },
                        { "name": "storage_target", "value": "orders", "kind": "collection" },
                        { "name": "cache_target", "value": "product-prices", "kind": "keyspace" },
                        { "name": "query_fingerprint", "value": "read-items-by-owner" },
                        { "name": "write_amplification", "value": "line-item-fanout" },
                        { "name": "duplicate_work_group", "value": "totals-recalculation" },
                        { "name": "task_unit", "value": "post-submit-handler" },
                        { "name": "page_screen", "value": "checkout" },
                        { "name": "browser_step", "value": "submit-order" },
                        { "name": "extension_label", "value": "provider-specific-bucket" }
                    ],
                    "kind": "handler",
                    "metric": "duration",
                    "value": 481.5,
                    "unit": "ms",
                    "metrics": [
                        { "name": "duration", "value": 481.5, "unit": "ms", "relative_score": 0.98 },
                        { "name": "write_amplification", "value": 8, "unit": "writes_per_action" },
                        { "name": "cache_miss_rate", "value": 0.42, "unit": "ratio" }
                    ],
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
    let item = &set.items[0];
    assert_eq!(item.id, "action:checkout");
    assert_eq!(item.dimensions.len(), 10);
    assert_eq!(item.dimensions[1].kind.as_deref(), Some("collection"));
    assert_eq!(item.value, 481.5);
    assert_eq!(item.metrics.len(), 3);
    assert_eq!(item.metrics[0].relative_score, Some(0.98));
    assert_eq!(item.rank, Some(1));
    assert_eq!(item.evidence_refs, vec!["case-log:case-1"]);
}

#[test]
fn preserves_minimal_hotspot_artifact_compatibility() {
    let minimal = json!({
        "schema": FUZZ_HOTSPOT_SET_SCHEMA,
        "id": "legacy-hotspots",
        "items": [
            {
                "id": "action:save",
                "dimension": "action",
                "metric": "duration",
                "value": 12.0,
                "unit": "ms"
            }
        ]
    });

    let set = parse_fuzz_hotspot_set_value(&minimal).expect("minimal hotspot set");
    let serialized = serde_json::to_value(&set).expect("serialize minimal hotspot set");

    assert!(set.items[0].dimensions.is_empty());
    assert!(set.items[0].metrics.is_empty());
    assert!(serialized.pointer("/items/0/dimensions").is_none());
    assert!(serialized.pointer("/items/0/metrics").is_none());
}

#[test]
fn rejects_invalid_hotspot_sets() {
    let item = |value: serde_json::Value, metrics: serde_json::Value| {
        json!({
            "id": "action:slow",
            "dimension": "action",
            "metric": "duration",
            "value": value,
            "unit": "ms",
            "metrics": metrics
        })
    };
    let cases = [
        (
            "non-numeric value",
            json!({
                "schema": FUZZ_HOTSPOT_SET_SCHEMA,
                "id": "hotspots",
                "items": [item(json!("not-a-number"), json!([]))]
            }),
        ),
        (
            "non-numeric nested metric",
            json!({
                "schema": FUZZ_HOTSPOT_SET_SCHEMA,
                "id": "hotspots",
                "items": [item(
                    json!(10),
                    json!([{ "name": "cache_miss_rate", "value": "not-a-number", "unit": "ratio" }])
                )]
            }),
        ),
        (
            "unsupported version",
            json!({
                "schema": FUZZ_HOTSPOT_SET_SCHEMA,
                "version": 999,
                "id": "hotspots",
                "items": []
            }),
        ),
    ];

    for (label, invalid) in cases {
        assert!(parse_fuzz_hotspot_set_value(&invalid).is_none(), "{label}");
    }
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
                "fingerprint": "query-a:duration",
                "operation_id": "read-items",
                "target_id": "items-store",
                "case_id": "catalog-page",
                "phase": "load"
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
    assert_eq!(hotspots.items[2].dimensions[0].name, "family");
    assert_eq!(hotspots.items[2].dimensions[0].value, "query");
    assert!(hotspots.items[2]
        .dimensions
        .iter()
        .any(|dimension| dimension.name == "operation" && dimension.value == "read-items"));
    assert!(hotspots.items[2]
        .dimensions
        .iter()
        .any(|dimension| dimension.name == "target" && dimension.value == "items-store"));
}
