//! Tests for the lint run workflow, grouped by concern.

use super::types::{LintRunWorkflowArgs, LintSniffFilters};
use crate::core::component::Component;
use crate::core::engine::baseline::BaselineFlags;
use crate::core::extension::LintChangedFileRoute;
use crate::core::finding::HomeboyFinding;

mod exit_code;
mod findings;
mod formatting;
mod hints;
mod scoping;
mod workflow;

pub(super) fn component(root: &str) -> Component {
    Component::new(
        "fixture".to_string(),
        root.to_string(),
        "".to_string(),
        None,
    )
}

pub(super) fn split_lint_routes() -> Vec<LintChangedFileRoute> {
    vec![
        LintChangedFileRoute {
            extensions: vec!["php".to_string()],
            globs: Vec::new(),
            step: "phpcs,phpstan".to_string(),
        },
        LintChangedFileRoute {
            extensions: vec![
                "js".to_string(),
                "jsx".to_string(),
                "ts".to_string(),
                "tsx".to_string(),
            ],
            globs: Vec::new(),
            step: "eslint".to_string(),
        },
    ]
}

pub(super) fn lint_args() -> LintRunWorkflowArgs {
    LintRunWorkflowArgs {
        component_label: "demo".to_string(),
        component_id: "demo".to_string(),
        path_override: None,
        settings: Vec::new(),
        summary: false,
        file: None,
        glob: None,
        changed_only: false,
        changed_since: None,
        precomputed_changed_files: None,
        sniff_filters: LintSniffFilters::default(),
        category: None,
        ci_env: Vec::new(),
        baseline_flags: BaselineFlags::default(),
        json_summary: false,
    }
}

pub(super) fn lint_finding(id: &str, category: &str, rule: &str) -> HomeboyFinding {
    HomeboyFinding::builder("lint", "message")
        .category(category)
        .rule(rule)
        .fingerprint(id)
        .build()
}
