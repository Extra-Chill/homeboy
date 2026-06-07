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
    };

    let raw = serde_json::to_string(&artifact).unwrap();

    assert_eq!(
        raw,
        r#"{"url":"https://example.test/wp-admin/","type":"url","kind":"admin_url","label":"Admin"}"#
    );
}

#[test]
fn bench_artifact_rejects_unknown_fields() {
    let err = serde_json::from_str::<BenchArtifact>(r#"{"path":"artifact.txt","unexpected":true}"#)
        .expect_err("artifact schema is strict");

    assert!(err.to_string().contains("unknown field"));
}
