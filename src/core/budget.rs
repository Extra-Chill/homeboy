//! Shared budget/threshold finding contract.
//!
//! Benchmark and profile workloads can emit these findings when a measured
//! value crosses a fixed budget. `severity = "error"` (or `passed = false`)
//! gates the run; lower severities remain report-only.

use serde::{Deserialize, Serialize};

use crate::core::finding::{FindingSource, HomeboyFinding};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BudgetFinding {
    #[serde(default = "budget_category")]
    pub category: String,
    pub code: String,
    #[serde(default = "error_severity")]
    pub severity: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,
    pub context_label: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<f64>,
    pub expected: f64,
    pub unit: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
    #[serde(default = "default_passed")]
    pub passed: bool,
}

impl BudgetFinding {
    pub fn failure(
        code: impl Into<String>,
        context_label: impl Into<String>,
        message: impl Into<String>,
        actual: impl Into<Option<f64>>,
        expected: f64,
        unit: impl Into<String>,
        subject: Option<String>,
    ) -> Self {
        Self {
            category: budget_category(),
            code: code.into(),
            severity: error_severity(),
            file: None,
            context_label: context_label.into(),
            message: message.into(),
            actual: actual.into(),
            expected,
            unit: unit.into(),
            subject,
            passed: false,
        }
    }

    pub fn is_gate_failure(&self) -> bool {
        !self.passed || self.severity == "error"
    }

    pub fn fingerprint(&self) -> String {
        match self.subject.as_deref() {
            Some(subject) if !subject.is_empty() => format!("{}:{}", self.code, subject),
            _ => self.code.clone(),
        }
    }

    pub fn to_homeboy_finding(&self) -> HomeboyFinding {
        let mut normalized = HomeboyFinding::builder("budget", self.message.clone())
            .rule(self.code.clone())
            .category(self.category.clone())
            .severity(self.severity.clone())
            .fingerprint(self.fingerprint())
            .source(FindingSource::new("budget").label(self.context_label.clone()))
            .metadata("code", self.code.clone())
            .metadata("category", self.category.clone())
            .metadata("context_label", self.context_label.clone())
            .metadata("actual", self.actual)
            .metadata("expected", self.expected)
            .metadata("unit", self.unit.clone())
            .metadata("subject", self.subject.clone())
            .metadata("passed", self.passed)
            .raw(self)
            .build();
        normalized.location.file = self.file.clone();
        normalized
    }
}

fn budget_category() -> String {
    "budget".to_string()
}

fn error_severity() -> String {
    "error".to_string()
}

fn default_passed() -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failure_uses_budget_defaults() {
        let finding = BudgetFinding::failure(
            "rest.max_response_bytes",
            "profile:wordpress-rest",
            "REST response exceeded 250 KB budget",
            4378195.0,
            250000.0,
            "bytes",
            Some("/wp-json/datamachine/v1/pipelines?per_page=100".to_string()),
        );

        assert_eq!(finding.category, "budget");
        assert_eq!(finding.severity, "error");
        assert_eq!(finding.actual, Some(4378195.0));
        assert!(!finding.passed);
        assert!(finding.is_gate_failure());
    }

    #[test]
    fn test_fingerprint() {
        let finding = BudgetFinding::failure(
            "rest.max_response_bytes",
            "profile:wordpress-rest",
            "REST response exceeded 250 KB budget",
            4378195.0,
            250000.0,
            "bytes",
            Some("/wp-json/datamachine/v1/pipelines?per_page=100".to_string()),
        );

        assert_eq!(
            finding.fingerprint(),
            "rest.max_response_bytes:/wp-json/datamachine/v1/pipelines?per_page=100"
        );

        let without_subject = BudgetFinding::failure(
            "page.ready_ms",
            "profile:page-ready",
            "Page ready time exceeded budget",
            1200.0,
            1000.0,
            "ms",
            None,
        );
        assert_eq!(without_subject.fingerprint(), "page.ready_ms");
    }
}
