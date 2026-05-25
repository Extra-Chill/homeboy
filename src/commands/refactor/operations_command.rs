use std::path::Path;

use homeboy::core::code_audit::CodeAuditResult;
use homeboy::core::refactor;

use super::{run_across_targets, RefactorOutput, RefactorTargetArgs};
use crate::commands::CmdResult;

pub(super) fn run_add(
    from_audit: Option<&str>,
    import: Option<&str>,
    to: Option<&str>,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    if let Some(audit_source) = from_audit {
        return run_add_from_audit(audit_source, write);
    }

    if let Some(import_line) = import {
        let destination = to.ok_or_else(|| {
            homeboy::core::Error::validation_invalid_argument(
                "to",
                "--to is required when using --import",
                None,
                Some(vec![
                    "homeboy refactor add --import \"use serde::Serialize;\" --to \"src/**/*.rs\""
                        .to_string(),
                ]),
            )
        })?;

        let targets = target.resolve_targets()?;
        return run_across_targets("add", targets, |component_id, path| {
            run_add_import(import_line, destination, component_id, path, write)
        });
    }

    Err(homeboy::core::Error::validation_invalid_argument(
        "add",
        "Specify either --from-audit or --import with --to",
        None,
        Some(vec![
            "homeboy refactor add --from-audit @audit.json".to_string(),
            "homeboy refactor add --import \"use serde::Serialize;\" --to \"src/**/*.rs\""
                .to_string(),
        ]),
    ))
}

pub(super) fn run_move(
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

pub(super) fn run_move_file(
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

pub(super) fn run_propagate(
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

pub(super) fn run_decompose(
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

fn run_add_from_audit(source: &str, write: bool) -> CmdResult<RefactorOutput> {
    let effective_source = if !source.starts_with('{')
        && !source.starts_with('[')
        && source != "-"
        && !source.starts_with('@')
        && Path::new(source).exists()
    {
        format!("@{}", source)
    } else {
        source.to_string()
    };

    let json_content = crate::commands::merge_json_sources(Some(&effective_source), &[])?;
    let audit: CodeAuditResult = if let Some(data) = json_content.get("data") {
        serde_json::from_value(data.clone())
    } else {
        serde_json::from_value(json_content)
    }
    .map_err(|e| {
        homeboy::core::Error::validation_invalid_json(
            e,
            Some("parse audit result for refactor add".to_string()),
            Some(
                "Input must be output from `homeboy audit <component>`. \
                 Save it with: homeboy --format json audit <component> > audit.json"
                    .to_string(),
            ),
        )
    })?;

    let fix_result = refactor::fixes_from_audit(&audit, write)?;
    let exit_code = if fix_result.total_insertions > 0 {
        1
    } else {
        0
    };

    homeboy::log_status!(
        "refactor",
        "{} fix(es) across {} file(s){}",
        fix_result.total_insertions,
        fix_result.fixes.len(),
        if write {
            format!(" — {} written", fix_result.files_modified)
        } else {
            " (dry run)".to_string()
        }
    );

    Ok((
        RefactorOutput::AddFromAudit {
            source_path: audit.source_path,
            fix_result,
            dry_run: !write,
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
        homeboy::core::engine::undo::UndoSnapshot::capture_and_save(
            &root,
            "refactor move",
            [from, to],
        );
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
        if write { " (applied)" } else { " (dry run)" }
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

fn run_move_file_single(
    file: &str,
    to: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;

    if write {
        homeboy::core::engine::undo::UndoSnapshot::capture_and_save(
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

fn run_propagate_single(
    struct_name: &str,
    definition_file: Option<&str>,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;
    let config = refactor::PropagateConfig {
        struct_name,
        definition_file,
        root: &root,
        write: false,
    };

    if write {
        let preview = refactor::propagate(&config)?;
        let affected_files: Vec<&str> = preview.edits.iter().map(|e| e.file.as_str()).collect();
        homeboy::core::engine::undo::UndoSnapshot::capture_and_save(
            &root,
            "refactor propagate",
            affected_files,
        );
    }

    let write_config = refactor::PropagateConfig {
        struct_name,
        definition_file,
        root: &root,
        write,
    };
    let result = refactor::propagate(&write_config)?;

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
        homeboy::core::engine::undo::UndoSnapshot::capture_and_save(
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

    for warning in &plan.warnings {
        homeboy::log_status!("warning", "{}", warning);
    }

    for finding in &plan.projected_audit_impact.likely_findings {
        homeboy::log_status!("impact", "{}", finding);
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
