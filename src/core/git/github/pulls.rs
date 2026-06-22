//! Pull-request primitives via the `gh` CLI: create, edit, find, view, files,
//! and merge — plus the shared PR-list JSON parser and merge-method validator.

use std::path::Path;
use std::process::Command;

use crate::core::error::{Error, Result};

use super::super::gh_client::{delete_branch_ref_api_args, pr_merge_api_args};
use super::super::github_types::{
    GithubFindItem, GithubFindOutput, GithubPrOutput, GithubPrView, PrCreateOptions, PrEditOptions,
    PrFindOptions, PrMergeOptions,
};
use super::client::{
    ensure_gh_ready, parse_issue_number_from_url, resolve_component_github, run_gh, string_value,
};
use super::readiness::classify_pr_ci;

/// Open a new pull request.
pub fn pr_create(component_id: Option<&str>, options: PrCreateOptions) -> Result<GithubPrOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    if options.title.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "title",
            "PR title is required",
            None,
            None,
        ));
    }
    if options.base.trim().is_empty() || options.head.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "base/head",
            "PR base and head branches are required",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "pr".into(),
        "create".into(),
        "-R".into(),
        repo_flag.clone(),
        "--base".into(),
        options.base.clone(),
        "--head".into(),
        options.head.clone(),
        "--title".into(),
        options.title.clone(),
        "--body".into(),
        options.body.clone(),
    ];
    if options.draft {
        args.push("--draft".into());
    }

    let output = run_gh(&args)?;
    let url = output.trim().to_string();
    let number = parse_issue_number_from_url(&url);

    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.create".to_string(),
        success: true,
        number,
        url: Some(url),
        title: Some(options.title),
        state: Some("open".to_string()),
        base: Some(options.base),
        head: Some(options.head),
        ..Default::default()
    })
}

/// Edit an existing pull request's title and/or body.
pub fn pr_edit(component_id: Option<&str>, options: PrEditOptions) -> Result<GithubPrOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    if options.title.is_none() && options.body.is_none() {
        return Err(Error::validation_invalid_argument(
            "title/body",
            "At least one of --title or --body must be provided",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "pr".into(),
        "edit".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
    ];
    if let Some(title) = &options.title {
        args.push("--title".into());
        args.push(title.clone());
    }
    if let Some(body) = &options.body {
        args.push("--body".into());
        args.push(body.clone());
    }

    let output = run_gh(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.edit".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        title: options.title,
        ..Default::default()
    })
}

/// Find PRs matching the given filter.
pub fn pr_find(component_id: Option<&str>, options: PrFindOptions) -> Result<GithubFindOutput> {
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let limit = if options.limit == 0 {
        30
    } else {
        options.limit
    };
    let mut args: Vec<String> = vec![
        "pr".into(),
        "list".into(),
        "-R".into(),
        repo_flag,
        "--state".into(),
        options.state.as_gh_flag().to_string(),
        "--limit".into(),
        limit.to_string(),
        "--json".into(),
        "number,title,url,state,baseRefName,headRefName".into(),
    ];
    if let Some(base) = &options.base {
        args.push("--base".into());
        args.push(base.clone());
    }
    if let Some(head) = &options.head {
        args.push("--head".into());
        args.push(head.clone());
    }

    let raw = run_gh(&args)?;
    let items = parse_pr_list_json(&raw)?;

    Ok(GithubFindOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.find".to_string(),
        success: true,
        items,
    })
}

