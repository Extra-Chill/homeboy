use crate::code_audit::{AuditSummary, CodeAuditResult, Finding, Severity};

fn empty_result() -> CodeAuditResult {
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

fn make_finding(severity: Severity) -> Finding {
    Finding {
        convention: "TestConvention".to_string(),
        severity,
        file: "src/example.rs".to_string(),
        description: "Test finding".to_string(),
        suggestion: "Fix it".to_string(),
        kind: crate::code_audit::AuditFinding::MissingMethod,
    }
}
