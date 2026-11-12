//! GitHub pull-request comment helpers.

use crate::error::{Error, Result};
use crate::git::release_download::GitHubRepo;

use super::gh_client::GhClient;
use super::github::{push_api_markdown_body_file_arg, resolve_component_github, GithubPrOutput};
use super::github_comment_sections::{
    comment_matches_key, extract_footer, extract_header, merge_section, parse_comment_sections,
    render_comment,
};

/// Parameters for posting a (potentially sticky) PR comment.
///
/// Three shapes are supported, selected by `mode`:
/// - [`PrCommentMode::Fresh`] — plain append, no marker, no find-or-update.
/// - [`PrCommentMode::StickyWholeBody`] — single-section sticky (PR #1334
///   semantics): prepend `<!-- homeboy:key=<key> -->` marker and update the
///   one matching comment in place.
/// - [`PrCommentMode::Sectioned`] — multi-section aggregation: a single shared
///   comment carries `<!-- homeboy:comment-key=<outer> -->` and N section
///   blocks delimited by `<!-- homeboy:section-key=<inner>:start|end -->`.
///   Each invocation replaces its own inner section and leaves the others
///   untouched. Handles race consolidation when parallel jobs raced to create
///   the shared comment.
#[derive(Debug, Clone)]
pub struct PrCommentOptions {
    pub number: u64,
    pub body: String,
    pub mode: PrCommentMode,
    /// Only publish if the PR is open and its current head matches this SHA.
    /// Omit to preserve unguarded comment publication.
    pub expected_head_sha: Option<String>,
    /// Optional workspace path. See `IssueCreateOptions::path`.
    pub path: Option<String>,
}

impl Default for PrCommentOptions {
    fn default() -> Self {
        Self {
            number: 0,
            body: String::new(),
            mode: PrCommentMode::Fresh,
            expected_head_sha: None,
            path: None,
        }
    }
}

/// Which comment-posting flow to run. Mutually exclusive shapes.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PrCommentMode {
    /// Plain append. No marker. No find-or-update.
    #[default]
    Fresh,
    /// Single-section sticky comment (PR #1334). The `body` is treated as the
    /// whole comment body; the marker `<!-- homeboy:key=<key> -->` is prepended.
    StickyWholeBody { key: String },
    /// Multi-section aggregated sticky comment. `body` is ONE section's body;
    /// it is merged under `section_key` into the comment carrying `comment_key`.
    Sectioned {
        /// Outer marker (one shared comment per PR per outer key).
        comment_key: String,
        /// Inner marker (one section per inner key within the shared comment).
        section_key: String,
        /// Optional header line written just after the outer marker on fresh
        /// comments (e.g. `## Homeboy Results — \`<component>\``). Preserved
        /// from existing comments on merge.
        header: Option<String>,
        /// Optional footer block written after the last section on fresh
        /// comments (e.g. a `<details><summary>Tooling versions</summary>`
        /// block). Preserved from existing comments on merge when the caller
        /// does not pass one explicitly; overwritten when the caller does.
        footer: Option<String>,
        /// Optional explicit section ordering. Sections listed here come first
        /// in the given order; any other sections are appended alphabetically.
        /// `None` = pure alphabetical.
        section_order: Option<Vec<String>>,
    },
}

/// Post a comment on a PR.
///
/// Dispatches on [`PrCommentOptions::mode`]:
/// - [`PrCommentMode::Fresh`] — plain append, no marker.
/// - [`PrCommentMode::StickyWholeBody`] — find-or-update the one comment
///   tagged `<!-- homeboy:key=<key> -->` (single-section sticky, PR #1334).
/// - [`PrCommentMode::Sectioned`] — multi-section aggregation: merge this
///   invocation's section under `section_key` into the shared comment tagged
///   `<!-- homeboy:comment-key=<comment_key> -->`.
pub fn pr_comment(component_id: Option<&str>, options: PrCommentOptions) -> Result<GithubPrOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    pr_comment_with_client(id, repo, &gh, options)
}

