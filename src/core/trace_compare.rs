//! Trace-compare persistence core service.
//!
//! Command modules stay thin adapters: they resolve targets, run the proof
//! matrix, and assemble the comparison output, then delegate the orchestration
//! — output-directory creation, JSON/markdown artifact persistence, and the
//! observation run lifecycle — to this core service. Keeping filesystem
//! mutation and run-artifact persistence here means the command layer never
//! accumulates orchestration weight.

use std::path::{Path, PathBuf};

use serde::Serialize;

use crate::core::observation::{NewRunRecord, ObservationStore, RunStatus};

/// Resolved on-disk locations of the artifacts a compare run persists.
pub struct CompareArtifactPaths {
    pub baseline: PathBuf,
    pub candidate: PathBuf,
    pub compare: PathBuf,
    pub summary: PathBuf,
}

/// The data a single compare run persists to its output directory.
pub struct CompareArtifactSet<'a, B, C, M> {
    pub baseline_aggregate: &'a B,
    pub candidate_aggregate: &'a C,
    pub compare: &'a M,
    pub summary_markdown: &'a str,
}

/// Create the output directory for a compare run, mirroring `mkdir -p`.
pub fn prepare_output_dir(output_dir: &Path) -> crate::core::Result<()> {
    std::fs::create_dir_all(output_dir).map_err(|err| {
        crate::core::Error::internal_io(
            format!(
                "Failed to create trace compare output dir {}: {}",
                output_dir.display(),
                err
            ),
            Some("trace.compare.output_dir".to_string()),
        )
    })
}

/// Serialize `value` to pretty JSON and write it to `path`.
pub fn write_json_artifact<T: Serialize>(path: &Path, value: &T) -> crate::core::Result<()> {
    let content = serde_json::to_string_pretty(value).map_err(|err| {
        crate::core::Error::internal_json(err.to_string(), Some("trace.compare.json".to_string()))
    })?;
    std::fs::write(path, content).map_err(|err| {
        crate::core::Error::internal_io(
            format!("Failed to write trace artifact {}: {}", path.display(), err),
            Some("trace.compare.write".to_string()),
        )
    })
}

/// Persist the full compare artifact set into `output_dir`, returning the
/// resolved artifact paths. Owns every filesystem write so the command layer
/// only assembles the in-memory comparison.
pub fn persist_compare_artifacts<B: Serialize, C: Serialize, M: Serialize>(
    output_dir: &Path,
    set: CompareArtifactSet<'_, B, C, M>,
) -> crate::core::Result<CompareArtifactPaths> {
    let paths = CompareArtifactPaths {
        baseline: output_dir.join("baseline.aggregate.json"),
        candidate: output_dir.join("candidate.aggregate.json"),
        compare: output_dir.join("compare.json"),
        summary: output_dir.join("summary.md"),
    };
    write_json_artifact(&paths.baseline, set.baseline_aggregate)?;
    write_json_artifact(&paths.candidate, set.candidate_aggregate)?;
    write_json_artifact(&paths.compare, set.compare)?;
    std::fs::write(&paths.summary, set.summary_markdown).map_err(|err| {
        crate::core::Error::internal_io(
            format!(
                "Failed to write trace compare summary {}: {}",
                paths.summary.display(),
                err
            ),
            Some("trace.compare.summary".to_string()),
        )
    })?;
    Ok(paths)
}

/// An active observation run bracketing a trace-compare invocation. Owns the
/// `ObservationStore` interactions (run start, artifact recording, run finish)
/// so the command layer never touches run-artifact persistence directly.
pub struct CompareObservation {
    store: ObservationStore,
    run_id: String,
}

impl CompareObservation {
    /// Open an observation run for a compare invocation. Returns `None` when the
    /// store is unavailable or the run cannot be started; compare runs treat
    /// observation as best-effort.
    pub fn start(record: NewRunRecord) -> Option<Self> {
        let store = ObservationStore::open_initialized().ok()?;
        let run = store.start_run(record).ok()?;
        Some(Self {
            store,
            run_id: run.id,
        })
    }

    /// Record the standard compare artifact set against the run and finish it.
    pub fn finish(
        self,
        status: RunStatus,
        paths: &CompareArtifactPaths,
        metadata: serde_json::Value,
    ) {
        let _ = self.store.record_artifact(
            &self.run_id,
            "trace-compare-baseline-aggregate",
            &paths.baseline,
        );
        let _ = self.store.record_artifact(
            &self.run_id,
            "trace-compare-candidate-aggregate",
            &paths.candidate,
        );
        let _ = self
            .store
            .record_artifact(&self.run_id, "trace-compare-json", &paths.compare);
        let _ = self
            .store
            .record_artifact(&self.run_id, "trace-compare-summary", &paths.summary);
        let _ = self.store.finish_run(&self.run_id, status, Some(metadata));
    }
}
