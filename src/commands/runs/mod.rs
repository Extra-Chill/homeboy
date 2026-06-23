//! `homeboy runs` command surface.
//!
//! The module root wires together the focused submodules:
//! - [`types`] — clap argument/subcommand enums and serializable outputs.
//! - [`dispatch`] — subcommand routing and `RunsArgs` inherent helpers.
//! - [`handlers`] — observation-store reads backing list/show/artifacts.
//! - per-concern submodules (`bench`, `bundle`, `compare`, ...) own the rest.

#[cfg(test)]
mod artifact_index_tests;
mod bench;
mod bundle;
#[cfg(test)]
mod bundle_import_tests;
mod common;
mod compare;
#[cfg(test)]
mod corpus_tests;
mod disk;
mod dispatch;
mod distribution;
mod drift;
mod evidence;
mod findings;
mod gh_actions;
mod handlers;
mod latest;
mod loop_sync;
mod query;
mod reconcile;
mod refs;
mod remote;
mod remote_artifact;
#[cfg(test)]
mod tests;
mod types;

use super::{CmdResult, GlobalArgs};

// Public command-layer API consumed by routing, raw/json output, rig, and bench.
pub use dispatch::{global_runner_error, run, run_markdown};
pub use handlers::list_runs;
pub use types::{RunsArgs, RunsOutput, WORDPRESS_PLAYGROUND_BLUEPRINT_VIEWER};

// Intra-module re-exports so sibling submodules can reference shared items via
// `super::` without depending on each other's internal module paths.
pub(crate) use common::RunSummary;
pub(crate) use handlers::{require_run, run_summary};
pub(crate) use types::{
    DEFAULT_LIMIT, RunsArtifactGetArgs, RunsListArgs, RunsListOutput,
};

pub(crate) use bench::bench_compare_from_args;
pub use bench::{bench_compare, BenchCompareOutput, RunsBenchCompareArgs};
pub(super) use bench::{bench_numeric_metrics, run_contains_scenario};
pub(crate) use bundle::{
    export_runs, import_runs, RunsExportArgs, RunsExportOutput, RunsImportArgs, RunsImportOutput,
};
pub use distribution::{runs_distribution, RunsDistributionArgs, RunsDistributionOutput};

#[cfg(test)]
pub(crate) use common::SkippedArtifactRow;
pub(crate) use drift::RunsDriftOutput;
#[cfg(test)]
pub(crate) use drift::{DriftValue, RunsDriftFilters};
#[cfg(test)]
pub(crate) use query::{
    QueryGroup, QueryRow, RunsQueryFilters, RunsQueryOutput as TestRunsQueryOutput,
};
pub(crate) use refs::RunsRefsOutput;
#[cfg(test)]
pub(crate) use refs::{
    ArtifactRef as RunsRefsArtifactRef, RunRef as RunsRefsRunRef, RunsRefsFilters,
};
