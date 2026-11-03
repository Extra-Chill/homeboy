//! Work identity and decision facts for terminal cook notifications.
//!
//! Everything here is read from what the cook already recorded (its report,
//! recipe and lifecycle record). A notification never runs Git or any other
//! subprocess to rediscover it; an unrecorded value is simply omitted.
use super::*;

pub(super) struct Work {
    pub component: Option<String>,
    pub cancelled: bool,
    task: String,
    pub pr: Option<String>,
    pub links: Vec<homeboy_core::notification_payload::NotifyLink>,
    pub facts: Vec<(String, String)>,
}

impl Work {
    /// The headline names the work. Outcome markers belong to the transport.
    pub fn title(&self, kind: NotifyEventKind) -> String {
        if !self.cancelled && kind == NotifyEventKind::Completed {
            if self.pr.is_some() {
                return short_title(&self.task);
            }
        }
        let outcome = if self.cancelled {
            "cancelled"
        } else if kind == NotifyEventKind::Completed {
            "succeeded"
        } else {
            "needs attention"
        };
        work_title(self.component.as_deref(), &self.task, outcome)
    }
}

pub(super) fn metadata(report: &AgentTaskCookReport, component: Option<&str>) -> Work {
    let recipe = match &report.report_stores {
        Some((store, _)) => store.load_recipe(&report.cook_id).ok(),
        None => crate::agent_task_service::CookRecipeStore::from_current_data_root()
            .ok()
            .and_then(|store| store.load_recipe(&report.cook_id).ok()),
    };
    let task = recipe
        .as_ref()
        .and_then(|recipe| recipe.attempts.first())
        .and_then(|attempt| attempt.plan.tasks.first());
    let finalization = report.finalization.as_ref();
    let pr_url = finalization.and_then(|value| {
        let receipt = crate::agent_task_service::publication_receipt(value);
        receipt.published.then_some(receipt.pr_url).flatten()
    });
    let pr = pr_url.as_deref().and_then(|url| {
        let (repository, number) = url.split_once("/pull/")?;
        Some(format!("{}#{number}", repository.rsplit('/').next()?))
    });
    let component = component.map(str::to_string);
    let task_title = resolve_title(finalization, recipe.as_ref());
    let record_id = report.latest_run_id.as_deref().unwrap_or(&report.cook_id);
    let record = match &report.report_stores {
        Some((_, store)) => crate::agent_task_lifecycle::status_in_store(store, record_id).ok(),
        None => crate::agent_task_lifecycle::status(record_id).ok(),
    };
    let cancelled = report.status == "cancelled"
        || record.as_ref().is_some_and(|record| {
            record.state() == crate::agent_task_lifecycle::AgentTaskRunState::Cancelled
        });

    let mut facts = Vec::new();
    let mut links = Vec::new();
    if let Some(relationships) = finalization
        .and_then(|value| value.pointer("/review_dossier/source_relationships"))
        .and_then(Value::as_array)
    {
        for relationship in relationships {
            if let Some(reference) = relationship["reference"].as_str() {
                let label = if relationship["kind"] == "closes" {
                    "Closes"
                } else {
                    "Related issue"
                };
                let (text, url) = issue_reference(reference, pr_url.as_deref());
                let text = format!("{label} {text}");
                if let Some(url) = url {
                    links.push(homeboy_core::notification_payload::NotifyLink::new(
                        text, url,
                    ));
                } else {
                    facts.push((label.to_string(), text));
                }
            }
        }
    }
    let model = finalization
        .and_then(|value| {
            value["ai_model"].as_str().or_else(|| {
                value
                    .pointer("/review_dossier/ai_assistance/model")
                    .and_then(Value::as_str)
            })
        })
        .map(str::to_string)
        .or_else(|| {
            record
                .as_ref()
                .and_then(|record| record.served_models.last().cloned())
        })
        .or_else(|| task.and_then(|task| task.executor.model.clone()));
    if let Some(model) = model {
        facts.push(("Model".to_string(), model));
    }
    if let Some(record) = &record {
        if let (Ok(start), Some(Ok(end))) = (
            chrono::DateTime::parse_from_rfc3339(&record.submitted_at),
            record
                .updated_at
                .as_deref()
                .map(chrono::DateTime::parse_from_rfc3339),
        ) {
            facts.push((
                "Duration".to_string(),
                format!("{}m", (end - start).num_minutes().max(0)),
            ));
        }
    }
    Work {
        component,
        cancelled,
        task: task_title,
        pr,
        links,
        facts,
    }
}

fn bounded(text: &str, max: usize) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.chars().count() <= max {
        return text;
    }
    let prefix: String = text.chars().take(max - 1).collect();
    let end = prefix.rfind(' ').unwrap_or(prefix.len());
    format!("{}…", &prefix[..end])
}

pub(super) fn short_title(text: &str) -> String {
    bounded(text, 80)
}

