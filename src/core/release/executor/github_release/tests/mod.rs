//! Test suite for the `github_release` module, grouped by concern so each test
//! file stays under the structural item threshold.

use crate::core::component::Component;
use crate::core::component::GithubConfig;
use crate::core::deploy::release_download::GitHubRepo;

use super::{github_release_repair_commands, GitHubReleaseBody, GitHubReleaseRepairCommands};

mod notes_tests;
mod repair_tests;
mod result_builders;
mod start_tag_tests;

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
    }
}

pub(super) fn run_git(dir: &std::path::Path, args: &[&str]) {
    let output = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("run git");
    assert!(
        output.status.success(),
        "git {:?} failed: stdout={} stderr={}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

pub(super) fn commit_file(dir: &std::path::Path, name: &str, content: &str, message: &str) {
    std::fs::write(dir.join(name), content).expect("write fixture file");
    run_git(dir, &["add", name]);
    run_git(dir, &["commit", "-q", "-m", message]);
}

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
