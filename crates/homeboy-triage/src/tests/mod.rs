use super::*;

mod observations;
mod parsing;
mod priority_and_summary;
mod pull_requests;
mod targets;

pub(crate) fn triage_pr_with_action(action: &str) -> TriagePrItem {
    TriagePrItem {
        number: 1,
        title: "PR".to_string(),
        url: "https://github.com/o/r/pull/1".to_string(),
        state: "OPEN".to_string(),
        draft: false,
        signals: TriagePullRequestSignals {
            next_action: Some(action.to_string()),
            ..TriagePullRequestSignals::default()
        },
        ci_readiness: None,
        check_failures: Vec::new(),
        labels: vec![],
        assignees: vec![],
        author: None,
        updated_at: None,
        stale: false,
    }
}

pub(crate) fn stored_triage_item(
    number: u64,
    title: &str,
    next_action: Option<&str>,
) -> TriageItemRecord {
    TriageItemRecord {
        id: format!("item-{number}"),
        run_id: "previous-run".to_string(),
        provider: "github".to_string(),
        repo_owner: "Extra-Chill".to_string(),
        repo_name: "homeboy".to_string(),
        item_type: "pull_request".to_string(),
        number,
        state: "OPEN".to_string(),
        title: title.to_string(),
        url: format!("https://github.com/Extra-Chill/homeboy/pull/{number}"),
        signals: TriagePullRequestSignals {
            next_action: next_action.map(str::to_string),
            ..TriagePullRequestSignals::default()
        },
        updated_at: None,
        metadata_json: serde_json::json!({}),
        observed_at: "2026-05-08T12:00:00Z".to_string(),
    }
}

pub(crate) fn new_triage_item(
    run_id: &str,
    number: u64,
    title: &str,
    next_action: Option<&str>,
) -> NewTriageItemRecord {
    NewTriageItemRecord {
        run_id: run_id.to_string(),
        provider: "github".to_string(),
        repo_owner: "Extra-Chill".to_string(),
        repo_name: "homeboy".to_string(),
        item_type: "pull_request".to_string(),
        number,
        state: "OPEN".to_string(),
        title: title.to_string(),
        url: format!("https://github.com/Extra-Chill/homeboy/pull/{number}"),
        signals: TriagePullRequestSignals {
            next_action: next_action.map(str::to_string),
            ..TriagePullRequestSignals::default()
        },
        updated_at: None,
        metadata_json: serde_json::json!({}),
    }
}

pub(crate) fn default_priority_labels_vec() -> Vec<String> {
    DEFAULT_PRIORITY_LABELS
        .iter()
        .map(|label| label.to_string())
        .collect()
}

pub(crate) fn issues_with_labels(labels: Vec<Vec<&str>>) -> TriageIssueBucket {
    TriageIssueBucket {
        open: labels.len(),
        items: labels
            .into_iter()
            .enumerate()
            .map(|(index, labels)| TriageIssueItem {
                number: index as u64 + 1,
                title: format!("Issue {}", index + 1),
                url: format!("https://github.com/o/r/issues/{}", index + 1),
                state: "OPEN".to_string(),
                labels: labels.into_iter().map(str::to_string).collect(),
                assignees: vec![],
                updated_at: None,
                comments_count: None,
                last_comment_at: None,
                stale: false,
                linked_prs: Vec::new(),
            })
            .collect(),
    }
}
