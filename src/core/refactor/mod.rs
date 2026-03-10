//! Structural refactoring — rename, add, move, and transform code across a codebase.
//!
//! Walks source files, finds all references to a term (with word-boundary matching
//! and case-variant awareness), generates edits, and optionally applies them.

pub mod add;
pub mod decompose;
pub mod move_items;
pub mod planner;
mod rename;
pub mod runner;
pub mod transform;

pub use add::{add_import, fixes_from_audit, AddResult};
pub use planner::{
    analyze_stage_overlaps, build_refactor_plan, normalize_sources, summarize_plan_totals,
    PlanOverlap, PlanStageSummary, PlanTotals, RefactorPlan, RefactorPlanRequest,
    KNOWN_PLAN_SOURCES,
};
pub use decompose::{
    apply_plan, apply_plan_skeletons, build_plan, DecomposeAuditImpact, DecomposeGroup,
    DecomposePlan,
};
pub use move_items::{move_items, ImportRewrite, ItemKind, MoveResult, MovedItem};
pub use rename::{
    apply_renames, find_references, find_references_with_targeting, generate_renames,
    generate_renames_with_targeting, CaseVariant, FileEdit, FileRename, Reference, RenameResult,
    RenameScope, RenameSpec, RenameTargeting, RenameWarning,
};
pub use transform::{
    ad_hoc_transform, apply_transforms, load_transform_set, TransformResult, TransformRule,
    TransformSet,
};
