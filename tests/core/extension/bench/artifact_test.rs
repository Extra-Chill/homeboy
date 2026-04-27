use super::BenchArtifact;

#[test]
fn bench_artifact_serializes_optional_fields_when_present() {
    let artifact = BenchArtifact {
        path: "artifacts/run-1/transcript.json".to_string(),
        kind: Some("json".to_string()),
        label: Some("Run 1 transcript".to_string()),
    };

    let raw = serde_json::to_string(&artifact).unwrap();

    assert_eq!(
        raw,
        r#"{"path":"artifacts/run-1/transcript.json","kind":"json","label":"Run 1 transcript"}"#
    );
}

#[test]
fn bench_artifact_omits_absent_optional_fields() {
    let artifact = BenchArtifact {
        path: "artifacts/run-1/out.txt".to_string(),
        kind: None,
        label: None,
    };

    let raw = serde_json::to_string(&artifact).unwrap();

    assert_eq!(raw, r#"{"path":"artifacts/run-1/out.txt"}"#);
}

#[test]
fn bench_artifact_rejects_unknown_fields() {
    let err = serde_json::from_str::<BenchArtifact>(r#"{"path":"artifact.txt","unexpected":true}"#)
        .expect_err("artifact schema is strict");

    assert!(err.to_string().contains("unknown field"));
}
