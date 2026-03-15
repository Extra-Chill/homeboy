use clap::{Args, Subcommand};
use homeboy::code_audit::{AuditFinding, CodeAuditResult};
use homeboy::refactor::{
    self, auto, AddResult, MoveResult, RenameScope, RenameSpec, RenameTargeting,
};
use serde::Serialize;

use super::utils::args::{
    BaselineArgs, ComponentArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs,
};
use crate::commands::CmdResult;

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct RefactorArgs {
    #[command(flatten)]
    comp: Option<PositionalComponentArgs>,

    /// Include a specific proposal source (repeatable): audit, lint, test, all
    #[arg(long = "from", value_name = "SOURCE", action = clap::ArgAction::Append)]
    from: Vec<String>,

    /// Only include files changed since a git ref (branch, tag, or SHA)
    #[arg(long)]
    changed_since: Option<String>,

    /// Restrict audit-generated fixes to these fix kinds (repeatable)
    #[arg(long = "only", value_name = "kind")]
    only: Vec<String>,

    /// Exclude audit-generated fixes for these fix kinds (repeatable)
    #[arg(long = "exclude", value_name = "kind")]
    exclude: Vec<String>,

    #[command(flatten)]
    setting_args: SettingArgs,

    #[command(flatten)]
    baseline_args: BaselineArgs,

    #[command(flatten)]
    write_mode: WriteModeArgs,

    #[command(subcommand)]
    command: Option<RefactorCommand>,
}

#[derive(Subcommand)]
enum RefactorCommand {
    /// Rename a term across the codebase with case-variant awareness
    Rename {
        /// Term to rename from
        #[arg(long)]
        from: String,
        /// Term to rename to
        #[arg(long)]
        to: String,
        #[command(flatten)]
        component: ComponentArgs,
        /// Scope: code, config, all (default: all)
        #[arg(long, default_value = "all")]
        scope: String,
        /// Exact string matching (no boundary detection, no case variants)
        #[arg(long)]
        literal: bool,
        /// Include only files matching this glob (repeatable)
        #[arg(long = "files", value_name = "GLOB")]
        files: Vec<String>,
        /// Exclude files matching this glob (repeatable)
        #[arg(long, value_name = "GLOB")]
        exclude: Vec<String>,
        /// Disable file/directory path renames (content edits only)
        #[arg(long)]
        no_file_renames: bool,
        #[command(flatten)]
        write_mode: WriteModeArgs,
    },

    /// Add imports, stubs, or fixes to source files
    ///
    /// Two modes:
    ///   From audit: `refactor add --from-audit @audit.json [--write]`
    ///   Explicit:   `refactor add --import "use serde::Serialize;" --to "src/**/*.rs" [--write]`
    Add {
        /// Apply fixes from saved audit JSON (supports @file, -, or inline JSON)
        #[arg(long, value_name = "AUDIT_JSON")]
        from_audit: Option<String>,

        /// Import/use statement to add (explicit mode)
        #[arg(long, value_name = "IMPORT")]
        import: Option<String>,

        /// Target file or glob pattern for explicit additions
        #[arg(long, value_name = "PATTERN")]
        to: Option<String>,

        #[command(flatten)]
        component: ComponentArgs,
        #[command(flatten)]
        write_mode: WriteModeArgs,
    },

    /// Move items or entire files between modules
    ///
    /// Item mode: `refactor move --item has_import --from src/conventions.rs --to src/import_matching.rs`
    /// File mode: `refactor move --file src/core/hooks.rs --to src/core/engine/hooks.rs`
    Move {
        /// Name(s) of items to move (functions, structs, enums, consts).
        /// When omitted with --file, moves the entire file.
        #[arg(long, value_name = "NAME", num_args = 1..)]
        item: Vec<String>,

        /// Move an entire module file to a new location.
        /// Rewrites all imports and updates mod.rs declarations.
        #[arg(long, value_name = "FILE", conflicts_with = "from")]
        file: Option<String>,

        /// Source file (for item mode — relative to component/path root)
        #[arg(long, value_name = "FILE")]
        from: Option<String>,

        /// Destination file (relative to component/path root, created if needed)
        #[arg(long, value_name = "FILE")]
        to: String,

        #[command(flatten)]
        component: ComponentArgs,
        #[command(flatten)]
        write_mode: WriteModeArgs,
    },

