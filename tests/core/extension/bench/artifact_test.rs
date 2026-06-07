use super::BenchArtifact;

#[test]
fn bench_artifact_serializes_optional_fields_when_present() {
    let artifact = BenchArtifact {
        path: Some("artifacts/run-1/transcript.json".to_string()),
        url: Some("https://example.test/transcript.json".to_string()),
        artifact_type: None,
        kind: Some("json".to_string()),
        label: Some("Run 1 transcript".to_string()),
        observation_artifact_id: None,
        ..BenchArtifact::default()
    };

    let raw = serde_json::to_string(&artifact).unwrap();

    assert_eq!(
        raw,
        r#"{"path":"artifacts/run-1/transcript.json","url":"https://example.test/transcript.json","kind":"json","label":"Run 1 transcript"}"#
    );
}

#[test]
fn bench_artifact_omits_absent_optional_fields() {
    let artifact = BenchArtifact {
        path: Some("artifacts/run-1/out.txt".to_string()),
        url: None,
        artifact_type: None,
        kind: None,
        label: None,
        observation_artifact_id: None,
        ..BenchArtifact::default()
    };

    let raw = serde_json::to_string(&artifact).unwrap();

    assert_eq!(raw, r#"{"path":"artifacts/run-1/out.txt"}"#);
}

#[test]
fn bench_artifact_serializes_url_fields_when_present() {
    let artifact = BenchArtifact {
        path: None,
        url: Some("https://example.test/wp-admin/".to_string()),
        artifact_type: Some("url".to_string()),
        kind: Some("admin_url".to_string()),
        label: Some("Admin".to_string()),
        observation_artifact_id: None,
        ..BenchArtifact::default()
    };

    let raw = serde_json::to_string(&artifact).unwrap();

    assert_eq!(
        raw,
        r#"{"url":"https://example.test/wp-admin/","type":"url","kind":"admin_url","label":"Admin"}"#
    );
}

#[test]
fn bench_artifact_serializes_preview_metadata_when_present() {
    let artifact = BenchArtifact {
        path: Some("artifacts/preview.json".to_string()),
        artifact_type: Some("preview".to_string()),
        kind: Some("preview".to_string()),
        role: Some("baseline".to_string()),
        preview_url: Some("https://preview.example.test/".to_string()),
        public_url: Some("https://preview.example.test/".to_string()),
        local_url: Some("http://127.0.0.1:8080".to_string()),
        status: Some("running".to_string()),
        expires_at: Some("2026-06-08T12:00:00Z".to_string()),
        cleanup_status: Some("pending".to_string()),
        service_lifecycle: Some(serde_json::json!({ "service_id": "site-preview" })),
        browser_origin_evidence: Some(serde_json::json!({
            "browser_effective_origin": "https://preview.example.test/"
        })),
        ..BenchArtifact::default()
    };

    let value = serde_json::to_value(&artifact).unwrap();

    assert_eq!(value["role"], "baseline");
    assert_eq!(value["preview_url"], "https://preview.example.test/");
    assert_eq!(value["status"], "running");
    assert_eq!(value["expires_at"], "2026-06-08T12:00:00Z");
    assert_eq!(value["cleanup_status"], "pending");
    assert_eq!(value["service_lifecycle"]["service_id"], "site-preview");
    assert_eq!(
        value["browser_origin_evidence"]["browser_effective_origin"],
        "https://preview.example.test/"
    );
}

#[test]
fn bench_artifact_rejects_unknown_fields() {
    let err = serde_json::from_str::<BenchArtifact>(r#"{"path":"artifact.txt","unexpected":true}"#)
        .expect_err("artifact schema is strict");

    assert!(err.to_string().contains("unknown field"));
}
