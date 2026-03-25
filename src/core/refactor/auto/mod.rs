pub mod apply;
pub mod contracts;
pub mod outcome;
pub mod policy;
pub mod preflight;
pub mod sidecar;
pub mod summary;
pub mod tracking;

pub use apply::{
    apply_decompose_plans, apply_file_moves, apply_fixes, apply_fixes_chunked,
    apply_new_files_chunked, auto_apply_subset,
};
pub use contracts::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, ChunkVerifier, DecomposeFixPlan, Fix, FixPolicy,
    FixResult, FixSafetyTier, Insertion, InsertionKind, NewFile, PolicySummary, PreflightCheck,
    PreflightContext, PreflightReport, PreflightStatus, SkippedFile,
};
pub use outcome::{
    standard_outcome, AppliedAutofixCapture, AutofixMode, AutofixOutcome, AutofixSidecarFiles,
    FixApplied, FixResultsSummary, RuleFixCount,
};
pub use policy::apply_fix_policy;
pub use preflight::{run_fix_preflight, run_insertion_preflight, run_new_file_preflight};
pub use summary::{
    summarize_audit_fix_result, summarize_fix_results, summarize_optional_fix_results,
};
