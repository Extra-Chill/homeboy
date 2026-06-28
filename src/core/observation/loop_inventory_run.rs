//! Loop-archive inventory run persistence.
//!
//! Boundary: the caller computes the agnostic inputs (the run record, the set of
//! archive paths, and the indexed artifact paths with their kind labels). This
//! module owns the run-persistence orchestration: opening the observation store,
//! starting the run, recording each artifact, and finishing the run as a pass.

use std::path::PathBuf;

use crate::core::observation::{ArtifactRecord, NewRunRecord, ObservationStore, RunStatus};
use crate::core::Result;

/// Persist a loop-archive inventory run to the observation store.
///
/// Opens (and initializes) the observation store, starts a run from the supplied
/// record, records each top-level archive (as a directory or file artifact) and
/// each indexed artifact (with its supplied kind), then finishes the run as a
/// pass. Returns the finished run id and the recorded artifacts.
///
/// `archive_paths` are recorded under the `loop_archive` kind; directories use a
/// directory artifact and files use a file artifact. `indexed_artifacts` are
/// `(path, kind)` pairs recorded as file artifacts when the path is a file.
pub fn persist_loop_inventory_run(
    run_record: NewRunRecord,
    archive_paths: &[PathBuf],
    indexed_artifacts: &[(PathBuf, String)],
) -> Result<(String, Vec<ArtifactRecord>)> {
    let store = ObservationStore::open_initialized()?;
    let run = store.start_run(run_record)?;
    let mut artifacts = Vec::new();

    for archive in archive_paths {
        if archive.is_dir() {
            artifacts.push(store.record_directory_artifact(&run.id, "loop_archive", archive)?);
        } else if archive.is_file() {
            artifacts.push(store.record_artifact(&run.id, "loop_archive", archive)?);
        }
    }

    for (file, kind) in indexed_artifacts {
        if file.is_file() {
            artifacts.push(store.record_artifact(&run.id, kind, file)?);
        }
    }

    let finished = store.finish_run(&run.id, RunStatus::Pass, None)?;
    Ok((finished.id, artifacts))
}
