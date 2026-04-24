use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// Shared verification phase vocabulary for isolated commands and composed runners.
///
/// `homeboy lint`, `homeboy audit`, and `homeboy test` stay independent. A
/// future composed command can run these phases in canonical order while reusing
/// the same phase reports and exit-code semantics.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(rename_all = "lowercase")]
pub enum VerificationPhase {
    Syntax,
    Lint,
    Typecheck,
    Audit,
    Test,
}

impl VerificationPhase {
    pub fn canonical_order() -> [Self; 5] {
        [
            Self::Syntax,
            Self::Lint,
            Self::Typecheck,
            Self::Audit,
            Self::Test,
        ]
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum PhaseStatus {
    Passed,
    Failed,
    Error,
    Skipped,
    NotRun,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PhaseFailureCategory {
    Findings,
    Infrastructure,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PhaseReport {
    pub phase: VerificationPhase,
    pub status: PhaseStatus,
    pub exit_code: Option<i32>,
    pub summary: String,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PhaseFailure {
    pub phase: VerificationPhase,
    pub category: PhaseFailureCategory,
    pub summary: String,
}

pub fn phase_status_from_exit_code(exit_code: i32) -> PhaseStatus {
    if exit_code == 0 {
        PhaseStatus::Passed
    } else if exit_code >= 2 {
        PhaseStatus::Error
    } else {
        PhaseStatus::Failed
    }
}

pub fn phase_failure_category_from_exit_code(exit_code: i32) -> PhaseFailureCategory {
    if exit_code >= 2 {
        PhaseFailureCategory::Infrastructure
    } else {
        PhaseFailureCategory::Findings
    }
}

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

    #[test]
    fn verification_phase_order_is_canonical() {
        assert_eq!(
            VerificationPhase::canonical_order(),
            [
                VerificationPhase::Syntax,
                VerificationPhase::Lint,
                VerificationPhase::Typecheck,
                VerificationPhase::Audit,
                VerificationPhase::Test,
            ]
        );
    }

    #[test]
    fn phase_exit_codes_are_classified() {
        assert_eq!(phase_status_from_exit_code(0), PhaseStatus::Passed);
        assert_eq!(phase_status_from_exit_code(1), PhaseStatus::Failed);
        assert_eq!(phase_status_from_exit_code(2), PhaseStatus::Error);
        assert_eq!(
            phase_failure_category_from_exit_code(1),
            PhaseFailureCategory::Findings
        );
        assert_eq!(
            phase_failure_category_from_exit_code(2),
            PhaseFailureCategory::Infrastructure
        );
    }
}

#[cfg(test)]
#[path = "../../../tests/core/extension/runner_contract_test.rs"]
mod runner_contract_test;
