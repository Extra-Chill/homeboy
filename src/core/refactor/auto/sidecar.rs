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

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_for_run_dir_plan_file_some_run_dir_step_file_run_dir_files_fix_plan() {
        let instance = AutofixSidecarFiles::default();
        let run_dir = Default::default();
        let _result = instance.for_run_dir(&run_dir);
    }

    #[test]
    fn test_consume_fix_results_default_path() {
        let instance = AutofixSidecarFiles::default();
        let result = instance.consume_fix_results();
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_parse_fix_results_file_path_exists() {
        let path = Path::new("/tmp/nonexistent_test_path");
        let result = parse_fix_results_file(&path);
        assert!(!result.is_empty(), "expected non-empty collection for: !path.exists()");
    }

    #[test]
    fn test_parse_fix_results_file_err_return_vec_new() {
        let path = tempfile::tempdir().unwrap();
        let result = parse_fix_results_file(path.path());
        assert!(!result.is_empty(), "expected non-empty collection for: Err(_) => return Vec::new(),");
    }

    #[test]
    fn test_parse_fix_results_file_has_expected_effects() {
        // Expected effects: file_read
        let path = Path::new("");
        let _ = parse_fix_results_file(&path);
    }

    #[test]
    fn test_parse_fix_plan_file_default_path() {
        let path = Path::new("");
        let result = parse_fix_plan_file(&path);
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

    #[test]
    fn test_read_fix_results_if_let_some_plan_file_plan_file() {
        let results_file = Path::new("");
        let plan_file = Some(Default::default());
        let result = read_fix_results(&results_file, plan_file);
        assert!(!result.is_empty(), "expected non-empty collection for: if let Some(plan_file) = plan_file {{");
    }

}
