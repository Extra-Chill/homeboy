use serde_json::json;

use crate::core::fuzz::*;

#[test]
fn surface_normalizes_optional_fields_and_nested_inputs() {
    let surface = FuzzSurface::from_value(json!({
        "id": " orders ",
        "kind": " api ",
        "label": " ",
        "target": " https://example.test/resource ",
        "safety_class": "read_only",
        "operations": [
            { "id": " list ", "kind": " read ", "tags": [" stable ", " "] }
        ],
        "inputs": [
            { "name": " query ", "kind": " string ", "generator": " ascii ", "constraints": [" max:64 "] }
        ],
        "owner": "extension"
    }))
    .expect("surface contract");

    assert_eq!(surface.schema, FUZZ_SURFACE_SCHEMA);
    assert_eq!(surface.id, "orders");
    assert_eq!(surface.kind, "api");
    assert_eq!(surface.label, None);
    assert_eq!(
        surface.target.as_deref(),
        Some("https://example.test/resource")
    );
    assert_eq!(surface.operations[0].tags, vec!["stable"]);
    assert_eq!(
        surface.operations[0].family,
        Some(FuzzOperationFamily::Read)
    );
    assert_eq!(surface.inputs[0].constraints, vec!["max:64"]);
    assert_eq!(surface.extra["owner"], "extension");
}

#[test]
fn operation_deserializes_old_payload_and_preserves_custom_kind() {
    let surface = FuzzSurface::from_value(json!({
        "id": "surface-1",
        "kind": "api",
        "safety_class": "read_only",
        "operations": [
            { "id": "custom-1", "kind": "domain_specific_probe" }
        ]
    }))
    .expect("surface contract");

    assert_eq!(surface.operations[0].kind, "domain_specific_probe");
    assert_eq!(surface.operations[0].family, None);
}

#[test]
fn operation_normalizes_canonical_families_from_kind() {
    let surface = FuzzSurface::from_value(json!({
        "id": "surface-1",
        "kind": "api",
        "safety_class": "read_only",
        "operations": [
            { "id": "read-1", "kind": " GET " },
            { "id": "create-1", "kind": "post" },
            { "id": "update-1", "kind": "PATCH" },
            { "id": "delete-1", "kind": "delete" },
            { "id": "block-render-1", "kind": "block-render" },
            { "id": "performance-1", "kind": "performance probe" }
        ]
    }))
    .expect("surface contract");

    let families: Vec<Option<FuzzOperationFamily>> = surface
        .operations
        .iter()
        .map(|operation| operation.family)
        .collect();

    assert_eq!(
        families,
        vec![
            Some(FuzzOperationFamily::Read),
            Some(FuzzOperationFamily::Create),
            Some(FuzzOperationFamily::Update),
            Some(FuzzOperationFamily::Delete),
            Some(FuzzOperationFamily::BlockRender),
            Some(FuzzOperationFamily::PerformanceProbe),
        ]
    );
    assert_eq!(surface.operations[0].kind, "GET");
}

#[test]
fn operation_preserves_declared_canonical_family() {
    let surface = FuzzSurface::from_value(json!({
        "id": "surface-1",
        "kind": "api",
        "safety_class": "read_only",
        "operations": [
            { "id": "custom-search", "kind": "bespoke_lookup", "family": "search" }
        ]
    }))
    .expect("surface contract");

    assert_eq!(surface.operations[0].kind, "bespoke_lookup");
    assert_eq!(
        surface.operations[0].family,
        Some(FuzzOperationFamily::Search)
    );
}
