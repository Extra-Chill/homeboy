//! types — extracted from refactor.rs.

use clap::{Args, Subcommand};
use serde::Serialize;

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

    /// Skip the clean working tree check (for CI or when you know what you're doing)
    #[arg(long)]
    force: bool,

    #[command(flatten)]
    write_mode: WriteModeArgs,

    #[command(subcommand)]
    command: Option<RefactorCommand>,
}

#[derive(Subcommand)]
pub(crate) enum RefactorCommand {
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
