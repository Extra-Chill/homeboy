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

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct RefactorArgs {
    #[command(flatten)]
    comp: Option<PositionalComponentArgs>,

    /// Target a component by ID (repeatable)
    #[arg(short, long = "component", value_name = "ID", action = clap::ArgAction::Append)]
    component_ids: Vec<String>,

    /// Target multiple components with a comma-separated list
    #[arg(long, value_name = "ID[,ID...]", value_delimiter = ',')]
    components: Vec<String>,

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

    /// Skip the clean working tree check (for CI or when you know what you're doing)
    #[arg(long)]
    force: bool,

    #[command(flatten)]
    write_mode: WriteModeArgs,

    /// After applying fixes, stage all changes and commit.
    /// Only effective with --write. The commit message is built from fix results.
    #[arg(long, requires = "write")]
    commit: bool,

    /// Git identity for the commit (used with --commit).
    /// Use "bot" for the default CI bot identity, or "Name <email>" for custom.
    #[arg(long)]
    git_identity: Option<String>,

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
        target: RefactorTargetArgs,
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
        /// Syntactic context filter: key (strings/property access), variable/var,
        /// parameter/param, all (default — match everything)
        #[arg(long, default_value = "all")]
        context: String,
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
        target: RefactorTargetArgs,
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
        target: RefactorTargetArgs,
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
        target: RefactorTargetArgs,
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
    ///
    /// Replacement templates support capture group refs ($1, $2, ${name}),
    /// case transforms ($1:lower, $1:upper, $1:kebab, $1:snake, $1:pascal, $1:camel),
    /// and literal $ via $$ (important for PHP code where every variable starts with $).
    Transform {
        /// Transform set name (from homeboy.json transforms key)
        #[arg(value_name = "NAME")]
        name: Option<String>,

        /// Regex pattern to find (ad-hoc mode)
        #[arg(long, value_name = "REGEX")]
        find: Option<String>,

        /// Replacement template (ad-hoc mode).
        /// Supports $1, $2 capture group refs, ${name} named groups,
        /// $1:lower/:upper/:kebab/:snake/:pascal/:camel case transforms,
        /// and $$ for a literal dollar sign.
        #[arg(long, value_name = "TEMPLATE")]
        replace: Option<String>,

        /// Glob pattern for files to apply to (ad-hoc mode, default: **/*)
        #[arg(long, value_name = "GLOB", default_value = "**/*")]
        files: String,

        /// Match context: "line" (default, per-line matching) or "file" (whole-file,
        /// enables multi-line regex with (?s) dotall flag for patterns spanning newlines)
        #[arg(long, value_name = "CONTEXT", default_value = "line")]
        context: String,

        /// Only apply a specific rule ID within a named transform set
        #[arg(long, value_name = "RULE_ID")]
        rule: Option<String>,

        #[command(flatten)]
        target: RefactorTargetArgs,
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
        target: RefactorTargetArgs,

        #[command(flatten)]
        write_mode: WriteModeArgs,
    },
}

