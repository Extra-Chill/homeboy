//! `homeboy agent-task capacity` (#15024).
//!
//! One command that reports the live capacity of every connected provider plan
//! in the configured rotation: each account, its state, remaining capacity, and
//! reset instant. It walks `agent_task.default_backend` plus every
//! `agent_task.rotation` entry, probes each route's declared readiness
//! invocation in capacity-only mode (`mode: "capacity"` — the runtime opt-in
//! that reads usage/profile endpoints without spending its bounded inference
//! probe), groups routes that report the same account-pool `scope`, and returns
//! a stable `homeboy/agent-task-capacity/v1` envelope Homeboy Desktop can
//! render as a control-plane panel.
//!
//! The command never runs provider inference. A runtime that ignores the
//! capacity mode could still run inference on its own, so this command reports
//! only what each capacity result contains — never a dispatchability verdict —
//! and a runtime that publishes no capacity is reported as `unknown` with its
//! diagnostic rather than inferred from any probe success.

use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use serde_json::{json, Value};

use homeboy::agents::agent_tasks::dispatch_service as agent_task_dispatch_service;
use homeboy::agents::agent_tasks::provider::{
    evaluate_provider_capacity_with_config, AgentTaskProviderCapacityReadiness,
    AgentTaskProviderCatalog, ProviderRuntimeReadinessCache,
};

use super::super::CmdResult;
use super::AgentTaskCapacityArgs;

/// Bounded per-route probe budget. Each readiness invocation carries its own
/// `timeout_ms`; this deadline additionally bounds the whole per-route probe
/// (including the runtime readiness layer's bounded transient retry) so one
/// stuck route cannot hold the whole rotation report hostage.
const CAPACITY_ROUTE_TIMEOUT_MS: u64 = 30_000;

/// Upper bound on walked routes so a pathological rotation config cannot fork
/// an unbounded probe fan-out.
const MAX_CAPACITY_ROUTES: usize = 64;

pub fn capacity(args: AgentTaskCapacityArgs) -> CmdResult<Value> {
    capacity_with_catalog(args, AgentTaskProviderCatalog::discover())
}

pub(crate) fn capacity_with_catalog(
    args: AgentTaskCapacityArgs,
    catalog: AgentTaskProviderCatalog,
) -> CmdResult<Value> {
    let routes = rotation_capacity_routes(&args);
    let catalog = std::sync::Arc::new(catalog);
    let cache = ProviderRuntimeReadinessCache::default();
    let provider_config = super::review::effective_provider_catalog_config()?;
    let deadline_unix_ms = bounded_route_deadline_unix_ms();
    let outcomes = routes
        .into_iter()
        .map(|route| {
            let catalog = std::sync::Arc::clone(&catalog);
            let mut cache = cache.clone();
            let provider_config = provider_config.clone();
            std::thread::Builder::new()
                .name("agent-task-capacity-route".to_string())
                .spawn(move || {
                    let capacity = evaluate_provider_capacity_with_config(
                        &catalog,
                        &route.backend,
                        route.selector.as_deref(),
                        route.model.as_deref(),
                        &merge_route_provider_config(&provider_config, &route.provider_config),
                        &mut cache,
                        Some(deadline_unix_ms),
                    );
                    CapacityRouteOutcome {
                        backend: route.backend,
                        selector: route.selector,
                        model: route.model,
                        capacity,
                    }
                })
                .map(|handle| {
                    handle.join().unwrap_or_else(|_| CapacityRouteOutcome {
                        backend: String::new(),
                        selector: None,
                        model: None,
                        capacity: AgentTaskProviderCapacityReadiness::Unknown {
                            reason: "the capacity probe panicked before it could report"
                                .to_string(),
                        },
                    })
                })
                .unwrap_or_else(|_| CapacityRouteOutcome {
                    backend: String::new(),
                    selector: None,
                    model: None,
                    capacity: AgentTaskProviderCapacityReadiness::Unknown {
                        reason: "the capacity probe could not be started".to_string(),
                    },
                })
        })
        .collect::<Vec<_>>();

    let groups = group_outcomes_by_scope(outcomes);
    let next_reset = groups
        .iter()
        .filter_map(|group| next_reset_for_capacity(&route_group_capacity(group)))
        .min()
        .map(|reset| reset.to_rfc3339());
    let route_values = groups
        .iter()
        .map(|group| route_group_value(group))
        .collect::<Vec<_>>();

    Ok((
        json!({
            "schema": "homeboy/agent-task-capacity/v1",
            "generated_at": chrono::Utc::now().to_rfc3339(),
            "routes": route_values,
            "next_reset": next_reset,
        }),
        0,
    ))
}

/// One walked rotation route: backend/selector/model identity plus the
/// entry-scoped provider config overrides, deduplicated by identity.
struct CapacityRoute {
    backend: String,
    selector: Option<String>,
    model: Option<String>,
    provider_config: Value,
}

/// One probed route's capacity outcome.
struct CapacityRouteOutcome {
    backend: String,
    selector: Option<String>,
    model: Option<String>,
    capacity: AgentTaskProviderCapacityReadiness,
}

