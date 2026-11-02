//! Pull-request primitives via the `gh` CLI: create, edit, find, view, files,
//! merge, and ready-for-review — plus the shared PR-list JSON parser and merge-method validator.

use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

use super::super::gh_client::{delete_branch_ref_api_args, pr_merge_api_args, GhClient};
use super::super::github_types::{
    GithubFindItem, GithubFindOutput, GithubPrOutput, GithubPrView, PrCreateOptions, PrEditOptions,
    PrFindOptions, PrMergeOptions,
};
use super::client::{
    default_gh_host, parse_html_url, parse_issue_number_from_url, resolve_component_github,
};
use super::pr_view_record::{parse_pr_view, pr_view_args};
use super::readiness::classify_pr_ci;
use super::{push_api_markdown_body_file_arg, push_markdown_body_file_arg};

/// Open a new pull request.
pub fn pr_create(component_id: Option<&str>, options: PrCreateOptions) -> Result<GithubPrOutput> {
    pr_create_with_token(component_id, options, None)
}

/// Create a PR with an invocation-local credential, without changing host login.
pub fn pr_create_with_token(
    component_id: Option<&str>,
    options: PrCreateOptions,
    token: Option<&str>,
) -> Result<GithubPrOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    let gh = match token {
        Some(token) => gh.with_token(token),
        None => gh,
    };
    gh.ensure_ready()?;

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
    ];
    let mut body_files = Vec::new();
    push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", &options.body)?;
    if options.draft {
        args.push("--draft".into());
    }

    let output = gh.run(&args)?;
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
///
/// Goes through REST (`gh api -X PATCH repos/{owner}/{repo}/pulls/{n}`) rather
/// than `gh pr edit`: gh 2.45's `pr edit` queries the deprecated GraphQL
/// `projectCards` field, which GitHub now rejects, failing the whole edit even
/// though nothing project-related was requested.
pub fn pr_edit(component_id: Option<&str>, options: PrEditOptions) -> Result<GithubPrOutput> {
    pr_edit_with_token(component_id, options, None)
}

pub fn pr_edit_with_token(
    component_id: Option<&str>,
    options: PrEditOptions,
    token: Option<&str>,
) -> Result<GithubPrOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    let gh = match token {
        Some(token) => gh.with_token(token),
        None => gh,
    };
    gh.ensure_ready()?;

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
        "api".into(),
        "-X".into(),
        "PATCH".into(),
        format!("repos/{}/pulls/{}", repo_flag, options.number),
    ];
    let mut body_files = Vec::new();
    if let Some(title) = &options.title {
        args.push("-f".into());
        args.push(format!("title={title}"));
    }
    if let Some(body) = &options.body {
        push_api_markdown_body_file_arg(&mut args, &mut body_files, "body", body)?;
    }

    let output = gh.run(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.edit".to_string(),
        success: true,
        number: Some(options.number),
        url: parse_html_url(&output),
        title: options.title,
        ..Default::default()
    })
}

/// Mark a draft pull request ready for review.
///
/// The transition is idempotent: an already-ready PR is observed but not mutated.
pub fn pr_ready(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<GithubPrOutput> {
    let view = pr_view(component_id, number, path.clone())?;
    if !view.draft {
        return Ok(GithubPrOutput {
            component_id: view.component_id,
            owner: view.owner,
            repo: view.repo,
            action: "pr.ready".to_string(),
            success: true,
            number: Some(number),
            url: Some(view.url),
            state: Some(view.state),
            ..Default::default()
        });
    }

    let (id, repo, gh) = resolve_component_github(component_id, path.as_deref())?;
    gh.ensure_ready()?;
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    gh.run(&pr_ready_args(&repo_flag, number))?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.ready".to_string(),
        success: true,
        number: Some(number),
        url: Some(view.url),
        state: Some(view.state),
        ..Default::default()
    })
}

fn pr_ready_args(repo: &str, number: u64) -> Vec<String> {
    vec![
        "pr".to_string(),
        "ready".to_string(),
        number.to_string(),
        "-R".to_string(),
        repo.to_string(),
    ]
}

/// Find PRs matching the given filter.
pub fn pr_find(component_id: Option<&str>, options: PrFindOptions) -> Result<GithubFindOutput> {
    pr_find_with_token(component_id, options, None)
}

