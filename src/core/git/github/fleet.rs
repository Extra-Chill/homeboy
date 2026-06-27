//! Fleet operations: report and optionally land a batch of PRs in one pass,
//! plus the status-check rollup summarizer and PR-reference parser they use.

use crate::core::error::Result;

use super::super::gh_client::{pr_merge_api_args, GhClient};
use super::super::github_types::{
    GithubPrCheckRollup, GithubPrFleetItem, GithubPrFleetOutput, GithubPrFleetSummary,
    GithubPrView, PrFleetOptions,
};
use super::client::resolve_component_github;
use super::pulls::{pr_view, validate_pr_merge_method};

/// Report and optionally land a fleet of PRs.
pub fn pr_fleet(
    component_id: Option<&str>,
    options: PrFleetOptions,
) -> Result<GithubPrFleetOutput> {
    let (id, repo, gh) = resolve_component_github(component_id, options.path.as_deref())?;
    gh.ensure_ready()?;
    let repo_flag = format!("{}/{}", repo.owner, repo.repo);
    let merge_method = validate_pr_merge_method(&options.merge_method)?;

    let mut items = Vec::new();
    for input in &options.refs {
        let parsed = match parse_pr_fleet_ref(input, &repo.owner, &repo.repo) {
            Ok(number) => number,
            Err(error) => {
                items.push(pr_fleet_error_item(input, None, error));
                continue;
            }
        };

        let mut view = match pr_view(Some(&id), parsed, options.path.clone()) {
            Ok(view) => view,
            Err(error) => {
                items.push(pr_fleet_error_item(input, Some(parsed), error.to_string()));
                continue;
            }
        };
        let mut updated = false;

        if options.update_branches && pr_fleet_should_update_branch(&view) {
            let args: Vec<String> = vec![
                "pr".into(),
                "update-branch".into(),
                parsed.to_string(),
                "-R".into(),
                repo_flag.clone(),
            ];
            match gh.run(&args) {
                Ok(_) => {
                    updated = true;
                    view = pr_view(Some(&id), parsed, options.path.clone())?;
                }
                Err(error) => {
                    let mut item = pr_fleet_item(input, &view, GithubPrCheckRollup::default());
                    item.required_action = "update_branch_failed".to_string();
                    item.error = Some(error.to_string());
                    items.push(item);
                    continue;
                }
            }
        }

        let rollup = pr_fleet_check_rollup(&gh, &repo_flag, parsed)?;
        let mut item = pr_fleet_item(input, &view, rollup);
        item.updated = updated;

        if options.apply && item.mergeable {
            let args = pr_merge_api_args(&repo_flag, parsed, &merge_method);
            match gh.run(&args) {
                Ok(_) => {
                    item.merged = true;
                    item.required_action = "merged".to_string();
                }
                Err(error) => {
                    item.required_action = "merge_failed".to_string();
                    item.error = Some(error.to_string());
                }
            }
        }

        items.push(item);
    }

    let summary = summarize_pr_fleet(&items);
    let success = summary.errors == 0;
    Ok(GithubPrFleetOutput {
        component_id: id,
        owner: repo.owner,
        repo: repo.repo,
        action: "pr.fleet".to_string(),
        success,
        apply: options.apply,
        update_branches: options.update_branches,
        summary,
        items,
    })
}

fn parse_pr_fleet_ref(input: &str, owner: &str, repo: &str) -> std::result::Result<u64, String> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err("empty PR reference".to_string());
    }
    if let Ok(number) = trimmed.parse::<u64>() {
        return Ok(number);
    }

    let path = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .and_then(|rest| rest.split_once('/').map(|(_, path)| path))
        .unwrap_or(trimmed);
    let parts: Vec<&str> = path.trim_end_matches('/').split('/').collect();
    match parts.as_slice() {
        [url_owner, url_repo, "pull", number] if *url_owner == owner && *url_repo == repo => {
            number
                .parse::<u64>()
                .map_err(|_| format!("invalid PR number in reference `{input}`"))
        }
        [url_owner, url_repo, "pull", _] => Err(format!(
            "PR reference `{input}` targets {url_owner}/{url_repo}, expected {owner}/{repo}"
        )),
        _ => Err(format!(
            "invalid PR reference `{input}`; use a PR number or https://github.com/{owner}/{repo}/pull/<number>"
        )),
    }
}

