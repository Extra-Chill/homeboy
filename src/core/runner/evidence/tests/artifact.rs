use std::fs;

use reqwest::header;

use super::super::artifact::{
    content_disposition_filename, download_remote_artifact, is_reportable_artifact_evidence_path,
    is_retrievable_runner_artifact, runner_artifact_token, RemoteArtifactToken,
};

#[test]
fn test_download_remote_artifact_rejects_non_runner_token() {
    let err = download_remote_artifact("/tmp/raw-file", None).expect_err("reject raw path");
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
}

#[test]
fn test_runner_artifact_token_round_trips_escaped_segments() {
    let token = runner_artifact_token("runner/a", "run b", "artifact:c");
    assert_eq!(token, "runner-artifact://runner%2Fa/run%20b/artifact%3Ac");
    let parsed = RemoteArtifactToken::parse(&token).expect("parse token");
    assert_eq!(parsed.runner_id, "runner/a");
    assert_eq!(parsed.run_id, "run b");
    assert_eq!(parsed.artifact_id, "artifact:c");
}

#[test]
fn test_content_disposition_filename_parses_quoted_attachment_name() {
    let mut headers = header::HeaderMap::new();
    headers.insert(
        header::CONTENT_DISPOSITION,
        header::HeaderValue::from_static("attachment; filename=\"report.json\""),
    );

    assert_eq!(
        content_disposition_filename(&headers).as_deref(),
        Some("report.json")
    );
}

#[test]
fn test_reportable_artifact_evidence_requires_local_or_retrievable_path() {
    crate::test_support::with_isolated_home(|home| {
        let local = home.path().join("artifact.json");
        fs::write(&local, b"{}").expect("artifact");

        assert!(is_reportable_artifact_evidence_path(
            &local.to_string_lossy()
        ));
        assert!(is_reportable_artifact_evidence_path(
            "runner-artifact://lab/run-1/artifact-1"
        ));
        assert!(is_reportable_artifact_evidence_path(
            "metadata-only:trace.zip"
        ));
        assert!(is_reportable_artifact_evidence_path(
            "artifacts/relative-trace.zip"
        ));
        assert!(!is_reportable_artifact_evidence_path(
            "/srv/remote-only/trace.zip"
        ));
        assert!(!is_retrievable_runner_artifact(
            "runner-artifact://missing-segments"
        ));
    });
}
