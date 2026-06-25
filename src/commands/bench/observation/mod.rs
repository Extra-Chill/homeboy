mod artifacts;
mod lifecycle;
mod metadata;
mod status;

pub(in crate::commands::bench) use lifecycle::{
    finish_error, finish_success, history_hints, persisted_run_pointer, start,
    BenchObservationStart,
};

#[cfg(test)]
pub(in crate::commands::bench::observation) use artifacts::apply_recorded_bench_artifact_links;
#[cfg(test)]
pub(in crate::commands::bench::observation) use homeboy::core::observation::ArtifactRecord;
#[cfg(test)]
pub(in crate::commands::bench::observation) use lifecycle::BenchObservationSummary;

#[cfg(test)]
#[path = "../../../../tests/commands/bench/observation_artifact_test.rs"]
mod observation_artifact_test;

#[cfg(test)]
mod tests;
