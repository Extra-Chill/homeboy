//! Structural refactoring — rename, add, move, and transform code across a codebase.
//!
//! Walks source files, finds all references to a term (with word-boundary matching
//! and case-variant awareness), generates edits, and optionally applies them.

use crate::refactor::auto::{AppliedAutofixCapture, FixResultsSummary};
use serde::Serialize;
use std::path::PathBuf;

pub mod add;
pub mod audit_fixability_provider;
pub mod auto;
pub mod decompose;
pub mod edit_op_tagged;
pub mod move_items;
pub mod plan;
pub mod primitive_builders;
pub mod propagate;
mod rename;
pub mod transform;

/// Resolve the refactor root directory from an explicit path or component id.
pub fn resolve_root(component_id: Option<&str>, path: Option<&str>) -> crate::Result<PathBuf> {
    let target =
        crate::component::resolve_target(crate::component::TargetSpec::new(component_id, path))?;
    if !target.source_path.is_dir() {
        return Err(crate::Error::validation_invalid_argument(
            "path",
            format!("Not a directory: {}", target.source_path.display()),
            None,
            None,
        ));
    }

    Ok(target.source_path)
}

/// Shared output for refactors/fixes.
///
// AppliedRefactor and its FixResultsSummary/RuleFixCount/PrimitiveFixCount
// result-type cluster live in homeboy-refactor-contract so consumers (e.g. the
// extension lint/test report layer) can carry refactor results without depending
// on this engine. Re-exported here to preserve crate::refactor::AppliedRefactor
// call sites.
pub use homeboy_refactor_contract::AppliedRefactor;

/// Build an [`AppliedRefactor`] from an autofix capture. Lives in the refactor
/// engine (not on the contract type) because it consumes the engine-internal
/// `AppliedAutofixCapture`.
pub fn applied_refactor_from_capture(
    capture: AppliedAutofixCapture,
    rerun_recommended: bool,
    changed_files: Vec<String>,
) -> AppliedRefactor {
    AppliedRefactor {
        files_modified: capture.files_modified,
        rerun_recommended,
        changed_files,
        fix_summary: capture.fix_summary,
    }
}

pub use add::{add_import, fixes_from_audit, AddResult};
pub use auto::{
    apply_fix_policy, apply_fixes_via_edit_ops, ApplyChunkResult, ChunkStatus, Fix, FixPolicy,
    FixResult, Insertion, InsertionKind, NewFile, PolicySummary, RefactorPrimitive, SkippedFile,
};
pub use decompose::{
    apply_plan, apply_plan_skeletons, build_plan, DecomposeAuditImpact, DecomposeGroup,
    DecomposePlan,
};
pub use move_items::{move_items, ImportRewrite, ItemKind, MoveResult, MovedItem};
pub use plan::{
    finding_fingerprint, run_audit_refactor, score_delta, weighted_finding_score_with,
    AuditConvergenceScoring, AuditRefactorIterationSummary, AuditRefactorOutcome,
};
pub use propagate::{propagate, PropagateConfig, PropagateEdit, PropagateField, PropagateResult};
pub use rename::{
    apply_renames, find_references, find_references_with_targeting, generate_renames,
    generate_renames_with_targeting, CaseVariant, FileEdit, FileRename, Reference, RenameContext,
    RenameResult, RenameScope, RenameSpec, RenameTargeting, RenameWarning,
};
pub use transform::{
    ad_hoc_transform, apply_transforms, RuleResult, TransformMatch, TransformResult, TransformRule,
    TransformSet, DEFAULT_MATCH_DETAIL_LIMIT,
};
