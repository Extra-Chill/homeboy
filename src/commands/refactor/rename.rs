//! rename — extracted from refactor.rs.

use super::run_add_import;
use super::run_decompose;
use super::run_move;
use super::run_move_file;
use super::run_propagate;
use super::run_transform;
use super::EditSummary;
use super::RefactorArgs;
use super::RefactorCommand;
use super::RefactorOutput;
use super::RenameSummary;
use super::VariantSummary;
use super::WarningSummary;
use crate::commands::CmdResult;
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::engine::execution_context::{self, ResolveOptions};
use serde::Serialize;

pub fn run(args: RefactorArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<RefactorOutput> {
    match args.command {
        None => run_refactor_sources(
            args.comp.as_ref(),
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
            component,
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
            component.component.as_deref(),
            component.path.as_deref(),
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
            component,
            write_mode,
        }) => run_add(
            from_audit.as_deref(),
            import.as_deref(),
            to.as_deref(),
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),

        Some(RefactorCommand::Move {
            item,
            file,
            from,
            to,
            component,
            write_mode,
        }) => {
            if let Some(file_path) = file {
                // File mode: move entire module
                run_move_file(
                    &file_path,
                    &to,
                    component.component.as_deref(),
                    component.path.as_deref(),
                    write_mode.write,
                )
            } else if let Some(from_path) = from {
                // Item mode: move specific items
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
                run_move(
                    &item,
                    &from_path,
                    &to,
                    component.component.as_deref(),
                    component.path.as_deref(),
                    write_mode.write,
                )
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
            component,
            write_mode,
        }) => run_propagate(
            &struct_name,
            definition.as_deref(),
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),

        Some(RefactorCommand::Transform {
            name,
            find,
            replace,
            files,
            context,
            rule,
            component,
            write_mode,
        }) => run_transform(
            name.as_deref(),
            find.as_deref(),
            replace.as_deref(),
            &files,
            &context,
            rule.as_deref(),
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),

        Some(RefactorCommand::Decompose {
            file,
            strategy,
            component,
            write_mode,
        }) => run_decompose(
            &file,
            &strategy,
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),
    }
}

