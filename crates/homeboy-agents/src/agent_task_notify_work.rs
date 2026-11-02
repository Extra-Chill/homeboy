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
                return format!("{pr}: {}", self.task);
            }
        }
        let outcome = if self.cancelled {
            "cancelled"
        } else if kind == NotifyEventKind::Completed {
            "succeeded"
        } else {
            "needs attention"
        };
        match &self.component {
            Some(component) => format!("{component}: {} — {outcome}", self.task),
            None => format!("{} — {outcome}", self.task),
        }
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
                facts.push((label.to_string(), reference.to_string()));
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
