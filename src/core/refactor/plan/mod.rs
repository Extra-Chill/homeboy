pub mod file_intent;
pub mod generate;
pub mod sources;
pub mod verify;

pub use generate::generate_audit_fixes;
pub use sources::{
    analyze_stage_overlaps, collect_refactor_sources, lint_refactor_request, normalize_sources,
    run_lint_refactor, run_test_refactor, summarize_source_totals, test_refactor_request,
    CollectedEdit, LintSourceOptions, RefactorSourceRequest, RefactorSourceRun, SourceOverlap,
    SourceStageSummary, SourceTotals, TestSourceOptions, KNOWN_REFACTOR_SOURCES,
};
pub use verify::{
    finding_fingerprint, run_audit_refactor, score_delta, weighted_finding_score_with,
    AuditConvergenceScoring, AuditRefactorIterationSummary, AuditRefactorOutcome,
};
