//! The kind-agnostic workload engine behind `homeboy workload` (#15840).
//!
//! A workload kind (`fuzz`, `bench`, `db-perf`, ...) is data: an extension
//! declares it under `workloads.<kind>` and core runs whatever is declared.
//! Nothing in this module names a kind.
//!
//! - [`resolve`] finds the extension that provides a kind and its runner.
//! - [`list`] reads declared and probed workloads.
//! - [`run`] executes one workload through the shared scenario runner,
//!   persists it as an observation run of kind `<kind>` with the generic
//!   result envelope as its artifact, and records its findings.
//! - [`compare`] is the one generic [`KindAdapter`] that compares and gates
//!   two envelopes by the kind's declared metrics and findings key.
//! - [`case`] replays or minimizes one persisted case through the kind's
//!   declared `replay_command` / `minimize_command`.
//! - [`safety`] is the generic destructive-workload gate.
//!
//! [`KindAdapter`]: homeboy_engine_primitives::workload_compare::KindAdapter

pub mod case;
pub mod compare;
pub mod env;
pub mod list;
pub mod resolve;
pub mod run;
pub mod safety;
pub mod stored;

#[cfg(test)]
mod tests;

/// Artifact kind of the persisted generic result envelope.
pub const WORKLOAD_RESULT_ARTIFACT_KIND: &str = "workload_result";
/// Artifact kind of each file the envelope lists under `artifacts`.
pub const WORKLOAD_ARTIFACT_KIND: &str = "workload_artifact";
/// Artifact kind of the isolation proof a destructive run was authorized with.
pub const WORKLOAD_ISOLATION_PROOF_ARTIFACT_KIND: &str = "isolation_proof";
/// Run-metadata key holding the kind's declared compare semantics at run
/// time, so a later compare does not depend on the extension still being
/// installed (or installed at the same revision).
pub const WORKLOAD_COMPARE_METADATA_KEY: &str = "workload_compare";

/// Baseline slot key of a kind inside `homeboy.json` `baselines`.
///
/// Namespaced so a declared kind never collides with a per-kind command's
/// own baseline (`baselines.bench`) while both exist.
pub fn baseline_key(kind: &str) -> String {
    format!("workload.{kind}")
}
