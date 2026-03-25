mod refactor_target_args;
mod rename_single;
mod types;

pub use refactor_target_args::*;
pub use rename_single::*;
pub use types::*;

use clap::{Args, Subcommand};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use serde::Serialize;
use std::collections::HashSet;

use super::utils::args::{BaselineArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs};
use crate::commands::CmdResult;

impl RefactorTargetArgs {
    fn resolve_targets(&self) -> homeboy::Result<Vec<RefactorTarget>> {
        let component_ids = collect_component_ids(&self.component_ids, &self.components);
        if self.path.is_some() && !component_ids.is_empty() {
            return Err(homeboy::Error::validation_invalid_argument(
                "component",
                "--path cannot be combined with multiple component IDs",
                None,
                Some(vec![
                    "Use --path for one target only".to_string(),
                    "Use --component/--components for multi-component refactors".to_string(),
                ]),
            ));
        }

        if let Some(path) = &self.path {
            return Ok(vec![RefactorTarget {
                component_id: None,
                path: Some(path.clone()),
                label: path.clone(),
            }]);
        }

        if component_ids.is_empty() {
            return Err(homeboy::Error::validation_missing_argument(vec![
                "component".to_string(),
            ]));
        }

        Ok(component_ids
            .into_iter()
            .map(|id| RefactorTarget {
                label: id.clone(),
                component_id: Some(id),
                path: None,
            })
            .collect())
    }
}

fn run_across_targets<F>(
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

fn run_add_import(
    import_line: &str,
    target: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let result = refactor::add_import(import_line, target, component_id, path, write)?;

    let exit_code = if result.total_insertions > 0 { 1 } else { 0 };

    homeboy::log_status!(
        "refactor",
        "{} file(s) to update with '{}'{}",
        result.total_insertions,
        import_line,
        if write {
            format!(" — {} written", result.files_modified)
        } else {
            " (dry run)".to_string()
        }
    );

    Ok((
        RefactorOutput::AddImport {
            import: import_line.to_string(),
            target: target.to_string(),
            result,
            dry_run: !write,
        },
        exit_code,
    ))
}

fn run_move(
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

fn run_move_single(
    items: &[String],
    from: &str,
    to: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;

    if write {
        homeboy::engine::undo::UndoSnapshot::capture_and_save(&root, "refactor move", [from, to]);
    }

    let item_refs: Vec<&str> = items.iter().map(|s| s.as_str()).collect();
    let result = refactor::move_items(&item_refs, from, to, &root, write)?;

    let exit_code = if result.items_moved.is_empty() { 1 } else { 0 };

    homeboy::log_status!(
        "refactor",
        "{} item(s) from {} → {}{}",
        result.items_moved.len(),
        from,
        to,
        if write {
            " (applied)".to_string()
        } else {
            " (dry run)".to_string()
        }
    );

    for item in &result.items_moved {
        homeboy::log_status!(
            "move",
            "{} {:?} (lines {}-{})",
            item.name,
            item.kind,
            item.source_lines.0,
            item.source_lines.1
        );
    }

    for test in &result.tests_moved {
        homeboy::log_status!(
            "move",
            "test {} (lines {}-{})",
            test.name,
            test.source_lines.0,
            test.source_lines.1
        );
    }

    if result.imports_updated > 0 {
        homeboy::log_status!(
            "move",
            "{} import reference(s) updated across codebase",
            result.imports_updated
        );
    }

    for warning in &result.warnings {
        homeboy::log_status!("warning", "{}", warning);
    }

    Ok((RefactorOutput::Move { result }, exit_code))
}

fn run_move_file(
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

fn run_move_file_single(
    file: &str,
    to: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;

    if write {
        homeboy::engine::undo::UndoSnapshot::capture_and_save(
            &root,
            "refactor move --file",
            [file, to],
        );
    }

    let result = refactor::move_items::move_file(file, to, &root, write)?;

    let exit_code = if result.imports_updated > 0 || result.mod_declarations_updated {
        0
    } else {
        1
    };

    homeboy::log_status!(
        "refactor",
        "move {} → {}{}",
        file,
        to,
        if write { " (applied)" } else { " (dry run)" }
    );
    homeboy::log_status!(
        "move",
        "{} import(s) rewritten across {} file(s)",
        result.imports_updated,
        result.caller_files_modified.len()
    );
    if result.mod_declarations_updated {
        homeboy::log_status!("move", "mod.rs declarations updated");
    }
    for warning in &result.warnings {
        homeboy::log_status!("warning", "{}", warning);
    }

    Ok((RefactorOutput::MoveFile { result }, exit_code))
}

// ============================================================================
// Propagate (add missing fields to struct instantiations)
// ============================================================================

fn run_propagate(
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

fn run_propagate_single(
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

// ============================================================================
// Transform
// ============================================================================

#[allow(clippy::too_many_arguments)]
fn run_transform(
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
fn run_transform_single(
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

fn run_decompose(
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

fn run_decompose_single(
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collect_component_ids_dedupes_and_trims() {
        let ids = collect_component_ids(
            &["alpha".to_string(), " beta ".to_string()],
            &["beta".to_string(), "gamma".to_string(), "".to_string()],
        );

        assert_eq!(ids, vec!["alpha", "beta", "gamma"]);
    }

    #[test]
    fn target_args_reject_path_with_multiple_components() {
        let args = RefactorTargetArgs {
            component_ids: vec!["alpha".to_string(), "beta".to_string()],
            components: vec![],
            path: Some("/tmp/example".to_string()),
        };

        let error = args.resolve_targets().unwrap_err();
        assert!(
            error.to_string().contains("--path cannot be combined"),
            "unexpected error: {}",
            error
        );
    }

    #[test]
    fn target_args_build_multi_component_targets() {
        let args = RefactorTargetArgs {
            component_ids: vec!["alpha".to_string()],
            components: vec!["beta".to_string(), "alpha".to_string()],
            path: None,
        };

        let targets = args.resolve_targets().unwrap();
        let labels: Vec<_> = targets.into_iter().map(|target| target.label).collect();
        assert_eq!(labels, vec!["alpha", "beta"]);
    }
}