pub(super) fn stop_reason(text: &str) -> String {
    let end = text
        .char_indices()
        .find_map(|(index, ch)| {
            (matches!(ch, '.' | '!' | '?')
                && text[index + ch.len_utf8()..].starts_with(char::is_whitespace))
            .then_some(index + ch.len_utf8())
        })
        .unwrap_or(text.len());
    bounded(&text[..end], 200)
}

pub(super) fn work_title(component: Option<&str>, task: &str, outcome: &str) -> String {
    let prefix = component
        .map(|value| format!("{value}: "))
        .unwrap_or_default();
    let suffix = format!(" — {outcome}");
    let budget = 80usize.saturating_sub(suffix.chars().count());
    format!("{}{suffix}", bounded(&format!("{prefix}{task}"), budget))
}

pub(super) fn retry_title(
    cook_id: &str,
    component: Option<&str>,
    attempt: u32,
    max: u32,
) -> String {
    let recipe = crate::agent_task_service::CookRecipeStore::from_current_data_root()
        .ok()
        .and_then(|store| store.load_recipe(cook_id).ok());
    let title = resolve_title(None, recipe.as_ref());
    work_title(component, &title, &format!("attempt {attempt} of {max}"))
}

fn resolve_title(
    finalization: Option<&Value>,
    recipe: Option<&crate::agent_task_service::AgentTaskCookRecipe>,
) -> String {
    finalization
        .and_then(|value| value["title"].as_str())
        .filter(|title| !title.trim().is_empty())
        .or_else(|| {
            recipe
                .and_then(|recipe| recipe.finalization["title"].as_str())
                .filter(|title| !title.trim().is_empty())
        })
        .or_else(|| {
            recipe?
                .attempts
                .first()?
                .plan
                .tasks
                .first()?
                .instructions
                .lines()
                .next()
                .filter(|title| !title.trim().is_empty())
        })
        .unwrap_or("Cook task")
        .to_string()
}

fn issue_reference(reference: &str, pr: Option<&str>) -> (String, Option<String>) {
    if reference.starts_with("https://") || reference.starts_with("http://") {
        let text = reference
            .split_once("/issues/")
            .or_else(|| reference.split_once("/pull/"))
            .map(|(repo, number)| format!("{}#{number}", repo.rsplit('/').next().unwrap_or(repo)))
            .unwrap_or_else(|| reference.to_string());
        return (text, Some(reference.to_string()));
    }
    let Some((base, _)) = pr.and_then(|url| url.split_once("/pull/")) else {
        return (reference.to_string(), None);
    };
    let (repo, number) = reference.split_once('#').unwrap_or(("", reference));
    let target = if repo.is_empty() {
        base.to_string()
    } else if repo.contains('/') {
        let Some((scheme, rest)) = base.split_once("://") else {
            return (reference.to_string(), None);
        };
        format!(
            "{scheme}://{}/{repo}",
            rest.split('/').next().unwrap_or(rest)
        )
    } else {
        format!(
            "{}/{repo}",
            base.rsplit_once('/')
                .map(|(owner, _)| owner)
                .unwrap_or(base)
        )
    };
    (
        format!("{}#{number}", target.rsplit('/').next().unwrap_or(&target)),
        Some(format!("{target}/issues/{number}")),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linked_references_preserve_recorded_hosts_and_plain_fallbacks() {
        for (reference, pr, text, url) in [
            (
                "#7",
                Some("https://forge.test/team/repo/pull/1"),
                "repo#7",
                Some("https://forge.test/team/repo/issues/7"),
            ),
            (
                "other#8",
                Some("https://forge.test/team/repo/pull/1"),
                "other#8",
                Some("https://forge.test/team/other/issues/8"),
            ),
            (
                "owner/other#9",
                Some("https://forge.test/team/repo/pull/1"),
                "other#9",
                Some("https://forge.test/owner/other/issues/9"),
            ),
            (
                "https://elsewhere.test/team/other/issues/10",
                None,
                "other#10",
                Some("https://elsewhere.test/team/other/issues/10"),
            ),
            (
                "https://elsewhere.test/tickets/11",
                None,
                "https://elsewhere.test/tickets/11",
                Some("https://elsewhere.test/tickets/11"),
            ),
            ("#12", None, "#12", None),
            ("owner/repo#13", None, "owner/repo#13", None),
        ] {
            assert_eq!(
                issue_reference(reference, pr),
                (text.to_string(), url.map(str::to_string))
            );
        }
    }

    #[test]
    fn outcome_survives_title_bounding() {
        for outcome in ["needs attention", "cancelled", "attempt 2 of 3", "started"] {
            let title = work_title(Some("studio"), &"long task title ".repeat(30), outcome);
            assert!(title.chars().count() <= 80, "{title}");
            assert!(title.ends_with(&format!(" — {outcome}")), "{title}");
        }
    }
}
