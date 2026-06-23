//! Tests for `github_changelog_url`, covering both root-level components and
//! components that live in a monorepo subdirectory (issue #6146).

use crate::core::component::Component;

use super::super::github_changelog_url;
use super::{commit_file, git_repo, run_git, test_repo};

#[test]
fn changelog_url_for_root_component_omits_subdirectory() {
    let repo = git_repo();
    let dir = repo.path();
    commit_file(dir, "CHANGELOG.md", "# Changelog\n", "chore: add changelog");

    let component = Component {
        id: "homeboy".to_string(),
        local_path: dir.to_string_lossy().to_string(),
        changelog_target: Some("CHANGELOG.md".to_string()),
        ..Default::default()
    };

    let url = github_changelog_url(&component, &test_repo(), "v1.2.3")
        .expect("changelog url should be built");

    assert_eq!(
        url,
        "https://github.com/example-org/studio-web/blob/v1.2.3/CHANGELOG.md"
    );
}

#[test]
fn changelog_url_for_subdirectory_component_includes_path_prefix() {
    let repo = git_repo();
    let root = repo.path();

    // Component lives at <repo>/php-transformer with its own changelog.
    let component_dir = root.join("php-transformer");
    std::fs::create_dir_all(&component_dir).expect("create component subdir");
    std::fs::write(component_dir.join("CHANGELOG.md"), "# Changelog\n")
        .expect("write component changelog");
    run_git(root, &["add", "."]);
    run_git(root, &["commit", "-q", "-m", "chore: add component"]);

    let component = Component {
        id: "php-transformer".to_string(),
        local_path: component_dir.to_string_lossy().to_string(),
        changelog_target: Some("CHANGELOG.md".to_string()),
        ..Default::default()
    };

    let url = github_changelog_url(&component, &test_repo(), "php-transformer-v0.1.3")
        .expect("changelog url should be built");

    assert_eq!(
        url,
        "https://github.com/example-org/studio-web/blob/php-transformer-v0.1.3/php-transformer/CHANGELOG.md"
    );
}
