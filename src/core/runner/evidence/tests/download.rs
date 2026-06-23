use reqwest::header;

use super::super::download::content_disposition_filename;
use super::super::download::download_remote_artifact;

#[test]
fn test_download_remote_artifact_rejects_non_runner_token() {
    let err = download_remote_artifact("/tmp/raw-file", None).expect_err("reject raw path");
    assert_eq!(err.code.as_str(), "validation.invalid_argument");
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
