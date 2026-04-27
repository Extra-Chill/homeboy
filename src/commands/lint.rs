use clap::Args;

use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::engine::run_dir::RunDir;
use homeboy::extension::lint::{
    report, run_main_lint_workflow, LintCommandOutput, LintRunWorkflowArgs,
};
use homeboy::extension::ExtensionCapability;
use homeboy::refactor::plan::{run_lint_refactor, LintSourceOptions};

use super::utils::args::{BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct LintArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    /// Show compact summary instead of full output
    #[arg(long)]
    pub summary: bool,

    /// Lint only a single file (path relative to component root)
    #[arg(long)]
    pub file: Option<String>,

    /// Lint only files matching glob pattern (e.g., "inc/**/*.php")
    #[arg(long)]
    pub glob: Option<String>,

    /// Lint only files modified in the working tree (staged, unstaged, untracked)
    #[arg(long, conflicts_with = "changed_since")]
    pub changed_only: bool,

    /// Lint only files changed since a git ref (branch, tag, or SHA) — CI-friendly
    #[arg(long, conflicts_with = "changed_only")]
    pub changed_since: Option<String>,

    /// Show only errors, suppress warnings
    #[arg(long)]
    pub errors_only: bool,

    /// Only check specific sniffs (comma-separated codes)
    #[arg(long)]
    pub sniffs: Option<String>,

    /// Exclude sniffs from checking (comma-separated codes)
    #[arg(long)]
    pub exclude_sniffs: Option<String>,

    /// Filter by category: security, i18n, yoda, whitespace
    #[arg(long)]
    pub category: Option<String>,

    /// Apply auto-fixable lint findings in place.
    ///
    /// Thin alias for `homeboy refactor <component> --from lint --write` —
    /// dispatches to the existing fixer pipeline so a single ergonomic flag
    /// resolves the auto-fix CTA without re-typing the canonical invocation.
    #[arg(long)]
    pub fix: bool,

    #[command(flatten)]
    pub setting_args: SettingArgs,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    #[command(flatten)]
    pub _json: HiddenJsonArgs,
}

pub fn run(args: LintArgs, _global: &GlobalArgs) -> CmdResult<LintCommandOutput> {
    let ctx = execution_context::resolve(&ResolveOptions {
        component_id: args.comp.component.clone(),
        path_override: args.comp.path.clone(),
        capability: Some(ExtensionCapability::Lint),
        settings_overrides: args.setting_args.setting.clone(),
        settings_json_overrides: Vec::new(),
    })?;
    let effective_id = ctx.component_id.clone();

    let stringified_settings: Vec<(String, String)> = ctx
        .settings
        .iter()
        .map(|(k, v)| {
            (
                k.clone(),
                match v {
                    serde_json::Value::String(s) => s.clone(),
                    other => other.to_string(),
                },
            )
        })
        .collect();

    // --fix dispatches to the canonical refactor sources pipeline.
    // The fixer pipeline already exists; this flag connects the existing wire
    // so users don't have to re-type `homeboy refactor <component> --from lint
    // --write` to resolve the auto-fix CTA.
    if args.fix {
        return run_fix(args, &ctx, effective_id, stringified_settings);
    }

    let run_dir = RunDir::create()?;

    let workflow = run_main_lint_workflow(
        &ctx.component,
        &ctx.source_path,
        LintRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
            settings: stringified_settings,
            summary: args.summary,
            file: args.file.clone(),
            glob: args.glob.clone(),
            changed_only: args.changed_only,
            changed_since: args.changed_since.clone(),
            errors_only: args.errors_only,
            sniffs: args.sniffs.clone(),
            exclude_sniffs: args.exclude_sniffs.clone(),
            category: args.category.clone(),
            baseline_flags: homeboy::engine::baseline::BaselineFlags {
                baseline: args.baseline_args.baseline,
                ignore_baseline: args.baseline_args.ignore_baseline,
                ratchet: args.baseline_args.ratchet,
            },
        },
        &run_dir,
    )?;

    Ok(report::from_main_workflow(workflow))
}

/// Dispatch `homeboy lint --fix` to the canonical refactor sources pipeline.
///
/// `homeboy lint --fix` is a thin alias for `homeboy refactor <component>
/// --from lint --write`. Under the hood we invoke the same
/// `run_lint_refactor` primitive that the refactor command uses, then wrap
/// the result in a `LintCommandOutput` so the lint command surface returns a
/// stable shape regardless of which mode was requested.
///
/// Exit code semantics: autofixable findings should never fail the run, so
/// this path returns exit 0 unless the underlying fixer actually errored.
fn run_fix(
    args: LintArgs,
    ctx: &homeboy::engine::execution_context::ExecutionContext,
    component_label: String,
    settings: Vec<(String, String)>,
) -> CmdResult<LintCommandOutput> {
    let lint_options = LintSourceOptions {
        selected_files: None,
        file: args.file.clone(),
        glob: args.glob.clone(),
        errors_only: args.errors_only,
        sniffs: args.sniffs.clone(),
        exclude_sniffs: args.exclude_sniffs.clone(),
        category: args.category.clone(),
    };

    let run = run_lint_refactor(
        ctx.component.clone(),
        ctx.source_path.clone(),
        settings,
        lint_options,
        true,
    )?;

    Ok(report::from_lint_fix(component_label, run))
}

#[cfg(test)]
mod tests {
    use homeboy::component::Component;
    use homeboy::extension::lint as extension_lint;
    use homeboy::extension::lint::baseline::{self as lint_baseline, LintFinding};
    use homeboy::extension::lint::report;
    use homeboy::refactor::plan::{
        lint_refactor_request, LintSourceOptions, RefactorSourceRun, SourceTotals,
    };
    use std::path::Path;

