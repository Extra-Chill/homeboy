use super::{BenchArtifact, BenchPreviewLifecycleMetadata};

/// Bench artifacts are read by extension bench runners: absent optional
/// fields stay off the wire and preview lifecycle metadata is flattened.
#[test]
fn bench_artifact_wire_shape_omits_absent_fields_and_flattens_preview_metadata() {
    let minimal = BenchArtifact {
        path: Some("artifacts/run-1/out.txt".to_string()),
        ..BenchArtifact::default()
    };
    assert_eq!(
        serde_json::to_string(&minimal).unwrap(),
        r#"{"path":"artifacts/run-1/out.txt"}"#
    );

    let artifact = BenchArtifact {
        path: Some("artifacts/preview.json".to_string()),
        url: Some("https://example.test/preview.json".to_string()),
        artifact_type: Some("preview".to_string()),
        kind: Some("preview".to_string()),
        label: Some("Baseline preview".to_string()),
        required_durable: true,
        role: Some("baseline".to_string()),
        preview_url: Some("https://preview.example.test/".to_string()),
        public_url: Some("https://preview.example.test/".to_string()),
        local_url: Some("http://127.0.0.1:8080".to_string()),
        status: Some("running".to_string()),
        preview_lifecycle: BenchPreviewLifecycleMetadata {
            expires_at: Some("2026-06-08T12:00:00Z".to_string()),
            cleanup_status: Some("pending".to_string()),
            service_lifecycle: Some(serde_json::json!({ "service_id": "site-preview" })),
            browser_origin_evidence: Some(serde_json::json!({
                "browser_effective_origin": "https://preview.example.test/"
            })),
        },
        ..BenchArtifact::default()
    };

    assert_eq!(
        serde_json::to_value(&artifact).unwrap(),
        serde_json::json!({
            "path": "artifacts/preview.json",
            "url": "https://example.test/preview.json",
            "type": "preview",
            "kind": "preview",
            "label": "Baseline preview",
            "required_durable": true,
            "role": "baseline",
            "preview_url": "https://preview.example.test/",
            "public_url": "https://preview.example.test/",
            "local_url": "http://127.0.0.1:8080",
            "status": "running",
            "expires_at": "2026-06-08T12:00:00Z",
            "cleanup_status": "pending",
            "service_lifecycle": { "service_id": "site-preview" },
            "browser_origin_evidence": {
                "browser_effective_origin": "https://preview.example.test/"
            }
        })
    );
}

#[test]
fn bench_artifact_decodes_legacy_records_and_rejects_unknown_fields() {
    let legacy: BenchArtifact =
        serde_json::from_str(r#"{"path":"artifacts/legacy.png"}"#).expect("legacy artifact");
    assert!(!legacy.required_durable);

    let err = serde_json::from_str::<BenchArtifact>(r#"{"path":"artifact.txt","unexpected":true}"#)
        .expect_err("artifact schema is strict");
    assert!(err.to_string().contains("unknown field"));
}
