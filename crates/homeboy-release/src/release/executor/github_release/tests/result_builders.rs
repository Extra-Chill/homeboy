//! Tests for the `ReleaseStepResult` builders (issue #3541).

use crate::release::types::ReleaseStepStatus;

use super::super::gh_cli::{gh_failure_diagnostic, GhCommandOutput};
use super::super::{
    create_failed_result, not_created_result, published_existing_draft_result,
    published_release_url, unfinished_release_result, upload_failed_result, upload_success_result,
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
        &GhCommandOutput {
            stdout: String::new(),
            stderr: "HTTP 502: bad gateway".to_string(),
            exit_code: Some(1),
            timed_out: false,
        },
        test_repair(),
        &test_body(),
        Some("build/v0.10.6-release-notes.md"),
        &[],
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
        &GhCommandOutput {
            stdout: String::new(),
            stderr: "release v0.10.6 already exists".to_string(),
            exit_code: Some(1),
            timed_out: false,
        },
        test_repair(),
        &test_body(),
        Some("build/v0.10.6-release-notes.md"),
        &[],
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
        &[],
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
        &[],
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
fn upload_failed_result_sanitizes_persisted_output() {
    let result = upload_failed_result(
        "v0.10.6",
        &test_repo(),
        format!(
            "https://user:password@example.test/file?token=secret {}",
            "x".repeat(5000)
        ),
        "Authorization: Bearer secret".to_string(),
        Some(1),
        false,
        1,
        test_repair(),
        &[],
    );
    let data = result.data.as_ref().expect("failed result data");
    let stdout = data["stdout"].as_str().expect("sanitized stdout");
    let stderr = data["stderr"].as_str().expect("sanitized stderr");
    assert!(!stdout.contains("password"));
    assert!(!stdout.contains("secret"));
    assert!(!stderr.contains("secret"));
    assert!(stdout.len() <= 4096 + "...[truncated]".len());
}

#[test]
fn create_failed_result_sanitizes_persisted_output() {
    let result = create_failed_result(
        "v0.10.6",
        &test_repo(),
        "gh-command-failed",
        &GhCommandOutput {
            stdout: "token=secret".repeat(1000),
            stderr: "Authorization: Bearer secret".to_string(),
            exit_code: Some(1),
            timed_out: false,
        },
        test_repair(),
        &test_body(),
        None,
        &[],
    );
    let data = result.data.as_ref().expect("failed result data");
    assert!(!data["stdout"].as_str().unwrap().contains("secret"));
    assert!(!data["stderr"].as_str().unwrap().contains("secret"));
    assert!(data["stdout"].as_str().unwrap().len() <= 4096 + "...[truncated]".len());
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
fn publishing_a_stranded_draft_is_a_success_not_a_skip() {
    // Issue #10441: finishing a draft that was left behind a pushed tag is a
    // real delivery action. It must not be reported as an idempotent skip,
    // because the release only became downloadable in this run.
    let result = published_existing_draft_result(
        "v0.320.0",
        &test_repo(),
        15,
        "https://github.com/example-org/studio-web/releases/tag/v0.320.0",
    );

    assert_eq!(result.status, ReleaseStepStatus::Success);
    assert_eq!(data_bool(&result, "skipped"), Some(false));
    assert_eq!(data_bool(&result, "published"), Some(true));
    assert_eq!(data_str(&result, "reason"), Some("draft-release-published"));
    assert_eq!(
        data_str(&result, "action"),
        Some("github.release.publish_existing_draft")
    );
    assert_eq!(
        result
            .data
            .as_ref()
            .and_then(|data| data.get("artifact_count"))
            .and_then(|value| value.as_u64()),
        Some(15)
    );
}

#[test]
fn an_unfinished_release_is_failed_so_the_tag_is_never_reported_as_delivered() {
    // Issue #10441: the tag is already durable on origin by the time this step
    // runs. Reporting success over a release that is still a draft tells every
    // downstream consumer the version shipped when nothing is downloadable.
    let result = unfinished_release_result(
        "v0.320.0",
        &test_repo(),
        "draft-publish-failed",
        "`gh release edit --draft=false` failed for v0.320.0: HTTP 502",
        test_repair(),
        &[],
    );

    assert_eq!(result.status, ReleaseStepStatus::Failed);
    assert_eq!(data_bool(&result, "skipped"), Some(false));
    assert_eq!(data_bool(&result, "published"), Some(false));
    // The release object does exist — recovery must resume it, not re-tag.
    assert_eq!(data_bool(&result, "release_created"), Some(true));
    assert_eq!(data_str(&result, "reason"), Some("draft-publish-failed"));
    assert!(result.error.as_deref().unwrap().contains("HTTP 502"));
    // Recovery guidance must point at the existing draft, never at a new tag.
    assert!(result
        .hints
        .iter()
        .any(|hint| hint.message.contains("Resume the existing draft")));
    assert!(result
        .hints
        .iter()
        .any(|hint| hint.message.contains("--draft=false")));
    assert!(result
        .hints
        .iter()
        .all(|hint| !hint.message.contains("release create")));
}

#[test]
fn an_empty_draft_is_refused_with_the_same_failed_contract() {
    let result = unfinished_release_result(
        "v0.319.3",
        &test_repo(),
        "draft-release-has-no-assets",
        "Refusing to publish an empty release over the pushed tag.",
        test_repair(),
        &[],
    );

    assert_eq!(result.status, ReleaseStepStatus::Failed);
    assert_eq!(data_bool(&result, "published"), Some(false));
    assert_eq!(
        data_str(&result, "reason"),
        Some("draft-release-has-no-assets")
    );
}

#[test]
fn failed_results_retain_structured_command_diagnostics() {
    let output = GhCommandOutput {
        stdout: String::new(),
        stderr: "HTTP 403\nX-GitHub-Request-Id: request-123\nAuthorization: token ghp_secret"
            .to_string(),
        exit_code: Some(1),
        timed_out: false,
    };
    let diagnostic = gh_failure_diagnostic(
        "gh release edit --draft=false",
        "repos/example-org/studio-web/releases/v0.10.6",
        &output,
    );
    let result = unfinished_release_result(
        "v0.10.6",
        &test_repo(),
        "draft-publish-failed",
        "gh release edit --draft=false exited with status 1",
        test_repair(),
        &[diagnostic],
    );

    let details = result
        .data
        .as_ref()
        .and_then(|data| data.get("error_details"))
        .expect("structured diagnostic details retained");
    assert_eq!(details["code"], "github_command_failed");
    let failures = details
        .get("failures")
        .and_then(|value| value.as_array())
        .expect("structured diagnostic retained");
    assert_eq!(failures[0]["http_status"], 403);
    assert_eq!(failures[0]["github_request_id"], "request-123");
    assert!(!failures[0]["stderr"]
        .as_str()
        .unwrap()
        .contains("ghp_secret"));
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
