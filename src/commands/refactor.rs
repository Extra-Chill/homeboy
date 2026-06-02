use clap::{Args, Subcommand};
use homeboy::core::code_audit::AuditFinding;
use homeboy::core::component::{self, TargetSpec};
use homeboy::core::engine::execution_context::{self, ResolveOptions};
use homeboy::core::refactor::{
    self, auto, AddResult, MoveResult, RenameContext, RenameScope, RenameSpec, RenameTargeting,
};
use serde::Serialize;
use std::collections::HashSet;

use super::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs,
};
use crate::commands::CmdResult;

mod autofix_commit;
mod operations_command;
mod transform_command;

#[derive(Args)]
#[command(args_conflicts_with_subcommands = true, subcommand_negates_reqs = true)]
pub struct RefactorArgs {
    #[command(flatten)]
    comp: Option<PositionalComponentArgs>,

    #[command(flatten)]
    extension_override: ExtensionOverrideArgs,

    /// Target a component by ID (repeatable)
    #[arg(short, long = "component", value_name = "ID", action = clap::ArgAction::Append)]
    component_ids: Vec<String>,

    /// Target multiple components with a comma-separated list
    #[arg(long, value_name = "ID[,ID...]", value_delimiter = ',')]
    components: Vec<String>,

    /// Include a specific proposal source (repeatable): audit, lint, test, all
    #[arg(long = "from", value_name = "SOURCE", action = clap::ArgAction::Append)]
    from: Vec<String>,

    /// Compatibility alias for `--from all`
    #[arg(long = "all")]
    all: bool,

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
        /// Add an explicit variant mapping as FROM=TO (repeatable)
        #[arg(long = "variant", value_name = "FROM=TO")]
        variants: Vec<String>,
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
    /// Example: `refactor propagate --struct-name FileFingerprint --component homeboy`
    Propagate {
        /// Name of the struct to propagate fields for
        #[arg(long, value_name = "NAME")]
        struct_name: String,

        /// File containing the struct definition (auto-detected if omitted)
        #[arg(long, value_name = "FILE")]
        definition: Option<String>,

        #[command(flatten)]
        target: RefactorTargetArgs,
        #[command(flatten)]
        write_mode: WriteModeArgs,
    },

