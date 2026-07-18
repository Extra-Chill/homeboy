use std::path::PathBuf;

use homeboy::core::engine::run_dir::RunDir;
use homeboy::core::observation::ArtifactRecord;
use homeboy_extension::bench::{self, BenchDiagnostic};

use super::lifecycle::BenchObservation;

pub(super) fn record_bench_observation_artifacts(
    observation: &BenchObservation,
    workflow: &mut homeboy_extension::bench::BenchRunWorkflowResult,
    run_dir: &RunDir,
) {
    bench::record_bench_observation_artifacts(&observation.0, workflow, run_dir);
}

pub(super) fn record_if_exists(observation: &BenchObservation, kind: &str, path: PathBuf) {
    bench::record_if_exists(&observation.0, kind, path);
}

pub(super) fn record_memory_timeline_artifacts(observation: &BenchObservation, run_dir: &RunDir) {
    bench::record_memory_timeline_artifacts(&observation.0, run_dir);
}

pub(in crate::commands::bench::observation) fn apply_recorded_bench_artifact_links(
    scenario_id: &str,
    run_index: Option<usize>,
    name: &str,
    artifact: &mut homeboy_extension::bench::BenchArtifact,
    record: &ArtifactRecord,
) -> Option<BenchDiagnostic> {
    bench::apply_recorded_bench_artifact_links(scenario_id, run_index, name, artifact, record)
}
