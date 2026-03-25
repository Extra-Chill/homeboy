//! Check files against discovered or explicit conventions.
//!
//! Takes conventions and produces check results that can be converted
//! into actionable findings.

use super::conventions::{Convention, Outlier};

/// Result of checking a set of conventions.
#[derive(Debug, Clone, serde::Serialize)]
pub struct CheckResult {
    /// The convention that was checked.
    pub convention_name: String,
    /// Whether the convention is healthy (no outliers).
    pub status: CheckStatus,
    /// Number of conforming files.
    pub conforming_count: usize,
    /// Total files in the group.
    pub total_count: usize,
    /// Outliers found.
    pub outliers: Vec<Outlier>,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    /// All files conform.
    Clean,
    /// Some files deviate.
    Drift,
    /// Less than half conform — convention may be wrong or split.
    Fragmented,
}

/// Run checks on all discovered conventions.
pub fn check_conventions(conventions: &[Convention]) -> Vec<CheckResult> {
    conventions
        .iter()
        .map(|conv| {
            let status = if conv.outliers.is_empty() {
                CheckStatus::Clean
            } else if conv.confidence >= 0.5 {
                CheckStatus::Drift
            } else {
                CheckStatus::Fragmented
            };

            CheckResult {
                convention_name: conv.name.clone(),
                status,
                conforming_count: conv.conforming.len(),
                total_count: conv.total_files,
                outliers: conv.outliers.clone(),
            }
        })
        .collect()
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::code_audit::conventions::{AuditFinding, Deviation};

    #[test]
    fn clean_convention_produces_clean_status() {
        let conv = Convention {
            name: "Test".to_string(),
            glob: "*.rs".to_string(),
            expected_methods: vec!["run".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["a.rs".to_string(), "b.rs".to_string()],
            outliers: vec![],
            total_files: 2,
            confidence: 1.0,
        };

        let results = check_conventions(&[conv]);
        assert_eq!(results[0].status, CheckStatus::Clean);
    }

    #[test]
    fn outliers_produce_drift_status() {
        let conv = Convention {
            name: "Test".to_string(),
            glob: "*.rs".to_string(),
            expected_methods: vec!["run".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["a.rs".to_string(), "b.rs".to_string()],
            outliers: vec![Outlier {
                file: "c.rs".to_string(),
                noisy: false,
                deviations: vec![Deviation {
                    kind: AuditFinding::MissingMethod,
                    description: "Missing method: run".to_string(),
                    suggestion: "Add run()".to_string(),
                }],
            }],
            total_files: 3,
            confidence: 0.67,
        };

        let results = check_conventions(&[conv]);
        assert_eq!(results[0].status, CheckStatus::Drift);
        assert_eq!(results[0].outliers.len(), 1);
    }

    #[test]
    fn low_confidence_produces_fragmented_status() {
        let conv = Convention {
            name: "Test".to_string(),
            glob: "*.rs".to_string(),
            expected_methods: vec!["run".to_string()],
            expected_registrations: vec![],
            expected_interfaces: vec![],
            expected_namespace: None,
            expected_imports: vec![],
            conforming: vec!["a.rs".to_string()],
            outliers: vec![
                Outlier {
                    file: "b.rs".to_string(),
                    noisy: false,
                    deviations: vec![Deviation {
                        kind: AuditFinding::MissingMethod,
                        description: "Missing".to_string(),
                        suggestion: "Fix".to_string(),
                    }],
                },
                Outlier {
                    file: "c.rs".to_string(),
                    noisy: false,
                    deviations: vec![Deviation {
                        kind: AuditFinding::MissingMethod,
                        description: "Missing".to_string(),
                        suggestion: "Fix".to_string(),
                    }],
                },
            ],
            total_files: 3,
            confidence: 0.33,
        };

        let results = check_conventions(&[conv]);
        assert_eq!(results[0].status, CheckStatus::Fragmented);
    }

    #[test]
    fn test_check_conventions_default_path() {
        let conventions = Vec::new();
        let result = check_conventions(&conventions);
        assert!(!result.is_empty(), "expected non-empty collection for: default path");
    }

}
