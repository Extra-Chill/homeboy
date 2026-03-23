pub mod generate;
pub mod planner;
pub mod verify;

pub use generate::generate_audit_fixes;
pub use planner::{
    analyze_stage_overlaps, build_refactor_plan, lint_refactor_request, normalize_sources,
    run_lint_refactor, run_test_refactor, summarize_plan_totals, test_refactor_request,
    LintSourceOptions, PlanOverlap, PlanStageSummary, RefactorPlan, RefactorPlanRequest,
    TestSourceOptions, KNOWN_PLAN_SOURCES,
};
pub use verify::{
    finding_fingerprint, run_audit_refactor, score_delta, weighted_finding_score_with,
    AuditConvergenceScoring, AuditRefactorIterationSummary, AuditRefactorOutcome,
};
