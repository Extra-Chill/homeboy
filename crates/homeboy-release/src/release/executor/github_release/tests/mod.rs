//! Test suite for the `github_release` module, grouped by concern so each test
//! file stays under the structural item threshold.

use homeboy_core::component::Component;
use homeboy_core::component::GithubConfig;
use homeboy_core::git::release_download::GitHubRepo;

use super::{github_release_repair_commands, GitHubReleaseBody, GitHubReleaseRepairCommands};

mod changelog_url_tests;
mod delivery_tests;
mod draft_lookup_tests;
mod notes_tests;
mod repair_tests;
mod result_builders;
mod start_tag_tests;
mod upload_recovery_tests;

pub(super) fn test_repo() -> GitHubRepo {
    GitHubRepo {
        host: "github.com".to_string(),
        owner: "example-org".to_string(),
        repo: "studio-web".to_string(),
    }
}

pub(super) fn test_repair() -> GitHubReleaseRepairCommands {
    github_release_repair_commands(
        "v0.10.6",
        &test_repo(),
        &GithubConfig::default(),
        &["build/studio-web.zip".to_string()],
        None,
        None,
    )
}

pub(super) fn test_body() -> GitHubReleaseBody {
    GitHubReleaseBody {
        body: "## What's Changed\n\n**Full Changelog**: https://example/CHANGELOG.md".to_string(),
        generated_notes_ok: true,
        changelog_url: Some("https://example/CHANGELOG.md".to_string()),
        source: "generated-notes".to_string(),
    }
}

pub(super) use homeboy_test_support::git::run as run_git;

pub(super) use homeboy_test_support::git::commit_file;

pub(super) fn git_repo() -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let dir = temp.path();
    run_git(dir, &["init", "-q", "-b", "main"]);
    run_git(dir, &["config", "user.email", "homeboy@example.com"]);
    run_git(dir, &["config", "user.name", "Homeboy Test"]);
    run_git(dir, &["config", "commit.gpgsign", "false"]);
    temp
}

pub(super) fn component_for_repo(dir: &std::path::Path) -> Component {
    Component {
        id: "test-component".to_string(),
        local_path: dir.to_string_lossy().to_string(),
        ..Default::default()
    }
}

pub(super) fn data_str<'a>(result: &'a super::ReleaseStepResult, key: &str) -> Option<&'a str> {
    result
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|value| value.as_str())
}

pub(super) fn data_bool(result: &super::ReleaseStepResult, key: &str) -> Option<bool> {
    result
        .data
        .as_ref()
        .and_then(|data| data.get(key))
        .and_then(|value| value.as_bool())
}