pub fn pr_find_with_token(
    component_id: Option<&str>,
    options: PrFindOptions,
    token: Option<&str>,
) -> Result<GithubFindOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    let gh = match token {
        Some(token) => gh.with_token(token),
        None => gh,
    };
    gh.ensure_ready()?;

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
        "number,title,url,state,isDraft,baseRefName,headRefName,headRepositoryOwner".into(),
    ];
    if let Some(base) = &options.base {
        args.push("--base".into());
        args.push(base.clone());
    }
    if let Some(head) = &options.head {
        args.push("--head".into());
        args.push(
            head.split_once(':')
                .map(|(_, branch)| branch)
                .unwrap_or(head)
                .to_string(),
        );
    }

    let raw = gh.run(&args)?;
    // gh --head accepts a branch, not owner:branch. Filter the observed owner
    // as well so a same-named branch in the base repository cannot match.
    let raw = if let Some((owner, _)) = options
        .head
        .as_deref()
        .and_then(|head| head.split_once(':'))
    {
        let mut values: Vec<serde_json::Value> = serde_json::from_str(&raw)
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        values.retain(|value| {
            value["headRepositoryOwner"]["login"]
                .as_str()
                .is_some_and(|login| login.eq_ignore_ascii_case(owner))
        });
        serde_json::to_string(&values)
            .map_err(|error| Error::git_command_failed(error.to_string()))?
    } else {
        raw
    };
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
    // Resolve a host-aware client so readiness (and the GH_HOST passed to the
    // raw `gh` below) target the repo's actual host, not always github.com.
    let client = match repo {
        Some(repo) => GhClient::from_repo_arg(repo)?,
        None => GhClient::for_host(default_gh_host()),
    };
    client.ensure_ready()?;

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

    let mut command = Command::new("gh");
    command.args(&args).current_dir(repo_path);
    if client.host() != "github.com" {
        command.env("GH_HOST", client.host());
    }
    let output = command
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
    pr_view_with_token(component_id, number, path, None)
}

