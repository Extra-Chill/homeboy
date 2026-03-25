//! across_targets — extracted from refactor.rs.

use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use std::collections::HashSet;
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::engine::execution_context::{self, ResolveOptions};
use serde::Serialize;
use super::super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use super::run_propagate;
use super::RefactorArgs;
use super::RefactorOutput;
use super::run_refactor_sources;
use super::RefactorTargetArgs;
use super::run_transform;
use super::run_rename_single;
use super::run_move;
use super::run_add;
use super::run_decompose;
use super::run_move_file;
use super::run_across_targets;
use super::RefactorCommand;


pub fn run(args: RefactorArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<RefactorOutput> {
    match args.command {
        None => run_refactor_sources(
            args.comp.as_ref(),
            &args.component_ids,
            &args.components,
            &args.from,
            args.changed_since.as_deref(),
            &args.only,
            &args.exclude,
            &args.setting_args.setting,
            args.force,
            args.write_mode.write,
        ),

        Some(RefactorCommand::Rename {
            from,
            to,
            target,
            scope,
            literal,
            files,
            exclude,
            no_file_renames,
            context,
            write_mode,
        }) => run_rename(
            &from,
            &to,
            &target,
            &scope,
            literal,
            &files,
            &exclude,
            no_file_renames,
            &context,
            write_mode.write,
        ),

        Some(RefactorCommand::Add {
            from_audit,
            import,
            to,
            target,
            write_mode,
        }) => run_add(
            from_audit.as_deref(),
            import.as_deref(),
            to.as_deref(),
            &target,
            write_mode.write,
        ),

        Some(RefactorCommand::Move {
            item,
            file,
            from,
            to,
            target,
            write_mode,
        }) => {
            if let Some(file_path) = file {
                run_move_file(&file_path, &to, &target, write_mode.write)
            } else if let Some(from_path) = from {
                if item.is_empty() {
                    return Err(homeboy::Error::validation_invalid_argument(
                        "item",
                        "Either --item (with --from) or --file is required",
                        None,
                        Some(vec![
                            "Move items: refactor move --item foo --from src/a.rs --to src/b.rs"
                                .to_string(),
                            "Move file: refactor move --file src/a.rs --to src/b.rs".to_string(),
                        ]),
                    ));
                }
                run_move(&item, &from_path, &to, &target, write_mode.write)
            } else {
                Err(homeboy::Error::validation_invalid_argument(
                    "from",
                    "Either --from (with --item) or --file is required",
                    None,
                    Some(vec![
                        "Move items: refactor move --item foo --from src/a.rs --to src/b.rs"
                            .to_string(),
                        "Move file: refactor move --file src/a.rs --to src/b.rs".to_string(),
                    ]),
                ))
            }
        }

        Some(RefactorCommand::Propagate {
            struct_name,
            definition,
            target,
            write_mode,
        }) => run_propagate(
            &struct_name,
            definition.as_deref(),
            &target,
            write_mode.write,
        ),

        Some(RefactorCommand::Transform {
            name,
            find,
            replace,
            files,
            context,
            rule,
            target,
            write_mode,
        }) => run_transform(
            name.as_deref(),
            find.as_deref(),
            replace.as_deref(),
            &files,
            &context,
            rule.as_deref(),
            &target,
            write_mode.write,
        ),

        Some(RefactorCommand::Decompose {
            file,
            strategy,
            target,
            write_mode,
        }) => run_decompose(&file, &strategy, &target, write_mode.write),
    }
}

pub(crate) fn collect_component_ids(primary: &[String], secondary: &[String]) -> Vec<String> {
    let mut seen = HashSet::new();
    primary
        .iter()
        .chain(secondary.iter())
        .filter_map(|id| {
            let trimmed = id.trim();
            if trimmed.is_empty() {
                None
            } else if seen.insert(trimmed.to_string()) {
                Some(trimmed.to_string())
            } else {
                None
            }
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_rename(
    from: &str,
    to: &str,
    target: &RefactorTargetArgs,
    scope: &str,
    literal: bool,
    include_globs: &[String],
    exclude_globs: &[String],
    no_file_renames: bool,
    context: &str,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = target.resolve_targets()?;
    run_across_targets("rename", targets, |component_id, path| {
        run_rename_single(
            from,
            to,
            component_id,
            path,
            scope,
            literal,
            include_globs,
            exclude_globs,
            no_file_renames,
            context,
            write,
        )
    })
}
