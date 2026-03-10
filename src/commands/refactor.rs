use clap::{Args, Subcommand};
use serde::Serialize;
use std::collections::{BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use homeboy::code_audit::{fixer, CodeAuditResult};
use homeboy::component;
use homeboy::extension;
use homeboy::git;
use homeboy::refactor::{self, AddResult, MoveResult, RenameScope, RenameSpec, RenameTargeting};
use homeboy::utils::autofix::{self, FixResultsSummary};

use super::args::{
    BaselineArgs, ComponentArgs, PositionalComponentArgs, SettingArgs, WriteModeArgs,
};
use crate::commands::CmdResult;

#[derive(Args)]
pub struct RefactorArgs {
    #[command(subcommand)]
    command: RefactorCommand,
}

#[derive(Subcommand)]
enum RefactorCommand {
    /// Run the end-of-pipeline automated refactor/fix orchestration
    Ci {
        #[command(flatten)]
        comp: PositionalComponentArgs,

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
    },

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

    /// Move functions, structs, or other items from one file to another
    ///
    /// Example: `refactor move --item has_import --item contains_word --from src/conventions.rs --to src/import_matching.rs`
    Move {
        /// Name(s) of items to move (functions, structs, enums, consts)
        #[arg(long, value_name = "NAME", required = true, num_args = 1..)]
        item: Vec<String>,

        /// Source file (relative to component/path root)
        #[arg(long, value_name = "FILE")]
        from: String,

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
        RefactorCommand::Ci {
            comp,
            changed_since,
            only,
            exclude,
            setting_args,
            baseline_args: _,
            write_mode,
        } => run_ci(
            &comp,
            changed_since.as_deref(),
            &only,
            &exclude,
            &setting_args.setting,
            write_mode.write,
        ),

        RefactorCommand::Rename {
            from,
            to,
            component,
            scope,
            literal,
            files,
            exclude,
            no_file_renames,
            write_mode,
        } => run_rename(
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

        RefactorCommand::Add {
            from_audit,
            import,
            to,
            component,
            write_mode,
        } => run_add(
            from_audit.as_deref(),
            import.as_deref(),
            to.as_deref(),
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),

        RefactorCommand::Move {
            item,
            from,
            to,
            component,
            write_mode,
        } => run_move(
            &item,
            &from,
            &to,
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),

        RefactorCommand::Propagate {
            struct_name,
            definition,
            component,
            write_mode,
        } => run_propagate(
            &struct_name,
            definition.as_deref(),
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),

        RefactorCommand::Transform {
            name,
            find,
            replace,
            files,
            rule,
            component,
            write_mode,
        } => run_transform(
            name.as_deref(),
            find.as_deref(),
            replace.as_deref(),
            &files,
            rule.as_deref(),
            component.component.as_deref(),
            component.path.as_deref(),
            write_mode.write,
        ),

        RefactorCommand::Decompose {
            file,
            strategy,
            component,
            write_mode,
        } => run_decompose(
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
    #[serde(rename = "refactor.ci")]
    Ci {
        component_id: String,
        source_path: String,
        dry_run: bool,
        applied: bool,
        merge_strategy: String,
        stages: Vec<CiStageSummary>,
        plan_totals: CiPlanTotals,
        overlaps: Vec<CiStageOverlap>,
        files_modified: usize,
        changed_files: Vec<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        fix_summary: Option<FixResultsSummary>,
        warnings: Vec<String>,
        hints: Vec<String>,
    },

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
        fix_result: fixer::FixResult,
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

    #[serde(rename = "refactor.propagate")]
    Propagate {
        struct_name: String,
        definition_file: String,
        fields: Vec<PropagateField>,
        files_scanned: usize,
        instantiations_found: usize,
        instantiations_needing_fix: usize,
        edits: Vec<PropagateEdit>,
        dry_run: bool,
        applied: bool,
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

#[derive(Serialize)]
pub struct CiStageSummary {
    pub stage: String,
    pub planned: bool,
    pub applied: bool,
    pub fixes_proposed: usize,
    pub files_modified: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detected_findings: Option<usize>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub changed_files: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix_summary: Option<FixResultsSummary>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Serialize, Debug, PartialEq, Eq)]
pub struct CiStageOverlap {
    pub file: String,
    pub earlier_stage: String,
    pub later_stage: String,
    pub resolution: String,
}

#[derive(Serialize)]
pub struct CiPlanTotals {
    pub stages_with_proposals: usize,
    pub total_fixes_proposed: usize,
    pub total_files_selected: usize,
}

#[derive(Default)]
struct FixAccumulator {
    fixes: Vec<autofix::FixApplied>,
}

impl FixAccumulator {
    fn extend(&mut self, items: Vec<autofix::FixApplied>) {
        self.fixes.extend(items);
    }

    fn summary(&self) -> Option<FixResultsSummary> {
        if self.fixes.is_empty() {
            None
        } else {
            Some(autofix::summarize_fix_results(&self.fixes))
        }
    }
}

fn run_ci(
    comp: &PositionalComponentArgs,
    changed_since: Option<&str>,
    only: &[String],
    exclude: &[String],
    settings: &[(String, String)],
    write: bool,
) -> CmdResult<RefactorOutput> {
    let component = comp.load()?;
    let root = comp.source_path()?;
    let root_str = root.to_string_lossy().to_string();

    let original_changes = git::get_uncommitted_changes(&root_str).ok();
    let scoped_changed_files = if let Some(git_ref) = changed_since {
        Some(git::get_files_changed_since(&root_str, git_ref)?)
    } else {
        None
    };
    let scoped_test_files = if let Some(git_ref) = changed_since {
        Some(super::test_scope::compute_changed_test_scope(&component, git_ref)?.selected_files)
    } else {
        None
    };

    let mut planned_stages = Vec::new();
    let mut warnings = Vec::new();
    let mut accumulator = FixAccumulator::default();

    warnings.push(
        "Deterministic merge order: audit structural fixes → lint fixes → test fixes".to_string(),
    );

    let plan_mode = true;
    let working_root = clone_tree(&root)?;

    let audit_stage = plan_audit_stage(
        &component.id,
        &root,
        scoped_changed_files.as_deref(),
        only,
        exclude,
        Some(working_root.path()),
        plan_mode,
    )?;
    accumulator.extend(audit_stage.fix_results.clone());
    planned_stages.push(audit_stage.summary);

    let lint_stage = run_lint_stage(
        &component,
        &working_root,
        settings,
        scoped_changed_files.as_deref(),
        plan_mode,
    )?;
    accumulator.extend(lint_stage.fix_results.clone());
    planned_stages.push(lint_stage.summary);

    let test_stage = run_test_stage(
        &component,
        &working_root,
        settings,
        scoped_test_files.as_deref(),
        plan_mode,
    )?;
    accumulator.extend(test_stage.fix_results.clone());
    planned_stages.push(test_stage.summary);

    let mut final_changed_files = BTreeSet::new();
    for stage in &planned_stages {
        for file in &stage.changed_files {
            final_changed_files.insert(file.clone());
        }
    }

    let overlaps = analyze_stage_overlaps(&planned_stages);
    if !overlaps.is_empty() {
        warnings.push(format!(
            "{} staged file overlap(s) resolved by precedence order {}",
            overlaps.len(),
            "audit→lint→test"
        ));
    }

    let changed_files: Vec<String> = final_changed_files.into_iter().collect();
    let plan_totals = summarize_plan_totals(&planned_stages, changed_files.len());

    let files_modified = changed_files.len();
    let applied = write && files_modified > 0;

    if write && applied {
        let mut snapshot_files: HashSet<String> = changed_files.iter().cloned().collect();
        if let Some(changes) = &original_changes {
            snapshot_files.extend(changes.staged.iter().cloned());
            snapshot_files.extend(changes.unstaged.iter().cloned());
            snapshot_files.extend(changes.untracked.iter().cloned());
        }

        if !snapshot_files.is_empty() {
            let mut snap = homeboy::undo::UndoSnapshot::new(&root, "refactor ci");
            for file in &snapshot_files {
                snap.capture_file(file);
            }
            if let Err(e) = snap.save() {
                homeboy::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
            }
        }

        copy_changed_files(working_root.path(), &root, &changed_files)?;
    }

    for stage in &mut planned_stages {
        stage.applied = write && stage.files_modified > 0;
    }

    if files_modified == 0 {
        warnings.push("No automated fixes accumulated across audit/lint/test".to_string());
    }

    let hints = if applied {
        vec![
            format!("Re-run checks: homeboy audit {}", comp.component),
            format!("Re-run checks: homeboy lint {}", comp.component),
            format!("Re-run checks: homeboy test {}", comp.component),
        ]
    } else if files_modified > 0 {
        vec![
            "Plan only. Sandbox passes were used to accumulate fix proposals without touching the real tree. Re-run with --write to apply them.".to_string(),
        ]
    } else {
        Vec::new()
    };

    let exit_code = if files_modified > 0 { 1 } else { 0 };

    Ok((
        RefactorOutput::Ci {
            component_id: component.id,
            source_path: root_str,
            dry_run: !write,
            applied,
            merge_strategy: "sequential_sandbox_merge(audit→lint→test)".to_string(),
            stages: planned_stages,
            plan_totals,
            overlaps,
            files_modified,
            changed_files,
            fix_summary: accumulator.summary(),
            warnings,
            hints,
        },
        exit_code,
    ))
}

struct PlannedStage {
    summary: CiStageSummary,
    fix_results: Vec<autofix::FixApplied>,
}

fn plan_audit_stage(
    component_id: &str,
    root: &Path,
    changed_files: Option<&[String]>,
    only: &[String],
    exclude: &[String],
    apply_root: Option<&Path>,
    write: bool,
) -> homeboy::Result<PlannedStage> {
    let result = if let Some(changed) = changed_files {
        if changed.is_empty() {
            homeboy::code_audit::CodeAuditResult {
                component_id: component_id.to_string(),
                source_path: root.to_string_lossy().to_string(),
                summary: homeboy::code_audit::AuditSummary {
                    files_scanned: 0,
                    conventions_detected: 0,
                    outliers_found: 0,
                    alignment_score: None,
                    files_skipped: 0,
                    warnings: vec![],
                },
                conventions: vec![],
                directory_conventions: vec![],
                findings: vec![],
                duplicate_groups: vec![],
            }
        } else {
            homeboy::code_audit::audit_path_scoped(
                component_id,
                &root.to_string_lossy(),
                changed,
                None,
            )?
        }
    } else {
        homeboy::code_audit::audit_path_with_id(component_id, &root.to_string_lossy())?
    };

    let mut fix_result = fixer::generate_fixes(&result, root);
    let only_kinds = parse_audit_findings(only)?;
    let exclude_kinds = parse_audit_findings(exclude)?;
    let policy = fixer::FixPolicy {
        only: (!only_kinds.is_empty()).then_some(only_kinds),
        exclude: exclude_kinds,
    };
    let preflight_context = fixer::PreflightContext { root };
    let policy_summary =
        fixer::apply_fix_policy(&mut fix_result, write, &policy, &preflight_context);

    let changed_files: Vec<String> = collect_audit_changed_files(&fix_result);
    let mut fix_results = summarize_audit_fix_result_entries(&fix_result);
    let fixes_proposed = fix_results.len();
    if !write {
        fix_results.clear();
    }

    if write {
        if let Some(apply_root) = apply_root {
            if !fix_result.fixes.is_empty() {
                fixer::apply_fixes(&mut fix_result.fixes, apply_root);
            }
            if !fix_result.new_files.is_empty() {
                fixer::apply_new_files(&mut fix_result.new_files, apply_root);
            }
        }
    }

    Ok(PlannedStage {
        summary: CiStageSummary {
            stage: "audit".to_string(),
            planned: true,
            applied: write && !changed_files.is_empty(),
            fixes_proposed,
            files_modified: changed_files.len(),
            detected_findings: Some(result.findings.len()),
            changed_files,
            fix_summary: if policy_summary.visible_insertions + policy_summary.visible_new_files > 0
            {
                Some(autofix::summarize_audit_fix_result(&fix_result))
            } else {
                None
            },
            warnings: Vec::new(),
        },
        fix_results,
    })
}

fn run_lint_stage(
    component: &homeboy::component::Component,
    sandbox: &SandboxDir,
    settings: &[(String, String)],
    changed_files: Option<&[String]>,
    plan_mode: bool,
) -> homeboy::Result<PlannedStage> {
    let mut sandbox_component = component.clone();
    sandbox_component.local_path = sandbox.path().to_string_lossy().to_string();
    let script_path = super::lint::resolve_lint_script(&sandbox_component)?;
    let findings_file = std::env::temp_dir().join(format!(
        "homeboy-lint-findings-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let fix_results_file = autofix::fix_results_temp_path();
    let fix_plan_file = autofix::fix_plan_temp_path();
    let before_fix = if plan_mode {
        Some(autofix::changed_file_set(&sandbox_component.local_path)?)
    } else {
        None
    };

    let effective_glob = if let Some(changed_files) = changed_files {
        if changed_files.is_empty() {
            None
        } else {
            let abs_files: Vec<String> = changed_files
                .iter()
                .map(|f| format!("{}/{}", sandbox_component.local_path, f))
                .collect();
            if abs_files.len() == 1 {
                Some(abs_files[0].clone())
            } else {
                Some(format!("{{{}}}", abs_files.join(",")))
            }
        }
    } else {
        None
    };

    let _output = extension::ExtensionRunner::new(&sandbox_component.id, &script_path)
        .component(sandbox_component.clone())
        .settings(settings)
        .env_if(plan_mode, "HOMEBOY_AUTO_FIX", "1")
        .env_opt("HOMEBOY_LINT_GLOB", &effective_glob)
        .env(
            "HOMEBOY_LINT_FINDINGS_FILE",
            &findings_file.to_string_lossy(),
        )
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_PLAN_FILE",
            &fix_plan_file.to_string_lossy(),
        )
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_RESULTS_FILE",
            &fix_results_file.to_string_lossy(),
        )
        .run()?;

    let changed_files = if plan_mode {
        let after_fix = autofix::changed_file_set(&sandbox_component.local_path)?;
        before_fix
            .as_ref()
            .map(|before| {
                after_fix
                    .difference(before)
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let planned_fix_results = autofix::parse_fix_plan_file(&fix_plan_file);
    let fix_results = if planned_fix_results.is_empty() {
        autofix::parse_fix_results_file(&fix_results_file)
    } else {
        planned_fix_results
    };
    let fixes_proposed = fix_results.len();
    let lint_findings =
        homeboy::lint_baseline::parse_findings_file(&findings_file).unwrap_or_default();
    let _ = std::fs::remove_file(&fix_results_file);
    let _ = std::fs::remove_file(&fix_plan_file);
    let _ = std::fs::remove_file(&findings_file);

    Ok(PlannedStage {
        summary: CiStageSummary {
            stage: "lint".to_string(),
            planned: true,
            applied: plan_mode && !changed_files.is_empty(),
            fixes_proposed,
            files_modified: changed_files.len(),
            detected_findings: Some(lint_findings.len()),
            changed_files: changed_files.clone(),
            fix_summary: if fix_results.is_empty() {
                None
            } else {
                Some(autofix::summarize_fix_results(&fix_results))
            },
            warnings: Vec::new(),
        },
        fix_results,
    })
}

fn run_test_stage(
    component: &homeboy::component::Component,
    sandbox: &SandboxDir,
    settings: &[(String, String)],
    changed_test_files: Option<&[String]>,
    plan_mode: bool,
) -> homeboy::Result<PlannedStage> {
    let mut sandbox_component = component.clone();
    sandbox_component.local_path = sandbox.path().to_string_lossy().to_string();
    let script_path = super::test::resolve_test_script(&sandbox_component)?;
    let results_file =
        std::env::temp_dir().join(format!("homeboy-test-results-{}.json", std::process::id()));
    let fix_results_file = autofix::fix_results_temp_path();
    let fix_plan_file = autofix::fix_plan_temp_path();
    let before_fix = if plan_mode {
        Some(autofix::changed_file_set(&sandbox_component.local_path)?)
    } else {
        None
    };

    let mut runner = extension::ExtensionRunner::new(&sandbox_component.id, &script_path)
        .component(sandbox_component.clone())
        .settings(settings)
        .env("HOMEBOY_TEST_RESULTS_FILE", &results_file.to_string_lossy())
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_PLAN_FILE",
            &fix_plan_file.to_string_lossy(),
        )
        .env_if(
            plan_mode,
            "HOMEBOY_FIX_RESULTS_FILE",
            &fix_results_file.to_string_lossy(),
        )
        .env_if(plan_mode, "HOMEBOY_AUTO_FIX", "1");

    if let Some(changed_test_files) = changed_test_files {
        if !changed_test_files.is_empty() {
            runner = runner.env("HOMEBOY_CHANGED_TEST_FILES", &changed_test_files.join("\n"));
        }
    }

    let _output = runner.run()?;

    let changed_files = if plan_mode {
        let after_fix = autofix::changed_file_set(&sandbox_component.local_path)?;
        before_fix
            .as_ref()
            .map(|before| {
                after_fix
                    .difference(before)
                    .cloned()
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect()
            })
            .unwrap_or_default()
    } else {
        Vec::new()
    };

    let planned_fix_results = autofix::parse_fix_plan_file(&fix_plan_file);
    let fix_results = if planned_fix_results.is_empty() {
        autofix::parse_fix_results_file(&fix_results_file)
    } else {
        planned_fix_results
    };
    let fixes_proposed = fix_results.len();
    let _ = std::fs::remove_file(&fix_results_file);
    let _ = std::fs::remove_file(&fix_plan_file);
    let _ = std::fs::remove_file(&results_file);

    Ok(PlannedStage {
        summary: CiStageSummary {
            stage: "test".to_string(),
            planned: true,
            applied: plan_mode && !changed_files.is_empty(),
            fixes_proposed,
            files_modified: changed_files.len(),
            detected_findings: None,
            changed_files: changed_files.clone(),
            fix_summary: if fix_results.is_empty() {
                None
            } else {
                Some(autofix::summarize_fix_results(&fix_results))
            },
            warnings: Vec::new(),
        },
        fix_results,
    })
}

struct SandboxDir {
    path: PathBuf,
}

impl SandboxDir {
    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SandboxDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

fn clone_tree(src: &Path) -> homeboy::Result<SandboxDir> {
    let temp = std::env::temp_dir().join(format!("homeboy-refactor-ci-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&temp).map_err(|e| {
        homeboy::Error::internal_io(
            e.to_string(),
            Some("create temp refactor sandbox".to_string()),
        )
    })?;
    copy_dir_recursive(src, &temp)?;
    Ok(SandboxDir { path: temp })
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> homeboy::Result<()> {
    std::fs::create_dir_all(dst).map_err(|e| {
        homeboy::Error::internal_io(e.to_string(), Some("create sandbox dir".to_string()))
    })?;

    for entry in std::fs::read_dir(src).map_err(|e| {
        homeboy::Error::internal_io(e.to_string(), Some("read source dir".to_string()))
    })? {
        let entry = entry.map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some("read dir entry".to_string()))
        })?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());

        if src_path.is_dir() {
            if entry.file_name() == ".git" {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            std::fs::copy(&src_path, &dst_path).map_err(|e| {
                homeboy::Error::internal_io(e.to_string(), Some("copy sandbox file".to_string()))
            })?;
        }
    }

    Ok(())
}

fn copy_changed_files(
    src_root: &Path,
    dst_root: &Path,
    changed_files: &[String],
) -> homeboy::Result<()> {
    for file in changed_files {
        let src = src_root.join(file);
        let dst = dst_root.join(file);

        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                homeboy::Error::internal_io(
                    e.to_string(),
                    Some(format!("create parent for {}", file)),
                )
            })?;
        }

        std::fs::copy(&src, &dst).map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some(format!("copy changed file {}", file)))
        })?;
    }

    Ok(())
}

