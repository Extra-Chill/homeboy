//! Structural refactoring — rename, add, move, and transform code across a codebase.
//!
//! Walks source files, finds all references to a term (with word-boundary matching
//! and case-variant awareness), generates edits, and optionally applies them.

use crate::refactor::auto::{AppliedAutofixCapture, FixResultsSummary};
use serde::Serialize;

pub mod add;
pub mod auto;
pub mod decompose;
pub mod move_items;
pub mod plan;
pub mod propagate;
mod rename;
pub mod transform;

/// Shared output for refactors/fixes.
///
/// `refactor --from lint/test/audit --write` are the entrypoints for fixes.
/// Keep the applied-change reporting in refactor so commands don't invent
/// parallel output models.
#[derive(Debug, Clone, Serialize)]
pub struct AppliedRefactor {
    pub files_modified: usize,
    pub rerun_recommended: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
}

impl AppliedRefactor {
    pub fn from_capture(
        capture: AppliedAutofixCapture,
        rerun_recommended: bool,
        changed_files: Vec<String>,
    ) -> Self {
        Self {
            files_modified: capture.files_modified,
            rerun_recommended,
            changed_files,
            fix_summary: capture.fix_summary,
        }
    }

}

pub use add::{add_import, fixes_from_audit, AddResult};
pub use auto::{
    apply_decompose_plans, apply_fix_policy, apply_fixes, apply_fixes_chunked,
    apply_new_files_chunked, auto_apply_subset, ApplyChunkResult, ApplyOptions, ChunkStatus, Fix,
    FixPolicy, FixResult, Insertion, InsertionKind, NewFile, PolicySummary, RefactorPrimitive,
    SkippedFile,
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
    ad_hoc_transform, apply_transforms, load_transform_set, TransformResult, TransformRule,
    TransformSet,
};
