//! Activity collector — dedupes and reconciles activity items from multiple
//! sources into canonical work records, applying source precedence, ref
//! merging, evidence/action de-duplication, and final sort/conflict
//! projection. Extracted from the `activity` module (#9794).

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};

use super::{
    action_helpers::parse_ts, is_active, ActivityEvidenceRef, ActivityItem, ActivityNextAction,
    ActivityScope, ActivitySourceProjection, ActivityStateConflict,
};

#[derive(Default)]
pub(crate) struct ActivityCollector {
    items: BTreeMap<String, ActivityItem>,
}

impl ActivityCollector {
    pub(crate) fn insert(&mut self, mut item: ActivityItem) {
        let projection = source_projection(&item);
        append_source_projection(&mut item.source_projections, projection);
        let key = canonical_identity(&item);
        self.items
            .entry(key)
            .and_modify(|existing| merge_item(existing, &item))
            .or_insert(item);
    }

    pub(crate) fn items(self, scope: ActivityScope, limit: usize) -> Vec<ActivityItem> {
        let mut items = self.items.into_values().collect::<Vec<_>>();
        for item in &mut items {
            finalize_item(item);
        }
        items.sort_by(|left, right| item_sort_key(right).cmp(&item_sort_key(left)));
        if scope == ActivityScope::ActiveRecent {
            items.retain(|item| is_active(item.state) || item.finished_at.is_some());
        }
        items.truncate(limit.max(1));
        items
    }
}

fn canonical_identity(item: &ActivityItem) -> String {
    // Lifecycle records own agent-task state. Their durable id is also the
    // observation run id, so normalize both projections onto that one identity.
    if item.source_store == "agent-task.lifecycle" {
        return format!("run:{}", item.id);
    }
    if let Some(agent_task_run_id) = &item.refs.agent_task_run_id {
        return format!("run:{agent_task_run_id}");
    }
    if let Some(run_id) = &item.refs.run_id {
        return format!("run:{run_id}");
    }
    if let Some(job_id) = &item.refs.runner_job_id {
        return format!("runner-job:{job_id}");
    }
    format!("item:{}", item.id)
}

fn merge_item(existing: &mut ActivityItem, incoming: &ActivityItem) {
    if source_precedence(incoming) > source_precedence(existing)
        || (source_precedence(incoming) == source_precedence(existing)
            && incoming.updated_at > existing.updated_at)
    {
        let mut replacement = incoming.clone();
        merge_refs(&mut replacement, existing);
        append_unique(&mut replacement.artifacts, &existing.artifacts);
        append_unique(&mut replacement.evidence, &existing.evidence);
        append_actions(&mut replacement.next_actions, &existing.next_actions);
        append_source_projections(
            &mut replacement.source_projections,
            &existing.source_projections,
        );
        *existing = replacement;
        return;
    }

    merge_refs(existing, incoming);
    append_unique(&mut existing.artifacts, &incoming.artifacts);
    append_unique(&mut existing.evidence, &incoming.evidence);
    append_actions(&mut existing.next_actions, &incoming.next_actions);
    append_source_projections(
        &mut existing.source_projections,
        &incoming.source_projections,
    );
}

fn source_precedence(item: &ActivityItem) -> u8 {
    source_store_precedence(&item.source_store)
}

fn source_store_precedence(source_store: &str) -> u8 {
    match source_store {
        "agent-task.lifecycle" => 4,
        "runner.session" => 3,
        "daemon.jobs-json" => 2,
        "observation.sqlite" => 1,
        _ => 0,
    }
}

fn finalize_item(item: &mut ActivityItem) {
    item.artifacts.sort_by(|left, right| {
        left.uri
            .cmp(&right.uri)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.id.cmp(&right.id))
    });
    item.evidence.sort_by(|left, right| {
        left.uri
            .cmp(&right.uri)
            .then_with(|| left.kind.cmp(&right.kind))
            .then_with(|| left.id.cmp(&right.id))
    });
    item.next_actions.sort_by(|left, right| {
        left.command
            .cmp(&right.command)
            .then_with(|| left.label.cmp(&right.label))
    });
    item.source_projections.sort_by(|left, right| {
        source_store_precedence(&right.source_store)
            .cmp(&source_store_precedence(&left.source_store))
            .then_with(|| right.updated_at.cmp(&left.updated_at))
            .then_with(|| left.source_store.cmp(&right.source_store))
            .then_with(|| left.id.cmp(&right.id))
    });
    item.state_conflicts = item
        .source_projections
        .iter()
        .filter(|projection| projection.state != item.state)
        .map(|projection| ActivityStateConflict {
            source_store: projection.source_store.clone(),
            id: projection.id.clone(),
            state: projection.state,
        })
        .collect();
}

fn merge_refs(existing: &mut ActivityItem, incoming: &ActivityItem) {
    if existing.refs.run_id.is_none() {
        existing.refs.run_id = incoming.refs.run_id.clone();
    }
    if existing.refs.agent_task_run_id.is_none() {
        existing.refs.agent_task_run_id = incoming.refs.agent_task_run_id.clone();
    }
    if existing.refs.runner_job_id.is_none() {
        existing.refs.runner_job_id = incoming.refs.runner_job_id.clone();
    }
    if existing.runner.runner_id.is_none() {
        existing.runner.runner_id = incoming.runner.runner_id.clone();
    }
    if existing.runner.job_id.is_none() {
        existing.runner.job_id = incoming.runner.job_id.clone();
    }
    if existing.runner.transport.is_none() {
        existing.runner.transport = incoming.runner.transport.clone();
    }
}

fn source_projection(item: &ActivityItem) -> ActivitySourceProjection {
    ActivitySourceProjection {
        source_store: item.source_store.clone(),
        id: item.id.clone(),
        state: item.state,
        updated_at: item.updated_at.clone(),
        finished_at: item.finished_at.clone(),
    }
}

fn append_source_projection(
    target: &mut Vec<ActivitySourceProjection>,
    incoming: ActivitySourceProjection,
) {
    if !target.iter().any(|projection| {
        projection.source_store == incoming.source_store && projection.id == incoming.id
    }) {
        target.push(incoming);
    }
}

fn append_source_projections(
    target: &mut Vec<ActivitySourceProjection>,
    incoming: &[ActivitySourceProjection],
) {
    for projection in incoming {
        append_source_projection(target, projection.clone());
    }
}

fn append_unique(target: &mut Vec<ActivityEvidenceRef>, incoming: &[ActivityEvidenceRef]) {
    for item in incoming {
        if !target.iter().any(|existing| existing.uri == item.uri) {
            target.push(item.clone());
        }
    }
}

fn append_actions(target: &mut Vec<ActivityNextAction>, incoming: &[ActivityNextAction]) {
    for action in incoming {
        if !target
            .iter()
            .any(|existing| existing.command == action.command)
        {
            target.push(action.clone());
        }
    }
}

fn item_sort_key(item: &ActivityItem) -> (bool, Option<DateTime<Utc>>, String) {
    (
        is_active(item.state),
        item.updated_at
            .as_deref()
            .or(Some(item.created_at.as_str()))
            .and_then(parse_ts),
        item.id.clone(),
    )
}
