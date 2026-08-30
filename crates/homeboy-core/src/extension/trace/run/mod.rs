//! Trace workflows: invoke extension runners, parse JSON, preserve artifacts.
//!
//! Split into focused submodules:
//! - [`types`] — public input/output structs
//! - [`workflow`] — top-level run orchestration
//! - [`list`] — scenario discovery (list-only)
//! - [`runner`] — runner construction, failure/baseline helpers
//! - [`provenance`] — git/toolchain provenance probes
//! - [`artifacts`] — declared-artifact resolution and results persistence
//! - [`preview`] — public-preview session lifecycle

mod artifacts;
mod list;
mod preview;
mod provenance;
mod runner;
mod types;
mod workflow;

#[cfg(test)]
mod tests;

pub use artifacts::resolve_declared_trace_artifact_path;
pub use list::run_trace_list_workflow;
pub use runner::trace_is_unclaimed;
pub use types::{
    TraceCheckoutProvenance, TraceListWorkflowArgs, TraceOverlay, TraceRunFailure,
    TraceRunWorkflowArgs, TraceRunWorkflowResult, TraceRunnerInputs,
};
pub use workflow::run_trace_workflow;
