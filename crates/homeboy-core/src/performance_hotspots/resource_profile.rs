//! Where CPU goes: per-step and per-phase usage recorded on durable phase
//! spans (#18461), for one run (`runs show`) or across runs (`runs profile`).
use std::collections::BTreeMap;

use super::{usage_buckets, CookUsage};

use serde::Serialize;

use crate::observation::{runs_service, ObservationStore, RunListFilter};
use crate::Result;
use homeboy_engine_primitives::phase_timing::{PhaseSpan, PhaseStatus};

/// Most rows one report returns.
const MAX_ROWS: usize = 200;
/// Most runs one profile reads.
const MAX_RUNS: i64 = 1000;

/// One step or phase name's usage within a run.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseUsageRow {
    pub phase: String,
    pub instances: usize,
    pub cpu_seconds: f64,
    pub cpu_user_seconds: f64,
    pub cpu_system_seconds: f64,
    /// Homeboy's own share (daemon, controller) of `cpu_seconds`.
    #[serde(skip_serializing_if = "is_zero")]
    pub homeboy_cpu_seconds: f64,
    pub wall_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory_peak_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_read_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_write_bytes: Option<u64>,
    /// Some usage came from process totals, not the job's cgroup.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub estimated: bool,
}

/// CPU split between Homeboy's own work and the workload it ran.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct CpuOverhead {
    /// The daemon and controller threads' own CPU.
    pub homeboy_cpu_seconds: f64,
    /// Workload CPU of staging phases (`stage*`, `*staging*`).
    pub staging_cpu_seconds: f64,
    pub workload_cpu_seconds: f64,
}

/// `runs show`: usage per step or phase, sorted by CPU.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunResources {
    pub cpu_seconds: f64,
    pub cooks: Vec<CookUsage>,
    pub overhead: CpuOverhead,
    pub phases: Vec<PhaseUsageRow>,
    #[serde(skip_serializing_if = "is_zero_count")]
    pub omitted_phases: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsagePercentiles {
    pub p50: f64,
    pub p95: f64,
}

/// `runs profile`: one step or phase name across runs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct PhaseProfileRow {
    pub phase: String,
    pub instances: usize,
    pub cpu_seconds_total: f64,
    pub cpu_seconds: UsagePercentiles,
    pub wall_ms: UsagePercentiles,
    #[serde(skip_serializing_if = "is_zero_count")]
    pub estimated_instances: usize,
}

