pub(crate) mod file_intent;
pub(crate) mod generate;
pub(crate) mod sources;
pub(crate) mod verify;

pub(crate) use generate::generate_audit_fixes;
// `lint`, `test`, and `refactor sources` build and run refactor source requests.
pub use sources::{
    build_test_refactor_request, collect_refactor_sources, lint_refactor_request,
    LintSourceOptions, RefactorSourceRequest, RefactorSourceRun, SourceTotals, TestSourceOptions,
};
// Reachable through `RefactorSourceRun`'s public fields.
pub use sources::{CollectedEdit, SourceOverlap, SourceStageSummary};
