use serde_json::json;

#[test]
fn core_structured_sidecar_registry_validates_current_contracts() {
    for (key, payload) in [
        ("lint.findings", json!([{ "message": "lint failed" }])),
        ("test.results", json!({ "total": 1, "passed": 1 })),
        ("test.failures", json!([{ "message": "test failed" }])),
        ("bench.results", json!({ "results": [] })),
        ("trace.results", json!({ "traces": [] })),
    ] {
        homeboy::core::structured_sidecar::validate_payload(key, &payload)
            .unwrap_or_else(|err| panic!("{key} should validate: {err}"));
    }
}

#[test]
fn core_structured_sidecar_registry_rejects_malformed_output() {
    let err = homeboy::core::structured_sidecar::validate_payload(
        "lint.findings",
        &json!({ "message": "not an array" }),
    )
    .expect_err("lint findings must be an array");

    assert!(err.to_string().contains("lint.findings"));
    assert!(err.to_string().contains("JSON array"));
}
