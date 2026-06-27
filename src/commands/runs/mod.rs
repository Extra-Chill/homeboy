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
mod fuzz_compare;
mod gh_actions;
mod handlers;
mod hotspots;
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

// Intra-module re-exports so sibling submodules (and the test modules) can
// reference shared items via `super::` without depending on each other's
// internal module paths. `pub(super)` items are re-exported with a private
// `use` (still reachable by descendant submodules) so the re-export never
// widens their visibility.
pub(crate) use common::RunSummary;
#[cfg(test)]
pub(crate) use handlers::artifact_get;
use handlers::require_run;
pub(crate) use handlers::run_summary;
pub(crate) use hotspots::fuzz_hotspot_lines;
#[cfg(test)]
pub(crate) use types::RunsArtifactGetArgs;
use types::DEFAULT_LIMIT;
pub(crate) use types::{RunsListArgs, RunsListOutput};

pub use bench::{bench_compare, BenchCompareOutput, RunsBenchCompareArgs};
pub(super) use bench::{bench_numeric_metrics, run_contains_scenario};
pub use distribution::{runs_distribution, RunsDistributionArgs, RunsDistributionOutput};

// Test-only helpers consumed by sibling test modules via `super::runs::*` / `super::*`.
#[cfg(test)]
use bundle::{export_runs, import_runs, RunsExportArgs, RunsImportArgs};
#[cfg(test)]
use common::dead_owned_run;
