//! Lint workflow orchestration — runs lint, resolves changed-file scoping,
//! drives autofix, processes baseline lifecycle, and assembles results.
//!
//! Mirrors `core/extension/test/run.rs` — the command layer provides CLI args,
//! this module owns all business logic and returns a structured result.

mod exit_code;
mod findings;
mod formatting;
mod hints;
mod scoping;
mod types;
mod workflow;

#[cfg(test)]
mod tests;

pub use types::{
    FormattingFindings, LintRunWorkflowArgs, LintRunWorkflowResult, LintSniffFilters,
    LintSummaryOutput,
};
pub use workflow::{
    run_main_lint_workflow, run_self_check_lint_workflow,
    run_self_check_lint_workflow_with_progress,
};
