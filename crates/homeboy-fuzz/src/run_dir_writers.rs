//! Fuzz run-directory writers.
//!
//! Boundary: the caller supplies the run directory handle and the typed request
//! or plan. This module owns serializing those to the canonical run-dir step
//! files, so command modules stay thin adapters that delegate here.

use std::path::PathBuf;

use crate::{FuzzExecutionRequest, FuzzSequencePlan};
use homeboy_core::engine::run_dir::RunDir;

/// Serialize and write the fuzz execution request to its run-dir step file.
pub fn persist_fuzz_execution_request(
    run_dir: &RunDir,
    request: &FuzzExecutionRequest,
) -> homeboy_core::Result<PathBuf> {
    let path = run_dir.step_file(homeboy_core::engine::run_dir::files::FUZZ_EXECUTION_REQUEST);
    let raw = serde_json::to_vec_pretty(request).map_err(|error| {
        homeboy_core::Error::internal_io(
            error.to_string(),
            Some("serialize fuzz execution request".to_string()),
        )
    })?;
    std::fs::write(&path, raw).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    Ok(path)
}

/// Serialize and write the fuzz sequence plan (when present) to its run-dir
/// step file. Returns `Ok(None)` when no plan is supplied.
pub fn persist_fuzz_sequence_plan(
    run_dir: &RunDir,
    plan: Option<&FuzzSequencePlan>,
) -> homeboy_core::Result<Option<PathBuf>> {
    let Some(plan) = plan else {
        return Ok(None);
    };
    let path = run_dir.step_file(homeboy_core::engine::run_dir::files::FUZZ_SEQUENCE_PLAN);
    let raw = serde_json::to_vec_pretty(plan).map_err(|error| {
        homeboy_core::Error::internal_io(
            error.to_string(),
            Some("serialize fuzz sequence plan".to_string()),
        )
    })?;
    std::fs::write(&path, raw).map_err(|error| {
        homeboy_core::Error::internal_io(error.to_string(), Some(path.display().to_string()))
    })?;
    Ok(Some(path))
}