    /// Add missing fields to struct instantiations after a struct definition changes
    ///
    /// Scans the codebase for instantiations of the named struct, detects which fields
    /// are missing, and inserts them with sensible defaults (None, vec![], false, etc.).
    ///
    /// Example: `refactor propagate --struct FileFingerprint --component homeboy`
    Propagate {
        /// Name of the struct to propagate fields for
        #[arg(long, value_name = "NAME", alias = "struct")]
        struct_name: String,

        /// File containing the struct definition (auto-detected if omitted)
        #[arg(long, value_name = "FILE")]
        definition: Option<String>,

        #[command(flatten)]
        component: ComponentArgs,
        #[command(flatten)]
        write_mode: WriteModeArgs,
    },

    /// Apply pattern-based find/replace transforms across a codebase
    ///
    /// Rules are defined in homeboy.json under the "transforms" key,
    /// or passed ad-hoc via --find/--replace/--files flags.
    ///
    /// Named:  `refactor transform wp69_migration --component data-machine`
    /// Ad-hoc: `refactor transform --find "old" --replace "new" --files "**/*.php" --component C`
    Transform {
        /// Transform set name (from homeboy.json transforms key)
        #[arg(value_name = "NAME")]
        name: Option<String>,

        /// Regex pattern to find (ad-hoc mode)
        #[arg(long, value_name = "REGEX")]
        find: Option<String>,

        /// Replacement template with $1, $2 capture group refs (ad-hoc mode)
        #[arg(long, value_name = "TEMPLATE")]
        replace: Option<String>,

        /// Glob pattern for files to apply to (ad-hoc mode, default: **/*)
        #[arg(long, value_name = "GLOB", default_value = "**/*")]
        files: String,

        /// Only apply a specific rule ID within a named transform set
        #[arg(long, value_name = "RULE_ID")]
        rule: Option<String>,

        #[command(flatten)]
        component: ComponentArgs,
        #[command(flatten)]
        write_mode: WriteModeArgs,
    },

    /// Decompose a large source file into a directory of smaller modules
    Decompose {
        /// Source file to decompose (relative to component/path root)
        #[arg(long, value_name = "FILE")]
        file: String,

        /// Planning strategy (currently: grouped)
        #[arg(long, default_value = "grouped")]
        strategy: String,

        #[command(flatten)]
        component: ComponentArgs,

        #[command(flatten)]
        write_mode: WriteModeArgs,
    },
}

