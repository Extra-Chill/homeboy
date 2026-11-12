//! The tracker note: a Cook that settles without a pull request leaves its
//! final review form on its task's GitHub issue, so the next Cook on that task
//! starts from the last one's findings instead of from zero.
//!
//! One sticky comment per Cook (keyed by cook id): a later attempt of the same
//! Cook rewrites it in place. A published attempt posts nothing; its pull
//! request is the record.

use super::tracker::{github_issue_ref, GithubIssueRef};
use crate::agent_task_review_dossier::AiFilledReviewForm;
use crate::agent_task_service::CookDelivery;

/// What one terminal attempt contributes to its tracker note.
pub(super) struct AttemptNote<'a> {
    pub run_id: &'a str,
    pub form: &'a AiFilledReviewForm,
    pub delivery: Option<&'a CookDelivery>,
    pub disclosure: &'a str,
}

/// The note for `cook_id`'s first GitHub issue source ref, or `None` when it
/// has no issue tracker or its attempt published a pull request.
pub(super) fn note(
    cook_id: &str,
    source_refs: &[String],
    attempt: &AttemptNote<'_>,
) -> Option<(GithubIssueRef, String)> {
    if attempt.delivery.is_some_and(CookDelivery::published) {
        return None;
    }
    let issue = source_refs
        .iter()
        .find_map(|source| github_issue_ref(source))?;
    let form = attempt.form;
    let mut body = format!(
        "### Cook `{cook_id}` stopped without a pull request\n\nAttempt `{}`\n\n**Summary:** {}\n",
        attempt.run_id, form.summary
    );
    if !form.what_changed.is_empty() {
        body.push_str("\n**What changed**\n");
        for change in &form.what_changed {
            body.push_str(&format!("- {change}\n"));
        }
    }
    let reason = form
        .incomplete_reason()
        .filter(|reason| !reason.is_empty())
        .map(str::to_string)
        .or_else(|| {
            attempt
                .delivery
                .and_then(|delivery| delivery.blocker.as_ref())
                .map(|blocker| blocker.message.clone())
        });
    if let Some(reason) = reason {
        body.push_str(&format!("\n**Why it stopped:** {reason}\n"));
    }
    if let Some(gate) = attempt.delivery.and_then(|delivery| delivery.gates.last()) {
        let state = serde_json::to_value(gate.state)
            .ok()
            .and_then(|value| value.as_str().map(str::to_string))
            .unwrap_or_default();
        body.push_str(&format!(
            "\n**Last gate:** `{}` {state}",
            gate.id.as_deref().unwrap_or("gate")
        ));
        if let Some(summary) = gate.summary.as_deref() {
            body.push_str(&format!(" — {summary}"));
        }
        body.push('\n');
    }
    body.push_str(&format!(
        "\n<sub>AI assistance: {} wrote this report; Homeboy posted it unedited. A later attempt of this Cook updates this comment.</sub>\n",
        attempt.disclosure
    ));
    Some((issue, body))
}

/// Post or update the note for `cook_id` from its durable records. Best
/// effort: a tracker the Cook cannot reach never changes its outcome.
pub(super) fn post(cook_id: &str) {
    let Some((issue, body)) = durable_note(cook_id) else {
        return;
    };
    deliver(cook_id, &issue, body);
}

fn durable_note(cook_id: &str) -> Option<(GithubIssueRef, String)> {
    let recipe = crate::agent_task_service::load_recipe(cook_id).ok()?;
    if recipe.finalization["tracker_note"] == false {
        return None;
    }
    let store = crate::agent_task_lifecycle::AgentTaskLifecycleStore::from_environment().ok()?;
    let run_id = store
        .read_cook_index(cook_id)
        .ok()?
        .attempts
        .last()?
        .run_id
        .clone();
    let record = store.read_record(&run_id).ok()?;
    if record.state() == crate::agent_task_lifecycle::AgentTaskRunState::Cancelled {
        return None;
    }
    let aggregate = crate::agent_task_lifecycle::read_attempt_aggregate(&run_id).ok()?;
    let form = crate::agent_task_service::review_form_from_aggregate(&aggregate).ok()??;
    let delivery = CookDelivery::from_run_metadata(&record.metadata);
    let provenance = crate::execution_provenance::execution_provenance(&record);
    let disclosure = format!(
        "{} / {}",
        crate::agent_task_review_dossier::homeboy_tool_disclosure(&provenance.tool),
        crate::execution_provenance::render_execution_provenance(&provenance)
    );
    note(
        cook_id,
        &recipe.source_refs,
        &AttemptNote {
            run_id: &run_id,
            form: &form,
            delivery: delivery.as_ref(),
            disclosure: &disclosure,
        },
    )
}

#[cfg(not(any(test, feature = "test-support")))]
fn deliver(cook_id: &str, issue: &GithubIssueRef, body: String) {
    let repo = homeboy_core::git::release_download::GitHubRepo {
        host: "github.com".to_string(),
        owner: issue.owner.clone(),
        repo: issue.repo.clone(),
    };
    let _ = homeboy_core::git::repo_comment(
        &repo,
        homeboy_core::git::PrCommentOptions {
            number: issue.number,
            body,
            mode: homeboy_core::git::PrCommentMode::StickyWholeBody {
                key: format!("agent-task-note:{cook_id}"),
            },
            ..Default::default()
        },
    );
}

/// Test builds never write to GitHub: fixtures name real repositories.
#[cfg(any(test, feature = "test-support"))]
fn deliver(_cook_id: &str, _issue: &GithubIssueRef, _body: String) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_unpublished_attempt_leaves_its_findings_on_the_task_issue() {
        let form: AiFilledReviewForm = serde_json::from_value(serde_json::json!({
            "pr_title": "fix: header parts",
            "summary": "Traced the header template-part references.",
            "what_changed": ["Nothing kept."],
            "completion": "incomplete",
            "incomplete_reason": "The fix retained all 3 trunk occurrences, so it was reverted."
        }))
        .unwrap();
        let issue = ["https://github.com/o/r/issues/7".to_string()];
        let attempt = |delivery| AttemptNote {
            run_id: "cook-a2",
            form: &form,
            delivery,
            disclosure: "Homeboy (Pi) / openai/gpt-6",
        };

        let (tracker, body) = note("cook", &issue, &attempt(None)).unwrap();
        assert_eq!((tracker.owner.as_str(), tracker.number), ("o", 7));
        for expected in [
            "Cook `cook` stopped without a pull request",
            "Attempt `cook-a2`",
            "Traced the header template-part references.",
            "- Nothing kept.",
            "**Why it stopped:** The fix retained all 3 trunk occurrences",
            "Homeboy (Pi) / openai/gpt-6",
        ] {
            assert!(body.contains(expected), "{expected}\n{body}");
        }

        assert!(note(
            "cook",
            &["https://github.com/o/r/pull/7".into()],
            &attempt(None)
        )
        .is_none());
        let published: CookDelivery = serde_json::from_value(serde_json::json!({
            "state": "published",
            "publication": {"status": "created", "published": true, "draft": false}
        }))
        .unwrap();
        assert!(note("cook", &issue, &attempt(Some(&published))).is_none());
    }
}
