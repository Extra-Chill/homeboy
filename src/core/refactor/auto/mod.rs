pub mod apply;
pub mod contracts;
pub mod guard;
pub mod outcome;
pub mod policy;
pub mod sidecar;
pub mod summary;
pub mod transaction;
pub mod verify;

#[cfg(test)]
#[path = "../../../../tests/utils/autofix_test.rs"]
mod autofix_test;

pub use apply::{apply_fixes_via_edit_ops, apply_fixes_via_edit_ops_with_verify};
pub use contracts::{
    ApplyChunkResult, ChunkStatus, DecomposeFixPlan, Fix, FixPolicy, FixResult, Insertion,
    InsertionKind, NewFile, PolicySummary, RefactorPrimitive, SkippedFile,
};
pub use guard::{GuardBlock, GuardConfig, GuardResult};
pub use outcome::{
    standard_outcome, AppliedAutofixCapture, AutofixMode, AutofixOutcome, AutofixSidecarFiles,
    FixApplied, FixResultsSummary, RuleFixCount,
};
pub use policy::apply_fix_policy;
pub use sidecar::parse_fix_results_file;
pub(crate) use summary::primitive_name;
pub use summary::{
    summarize_audit_fix_result, summarize_fix_results, summarize_optional_fix_results,
};
pub use transaction::{
    build_github_remote_url, changes_are_only_drift, run_autofix_transaction, CiContext, PushRoute,
    TransactionOutcome, TransactionRequest, AUTOFIX_COMMIT_PREFIX, DRIFT_COMMIT_PREFIX,
};
pub use verify::{
    applied_files_from_chunks, capture_pre_apply_snapshot, run_verify_gate, VerifyOutcome,
    VERIFY_ENV_VAR,
};
