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

use crate::core::observation::{NewRunRecord, ObservationStore, RunEvidenceCommands, RunStatus};

/// Resolved on-disk locations of the artifacts a compare run persists.
pub struct CompareArtifactPaths {
    pub baseline: PathBuf,
    pub candidate: PathBuf,
    pub compare: PathBuf,
    pub summary: PathBuf,
    pub pair: PathBuf,
}

/// First-class, provider-agnostic compare pair artifact. It is the canonical
/// evidence record a `trace compare` invocation produces: a single index that
/// links the compare command, the child baseline/candidate observation run ids,
/// the generated report/summary path, and the persisted artifact bundle
/// directories. Downstream report commands address this record instead of
/// rediscovering run ids and artifact directories from temp paths.
///
/// The shape is intentionally generic — it knows nothing about WordPress,
/// browsers, screenshots, or any specific extension. Visual diffs and other
/// post-compare evidence are linked generically through `post_compare_artifacts`.
#[derive(Debug, Clone, Serialize)]
pub struct ComparePairArtifact {
    /// Schema marker so consumers can detect the artifact kind without guessing.
    pub kind: &'static str,
    /// The command that produced this compare (e.g. `trace.compare`).
    pub command: String,
    /// RFC3339 timestamp of when the pair artifact was assembled.
    pub timestamp: String,
    /// Component the compare ran against.
    pub component: String,
    /// Scenario identifier the compare exercised.
    pub scenario_id: String,
    /// Resolved status of the compare (`pass`/`fail`).
    pub status: String,
    /// Baseline side reference (target input, resolved run ids, artifact dirs).
    pub baseline: ComparePairSide,
    /// Candidate side reference (target input, resolved run ids, artifact dirs).
    pub candidate: ComparePairSide,
    /// Output directory holding the persisted compare artifacts.
    pub output_dir: String,
    /// Compare JSON artifact path (the structured comparison result).
    pub compare_json: String,
    /// Generated Markdown report/summary path.
    pub summary_path: String,
    /// Any post-compare evidence artifacts (e.g. visual diffs) addressed
    /// generically by kind + path so the model stays extension-agnostic.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub post_compare_artifacts: Vec<ComparePairLinkedArtifact>,
}

/// One side (baseline or candidate) of a compare pair: the target it ran
/// against, the child observation run ids it produced, and the run artifact
/// directories those runs wrote.
#[derive(Debug, Clone, Serialize)]
pub struct ComparePairSide {
    /// The target input (path or git ref) requested for this side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// Resolved git sha for the side, when known.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    /// Aggregate status reported for the side.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<String>,
    /// Child observation run ids, each addressable via `homeboy runs evidence`.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub run_ids: Vec<String>,
    /// Retrieval commands for the first child run, when present, so an agent can
    /// pivot to per-run evidence without constructing commands by hand.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub run_evidence: Vec<RunEvidenceCommands>,
    /// Run artifact directories produced by the side's child runs.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifact_dirs: Vec<String>,
}

/// A generically-addressed post-compare artifact (kind + path) such as a visual
/// diff bundle. The pair artifact records the address, not the semantics.
#[derive(Debug, Clone, Serialize)]
pub struct ComparePairLinkedArtifact {
    pub kind: String,
    pub path: String,
}

impl ComparePairSide {
    /// Build a side reference, deriving the evidence retrieval commands from the
    /// child run ids so downstream consumers get ready-to-run addresses.
    pub fn new(
        target: Option<String>,
        git_sha: Option<String>,
        status: Option<String>,
        run_ids: Vec<String>,
        artifact_dirs: Vec<String>,
    ) -> Self {
        let run_evidence = run_ids
            .iter()
            .map(|run_id| RunEvidenceCommands::for_run_id(run_id))
            .collect();
        Self {
            target,
            git_sha,
            status,
            run_ids,
            run_evidence,
            artifact_dirs,
        }
    }
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
        pair: output_dir.join("compare.pair.json"),
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

/// Persist the first-class compare pair artifact to `path`, returning the
/// artifact unchanged so the command layer can embed it in observation
/// metadata. This is the canonical evidence index for a compare run.
pub fn persist_compare_pair_artifact(
    path: &Path,
    pair: &ComparePairArtifact,
) -> crate::core::Result<()> {
    write_json_artifact(path, pair)
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
        let _ = self
            .store
            .record_artifact(&self.run_id, "trace-compare-pair", &paths.pair);
        let _ = self.store.finish_run(&self.run_id, status, Some(metadata));
    }
}