fn pr_fleet_should_update_branch(view: &GithubPrView) -> bool {
    view.state == "OPEN" && view.merge_state.as_deref() == Some("BEHIND")
}

fn pr_fleet_check_rollup(
    gh: &GhClient,
    repo_flag: &str,
    number: u64,
) -> Result<GithubPrCheckRollup> {
    let args: Vec<String> = vec![
        "pr".into(),
        "view".into(),
        number.to_string(),
        "-R".into(),
        repo_flag.to_string(),
        "--json".into(),
        "statusCheckRollup".into(),
    ];
    let raw = gh.run(&args)?;
    let parsed: serde_json::Value = serde_json::from_str(&raw).map_err(|e| {
        crate::core::error::Error::internal_json(
            format!("Failed to parse gh pr view statusCheckRollup JSON: {e}"),
            Some(raw.clone()),
        )
    })?;
    let checks = parsed
        .get("statusCheckRollup")
        .and_then(|value| value.as_array())
        .map(|value| value.as_slice())
        .unwrap_or(&[]);
    Ok(summarize_check_rollup(checks))
}

fn summarize_check_rollup(checks: &[serde_json::Value]) -> GithubPrCheckRollup {
    let mut rollup = GithubPrCheckRollup {
        total: checks.len(),
        ..Default::default()
    };
    for check in checks {
        let status = check.get("status").and_then(serde_json::Value::as_str);
        let conclusion = check
            .get("conclusion")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty());
        match (status, conclusion) {
            (
                _,
                Some("FAILURE" | "CANCELLED" | "TIMED_OUT" | "ACTION_REQUIRED" | "STARTUP_FAILURE"),
            ) => rollup.failed += 1,
            (Some("COMPLETED"), Some("SUCCESS" | "SKIPPED" | "NEUTRAL")) => rollup.passed += 1,
            (Some("COMPLETED"), Some(_)) | (Some("COMPLETED"), None) => rollup.unknown += 1,
            _ => rollup.pending += 1,
        }
    }
    rollup
}

fn pr_fleet_item(
    input: &str,
    view: &GithubPrView,
    check_rollup: GithubPrCheckRollup,
) -> GithubPrFleetItem {
    let stale_base = view.merge_state.as_deref() == Some("BEHIND");
    let conflicts = view.merge_state.as_deref() == Some("DIRTY");
    let mergeable = view.state == "OPEN"
        && view.merge_state.as_deref() == Some("CLEAN")
        && view.ci_state == "terminal_green";
    let required_action = if view.state != "OPEN" {
        "noop_closed"
    } else if conflicts {
        "resolve_conflicts"
    } else if stale_base {
        "update_branch"
    } else if view.ci_state == "pending" || view.ci_state == "no_checks" {
        "wait_for_checks"
    } else if view.ci_state != "terminal_green" {
        "fix_checks"
    } else if mergeable {
        "merge"
    } else {
        "review_required"
    };

    GithubPrFleetItem {
        input: input.to_string(),
        number: Some(view.number),
        url: Some(format!(
            "https://github.com/{}/{}/pull/{}",
            view.owner, view.repo, view.number
        )),
        state: Some(view.state.clone()),
        base: Some(view.base.clone()),
        head: Some(view.head.clone()),
        head_sha: view.head_sha.clone(),
        merge_state: view.merge_state.clone(),
        ci_state: Some(view.ci_state.clone()),
        ci_summary: Some(view.ci_summary.clone()),
        review_decision: view.review_decision.clone(),
        check_rollup,
        stale_base,
        conflicts,
        mergeable,
        required_action: required_action.to_string(),
        updated: false,
        merged: false,
        error: None,
    }
}

fn pr_fleet_error_item(input: &str, number: Option<u64>, error: String) -> GithubPrFleetItem {
    GithubPrFleetItem {
        input: input.to_string(),
        number,
        url: None,
        state: None,
        base: None,
        head: None,
        head_sha: None,
        merge_state: None,
        ci_state: None,
        ci_summary: None,
        review_decision: None,
        check_rollup: GithubPrCheckRollup::default(),
        stale_base: false,
        conflicts: false,
        mergeable: false,
        required_action: "error".to_string(),
        updated: false,
        merged: false,
        error: Some(error),
    }
}