    #[test]
    fn lint_baseline_roundtrip_and_compare() {
        let dir = tempfile::tempdir().expect("temp dir");
        let findings = vec![
            LintFinding {
                id: "src/foo.php::WordPress.Security.EscapeOutput".to_string(),
                message: "Missing esc_html()".to_string(),
                category: "security".to_string(),
            },
            LintFinding {
                id: "src/bar.php::WordPress.WP.I18n".to_string(),
                message: "Untranslated string".to_string(),
                category: "i18n".to_string(),
            },
        ];

        let saved = lint_baseline::save_baseline(dir.path(), "homeboy", &findings)
            .expect("baseline should save");
        assert!(saved.exists());

        let loaded = lint_baseline::load_baseline(dir.path()).expect("baseline should load");
        assert_eq!(loaded.context_id, "homeboy");
        assert_eq!(loaded.item_count, 2);

        let current = vec![findings[0].clone()];
        let comparison = lint_baseline::compare(&current, &loaded);
        assert!(!comparison.drift_increased);
        assert_eq!(comparison.resolved_fingerprints.len(), 1);
    }

    #[test]
    fn test_lint_baseline_roundtrip_and_compare() {
        lint_baseline_roundtrip_and_compare();
    }

    #[test]
    fn lint_findings_parse_empty_when_file_missing() {
        let parsed =
            lint_baseline::parse_findings_file(Path::new("/tmp/definitely-missing-lint.json"))
                .expect("missing file should parse as empty");
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_lint_findings_parse_empty_when_file_missing() {
        lint_findings_parse_empty_when_file_missing();
    }

    #[test]
    fn test_resolve_lint_command() {
        let component =
            Component::new("test".to_string(), "/tmp".to_string(), "".to_string(), None);
        let result = extension_lint::resolve_lint_command(&component);
        assert!(result.is_err());
    }

    fn fixture_refactor_run(applied: bool, files_modified: usize) -> RefactorSourceRun {
        RefactorSourceRun {
            component_id: "demo".to_string(),
            source_path: "/tmp/demo".to_string(),
            sources: vec!["lint".to_string()],
            dry_run: !applied,
            applied,
            merge_strategy: "sequential_source_merge".to_string(),
            collected_edits: Vec::new(),
            stages: Vec::new(),
            source_totals: SourceTotals {
                stages_with_edits: if files_modified > 0 { 1 } else { 0 },
                total_edits: files_modified,
                total_files_selected: files_modified,
            },
            overlaps: Vec::new(),
            files_modified,
            changed_files: (0..files_modified)
                .map(|i| format!("src/file_{}.rs", i))
                .collect(),
            fix_summary: None,
            warnings: Vec::new(),
            hints: Vec::new(),
            guard_block: None,
        }
    }

    #[test]
    fn lint_fix_report_passes_with_zero_exit_when_fixes_applied() {
        // The contract under #1507: autofixable findings never fail the run.
        // Even when --fix actually modifies files, the lint command exits 0.
        let run = fixture_refactor_run(true, 3);
        let (output, exit_code) = report::from_lint_fix("demo".to_string(), run);

        assert_eq!(exit_code, 0);
        assert!(output.passed);
        assert_eq!(output.status, "passed");
        assert!(output.failure.is_none());

        let autofix = output.autofix.as_ref().expect("autofix populated");
        assert_eq!(autofix.files_modified, 3);
        assert!(autofix.rerun_recommended);
        assert_eq!(autofix.changed_files.len(), 3);

        let hints = output.hints.as_ref().expect("hints populated");
        assert!(
            hints.iter().any(|h| h.contains("homeboy lint demo")),
            "expected re-run hint pointing back at lint, got {:?}",
            hints
        );
    }

    #[test]
    fn lint_fix_report_passes_when_no_fixes_needed() {
        // When no autofixable findings exist, --fix is a clean no-op:
        // exit 0, no autofix changes reported, friendly hint.
        let run = fixture_refactor_run(false, 0);
        let (output, exit_code) = report::from_lint_fix("demo".to_string(), run);

        assert_eq!(exit_code, 0);
        assert!(output.passed);
        let autofix = output.autofix.as_ref().expect("autofix populated");
        assert_eq!(autofix.files_modified, 0);
        assert!(!autofix.rerun_recommended);
    }

    #[test]
    fn lint_fix_builds_canonical_refactor_request() {
        let component = Component::new(
            "demo".to_string(),
            "/tmp/demo".to_string(),
            String::new(),
            None,
        );

        let request = lint_refactor_request(
            component.clone(),
            std::path::PathBuf::from("/tmp/demo"),
            vec![("mode".to_string(), "strict".to_string())],
            LintSourceOptions {
                selected_files: Some(vec!["src/lib.rs".to_string()]),
                file: None,
                glob: Some("/tmp/demo/src/lib.rs".to_string()),
                errors_only: true,
                sniffs: Some("WordPress.Security".to_string()),
                exclude_sniffs: Some("WordPress.WhiteSpace".to_string()),
                category: Some("security".to_string()),
            },
            true,
        );

        assert_eq!(request.component.id, component.id);
        assert_eq!(request.sources, vec!["lint".to_string()]);
        assert!(request.write);
        assert_eq!(request.settings.len(), 1);
        assert_eq!(request.lint.selected_files.as_ref().unwrap().len(), 1);
        assert!(request.test.selected_files.is_none());
    }
}
