use serde_json::json;

use crate::core::fuzz::*;

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
fn workload_from_value_rejects_wrong_schema() {
    let error = FuzzWorkload::from_value(json!({
        "schema": "example/custom/v1",
        "id": "default",
        "safety_class": "read_only"
    }))
    .expect_err("schema mismatch should fail");

    assert!(
        error.contains("fuzz workload schema must be homeboy/fuzz-workload/v1"),
        "unexpected error: {error}"
    );
}

#[test]
fn workload_from_value_requires_non_empty_id() {
    let error = FuzzWorkload::from_value(json!({
        "id": " ",
        "safety_class": "read_only"
    }))
    .expect_err("blank id should fail");

    assert_eq!(error, "workload.id must be non-empty");
}
