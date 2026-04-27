//! Shared GitHub PR metadata lookup for stack verbs.

use std::process::Command;

use crate::error::{Error, Result};

use super::spec::StackPrEntry;

/// PR head info extracted from `gh pr view`.
#[derive(Debug, Clone)]
pub(crate) struct PrHead {
    pub(crate) sha: String,
    /// `<owner>/<name>` of the head repo (may differ from the PR's base repo
    /// if the PR was opened from a fork).
    pub(crate) head_repo: String,
    /// `https://github.com/<owner>/<name>.git` — used as fetch URL for any
    /// temp remote we add.
    pub(crate) clone_url: String,
}

/// Superset of `gh pr view` fields used by stack apply/status/sync.
#[derive(Debug, Clone)]
pub(crate) struct StackPrMeta {
    pub(crate) head_sha: String,
    pub(crate) head_owner: Option<String>,
    pub(crate) head_name: Option<String>,
    pub(crate) state: String,
    pub(crate) title: Option<String>,
    pub(crate) url: Option<String>,
    pub(crate) review_decision: Option<String>,
    pub(crate) merged_at: Option<String>,
}

impl StackPrMeta {
    /// Convert to the fetchable PR-head contract used by apply/sync.
    pub(crate) fn require_head(&self, pr: &StackPrEntry) -> Result<PrHead> {
        if self.head_sha.is_empty() {
            return Err(Error::git_command_failed(format!(
                "gh pr view {}#{} returned no headRefOid",
                pr.repo, pr.number
            )));
        }
        let head_owner = self.head_owner.as_deref().ok_or_else(|| {
            Error::git_command_failed(format!(
                "gh pr view {}#{} returned no headRepositoryOwner.login",
                pr.repo, pr.number
            ))
        })?;
        let head_name = self.head_name.as_deref().ok_or_else(|| {
            Error::git_command_failed(format!(
                "gh pr view {}#{} returned no headRepository.name",
                pr.repo, pr.number
            ))
        })?;
        let head_repo = format!("{}/{}", head_owner, head_name);
        Ok(PrHead {
            sha: self.head_sha.clone(),
            clone_url: format!("https://github.com/{}.git", head_repo),
            head_repo,
        })
    }

    pub(crate) fn title_for_status(&self) -> String {
        self.title.clone().unwrap_or_default()
    }

    pub(crate) fn url_for_status(&self) -> String {
        self.url.clone().unwrap_or_default()
    }
}

/// Resolve PR metadata via `gh pr view`.
pub(crate) fn fetch_pr_meta(pr: &StackPrEntry) -> Result<StackPrMeta> {
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr.number.to_string(),
            "--repo",
            &pr.repo,
            "--json",
            "headRefOid,headRepository,headRepositoryOwner,state,title,url,reviewDecision,mergedAt",
        ])
        .output()
        .map_err(|e| {
            Error::git_command_failed(format!(
                "gh pr view {}#{}: {} (is `gh` installed and authenticated?)",
                pr.repo, pr.number, e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "gh pr view {}#{} failed: {}",
            pr.repo,
            pr.number,
            stderr.trim()
        )));
    }

    parse_pr_meta(pr, &String::from_utf8_lossy(&output.stdout))
}

pub(crate) fn parse_pr_meta(pr: &StackPrEntry, stdout: &str) -> Result<StackPrMeta> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse `gh pr view {}#{}`", pr.repo, pr.number)),
            Some(stdout.chars().take(200).collect()),
        )
    })?;

    Ok(StackPrMeta {
        head_sha: string_field(&parsed, "headRefOid").unwrap_or_default(),
        head_owner: nested_string_field(&parsed, "headRepositoryOwner", "login"),
        head_name: nested_string_field(&parsed, "headRepository", "name"),
        state: string_field(&parsed, "state").unwrap_or_default(),
        title: string_field(&parsed, "title"),
        url: string_field(&parsed, "url"),
        review_decision: non_empty_string_field(&parsed, "reviewDecision"),
        merged_at: non_empty_string_field(&parsed, "mergedAt"),
    })
}

fn string_field(parsed: &serde_json::Value, key: &str) -> Option<String> {
    parsed.get(key).and_then(|v| v.as_str()).map(str::to_string)
}

fn non_empty_string_field(parsed: &serde_json::Value, key: &str) -> Option<String> {
    string_field(parsed, key).filter(|s| !s.is_empty())
}