pub(crate) fn run_refactor_sources(
    comp: Option<&PositionalComponentArgs>,
    from: &[String],
    changed_since: Option<&str>,
    only: &[String],
    exclude: &[String],
    settings: &[(String, String)],
    force: bool,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let comp = comp.ok_or_else(|| {
        homeboy::Error::validation_missing_argument(vec!["component".to_string()])
    })?;
    let ctx =
        execution_context::resolve(&ResolveOptions::source_only(comp.id(), comp.path.clone()))?;
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

pub(crate) fn parse_audit_findings(values: &[String]) -> homeboy::Result<Vec<AuditFinding>> {
    values
        .iter()
        .map(|value| {
            value.parse::<AuditFinding>().map_err(|_| {
                homeboy::Error::validation_invalid_argument(
                    "kind",
                    format!("Unknown audit finding kind: {}", value),
                    None,
                    None,
                )
            })
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn run_rename(
    from: &str,
    to: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    scope: &str,
    literal: bool,
    include_globs: &[String],
    exclude_globs: &[String],
    no_file_renames: bool,
    context: &str,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let scope = RenameScope::from_str(scope)?;
    let rename_context = RenameContext::from_str(context)?;

    let root = refactor::move_items::resolve_root(component_id, path)?;

    let mut spec = if literal {
        RenameSpec::literal(from, to, scope.clone())
    } else {
        RenameSpec::new(from, to, scope.clone())
    };
    spec.rename_context = rename_context;
    let targeting = RenameTargeting {
        include_globs: include_globs.to_vec(),
        exclude_globs: exclude_globs.to_vec(),
        rename_files: !no_file_renames,
    };
    let mut result = refactor::generate_renames_with_targeting(&spec, &root, &targeting);

    // Print warnings to stderr before applying
    for warning in &result.warnings {
        let location = warning
            .line
            .map(|l| format!("{}:{}", warning.file, l))
            .unwrap_or_else(|| warning.file.clone());
        homeboy::log_status!("warning", "{}: {}", location, warning.message);
    }

    if write {
        if !result.warnings.is_empty() {
            homeboy::log_status!(
                "warning",
                "{} collision warning(s) detected — applying anyway",
                result.warnings.len()
            );
        }

        // Capture undo snapshot before writes
        let affected_files: Vec<String> = result
            .edits
            .iter()
            .map(|e| e.file.clone())
            .chain(result.file_renames.iter().map(|r| r.from.clone()))
            .chain(result.file_renames.iter().map(|r| r.to.clone()))
            .collect();
        homeboy::engine::undo::UndoSnapshot::capture_and_save(
            &root,
            "refactor rename",
            &affected_files,
        );

        refactor::apply_renames(&mut result, &root)?;
    }

    let scope_str = match scope {
        RenameScope::Code => "code",
        RenameScope::Config => "config",
        RenameScope::All => "all",
    };

    let exit_code = if result.total_references == 0 { 1 } else { 0 };

    Ok((
        RefactorOutput::Rename {
            from: from.to_string(),
            to: to.to_string(),
            scope: scope_str.to_string(),
            dry_run: !write,
            variants: result
                .variants
                .iter()
                .map(|v| VariantSummary {
                    from: v.from.clone(),
                    to: v.to.clone(),
                    label: v.label.clone(),
                })
                .collect(),
            total_references: result.total_references,
            total_files: result.total_files,
            edits: result
                .edits
                .iter()
                .map(|e| EditSummary {
                    file: e.file.clone(),
                    replacements: e.replacements,
                })
                .collect(),
            file_renames: result
                .file_renames
                .iter()
                .map(|r| RenameSummary {
                    from: r.from.clone(),
                    to: r.to.clone(),
                })
                .collect(),
            warnings: result
                .warnings
                .iter()
                .map(|w| WarningSummary {
                    kind: w.kind.clone(),
                    file: w.file.clone(),
                    line: w.line,
                    message: w.message.clone(),
                })
                .collect(),
            applied: result.applied,
        },
        exit_code,
    ))
}

pub(crate) fn run_add(
    from_audit: Option<&str>,
    import: Option<&str>,
    to: Option<&str>,
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    // Mode 1: From audit JSON
    if let Some(audit_source) = from_audit {
        return run_add_from_audit(audit_source, write);
    }

    // Mode 2: Explicit import addition
    if let Some(import_line) = import {
        let target = to.ok_or_else(|| {
            homeboy::Error::validation_invalid_argument(
                "to",
                "--to is required when using --import",
                None,
                Some(vec![
                    "homeboy refactor add --import \"use serde::Serialize;\" --to \"src/**/*.rs\""
                        .to_string(),
                ]),
            )
        })?;

        return run_add_import(import_line, target, component_id, path, write);
    }

    // Neither mode specified
    Err(homeboy::Error::validation_invalid_argument(
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

pub(crate) fn run_add_from_audit(source: &str, write: bool) -> CmdResult<RefactorOutput> {
    // Parse audit JSON from @file, stdin (-), file path, or inline string.
    // Auto-detect bare file paths (same pattern as docs generate --from-audit).
    let effective_source = if !source.starts_with('{')
        && !source.starts_with('[')
        && source != "-"
        && !source.starts_with('@')
        && std::path::Path::new(source).exists()
    {
        format!("@{}", source)
    } else {
        source.to_string()
    };

    let json_content = crate::commands::merge_json_sources(Some(&effective_source), &[])?;

    // Parse audit result — handle both envelope and raw formats.
    // The envelope format wraps the audit in a "data" field (from `homeboy --format json audit`).
    let audit: CodeAuditResult = if let Some(data) = json_content.get("data") {
        serde_json::from_value(data.clone())
    } else {
        serde_json::from_value(json_content)
    }
    .map_err(|e| {
        homeboy::Error::validation_invalid_json(
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
