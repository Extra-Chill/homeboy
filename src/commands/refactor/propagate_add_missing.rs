//! propagate_add_missing — extracted from refactor.rs.

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
use super::RefactorOutput;
use super::run_across_targets;
use super::RefactorTargetArgs;


pub(crate) fn run_propagate(
    struct_name: &str,
    definition_file: Option<&str>,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = target.resolve_targets()?;
    run_across_targets("propagate", targets, |component_id, path| {
        run_propagate_single(struct_name, definition_file, component_id, path, write)
    })
}

pub(crate) fn run_propagate_single(
    struct_name: &str,
    definition_file: Option<&str>,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;

    // Capture undo snapshot before writes
    let config = refactor::PropagateConfig {
        struct_name,
        definition_file,
        root: &root,
        write: false, // dry-run first if we need undo
    };

    if write {
        // Dry-run to discover affected files for the undo snapshot
        let preview = refactor::propagate(&config)?;
        let affected_files: Vec<&str> = preview.edits.iter().map(|e| e.file.as_str()).collect();
        homeboy::engine::undo::UndoSnapshot::capture_and_save(
            &root,
            "refactor propagate",
            affected_files,
        );
    }

    // Run the actual propagation (with write mode as requested)
    let write_config = refactor::PropagateConfig {
        struct_name,
        definition_file,
        root: &root,
        write,
    };
    let result = refactor::propagate(&write_config)?;

    // Log results to stderr
    homeboy::log_status!(
        "propagate",
        "{} instantiation(s) found, {} need fixes, {} edit(s){}",
        result.instantiations_found,
        result.instantiations_needing_fix,
        result.edits.len(),
        if write {
            if result.applied {
                " (applied)".to_string()
            } else {
                " (nothing to apply)".to_string()
            }
        } else {
            " (dry run)".to_string()
        }
    );

    for edit in &result.edits {
        homeboy::log_status!("edit", "{}:{} — {}", edit.file, edit.line, edit.description);
    }

    let exit_code = if result.edits.is_empty() { 0 } else { 1 };

    Ok((
        RefactorOutput::Propagate {
            result,
            dry_run: !write,
        },
        exit_code,
    ))
}
