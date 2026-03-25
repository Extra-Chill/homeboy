//! targets — extracted from refactor.rs.

use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use super::super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::engine::execution_context::{self, ResolveOptions};
use serde::Serialize;
use std::collections::HashSet;
use super::RefactorBulkSummary;
use super::RefactorBulkItem;
use super::RefactorTarget;
use super::RefactorOutput;


pub(crate) fn resolve_top_level_targets(
    comp: Option<&PositionalComponentArgs>,
    component_ids: &[String],
    components: &[String],
) -> homeboy::Result<Vec<RefactorTarget>> {
    let flagged_ids = collect_component_ids(component_ids, components);

    if let Some(comp) = comp {
        if !flagged_ids.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "component",
                "Use either positional component syntax or --component/--components, not both",
                None,
                None,
            ));
        }

        return Ok(vec![RefactorTarget {
            component_id: Some(comp.component.clone()),
            path: comp.path.clone(),
            label: comp.component.clone(),
        }]);
    }

    if flagged_ids.is_empty() {
        return Err(homeboy::Error::validation_missing_argument(vec![
            "component".to_string(),
        ]));
    }

    Ok(flagged_ids
        .into_iter()
        .map(|id| RefactorTarget {
            label: id.clone(),
            component_id: Some(id),
            path: None,
        })
        .collect())
}

pub(crate) fn run_across_targets<F>(
    action: &str,
    targets: Vec<RefactorTarget>,
    mut run_single: F,
) -> CmdResult<RefactorOutput>
where
    F: FnMut(Option<&str>, Option<&str>) -> CmdResult<RefactorOutput>,
{
    if targets.len() == 1 {
        let target = &targets[0];
        return run_single(target.component_id.as_deref(), target.path.as_deref());
    }

    let mut results = Vec::with_capacity(targets.len());
    let mut succeeded = 0usize;
    let mut failed = 0usize;
    let mut any_zero_exit = false;

    for target in targets {
        match run_single(target.component_id.as_deref(), target.path.as_deref()) {
            Ok((output, exit_code)) => {
                if exit_code == 0 {
                    any_zero_exit = true;
                }
                succeeded += 1;
                results.push(RefactorBulkItem {
                    id: target.label,
                    result: Some(Box::new(output)),
                    error: None,
                });
            }
            Err(error) => {
                failed += 1;
                results.push(RefactorBulkItem {
                    id: target.label,
                    result: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }

    let exit_code = if failed > 0 || !any_zero_exit { 1 } else { 0 };

    Ok((
        RefactorOutput::Bulk {
            action: action.to_string(),
            results,
            summary: RefactorBulkSummary {
                total: succeeded + failed,
                succeeded,
                failed,
            },
        },
        exit_code,
    ))
}
