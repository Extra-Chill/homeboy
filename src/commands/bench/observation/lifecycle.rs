use std::path::Path;

use homeboy::core::engine::run_dir::{self, RunDir};
use homeboy::core::extension::bench::BenchRunWorkflowResult;
use homeboy::core::git::short_head_revision_at;
use homeboy::core::observation::{merge_metadata, ActiveObservation, NewRunRecord, RunStatus};
use homeboy::core::rig::RigStateSnapshot;

use super::artifacts::{record_bench_observation_artifacts, record_if_exists, record_memory_timeline_artifacts};
use super::metadata::{bench_observation_command, bench_observation_finish_metadata, bench_observation_initial_metadata};
use super::status::write_status_file;
use crate::commands::bench::BenchRunArgs;

pub(in crate::commands::bench) struct BenchObservation(pub(super) ActiveObservation);
impl BenchObservation {
    pub(super) fn run_id(&self) -> &str {
        self.0.run_id()
    }
}

pub(in crate::commands::bench) struct BenchObservationSummary {
    pub run_id: String,
    pub component_id: String,
    pub rig_id: Option<String>,
    pub store_path: String,
}
pub(in crate::commands::bench) struct BenchObservationStart<'a> {
    pub component_id: &'a str,
    pub component_label: &'a str,
    pub source_path: &'a Path,
    pub args: &'a BenchRunArgs,
    pub selected_scenarios: &'a [String],
    pub rig_id: Option<&'a str>,
    pub rig_snapshot: Option<&'a RigStateSnapshot>,
    pub run_dir: &'a RunDir,
}
pub(in crate::commands::bench) fn start(start: BenchObservationStart<'_>) -> Option<BenchObservation> {
    let metadata = bench_observation_initial_metadata(
        start.component_label,
        start.args,
        start.selected_scenarios,
        start.rig_snapshot,
        start.run_dir,
    );
    let observation = ActiveObservation::start_best_effort(
        NewRunRecord::builder("bench")
            .component_id(start.component_id)
            .command(bench_observation_command(
                start.component_id,
                start.args,
                start.rig_id,
            ))
            .cwd_path(start.source_path)
            .current_homeboy_version()
            .git_sha(short_head_revision_at(start.source_path))
            .optional_rig_id(start.rig_id)
            .metadata(metadata.clone())
            .build(),
    )
    .map(BenchObservation);
    if let Some(observation) = &observation {
        write_status_file(
            start.args,
            start.run_dir,
            observation,
            "running",
            None,
            None,
        );
    }
    observation
}

pub(in crate::commands::bench) fn finish_success(
    observation: Option<BenchObservation>,
    workflow: &mut BenchRunWorkflowResult,
    run_dir: &RunDir,
) -> Option<BenchObservationSummary> {
    let observation = observation?;

    record_bench_observation_artifacts(&observation, workflow, run_dir);
    let metadata =
        bench_observation_finish_metadata(observation.0.initial_metadata().clone(), workflow);
    let status = if workflow.exit_code == 0 {
        RunStatus::Pass
    } else {
        RunStatus::Fail
    };
    let summary = BenchObservationSummary {
        run_id: observation.run_id().to_string(),
        component_id: observation.0.component_id().unwrap_or_default().to_string(),
        rig_id: observation.0.rig_id().map(str::to_string),
        store_path: observation.0.store_path(),
    };
    write_status_file(
        &observation.0.initial_metadata().clone(),
        run_dir,
        &observation,
        workflow.status.as_str(),
        Some(workflow),
        None,
    );
    observation.0.finish(status, Some(metadata));
    Some(summary)
}

/// Build the structured persisted-run pointer surfaced on the bench output
/// envelope. Carries the run id plus the canonical `homeboy runs show` /
/// `homeboy runs artifacts` follow-up commands so the compact summary and
/// downstream tools can locate the full evidence (#3257, #3260).
pub(in crate::commands::bench) fn persisted_run_pointer(
    summary: &BenchObservationSummary,
) -> homeboy::core::extension::bench::BenchPersistedRun {
    homeboy::core::extension::bench::BenchPersistedRun {
        run_id: summary.run_id.clone(),
        component_id: (!summary.component_id.is_empty()).then(|| summary.component_id.clone()),
        rig_id: summary.rig_id.clone(),
        show_command: format!("homeboy runs show {}", summary.run_id),
        artifacts_command: format!("homeboy runs artifacts {}", summary.run_id),
    }
}

pub(in crate::commands::bench) fn history_hints(summary: &BenchObservationSummary) -> Vec<String> {
    let mut list_command = format!(
        "homeboy runs list --kind bench --component {}",
        summary.component_id
    );
    if let Some(rig_id) = &summary.rig_id {
        list_command.push_str(&format!(" --rig {rig_id}"));
    }

    vec![
        format!("Persisted benchmark run ID: {}", summary.run_id),
        format!("View this run: homeboy runs show {}", summary.run_id),
        format!("List artifacts: homeboy runs artifacts {}", summary.run_id),
        format!(
            "Fetch an artifact: homeboy runs artifact get {} <artifact-name> -o <path>",
            summary.run_id
        ),
        format!("List related bench runs: {list_command}"),
        format!(
            "Query persisted bench metadata: homeboy runs query --component {} --since 24h",
            summary.component_id
        ),
        format!(
            "Observation store: {} (debug path; prefer `homeboy runs ...` commands above)",
            summary.store_path
        ),
    ]
}

pub(in crate::commands::bench) fn finish_error(
    observation: Option<BenchObservation>,
    error: &homeboy::core::Error,
    run_dir: &RunDir,
) {
    let Some(observation) = observation else {
        return;
    };

    record_if_exists(
        &observation,
        "bench_results",
        run_dir.step_file(run_dir::files::BENCH_RESULTS),
    );
    record_if_exists(
        &observation,
        "resource_summary",
        run_dir.step_file(run_dir::files::RESOURCE_SUMMARY),
    );
    record_memory_timeline_artifacts(&observation, run_dir);
    let metadata = merge_metadata(
        observation.0.initial_metadata().clone(),
        serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
        }),
    );
    write_status_file(
        &observation.0.initial_metadata().clone(),
        run_dir,
        &observation,
        "error",
        None,
        Some(error),
    );
    observation.0.finish_error(Some(metadata));
}
