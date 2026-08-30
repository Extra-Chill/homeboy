use std::path::PathBuf;

use homeboy::core::engine::run_dir::RunDir;
use homeboy_core::bench::{self};

use super::lifecycle::BenchObservation;

pub(super) fn record_bench_observation_artifacts(
    observation: &BenchObservation,
    workflow: &mut homeboy_core::bench::BenchRunWorkflowResult,
    run_dir: &RunDir,
) -> bool {
    bench::record_bench_observation_artifacts(&observation.0, workflow, run_dir)
}

pub(super) fn record_if_exists(observation: &BenchObservation, kind: &str, path: PathBuf) {
    bench::record_if_exists(&observation.0, kind, path);
}

pub(super) fn record_memory_timeline_artifacts(observation: &BenchObservation, run_dir: &RunDir) {
    bench::record_memory_timeline_artifacts(&observation.0, run_dir);
}
