//! move — extracted from refactor.rs.

use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::engine::execution_context::{self, ResolveOptions};
use serde::Serialize;
use std::collections::HashSet;
use super::super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use super::run_move_single;
use super::RefactorOutput;
use super::run_across_targets;
use super::RefactorTargetArgs;
use super::run_move_file_single;


pub(crate) fn run_move(
    items: &[String],
    from: &str,
    to: &str,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = target.resolve_targets()?;
    run_across_targets("move", targets, |component_id, path| {
        run_move_single(items, from, to, component_id, path, write)
    })
}

pub(crate) fn run_move_file(
    file: &str,
    to: &str,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = target.resolve_targets()?;
    run_across_targets("move_file", targets, |component_id, path| {
        run_move_file_single(file, to, component_id, path, write)
    })
}
