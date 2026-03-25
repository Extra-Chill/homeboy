//! refactor_target_args — extracted from refactor.rs.

use clap::{Args, Subcommand};
use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::engine::execution_context::{self, ResolveOptions};
use serde::Serialize;
use std::collections::HashSet;
use super::super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use crate::commands::CmdResult;


#[derive(Args, Debug, Clone, Default)]
pub(crate) struct RefactorTargetArgs {
    /// Target a component by ID (repeatable)
    #[arg(short, long = "component", value_name = "ID", action = clap::ArgAction::Append)]
    component_ids: Vec<String>,

    /// Target multiple components with a comma-separated list
    #[arg(long, value_name = "ID[,ID...]", value_delimiter = ',')]
    components: Vec<String>,

    /// Override the source root for a single target
    #[arg(long)]
    path: Option<String>,
}
