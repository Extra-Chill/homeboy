//! transform — extracted from refactor.rs.

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
use super::RefactorTargetArgs;
use super::RefactorOutput;


#[allow(clippy::too_many_arguments)]
pub(crate) fn run_transform(
    name: Option<&str>,
    find: Option<&str>,
    replace: Option<&str>,
    files: &str,
    context: &str,
    rule_filter: Option<&str>,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = target.resolve_targets()?;
    run_across_targets("transform", targets, |component_id, path| {
        run_transform_single(
            name,
            find,
            replace,
            files,
            context,
            rule_filter,
            component_id,
            path,
            write,
        )
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_transform_single(
    name: Option<&str>,
    find: Option<&str>,
    replace: Option<&str>,
    files: &str,
    context: &str,
    rule_filter: Option<&str>,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;

    // Resolve transform set: ad-hoc or named
    let (set_name, set) = if let (Some(f), Some(r)) = (find, replace) {
        // Ad-hoc mode
        if name.is_some() {
            return Err(homeboy::Error::validation_invalid_argument(
                "name",
                "Cannot use both a named transform and --find/--replace",
                None,
                None,
            ));
        }
        (
            "ad-hoc".to_string(),
            refactor::ad_hoc_transform(f, r, files, context),
        )
    } else if let Some(n) = name {
        // Named mode — load from homeboy.json
        let set = refactor::load_transform_set(&root, n)?;
        (n.to_string(), set)
    } else {
        return Err(homeboy::Error::validation_missing_argument(vec![
            "name".to_string(),
            "--find/--replace".to_string(),
        ]));
    };

    // Report what we're about to do
    homeboy::log_status!(
        "transform",
        "{} ({} rule{})",
        set_name,
        set.rules.len(),
        if set.rules.len() == 1 { "" } else { "s" }
    );

    if !set.description.is_empty() {
        homeboy::log_status!("info", "{}", set.description);
    }

    if write {
        // Dry-run to discover affected files for the undo snapshot
        if let Ok(preview) = refactor::apply_transforms(&root, &set_name, &set, false, rule_filter)
        {
            let affected_files: std::collections::HashSet<String> = preview
                .rules
                .iter()
                .flat_map(|r| r.matches.iter().map(|m| m.file.clone()))
                .collect();
            homeboy::engine::undo::UndoSnapshot::capture_and_save(
                &root,
                "refactor transform",
                &affected_files,
            );
        }
    }

    // Apply transforms
    let result = refactor::apply_transforms(&root, &set_name, &set, write, rule_filter)?;

    // Report results to stderr
    for rule_result in &result.rules {
        if rule_result.matches.is_empty() {
            homeboy::log_status!("skip", "{}: no matches", rule_result.id);
            continue;
        }

        homeboy::log_status!(
            "rule",
            "{}: {} replacement{}",
            rule_result.id,
            rule_result.replacement_count,
            if rule_result.replacement_count == 1 {
                ""
            } else {
                "s"
            }
        );

        for m in &rule_result.matches {
            homeboy::log_status!("  match", "{}:{}", m.file, m.line);
            if !m.before.is_empty() {
                homeboy::log_status!("  -", "{}", m.before.trim());
                homeboy::log_status!("  +", "{}", m.after.trim());
            }
        }
    }

    // Summary
    if result.total_replacements == 0 {
        homeboy::log_status!("result", "No matches found");
    } else if write {
        homeboy::log_status!(
            "result",
            "{} replacement{} applied across {} file{}",
            result.total_replacements,
            if result.total_replacements == 1 {
                ""
            } else {
                "s"
            },
            result.total_files,
            if result.total_files == 1 { "" } else { "s" },
        );
    } else {
        homeboy::log_status!(
            "result",
            "{} replacement{} across {} file{} (dry-run, use --write to apply)",
            result.total_replacements,
            if result.total_replacements == 1 {
                ""
            } else {
                "s"
            },
            result.total_files,
            if result.total_files == 1 { "" } else { "s" },
        );
    }

    let exit_code = if result.total_replacements == 0 { 1 } else { 0 };
    Ok((RefactorOutput::Transform { result }, exit_code))
}

pub(crate) fn run_decompose(
    file: &str,
    strategy: &str,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let targets = target.resolve_targets()?;
    run_across_targets("decompose", targets, |component_id, path| {
        run_decompose_single(file, strategy, component_id, path, write)
    })
}

pub(crate) fn run_decompose_single(
    file: &str,
    strategy: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;
    let plan = refactor::build_plan(file, &root, strategy)?;

    if write {
        let affected: Vec<&str> = std::iter::once(file)
            .chain(plan.groups.iter().map(|g| g.suggested_target.as_str()))
            .collect();
        homeboy::engine::undo::UndoSnapshot::capture_and_save(
            &root,
            "refactor decompose",
            &affected,
        );
    }

    let move_results = refactor::apply_plan(&plan, &root, write)?;
    let groups_applied = move_results
        .iter()
        .filter(|result| !result.items_moved.is_empty())
        .count();

    homeboy::log_status!(
        "decompose",
        "{} group(s) planned for {}{}",
        plan.groups.len(),
        file,
        if write { " (applied)" } else { " (dry run)" }
    );

    for group in &plan.groups {
        homeboy::log_status!(
            "decompose",
            "{} -> {} ({} item(s))",
            group.name,
            group.suggested_target,
            group.item_names.len()
        );
    }

    if !plan.warnings.is_empty() {
        for warning in &plan.warnings {
            homeboy::log_status!("warning", "{}", warning);
        }
    }

    if !plan.projected_audit_impact.likely_findings.is_empty() {
        for finding in &plan.projected_audit_impact.likely_findings {
            homeboy::log_status!("impact", "{}", finding);
        }
    }

    homeboy::log_status!(
        "decompose",
        "{} move group(s) {}",
        groups_applied,
        if write { "applied" } else { "planned" }
    );

    Ok((
        RefactorOutput::Decompose {
            plan,
            move_results,
            dry_run: !write,
            applied: write,
        },
        0,
    ))
}
