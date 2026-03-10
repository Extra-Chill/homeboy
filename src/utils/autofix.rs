//! Compatibility shim for historical autofix imports.
//!
//! Code-factory autofix concepts now live under `crate::refactor::auto`.
//! Keep this module as a temporary re-export layer while older callers are
//! migrated off the `utils::autofix` path.

pub use crate::refactor::auto::{
    begin_applied_fix_capture, changed_file_set, count_newly_changed, finish_applied_fix_capture,
    fix_plan_temp_path, fix_results_temp_path, newly_changed_files, parse_fix_plan_file,
    parse_fix_results_file, read_fix_results, standard_outcome, summarize_audit_fix_result,
    summarize_fix_results, summarize_optional_fix_results, AppliedAutofixCapture, AutofixMode,
    AutofixOutcome, AutofixSidecarFiles, FixApplied, FixResultsSummary, RuleFixCount,
};
