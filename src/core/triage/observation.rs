use std::collections::BTreeMap;

use serde_json::Value;

use crate::core::observation::{
    NewRunRecord, NewTriageItemRecord, ObservationStore, RunListFilter, RunStatus,
    TriageItemRecord, TriagePullRequestSignals,
};

use super::{
    triage_command, TriageObservationChangedItem, TriageObservationComparison,
    TriageObservationItemRef, TriageObservationOutput, TriageOptions, TriageOutput, TriageTarget,
};

pub(super) struct TriageObservation {
    store: ObservationStore,
    run_id: String,
    store_path: String,
    previous_run_id: Option<String>,
    previous_run_at: Option<String>,
}

impl TriageObservation {
    pub(super) fn start(target: &TriageTarget, options: &TriageOptions) -> Option<Self> {
        let store = ObservationStore::open_initialized().ok()?;
        let component_id = triage_observation_component_id(target);
        let metadata = triage_observation_metadata(target, options);
        let previous_run = store
            .latest_run(RunListFilter {
                kind: Some("triage".to_string()),
                component_id: Some(component_id.clone()),
                status: None,
                rig_id: None,
                limit: Some(1),
            })
            .ok()
            .flatten()
            .filter(|run| run.metadata_json == metadata);
        let store_path = store
            .status()
            .map(|status| status.path)
            .unwrap_or_else(|_| "<unavailable>".to_string());
        let cwd = std::env::current_dir().ok();
        let run = store
            .start_run(
                NewRunRecord::builder("triage")
                    .component_id(component_id)
                    .command(triage_command(target))
                    .optional_cwd_path(cwd.as_deref())
                    .current_homeboy_version()
                    .optional_rig_id(match target {
                        TriageTarget::Rig(id) => Some(id.clone()),
                        _ => None,
                    })
                    .metadata(metadata)
                    .build(),
            )
            .ok()?;

        Some(Self {
            store,
            run_id: run.id,
            store_path,
            previous_run_id: previous_run.as_ref().map(|run| run.id.clone()),
            previous_run_at: previous_run.map(|run| run.started_at),
        })
    }

    pub(super) fn finish(self, output: &TriageOutput) -> Option<TriageObservationOutput> {
        let items = triage_observation_items(&self.run_id, output);
        let item_count = items.len();
        let previous_items = self
            .previous_run_id
            .as_deref()
            .and_then(|run_id| self.store.list_triage_items_for_run(run_id).ok());
        let comparison = self
            .previous_run_id
            .as_ref()
            .zip(previous_items.as_ref())
            .map(|(previous_run_id, previous_items)| {
                compare_triage_observations(previous_run_id, previous_items, &items)
            });
        let record_result = self.store.record_triage_items(&items);
        let status = if record_result.is_ok() {
            RunStatus::Pass
        } else {
            RunStatus::Error
        };
        let _ = self.store.finish_run(
            &self.run_id,
            status,
            Some(serde_json::json!({
                "summary": output.summary,
                "item_count": item_count,
                "recorded": record_result.is_ok(),
            })),
        );

        if record_result.is_err() {
            return None;
        }

        Some(TriageObservationOutput {
            run_id: self.run_id,
            item_count,
            store_path: self.store_path,
            previous_run_at: self.previous_run_at,
            comparison,
        })
    }
}

pub(super) fn triage_observation_metadata(target: &TriageTarget, options: &TriageOptions) -> Value {
    serde_json::json!({
        "target": {
            "kind": target.kind_name(),
            "id": target.id(),
        },
        "options": {
            "include_issues": options.include_issues,
            "include_prs": options.include_prs,
            "mine": options.mine,
            "assigned": options.assigned,
            "labels": options.labels,
            "needs_review": options.needs_review,
            "failing_checks": options.failing_checks,
            "drilldown": options.drilldown,
            "issue_numbers": options.issue_numbers,
            "stale_days": options.stale_days,
            "limit": options.limit,
        }
    })
}

type TriageObservationItemKey = (String, String, String, String, u64);

pub(super) fn compare_triage_observations(
    previous_run_id: &str,
    previous_items: &[TriageItemRecord],
    current_items: &[NewTriageItemRecord],
) -> TriageObservationComparison {
    let previous_by_key: BTreeMap<_, _> = previous_items
        .iter()
        .map(|item| (triage_record_key(item), item))
        .collect();
    let current_by_key: BTreeMap<_, _> = current_items
        .iter()
        .map(|item| (triage_new_item_key(item), item))
        .collect();

    let new_items = current_by_key
        .iter()
        .filter(|(key, _)| !previous_by_key.contains_key(*key))
        .map(|(_, item)| triage_new_item_ref(item))
        .collect();
    let resolved_items = previous_by_key
        .iter()
        .filter(|(key, _)| !current_by_key.contains_key(*key))
        .map(|(_, item)| triage_record_item_ref(item))
        .collect();
    let changed_items = current_by_key
        .iter()
        .filter_map(|(key, current)| {
            let previous = previous_by_key.get(key)?;
            let changed_fields = triage_changed_fields(previous, current);
            if changed_fields.is_empty() {
                return None;
            }
            Some(TriageObservationChangedItem {
                item: triage_new_item_ref(current),
                changed_fields,
            })
        })
        .collect();

    TriageObservationComparison {
        previous_run_id: previous_run_id.to_string(),
        previous_item_count: previous_items.len(),
        new_items,
        resolved_items,
        changed_items,
    }
}

fn triage_record_key(item: &TriageItemRecord) -> TriageObservationItemKey {
    (
        item.provider.clone(),
        item.repo_owner.clone(),
        item.repo_name.clone(),
        item.item_type.clone(),
        item.number,
    )
}

