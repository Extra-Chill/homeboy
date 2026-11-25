use serde_json::json;

use crate::*;

#[test]
fn workload_from_value_normalizes_structural_fields() {
    let workload = FuzzWorkload::from_value(json!({
        "id": " default ",
        "label": " ",
        "safety_class": "read_only",
        "surface_ids": [" api ", " "],
        "operations": [" read ", ""],
        "seed_ids": [" seed-1 "],
        "custom": { "owner": "extension" }
    }))
    .expect("workload contract");

    assert_eq!(workload.schema, FUZZ_WORKLOAD_SCHEMA);
    assert_eq!(workload.id, "default");
    assert_eq!(workload.label, None);
    assert_eq!(workload.safety_class, FuzzSafetyClass::ReadOnly);
    assert_eq!(workload.surface_ids, vec!["api"]);
    assert_eq!(workload.operations, vec!["read"]);
    assert_eq!(workload.seed_ids, vec!["seed-1"]);
    assert_eq!(workload.extra["custom"]["owner"], "extension");
}

#[test]
fn workload_from_value_rejects_invalid_contracts() {
    let cases = [
        (
            json!({ "schema": "example/custom/v1", "id": "default", "safety_class": "read_only" }),
            "fuzz workload schema must be homeboy/fuzz-workload/v1",
        ),
        (
            json!({ "id": " ", "safety_class": "read_only" }),
            "workload.id must be non-empty",
        ),
    ];

    for (value, expected) in cases {
        let error = FuzzWorkload::from_value(value).expect_err(expected);
        assert!(error.contains(expected), "unexpected error: {error}");
    }
}
