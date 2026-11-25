use serde_json::json;

use crate::*;

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
fn operation_family_is_inferred_from_kind_unless_declared() {
    let surface = FuzzSurface::from_value(json!({
        "id": "surface-1",
        "kind": "api",
        "safety_class": "read_only",
        "operations": [
            { "id": "read-1", "kind": " GET " },
            { "id": "create-1", "kind": "post" },
            { "id": "update-1", "kind": "PATCH" },
            { "id": "delete-1", "kind": "delete" },
            { "id": "render-1", "kind": "render" },
            { "id": "performance-1", "kind": "performance probe" },
            { "id": "custom-1", "kind": "domain_specific_probe" },
            { "id": "custom-search", "kind": "bespoke_lookup", "family": "search" }
        ]
    }))
    .expect("surface contract");

    let operations: Vec<(&str, Option<FuzzOperationFamily>)> = surface
        .operations
        .iter()
        .map(|operation| (operation.kind.as_str(), operation.family))
        .collect();

    assert_eq!(
        operations,
        vec![
            ("GET", Some(FuzzOperationFamily::Read)),
            ("post", Some(FuzzOperationFamily::Create)),
            ("PATCH", Some(FuzzOperationFamily::Update)),
            ("delete", Some(FuzzOperationFamily::Delete)),
            ("render", Some(FuzzOperationFamily::Render)),
            (
                "performance probe",
                Some(FuzzOperationFamily::PerformanceProbe)
            ),
            // Custom kinds are preserved without a guessed family.
            ("domain_specific_probe", None),
            ("bespoke_lookup", Some(FuzzOperationFamily::Search)),
        ]
    );
}