/// Post a comment on an issue or pull request in `repo`. They share one comments
/// API, so every [`PrCommentMode`] behaves identically on either.
pub fn repo_comment(repo: &GitHubRepo, options: PrCommentOptions) -> Result<GithubPrOutput> {
    let id = format!("{}/{}", repo.owner, repo.repo);
    pr_comment_with_client(id, repo.clone(), &GhClient::for_repo(repo), options)
}

/// Create one comment through the shared issue/PR comments API; returns its URL.
fn create_comment(gh: &GhClient, repo: &GitHubRepo, number: u64, body: &str) -> Result<String> {
    let mut args: Vec<String> = vec![
        "api".into(),
        format!(
            "repos/{}/{}/issues/{}/comments",
            repo.owner, repo.repo, number
        ),
        "--method".into(),
        "POST".into(),
        "--jq".into(),
        ".html_url".into(),
    ];
    let mut body_files = Vec::new();
    push_api_markdown_body_file_arg(&mut args, &mut body_files, "body", body)?;
    Ok(gh.run(&args)?.trim().to_string())
}

fn pr_comment_with_client(
    id: String,
    repo: GitHubRepo,
    gh: &GhClient,
    options: PrCommentOptions,
) -> Result<GithubPrOutput> {
    gh.ensure_ready()?;

    // Opt-in: legacy callers neither fetch the PR nor change closed-PR behavior.
    // Read once before any comment lookup or mutation, for every posting mode.
    if let Some(expected_head_sha) = options.expected_head_sha.as_deref() {
        #[derive(serde::Deserialize)]
        struct PrHead {
            sha: String,
        }
        #[derive(serde::Deserialize)]
        struct PrSnapshot {
            state: String,
            head: PrHead,
        }
        let pr: PrSnapshot = gh.api_json(&format!(
            "repos/{}/{}/pulls/{}",
            repo.owner, repo.repo, options.number
        ))?;
        let skip_reason = if pr.state != "open" {
            Some("pr_not_open")
        } else if pr.head.sha != expected_head_sha {
            Some("stale_head")
        } else {
            None
        };
        if let Some(reason) = skip_reason {
            return Ok(GithubPrOutput {
                component_id: id,
                owner: repo.owner,
                repo: repo.repo,
                action: "skipped".into(),
                success: true,
                number: Some(options.number),
                state: Some(pr.state),
                skip_reason: Some(reason.into()),
                expected_head_sha: Some(expected_head_sha.into()),
                current_head_sha: Some(pr.head.sha),
                ..Default::default()
            });
        }
    }

    match options.mode.clone() {
        PrCommentMode::Fresh => pr_comment_fresh(id, repo, gh, options),
        PrCommentMode::StickyWholeBody { key } => {
            pr_comment_sticky_whole(id, repo, gh, options.number, options.body, key)
        }
        PrCommentMode::Sectioned {
            comment_key,
            section_key,
            header,
            footer,
            section_order,
        } => pr_comment_sectioned(
            id,
            repo,
            gh,
            options.number,
            options.body,
            comment_key,
            section_key,
            header,
            footer,
            section_order,
        ),
    }
}

/// Plain append flow. Shared by `Fresh` mode and the "no existing comment"
/// branch of the sticky flow.
fn pr_comment_fresh(
    id: String,
    repo: GitHubRepo,
    gh: &GhClient,
    options: PrCommentOptions,
) -> Result<GithubPrOutput> {
    let body = super::neutralize_mentions(&options.body);
    let url = create_comment(gh, &repo, options.number, &body)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.comment.create".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(url),
        ..Default::default()
    })
}