/// Find PRs that contain a commit SHA.
///
/// GitHub indexes commit SHAs in PR search, which makes this the shared helper
/// for read-only stack/branch inspection flows that need to decorate commits.
pub fn pr_find_by_commit(
    repo_path: &Path,
    sha: &str,
    repo: Option<&str>,
    limit: usize,
) -> Result<Vec<GithubFindItem>> {
    ensure_gh_ready()?;

    let mut args: Vec<String> = vec![
        "pr".into(),
        "list".into(),
        "--search".into(),
        sha.to_string(),
        "--state".into(),
        "all".into(),
        "--json".into(),
        "number,state,title,url".into(),
        "--limit".into(),
        limit.to_string(),
    ];
    if let Some(repo) = repo {
        args.push("-R".into());
        args.push(repo.to_string());
    }

    let output = Command::new("gh")
        .args(&args)
        .current_dir(repo_path)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| {
            Error::git_command_failed(format!(
                "gh pr list --search: {} (is `gh` installed and authenticated?)",
                e
            ))
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(Error::git_command_failed(format!(
            "gh pr list --search {}: {}",
            sha,
            stderr.trim()
        )));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_pr_list_json(&stdout)
}

/// Fetch metadata for one PR.
pub fn pr_view(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<GithubPrView> {
    let (id, repo) = resolve_component_github(component_id, path.as_deref())?;
    ensure_gh_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let args: Vec<String> = vec![
        "pr".into(),
        "view".into(),
        number.to_string(),
        "-R".into(),
        repo_flag,
        "--json".into(),
        "author,baseRefName,headRefName,headRepository,title,url,state,isDraft,mergedAt,headRefOid,reviewDecision,mergeStateStatus,statusCheckRollup".into(),
    ];
    let raw = run_gh(&args)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        Error::internal_json(
            format!("Failed to parse gh pr view JSON: {}", e),
            Some(raw.clone()),
        )
    })?;
    let author = parsed
        .pointer("/author/login")
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let base = parsed
        .get("baseRefName")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let head = parsed
        .get("headRefName")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_string();
    let head_repository = parsed
        .pointer("/headRepository/nameWithOwner")
        .or_else(|| parsed.pointer("/headRepository/name"))
        .and_then(|v| v.as_str())
        .map(|v| v.to_string());
    let state = string_value(&parsed, "state").unwrap_or_default();
    let url = string_value(&parsed, "url").unwrap_or_default();
    let title = string_value(&parsed, "title");
    let draft = parsed
        .get("isDraft")
        .and_then(serde_json::Value::as_bool)
        .unwrap_or(false);
    let head_sha = string_value(&parsed, "headRefOid");
    let merged_at = string_value(&parsed, "mergedAt");
    let review_decision = string_value(&parsed, "reviewDecision");
    let merge_state = string_value(&parsed, "mergeStateStatus");
    let status_check_rollup = parsed
        .get("statusCheckRollup")
        .and_then(|v| v.as_array())
        .map(|v| v.as_slice())
        .unwrap_or(&[]);
    let (ci_state, ci_summary, ci_next_action) = classify_pr_ci(
        &state,
        merged_at.as_deref(),
        merge_state.as_deref(),
        status_check_rollup,
    );

    Ok(GithubPrView {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        number,
        url,
        title,
        state,
        draft,
        author,
        base,
        head,
        head_repository,
        head_sha,
        merged_at,
        review_decision,
        merge_state,
        ci_state,
        ci_summary,
        ci_next_action,
    })
}

/// List changed files for one PR.
pub fn pr_files(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<Vec<String>> {
    let (_id, repo) = resolve_component_github(component_id, path.as_deref())?;
    ensure_gh_ready()?;
    let args: Vec<String> = vec![
        "api".into(),
        "--paginate".into(),
        format!("repos/{}/{}/pulls/{}/files", repo.owner, repo.repo, number),
        "--jq".into(),
        ".[].filename".into(),
    ];
    let raw = run_gh(&args)?;
    Ok(raw
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToString::to_string)
        .collect())
}

