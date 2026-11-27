//! Explicit publication of an agent-declared partial Cook candidate.

use super::*;

/// Explicit partial recovery preserves the provider verdict and records its remainder.
pub(crate) fn publish_partial_cook(
    id: &str,
    request: &super::super::CookPublishRequest,
    preflight: bool,
) -> Result<(Value, i32)> {
    publish_partial_cook_with(
        id,
        request,
        preflight,
        &mut RealAgentTaskPrFinalizationBackend,
        &mut partial_tracker_command,
    )
}

pub(crate) fn publish_partial_cook_with<B: AgentTaskPrFinalizationBackend>(
    id: &str,
    request: &super::super::CookPublishRequest,
    preflight: bool,
    backend: &mut B,
    tracker: &mut dyn FnMut(&[&str]) -> Result<String>,
) -> Result<(Value, i32)> {
    if request.review_form.is_some() || !request.review_overrides.is_empty() {
        return Err(Error::validation_invalid_argument(
            "publish-partial",
            "partial publication must retain the agent review form unchanged",
            None,
            None,
        ));
    }
    let store = agent_task_lifecycle::AgentTaskLifecycleStore::from_environment()?;
    let recipe = super::super::load_recipe_for_attempt(id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "publish-partial",
            "a durable Cook recipe is required",
            None,
            None,
        )
    })?;
    let promotion = persisted_promotion_for_attempt(id)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "publish-partial",
            "an applied candidate is required",
            None,
            None,
        )
    })?;
    if !promotion.applied_with_passing_verdict() {
        return Err(Error::validation_invalid_argument(
            "publish-partial",
            "partial publication requires passing candidate gates",
            None,
            None,
        ));
    }
    let aggregate = store.read_aggregate(id)?;
    let form = super::super::cook::review_form_from_aggregate(&aggregate)?.ok_or_else(|| {
        Error::validation_invalid_argument(
            "publish-partial",
            "a recorded review form is required",
            None,
            None,
        )
    })?;
    form.validate(title_policy_for_recipe(&recipe, id))?;
    let reason = form.incomplete_reason().ok_or_else(|| {
        Error::validation_invalid_argument(
            "publish-partial",
            "the agent must declare incomplete with remaining scope",
            None,
            None,
        )
    })?;
    let source = recipe
        .source_refs
        .iter()
        .find(|url| crate::agent_task_notify::tracker::github_issue_ref(url).is_some())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "publish-partial",
                "a GitHub issue task tracker is required",
                None,
                None,
            )
        })?;
    let issue = crate::agent_task_notify::tracker::github_issue_ref(source).unwrap();
    let repo = format!("{}/{}", issue.owner, issue.repo);
    if preflight {
        return Ok((
            json!({"status": "validated", "remaining_scope": reason, "source": source}),
            0,
        ));
    }
    let marker = format!("<!-- homeboy-partial:{id} -->");
    let record = store.read_record(id)?;
    let follow_up = if let Some(url) = record
        .metadata
        .pointer("/partial_delivery/follow_up_url")
        .and_then(Value::as_str)
    {
        url.to_string()
    } else {
        // Search the durable remote marker before creating: retry after a lost
        // create response must not file the remaining work twice.
        let found = tracker(&[
            "issue",
            "list",
            "-R",
            &repo,
            "--state",
            "all",
            "--search",
            &format!("{id} in:body"),
            "--json",
            "url,body",
        ])?;
        let entries: Vec<Value> =
            serde_json::from_str(&found).map_err(|e| Error::internal_json(e.to_string(), None))?;
        let existing = entries
            .iter()
            .find(|entry| {
                entry["body"]
                    .as_str()
                    .is_some_and(|body| body.contains(&marker))
            })
            .and_then(|entry| entry["url"].as_str());
        let url = match existing {
            Some(url) => url.to_string(),
            None => tracker(&[
                "issue",
                "create",
                "-R",
                &repo,
                "--title",
                &format!("Remaining scope: {}", form.pr_title),
                "--body",
                &format!(
                    "{marker}\n\nRemaining scope from partial delivery of {source}:\n\n{reason}"
                ),
            ])?
            .trim()
            .to_string(),
        };
        store.compare_and_set_metadata_value(
            id,
            "partial_delivery",
            json!({"follow_up_url": url, "remaining_scope": reason, "source": source}),
        )?;
        url
    };
    let value = recover_cook_pr_with_backend_and_review_form(
        id,
        None,
        request.component.as_deref(),
        Vec::new(),
        false,
        backend,
    )?;
    // The source task and follow-up retain the publication receipt as well as
    // the PR's reviewer-facing follow-up link.
    let receipt = serde_json::to_string_pretty(&value)
        .map_err(|e| Error::internal_json(e.to_string(), None))?;
    tracker(&[
        "issue",
        "comment",
        source,
        "--body",
        &format!("Partial delivery; remaining scope tracked in {follow_up}.\n\n{receipt}"),
    ])?;
    tracker(&[
        "issue",
        "comment",
        &follow_up,
        "--body",
        &format!("Partial delivery of {source}:\n\n{receipt}"),
    ])?;
    let code = i32::from(!matches!(
        value["status"].as_str(),
        Some("succeeded_partial" | "review_ready" | "draft_published")
    ));
    Ok((value, code))
}

fn partial_tracker_command(args: &[&str]) -> Result<String> {
    let output = std::process::Command::new("gh")
        .args(args)
        .output()
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "tracker",
            String::from_utf8_lossy(&output.stderr).to_string(),
            None,
            None,
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}
