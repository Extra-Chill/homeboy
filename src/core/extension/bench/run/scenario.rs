//! Scenario discovery, filtering, and id normalization.

use std::path::PathBuf;

use crate::core::component::Component;
use crate::core::engine::run_dir::{self, RunDir};
use crate::core::error::{Error, Result};
use crate::core::extension::bench::parsing::{self, BenchResults};
use crate::core::extension::ExtensionExecutionContext;

use super::runner::build_runner;
use super::types::BenchRunWorkflowArgs;

pub(crate) fn normalize_workload_json_scenario_ids(results: &mut BenchResults) {
    for scenario in &mut results.scenarios {
        let Some(file) = scenario.file.as_deref() else {
            continue;
        };
        let path = std::path::Path::new(file);
        let Some(file_stem) = path.file_stem().map(|stem| stem.to_string_lossy()) else {
            continue;
        };
        if !file_stem.contains(".workload") || scenario.id != file_stem {
            continue;
        }

        scenario.id = scenario_id_for_workload_path(path);
    }
}

pub(crate) fn apply_scenario_filter(
    mut results: BenchResults,
    scenario_ids: &[String],
) -> Result<BenchResults> {
    if scenario_ids.is_empty() {
        return Ok(results);
    }

    let discovered: Vec<String> = results.scenarios.iter().map(|s| s.id.clone()).collect();
    let missing: Vec<String> = scenario_ids
        .iter()
        .filter(|id| !discovered.contains(id))
        .cloned()
        .collect();

    if !missing.is_empty() {
        return Err(Error::validation_invalid_argument(
            "scenario",
            format!(
                "unknown bench scenario id(s): {}; discovered ids: {}",
                missing.join(", "),
                if discovered.is_empty() {
                    "<none>".to_string()
                } else {
                    discovered.join(", ")
                }
            ),
            Some(missing.join(", ")),
            Some(discovered),
        ));
    }

    results
        .scenarios
        .retain(|scenario| scenario_ids.contains(&scenario.id));
    Ok(results)
}

pub(crate) fn scenario_id_for_workload_path(path: &std::path::Path) -> String {
    let basename = path
        .file_name()
        .map(|name| name.to_string_lossy())
        .unwrap_or_default();
    let name = basename
        .split_once(".bench.")
        .map(|(stem, _)| stem)
        .or_else(|| basename.split_once(".workload.").map(|(stem, _)| stem))
        .unwrap_or_else(|| {
            basename
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(&basename)
        });

    let mut slug = String::new();
    let mut prev_was_separator = true;
    let mut prev_was_lower_or_digit = false;
    for ch in name.chars() {
        if ch.is_ascii_uppercase() && prev_was_lower_or_digit && !prev_was_separator {
            slug.push('-');
        }

        if ch.is_ascii_alphanumeric() {
            slug.push(ch.to_ascii_lowercase());
            prev_was_separator = false;
            prev_was_lower_or_digit = ch.is_ascii_lowercase() || ch.is_ascii_digit();
        } else if !prev_was_separator {
            slug.push('-');
            prev_was_separator = true;
            prev_was_lower_or_digit = false;
        } else {
            prev_was_lower_or_digit = false;
        }
    }

    slug.trim_matches('-').to_string()
}

pub(crate) fn filter_extra_workloads_by_scenario_ids(
    workloads: &[PathBuf],
    scenario_ids: &[String],
) -> Vec<PathBuf> {
    if scenario_ids.is_empty() {
        return workloads.to_vec();
    }

    workloads
        .iter()
        .filter(|path| scenario_ids.contains(&scenario_id_for_workload_path(path)))
        .cloned()
        .collect()
}

pub(crate) fn failure_scenario_id(scenario_ids: &[String]) -> Option<String> {
    if scenario_ids.len() == 1 {
        Some(scenario_ids[0].clone())
    } else {
        None
    }
}

pub(crate) fn discover_bench_scenarios(
    execution_context: &ExtensionExecutionContext,
    component: &Component,
    args: &BenchRunWorkflowArgs,
    run_dir: &RunDir,
) -> Result<BenchResults> {
    let results_file = run_dir.step_file(run_dir::files::BENCH_RESULTS);
    if results_file.exists() {
        std::fs::remove_file(&results_file).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to clear previous bench discovery results file {}: {}",
                    results_file.display(),
                    e
                ),
                Some("bench.discovery.results_file".to_string()),
            )
        })?;
    }

    let runner_output = build_runner(execution_context, component, args, run_dir, None)?
        .env("HOMEBOY_BENCH_LIST_ONLY", "1")
        .run()?;

    if !runner_output.success {
        return Err(Error::validation_invalid_argument(
            "scenario",
            format!(
                "bench scenario discovery failed with exit code {}",
                runner_output.exit_code
            ),
            Some(format!(
                "stdout:\n{}\n\nstderr:\n{}",
                runner_output.stdout, runner_output.stderr
            )),
            None,
        ));
    }

    parsing::parse_bench_results_file_with_artifact_context(&results_file, None)
}