#[derive(Args, Debug, Clone, Default)]
struct RefactorTargetArgs {
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
            args.commit,
            args.git_identity.as_deref(),
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

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum RefactorOutput {
    #[serde(rename = "refactor.sources")]
    Sources(homeboy::refactor::plan::RefactorSourceRun),

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

    #[serde(rename = "refactor.bulk")]
    Bulk {
        action: String,
        results: Vec<RefactorBulkItem>,
        summary: RefactorBulkSummary,
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

#[derive(Debug, Clone)]
struct RefactorTarget {
    component_id: Option<String>,
    path: Option<String>,
    label: String,
}

#[derive(Serialize)]
pub struct RefactorBulkItem {
    pub id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Box<RefactorOutput>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Serialize)]
pub struct RefactorBulkSummary {
    pub total: usize,
    pub succeeded: usize,
    pub failed: usize,
}

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

fn resolve_top_level_targets(
    comp: Option<&PositionalComponentArgs>,
    component_ids: &[String],
    components: &[String],
) -> homeboy::Result<Vec<RefactorTarget>> {
    let flagged_ids = collect_component_ids(component_ids, components);

    if let Some(comp) = comp {
        if let Some(ref component_id) = comp.component {
            if !flagged_ids.is_empty() {
                return Err(homeboy::Error::validation_invalid_argument(
                    "component",
                    "Use either positional component syntax or --component/--components, not both",
                    None,
                    None,
                ));
            }

            return Ok(vec![RefactorTarget {
                component_id: Some(component_id.clone()),
                path: comp.path.clone(),
                label: component_id.clone(),
            }]);
        }
        // Component omitted — fall through to flagged_ids or CWD auto-discovery
    }

    if flagged_ids.is_empty() {
        // No component specified anywhere — try CWD auto-discovery
        let component = homeboy::component::resolution::resolve(None)?;
        return Ok(vec![RefactorTarget {
            label: component.id.clone(),
            component_id: Some(component.id),
            path: None,
        }]);
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

fn collect_component_ids(primary: &[String], secondary: &[String]) -> Vec<String> {
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

#[allow(clippy::too_many_arguments)]
fn run_refactor_sources(
    comp: Option<&PositionalComponentArgs>,
    component_ids: &[String],
    components: &[String],
    from: &[String],
    changed_since: Option<&str>,
    only: &[String],
    exclude: &[String],
    settings: &[(String, String)],
    force: bool,
    write: bool,
    commit: bool,
    git_identity: Option<&str>,
) -> CmdResult<RefactorOutput> {
    let targets = resolve_top_level_targets(comp, component_ids, components)?;
    run_across_targets("sources", targets, |component_id, path| {
        run_refactor_sources_single(
            component_id,
            path,
            from,
            changed_since,
            only,
            exclude,
            settings,
            force,
            write,
            commit,
            git_identity,
        )
    })
}

#[allow(clippy::too_many_arguments)]
fn run_refactor_sources_single(
    component_id: Option<&str>,
    path: Option<&str>,
    from: &[String],
    changed_since: Option<&str>,
    only: &[String],
    exclude: &[String],
    settings: &[(String, String)],
    force: bool,
    write: bool,
    commit: bool,
    git_identity: Option<&str>,
) -> CmdResult<RefactorOutput> {
    let component_id = component_id.ok_or_else(|| {
        homeboy::Error::validation_missing_argument(vec!["component".to_string()])
    })?;
    let ctx = execution_context::resolve(&ResolveOptions::source_only(
        component_id,
        path.map(str::to_string),
    ))?;
    let requested_sources = from.to_vec();
    let only_findings = parse_audit_findings(only)?;
    let exclude_findings = parse_audit_findings(exclude)?;
    let source_path = ctx.source_path.clone();
    let sources = homeboy::refactor::plan::collect_refactor_sources(
        homeboy::refactor::plan::RefactorSourceRequest {
            component: ctx.component,
            root: ctx.source_path,
            sources: requested_sources,
            changed_since: changed_since.map(ToOwned::to_owned),
            only: only_findings,
            exclude: exclude_findings,
            settings: settings.to_vec(),
            lint: homeboy::refactor::plan::LintSourceOptions::default(),
            test: homeboy::refactor::plan::TestSourceOptions::default(),
            write,
            force,
        },
    )?;
    let exit_code = if sources.files_modified > 0 { 1 } else { 0 };

    // --commit: stage all changes and create a commit with a structured message
    if commit && write && sources.applied {
        let root_str = source_path.to_string_lossy();
        autofix_commit(&root_str, &sources, git_identity)?;
    }

    Ok((RefactorOutput::Sources(sources), exit_code))
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

#[allow(clippy::too_many_arguments)]
fn run_rename_single(
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

fn run_add(
    from_audit: Option<&str>,
    import: Option<&str>,
    to: Option<&str>,
    target: &RefactorTargetArgs,
    write: bool,
) -> CmdResult<RefactorOutput> {
    // Mode 1: From audit JSON
    if let Some(audit_source) = from_audit {
        return run_add_from_audit(audit_source, write);
    }

    // Mode 2: Explicit import addition
    if let Some(import_line) = import {
        let destination = to.ok_or_else(|| {
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

        let targets = target.resolve_targets()?;
        return run_across_targets("add", targets, |component_id, path| {
            run_add_import(import_line, destination, component_id, path, write)
        });
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

// ── Autofix commit ──────────────────────────────────────────────────────────

const BOT_NAME: &str = "homeboy-ci[bot]";
const BOT_EMAIL: &str = "266378653+homeboy-ci[bot]@users.noreply.github.com";
const AUTOFIX_PREFIX: &str = "chore(ci): homeboy autofix";

/// Stage all changes and create a commit after refactor --write.
fn autofix_commit(
    path: &str,
    sources: &homeboy::refactor::plan::RefactorSourceRun,
    git_identity: Option<&str>,
) -> homeboy::Result<()> {
    use std::process::Command;

    // Stage all changes
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(path)
        .output()
        .map_err(|e| homeboy::Error::git_command_failed(format!("git add: {e}")))?;
    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        return Err(homeboy::Error::git_command_failed(format!(
            "git add -A failed: {stderr}"
        )));
    }

    // Check if there's anything staged (fixes may have been no-ops after formatting)
    let diff_check = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(path)
        .output()
        .map_err(|e| homeboy::Error::git_command_failed(format!("git diff: {e}")))?;
    if diff_check.status.success() {
        eprintln!("[refactor] No staged changes after git add — skipping commit");
        return Ok(());
    }

    // Resolve git identity
    let (name, email) = resolve_git_identity(git_identity);

    // Configure git identity
    for (key, value) in [("user.name", name.as_str()), ("user.email", email.as_str())] {
        let config = Command::new("git")
            .args(["config", key, value])
            .current_dir(path)
            .output()
            .map_err(|e| homeboy::Error::git_command_failed(format!("git config: {e}")))?;
        if !config.status.success() {
            let stderr = String::from_utf8_lossy(&config.stderr);
            return Err(homeboy::Error::git_command_failed(format!(
                "git config {key} failed: {stderr}"
            )));
        }
    }

    // Build commit message
    let message = build_autofix_commit_message(sources);

    // Commit with explicit author to match the configured identity
    let author = format!("{name} <{email}>");
    let commit = Command::new("git")
        .args(["commit", "-m", &message, "--author", &author])
        .current_dir(path)
        .output()
        .map_err(|e| homeboy::Error::git_command_failed(format!("git commit: {e}")))?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        return Err(homeboy::Error::git_command_failed(format!(
            "git commit failed: {stderr}"
        )));
    }

    eprintln!(
        "[refactor] Committed autofix: {} files changed",
        sources.files_modified
    );
    Ok(())
}

/// Resolve git identity from the --git-identity flag.
/// "bot" → default CI bot. "Name <email>" → parsed. None → default bot.
fn resolve_git_identity(identity: Option<&str>) -> (String, String) {
    match identity {
        None | Some("bot") => (BOT_NAME.to_string(), BOT_EMAIL.to_string()),
        Some(custom) => {
            // Parse "Name <email>" format
            if let Some(angle_start) = custom.find('<') {
                let name = custom[..angle_start].trim().to_string();
                let email = custom[angle_start + 1..]
                    .trim_end_matches('>')
                    .trim()
                    .to_string();
                if !name.is_empty() && !email.is_empty() {
                    return (name, email);
                }
            }
            // Fallback: use the whole string as name, bot email
            (custom.to_string(), BOT_EMAIL.to_string())
        }
    }
}

/// Build a structured commit message from refactor results.
///
/// Format matches the homeboy-action convention:
/// ```text
/// chore(ci): homeboy autofix — refactor (5 files, 12 fixes)
///
/// Unused imports removed: 5 fixes (3 files)
/// Dead code removed: 4 fixes (2 files)
/// ...
/// ```
fn build_autofix_commit_message(sources: &homeboy::refactor::plan::RefactorSourceRun) -> String {
    let source_labels: Vec<&str> = sources.sources.iter().map(|s| s.as_str()).collect();
    let source_desc = source_labels.join(", ");

    let total_fixes = sources
        .fix_summary
        .as_ref()
        .map(|s| s.fixes_applied)
        .unwrap_or(0);

    // Subject line
    let subject = if total_fixes > 0 {
        format!(
            "{AUTOFIX_PREFIX} — {source_desc} ({} files, {total_fixes} fixes)",
            sources.files_modified
        )
    } else {
        format!(
            "{AUTOFIX_PREFIX} — {source_desc} ({} files)",
            sources.files_modified
        )
    };

    // Body: per-rule breakdown from fix_summary
    let mut body_lines = Vec::new();
    if let Some(ref summary) = sources.fix_summary {
        for rule in &summary.rules {
            body_lines.push(format!("{}: {} fixes", rule.rule, rule.count));
        }
    }

    // Fall back to changed file list if no rule breakdown
    if body_lines.is_empty() {
        for file in &sources.changed_files {
            body_lines.push(file.clone());
        }
    }

    if body_lines.is_empty() {
        subject
    } else {
        format!("{subject}\n\n{}", body_lines.join("\n"))
    }
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

    #[test]
    fn resolve_git_identity_bot_shorthand() {
        let (name, email) = resolve_git_identity(Some("bot"));
        assert_eq!(name, BOT_NAME);
        assert_eq!(email, BOT_EMAIL);
    }

    #[test]
    fn resolve_git_identity_none_defaults_to_bot() {
        let (name, email) = resolve_git_identity(None);
        assert_eq!(name, BOT_NAME);
        assert_eq!(email, BOT_EMAIL);
    }

    #[test]
    fn resolve_git_identity_custom_parsed() {
        let (name, email) = resolve_git_identity(Some("My Bot <my-bot@example.com>"));
        assert_eq!(name, "My Bot");
        assert_eq!(email, "my-bot@example.com");
    }

    #[test]
    fn resolve_git_identity_name_only_uses_bot_email() {
        let (name, email) = resolve_git_identity(Some("Just A Name"));
        assert_eq!(name, "Just A Name");
        assert_eq!(email, BOT_EMAIL);
    }
}