fn summarize_pr_fleet(items: &[GithubPrFleetItem]) -> GithubPrFleetSummary {
    GithubPrFleetSummary {
        total: items.len(),
        mergeable: items.iter().filter(|item| item.mergeable).count(),
        merged: items.iter().filter(|item| item.merged).count(),
        updated: items.iter().filter(|item| item.updated).count(),
        blocked: items
            .iter()
            .filter(|item| !item.mergeable && item.error.is_none())
            .count(),
        errors: items.iter().filter(|item| item.error.is_some()).count(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_pr_fleet_ref_accepts_number() {
        assert_eq!(parse_pr_fleet_ref("42", "owner", "repo").unwrap(), 42);
    }

    #[test]
    fn parse_pr_fleet_ref_accepts_matching_url() {
        assert_eq!(
            parse_pr_fleet_ref("https://github.com/owner/repo/pull/42", "owner", "repo").unwrap(),
            42
        );
    }

    #[test]
    fn parse_pr_fleet_ref_rejects_wrong_repo_url() {
        let error = parse_pr_fleet_ref("https://github.com/other/repo/pull/42", "owner", "repo")
            .unwrap_err();

        assert!(error.contains("expected owner/repo"));
    }

    #[test]
    fn summarize_check_rollup_counts_status_groups() {
        let checks = serde_json::json!([
            {"status":"COMPLETED","conclusion":"SUCCESS"},
            {"status":"COMPLETED","conclusion":"FAILURE"},
            {"status":"IN_PROGRESS","conclusion":""},
            {"status":"COMPLETED","conclusion":"BOGUS"}
        ]);
        let rollup = summarize_check_rollup(checks.as_array().unwrap());

        assert_eq!(rollup.total, 4);
        assert_eq!(rollup.passed, 1);
        assert_eq!(rollup.failed, 1);
        assert_eq!(rollup.pending, 1);
        assert_eq!(rollup.unknown, 1);
    }

    #[test]
    fn pr_fleet_item_requires_clean_green_before_merge() {
        let view = GithubPrView {
            component_id: "homeboy".into(),
            owner: "Extra-Chill".into(),
            repo: "homeboy".into(),
            number: 42,
            url: "https://github.com/Extra-Chill/homeboy/pull/42".into(),
            title: Some("Ready".into()),
            state: "OPEN".into(),
            draft: false,
            author: None,
            base: "main".into(),
            head: "feature".into(),
            head_repository: None,
            head_sha: Some("abc".into()),
            merged_at: None,
            review_decision: None,
            merge_state: Some("CLEAN".into()),
            ci_state: "terminal_green".into(),
            ci_summary: "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending".into(),
            ci_next_action: "merge_ready".into(),
        };

        let item = pr_fleet_item("42", &view, GithubPrCheckRollup::default());

        assert!(item.mergeable);
        assert_eq!(item.required_action, "merge");
    }

    #[test]
    fn pr_fleet_item_reports_stale_base_action() {
        let view = GithubPrView {
            component_id: "homeboy".into(),
            owner: "Extra-Chill".into(),
            repo: "homeboy".into(),
            number: 42,
            url: "https://github.com/Extra-Chill/homeboy/pull/42".into(),
            title: Some("Stale base".into()),
            state: "OPEN".into(),
            draft: false,
            author: None,
            base: "main".into(),
            head: "feature".into(),
            head_repository: None,
            head_sha: Some("abc".into()),
            merged_at: None,
            review_decision: None,
            merge_state: Some("BEHIND".into()),
            ci_state: "terminal_green".into(),
            ci_summary: "1 check(s): 1 terminal-green, 0 failed/unknown, 0 pending".into(),
            ci_next_action: "merge_ready".into(),
        };

        let item = pr_fleet_item("42", &view, GithubPrCheckRollup::default());

        assert!(!item.mergeable);
        assert!(item.stale_base);
        assert_eq!(item.required_action, "update_branch");
    }
}
