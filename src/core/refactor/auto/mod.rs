pub mod apply;
pub mod contracts;
pub mod guard;
pub mod outcome;
pub mod policy;
pub mod sidecar;
pub mod summary;
pub mod tracking;

pub use apply::{
    apply_decompose_plans, apply_file_moves, apply_fixes, apply_fixes_chunked, apply_new_files,
    apply_new_files_chunked, auto_apply_subset,
};
pub use contracts::{
    ApplyChunkResult, ApplyOptions, ChunkStatus, ChunkVerifier, DecomposeFixPlan, Fix, FixPolicy,
    FixResult, Insertion, InsertionKind, NewFile, PolicySummary, RefactorPrimitive, SkippedFile,
};
pub use guard::{GuardBlock, GuardConfig, GuardResult};
pub use outcome::{
    standard_outcome, AppliedAutofixCapture, AutofixMode, AutofixOutcome, AutofixSidecarFiles,
    FixApplied, FixResultsSummary, RuleFixCount,
};
pub use policy::apply_fix_policy;
pub use sidecar::{parse_fix_plan_file, parse_fix_results_file, read_fix_results};
pub use summary::{
    primitive_name, summarize_audit_fix_result, summarize_fix_results,
    summarize_optional_fix_results,
};
pub use tracking::{changed_file_set, count_newly_changed, newly_changed_files};
