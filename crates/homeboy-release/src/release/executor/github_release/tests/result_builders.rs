//! Tests for the `ReleaseStepResult` builders (issue #3541).

use crate::release::types::ReleaseStepStatus;

use super::super::{
    create_failed_result, not_created_result, published_release_url, upload_failed_result,
    upload_success_result,
};
use super::{data_bool, data_str, test_body, test_repair, test_repo};

#[test]
fn not_created_result_is_failed_and_not_marked_skipped_success() {
    // Regression for #3541: a release that was never created must NOT be a
    // success-with-skipped step — that lets publish/upload run against a
    // missing release. It must be Failed.
    let result = not_created_result(
        "v0.10.6",
        &test_repo(),
        "gh-not-authenticated",
        "`gh` is not authenticated; GitHub Release was not created.",
        test_repair(),
    );

    assert_eq!(result.status, ReleaseStepStatus::Failed);
    assert_eq!(data_bool(&result, "skipped"), Some(false));
    assert_eq!(data_bool(&result, "release_created"), Some(false));
    assert_eq!(data_str(&result, "reason"), Some("gh-not-authenticated"));
    assert!(result
        .error
        .as_deref()
        .unwrap()
        .contains("not authenticated"));
    assert!(data_str(&result, "fallback_command").is_some());
    assert!(result
        .hints
        .iter()
        .any(|hint| hint.message.contains("no new tag")));
}

#[test]
fn create_failed_result_reports_generated_notes_failed_as_failure() {
    // The exact scenario from #3541: generated notes failed, the fallback
    // create also failed, so no release object exists. Must be Failed and
    // must carry the generated-notes-failed reason — not success/skipped.
    let result = create_failed_result(
        "v0.10.6",
        &test_repo(),
        "generated-notes-failed",
        String::new(),
        "HTTP 502: bad gateway".to_string(),
        test_repair(),
        &test_body(),
        Some("build/v0.10.6-release-notes.md"),
    );

    assert_eq!(result.status, ReleaseStepStatus::Failed);
    assert_eq!(data_bool(&result, "skipped"), Some(false));
    assert_eq!(data_bool(&result, "release_created"), Some(false));
    assert_eq!(data_str(&result, "reason"), Some("generated-notes-failed"));
    assert!(result
        .error
        .as_deref()
        .unwrap()
        .contains("`gh release create` failed for v0.10.6"));
    assert!(result
        .error
        .as_deref()
        .unwrap()
        .contains("HTTP 502: bad gateway"));
    assert!(data_str(&result, "fallback_command").is_some());
}

#[test]
fn create_failed_result_reports_plain_create_failure() {
    let result = create_failed_result(
        "v0.10.6",
        &test_repo(),
        "gh-command-failed",
        String::new(),
        "release v0.10.6 already exists".to_string(),
        test_repair(),
        &test_body(),
        Some("build/v0.10.6-release-notes.md"),
    );

    assert_eq!(result.status, ReleaseStepStatus::Failed);
    assert_eq!(data_str(&result, "reason"), Some("gh-command-failed"));
}

#[test]
fn upload_failed_result_is_failed_but_records_release_exists() {
    // The release object exists but assets did not attach. Still Failed so
    // nothing assumes the assets are present, but release_created stays true.
    let result = upload_failed_result(
        "v0.10.6",
        &test_repo(),
        String::new(),
        "could not upload asset".to_string(),
        Some(1),
        false,
        1,
        test_repair(),
    );

    assert_eq!(result.status, ReleaseStepStatus::Failed);
    assert_eq!(data_bool(&result, "skipped"), Some(false));
    assert_eq!(data_bool(&result, "release_created"), Some(true));
    assert_eq!(data_str(&result, "reason"), Some("gh-upload-failed"));
    assert!(result
        .error
        .as_deref()
        .unwrap()
        .contains("could not upload asset"));
    assert!(result
        .hints
        .iter()
        .any(|hint| hint.message.contains("Resume the existing draft")));
}

#[test]
fn upload_timeout_is_classified_and_preserves_empty_stderr() {
    let result = upload_failed_result(
        "v0.10.6",
        &test_repo(),
        String::new(),
        String::new(),
        Some(124),
        true,
        1,
        test_repair(),
    );
    assert_eq!(data_bool(&result, "timed_out"), Some(true));
    assert_eq!(
        result
            .data
            .as_ref()
            .and_then(|data| data.get("exit_code"))
            .and_then(|value| value.as_i64()),
        Some(124)
    );
    assert!(result.error.as_deref().unwrap().contains("timed out"));
}

#[test]
fn verified_upload_result_is_successful_only_after_publication() {
    let result = upload_success_result("v0.10.6", &test_repo(), 2);

    assert_eq!(result.status, ReleaseStepStatus::Success);
    assert_eq!(data_str(&result, "action"), Some("github.release.upload"));
    assert_eq!(
        data_str(&result, "url"),
        Some("https://github.com/example-org/studio-web/releases/tag/v0.10.6")
    );
    assert_eq!(
        result
            .data
            .as_ref()
            .and_then(|data| data.get("artifact_count"))
            .and_then(|value| value.as_u64()),
        Some(2)
    );
}

#[test]
fn published_release_url_ignores_transient_draft_url() {
    let url = published_release_url(
        &test_repo(),
        "v0.49.4",
        "https://github.com/example-org/studio-web/releases/tag/untagged-944964b141cb713e104d\n",
        "",
    );

    assert_eq!(
        url,
        "https://github.com/example-org/studio-web/releases/tag/v0.49.4"
    );
}

#[test]
fn published_release_url_prefers_final_publish_response() {
    let url = published_release_url(
        &test_repo(),
        "v0.49.4",
        "https://github.com/example-org/studio-web/releases/tag/untagged-944964b141cb713e104d\n",
        "https://github.com/example-org/studio-web/releases/tag/v0.49.4\n",
    );

    assert_eq!(
        url,
        "https://github.com/example-org/studio-web/releases/tag/v0.49.4"
    );
}