fn collect_audit_changed_files(fix_result: &fixer::FixResult) -> Vec<String> {
    let mut files = BTreeSet::new();
    for fix in &fix_result.fixes {
        if !fix.insertions.is_empty() {
            files.insert(fix.file.clone());
        }
    }
    for file in &fix_result.new_files {
        files.insert(file.file.clone());
    }
    files.into_iter().collect()
}

fn summarize_audit_fix_result_entries(fix_result: &fixer::FixResult) -> Vec<autofix::FixApplied> {
    let mut entries = Vec::new();

    for fix in &fix_result.fixes {
        for insertion in &fix.insertions {
            if insertion.auto_apply {
                entries.push(autofix::FixApplied {
                    file: fix.file.clone(),
                    rule: format!("{:?}", insertion.finding).to_lowercase(),
                    action: Some("insert".to_string()),
                });
            }
        }
    }

    for new_file in &fix_result.new_files {
        entries.push(autofix::FixApplied {
            file: new_file.file.clone(),
            rule: format!("{:?}", new_file.finding).to_lowercase(),
            action: Some("create".to_string()),
        });
    }

    entries
}

fn parse_audit_findings(
    values: &[String],
) -> homeboy::Result<Vec<homeboy::code_audit::AuditFinding>> {
    values
        .iter()
        .map(|value| {
            value
                .parse::<homeboy::code_audit::AuditFinding>()
                .map_err(|_| {
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

fn analyze_stage_overlaps(stages: &[CiStageSummary]) -> Vec<CiStageOverlap> {
    let mut overlaps = Vec::new();

    for (later_index, later_stage) in stages.iter().enumerate() {
        if later_stage.changed_files.is_empty() {
            continue;
        }

        let later_files: BTreeSet<&str> = later_stage
            .changed_files
            .iter()
            .map(String::as_str)
            .collect();

        for earlier_stage in stages.iter().take(later_index) {
            if earlier_stage.changed_files.is_empty() {
                continue;
            }

            for file in earlier_stage.changed_files.iter().map(String::as_str) {
                if later_files.contains(file) {
                    overlaps.push(CiStageOverlap {
                        file: file.to_string(),
                        earlier_stage: earlier_stage.stage.clone(),
                        later_stage: later_stage.stage.clone(),
                        resolution: format!(
                            "{} pass ran after {} in sandbox sequence",
                            later_stage.stage, earlier_stage.stage
                        ),
                    });
                }
            }
        }
    }

    overlaps.sort_by(|a, b| {
        a.file
            .cmp(&b.file)
            .then(a.earlier_stage.cmp(&b.earlier_stage))
            .then(a.later_stage.cmp(&b.later_stage))
    });

    overlaps
}

fn summarize_plan_totals(stages: &[CiStageSummary], total_files_selected: usize) -> CiPlanTotals {
    CiPlanTotals {
        stages_with_proposals: stages
            .iter()
            .filter(|stage| stage.fixes_proposed > 0)
            .count(),
        total_fixes_proposed: stages.iter().map(|stage| stage.fixes_proposed).sum(),
        total_files_selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn analyze_stage_overlaps_reports_later_stage_precedence() {
        let stages = vec![
            CiStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            CiStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 2,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string(), "src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            CiStageSummary {
                stage: "test".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: None,
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let overlaps = analyze_stage_overlaps(&stages);

        assert_eq!(
            overlaps,
            vec![
                CiStageOverlap {
                    file: "src/lib.rs".to_string(),
                    earlier_stage: "audit".to_string(),
                    later_stage: "lint".to_string(),
                    resolution: "lint pass ran after audit in sandbox sequence".to_string(),
                },
                CiStageOverlap {
                    file: "src/main.rs".to_string(),
                    earlier_stage: "lint".to_string(),
                    later_stage: "test".to_string(),
                    resolution: "test pass ran after lint in sandbox sequence".to_string(),
                },
            ]
        );
    }

    #[test]
    fn analyze_stage_overlaps_ignores_disjoint_files() {
        let stages = vec![
            CiStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            CiStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: true,
                fixes_proposed: 1,
                files_modified: 1,
                detected_findings: Some(1),
                changed_files: vec!["src/main.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        assert!(analyze_stage_overlaps(&stages).is_empty());
    }

    #[test]
    fn summarize_plan_totals_counts_stage_and_fix_totals() {
        let stages = vec![
            CiStageSummary {
                stage: "audit".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 2,
                files_modified: 1,
                detected_findings: Some(2),
                changed_files: vec!["src/lib.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
            CiStageSummary {
                stage: "lint".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 0,
                files_modified: 0,
                detected_findings: Some(1),
                changed_files: Vec::new(),
                fix_summary: None,
                warnings: Vec::new(),
            },
            CiStageSummary {
                stage: "test".to_string(),
                planned: true,
                applied: false,
                fixes_proposed: 3,
                files_modified: 2,
                detected_findings: None,
                changed_files: vec!["tests/foo.rs".to_string(), "tests/bar.rs".to_string()],
                fix_summary: None,
                warnings: Vec::new(),
            },
        ];

        let totals = summarize_plan_totals(&stages, 3);

        assert_eq!(totals.stages_with_proposals, 2);
        assert_eq!(totals.total_fixes_proposed, 5);
        assert_eq!(totals.total_files_selected, 3);
    }
}

#[derive(Serialize)]
pub struct PropagateField {
    pub name: String,
    #[serde(rename = "type")]
    pub field_type: String,
    pub default: String,
}

#[derive(Serialize)]
pub struct PropagateEdit {
    pub file: String,
    pub line: usize,
    pub insert_text: String,
    pub description: String,
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

    // Resolve root directory
    let root = if let Some(p) = path {
        std::path::PathBuf::from(p)
    } else {
        let comp = component::resolve(component_id)?;
        component::validate_local_path(&comp)?
    };

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
            .collect();
        if !affected_files.is_empty() {
            let mut snap = homeboy::undo::UndoSnapshot::new(&root, "refactor rename");
            for file in &affected_files {
                snap.capture_file(file);
            }
            // New files from renames
            for rename in &result.file_renames {
                snap.capture_file(&rename.to);
            }
            if let Err(e) = snap.save() {
                homeboy::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
            }
        }

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

    // Capture undo snapshot before write operations
    if write {
        let mut snap = homeboy::undo::UndoSnapshot::new(&root, "refactor move");
        snap.capture_file(from);
        snap.capture_file(to);
        if let Err(e) = snap.save() {
            homeboy::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
        }
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

    // Step 1: Find the struct definition file
    let def_file = if let Some(f) = definition_file {
        PathBuf::from(f)
    } else {
        find_struct_definition(struct_name, &root)?
    };

    let def_path = if def_file.is_absolute() {
        def_file.clone()
    } else {
        root.join(&def_file)
    };

    let def_content = std::fs::read_to_string(&def_path).map_err(|e| {
        homeboy::Error::internal_io(
            e.to_string(),
            Some(format!(
                "read struct definition from {}",
                def_path.display()
            )),
        )
    })?;

    // Step 2: Extract the struct source block
    let struct_source = extract_struct_source(struct_name, &def_content).ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "struct_name",
            format!(
                "Could not find struct `{}` in {}",
                struct_name,
                def_path.display()
            ),
            None,
            None,
        )
    })?;

    // Step 3: Find the extension for .rs files
    let ext_manifest = extension::find_extension_for_file_ext("rs", "refactor").ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "extension",
            "No extension with refactor capability found for .rs files. Install the Rust extension.",
            None,
            None,
        )
    })?;

    // Step 4: Walk all .rs files and call the extension for each
    let rs_files = walk_rs_files(&root);
    let def_relative = def_file
        .strip_prefix(&root)
        .unwrap_or(&def_file)
        .to_string_lossy()
        .to_string();

    let mut all_edits: Vec<PropagateEdit> = Vec::new();
    let mut total_instantiations = 0usize;
    let mut total_needing_fix = 0usize;
    let mut files_scanned = 0usize;

    homeboy::log_status!(
        "propagate",
        "Scanning {} .rs files for {} instantiations",
        rs_files.len(),
        struct_name
    );

    for file_path in &rs_files {
        let relative = file_path
            .strip_prefix(&root)
            .unwrap_or(file_path)
            .to_string_lossy()
            .to_string();

        let Ok(file_content) = std::fs::read_to_string(file_path) else {
            continue;
        };

        // Quick check: skip files that don't mention the struct name at all
        if !file_content.contains(struct_name) {
            continue;
        }

        files_scanned += 1;

        let cmd = serde_json::json!({
            "command": "propagate_struct_fields",
            "struct_name": struct_name,
            "struct_source": struct_source,
            "file_content": file_content,
            "file_path": relative,
        });

        let Some(result) = extension::run_refactor_script(&ext_manifest, &cmd) else {
            homeboy::log_status!("warning", "Extension returned no result for {}", relative);
            continue;
        };

        if let Some(found) = result.get("instantiations_found").and_then(|v| v.as_u64()) {
            total_instantiations += found as usize;
        }
        if let Some(needing) = result
            .get("instantiations_needing_fix")
            .and_then(|v| v.as_u64())
        {
            total_needing_fix += needing as usize;
        }

        if let Some(edits) = result.get("edits").and_then(|v| v.as_array()) {
            for edit in edits {
                let file = edit
                    .get("file")
                    .and_then(|v| v.as_str())
                    .unwrap_or(&relative)
                    .to_string();
                let line = edit.get("line").and_then(|v| v.as_u64()).unwrap_or(0) as usize;
                let insert_text = edit
                    .get("insert_text")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();
                let description = edit
                    .get("description")
                    .and_then(|v| v.as_str())
                    .unwrap_or("")
                    .to_string();

                all_edits.push(PropagateEdit {
                    file,
                    line,
                    insert_text,
                    description,
                });
            }
        }
    }

    // Step 5: Apply edits if --write
    let applied = if write && !all_edits.is_empty() {
        // Capture undo snapshot before writes
        let affected_files: std::collections::HashSet<&str> =
            all_edits.iter().map(|e| e.file.as_str()).collect();
        let mut snap = homeboy::undo::UndoSnapshot::new(&root, "refactor propagate");
        for file in &affected_files {
            snap.capture_file(file);
        }
        if let Err(e) = snap.save() {
            homeboy::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
        }

        apply_propagate_edits(&all_edits, &root)?;
        true
    } else {
        false
    };

    // Parse struct fields from the edits we collected — each edit's description
    // tells us the field name and the insert_text gives us the default value.
    let fields: Vec<PropagateField> = {
        let mut seen = std::collections::HashSet::new();
        all_edits
            .iter()
            .filter_map(|e| {
                // "Add missing field `verbose` to FileFingerprint instantiation"
                let start = e.description.find('`')? + 1;
                let end = e.description[start..].find('`')? + start;
                let field_name = &e.description[start..end];
                if seen.insert(field_name.to_string()) {
                    // Extract type and default from insert_text: "        verbose: false,"
                    let trimmed = e.insert_text.trim().trim_end_matches(',');
                    let colon_pos = trimmed.find(':')?;
                    let default = trimmed[colon_pos + 1..].trim().to_string();
                    Some(PropagateField {
                        name: field_name.to_string(),
                        field_type: String::new(), // We don't have the type info from edits alone
                        default,
                    })
                } else {
                    None
                }
            })
            .collect()
    };

    let edit_count = all_edits.len();

    homeboy::log_status!(
        "propagate",
        "{} instantiation(s) found, {} need fixes, {} edit(s){}",
        total_instantiations,
        total_needing_fix,
        edit_count,
        if write {
            if applied {
                " (applied)".to_string()
            } else {
                " (nothing to apply)".to_string()
            }
        } else {
            " (dry run)".to_string()
        }
    );

    for edit in &all_edits {
        homeboy::log_status!("edit", "{}:{} — {}", edit.file, edit.line, edit.description);
    }

    let exit_code = if all_edits.is_empty() { 0 } else { 1 };

    Ok((
        RefactorOutput::Propagate {
            struct_name: struct_name.to_string(),
            definition_file: def_relative,
            fields,
            files_scanned,
            instantiations_found: total_instantiations,
            instantiations_needing_fix: total_needing_fix,
            edits: all_edits,
            dry_run: !write,
            applied,
        },
        exit_code,
    ))
}

/// Find the file containing a struct definition by grepping the codebase.
fn find_struct_definition(struct_name: &str, root: &Path) -> Result<PathBuf, homeboy::Error> {
    let pattern = format!("pub struct {} ", struct_name);
    let pattern_brace = format!("pub struct {} {{", struct_name);
    let pattern_crate = format!("pub(crate) struct {} ", struct_name);
    let pattern_crate_brace = format!("pub(crate) struct {} {{", struct_name);

    let files = walk_rs_files(root);
    for file_path in &files {
        let Ok(content) = std::fs::read_to_string(file_path) else {
            continue;
        };
        if content.contains(&pattern)
            || content.contains(&pattern_brace)
            || content.contains(&pattern_crate)
            || content.contains(&pattern_crate_brace)
        {
            return Ok(file_path.clone());
        }
    }

    Err(homeboy::Error::validation_invalid_argument(
        "struct_name",
        format!(
            "Could not find struct `{}` in any .rs file under {}",
            struct_name,
            root.display()
        ),
        None,
        Some(vec![format!(
            "homeboy refactor propagate --struct {} --definition src/path/to/file.rs",
            struct_name
        )]),
    ))
}

/// Extract the full struct source block from file content.
fn extract_struct_source(struct_name: &str, content: &str) -> Option<String> {
    let lines: Vec<&str> = content.lines().collect();

    // Find the struct keyword line
    let struct_pattern = format!("struct {} ", struct_name);
    let struct_pattern_brace = format!("struct {} {{", struct_name);
    let mut start_line = None;

    for (i, line) in lines.iter().enumerate() {
        if line.contains(&struct_pattern) || line.contains(&struct_pattern_brace) {
            // Walk backwards to include attributes and doc comments
            let mut actual_start = i;
            for j in (0..i).rev() {
                let trimmed = lines[j].trim();
                if trimmed.starts_with('#')
                    || trimmed.starts_with("///")
                    || trimmed.starts_with("//!")
                {
                    actual_start = j;
                } else if trimmed.is_empty() {
                    // Allow one blank line between attrs and struct
                    if j > 0
                        && (lines[j - 1].trim().starts_with('#')
                            || lines[j - 1].trim().starts_with("///"))
                    {
                        actual_start = j;
                    } else {
                        break;
                    }
                } else {
                    break;
                }
            }
            start_line = Some(actual_start);
            break;
        }
    }

    let start = start_line?;

    // Find the closing brace
    let mut depth = 0i32;
    let mut found_open = false;
    let mut end_line = start;

    for (i, line_content) in lines.iter().enumerate().skip(start) {
        for ch in line_content.chars() {
            if ch == '{' {
                depth += 1;
                found_open = true;
            } else if ch == '}' {
                depth -= 1;
            }
        }
        if found_open && depth == 0 {
            end_line = i;
            break;
        }
    }

    Some(lines[start..=end_line].join("\n"))
}

/// Walk all .rs files in the project, skipping standard non-source directories.
fn walk_rs_files(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    walk_rs_recursive(root, root, &mut files);
    files
}

fn walk_rs_recursive(dir: &Path, root: &Path, files: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };

    let is_root = dir == root;
    let skip_always = ["node_modules", "vendor", ".git", ".svn", ".hg"];
    let skip_root = ["build", "dist", "target", "cache", "tmp"];

    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            if skip_always.iter().any(|&s| s == name) {
                continue;
            }
            if is_root && skip_root.iter().any(|&s| s == name) {
                continue;
            }
            walk_rs_recursive(&path, root, files);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            files.push(path);
        }
    }
}

/// Apply propagate edits to disk. Edits are line-based insertions.
fn apply_propagate_edits(edits: &[PropagateEdit], root: &Path) -> Result<(), homeboy::Error> {
    // Group edits by file
    let mut edits_by_file: std::collections::HashMap<&str, Vec<&PropagateEdit>> =
        std::collections::HashMap::new();
    for edit in edits {
        edits_by_file.entry(&edit.file).or_default().push(edit);
    }

    for (file, file_edits) in &edits_by_file {
        let file_path = root.join(file);
        let content = std::fs::read_to_string(&file_path).map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some(format!("read {}", file)))
        })?;

        let lines: Vec<&str> = content.lines().collect();
        // Sort edits by line number descending so we insert from bottom to top
        let mut sorted_edits: Vec<&&PropagateEdit> = file_edits.iter().collect();
        sorted_edits.sort_by(|a, b| b.line.cmp(&a.line));

        // Convert to a mutable Vec<String>
        let mut mutable_lines: Vec<String> = lines.iter().map(|l| l.to_string()).collect();

        // Insert from bottom to top to preserve line numbers
        for edit in &sorted_edits {
            let insert_idx = edit.line.saturating_sub(1); // Convert 1-indexed to 0-indexed
            if insert_idx <= mutable_lines.len() {
                mutable_lines.insert(insert_idx, edit.insert_text.clone());
            }
        }

        let new_content = mutable_lines.join("\n");

        // Preserve trailing newline if original had one
        let final_content = if content.ends_with('\n') && !new_content.ends_with('\n') {
            format!("{}\n", new_content)
        } else {
            new_content
        };

        std::fs::write(&file_path, &final_content).map_err(|e| {
            homeboy::Error::internal_io(e.to_string(), Some(format!("write {}", file)))
        })?;

        homeboy::log_status!("write", "{} ({} edits)", file, file_edits.len());
    }

    Ok(())
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
    // Resolve root directory
    let root = if let Some(p) = path {
        PathBuf::from(p)
    } else {
        let comp = component::resolve(component_id)?;
        component::validate_local_path(&comp)?
    };

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

    // Capture undo snapshot before writes
    if write {
        // Dry-run first to discover affected files
        if let Ok(preview) = refactor::apply_transforms(&root, &set_name, &set, false, rule_filter)
        {
            let affected_files: Vec<String> = preview
                .rules
                .iter()
                .flat_map(|r| r.matches.iter().map(|m| m.file.clone()))
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            if !affected_files.is_empty() {
                let mut snap = homeboy::undo::UndoSnapshot::new(&root, "refactor transform");
                for file in &affected_files {
                    snap.capture_file(file);
                }
                if let Err(e) = snap.save() {
                    homeboy::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
                }
            }
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

    // Capture undo snapshot before writes
    if write {
        let mut snap = homeboy::undo::UndoSnapshot::new(&root, "refactor decompose");
        snap.capture_file(file);
        for group in &plan.groups {
            snap.capture_file(&group.suggested_target);
        }
        if let Err(e) = snap.save() {
            homeboy::log_status!("undo", "Warning: failed to save undo snapshot: {}", e);
        }
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