/// Walk `agent_task.default_backend` plus every `agent_task.rotation` entry,
/// deduplicated by (backend, selector, model) with first-seen order kept, then
/// apply the `--backend`/`--model`/`--selector` filters. A rotation entry
/// without a `backend` inherits the configured default, mirroring how the
/// dispatch policy resolves its initial route; an entry with neither an own
/// nor an inheritable backend is not a route.
fn rotation_capacity_routes(args: &AgentTaskCapacityArgs) -> Vec<CapacityRoute> {
    let config = homeboy::core::defaults::load_config();
    let default_backend = config.agent_task.default_backend.clone();
    let mut routes = Vec::new();
    if let Some(backend) = default_backend.clone() {
        routes.push(CapacityRoute {
            backend,
            selector: None,
            model: None,
            provider_config: Value::Null,
        });
    }
    if let Some(rotation) = agent_task_dispatch_service::configured_rotation_policy() {
        for entry in rotation.entries {
            let Some(backend) = entry.backend.or_else(|| default_backend.clone()) else {
                continue;
            };
            routes.push(CapacityRoute {
                backend,
                selector: entry.selector,
                model: entry.model,
                provider_config: entry.provider_config,
            });
        }
    }
    let mut deduped = Vec::new();
    for route in routes {
        let duplicate = deduped.iter().any(|seen: &CapacityRoute| {
            seen.backend == route.backend
                && seen.selector == route.selector
                && seen.model == route.model
        });
        if duplicate {
            continue;
        }
        deduped.push(route);
        if deduped.len() >= MAX_CAPACITY_ROUTES {
            break;
        }
    }
    // The bare default backend only stands in for "whatever that backend
    // selects"; once rotation names explicit models on it, it adds no pool.
    let modeled_backends = deduped
        .iter()
        .filter(|route| route.model.is_some())
        .map(|route| route.backend.clone())
        .collect::<Vec<_>>();
    deduped.retain(|route| {
        route.model.is_some()
            || route.selector.is_some()
            || !modeled_backends.contains(&route.backend)
    });
    deduped
        .into_iter()
        .filter(|route| {
            args.backend
                .as_deref()
                .is_none_or(|backend| route.backend == backend)
                && args
                    .selector
                    .as_deref()
                    .is_none_or(|selector| route.selector.as_deref() == Some(selector))
                && args
                    .model
                    .as_deref()
                    .is_none_or(|model| route.model.as_deref() == Some(model))
        })
        .collect()
}

/// Shallow-merge one rotation entry's `provider_config` overrides over the
/// materialized base provider config, the same merge the rotation scheduler
/// applies to an executor's config before dispatch.
fn merge_route_provider_config(base: &Value, overrides: &Value) -> Value {
    let Some(overrides) = overrides.as_object() else {
        return base.clone();
    };
    let mut merged = base.as_object().cloned().unwrap_or_default();
    merged.extend(overrides.clone());
    Value::Object(merged)
}

fn bounded_route_deadline_unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|value| value.as_millis() as u64)
        .unwrap_or_default()
        .saturating_add(CAPACITY_ROUTE_TIMEOUT_MS)
}

/// Group routes that report the same non-empty `scope` into one pool entry;
/// routes without a scope stay individual. First-seen order is preserved.
fn group_outcomes_by_scope(outcomes: Vec<CapacityRouteOutcome>) -> Vec<Vec<CapacityRouteOutcome>> {
    let mut groups: Vec<(Option<String>, Vec<CapacityRouteOutcome>)> = Vec::new();
    for outcome in outcomes {
        let scope = outcome
            .capacity
            .scope()
            .map(|scope| scope.to_string())
            .filter(|scope| !scope.trim().is_empty());
        match groups
            .iter_mut()
            .find(|(group_scope, _)| group_scope.is_some() && *group_scope == scope)
        {
            Some((_, members)) => members.push(outcome),
            None => groups.push((scope, vec![outcome])),
        }
    }
    groups.into_iter().map(|(_, members)| members).collect()
}

/// The capacity a route group publishes: the first probe that published
/// anything, so one failed lookup inside a shared pool cannot blank out data a
/// sibling route observed. A group where every probe came back unknown keeps
/// the first route's unknown reason.
fn route_group_capacity(group: &[CapacityRouteOutcome]) -> AgentTaskProviderCapacityReadiness {
    let first = group
        .first()
        .expect("a scope group always has at least one route");
    group
        .iter()
        .find(|member| {
            !matches!(
                member.capacity,
                AgentTaskProviderCapacityReadiness::Unknown { .. }
            )
        })
        .unwrap_or(first)
        .capacity
        .clone()
}

fn route_group_value(group: &[CapacityRouteOutcome]) -> Value {
    let first = group
        .first()
        .expect("a scope group always has at least one route");
    let mut models = Vec::new();
    for member in group {
        if let Some(model) = member.model.clone() {
            if !models.contains(&model) {
                models.push(model);
            }
        }
    }
    json!({
        "backend": first.backend,
        "selector": first.selector,
        "models": models,
        "scope": first.capacity.scope(),
        "capacity": serde_json::to_value(route_group_capacity(group)).unwrap_or(Value::Null),
    })
}

/// The soonest reset among this capacity's exhausted accounts, when any is
/// known. This is what makes "which plan frees up first" a single field.
fn next_reset_for_capacity(capacity: &AgentTaskProviderCapacityReadiness) -> Option<DateTime<Utc>> {
    let mut resets = Vec::new();
    if capacity.is_exhausted() {
        resets.extend(capacity.reset_at());
    }
    for account in capacity.accounts() {
        if account.state.trim().eq_ignore_ascii_case("exhausted") {
            if let Some(reset_at) = account.reset_at.as_deref() {
                if let Ok(reset) = DateTime::parse_from_rfc3339(reset_at) {
                    resets.push(reset.with_timezone(&Utc));
                }
            }
        }
    }
    resets.into_iter().min()
}

#[cfg(test)]
mod agent_task_capacity;