#[derive(Debug, Clone, Default)]
pub struct ResourceProfileQuery {
    /// RFC-3339 lower bound on run start.
    pub since: Option<String>,
    /// Workflow definition id: its instances are `wf-<id>-<hash>`.
    pub workflow: Option<String>,
    pub component: Option<String>,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceProfile {
    pub cooks: Vec<CookUsage>,
    pub runs_inspected: usize,
    pub runs_with_usage: usize,
    pub cpu_seconds: f64,
    pub overhead: CpuOverhead,
    pub phases: Vec<PhaseProfileRow>,
    #[serde(skip_serializing_if = "is_zero_count")]
    pub omitted_phases: usize,
}

fn is_zero(value: &f64) -> bool {
    *value == 0.0
}

fn is_zero_count(value: &usize) -> bool {
    *value == 0
}

fn seconds(usec: u64) -> f64 {
    usec as f64 / 1_000_000.0
}

/// The name a span aggregates under: a `foreach` item is its step.
fn phase_name(id: &str) -> &str {
    match id.rsplit_once(".item.") {
        Some((step, index)) if index.bytes().all(|byte| byte.is_ascii_digit()) => step,
        _ => id,
    }
}

fn is_staging(phase: &str) -> bool {
    phase.starts_with("stage") || phase.contains("staging")
}

fn measured(span: &PhaseSpan) -> Option<&homeboy_engine_primitives::resource_usage::ResourceUsage> {
    (span.status != PhaseStatus::Skipped)
        .then_some(span.usage.as_ref())
        .flatten()
}

fn add_overhead(overhead: &mut CpuOverhead, phase: &str, cpu: f64, homeboy: f64) {
    overhead.homeboy_cpu_seconds += homeboy;
    if is_staging(phase) {
        overhead.staging_cpu_seconds += cpu - homeboy;
    } else {
        overhead.workload_cpu_seconds += cpu - homeboy;
    }
}

/// Homeboy's own and staging CPU in the Cooks a workflow instance launched
/// (`<instance>-<step>-i…`). Their step's usage counts the Cook process as
/// workload; the Cook's own spans say which part of it was Homeboy (#18461).
fn launched_cook_overhead(store: &ObservationStore, instance: &str) -> Result<CpuOverhead> {
    let mut overhead = CpuOverhead::default();
    for run_id in store.run_ids_with_prefix(&format!("{instance}-"), MAX_RUNS)? {
        for span in store.phase_spans(&run_id)?.spans {
            if let Some(usage) = measured(&span) {
                add_overhead(
                    &mut overhead,
                    phase_name(&span.id),
                    seconds(usage.cpu_usage_usec),
                    seconds(usage.homeboy_cpu_usec),
                );
            }
        }
    }
    Ok(overhead)
}

/// Move the launched Cooks' Homeboy and staging CPU out of the instance's
/// workload, never past what the instance measured.
fn attribute_launched(overhead: &mut CpuOverhead, launched: &CpuOverhead) {
    for (share, total) in [
        (
            launched.homeboy_cpu_seconds,
            &mut overhead.homeboy_cpu_seconds,
        ),
        (
            launched.staging_cpu_seconds,
            &mut overhead.staging_cpu_seconds,
        ),
    ] {
        let moved = share.min(overhead.workload_cpu_seconds).max(0.0);
        overhead.workload_cpu_seconds -= moved;
        *total += moved;
    }
}

fn by_cpu_then_name(a: (f64, &str), b: (f64, &str)) -> std::cmp::Ordering {
    b.0.total_cmp(&a.0).then(a.1.cmp(b.1))
}

fn cook_usage(
    store: &ObservationStore,
    run_id: &str,
    spans: &[PhaseSpan],
) -> Result<Vec<CookUsage>> {
    if workflow_of(run_id).is_some() {
        let mut cooks = Vec::new();
        for id in store.run_ids_with_prefix(&format!("{run_id}-"), MAX_RUNS)? {
            let attempts = usage_buckets(&store.phase_spans(&id)?.spans);
            if !attempts.is_empty() {
                cooks.push(CookUsage {
                    run_id: id,
                    attempts,
                });
            }
        }
        Ok(cooks)
    } else {
        let attempts = usage_buckets(spans);
        Ok(if attempts.is_empty() {
            vec![]
        } else {
            vec![CookUsage {
                run_id: run_id.to_string(),
                attempts,
            }]
        })
    }
}

/// Usage recorded on one run's spans; `None` when nothing was measured.
pub fn run_resources(store: &ObservationStore, run_id: &str) -> Result<Option<RunResources>> {
    let mut rows: BTreeMap<String, PhaseUsageRow> = BTreeMap::new();
    let mut overhead = CpuOverhead::default();
    let spans = store.phase_spans(run_id)?.spans;
    let cooks = cook_usage(store, run_id, &spans)?;
    for span in &spans {
        let Some(usage) = measured(&span) else {
            continue;
        };
        let phase = phase_name(&span.id).to_string();
        let row = rows.entry(phase.clone()).or_insert_with(|| PhaseUsageRow {
            phase,
            instances: 0,
            cpu_seconds: 0.0,
            cpu_user_seconds: 0.0,
            cpu_system_seconds: 0.0,
            homeboy_cpu_seconds: 0.0,
            wall_ms: 0,
            memory_peak_bytes: None,
            io_read_bytes: None,
            io_write_bytes: None,
            estimated: false,
        });
        row.instances += 1;
        row.cpu_seconds += seconds(usage.cpu_usage_usec);
        row.cpu_user_seconds += seconds(usage.cpu_user_usec);
        row.cpu_system_seconds += seconds(usage.cpu_system_usec);
        row.homeboy_cpu_seconds += seconds(usage.homeboy_cpu_usec);
        row.wall_ms = row.wall_ms.saturating_add(usage.wall_ms);
        row.memory_peak_bytes = row.memory_peak_bytes.max(usage.memory_peak_bytes);
        for (total, value) in [
            (&mut row.io_read_bytes, usage.io_read_bytes),
            (&mut row.io_write_bytes, usage.io_write_bytes),
        ] {
            if let Some(value) = value {
                *total = Some(total.unwrap_or(0).saturating_add(value));
            }
        }
        row.estimated |= usage.estimated;
        add_overhead(
            &mut overhead,
            &row.phase,
            seconds(usage.cpu_usage_usec),
            seconds(usage.homeboy_cpu_usec),
        );
    }
    if rows.is_empty() && cooks.is_empty() {
        return Ok(None);
    }
    if workflow_of(run_id).is_some() {
        attribute_launched(&mut overhead, &launched_cook_overhead(store, run_id)?);
    }
    let mut phases = rows.into_values().collect::<Vec<_>>();
    phases.sort_by(|a, b| by_cpu_then_name((a.cpu_seconds, &a.phase), (b.cpu_seconds, &b.phase)));
    let omitted_phases = phases.len().saturating_sub(MAX_ROWS);
    phases.truncate(MAX_ROWS);
    Ok(Some(RunResources {
        cooks,
        cpu_seconds: phases.iter().map(|row| row.cpu_seconds).sum(),
        overhead,
        phases,
        omitted_phases,
    }))
}

/// `wf-<workflow>-<12 hex>` names its workflow definition.
fn workflow_of(run_id: &str) -> Option<&str> {
    let (workflow, hash) = run_id.strip_prefix("wf-")?.rsplit_once('-')?;
    (hash.len() == 12 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())).then_some(workflow)
}

