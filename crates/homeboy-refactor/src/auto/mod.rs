pub(crate) mod apply;
pub(crate) mod contracts;
pub(crate) mod guard;
pub(crate) mod outcome;
pub(crate) mod policy;
pub(crate) mod sidecar;
pub(crate) mod summary;
pub(crate) mod transaction;
pub(crate) mod verify;

#[cfg(test)]
mod autofix_test;

pub(crate) use apply::{apply_fixes_via_edit_ops, apply_fixes_via_edit_ops_with_verify};
// `refactor autofix` serializes per-fix outcomes, so only `FixResult` leaves the crate.
// `FixResult` and every type reachable through one of its public fields.
pub use contracts::{ApplyChunkResult, DecomposeFixPlan, Fix, FixResult, NewFile, SkippedFile};
// Reachable through `Fix::insertions`, `ApplyChunkResult::status`, and
// `NewFile::primitive` respectively.
pub use contracts::{ChunkStatus, Insertion, InsertionKind, RefactorPrimitive};
pub(crate) use contracts::{FixPolicy, PolicySummary};
// `GuardBlock` is reachable through `RefactorSourceRun::guard_block`.
pub use guard::GuardBlock;
pub(crate) use outcome::{
    standard_outcome, AutofixMode, AutofixSidecarFiles, FixApplied, FixResultsSummary,
};
pub(crate) use policy::apply_fix_policy;
pub(crate) use summary::primitive_name;
pub(crate) use summary::{
    summarize_audit_fix_result, summarize_fix_results, summarize_optional_fix_results,
};
// `ci autofix` drives the transaction, so the request/outcome cluster leaves the crate.
pub use transaction::{
    run_autofix_transaction, CiContext, TransactionOutcome, TransactionRequest,
    AUTOFIX_COMMIT_PREFIX,
};
// `PushRoute` is reachable through `TransactionOutcome::route`.
pub use transaction::PushRoute;