pub fn run(args: RefactorArgs, _global: &crate::commands::GlobalArgs) -> CmdResult<RefactorOutput> {
    match args.command {
        None => run_refactor_sources(
            args.comp.as_ref(),
            &args.from,
            args.changed_since.as_deref(),
            &args.only,
            &args.exclude,
            &args.setting_args.setting,
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
                return Err(homeboy::Error::validation_invalid_argument(
                    "from",
                    "Either --from (with --item) or --file is required",
                    None,
                    Some(vec![
                        "Move items: refactor move --item foo --from src/a.rs --to src/b.rs"
                            .to_string(),
                        "Move file: refactor move --file src/a.rs --to src/b.rs".to_string(),
                    ]),
                ));
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
            rule,
            component,
            write_mode,
        }) => run_transform(
            name.as_deref(),
            find.as_deref(),
            replace.as_deref(),
            &files,
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

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum RefactorOutput {
    #[serde(rename = "refactor.plan")]
    Plan(homeboy::refactor::RefactorPlan),

    #[serde(rename = "refactor.rename")]
    Rename {
        from: String,
        to: String,
        scope: String,
        dry_run: bool,
        variants: Vec<VariantSummary>,
        total_references: usize,
        total_files: usize,
        edits: Vec<EditSummary>,
        file_renames: Vec<RenameSummary>,
        warnings: Vec<WarningSummary>,
        applied: bool,
    },

    #[serde(rename = "refactor.add.from_audit")]
    AddFromAudit {
        source_path: String,
        #[serde(flatten)]
        fix_result: auto::FixResult,
        dry_run: bool,
    },

    #[serde(rename = "refactor.add.import")]
    AddImport {
        import: String,
        target: String,
        #[serde(flatten)]
        result: AddResult,
        dry_run: bool,
    },

    #[serde(rename = "refactor.move")]
    Move {
        #[serde(flatten)]
        result: MoveResult,
    },

    #[serde(rename = "refactor.move_file")]
    MoveFile {
        #[serde(flatten)]
        result: refactor::move_items::MoveFileResult,
    },

    #[serde(rename = "refactor.propagate")]
    Propagate {
        #[serde(flatten)]
        result: refactor::PropagateResult,
        dry_run: bool,
    },

    #[serde(rename = "refactor.transform")]
    Transform {
        #[serde(flatten)]
        result: homeboy::refactor::TransformResult,
    },

    #[serde(rename = "refactor.decompose")]
    Decompose {
        plan: homeboy::refactor::DecomposePlan,
        move_results: Vec<homeboy::refactor::MoveResult>,
        dry_run: bool,
        applied: bool,
    },
}

#[derive(Serialize)]
pub struct VariantSummary {
    pub from: String,
    pub to: String,
    pub label: String,
}

#[derive(Serialize)]
pub struct EditSummary {
    pub file: String,
    pub replacements: usize,
}

#[derive(Serialize)]
pub struct RenameSummary {
    pub from: String,
    pub to: String,
}

#[derive(Serialize)]
pub struct WarningSummary {
    pub kind: String,
    pub file: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub line: Option<usize>,
    pub message: String,
}

fn run_refactor_sources(
    comp: Option<&PositionalComponentArgs>,
    from: &[String],
    changed_since: Option<&str>,
    only: &[String],
    exclude: &[String],
    settings: &[(String, String)],
    write: bool,
) -> CmdResult<RefactorOutput> {
    let comp = comp.ok_or_else(|| {
        homeboy::Error::validation_missing_argument(vec!["component".to_string()])
    })?;
    let component = comp.load()?;
    let root = comp.source_path()?;
    let requested_sources = from.to_vec();
    let only_findings = parse_audit_findings(only)?;
    let exclude_findings = parse_audit_findings(exclude)?;
    let plan = homeboy::refactor::build_refactor_plan(homeboy::refactor::RefactorPlanRequest {
        component,
        root,
        sources: requested_sources,
        changed_since: changed_since.map(ToOwned::to_owned),
        only: only_findings,
        exclude: exclude_findings,
        settings: settings.to_vec(),
        lint: homeboy::refactor::LintSourceOptions::default(),
        test: homeboy::refactor::TestSourceOptions::default(),
        write,
    })?;
    let exit_code = if plan.files_modified > 0 { 1 } else { 0 };

    Ok((RefactorOutput::Plan(plan), exit_code))
}

fn parse_audit_findings(values: &[String]) -> homeboy::Result<Vec<AuditFinding>> {
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
fn run_rename(
    from: &str,
    to: &str,
    component_id: Option<&str>,
    path: Option<&str>,
    scope: &str,
    literal: bool,
    include_globs: &[String],
    exclude_globs: &[String],
    no_file_renames: bool,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let scope = RenameScope::from_str(scope)?;

    let root = refactor::move_items::resolve_root(component_id, path)?;

    let spec = if literal {
        RenameSpec::literal(from, to, scope.clone())
    } else {
        RenameSpec::new(from, to, scope.clone())
    };
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

fn run_add(
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

fn run_add_from_audit(source: &str, write: bool) -> CmdResult<RefactorOutput> {
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
    component_id: Option<&str>,
    path: Option<&str>,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let root = refactor::move_items::resolve_root(component_id, path)?;

    if write {
        homeboy::engine::undo::UndoSnapshot::capture_and_save(&root, "refactor move --file", [file, to]);
    }

    let result = refactor::move_items::move_file(file, to, &root, write)?;

    let exit_code = if result.imports_updated > 0 || result.mod_declarations_updated { 0 } else { 1 };

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
            refactor::ad_hoc_transform(f, r, files),
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
            affected,
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
