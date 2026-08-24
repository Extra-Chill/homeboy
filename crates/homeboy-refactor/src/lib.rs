//! Structural refactoring — rename, add, move, and transform code across a codebase.
//!
//! Walks source files, finds all references to a term (with word-boundary matching
//! and case-variant awareness), generates edits, and optionally applies them.

use std::path::PathBuf;

mod add;
mod auto;
mod collapse;
mod decompose;
mod definition;
mod edit_op_tagged;
mod move_items;
mod plan;
mod primitive_builders;
mod propagate;
mod rename;
mod transform;

// Provider-registration seams. Each module exposes exactly one `register()`
// entry point that `homeboy-cli` calls during runtime wiring, so publishing the
// module costs no dead-code visibility — the single public item is the one that
// crosses the boundary.
pub mod audit_fixability_provider;
pub mod transform_provider;

/// Resolve the refactor root directory from an explicit path or component id.
/// Every `homeboy refactor` subcommand resolves its target through this first.
pub fn resolve_root(
    component_id: Option<&str>,
    path: Option<&str>,
) -> homeboy_core::Result<PathBuf> {
    let target = homeboy_core::component::resolve_target(
        homeboy_core::component::TargetSpec::new(component_id, path),
    )?;
    if !target.source_path.is_dir() {
        return Err(homeboy_core::Error::validation_invalid_argument(
            "path",
            format!("Not a directory: {}", target.source_path.display()),
            None,
            None,
        ));
    }

    Ok(target.source_path)
}

// ---------------------------------------------------------------------------
// Public API — the `homeboy refactor`, `homeboy refs`, `homeboy lint`,
// `homeboy test`, and `homeboy ci` command surfaces are the only consumers of
// this engine. Every item below is re-exported because a command in
// `homeboy-cli` names it directly; everything else stays crate-private so
// rustc can see whether it is reachable at all.
// ---------------------------------------------------------------------------

// `refactor add` — both its --from-audit and --import forms.
pub use add::{add_import, fixes_from_audit, AddResult};
// `refactor autofix` reports per-fix outcomes in its JSON payload. The rest of
// this group is reachable through `FixResult`'s public fields, so it is part of
// the boundary whether or not a command names it directly.
pub use auto::{
    ApplyChunkResult, ChunkStatus, DecomposeFixPlan, Fix, FixResult, Insertion, InsertionKind,
    NewFile, RefactorPrimitive, SkippedFile,
};
// `ci autofix` drives the commit/push transaction, so the whole request and
// outcome cluster crosses the boundary.
pub use auto::{
    run_autofix_transaction, CiContext, TransactionOutcome, TransactionRequest,
    AUTOFIX_COMMIT_PREFIX,
};
// `TransactionOutcome::route` exposes the resolved push route.
pub use auto::PushRoute;
// `refactor collapse`. `CollapseEdit` is reachable through `CollapseResult::edits`.
pub use collapse::{collapse, CollapseConfig, CollapseEdit, CollapseResult};
// `refactor decompose` previews a plan, then applies it.
pub use decompose::{
    apply_plan, apply_plan_skeletons, build_plan, DecomposeAuditImpact, DecomposeGroup,
    DecomposePlan,
};
// `refactor move` moves items between files and whole files between paths.
// `MovedItem` is reachable through `MoveResult::items_moved`/`tests_moved`.
// `ItemKind`/`MovedItem` are reachable through `MoveResult::items_moved`.
pub use move_items::{move_file, move_items, ItemKind, MoveFileResult, MoveResult, MovedItem};
// `lint`, `test`, and `refactor sources` all collect refactor sources through
// this planning layer and serialize the resulting run into their reports.
pub use plan::{
    build_test_refactor_request, collect_refactor_sources, lint_refactor_request,
    LintSourceOptions, RefactorSourceRequest, RefactorSourceRun, SourceTotals, TestSourceOptions,
};
// Reachable through `RefactorSourceRun`'s public fields.
pub use auto::GuardBlock;
pub use plan::{CollectedEdit, SourceOverlap, SourceStageSummary};
// `refactor propagate`. The edit/field pair is reachable through
// `PropagateResult`'s public fields.
pub use propagate::{propagate, PropagateConfig, PropagateEdit, PropagateField, PropagateResult};
// `refactor rename` and `refs` share the targeting vocabulary; `RenameResult`
// and `Reference` are the values those entry points return, and `CaseVariant`
// is reachable through `RenameSpec::variants`.
pub use rename::{
    apply_renames, find_references_with_targeting, generate_renames_with_targeting, CaseVariant,
    FileEdit, FileRename, Reference, RenameContext, RenameResult, RenameScope, RenameSpec,
    RenameTargeting, RenameWarning,
};
// `refactor transform`.
// `refactor transform`. `TransformMatch` is reachable through `RuleResult::matches`.
pub use transform::{
    ad_hoc_transform, apply_transforms, RuleResult, TransformMatch, TransformResult,
    DEFAULT_MATCH_DETAIL_LIMIT,
};
