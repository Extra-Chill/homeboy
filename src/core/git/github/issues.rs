//! Issue CRUD primitives via the `gh` CLI: create, comment, close, edit, find.

use crate::core::error::{Error, Result};

use super::super::github_types::{
    GithubFindItem, GithubFindOutput, GithubIssueOutput, IssueCloseOptions, IssueCommentOptions,
    IssueCreateOptions, IssueEditOptions, IssueFindOptions,
};
use super::client::{parse_issue_number_from_url, resolve_component_github};
use super::push_markdown_body_file_arg;

/// Create a new issue on the component's GitHub repository.
pub fn issue_create(
    component_id: Option<&str>,
    options: IssueCreateOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    gh.ensure_ready()?;

    if options.title.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "title",
            "Issue title is required",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "issue".into(),
        "create".into(),
        "-R".into(),
        repo_flag.clone(),
        "--title".into(),
        options.title.clone(),
    ];
    let mut body_files = Vec::new();
    push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", &options.body)?;
    for label in &options.labels {
        args.push("--label".into());
        args.push(label.clone());
    }

    let output = gh.run(&args)?;
    let url = output.trim().to_string();
    let number = parse_issue_number_from_url(&url);

    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.create".to_string(),
        success: true,
        number,
        url: Some(url),
        title: Some(options.title),
        state: Some("open".to_string()),
    })
}

/// Post a comment on an existing issue.
pub fn issue_comment(
    component_id: Option<&str>,
    options: IssueCommentOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    gh.ensure_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "issue".into(),
        "comment".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
    ];
    let mut body_files = Vec::new();
    push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", &options.body)?;

    let output = gh.run(&args)?;
    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.comment".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        title: None,
        state: None,
    })
}

/// Close an existing issue with a typed reason.
///
/// `gh issue close --reason` accepts `completed | not planned | duplicate`.
/// We expose the two semantically-meaningful values via [`IssueCloseReason`];
/// `duplicate` is a special-case of "not planned" and not modeled here. Use
/// [`IssueCloseOptions::comment`] to leave a closing comment in the same
/// invocation (mirrors `gh issue close --comment`).
pub fn issue_close(
    component_id: Option<&str>,
    options: IssueCloseOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    gh.ensure_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "issue".into(),
        "close".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
        "--reason".into(),
        options.reason.as_gh_flag().to_string(),
    ];
    if let Some(comment) = &options.comment {
        args.push("--comment".into());
        args.push(comment.clone());
    }

    let _ = gh.run(&args)?;
    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.close".to_string(),
        success: true,
        number: Some(options.number),
        url: None,
        title: None,
        state: Some("closed".to_string()),
    })
}

/// Edit an existing issue's title, body, or labels.
///
/// At least one of `title`, `body`, `add_labels`, or `remove_labels` must be
/// provided. Mirrors `gh issue edit <n> [--title ...] [--body ...]
/// [--add-label ...] [--remove-label ...]`. Used by `homeboy issues reconcile`
/// to refresh the body of existing issues (open OR closed) so the latest
/// finding count and run link stay visible without duplicating the issue.
pub fn issue_edit(
    component_id: Option<&str>,
    options: IssueEditOptions,
) -> Result<GithubIssueOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    gh.ensure_ready()?;

    if options.title.is_none()
        && options.body.is_none()
        && options.add_labels.is_empty()
        && options.remove_labels.is_empty()
    {
        return Err(Error::validation_invalid_argument(
            "title/body/labels",
            "At least one of --title, --body, --add-label, or --remove-label must be provided",
            None,
            None,
        ));
    }

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let mut args: Vec<String> = vec![
        "issue".into(),
        "edit".into(),
        options.number.to_string(),
        "-R".into(),
        repo_flag,
    ];
    let mut body_files = Vec::new();
    if let Some(title) = &options.title {
        args.push("--title".into());
        args.push(title.clone());
    }
    if let Some(body) = &options.body {
        push_markdown_body_file_arg(&mut args, &mut body_files, "--body-file", body)?;
    }
    for label in &options.add_labels {
        args.push("--add-label".into());
        args.push(label.clone());
    }
    for label in &options.remove_labels {
        args.push("--remove-label".into());
        args.push(label.clone());
    }

    let output = gh.run(&args)?;
    Ok(GithubIssueOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.edit".to_string(),
        success: true,
        number: Some(options.number),
        url: Some(output.trim().to_string()),
        title: options.title,
        state: None,
    })
}

/// Find issues matching the given filter. Useful for dedup before creating.
///
/// Uses `gh issue list --json number,title,body,url,state,stateReason,closedAt,labels`
/// and filters locally (title and label conjunctions are simpler to enforce
/// client-side than via the gh search syntax).
pub fn issue_find(
    component_id: Option<&str>,
    options: IssueFindOptions,
) -> Result<GithubFindOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    gh.ensure_ready()?;

    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let limit = if options.limit == 0 {
        30
    } else {
        options.limit
    };
    let mut args: Vec<String> = vec![
        "issue".into(),
        "list".into(),
        "-R".into(),
        repo_flag,
        "--state".into(),
        options.state.as_gh_flag().to_string(),
        "--limit".into(),
        limit.to_string(),
        "--json".into(),
        "number,title,body,url,state,stateReason,closedAt,labels".into(),
    ];
    // Pass labels through gh to narrow the server-side result set; we still
    // enforce the exact label-set conjunction locally in case gh changes the
    // semantics of --label (currently: all-of).
    for label in &options.labels {
        args.push("--label".into());
        args.push(label.clone());
    }

    let raw = gh.run(&args)?;
    let items = parse_issue_list_json(&raw, &options)?;

    Ok(GithubFindOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "issue.find".to_string(),
        success: true,
        items,
    })
}

