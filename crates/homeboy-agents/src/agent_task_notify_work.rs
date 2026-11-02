//! Work identity and decision facts for terminal cook notifications.
use super::*;

pub(super) struct Work {
    pub component: String,
    pub cancelled: bool,
    task: String,
    pr: Option<String>,
    pub facts: Vec<(String, String)>,
}

impl Work {
    pub fn title(&self, kind: NotifyEventKind) -> String {
        if !self.cancelled && kind == NotifyEventKind::Completed {
            if let Some(pr) = &self.pr {
                return format!("✅ {pr}: {}", self.task);
            }
        }
        let outcome = if self.cancelled {
            "cancelled"
        } else if kind == NotifyEventKind::Completed {
            "succeeded"
        } else {
            "needs attention"
        };
        format!("{}: {} — {outcome}", self.component, self.task)
    }
}

pub(super) fn metadata(report: &AgentTaskCookReport, component: Option<&str>) -> Work {
    let recipe = if let Some((store, _)) = &report.report_stores {
        store.load_recipe(&report.cook_id).ok()
    } else {
        crate::agent_task_service::CookRecipeStore::from_current_data_root()
            .ok()
            .and_then(|store| store.load_recipe(&report.cook_id).ok())
    };
    let task = recipe
        .as_ref()
        .and_then(|recipe| recipe.attempts.first())
        .and_then(|attempt| attempt.plan.tasks.first());
    let finalization = report.finalization.as_ref();
    let url = finalization.and_then(|value| {
        let receipt = crate::agent_task_service::publication_receipt(value);
        receipt.published.then_some(receipt.pr_url).flatten()
    });
    let repo = url
        .as_deref()
        .and_then(|url| url.split("/pull/").next())
        .and_then(|url| url.rsplit('/').next());
    let repository = recipe
        .as_ref()
        .and_then(|recipe| recipe.finalization["source_worktree_path"].as_str())
        .and_then(|path| {
            let remote = homeboy_core::git::run_git(
                std::path::Path::new(path),
                &["remote", "get-url", "origin"],
                "notification repository",
            )
            .ok()?;
            remote
                .trim()
                .trim_end_matches(".git")
                .rsplit('/')
                .next()
                .map(str::to_string)
        });
    let component = repo
        .map(str::to_string)
        .or(repository)
        .or_else(|| component.map(str::to_string))
        .or_else(|| task.and_then(|task| task.workspace.component_id.clone()))
        .or_else(|| task.and_then(|task| task.workspace.slug.clone()))
        .unwrap_or_else(|| "repository".to_string());
    let title = finalization
        .and_then(|value| value["title"].as_str())
        .filter(|title| !title.is_empty())
        .map(str::to_string)
        .or_else(|| {
            recipe
                .as_ref()
                .and_then(|recipe| recipe.finalization["title"].as_str().map(str::to_string))
        })
        .or_else(|| {
            task.map(|task| {
                task.instructions
                    .lines()
                    .next()
                    .unwrap_or("Cook task")
                    .to_string()
            })
        })
        .unwrap_or_else(|| "Cook task".to_string());
    let record = report
        .latest_run_id
        .as_deref()
        .or(Some(&report.cook_id))
        .and_then(|id| {
            if let Some((_, store)) = &report.report_stores {
                crate::agent_task_lifecycle::status_in_store(store, id).ok()
            } else {
                crate::agent_task_lifecycle::status(id).ok()
            }
        });
    let cancelled = report.status == "cancelled"
        || record.as_ref().is_some_and(|record| {
            record.state() == crate::agent_task_lifecycle::AgentTaskRunState::Cancelled
        });
    let mut facts = Vec::new();
    if let Some(value) = finalization {
        if let Some(relationships) = value
            .pointer("/review_dossier/source_relationships")
            .and_then(Value::as_array)
        {
            for relationship in relationships {
                if let Some(reference) = relationship["reference"].as_str() {
                    facts.push((
                        if relationship["kind"] == "closes" {
                            "Closes"
                        } else {
                            "Related issue"
                        }
                        .to_string(),
                        reference.to_string(),
                    ));
                }
            }
        }
        if let Some(model) = value["ai_model"].as_str().or_else(|| {
            value
                .pointer("/review_dossier/ai_assistance/model")
                .and_then(Value::as_str)
        }) {
            facts.push(("Model".to_string(), model.to_string()));
        }
        if let (Some(path), Some(base), Some(head)) = (
            value["path"].as_str(),
            value
                .pointer("/review_dossier/verified_base/sha")
                .and_then(Value::as_str),
            value["head"].as_str(),
        ) {
            if let Ok(stats) = homeboy_core::git::run_git(
                std::path::Path::new(path),
                &["diff", "--numstat", base, head],
                "notification diff stats",
            ) {
                let (mut additions, mut deletions) = (0_u64, 0_u64);
                for line in stats.lines() {
                    let mut fields = line.split_whitespace();
                    additions += fields
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                    deletions += fields
                        .next()
                        .and_then(|s| s.parse::<u64>().ok())
                        .unwrap_or(0);
                }
                facts.push(("Diff".to_string(), format!("+{additions}/−{deletions}")));
            }
        }
    }
    if !facts.iter().any(|(label, _)| label == "Model") {
        if let Some(model) = record
            .as_ref()
            .and_then(|record| record.served_models.last())
            .cloned()
            .or_else(|| task.and_then(|task| task.executor.model.clone()))
        {
            facts.push(("Model".to_string(), model));
        }
    }
    if let Some(record) = &record {
        if let (Ok(start), Some(Ok(end))) = (
            chrono::DateTime::parse_from_rfc3339(&record.submitted_at),
            record
                .updated_at
                .as_ref()
                .map(|end| chrono::DateTime::parse_from_rfc3339(end)),
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
        task: title,
        pr: url.as_deref().and_then(|url| {
            let (repo, number) = url.split_once("/pull/")?;
            Some(format!("{}#{number}", repo.rsplit('/').next()?))
        }),
        facts,
    }
}