/// Sticky single-section flow (PR #1334 semantics).
fn pr_comment_sticky_whole(
    id: String,
    repo: GitHubRepo,
    gh: &GhClient,
    pr_number: u64,
    body: String,
    key: String,
) -> Result<GithubPrOutput> {
    let full_body = super::neutralize_mentions(&format!("{}\n{}", marker_for_key(&key), body));

    if let Some(existing_id) = find_sticky_comment_id(gh, &repo, pr_number, &key)? {
        let args: Vec<String> = vec![
            "api".into(),
            format!(
                "repos/{}/{}/issues/comments/{}",
                repo.owner, repo.repo, existing_id
            ),
            "--method".into(),
            "PATCH".into(),
            "-f".into(),
            format!("body={}", full_body),
        ];
        gh.run(&args)?;
        return Ok(GithubPrOutput {
            component_id: id,
            owner: repo.owner,
            repo: repo.repo,
            action: "pr.comment.update".to_string(),
            success: true,
            number: Some(pr_number),
            comment_id: Some(existing_id),
            ..Default::default()
        });
    }

    let url = create_comment(gh, &repo, pr_number, &full_body)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.comment.create".to_string(),
        success: true,
        number: Some(pr_number),
        url: Some(url),
        ..Default::default()
    })
}

/// Sectioned-comment flow.
///
/// Flow:
/// 1. List the PR's issue-comments, filter to those carrying the comment-key
///    marker.
/// 2. If none: create one with a single section block.
/// 3. If one: parse existing sections, merge this invocation's section, render.
///    Byte-compare to the existing body — if equal, emit `pr.comment.section.noop`
///    and skip the PATCH. Otherwise PATCH.
/// 4. If many (race): pick lowest id as canonical, merge sections from ALL
///    matching comments (current invocation wins last for duplicate keys),
///    PATCH canonical, DELETE the rest. Failed DELETEs become warnings, not
///    hard errors — the next invocation will consolidate.
#[allow(clippy::too_many_arguments)]
fn pr_comment_sectioned(
    id: String,
    repo: GitHubRepo,
    gh: &GhClient,
    pr_number: u64,
    section_body: String,
    comment_key: String,
    section_key: String,
    header: Option<String>,
    footer: Option<String>,
    section_order: Option<Vec<String>>,
) -> Result<GithubPrOutput> {
    let matches = list_matching_comments(gh, &repo, pr_number, &comment_key)?;

    if matches.is_empty() {
        let sections: Vec<(String, String)> = vec![(section_key.clone(), section_body.clone())];
        let rendered = render_comment(
            &comment_key,
            header.as_deref(),
            &sections,
            section_order.as_deref(),
            footer.as_deref(),
        );
        let url = create_comment(gh, &repo, pr_number, &rendered)?;
        return Ok(GithubPrOutput {
            component_id: id,
            owner: repo.owner,
            repo: repo.repo,
            action: "pr.comment.section.create".to_string(),
            success: true,
            number: Some(pr_number),
            url: Some(url),
            ..Default::default()
        });
    }

    let mut matches = matches;
    matches.sort_by_key(|m| m.id);
    let canonical_id = matches[0].id;
    let canonical_body = matches[0].body.clone();

    let mut merged: Vec<(String, String)> = Vec::new();
    let mut discovered_header: Option<String> = header.clone();
    let mut discovered_footer: Option<String> = footer.clone();
    for comment in &matches {
        let parsed = parse_comment_sections(&comment.body);
        for (k, v) in parsed {
            merged = merge_section(merged, &k, v);
        }
        if discovered_header.is_none() {
            discovered_header = extract_header(&comment.body);
        }
        if discovered_footer.is_none() {
            discovered_footer = extract_footer(&comment.body);
        }
    }
    merged = merge_section(merged, &section_key, section_body);

    let rendered = render_comment(
        &comment_key,
        discovered_header.as_deref(),
        &merged,
        section_order.as_deref(),
        discovered_footer.as_deref(),
    );

    let patch_needed = rendered.trim_end() != canonical_body.trim_end();
    let mut warnings: Vec<String> = Vec::new();

    if patch_needed {
        let args: Vec<String> = vec![
            "api".into(),
            format!(
                "repos/{}/{}/issues/comments/{}",
                repo.owner, repo.repo, canonical_id
            ),
            "--method".into(),
            "PATCH".into(),
            "-f".into(),
            format!("body={}", rendered),
        ];
        gh.run(&args)?;
    }

    for comment in matches.iter().skip(1) {
        let args: Vec<String> = vec![
            "api".into(),
            format!(
                "repos/{}/{}/issues/comments/{}",
                repo.owner, repo.repo, comment.id
            ),
            "--method".into(),
            "DELETE".into(),
        ];
        if gh.run(&args).is_err() {
            warnings.push(format!(
                "failed to delete duplicate comment id={} — next invocation will retry",
                comment.id
            ));
        }
    }

    let action = if patch_needed {
        "pr.comment.section.update"
    } else {
        "pr.comment.section.noop"
    };

    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: action.to_string(),
        success: true,
        number: Some(pr_number),
        comment_id: Some(canonical_id),
        warnings,
        ..Default::default()
    })
}

