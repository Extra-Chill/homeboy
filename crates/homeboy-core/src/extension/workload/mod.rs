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

use std::path::{Path, PathBuf};

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
/// Run-metadata key holding the selected rig workload file's absolute path,
/// so a later replay or minimize exports the same `HOMEBOY_WORKLOAD_PATH`.
pub const WORKLOAD_PATH_METADATA_KEY: &str = "workload_path";
/// Run-metadata key holding where the run's effective safety class came
/// from: the kind's declaration or the selected workload file.
pub const SAFETY_CLASS_SOURCE_METADATA_KEY: &str = "safety_class_source";

/// A rig-owned workload file, resolved above this crate and passed in as
/// plain data. `id` is the file stem, the same rule the legacy fuzz
/// command uses for rig workload files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RigWorkloadFile {
    pub id: String,
    pub path: PathBuf,
}

/// The rig workload file a selected `--workload <id>` names, if any.
pub fn selected_rig_workload<'a>(
    workloads: &'a [RigWorkloadFile],
    workload_id: Option<&str>,
) -> Option<&'a RigWorkloadFile> {
    let workload_id = workload_id?;
    workloads.iter().find(|file| file.id == workload_id)
}

/// Absolute form of a rig workload path, so a runner that changes directory
/// still resolves it.
pub fn absolute_workload_path(path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map(|cwd| cwd.join(path))
            .unwrap_or_else(|_| path.to_path_buf())
    }
}

/// Baseline slot key of a kind inside `homeboy.json` `baselines`.
///
/// Namespaced so a declared kind never collides with a per-kind command's
/// own baseline (`baselines.bench`) while both exist.
pub fn baseline_key(kind: &str) -> String {
    format!("workload.{kind}")
}