    /// Apply an ad-hoc pattern-based find/replace transform across a codebase
    ///
    /// Example: `refactor transform --find "old" --replace "new" --files "**/*.php" --component C`
    ///
    /// Replacement templates support capture group refs ($1, $2, ${name}),
    /// case transforms ($1:lower, $1:upper, $1:kebab, $1:snake, $1:pascal, $1:camel),
    /// and literal $ via $$ (important for PHP code where every variable starts with $).
    ///
    /// Backslash escapes collapse before regex replacement: `\\` → one literal
    /// backslash, `\n` → newline, `\t` → tab, `\r` → CR, `\"` / `\'` → the quote.
    /// Write `\\WP_Foo` to emit `\WP_Foo` on disk (useful for PHP fully-qualified
    /// class names). Unknown `\X` sequences pass through as-is.
    Transform {
        /// Regex pattern to find
        #[arg(long, value_name = "REGEX")]
        find: String,

        /// Replacement template.
        /// Supports $1, $2 capture group refs, ${name} named groups,
        /// $1:lower/:upper/:kebab/:snake/:pascal/:camel case transforms,
        /// and $$ for a literal dollar sign.
        /// Backslash escapes are collapsed: \\ → one literal backslash,
        /// \n/\t/\r/\0 → the control character, \" / \' → the quote.
        #[arg(long, value_name = "TEMPLATE")]
        replace: String,

        /// Glob pattern for files to apply to (default: **/*)
        #[arg(long, value_name = "GLOB", default_value = "**/*")]
        files: String,

        /// Match context: "line" (default, per-line matching) or "file" (whole-file,
        /// enables multi-line regex with (?s) dotall flag for patterns spanning newlines)
        #[arg(long, value_name = "CONTEXT", default_value = "line")]
        context: String,

        /// Include every match detail in JSON output instead of the default bounded sample.
        #[arg(long)]
        full_match_details: bool,

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
            &args.extension_override.extensions,
            &args.from,
            args.all,
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
            variants,
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
            &variants,
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
        }) => operations_command::run_add(
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
                operations_command::run_move_file(&file_path, &to, &target, write_mode.write)
            } else if let Some(from_path) = from {
                if item.is_empty() {
                    return Err(homeboy::core::Error::validation_invalid_argument(
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
                operations_command::run_move(&item, &from_path, &to, &target, write_mode.write)
            } else {
                Err(homeboy::core::Error::validation_invalid_argument(
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
        }) => operations_command::run_propagate(
            &struct_name,
            definition.as_deref(),
            &target,
            write_mode.write,
        ),

        Some(RefactorCommand::Transform {
            find,
            replace,
            files,
            context,
            full_match_details,
            target,
            write_mode,
        }) => transform_command::run_transform(
            &find,
            &replace,
            &files,
            &context,
            full_match_details,
            &target,
            write_mode.write,
        ),

        Some(RefactorCommand::Decompose {
            file,
            strategy,
            target,
            write_mode,
        }) => operations_command::run_decompose(&file, &strategy, &target, write_mode.write),
    }
}

impl RefactorArgs {
    pub fn is_hot_resource_command(&self) -> bool {
        self.command.is_none()
            && (self.all
                || self
                    .from
                    .iter()
                    .any(|source| matches_hot_refactor_source(source)))
    }

    pub fn lab_offload_writes_local_state(&self) -> bool {
        self.write_mode.write || self.commit
    }
}

fn matches_hot_refactor_source(source: &str) -> bool {
    matches!(
        source.to_ascii_lowercase().as_str(),
        "all" | "audit" | "lint" | "test"
    )
}

#[derive(Serialize)]
#[serde(tag = "command")]
pub enum RefactorOutput {
    #[serde(rename = "refactor.sources")]
    Sources(homeboy::core::refactor::plan::RefactorSourceRun),

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
        result: homeboy::core::refactor::TransformResult,
    },

    #[serde(rename = "refactor.decompose")]
    Decompose {
        plan: homeboy::core::refactor::DecomposePlan,
        move_results: Vec<homeboy::core::refactor::MoveResult>,
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
    fn resolve_targets(&self) -> homeboy::core::Result<Vec<RefactorTarget>> {
        let component_ids = collect_component_ids(&self.component_ids, &self.components);
        if self.path.is_some() && !component_ids.is_empty() {
            return Err(homeboy::core::Error::validation_invalid_argument(
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
            return resolve_refactor_target(None, Some(path));
        }

        if component_ids.is_empty() {
            return Err(homeboy::core::Error::validation_missing_argument(vec![
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

fn resolve_refactor_target(
    component_id: Option<&str>,
    path: Option<&str>,
) -> homeboy::core::Result<Vec<RefactorTarget>> {
    let target = component::resolve_target(TargetSpec::new(component_id, path))?;
    let path = target.source_path.to_string_lossy().to_string();

    Ok(vec![RefactorTarget {
        label: target.component_id.clone(),
        component_id: Some(target.component_id),
        path: Some(path),
    }])
}

fn resolve_top_level_targets(
    comp: Option<&PositionalComponentArgs>,
    component_ids: &[String],
    components: &[String],
) -> homeboy::core::Result<Vec<RefactorTarget>> {
    let flagged_ids = collect_component_ids(component_ids, components);

    if let Some(comp) = comp {
        if let Some(ref component_id) = comp.component {
            if !flagged_ids.is_empty() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "component",
                    "Use either positional component syntax or --component/--components, not both",
                    None,
                    None,
                ));
            }

            return resolve_refactor_target(Some(component_id), comp.path.as_deref());
        }
        // Component omitted — fall through to flagged_ids or CWD auto-discovery
    }

    if flagged_ids.is_empty() {
        return resolve_refactor_target(None, None);
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
    extension_overrides: &[String],
    from: &[String],
    all: bool,
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
    let requested_sources = requested_refactor_sources(from, all);
    run_across_targets("sources", targets, |component_id, path| {
        run_refactor_sources_single(
            component_id,
            path,
            extension_overrides,
            &requested_sources,
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

fn requested_refactor_sources(from: &[String], all: bool) -> Vec<String> {
    let mut sources = from.to_vec();
    if all
        && !sources
            .iter()
            .any(|source| source.eq_ignore_ascii_case("all"))
    {
        sources.push("all".to_string());
    }
    sources
}

#[allow(clippy::too_many_arguments)]
fn run_refactor_sources_single(
    component_id: Option<&str>,
    path: Option<&str>,
    extension_overrides: &[String],
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
        homeboy::core::Error::validation_missing_argument(vec!["component".to_string()])
    })?;
    let mut resolve_options = ResolveOptions::source_only(component_id, path.map(str::to_string));
    resolve_options.extension_overrides = extension_overrides.to_vec();
    let ctx = execution_context::resolve(&resolve_options)?;
    let requested_sources = from.to_vec();
    let only_findings = parse_audit_findings(only)?;
    let exclude_findings = parse_audit_findings(exclude)?;
    let source_path = ctx.source_path.clone();
    let sources = homeboy::core::refactor::plan::collect_refactor_sources(
        homeboy::core::refactor::plan::RefactorSourceRequest {
            component: ctx.component,
            root: ctx.source_path,
            sources: requested_sources,
            changed_since: changed_since.map(ToOwned::to_owned),
            only: only_findings,
            exclude: exclude_findings,
            settings: settings.to_vec(),
            lint: homeboy::core::refactor::plan::LintSourceOptions::default(),
            test: homeboy::core::refactor::plan::TestSourceOptions::default(),
            write,
            force,
        },
    )?;
    let exit_code = if sources.files_modified > 0 { 1 } else { 0 };

    // --commit: stage all changes and create a commit with a structured message
    if commit && write && sources.applied {
        let root_str = source_path.to_string_lossy();
        autofix_commit::commit_refactor_sources(&root_str, &sources, git_identity)?;
    }

    Ok((RefactorOutput::Sources(sources), exit_code))
}

fn parse_audit_findings(values: &[String]) -> homeboy::core::Result<Vec<AuditFinding>> {
    values
        .iter()
        .map(|value| {
            value.parse::<AuditFinding>().map_err(|_| {
                homeboy::core::Error::validation_invalid_argument(
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
    variants: &[String],
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
            variants,
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
    variants: &[String],
    no_file_renames: bool,
    context: &str,
    write: bool,
) -> CmdResult<RefactorOutput> {
    let scope = RenameScope::from_str(scope)?;
    let rename_context = RenameContext::from_str(context)?;

    let root = refactor::move_items::resolve_root(component_id, path)?;

    let explicit_variants = parse_rename_variants(variants)?;
    let mut spec = if literal {
        RenameSpec::literal(from, to, scope.clone())
    } else {
        RenameSpec::new(from, to, scope.clone())
    }
    .with_explicit_variants(explicit_variants);
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
        homeboy::core::engine::undo::UndoSnapshot::capture_and_save(
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

fn parse_rename_variants(values: &[String]) -> homeboy::core::Result<Vec<(String, String)>> {
    values
        .iter()
        .map(|value| {
            let (from, to) = value.split_once('=').ok_or_else(|| {
                homeboy::core::Error::validation_invalid_argument(
                    "variant",
                    format!("Invalid rename variant '{}'. Expected FROM=TO", value),
                    Some(value.clone()),
                    None,
                )
            })?;
            let from = from.trim();
            let to = to.trim();
            if from.is_empty() || to.is_empty() {
                return Err(homeboy::core::Error::validation_invalid_argument(
                    "variant",
                    format!(
                        "Invalid rename variant '{}'. FROM and TO must be non-empty",
                        value
                    ),
                    Some(value.clone()),
                    None,
                ));
            }
            Ok((from.to_string(), to.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        refactor: RefactorArgs,
    }

    #[test]
    fn parses_one_shot_extension_override_for_source_refactor() {
        let cli = TestCli::try_parse_from([
            "refactor",
            "--path",
            "/tmp/repo",
            "--from",
            "lint",
            "--extension",
            "wordpress",
        ])
        .expect("refactor should parse --extension override");

        assert_eq!(
            cli.refactor.extension_override.extensions,
            vec!["wordpress"]
        );
        assert_eq!(cli.refactor.from, vec!["lint"]);
    }

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
