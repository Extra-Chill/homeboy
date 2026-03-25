//! refactor — extracted from refactor.rs.

use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use super::super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use serde::Serialize;
use std::collections::HashSet;
use super::resolve_top_level_targets;
use super::RefactorOutput;
use super::run_across_targets;


pub(crate) fn run_refactor_sources(
    comp: Option<&PositionalComponentArgs>,
    component_ids: &[String],
    components: &[String],
    from: &[String],
    changed_since: Option<&str>,
    only: &[String],
    exclude: &[String],
    settings: &[(String, String)],
    force: bool,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = resolve_top_level_targets(comp, component_ids, components)?;
    run_across_targets("sources", targets, |component_id, path| {
        run_refactor_sources_single(
            component_id,
            path,
            from,
            changed_since,
            only,
            exclude,
            settings,
            force,
            write,
        )
    })
}

pub(crate) fn run_refactor_sources_single(
    component_id: Option<&str>,
    path: Option<&str>,
    from: &[String],
    changed_since: Option<&str>,
    only: &[String],
    exclude: &[String],
    settings: &[(String, String)],
    force: bool,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let component_id = component_id.ok_or_else(|| {
        homeboy::Error::validation_missing_argument(vec!["component".to_string()])
    })?;
    let ctx = execution_context::resolve(&ResolveOptions::source_only(
        component_id,
        path.map(str::to_string),
    ))?;
    let requested_sources = from.to_vec();
    let only_findings = parse_audit_findings(only)?;
    let exclude_findings = parse_audit_findings(exclude)?;
    let plan = homeboy::refactor::build_refactor_plan(homeboy::refactor::RefactorPlanRequest {
        component: ctx.component,
        root: ctx.source_path,
        sources: requested_sources,
        changed_since: changed_since.map(ToOwned::to_owned),
        only: only_findings,
        exclude: exclude_findings,
        settings: settings.to_vec(),
        lint: homeboy::refactor::LintSourceOptions::default(),
        test: homeboy::refactor::TestSourceOptions::default(),
        write,
        force,
    })?;
    let exit_code = if plan.files_modified > 0 { 1 } else { 0 };

    Ok((RefactorOutput::Plan(plan), exit_code))
}
