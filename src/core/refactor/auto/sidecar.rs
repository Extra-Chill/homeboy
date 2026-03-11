use super::outcome::{AutofixSidecarFiles, FixApplied};
use crate::engine::temp;
use std::path::Path;

impl AutofixSidecarFiles {
    pub fn for_apply() -> Self {
        Self {
            results_file: fix_results_temp_path(),
            plan_file: None,
        }
    }

    pub fn for_plan() -> Self {
        Self {
            results_file: fix_results_temp_path(),
            plan_file: Some(fix_plan_temp_path()),
        }
    }

    pub fn consume_fix_results(&self) -> Vec<FixApplied> {
        let fix_results = read_fix_results(&self.results_file, self.plan_file.as_deref());
        self.cleanup();
        fix_results
    }

    pub fn cleanup(&self) {
        let _ = std::fs::remove_file(&self.results_file);
        if let Some(plan_file) = &self.plan_file {
            let _ = std::fs::remove_file(plan_file);
        }
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

pub fn parse_fix_plan_file(path: &Path) -> Vec<FixApplied> {
    parse_fix_results_file(path)
}

pub fn read_fix_results(results_file: &Path, plan_file: Option<&Path>) -> Vec<FixApplied> {
    if let Some(plan_file) = plan_file {
        let planned_fix_results = parse_fix_plan_file(plan_file);
        if !planned_fix_results.is_empty() {
            return planned_fix_results;
        }
    }

    parse_fix_results_file(results_file)
}

pub fn fix_results_temp_path() -> std::path::PathBuf {
    temp::runtime_temp_file("homeboy-fix-results", ".json")
        .expect("runtime temp path should be creatable for fix results")
}

pub fn fix_plan_temp_path() -> std::path::PathBuf {
    temp::runtime_temp_file("homeboy-fix-plan", ".json")
        .expect("runtime temp path should be creatable for fix plan")
}