fn marker_for_key(key: &str) -> String {
    format!("<!-- homeboy:key={} -->", key)
}

/// Search a PR's issue-comments for one carrying our sticky marker.
fn find_sticky_comment_id(
    gh: &GhClient,
    repo: &GitHubRepo,
    pr_number: u64,
    key: &str,
) -> Result<Option<u64>> {
    let marker = marker_for_key(key);
    let args: Vec<String> = vec![
        "api".into(),
        format!(
            "repos/{}/{}/issues/{}/comments?per_page=100",
            repo.owner, repo.repo, pr_number
        ),
        "--paginate".into(),
        "--jq".into(),
        format!(".[] | select(.body | contains(\"{}\")) | .id", marker),
    ];
    let raw = gh.run(&args)?;
    Ok(raw.lines().next().and_then(|l| l.trim().parse().ok()))
}

/// Minimal shape for a fetched PR comment (id + body).
struct FetchedComment {
    id: u64,
    body: String,
}

/// List all PR issue-comments that carry the given comment-key marker. Returns
/// `(id, body)` pairs so the merge step can parse each body.
fn list_matching_comments(
    gh: &GhClient,
    repo: &GitHubRepo,
    pr_number: u64,
    comment_key: &str,
) -> Result<Vec<FetchedComment>> {
    let args: Vec<String> = vec![
        "api".into(),
        format!(
            "repos/{}/{}/issues/{}/comments?per_page=100",
            repo.owner, repo.repo, pr_number
        ),
        "--paginate".into(),
    ];
    let raw = gh.run(&args)?;
    parse_comments_list_json(&raw, comment_key)
}

