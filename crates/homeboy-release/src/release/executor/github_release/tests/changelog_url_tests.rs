//! Tests for `github_changelog_url`, covering both root-level components and
//! components that live in a monorepo subdirectory (issue #6146).

use homeboy_core::component::Component;

use super::super::github_changelog_url;
use super::{commit_file, git_repo, test_repo};

#[test]
fn changelog_url_is_relative_to_the_repository_root() {
    let repo = git_repo();
    let root = repo.path();
    commit_file(
        root,
        "CHANGELOG.md",
        "# Changelog\n",
        "chore: add changelog",
    );
    // A second component lives at <repo>/php-transformer with its own changelog.
    commit_file(
        root,
        "php-transformer/CHANGELOG.md",
        "# Changelog\n",
        "chore: add component",
    );

    for (id, local_path, tag, expected) in [
        (
            "homeboy",
            root.to_path_buf(),
            "v1.2.3",
            "https://github.com/example-org/studio-web/blob/v1.2.3/CHANGELOG.md",
        ),
        (
            "php-transformer",
            root.join("php-transformer"),
            "php-transformer-v0.1.3",
            "https://github.com/example-org/studio-web/blob/php-transformer-v0.1.3/php-transformer/CHANGELOG.md",
        ),
    ] {
        let component = Component {
            id: id.to_string(),
            local_path: local_path.to_string_lossy().to_string(),
            changelog_target: Some("CHANGELOG.md".to_string()),
            ..Default::default()
        };

        let url = github_changelog_url(&component, &test_repo(), tag)
            .expect("changelog url should be built");

        assert_eq!(url, expected);
    }
}