fn percentiles(mut values: Vec<f64>) -> UsagePercentiles {
    values.sort_by(f64::total_cmp);
    // Nearest-rank, as `runs hotspots` reports.
    let rank = |fraction: f64| {
        values[((values.len() as f64 * fraction).ceil() as usize)
            .saturating_sub(1)
            .min(values.len() - 1)]
    };
    UsagePercentiles {
        p50: rank(0.50),
        p95: rank(0.95),
    }
}

#[derive(Default)]
struct Samples {
    cpu: Vec<f64>,
    wall: Vec<f64>,
    estimated: usize,
}

/// Aggregate recorded usage over at most 1000 recent matching runs.
pub fn resource_profile(
    store: &ObservationStore,
    query: &ResourceProfileQuery,
) -> Result<ResourceProfile> {
    let since = query
        .since
        .as_deref()
        .and_then(|since| chrono::DateTime::parse_from_rfc3339(since).ok());
    let runs = store.list_runs_best_effort(RunListFilter {
        kind: query.workflow.as_ref().map(|_| "workflow".to_string()),
        component_id: query.component.clone(),
        limit: Some(MAX_RUNS),
        ..Default::default()
    })?;
    let runs = runs_service::dedupe_runner_execution_mirrors(runs).canonical;
    let mut groups: BTreeMap<String, Samples> = BTreeMap::new();
    let mut cooks = BTreeMap::new();
    let mut overhead = CpuOverhead::default();
    let (mut runs_inspected, mut runs_with_usage) = (0, 0);
    for run in runs {
        if since.is_some_and(|since| {
            chrono::DateTime::parse_from_rfc3339(&run.started_at)
                .map_or(true, |started| started < since)
        }) {
            continue;
        }
        if query
            .workflow
            .as_deref()
            .is_some_and(|workflow| workflow_of(&run.id) != Some(workflow))
        {
            continue;
        }
        runs_inspected += 1;
        let spans = match store.phase_spans(&run.id) {
            Ok(report) => report.spans,
            Err(error) => {
                crate::log_status!("runs", "skipping malformed phase row {}: {}", run.id, error);
                continue;
            }
        };
        // Unfiltered, every child Cook is itself a listed run; only a workflow
        // filter (which lists workflow runs alone) needs its children expanded.
        let run_cooks = if query.workflow.is_some() || workflow_of(&run.id).is_none() {
            cook_usage(store, &run.id, &spans)?
        } else {
            Vec::new()
        };
        for cook in run_cooks {
            cooks.insert(cook.run_id.clone(), cook);
        }
        let mut has_usage = false;
        let mut run_overhead = CpuOverhead::default();
        for span in &spans {
            let Some(usage) = measured(span) else {
                continue;
            };
            has_usage = true;
            let phase = phase_name(&span.id);
            let cpu = seconds(usage.cpu_usage_usec);
            add_overhead(
                &mut run_overhead,
                phase,
                cpu,
                seconds(usage.homeboy_cpu_usec),
            );
            let group = groups.entry(phase.to_string()).or_default();
            group.cpu.push(cpu);
            group.wall.push(usage.wall_ms as f64);
            group.estimated += usize::from(usage.estimated);
        }
        // With `--workflow`, only instances are listed, so their launched
        // Cooks are counted once, here.
        if has_usage && query.workflow.is_some() {
            attribute_launched(&mut run_overhead, &launched_cook_overhead(store, &run.id)?);
        }
        overhead.homeboy_cpu_seconds += run_overhead.homeboy_cpu_seconds;
        overhead.staging_cpu_seconds += run_overhead.staging_cpu_seconds;
        overhead.workload_cpu_seconds += run_overhead.workload_cpu_seconds;
        runs_with_usage += usize::from(has_usage);
    }
    let mut phases = groups
        .into_iter()
        .map(|(phase, samples)| PhaseProfileRow {
            phase,
            instances: samples.cpu.len(),
            cpu_seconds_total: samples.cpu.iter().sum(),
            cpu_seconds: percentiles(samples.cpu),
            wall_ms: percentiles(samples.wall),
            estimated_instances: samples.estimated,
        })
        .collect::<Vec<_>>();
    phases.sort_by(|a, b| {
        by_cpu_then_name(
            (a.cpu_seconds_total, &a.phase),
            (b.cpu_seconds_total, &b.phase),
        )
    });
    let limit = query.limit.clamp(1, MAX_ROWS);
    let omitted_phases = phases.len().saturating_sub(limit);
    phases.truncate(limit);
    // The heaviest Cooks first, bounded like the phase rows.
    let mut cooks: Vec<CookUsage> = cooks.into_values().collect();
    let cook_cpu = |cook: &CookUsage| -> f64 {
        cook.attempts
            .iter()
            .flat_map(|attempt| &attempt.buckets)
            .map(|bucket| bucket.cpu_seconds)
            .sum()
    };
    cooks.sort_by(|a, b| by_cpu_then_name((cook_cpu(a), &a.run_id), (cook_cpu(b), &b.run_id)));
    cooks.truncate(limit);
    Ok(ResourceProfile {
        cooks,
        runs_inspected,
        runs_with_usage,
        cpu_seconds: overhead.homeboy_cpu_seconds
            + overhead.staging_cpu_seconds
            + overhead.workload_cpu_seconds,
        overhead,
        phases,
        omitted_phases,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_profile_names_workflows_and_folds_foreach_items() {
        assert_eq!(
            workflow_of("wf-import-loop-0123456789ab"),
            Some("import-loop")
        );
        assert_eq!(workflow_of("wf-import-loop"), None);
        assert_eq!(
            phase_name("workflow.step.cooks.item.3"),
            "workflow.step.cooks"
        );
        assert_eq!(phase_name("gate.item.x"), "gate.item.x");
        let p = percentiles(vec![4.0, 1.0, 3.0, 2.0]);
        assert_eq!((p.p50, p.p95), (2.0, 4.0));
    }
}
