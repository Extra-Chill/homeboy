//! Tests for release-notes fallback bodies and exact-body exposure (issue #3508).

use crate::core::release::types::ReleaseState;

use super::super::{create_failed_result, fallback_release_notes, GitHubReleaseBody};
use super::{data_str, test_body, test_repair, test_repo};

#[test]
fn fallback_release_notes_uses_changelog_notes_when_present() {
    let state = ReleaseState {
        notes: Some("## v0.10.6\n\n- Fixed a thing".to_string()),
        ..Default::default()
    };

    let notes = fallback_release_notes(
        &state,
        Some("https://github.com/example-org/studio-web/blob/v0.10.6/CHANGELOG.md"),
        "v0.10.6",
    );

    assert!(notes.contains("- Fixed a thing"));
    assert!(notes.contains(
        "**Full Changelog**: https://github.com/example-org/studio-web/blob/v0.10.6/CHANGELOG.md"
    ));
}

#[test]
fn fallback_release_notes_falls_back_to_minimal_body_when_empty() {
    let state = ReleaseState {
        notes: Some("   ".to_string()),
        ..Default::default()
    };

    let notes = fallback_release_notes(&state, None, "v0.10.6");

    assert_eq!(notes, "Release v0.10.6");
}

// ---- Issue #3508: the exact GitHub Release body must be discoverable ----

#[test]
fn release_body_source_label_distinguishes_generated_from_fallback() {
    let generated = GitHubReleaseBody {
        body: "x".to_string(),
        generated_notes_ok: true,
        changelog_url: None,
    };
    let fallback = GitHubReleaseBody {
        body: "x".to_string(),
        generated_notes_ok: false,
        changelog_url: None,
    };
    assert_eq!(generated.source_label(), "generated-notes");
    assert_eq!(fallback.source_label(), "changelog-fallback");
}

#[test]
fn create_failed_result_exposes_exact_release_body_and_persisted_file() {
    // Regression for #3508: a failed create must surface the EXACT body
    // Homeboy attempted to post plus its persisted-file path so manual
    // recovery reproduces the identical body instead of reconstructing it.
    let body = test_body();
    let result = create_failed_result(
        "v0.10.6",
        &test_repo(),
        "generated-notes-failed",
        String::new(),
        "HTTP 502".to_string(),
        test_repair(),
        &body,
        Some("build/v0.10.6-release-notes.md"),
    );

    assert_eq!(data_str(&result, "release_body"), Some(body.body.as_str()));
    assert_eq!(
        data_str(&result, "release_body_source"),
        Some("generated-notes")
    );
    assert_eq!(
        data_str(&result, "release_body_file"),
        Some("build/v0.10.6-release-notes.md")
    );
    // The exact body must carry the changelog link footer.
    assert!(data_str(&result, "release_body")
        .unwrap()
        .contains("**Full Changelog**:"));
}
