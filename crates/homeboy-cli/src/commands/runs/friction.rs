//! Friction projection over terminal invocation rows and canonical Cook records.
//! Version-cohort notifications remain owned by hotspots; a future friction
//! comparator should feed that outbox path rather than implement another notifier.
use super::{CmdResult, RunsOutput};
use clap::Args;
use homeboy::core::observation::store::Invocation;
use homeboy::core::observation::{ObservationStore, RunListFilter};
use serde_json::{json, Value};
use std::collections::BTreeMap;

#[derive(Args, Debug, Clone)]
pub struct RunsFrictionArgs {
    /// Recent invocations (N, capped at 1000), or a duration such as 7d.
    #[arg(long, default_value = "7d")]
    pub last: String,
    /// Restrict to invocations against Cooks belonging to this component.
    #[arg(long)]
    pub component: Option<String>,
}

fn ranked(counts: BTreeMap<String, usize>) -> Vec<Value> {
    let mut rows: Vec<_> = counts.into_iter().collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    rows.into_iter()
        .map(|(key, count)| json!({"key": key, "count": count}))
        .collect()
}

pub(crate) fn friction(store: &ObservationStore, args: RunsFrictionArgs) -> CmdResult<RunsOutput> {
    let (limit, cutoff) = if let Ok(n) = args.last.parse::<usize>() {
        if n == 0 {
            return Err(homeboy::core::Error::validation_invalid_argument(
                "last",
                "last must be positive",
                None,
                None,
            ));
        }
        (n.min(1000), None)
    } else {
        let duration =
            homeboy::core::duration::parse_duration_parts(&args.last).map_err(|message| {
                homeboy::core::Error::validation_invalid_argument("last", message, None, None)
            })?;
        (
            1000,
            Some(
                chrono::Utc::now()
                    .checked_sub_signed(chrono::Duration::from_std(duration).map_err(|_| {
                        homeboy::core::Error::validation_invalid_argument(
                            "last",
                            "duration too large",
                            None,
                            None,
                        )
                    })?)
                    .ok_or_else(|| {
                        homeboy::core::Error::validation_invalid_argument(
                            "last",
                            "duration too large",
                            None,
                            None,
                        )
                    })?,
            ),
        )
    };
    let page = store.list_runs_page(RunListFilter {
        kind: Some("cli-invocation".into()),
        limit: Some(limit as i64),
        ..Default::default()
    })?;
    let mut invocations = Vec::new();
    let mut refusals = BTreeMap::new();
    let mut commands = BTreeMap::new();
    let mut retries = BTreeMap::new();
    let mut previous = BTreeMap::new();
    let mut cooks: BTreeMap<String, Value> = BTreeMap::new();
    let mut cook_aliases = BTreeMap::new();
    // Reuse the same canonical-run projection as hotspots, not runner mirrors.
    let runs = homeboy::core::observation::runs_service::dedupe_runner_execution_mirrors(
        store.list_runs(RunListFilter {
            kind: Some("agent-task".into()),
            component_id: args.component.clone(),
            limit: Some(10000),
            ..Default::default()
        })?,
    )
    .canonical;
    for run in &runs {
        let cook_id = run
            .metadata_json
            .pointer("/agent_task_run/metadata/cook_id")
            .and_then(Value::as_str)
            .unwrap_or(&run.id);
        cook_aliases.insert(run.id.clone(), cook_id.to_string());
        cook_aliases.insert(cook_id.to_string(), cook_id.to_string());
        if let Some(cook) = cooks.get_mut(cook_id) {
            if run.started_at.as_str() < cook["submitted_at"].as_str().unwrap_or_default() {
                cook["submitted_at"] = json!(run.started_at);
            }
            continue;
        }
        let delivery = run
            .metadata_json
            .pointer("/agent_task_run/metadata/cook_delivery/delivery");
        let stranded = delivery.is_some_and(|d| {
            matches!(d["state"].as_str(), Some("verified" | "applied"))
                && d.pointer("/publication/published").and_then(Value::as_bool) != Some(true)
                && d["gates"].as_array().is_some_and(|g| {
                    !g.is_empty()
                        && g.iter().all(|g| {
                            matches!(
                                g["state"].as_str(),
                                Some("succeeded" | "accepted_inherited_failure")
                            )
                        })
                })
        });
        let submission_to_pr_ms = if delivery
            .and_then(|d| d.pointer("/publication/published"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            let spans = store.phase_spans(&run.id)?;
            spans
                .span("publish")
                .filter(|span| span.is_ok())
                .and_then(|span| {
                    let submitted = chrono::DateTime::parse_from_rfc3339(&run.started_at).ok()?;
                    let published = chrono::DateTime::parse_from_rfc3339(&span.started_at).ok()?;
                    Some(
                        (published - submitted).num_milliseconds().max(0) as f64
                            + span.duration_ms.unwrap_or(0.0),
                    )
                })
        } else {
            None
        };
        cooks.insert(cook_id.to_string(), json!({"cook_id": cook_id, "run_id": run.id, "submitted_at": run.started_at, "manual_intervention_count": 0, "recover_count": 0, "retry_count": 0, "refusals": {}, "gate_green_unpublished": stranded, "submission_to_pr_ms": submission_to_pr_ms}));
    }
    for run in page.runs.iter().rev() {
        if cutoff.is_some_and(|cutoff| {
            chrono::DateTime::parse_from_rfc3339(&run.started_at).map_or(true, |time| time < cutoff)
        }) {
            continue;
        }
        let Ok(row) = serde_json::from_value::<Invocation>(run.metadata_json.clone()) else {
            continue;
        };
        if args.component.is_some()
            && !row
                .run_id
                .as_ref()
                .is_some_and(|id| cook_aliases.contains_key(id))
        {
            continue;
        }
        *commands.entry(row.command_path.clone()).or_default() += 1;
        if let Some(code) = &row.error_code {
            *refusals.entry(code.clone()).or_default() += 1;
        }
        // Adjacency is scoped by caller, including interleaved agent sessions.
        let key = row.caller_session.clone();
        let identity = (row.command_path.clone(), row.run_id.clone());
        if previous.insert(key, identity.clone()).as_ref() == Some(&identity) {
            *retries.entry(row.command_path.clone()).or_default() += 1;
        }
        if let Some(cook) = row
            .run_id
            .as_ref()
            .and_then(|id| cook_aliases.get(id))
            .and_then(|id| cooks.get_mut(id))
        {
            if run.started_at
                >= cook["submitted_at"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            {
                let n = cook["manual_intervention_count"].as_u64().unwrap_or(0);
                cook["manual_intervention_count"] = json!(n + 1);
                for verb in ["recover", "retry"] {
                    if row.command_path.split_whitespace().any(|part| part == verb) {
                        let field = format!("{verb}_count");
                        cook[&field] = json!(cook[&field].as_u64().unwrap_or(0) + 1);
                    }
                }
                if let Some(code) = &row.error_code {
                    cook["refusals"][code] =
                        json!(cook["refusals"][code].as_u64().unwrap_or(0) + 1);
                }
            }
        }
        invocations.push(json!({"started_at": run.started_at, "homeboy_version": run.homeboy_version, "invocation": row}));
    }
    let mut cooks: Vec<_> = cooks.into_values().collect();
    cooks.sort_by_key(|c| std::cmp::Reverse(c["manual_intervention_count"].as_u64().unwrap_or(0)));
    let stranded: Vec<_> = cooks
        .iter()
        .filter(|c| c["gate_green_unpublished"] == true)
        .cloned()
        .collect();
    Ok((
        RunsOutput::Friction(
            json!({"command": "runs.friction", "invocation_count": invocations.len(), "truncated": page.truncated, "invocations": invocations, "top_refusal_codes": ranked(refusals), "commands": ranked(commands), "immediate_reruns": ranked(retries), "highest_intervention_cooks": cooks, "stranded_green_cooks": stranded}),
        ),
        0,
    ))
}