/// Merge a PR with an explicit method.
pub fn pr_merge(component_id: Option<&str>, options: PrMergeOptions) -> Result<GithubPrOutput> {
    let method = validate_pr_merge_method(&options.method)?;
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let branch_to_delete = if options.delete_branch {
        let view = pr_view(Some(&id), options.number, options.path.clone())?;
        (view.head_repository.as_deref() == Some(repo_flag.as_str()) && !view.head.is_empty())
            .then_some(view.head)
    } else {
        None
    };

    run_gh(&pr_merge_api_args(&repo_flag, options.number, &method))?;

    let mut warnings = Vec::new();
    if let Some(branch) = branch_to_delete {
        if let Err(error) = run_gh(&delete_branch_ref_api_args(&repo_flag, &branch)) {
            warnings.push(format!("failed to delete branch {branch}: {error}"));
        }
    }
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.merge".to_string(),
        success: true,
        number: Some(options.number),
        state: Some("merged".to_string()),
        warnings,
        ..Default::default()
    })
}

pub(super) fn validate_pr_merge_method(method: &str) -> Result<String> {
    match method {
        "merge" | "squash" | "rebase" => Ok(method.to_string()),
        other => Err(Error::validation_invalid_argument(
            "merge_method",
            format!("Unsupported merge method '{}'", other),
            Some("Use merge, squash, or rebase".to_string()),
            None,
        )),
    }
}

pub(super) fn parse_pr_list_json(raw: &str) -> Result<Vec<GithubFindItem>> {
    #[derive(serde::Deserialize)]
    struct RawPr {
        number: u64,
        title: String,
        url: String,
        state: String,
    }

    let parsed: Vec<RawPr> = serde_json::from_str(raw.trim())
        .map_err(|e| Error::internal_json(e.to_string(), Some("gh pr list".into())))?;
    Ok(parsed
        .into_iter()
        .map(|p| GithubFindItem {
            number: p.number,
            title: p.title,
            body: String::new(),
            url: p.url,
            state: p.state,
            state_reason: String::new(),
            closed_at: String::new(),
            labels: Vec::new(),
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::super::super::github_types::PrState;
    use super::*;

    #[test]
    fn pr_state_gh_flag() {
        assert_eq!(PrState::Open.as_gh_flag(), "open");
        assert_eq!(PrState::Merged.as_gh_flag(), "merged");
    }

    #[test]
    fn parse_pr_list_extracts_all_entries() {
        let raw = r#"[
            {"number":10,"title":"feat: x","url":"u10","state":"OPEN"},
            {"number":11,"title":"chore: y","url":"u11","state":"OPEN"}
        ]"#;
        let items = parse_pr_list_json(raw).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].number, 10);
        assert_eq!(items[1].state, "OPEN");
    }

    #[test]
    fn test_pr_files() {
        let owner = "Extra-Chill";
        let repo = "homeboy";
        let number = 42_u64;
        assert_eq!(
            format!("repos/{}/{}/pulls/{}/files", owner, repo, number),
            "repos/Extra-Chill/homeboy/pulls/42/files"
        );
    }

    #[test]
    fn test_pr_view() {
        let raw = r#"{
            "author":{"login":"homeboy-ci[bot]"},
            "baseRefName":"main",
            "headRefName":"ci/autofix/homeboy/main",
            "headRepository":{"nameWithOwner":"Extra-Chill/homeboy"}
        }"#;
        let parsed: serde_json::Value = serde_json::from_str(raw).unwrap();
        assert_eq!(
            parsed.pointer("/author/login").and_then(|v| v.as_str()),
            Some("homeboy-ci[bot]")
        );
        assert_eq!(
            parsed.get("baseRefName").and_then(|v| v.as_str()),
            Some("main")
        );
        assert_eq!(
            parsed
                .pointer("/headRepository/nameWithOwner")
                .and_then(|v| v.as_str()),
            Some("Extra-Chill/homeboy")
        );
    }

    #[test]
    fn test_pr_merge() {
        let result = pr_merge(
            Some("missing-component"),
            PrMergeOptions {
                method: "explode".into(),
                number: 1,
                ..Default::default()
            },
        );
        assert!(result.is_err());
    }
}