pub(super) fn parse_issue_list_json(
    raw: &str,
    options: &IssueFindOptions,
) -> Result<Vec<GithubFindItem>> {
    #[derive(serde::Deserialize)]
    struct RawIssue {
        number: u64,
        title: String,
        #[serde(default)]
        body: Option<String>,
        url: String,
        state: String,
        #[serde(default, rename = "stateReason")]
        state_reason: Option<String>,
        #[serde(default, rename = "closedAt")]
        closed_at: Option<String>,
        #[serde(default)]
        labels: Vec<RawLabel>,
    }
    #[derive(serde::Deserialize)]
    struct RawLabel {
        name: String,
    }

    let parsed: Vec<RawIssue> = serde_json::from_str(raw.trim())
        .map_err(|e| Error::internal_json(e.to_string(), Some("gh issue list".into())))?;

    let out = parsed
        .into_iter()
        .filter(|i| match &options.title {
            Some(t) => &i.title == t,
            None => true,
        })
        .filter(|i| {
            options
                .labels
                .iter()
                .all(|needle| i.labels.iter().any(|l| &l.name == needle))
        })
        .map(|i| GithubFindItem {
            number: i.number,
            title: i.title,
            body: i.body.unwrap_or_default(),
            url: i.url,
            state: i.state,
            state_reason: i.state_reason.unwrap_or_default(),
            closed_at: i.closed_at.unwrap_or_default(),
            labels: i.labels.into_iter().map(|l| l.name).collect(),
        })
        .collect();
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::super::super::github_types::{IssueCloseReason, IssueState};
    use super::*;

    #[test]
    fn parse_issue_list_filters_by_title() {
        let raw = r#"[
            {"number":1,"title":"bug: one","url":"u1","state":"open","labels":[]},
            {"number":2,"title":"bug: two","url":"u2","state":"open","labels":[]}
        ]"#;
        let opts = IssueFindOptions {
            title: Some("bug: two".into()),
            ..Default::default()
        };
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, 2);
    }

    #[test]
    fn parse_issue_list_requires_all_labels() {
        let raw = r#"[
            {"number":1,"title":"a","url":"u1","state":"open","labels":[{"name":"ci-failure"}]},
            {"number":2,"title":"b","url":"u2","state":"open","labels":[{"name":"ci-failure"},{"name":"autofix"}]}
        ]"#;
        let opts = IssueFindOptions {
            labels: vec!["ci-failure".into(), "autofix".into()],
            ..Default::default()
        };
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].number, 2);
    }

    #[test]
    fn issue_state_gh_flag() {
        assert_eq!(IssueState::Open.as_gh_flag(), "open");
        assert_eq!(IssueState::Closed.as_gh_flag(), "closed");
        assert_eq!(IssueState::All.as_gh_flag(), "all");
    }

    #[test]
    fn issue_close_reason_gh_flag() {
        assert_eq!(IssueCloseReason::Completed.as_gh_flag(), "completed");
        assert_eq!(IssueCloseReason::NotPlanned.as_gh_flag(), "not planned");
    }

    #[test]
    fn parse_issue_list_extracts_state_reason_and_closed_at() {
        // gh issue list --json includes stateReason + closedAt fields when
        // requested. Closed-completed, closed-not_planned, and open issues
        // are represented in this fixture.
        let raw = r#"[
            {
                "number": 100,
                "title": "audit: thing in repo (3)",
                "url": "https://github.com/o/r/issues/100",
                "state": "OPEN",
                "stateReason": null,
                "closedAt": null,
                "labels": [{"name":"audit"}]
            },
            {
                "number": 101,
                "title": "audit: other in repo (5)",
                "url": "https://github.com/o/r/issues/101",
                "state": "CLOSED",
                "stateReason": "completed",
                "closedAt": "2026-04-25T12:00:00Z",
                "labels": [{"name":"audit"}]
            },
            {
                "number": 102,
                "title": "audit: muted in repo (12)",
                "url": "https://github.com/o/r/issues/102",
                "state": "CLOSED",
                "stateReason": "not_planned",
                "closedAt": "2026-04-26T03:00:00Z",
                "labels": [{"name":"audit"},{"name":"wontfix"}]
            }
        ]"#;
        let opts = IssueFindOptions {
            state: IssueState::All,
            ..Default::default()
        };
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 3);

        // Open issue: empty state_reason and closed_at, single label.
        assert_eq!(items[0].number, 100);
        assert_eq!(items[0].state, "OPEN");
        assert_eq!(items[0].state_reason, "");
        assert_eq!(items[0].closed_at, "");
        assert_eq!(items[0].labels, vec!["audit".to_string()]);

        // Closed completed: state_reason populated, closed_at populated.
        assert_eq!(items[1].number, 101);
        assert_eq!(items[1].state, "CLOSED");
        assert_eq!(items[1].state_reason, "completed");
        assert_eq!(items[1].closed_at, "2026-04-25T12:00:00Z");

        // Closed not_planned with suppression label.
        assert_eq!(items[2].number, 102);
        assert_eq!(items[2].state_reason, "not_planned");
        assert_eq!(
            items[2].labels,
            vec!["audit".to_string(), "wontfix".to_string()]
        );
    }

    #[test]
    fn parse_issue_list_handles_missing_optional_fields() {
        // Older gh versions or projects without state-reason support emit
        // payloads without those fields. Default-deserialize to empty.
        let raw = r#"[
            {"number":1,"title":"x","url":"u","state":"open","labels":[]}
        ]"#;
        let opts = IssueFindOptions::default();
        let items = parse_issue_list_json(raw, &opts).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].state_reason, "");
        assert_eq!(items[0].closed_at, "");
        assert!(items[0].labels.is_empty());
    }
}
