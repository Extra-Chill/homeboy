use std::collections::HashSet;

/// Generic step filter contract for extension runner scripts.
#[derive(Debug, Clone, Default)]
pub struct RunnerStepFilter {
    pub step: Option<String>,
    pub skip: Option<String>,
}

impl RunnerStepFilter {
    /// Returns true if a step should run under current filter settings.
    pub fn should_run(&self, step_name: &str) -> bool {
        let step_name = step_name.trim();
        if step_name.is_empty() {
            return true;
        }

        let selected = csv_set(self.step.as_deref());
        if !selected.is_empty() && !selected.contains(step_name) {
            return false;
        }

        let skipped = csv_set(self.skip.as_deref());
        if skipped.contains(step_name) {
            return false;
        }

        true
    }

    /// Convert filter to env vars understood by extension scripts.
    pub fn to_env_pairs(&self) -> Vec<(String, String)> {
        let mut env = Vec::new();
        if let Some(step) = &self.step {
            env.push((super::exec_context::STEP.to_string(), step.clone()));
        }
        if let Some(skip) = &self.skip {
            env.push((super::exec_context::SKIP.to_string(), skip.clone()));
        }
        env
    }
}

fn csv_set(value: Option<&str>) -> HashSet<String> {
    value
        .unwrap_or_default()
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(ToString::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_run_with_no_filters() {
        let filter = RunnerStepFilter::default();
        assert!(filter.should_run("lint"));
    }

    #[test]
    fn test_should_run_honors_step_include() {
        let filter = RunnerStepFilter {
            step: Some("lint,test".to_string()),
            skip: None,
        };
        assert!(filter.should_run("lint"));
        assert!(!filter.should_run("deploy"));
    }

    #[test]
    fn test_should_run_honors_skip() {
        let filter = RunnerStepFilter {
            step: None,
            skip: Some("lint".to_string()),
        };
        assert!(!filter.should_run("lint"));
        assert!(filter.should_run("test"));
    }

    #[test]
    fn test_to_env_pairs_exports_step_and_skip() {
        let filter = RunnerStepFilter {
            step: Some("a".to_string()),
            skip: Some("b".to_string()),
        };
        let env = filter.to_env_pairs();
        assert_eq!(env.len(), 2);
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_STEP" && v == "a"));
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_SKIP" && v == "b"));
    }

    #[test]
    fn test_csv_set() {
        let set = csv_set(Some("lint, test,,"));
        assert!(set.contains("lint"));
        assert!(set.contains("test"));
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn test_should_run() {
        let filter = RunnerStepFilter {
            step: Some("lint,test".to_string()),
            skip: Some("test".to_string()),
        };
        assert!(filter.should_run("lint"));
        assert!(!filter.should_run("test"));
        assert!(!filter.should_run("deploy"));
    }

    #[test]
    fn test_to_env_pairs() {
        let filter = RunnerStepFilter {
            step: Some("a".to_string()),
            skip: Some("b".to_string()),
        };
        let env = filter.to_env_pairs();
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_STEP" && v == "a"));
        assert!(env.iter().any(|(k, v)| k == "HOMEBOY_SKIP" && v == "b"));
    }
}

#[cfg(test)]
#[path = "../../../tests/core/extension/runner_contract_test.rs"]
mod runner_contract_test;
