//! GitHub pull-request comment helpers.

use crate::core::deploy::release_download::GitHubRepo;
use crate::core::error::{Error, Result};

use super::github::{
    ensure_gh_ready, push_markdown_body_file_arg, resolve_component_github, run_gh, GithubPrOutput,
};
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
    /// Optional workspace path. See `IssueCreateOptions::path`.
    pub path: Option<String>,
}

impl Default for PrCommentOptions {
    fn default() -> Self {
        Self {
            number: 0,
            body: String::new(),
            mode: PrCommentMode::Fresh,
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
    let (id, repo) = resolve_component_github(component_id, options.path.as_deref())?;
    ensure_gh_ready()?;

    match options.mode.clone() {
        PrCommentMode::Fresh => pr_comment_fresh(id, repo, options),
        PrCommentMode::StickyWholeBody { key } => {
            pr_comment_sticky_whole(id, repo, options.number, options.body, key)
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
    options: PrCommentOptions,
) -> Result<GithubPrOutput> {
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "pr".into(),
        "comment".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
    ];
    let mut body_files = Vec::new();
    push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", &options.body)?;
    let output = run_gh(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.comment.create".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        ..Default::default()
    })
}

/// Sticky single-section flow (PR #1334 semantics).
fn pr_comment_sticky_whole(
    id: String,
    repo: GitHubRepo,
    pr_number: u64,
    body: String,
    key: String,
) -> Result<GithubPrOutput> {
    let full_body = format!("{}\n{}", marker_for_key(&key), body);

    if let Some(existing_id) = find_sticky_comment_id(&repo, pr_number, &key)? {
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
        run_gh(&args)?;
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

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "pr".into(),
        "comment".into(),
        pr_number.to_string(),
        "-R".into(),
        repo_flag,
    ];
    let mut body_files = Vec::new();
    push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", &full_body)?;
    let output = run_gh(&args)?;
    Ok(GithubPrOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.comment.create".to_string(),
        success: true,
        number: Some(pr_number),
        url: Some(output.trim().to_string()),
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
    pr_number: u64,
    section_body: String,
    comment_key: String,
    section_key: String,
    header: Option<String>,
    footer: Option<String>,
    section_order: Option<Vec<String>>,
) -> Result<GithubPrOutput> {
    let matches = list_matching_comments(&repo, pr_number, &comment_key)?;

    if matches.is_empty() {
        let sections: Vec<(String, String)> = vec![(section_key.clone(), section_body.clone())];
        let rendered = render_comment(
            &comment_key,
            header.as_deref(),
            &sections,
            section_order.as_deref(),
            footer.as_deref(),
        );
        let repo_flag = format!("{}/{}", repo.owner, repo.repo);
        let mut args: Vec<String> = vec![
            "pr".into(),
            "comment".into(),
            pr_number.to_string(),
            "-R".into(),
            repo_flag,
        ];
        let mut body_files = Vec::new();
        push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", &rendered)?;
        let output = run_gh(&args)?;
        return Ok(GithubPrOutput {
            component_id: id,
            owner: repo.owner,
            repo: repo.repo,
            action: "pr.comment.section.create".to_string(),
            success: true,
            number: Some(pr_number),
            url: Some(output.trim().to_string()),
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
        run_gh(&args)?;
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
        if run_gh(&args).is_err() {
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
fn find_sticky_comment_id(repo: &GitHubRepo, pr_number: u64, key: &str) -> Result<Option<u64>> {
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
    let raw = run_gh(&args)?;
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
    let raw = run_gh(&args)?;
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
