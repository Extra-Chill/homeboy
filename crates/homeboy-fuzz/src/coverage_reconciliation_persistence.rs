//! Fuzz coverage-reconciliation persistence.
//!
//! Boundary: the caller supplies the observation store handle, the run id, the
//! on-disk execution-request path, and the parsed campaign. This module owns the
//! reconciliation compute + write + artifact-record orchestration so command
//! modules stay thin adapters that delegate here.

use std::path::{Path, PathBuf};

use crate::{reconcile_fuzz_coverage, FuzzCampaign, FuzzCoverageReconciliation};
use homeboy_core::observation::{ArtifactRecord, ObservationStore};

/// Observation-store artifact kind for a persisted fuzz coverage reconciliation.
pub const FUZZ_COVERAGE_RECONCILIATION_ARTIFACT_KIND: &str = "fuzz_coverage_reconciliation";

/// Reconcile fuzz coverage for a run and persist the reconciliation artifact.
///
/// Reads and parses the execution request at `execution_request_path`,
/// reconciles it against `campaign`, writes the reconciliation JSON next to the
/// request, and records it as a run artifact. Returns `Ok(None)` when the
/// execution request file is absent.
pub fn persist_fuzz_coverage_reconciliation(
    store: &ObservationStore,
    run_id: &str,
    execution_request_path: &Path,
    campaign: &FuzzCampaign,
) -> homeboy_core::Result<Option<ArtifactRecord>> {
    if !execution_request_path.is_file() {
        return Ok(None);
    }
    let raw = std::fs::read_to_string(execution_request_path).map_err(|error| {
        homeboy_core::Error::internal_io(
            error.to_string(),
            Some(execution_request_path.display().to_string()),
        )
    })?;
    let request: crate::FuzzExecutionRequest = serde_json::from_str(&raw).map_err(|error| {
        homeboy_core::Error::invalid_argument_for(
            "fuzz_execution_request",
            format!("failed to parse fuzz execution request for coverage reconciliation: {error}"),
            execution_request_path.display().to_string(),
        )
    })?;
    let reconciliation = reconcile_fuzz_coverage(&request, campaign);
    let artifact_path = fuzz_coverage_reconciliation_path(execution_request_path);
    write_fuzz_coverage_reconciliation(&artifact_path, &reconciliation)?;
    store
        .record_artifact_with_metadata(
            run_id,
            FUZZ_COVERAGE_RECONCILIATION_ARTIFACT_KIND,
            &artifact_path,
            serde_json::json!({
                "schema": crate::FUZZ_COVERAGE_RECONCILIATION_SCHEMA,
                "source": "homeboy fuzz run",
                "request_id": reconciliation.request_id,
                "campaign_id": reconciliation.campaign_id,
            }),
        )
        .map(Some)
}

fn fuzz_coverage_reconciliation_path(execution_request_path: &Path) -> PathBuf {
    execution_request_path
        .parent()
        .map(|parent| {
            parent.join(homeboy_core::engine::run_dir::files::FUZZ_COVERAGE_RECONCILIATION)
        })
        .unwrap_or_else(|| {
            PathBuf::from(homeboy_core::engine::run_dir::files::FUZZ_COVERAGE_RECONCILIATION)
        })
}

fn write_fuzz_coverage_reconciliation(
    path: &Path,
    reconciliation: &FuzzCoverageReconciliation,
) -> homeboy_core::Result<()> {
    let raw = serde_json::to_vec_pretty(reconciliation).map_err(|error| {
        homeboy_core::Error::internal_unexpected(format!(
            "failed to encode fuzz coverage reconciliation: {error}"
        ))
    })?;
    std::fs::write(path, raw).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })
}