fn triage_new_item_key(item: &NewTriageItemRecord) -> TriageObservationItemKey {
    (
        item.provider.clone(),
        item.repo_owner.clone(),
        item.repo_name.clone(),
        item.item_type.clone(),
        item.number,
    )
}

fn triage_record_item_ref(item: &TriageItemRecord) -> TriageObservationItemRef {
    TriageObservationItemRef {
        repo: format!("{}/{}", item.repo_owner, item.repo_name),
        item_type: item.item_type.clone(),
        number: item.number,
        title: item.title.clone(),
        url: item.url.clone(),
    }
}

fn triage_new_item_ref(item: &NewTriageItemRecord) -> TriageObservationItemRef {
    TriageObservationItemRef {
        repo: format!("{}/{}", item.repo_owner, item.repo_name),
        item_type: item.item_type.clone(),
        number: item.number,
        title: item.title.clone(),
        url: item.url.clone(),
    }
}

fn triage_changed_fields(
    previous: &TriageItemRecord,
    current: &NewTriageItemRecord,
) -> Vec<String> {
    let mut fields = Vec::new();
    push_if_changed(&mut fields, "state", &previous.state, &current.state);
    push_if_changed(&mut fields, "title", &previous.title, &current.title);
    push_if_changed(&mut fields, "url", &previous.url, &current.url);
    push_if_changed(
        &mut fields,
        "checks",
        &previous.signals.checks,
        &current.signals.checks,
    );
    push_if_changed(
        &mut fields,
        "review_decision",
        &previous.signals.review_decision,
        &current.signals.review_decision,
    );
    push_if_changed_unless_unknown(
        &mut fields,
        "merge_state",
        &previous.signals.merge_state,
        &current.signals.merge_state,
    );
    push_if_changed(
        &mut fields,
        "next_action",
        &previous.signals.next_action,
        &current.signals.next_action,
    );
    push_if_changed(
        &mut fields,
        "comments_count",
        &previous.signals.comments_count,
        &current.signals.comments_count,
    );
    push_if_changed(
        &mut fields,
        "reviews_count",
        &previous.signals.reviews_count,
        &current.signals.reviews_count,
    );
    push_if_changed(
        &mut fields,
        "last_comment_at",
        &previous.signals.last_comment_at,
        &current.signals.last_comment_at,
    );
    push_if_changed(
        &mut fields,
        "last_review_at",
        &previous.signals.last_review_at,
        &current.signals.last_review_at,
    );
    push_if_changed(
        &mut fields,
        "updated_at",
        &previous.updated_at,
        &current.updated_at,
    );
    fields
}

fn push_if_changed<T: PartialEq>(fields: &mut Vec<String>, field: &str, previous: &T, current: &T) {
    if previous != current {
        fields.push(field.to_string());
    }
}

fn push_if_changed_unless_unknown(
    fields: &mut Vec<String>,
    field: &str,
    previous: &Option<String>,
    current: &Option<String>,
) {
    if previous == current
        || previous.as_deref() == Some("UNKNOWN")
        || current.as_deref() == Some("UNKNOWN")
    {
        return;
    }
    fields.push(field.to_string());
}

fn triage_observation_component_id(target: &TriageTarget) -> String {
    format!("{}:{}", target.kind_name(), target.id())
}

fn triage_observation_items(run_id: &str, output: &TriageOutput) -> Vec<NewTriageItemRecord> {
    let mut records = Vec::new();
    for component in &output.components {
        if let Some(issues) = &component.issues {
            for issue in &issues.items {
                records.push(NewTriageItemRecord {
                    run_id: run_id.to_string(),
                    provider: component.repo.provider.to_string(),
                    repo_owner: component.repo.owner.clone(),
                    repo_name: component.repo.name.clone(),
                    item_type: "issue".to_string(),
                    number: issue.number,
                    state: issue.state.clone(),
                    title: issue.title.clone(),
                    url: issue.url.clone(),
                    signals: TriagePullRequestSignals {
                        comments_count: issue.comments_count.and_then(usize_to_i64),
                        last_comment_at: issue.last_comment_at.clone(),
                        next_action: if issue.stale {
                            Some("stale_issue".to_string())
                        } else {
                            None
                        },
                        ..TriagePullRequestSignals::default()
                    },
                    updated_at: issue.updated_at.clone(),
                    metadata_json: serde_json::json!({
                        "component_id": component.component_id,
                        "labels": issue.labels,
                        "assignees": issue.assignees,
                        "linked_prs": issue.linked_prs,
                    }),
                });
            }
        }
        if let Some(prs) = &component.pull_requests {
            for pr in &prs.items {
                records.push(NewTriageItemRecord {
                    run_id: run_id.to_string(),
                    provider: component.repo.provider.to_string(),
                    repo_owner: component.repo.owner.clone(),
                    repo_name: component.repo.name.clone(),
                    item_type: "pull_request".to_string(),
                    number: pr.number,
                    state: pr.state.clone(),
                    title: pr.title.clone(),
                    url: pr.url.clone(),
                    signals: pr.signals.clone(),
                    updated_at: pr.updated_at.clone(),
                    metadata_json: serde_json::json!({
                        "component_id": component.component_id,
                        "draft": pr.draft,
                        "labels": pr.labels,
                        "assignees": pr.assignees,
                        "author": pr.author,
                        "check_failures": pr.check_failures,
                    }),
                });
            }
        }
    }
    records
}

pub(super) fn usize_to_i64(value: usize) -> Option<i64> {
    i64::try_from(value).ok()
}