pub fn pr_view_with_token(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
    token: Option<&str>,
) -> Result<GithubPrView> {
    let (id, repo, gh) = resolve_component_github(component_id, path.as_deref())?;
    let gh = match token {
        Some(token) => gh.with_token(token),
        None => gh,
    };
    gh.ensure_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let raw = gh.run(&pr_view_args(&number.to_string(), Some(&repo_flag)))?;
    let record = parse_pr_view(&raw)?;
    let author = record.author.and_then(|author| author.login);
    let base = record.base_ref_name;
    let head = record.head_ref_name;
    let head_repository = record
        .head_repository
        .and_then(|head| head.name_with_owner.or(head.name));
    let state = normalized(record.state).unwrap_or_default();
    let url = normalized(record.url).unwrap_or_default();
    let title = normalized(record.title);
    let draft = record.is_draft;
    let head_sha = normalized(record.head_ref_oid);
    let merged_at = normalized(record.merged_at);
    let review_decision = normalized(record.review_decision);
    let merge_state = normalized(record.merge_state_status);
    let status_check_rollup = record.status_check_rollup;
    let (ci_state, ci_summary, ci_next_action) = classify_pr_ci(
        &state,
        merged_at.as_deref(),
        merge_state.as_deref(),
        &status_check_rollup,
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

/// Trim and drop empty strings, as the `Value`-pointer parser this replaced
/// did for every optional field, so the view projection is unchanged by the
/// shared-record refactor.
fn normalized(value: impl Into<Option<String>>) -> Option<String> {
    value
        .into()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

/// List changed files for one PR.
pub fn pr_files(
    component_id: Option<&str>,
    number: u64,
    path: Option<String>,
) -> Result<Vec<String>> {
    let (_id, repo, gh) = resolve_component_github(component_id, path.as_deref())?;
    gh.ensure_ready()?;
    let args: Vec<String> = vec![
        "api".into(),
        "--paginate".into(),
        format!("repos/{}/{}/pulls/{}/files", repo.owner, repo.repo, number),
        "--jq".into(),
        ".[].filename".into(),
    ];
    let raw = gh.run(&args)?;
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
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    gh.ensure_ready()?;
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let branch_to_delete = if options.delete_branch {
        let view = pr_view(Some(&id), options.number, options.path.clone())?;
        (view.head_repository.as_deref() == Some(repo_flag.as_str()) && !view.head.is_empty())
            .then_some(view.head)
    } else {
        None
    };

    gh.run(&pr_merge_api_args(&repo_flag, options.number, &method))?;

    let mut warnings = Vec::new();
    if let Some(branch) = branch_to_delete {
        if let Err(error) = gh.run(&delete_branch_ref_api_args(&repo_flag, &branch)) {
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
        #[serde(default, rename = "isDraft")]
        is_draft: bool,
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
            is_draft: p.is_draft,
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
            {"number":10,"title":"feat: x","url":"u10","state":"OPEN","isDraft":true},
            {"number":11,"title":"chore: y","url":"u11","state":"OPEN"}
        ]"#;
        let items = parse_pr_list_json(raw).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].number, 10);
        assert!(items[0].is_draft);
        assert!(!items[1].is_draft);
        assert_eq!(items[1].state, "OPEN");
    }

    #[test]
    fn pr_ready_uses_the_explicit_github_transition() {
        assert_eq!(
            pr_ready_args("Extra-Chill/homeboy", 42),
            ["pr", "ready", "42", "-R", "Extra-Chill/homeboy"]
        );
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

    #[cfg(unix)]
    #[test]
    fn fork_pr_lookup_filters_same_branch_by_observed_owner() {
        crate::test_support::with_isolated_home(|home| {
            let fixture = super::super::fake_gh::fake_gh_component(
                home.path(),
                "fork-find-fixture",
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOMEBOY_FAKE_GH_LOG"
if [ "$1 $2" = "pr list" ]; then
  printf '%s\n' '[{"number":1,"title":"base","url":"base","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feature","headRepositoryOwner":{"login":"example"}},{"number":2,"title":"fork","url":"fork","state":"OPEN","isDraft":false,"baseRefName":"main","headRefName":"feature","headRepositoryOwner":{"login":"Chubes4"}}]'
fi
exit 0
"#,
            );
            for state in [PrState::Open, PrState::Merged] {
                let result = pr_find(
                    Some("fork-find-fixture"),
                    PrFindOptions {
                        base: Some("main".into()),
                        head: Some("chubes4:feature".into()),
                        state,
                        limit: 10,
                        path: None,
                    },
                )
                .unwrap();
                assert_eq!(result.items.len(), 1);
                assert_eq!(result.items[0].number, 2);
            }
            let log = std::fs::read_to_string(fixture.log).unwrap();
            assert!(log.contains("--head feature"));
            assert!(!log.contains("--head chubes4:feature"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn pr_edit_patches_title_and_body_through_rest() {
        crate::test_support::with_isolated_home(|home| {
            let fixture = super::super::fake_gh::fake_gh_component(
                home.path(),
                "pr-edit-fixture",
                r#"#!/bin/sh
printf '%s\n' "$*" >> "$HOMEBOY_FAKE_GH_LOG"
[ "$1" = "api" ] && printf '%s\n' '{"number":7,"html_url":"https://github.com/example/pr-edit-fixture/pull/7"}'
exit 0
"#,
            );

            let output = pr_edit(
                Some("pr-edit-fixture"),
                PrEditOptions {
                    number: 7,
                    title: Some("feat:rest-edit".to_string()),
                    body: Some("Updated body\nwith `markdown`".to_string()),
                    path: None,
                },
            )
            .expect("pr edit through fake gh");

            // The URL comes from the REST response's html_url, not gh stdout.
            assert_eq!(
                output.url.as_deref(),
                Some("https://github.com/example/pr-edit-fixture/pull/7")
            );
            assert_eq!(output.number, Some(7));
            assert_eq!(output.title.as_deref(), Some("feat:rest-edit"));

            let api_calls = fixture.api_calls();
            assert_eq!(api_calls.len(), 1, "api calls: {api_calls:?}");
            let args: Vec<&str> = api_calls[0].split(' ').collect();
            assert_eq!(
                &args[..4],
                [
                    "api",
                    "-X",
                    "PATCH",
                    "repos/example/pr-edit-fixture/pulls/7"
                ]
            );
            assert_eq!(&args[4..6], ["-f", "title=feat:rest-edit"]);
            assert_eq!(args[6], "-F");
            assert!(
                args[7].starts_with("body=@"),
                "body must come from a file, not argv: {}",
                args[7]
            );
            // Never the deprecated `gh pr edit` path, which queries projectCards.
            assert!(!api_calls[0].contains("pr edit"));
        });
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
