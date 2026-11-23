use super::*;
use serde_json::json;

const PR: &str = "https://github.com/Automattic/blocks-engine/pull/7420";
const ISSUE: &str = "https://github.com/Extra-Chill/homeboy/issues/18884";

fn pr_view() -> Value {
    json!({
        "title": "Render the block",
        "body": "Initial spec.",
        "headRefOid": "head-2",
        "commits": [
            { "oid": "head-1", "committedDate": "2026-01-01T10:00:00Z" },
            { "oid": "head-2", "committedDate": "2026-01-02T10:00:00Z" }
        ],
        "comments": [
            { "author": { "login": "old" }, "body": "Addressed already.", "createdAt": "2026-01-01T12:00:00Z" },
            { "author": { "login": "lead" }, "body": "Rework:\n1. a\n2. b", "createdAt": "2026-01-03T09:00:00Z",
              "url": "https://github.com/Automattic/blocks-engine/pull/7420#issuecomment-1" }
        ],
        "reviews": [
            { "author": { "login": "rev" }, "body": "Stale review.", "state": "COMMENTED", "submittedAt": "2026-01-01T11:00:00Z" },
            { "author": { "login": "rev" }, "body": "Still missing b.", "state": "CHANGES_REQUESTED", "submittedAt": "2026-01-02T11:00:00+00:00" }
        ]
    })
}

#[test]
fn classifies_github_issue_and_pull_request_urls() {
    assert_eq!(task_source_kind(ISSUE), Some(TaskSourceKind::Issue));
    assert_eq!(
        task_source_kind(&format!("{PR}#discussion")),
        Some(TaskSourceKind::PullRequest)
    );
    assert_eq!(task_source_kind("https://example.com/issues/1"), None);
    assert_eq!(
        task_source_kind("https://github.com/o/r/pull/1/files"),
        None
    );
}

/// The rework spec posted as the latest PR comment reaches the snapshot;
/// discussion that predates the head commit does not.
#[test]
fn pull_request_snapshot_keeps_only_feedback_after_the_head_commit() {
    let snapshot = pull_request_snapshot(PR, &pr_view());
    assert_eq!(snapshot.kind, TaskSourceKind::PullRequest);
    assert_eq!(snapshot.body, "Initial spec.");
    assert_eq!(snapshot.head_sha.as_deref(), Some("head-2"));
    assert_eq!(snapshot.since.as_deref(), Some("2026-01-02T10:00:00Z"));
    assert_eq!(snapshot.comments.len(), 1);
    assert_eq!(snapshot.comments[0].author, "lead");
    assert!(snapshot.comments[0].body.contains("Rework:"));
    assert_eq!(snapshot.reviews.len(), 1);
    assert_eq!(
        snapshot.reviews[0].state.as_deref(),
        Some("CHANGES_REQUESTED")
    );
    assert_eq!(snapshot.omitted_before_head, 2);
    assert!(snapshot.revision.starts_with("sha256:"));
}

#[test]
fn issue_snapshot_keeps_title_body_and_every_comment() {
    let snapshot = issue_snapshot(
        ISSUE,
        &json!({
            "title": "Snapshot the spec",
            "body": "Body.",
            "comments": [
                { "author": { "login": "b" }, "body": "second", "createdAt": "2026-01-02T00:00:00Z" },
                { "author": { "login": "a" }, "body": "first", "createdAt": "2026-01-01T00:00:00Z" }
            ]
        }),
    );
    assert_eq!(snapshot.title, "Snapshot the spec");
    assert_eq!(snapshot.body, "Body.");
    let bodies: Vec<_> = snapshot.comments.iter().map(|c| c.body.as_str()).collect();
    assert_eq!(bodies, ["first", "second"]);
    assert_eq!(snapshot.since, None);
}

#[test]
fn revision_tracks_content_and_linked_issue() {
    let first = pull_request_snapshot(PR, &pr_view());
    assert_eq!(
        first.revision,
        pull_request_snapshot(PR, &pr_view()).revision
    );
    let mut edited = pr_view();
    edited["comments"][1]["body"] = json!("Rework: only a");
    assert_ne!(first.revision, pull_request_snapshot(PR, &edited).revision);

    let issue = issue_snapshot(ISSUE, &json!({ "title": "t", "body": "b" }));
    let linked = with_linked_issue(first.clone(), issue);
    assert_ne!(linked.revision, first.revision);
    assert_eq!(linked.linked_issue.as_ref().unwrap().title, "t");
}

#[test]
fn applying_the_snapshot_records_prompt_input_and_source_revision() {
    let mut request: AgentTaskRequest = serde_json::from_value(json!({
        "schema": crate::agent_task::AGENT_TASK_REQUEST_SCHEMA,
        "task_id": "cook",
        "executor": { "backend": "test" },
        "instructions": PR,
        "source_refs": [{ "kind": "task", "uri": PR }],
        "workspace": {}
    }))
    .expect("request");
    let snapshot = pull_request_snapshot(PR, &pr_view());
    apply_task_source_snapshot(&mut request, &snapshot);

    assert_eq!(request.instructions, PR, "the prompt prose is unchanged");
    assert_eq!(
        request.inputs["homeboy"][TASK_SOURCE_INPUT_KEY]["comments"][0]["author"],
        "lead"
    );
    assert_eq!(request.source_refs.len(), 1);
    assert_eq!(
        request.source_refs[0].revision.as_deref(),
        Some(snapshot.revision.as_str())
    );
}

#[test]
fn oversized_text_is_bounded() {
    let view = json!({ "title": "t", "body": "x".repeat(TASK_SOURCE_TEXT_MAX_CHARS + 10) });
    let snapshot = issue_snapshot(ISSUE, &view);
    assert!(snapshot.body.ends_with("characters]"));
    assert!(snapshot.body.chars().count() < TASK_SOURCE_TEXT_MAX_CHARS + 100);
}
