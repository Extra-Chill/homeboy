//! Tests for release-notes start-tag resolution across branch topologies.

use super::super::{github_generated_notes_start_tag, github_release_notes_start_tag};
use super::{commit_file, component_for_repo, git_repo, run_git};
use crate::core::component::Component;

#[test]
fn generated_notes_start_tag_fails_closed_when_prior_release_tag_is_off_branch() {
    let temp = git_repo();
    let dir = temp.path();
    commit_file(dir, "README.md", "initial", "chore: initial");
    run_git(dir, &["tag", "v0.1.0"]);
    commit_file(
        dir,
        "feature.txt",
        "first release work",
        "feat: first release work",
    );
    run_git(dir, &["branch", "release-v0.2.0"]);
    run_git(dir, &["checkout", "release-v0.2.0"]);
    commit_file(dir, "VERSION", "0.2.0", "release: v0.2.0");
    run_git(dir, &["tag", "v0.2.0"]);
    run_git(dir, &["checkout", "main"]);
    commit_file(
        dir,
        "fix.txt",
        "second release work",
        "fix: second release work",
    );
    run_git(dir, &["tag", "v0.2.1"]);

    assert_eq!(
        crate::core::git::get_previous_tag_before_with_prefix(
            &dir.to_string_lossy(),
            "v0.2.1",
            None,
        )
        .expect("reachable previous tag lookup"),
        Some("v0.1.0".to_string()),
        "the old reachable-only lookup skips the off-branch v0.2.0 boundary"
    );

    let err = github_generated_notes_start_tag(&component_for_repo(dir), "v0.2.1")
        .expect_err("off-branch prior release tag should fail closed");
    assert!(err.message.contains("Previous release tag v0.2.0"));
    assert!(err
        .message
        .contains("not reachable from release tag v0.2.1"));
    assert!(err.message.contains("duplicate prior release ranges"));
}

#[test]
fn release_notes_start_tag_falls_back_when_prior_release_tag_is_off_branch() {
    let temp = git_repo();
    let dir = temp.path();
    commit_file(dir, "README.md", "initial", "chore: initial");
    run_git(dir, &["tag", "v0.1.0"]);
    commit_file(
        dir,
        "feature.txt",
        "first release work",
        "feat: first release work",
    );
    run_git(dir, &["branch", "release-v0.2.0"]);
    run_git(dir, &["checkout", "release-v0.2.0"]);
    commit_file(dir, "VERSION", "0.2.0", "release: v0.2.0");
    run_git(dir, &["tag", "v0.2.0"]);
    run_git(dir, &["checkout", "main"]);
    commit_file(
        dir,
        "fix.txt",
        "second release work",
        "fix: second release work",
    );
    run_git(dir, &["tag", "v0.2.1"]);

    assert_eq!(
        github_release_notes_start_tag(&component_for_repo(dir), "v0.2.1"),
        None,
        "release creation should fall back to Homeboy notes instead of failing before gh release create"
    );
}

#[test]
fn generated_notes_start_tag_uses_prior_release_tag_when_reachable() {
    let temp = git_repo();
    let dir = temp.path();
    commit_file(dir, "README.md", "initial", "chore: initial");
    run_git(dir, &["tag", "v0.1.0"]);
    commit_file(
        dir,
        "feature.txt",
        "first release work",
        "feat: first release work",
    );
    commit_file(dir, "VERSION", "0.2.0", "release: v0.2.0");
    run_git(dir, &["tag", "v0.2.0"]);
    commit_file(
        dir,
        "fix.txt",
        "second release work",
        "fix: second release work",
    );
    run_git(dir, &["tag", "v0.2.1"]);

    let start_tag = github_generated_notes_start_tag(&component_for_repo(dir), "v0.2.1")
        .expect("reachable previous tag should resolve");

    assert_eq!(start_tag.as_deref(), Some("v0.2.0"));
}

#[test]
fn generated_notes_start_tag_uses_package_tag_namespace() {
    let temp = git_repo();
    let dir = temp.path();
    commit_file(dir, "README.md", "initial", "chore: initial");
    run_git(dir, &["tag", "v2.10.0"]);
    run_git(dir, &["tag", "nodejs-v2.2.0"]);
    run_git(dir, &["tag", "wordpress-v3.22.1"]);
    commit_file(
        dir,
        "packages/nodejs/src/index.ts",
        "nodejs",
        "fix: update nodejs package",
    );
    run_git(dir, &["tag", "nodejs-v2.2.1"]);
    commit_file(
        dir,
        "packages/wordpress/src/index.ts",
        "wordpress",
        "fix: update wordpress package",
    );
    run_git(dir, &["tag", "wordpress-v3.22.2"]);

    let component = Component {
        id: "wordpress".to_string(),
        local_path: dir.join("packages/wordpress").to_string_lossy().to_string(),
        ..Default::default()
    };

    let start_tag = github_generated_notes_start_tag(&component, "wordpress-v3.22.2")
        .expect("package previous tag should resolve");

    assert_eq!(start_tag.as_deref(), Some("wordpress-v3.22.1"));
}
