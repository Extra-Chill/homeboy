use super::outcome::{AutofixSidecarFiles, FixApplied};
use crate::engine::run_dir::{self, RunDir};
use std::path::Path;

impl AutofixSidecarFiles {
    /// Create sidecar files within a run directory.
    pub fn for_run_dir(run_dir: &RunDir) -> Self {
        Self {
            results_file: run_dir.step_file(run_dir::files::FIX_RESULTS),
            plan_file: Some(run_dir.step_file(run_dir::files::FIX_PLAN)),
        }
    }

    pub fn consume_fix_results(&self) -> Vec<FixApplied> {
        read_fix_results(&self.results_file, self.plan_file.as_deref())
    }
}

pub(crate) fn parse_fix_results_file(path: &Path) -> Vec<FixApplied> {
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

pub(crate) fn parse_fix_plan_file(path: &Path) -> Vec<FixApplied> {
    parse_fix_results_file(path)
}

pub(crate) fn read_fix_results(results_file: &Path, plan_file: Option<&Path>) -> Vec<FixApplied> {
    if let Some(plan_file) = plan_file {
        let planned_fix_results = parse_fix_plan_file(plan_file);
        if !planned_fix_results.is_empty() {
            return planned_fix_results;
        }
    }

    parse_fix_results_file(results_file)
}
