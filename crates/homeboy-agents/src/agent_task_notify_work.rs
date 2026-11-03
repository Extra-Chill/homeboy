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
    pr: Option<String>,
    pub facts: Vec<(String, String)>,
}

impl Work {
    /// The headline names the work. Outcome markers belong to the transport.
    pub fn title(&self, kind: NotifyEventKind) -> String {
        if !self.cancelled && kind == NotifyEventKind::Completed {
            if let Some(pr) = &self.pr {
                return format!("{pr}: {}", short_title(&self.task));
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
    let task_title = finalization
        .and_then(|value| value["title"].as_str())
        .or_else(|| {
            recipe
                .as_ref()
                .and_then(|recipe| recipe.finalization["title"].as_str())
        })
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .or_else(|| task.and_then(|task| task.instructions.lines().next().map(str::to_string)))
        .filter(|title| !title.trim().is_empty())
        .unwrap_or_else(|| "Cook task".to_string());
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
                let url = if reference.starts_with("https://") {
                    Some(reference.to_string())
                } else if let Some((repo, number)) = reference
                    .split_once('#')
                    .filter(|(repo, _)| !repo.is_empty())
                {
                    Some(format!("https://github.com/{repo}/issues/{number}"))
                } else {
                    pr_url
                        .as_deref()
                        .and_then(|pr| pr.split_once("/pull/"))
                        .map(|(repo, _)| {
                            format!("{repo}/issues/{}", reference.trim_start_matches('#'))
                        })
                };
                if let Some(url) = url {
                    if let Some((repo, number)) = url.split_once("/issues/") {
                        let repo = repo.rsplit('/').next().unwrap_or(repo);
                        facts.push((label.to_string(), format!("[{repo}#{number}]({url})")));
                    }
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
    let task = short_title(task);
    match component {
        Some(component) => format!("{component}: {task} — {outcome}"),
        None => format!("{task} — {outcome}"),
    }
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
    let title = recipe
        .as_ref()
        .and_then(|recipe| recipe.finalization["title"].as_str())
        .or_else(|| {
            recipe
                .as_ref()?
                .attempts
                .first()?
                .plan
                .tasks
                .first()?
                .instructions
                .lines()
                .next()
        })
        .unwrap_or("Cook task");
    work_title(component, title, &format!("attempt {attempt} of {max}"))
}
