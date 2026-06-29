use serde_json::json;

use crate::core::fuzz::{
    parse_fuzz_hotspot_set_value, rank_fuzz_observation_set_hotspots, FuzzHotspot,
    FuzzHotspotDimension, FuzzHotspotMetric, FuzzHotspotSet, FuzzObservationSet,
    FUZZ_CONTRACT_VERSION, FUZZ_HOTSPOT_SET_SCHEMA, FUZZ_OBSERVATION_SET_SCHEMA,
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
    assert_eq!(set.items.len(), 1);
    assert_eq!(set.items[0].id, "action:checkout");
    assert_eq!(set.items[0].dimension, "action");
    assert_eq!(set.items[0].dimensions.len(), 10);
    assert_eq!(set.items[0].dimensions[1].name, "storage_target");
    assert_eq!(
        set.items[0].dimensions[1].kind.as_deref(),
        Some("collection")
    );
    assert_eq!(set.items[0].kind.as_deref(), Some("handler"));
    assert_eq!(set.items[0].metric, "duration");
    assert_eq!(set.items[0].value, 481.5);
    assert_eq!(set.items[0].unit, "ms");
    assert_eq!(set.items[0].metrics.len(), 3);
    assert_eq!(set.items[0].metrics[0].name, "duration");
    assert_eq!(set.items[0].metrics[0].relative_score, Some(0.98));
    assert_eq!(set.items[0].basis.as_deref(), Some("p95_per_case"));
    assert_eq!(set.items[0].sample_count, Some(144));
    assert_eq!(set.items[0].rank, Some(1));
    assert_eq!(set.items[0].relative_score, Some(0.98));
    assert_eq!(set.items[0].labels, vec!["production", "p95"]);
    assert_eq!(set.items[0].evidence_refs, vec!["case-log:case-1"]);
    assert_eq!(set.items[0].artifact_refs, vec!["profile.json"]);
}

#[test]
fn serializes_generic_hotspot_dimensions_metrics_and_labels_without_gate_fields() {
    let set = FuzzHotspotSet {
        schema: FUZZ_HOTSPOT_SET_SCHEMA.to_string(),
        version: FUZZ_CONTRACT_VERSION,
        id: "rich-hotspots".to_string(),
        label: None,
        items: vec![FuzzHotspot {
            id: "operation:checkout".to_string(),
            dimension: "operation".to_string(),
            dimensions: vec![
                dimension("operation", "checkout"),
                dimension("query_fingerprint", "fetch-related-items"),
                dimension("storage_target", "items"),
                dimension("cache_target", "price-cache"),
                dimension("write_amplification", "metadata-fanout"),
                dimension("duplicate_work_group", "recalculate-summary"),
                dimension("task_unit", "submit-handler"),
                dimension("page_screen", "checkout"),
                dimension("browser_step", "place-order"),
                dimension("extension_label", "runtime-owned-label"),
            ],
            kind: Some("relative_hotspot".to_string()),
            metric: "duration".to_string(),
            value: 42.0,
            unit: "ms".to_string(),
            metrics: vec![
                metric("duration", 42.0, "ms"),
                metric("duplicate_work", 6.0, "count"),
                metric("writes_per_operation", 3.5, "ratio"),
            ],
            basis: Some("relative_rank".to_string()),
            sample_count: Some(12),
            rank: Some(1),
            relative_score: Some(1.0),
            label: Some("Checkout operation".to_string()),
            labels: vec!["production".to_string(), "candidate".to_string()],
            evidence_refs: Vec::new(),
            artifact_refs: Vec::new(),
            source_refs: Vec::new(),
            provenance: None,
            metadata: serde_json::Value::Null,
            extra: Default::default(),
        }],
        provenance: None,
        metadata: serde_json::Value::Null,
        extra: Default::default(),
    };

    let serialized = serde_json::to_value(&set).expect("serialize hotspot set");
    let parsed = parse_fuzz_hotspot_set_value(&serialized).expect("parse serialized hotspot set");

    assert_eq!(parsed, set);
    assert!(serialized.pointer("/items/0/dimensions").is_some());
    assert!(serialized.pointer("/items/0/metrics").is_some());
    assert_no_nested_gate_fields(&serialized);
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
fn rejects_invalid_nested_hotspot_metric_values() {
    let invalid = json!({
        "schema": FUZZ_HOTSPOT_SET_SCHEMA,
        "id": "hotspots",
        "items": [
            {
                "id": "action:slow",
                "dimension": "action",
                "metric": "duration",
                "value": 10,
                "unit": "ms",
                "metrics": [
                    { "name": "cache_miss_rate", "value": "not-a-number", "unit": "ratio" }
                ]
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

fn dimension(name: &str, value: &str) -> FuzzHotspotDimension {
    FuzzHotspotDimension {
        name: name.to_string(),
        value: value.to_string(),
        kind: None,
        label: None,
        labels: Vec::new(),
        metadata: serde_json::Value::Null,
        extra: Default::default(),
    }
}

fn metric(name: &str, value: f64, unit: &str) -> FuzzHotspotMetric {
    FuzzHotspotMetric {
        name: name.to_string(),
        value,
        unit: unit.to_string(),
        basis: None,
        sample_count: None,
        rank: None,
        relative_score: None,
        labels: Vec::new(),
        metadata: serde_json::Value::Null,
        extra: Default::default(),
    }
}

fn assert_no_nested_gate_fields(value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, value) in object {
                assert_ne!(key, "threshold");
                assert_ne!(key, "budget");
                assert_ne!(key, "status");
                assert_ne!(key, "passed");
                assert_ne!(key, "failed");
                assert_no_nested_gate_fields(value);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                assert_no_nested_gate_fields(item);
            }
        }
        _ => {}
    }
}