fn nested_string_field(parsed: &serde_json::Value, key: &str, child: &str) -> Option<String> {
    parsed
        .get(key)
        .and_then(|v| v.get(child))
        .and_then(|v| v.as_str())
        .map(str::to_string)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ErrorCode;
    use std::fs;

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    fn pr() -> StackPrEntry {
        StackPrEntry {
            repo: "Extra-Chill/homeboy".to_string(),
            number: 1653,
            note: None,
        }
    }

    fn full_json() -> &'static str {
        r#"{
            "headRefOid": "abc123",
            "headRepositoryOwner": { "login": "Extra-Chill" },
            "headRepository": { "name": "homeboy" },
            "state": "MERGED",
            "title": "Clean stack internals",
            "url": "https://github.com/Extra-Chill/homeboy/pull/1",
            "reviewDecision": "APPROVED",
            "mergedAt": "2026-04-26T00:00:00Z"
        }"#
    }

    #[test]
    fn test_fetch_pr_meta() {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let gh = dir.path().join("gh");
        fs::write(
            &gh,
            format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", full_json()),
        )
        .unwrap();
        #[cfg(unix)]
        fs::set_permissions(&gh, fs::Permissions::from_mode(0o755)).unwrap();

        let old_path = std::env::var("PATH").unwrap_or_default();
        std::env::set_var("PATH", format!("{}:{}", dir.path().display(), old_path));

        let meta = fetch_pr_meta(&pr()).expect("fetch fake gh metadata");

        std::env::set_var("PATH", old_path);

        assert_eq!(meta.head_sha, "abc123");
        assert_eq!(meta.title.as_deref(), Some("Clean stack internals"));
    }

    #[test]
    fn test_parse_pr_meta() {
        let meta = parse_pr_meta(&pr(), full_json()).expect("parse metadata");

        assert_eq!(meta.head_sha, "abc123");
        assert_eq!(meta.state, "MERGED");
        assert_eq!(meta.review_decision.as_deref(), Some("APPROVED"));
        assert_eq!(meta.merged_at.as_deref(), Some("2026-04-26T00:00:00Z"));
    }

    #[test]
    fn malformed_pr_metadata_surfaces_json_error() {
        let err = parse_pr_meta(&pr(), "not json").unwrap_err();
        assert_eq!(err.code, ErrorCode::ValidationInvalidJson);
        assert_eq!(
            err.details.get("context").and_then(|v| v.as_str()),
            Some("parse `gh pr view Extra-Chill/homeboy#1653`")
        );
    }

    #[test]
    fn test_require_head() {
        let meta = parse_pr_meta(&pr(), full_json()).expect("parse metadata");
        let head = meta.require_head(&pr()).expect("required head");
        assert_eq!(head.sha, "abc123");
        assert_eq!(head.head_repo, "Extra-Chill/homeboy");
        assert_eq!(head.clone_url, "https://github.com/Extra-Chill/homeboy.git");

        let missing = parse_pr_meta(&pr(), r#"{ "state": "OPEN" }"#).expect("parse missing head");
        assert!(missing
            .require_head(&pr())
            .unwrap_err()
            .to_string()
            .contains("returned no headRefOid"));
    }

    #[test]
    fn test_title_for_status() {
        let meta = parse_pr_meta(&pr(), full_json()).expect("parse metadata");
        assert_eq!(meta.title_for_status(), "Clean stack internals");

        let missing = parse_pr_meta(&pr(), r#"{ "state": "OPEN" }"#).expect("parse missing title");
        assert_eq!(missing.title_for_status(), "");
    }

    #[test]
    fn test_url_for_status() {
        let meta = parse_pr_meta(&pr(), full_json()).expect("parse metadata");
        assert_eq!(
            meta.url_for_status(),
            "https://github.com/Extra-Chill/homeboy/pull/1"
        );

        let missing = parse_pr_meta(&pr(), r#"{ "state": "OPEN" }"#).expect("parse missing url");
        assert_eq!(missing.url_for_status(), "");
    }

    #[test]
    fn missing_head_repo_coordinates_have_specific_errors() {
        let missing_owner = parse_pr_meta(
            &pr(),
            r#"{ "headRefOid": "abc123", "headRepository": { "name": "homeboy" } }"#,
        )
        .expect("parse missing owner");
        assert!(missing_owner
            .require_head(&pr())
            .unwrap_err()
            .to_string()
            .contains("headRepositoryOwner.login"));

        let missing_name = parse_pr_meta(
            &pr(),
            r#"{ "headRefOid": "abc123", "headRepositoryOwner": { "login": "Extra-Chill" } }"#,
        )
        .expect("parse missing name");
        assert!(missing_name
            .require_head(&pr())
            .unwrap_err()
            .to_string()
            .contains("headRepository.name"));
    }
}
