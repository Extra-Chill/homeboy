use super::outcome::{AutofixSidecarFiles, FixApplied};
use crate::engine::run_dir::{self, RunDir};
use std::path::Path;

impl AutofixSidecarFiles {
    /// Create sidecar files within a run directory.
    pub fn for_run_dir(run_dir: &RunDir) -> Self {
        Self {
            results_file: run_dir.step_file(run_dir::files::FIX_RESULTS),
        }
    }

    pub fn consume_fix_results(&self) -> Vec<FixApplied> {
        parse_fix_results_file(&self.results_file)
    }
}

pub fn parse_fix_results_file(path: &Path) -> Vec<FixApplied> {
    if !path.exists() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    if content.trim().is_empty() {
        return Vec::new();
    }

    serde_json::from_str(&content).unwrap_or_default()
}
