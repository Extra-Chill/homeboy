//! lint_refactor_request — extracted from planner.rs.

use crate::component::Component;
use std::path::{Path, PathBuf};
use crate::component::Component;
use super::build_refactor_plan;
use super::TestSourceOptions;
use super::RefactorPlanRequest;
use super::RefactorPlan;
use super::LintSourceOptions;


pub fn lint_refactor_request(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> RefactorPlanRequest {
    RefactorPlanRequest {
        component,
        root,
        sources: vec!["lint".to_string()],
        changed_since: None,
        only: Vec::new(),
        exclude: Vec::new(),
        settings,
        lint: options,
        test: TestSourceOptions::default(),
        write,
    }
}

pub(crate) fn run_lint_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: LintSourceOptions,
    write: bool,
) -> crate::Result<RefactorPlan> {
    build_refactor_plan(lint_refactor_request(
        component, root, settings, options, write,
    ))
}
