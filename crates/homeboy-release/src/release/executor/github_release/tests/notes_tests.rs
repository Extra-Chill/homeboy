//! Tests for release-notes fallback bodies and exact-body exposure (issue #3508).

use crate::release::types::ReleaseState;

use super::super::gh_cli::GhCommandOutput;
use super::super::{
    component_scoped_release_entries, create_failed_result, fallback_release_notes,
    AssociatedPullRequest, AssociatedPullRequestAuthor, GitHubReleaseBody,
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
fn component_scoped_blocks_engine_notes_attribute_prs_without_including_siblings() {
    let temp = git_repo();
    let root = temp.path();
    let component_dir = temp.path().join("php-transformer");
    std::fs::create_dir_all(&component_dir).expect("create component dir");
    std::fs::write(root.join("README.md"), "initial").expect("write initial file");
    super::run_git(root, &["add", "README.md"]);
    super::run_git(root, &["commit", "-q", "-m", "chore: initial"]);
    super::run_git(root, &["tag", "php-transformer-v0.4.15"]);
    super::commit_file(
        root,
        "php-transformer/src/crop.php",
        "crop",
        "fix: generic crop recognition",
    );
    super::commit_file(
        root,
        "php-transformer/src/crop-test.php",
        "test",
        "fix: crop coverage",
    );
    super::commit_file(
        root,
        "php-transformer/tools/visual-parity/figma.ts",
        "figma",
        "fix: sibling-only change",
    );
    super::commit_file(
        root,
        "php-transformer/src/direct.php",
        "direct",
        "fix: direct push fallback",
    );
    let component = homeboy_core::component::Component {
        id: "php-transformer".to_string(),
        local_path: component_dir.to_string_lossy().to_string(),
        scopes: Some(homeboy_core::component::ScopeConfig {
            release: Some(homeboy_core::component::CommandScopeConfig {
                include: vec![],
                exclude: vec!["tools/visual-parity/**".to_string()],
            }),
            ..Default::default()
        }),
        ..Default::default()
    };
    let commits = homeboy_core::git::get_component_changes_since_tag(
        &component,
        Some("php-transformer-v0.4.15"),
    )
    .expect("scoped commits");
    assert_eq!(
        commits.len(),
        3,
        "sibling-only commit remains outside the authority set"
    );
    let first_pr = AssociatedPullRequest {
        title: "Improve generic image crop recognition".to_string(),
        html_url: "https://github.com/Automattic/blocks-engine/pull/848".to_string(),
        merged_at: Some("2026-08-12T00:00:00Z".to_string()),
        user: AssociatedPullRequestAuthor {
            login: "chubes4".to_string(),
        },
    };
    let entries = component_scoped_release_entries(commits, |hash| {
        let subject = homeboy_core::git::execute_git_for_release(
            &component.local_path,
            &["show", "-s", "--format=%s", hash],
        )
        .expect("subject");
        match String::from_utf8_lossy(&subject.stdout).trim() {
            "fix: generic crop recognition" | "fix: crop coverage" => Some(vec![first_pr.clone()]),
            "fix: direct push fallback" => Some(vec![]),
            _ => None,
        }
    });

    assert_eq!(
        entries.len(),
        2,
        "two selected commits in one PR render once"
    );
    assert!(entries.contains(&"* Improve generic image crop recognition by @chubes4 in https://github.com/Automattic/blocks-engine/pull/848".to_string()));
    assert!(entries.contains(&"* direct push fallback".to_string()));
    assert!(entries.iter().all(|entry| !entry.contains("sibling-only")));
}

#[test]
fn component_scoped_notes_leave_ambiguous_or_unavailable_pr_metadata_unattributed() {
    let commits = vec![
        homeboy_core::git::CommitInfo {
            hash: "ambiguous".to_string(),
            subject: "fix: ambiguous association".to_string(),
            category: homeboy_core::git::CommitCategory::Fix,
        },
        homeboy_core::git::CommitInfo {
            hash: "unavailable".to_string(),
            subject: "fix: metadata unavailable".to_string(),
            category: homeboy_core::git::CommitCategory::Fix,
        },
    ];
    let pull_request = |number: u64, title: &str| AssociatedPullRequest {
        title: title.to_string(),
        html_url: format!("https://github.com/Automattic/blocks-engine/pull/{number}"),
        merged_at: Some("2026-08-12T00:00:00Z".to_string()),
        user: AssociatedPullRequestAuthor {
            login: "chubes4".to_string(),
        },
    };

    let entries = component_scoped_release_entries(commits, |hash| match hash {
        "ambiguous" => Some(vec![
            pull_request(848, "First PR"),
            pull_request(849, "Second PR"),
        ]),
        _ => None,
    });

    assert_eq!(
        entries,
        vec!["* ambiguous association", "* metadata unavailable"]
    );
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
        &GhCommandOutput {
            stdout: String::new(),
            stderr: "HTTP 502".to_string(),
            exit_code: Some(1),
            timed_out: false,
        },
        test_repair(),
        &body,
        Some("build/v0.10.6-release-notes.md"),
        &[],
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
