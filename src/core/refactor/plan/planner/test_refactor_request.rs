//! test_refactor_request — extracted from planner.rs.

use crate::component::Component;
use std::path::{Path, PathBuf};
use crate::code_audit::CodeAuditResult;
use crate::engine::run_dir::{self, RunDir};
use crate::engine::undo::UndoSnapshot;
use crate::refactor::auto::{self, FixApplied, FixResultsSummary};
use crate::Error;
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use super::super::verify::AuditConvergenceScoring;
use super::LintSourceOptions;
use super::RefactorPlan;
use super::RefactorPlanRequest;
use super::TestSourceOptions;


pub fn test_refactor_request(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: TestSourceOptions,
    write: bool,
) -> RefactorPlanRequest {
    RefactorPlanRequest {
        component,
        root,
        sources: vec!["test".to_string()],
        changed_since: None,
        only: Vec::new(),
        exclude: Vec::new(),
        settings,
        lint: LintSourceOptions::default(),
        test: options,
        write,
        force: false,
    }
}

pub fn run_test_refactor(
    component: Component,
    root: PathBuf,
    settings: Vec<(String, String)>,
    options: TestSourceOptions,
    write: bool,
) -> crate::Result<RefactorPlan> {
    build_refactor_plan(test_refactor_request(
        component, root, settings, options, write,
    ))
}