/// Parse `gh api --paginate issues/:n/comments` output and filter to those
/// carrying the outer marker. With `--paginate`, `gh` concatenates JSON arrays
/// (no separator between pages), so we re-parse as a stream.
fn parse_comments_list_json(raw: &str, comment_key: &str) -> Result<Vec<FetchedComment>> {
    #[derive(serde::Deserialize)]
    struct RawComment {
        id: u64,
        body: Option<String>,
    }

    let mut out: Vec<FetchedComment> = Vec::new();
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Ok(out);
    }

    let de = serde_json::Deserializer::from_str(trimmed);
    for value in de.into_iter::<Vec<RawComment>>() {
        let page = value
            .map_err(|e| Error::internal_json(e.to_string(), Some("gh api comments".into())))?;
        for c in page {
            let body = c.body.unwrap_or_default();
            if comment_matches_key(&body, comment_key) {
                out.push(FetchedComment { id: c.id, body });
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn publication_guard_checks_once_before_all_comment_modes() {
        let modes = [
            PrCommentMode::Fresh,
            PrCommentMode::StickyWholeBody { key: "ci".into() },
            PrCommentMode::Sectioned {
                comment_key: "ci".into(),
                section_key: "test".into(),
                header: None,
                footer: None,
                section_order: None,
            },
        ];
        for mode in modes {
            for (state, head, expected, reason) in [
                ("open", "new", Some("new"), None),
                ("open", "new", Some("old"), Some("stale_head")),
                ("closed", "new", Some("new"), Some("pr_not_open")),
                // Merged PRs also have state=closed in the REST API.
                ("closed", "new", Some("old"), Some("pr_not_open")),
                ("closed", "new", None, None),
                ("open", "new", None, None),
            ] {
                let fixture = tempfile::tempdir().unwrap();
                let log = fixture.path().join("calls");
                let quoted_log =
                    homeboy_engine_primitives::shell::quote_path(&log.to_string_lossy());
                homeboy_test_support::write_executable(
                    &fixture.path().join("gh"),
                    format!(
                        "#!/bin/sh\necho \"$*\" >> {quoted_log}\ncase \"$1 $2\" in\n  '--version '| 'auth status') exit 0 ;;\n  'api repos/o/r/pulls/42') echo '{{\"state\":\"{state}\",\"head\":{{\"sha\":\"{head}\"}}}}' ;;\n  'api repos/o/r/issues/42/comments') case \"$*\" in *POST*) echo 'https://github.com/o/r/pull/42#issuecomment-1' ;; *) echo ;; esac ;;\n  'api repos/o/r/issues/42/'*) case \"$*\" in *--jq*) echo ;; *) echo '[]' ;; esac ;;\n  *) exit 99 ;;\nesac\n"
                    ),
                    0o755,
                ).unwrap();
                let repo = GitHubRepo {
                    host: "github.com".into(),
                    owner: "o".into(),
                    repo: "r".into(),
                };
                let gh = GhClient::for_repo(&repo).with_test_path(fixture.path());
                let output = pr_comment_with_client(
                    "component".into(),
                    repo,
                    &gh,
                    PrCommentOptions {
                        number: 42,
                        body: "results".into(),
                        mode: mode.clone(),
                        expected_head_sha: expected.map(str::to_string),
                        ..Default::default()
                    },
                )
                .unwrap();
                let calls = std::fs::read_to_string(log).unwrap();
                assert_eq!(
                    calls.matches("api repos/o/r/pulls/42").count(),
                    usize::from(expected.is_some())
                );
                assert!(output.success);
                let json = serde_json::to_value(&output).unwrap();
                if let Some(reason) = reason {
                    assert_eq!(json["action"], "skipped");
                    assert_eq!(json["skip_reason"], reason);
                    assert_eq!(json["expected_head_sha"], expected.unwrap());
                    assert_eq!(json["current_head_sha"], head);
                    // Only readiness probes and the single PR read: no comment
                    // lookup, create, PATCH, or duplicate DELETE is reachable.
                    assert_eq!(calls.lines().count(), 3, "{calls}");
                } else {
                    assert!(output.action.ends_with("create"));
                    assert!(!json.as_object().unwrap().contains_key("skip_reason"));
                    assert_eq!(calls.matches("issues/42/comments --method POST").count(), 1);
                    assert_eq!(
                        output.url.as_deref(),
                        Some("https://github.com/o/r/pull/42#issuecomment-1")
                    );
                }
            }
        }
    }

    #[test]
    fn marker_format_is_stable() {
        assert_eq!(
            marker_for_key("ci-status"),
            "<!-- homeboy:key=ci-status -->"
        );
    }

    #[test]
    fn parse_comments_list_filters_by_key_and_handles_pagination() {
        let raw = r#"[
            {"id": 1, "body": "<!-- homeboy:comment-key=ci:x -->\nsection"},
            {"id": 2, "body": "unrelated"}
        ][
            {"id": 3, "body": "<!-- homeboy:comment-key=ci:y -->\nother key"},
            {"id": 4, "body": "<!-- homeboy:comment-key=other -->\nother"}
        ]"#;
        let got = parse_comments_list_json(raw, "ci:x").unwrap();
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].id, 1);
    }

    #[test]
    fn parse_comments_list_empty_input_is_ok() {
        let got = parse_comments_list_json("", "ci:x").unwrap();
        assert!(got.is_empty());
    }

    #[test]
    fn pr_comment_mode_default_is_fresh() {
        let opts = PrCommentOptions::default();
        assert_eq!(opts.mode, PrCommentMode::Fresh);
    }
}
