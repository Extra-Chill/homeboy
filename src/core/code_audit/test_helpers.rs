//! Shared test helpers for code_audit tests.
//!
//! Provides factory functions for building domain types with sensible defaults,
//! reducing boilerplate across test modules.

use super::checks::CheckStatus;
use super::{AuditFinding, AuditSummary, CodeAuditResult, ConventionReport, Finding, Severity};

/// Build a `ConventionReport` with sensible defaults for testing.
///
/// Sets `status` to `Clean`, `total_files` to 3, `confidence` to 1.0,
/// and leaves `conforming`, `outliers`, and optional fields empty.
pub fn make_convention(
    name: &str,
    glob: &str,
    methods: &[&str],
    registrations: &[&str],
) -> ConventionReport {
    ConventionReport {
        name: name.to_string(),
        glob: glob.to_string(),
        status: CheckStatus::Clean,
        expected_methods: methods.iter().map(|s| s.to_string()).collect(),
        expected_registrations: registrations.iter().map(|s| s.to_string()).collect(),
        expected_interfaces: vec![],
        expected_namespace: None,
        expected_imports: vec![],
        conforming: vec![],
        outliers: vec![],
        total_files: 3,
        confidence: 1.0,
    }
}

/// Build an empty `CodeAuditResult` for testing.
pub fn empty_result() -> CodeAuditResult {
    CodeAuditResult {
        component_id: "test-component".to_string(),
        source_path: "/tmp/test".to_string(),
        summary: AuditSummary {
            files_scanned: 0,
            conventions_detected: 0,
            outliers_found: 0,
            alignment_score: None,
            files_skipped: 0,
            warnings: vec![],
        },
        conventions: vec![],
        directory_conventions: vec![],
        findings: vec![],
        duplicate_groups: vec![],
    }
}

/// Build a `Finding` with the given severity for testing.
pub fn make_finding(severity: Severity) -> Finding {
    Finding {
        convention: "TestConvention".to_string(),
        severity,
        file: "src/example.rs".to_string(),
        description: "Test finding".to_string(),
        suggestion: "Fix it".to_string(),
        kind: AuditFinding::MissingMethod,
    }
}
