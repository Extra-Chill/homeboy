//! Tests for release-notes fallback bodies and exact-body exposure (issue #3508).

use crate::core::release::types::ReleaseState;

use super::super::{
    build_github_release_body, create_failed_result, fallback_release_notes, GitHubReleaseBody,
};
use super::{data_str, git_repo, test_body, test_repair, test_repo};

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

#[test]
fn component_scoped_release_body_uses_changelog_fallback() {
    let temp = git_repo();
    let component_dir = temp.path().join("packages/wordpress");
    std::fs::create_dir_all(&component_dir).expect("create component dir");
    let component = crate::core::component::Component {
        id: "wordpress".to_string(),
        local_path: component_dir.to_string_lossy().to_string(),
        ..Default::default()
    };
    let state = ReleaseState {
        notes: Some("## wordpress-v1.2.3\n\n- Fix scoped release notes".to_string()),
        ..Default::default()
    };

    let body = build_github_release_body(
        &component,
        &test_repo(),
        "wordpress-v1.2.3",
        &state,
        Some("https://github.com/example-org/studio-web/blob/wordpress-v1.2.3/packages/wordpress/CHANGELOG.md"),
        Some("wordpress-v1.2.2"),
    );

    assert!(!body.generated_notes_ok);
    assert_eq!(body.source_label(), "changelog-fallback");
    assert!(body.body.contains("- Fix scoped release notes"));
    assert!(body.body.contains("packages/wordpress/CHANGELOG.md"));
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
