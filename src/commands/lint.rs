use clap::Args;

use homeboy::core::ci_profile;
use homeboy::core::extension::lint::{
    report, run_main_lint_workflow, run_self_check_lint_workflow_with_progress, LintCommandOutput,
    LintRunWorkflowArgs,
};
use homeboy::core::extension::ExtensionCapability;
use homeboy::core::git;
use homeboy::core::observation::{
    finding_records_from_lint, merge_metadata, ActiveObservation, NewFindingRecord, NewRunRecord,
    RunStatus,
};
use homeboy::core::refactor::plan::{
    collect_refactor_sources, lint_refactor_request, LintSourceOptions,
};
use homeboy::core::validation_progress::validation_progress_metadata;

use super::source_command::{resolve_ci_job_for_command, resolve_source_context};
use super::utils::args::{
    BaselineArgs, ExtensionOverrideArgs, LintSniffArgs, PositionalComponentArgs, SettingArgs,
};
use super::utils::observed_workflow::{ObservedWorkflowRunner, WorkflowObservationAdapter};
use super::{CmdResult, GlobalArgs};
use crate::command_contract::{
    CommandJsonFamily, CommandOutputDescriptor, CommandOutputFileMode, LabCommandContract,
    LINT_LAB_LABEL,
};

const LINT_CHANGED_SCOPE_LAB_UNSUPPORTED_REASON: &str = "Changed-scope lint runs stay local because changed-file scopes are not represented in the current Lab portability contract yet.";

#[derive(Args)]
pub struct LintArgs {
    #[command(flatten)]
    pub comp: PositionalComponentArgs,

    #[command(flatten)]
    pub extension_override: ExtensionOverrideArgs,

    /// Show compact summary instead of full output
    #[arg(long)]
    pub summary: bool,

    /// Lint only a single file (path relative to component root)
    #[arg(long)]
    pub file: Option<String>,

    /// Lint only files matching a repo-relative glob pattern
    #[arg(long)]
    pub glob: Option<String>,

    /// Lint modified files in the working tree (file-scoped, not hunk-scoped)
    #[arg(long, conflicts_with = "changed_since")]
    pub changed_only: bool,

    /// Lint only files changed since a git ref (branch, tag, or SHA) — CI-friendly
    #[arg(long, conflicts_with = "changed_only")]
    pub changed_since: Option<String>,

    #[arg(skip)]
    pub precomputed_changed_files: Option<Vec<String>>,

    /// Run using env from a single extension-declared CI lint job.
    #[arg(long, value_name = "ID", conflicts_with = "fix")]
    pub ci_job: Option<String>,

    #[command(flatten)]
    pub sniff_filters: LintSniffArgs,

    /// Filter by category: security, i18n, yoda, whitespace
    #[arg(long)]
    pub category: Option<String>,

    /// Apply auto-fixable lint findings in place using the lint fixer pipeline.
    #[arg(long)]
    pub fix: bool,

    /// Allow --fix to edit the current dirty working tree for unbounded runs
    #[arg(long)]
    pub force: bool,

    #[command(flatten)]
    pub setting_args: SettingArgs,

    #[command(flatten)]
    pub baseline_args: BaselineArgs,

    /// Print compact machine-readable summary (for CI wrappers)
    #[arg(long)]
    pub json_summary: bool,
}

impl LintArgs {
    pub(crate) fn output_descriptor(
        &self,
        output_file_mode: CommandOutputFileMode,
    ) -> CommandOutputDescriptor {
        CommandOutputDescriptor::json_envelope(CommandJsonFamily::Quality, output_file_mode)
    }

    pub(crate) fn lab_contract(&self) -> Option<LabCommandContract> {
        if self.is_full_workspace_run() {
            return Some(
                LabCommandContract::portable(
                    LINT_LAB_LABEL,
                    self.fix.then_some("--fix"),
                    true,
                    &[],
                )
                .release_gate(),
            );
        }

        (self.changed_since.is_some() || self.changed_only).then(|| {
            LabCommandContract::local_only(
                LINT_LAB_LABEL,
                LINT_CHANGED_SCOPE_LAB_UNSUPPORTED_REASON,
            )
        })
    }

    pub fn is_full_workspace_run(&self) -> bool {
        self.changed_since.is_none()
            && !self.changed_only
            && self.file.is_none()
            && self.glob.is_none()
    }

    /// Positional component id targeted by this run, if any (the
    /// `homeboy lint <component>` form). Returns `None` when the component is
    /// auto-detected from the working directory.
    pub fn lab_offload_positional_component(&self) -> Option<String> {
        self.comp.component.clone()
    }

    /// Whether an explicit `--path` override was supplied. When present, the
    /// offload pipeline already syncs and remaps that path, so component-id
    /// based source resolution must not override it.
    pub fn lab_offload_has_path_override(&self) -> bool {
        self.comp.path.is_some()
    }
}

pub fn run(args: LintArgs, _global: &GlobalArgs) -> CmdResult<LintCommandOutput> {
    let source_ctx = resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        None,
    )?;

    if !args.fix
        && args.ci_job.is_none()
        && source_ctx.component.has_script(ExtensionCapability::Lint)
    {
        let runner =
            ObservedWorkflowRunner::create(format!("lint {} self-check", source_ctx.component_id))?;
        let observation = LintObservationAdapter::new(
            source_ctx.component_id.clone(),
            &source_ctx.source_path,
            lint_command_label(&source_ctx.component_id, &args),
            Some(runner.run_dir()),
        );
        let active_observation = ActiveObservation::start_best_effort(observation.start_record());
        let workflow = run_self_check_lint_workflow_with_progress(
            &source_ctx.component,
            &source_ctx.source_path,
            source_ctx.component_id.clone(),
            args.json_summary,
            Some(runner.run_dir()),
            active_observation.as_ref(),
        );
        let workflow = runner.finish(
            active_observation,
            workflow,
            |active, workflow| finish_lint_observation(active, &observation, workflow),
            |active, error| finish_lint_observation_error(active, &observation, error),
        )?;

        return Ok(report::from_main_workflow(workflow));
    }

    let ctx = resolve_source_context(
        &args.comp,
        &args.setting_args,
        &args.extension_override,
        Some(ExtensionCapability::Lint),
    )?;
    let effective_id = ctx.component_id.clone();
    let ci_job = resolve_ci_job_for_command(args.ci_job.as_deref(), &ctx.component, "lint")?;

    let typed_settings = ctx.resolved_settings().typed_overrides();

    // --fix dispatches to the canonical refactor sources pipeline.
    // The fixer pipeline already exists; this flag connects the existing wire
    // so users don't have to re-type `homeboy refactor <component> --from lint
    // --write` to resolve the auto-fix CTA.
    if args.fix {
        return run_fix(args, &ctx, effective_id, typed_settings);
    }

    let runner = ObservedWorkflowRunner::create(format!("lint {}", effective_id))?;
    let observation = LintObservationAdapter::new(
        ctx.component_id.clone(),
        &ctx.source_path,
        lint_command_label(&effective_id, &args),
        Some(runner.run_dir()),
    );

    let workflow = run_main_lint_workflow(
        &ctx.component,
        &ctx.source_path,
        LintRunWorkflowArgs {
            component_label: effective_id.clone(),
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
            settings: typed_settings,
            summary: args.summary,
            file: args.file.clone(),
            glob: args.glob.clone(),
            changed_only: args.changed_only,
            changed_since: args.changed_since.clone(),
            precomputed_changed_files: args.precomputed_changed_files.clone(),
            sniff_filters: args.sniff_filters.to_lint_sniff_filters(),
            category: args.category.clone(),
            ci_env: ci_profile::ci_job_env(ci_job.as_ref()),
            baseline_flags: homeboy::core::engine::baseline::BaselineFlags {
                baseline: args.baseline_args.baseline,
                ignore_baseline: args.baseline_args.ignore_baseline,
                ratchet: args.baseline_args.ratchet,
            },
            json_summary: args.json_summary,
        },
        runner.run_dir(),
    );
    let workflow = runner.finish_adapted(observation, workflow)?;

    Ok(report::from_main_workflow_with_ci_context(
        workflow,
        ci_profile::ci_context_for_job(ci_job.as_ref(), None),
    ))
}

struct LintObservationAdapter {
    component_id: String,
    source_path: std::path::PathBuf,
    command: String,
    run_dir: Option<std::path::PathBuf>,
}

impl LintObservationAdapter {
    fn new(
        component_id: String,
        source_path: &std::path::Path,
        command: String,
        run_dir: Option<&homeboy::core::engine::run_dir::RunDir>,
    ) -> Self {
        Self {
            component_id,
            source_path: source_path.to_path_buf(),
            command,
            run_dir: run_dir.map(|run_dir| run_dir.path().to_path_buf()),
        }
    }

    fn run_dir_metadata(&self) -> serde_json::Value {
        self.run_dir
            .as_ref()
            .and_then(|path| {
                homeboy::core::engine::run_dir::RunDir::from_existing(path.clone()).ok()
            })
            .map(|run_dir| validation_progress_metadata(&run_dir))
            .unwrap_or_else(|| serde_json::json!({}))
    }
}

impl WorkflowObservationAdapter<homeboy::core::extension::lint::LintRunWorkflowResult>
    for LintObservationAdapter
{
    fn start_record(&self) -> NewRunRecord {
        NewRunRecord::builder("lint")
            .component_id(self.component_id.clone())
            .command(self.command.clone())
            .cwd_path(&self.source_path)
            .current_homeboy_version()
            .metadata(serde_json::json!({
                "source": "homeboy lint",
            }))
            .build()
    }

    fn success_status(
        &self,
        workflow: &homeboy::core::extension::lint::LintRunWorkflowResult,
    ) -> RunStatus {
        if workflow.status == "passed" {
            RunStatus::Pass
        } else {
            RunStatus::Fail
        }
    }

    fn success_metadata(
        &self,
        workflow: &homeboy::core::extension::lint::LintRunWorkflowResult,
    ) -> serde_json::Value {
        serde_json::json!({
            "exit_code": workflow.exit_code,
            "finding_count": workflow.findings.as_ref().map(Vec::len).unwrap_or(0),
            "producer_summaries": workflow.producer_summaries,
        })
    }

    fn success_findings(
        &self,
        run_id: &str,
        workflow: &homeboy::core::extension::lint::LintRunWorkflowResult,
    ) -> Vec<NewFindingRecord> {
        workflow
            .findings
            .as_ref()
            .map(|findings| finding_records_from_lint(run_id, findings))
            .unwrap_or_default()
    }
}

fn finish_lint_observation(
    active: ActiveObservation,
    adapter: &LintObservationAdapter,
    workflow: &homeboy::core::extension::lint::LintRunWorkflowResult,
) {
    let mut metadata = merge_metadata(
        active.initial_metadata().clone(),
        adapter.success_metadata(workflow),
    );
    metadata = merge_metadata(metadata, adapter.run_dir_metadata());
    let findings = adapter.success_findings(active.run_id(), workflow);
    active.record_findings(&findings);
    active.finish(adapter.success_status(workflow), Some(metadata));
}

fn finish_lint_observation_error(
    active: ActiveObservation,
    adapter: &LintObservationAdapter,
    error: &homeboy::core::Error,
) {
    let mut metadata = merge_metadata(
        active.initial_metadata().clone(),
        serde_json::json!({
            "observation_status": "error",
            "error": error.to_string(),
        }),
    );
    metadata = merge_metadata(metadata, adapter.run_dir_metadata());
    active.finish_error(Some(metadata));
}

fn lint_command_label(component_id: &str, args: &LintArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "lint".to_string(),
        component_id.to_string(),
    ];
    if let Some(path) = &args.comp.path {
        parts.push("--path".to_string());
        parts.push(path.clone());
    }
    if let Some(file) = &args.file {
        parts.push("--file".to_string());
        parts.push(file.clone());
    }
    if let Some(glob) = &args.glob {
        parts.push("--glob".to_string());
        parts.push(glob.clone());
    }
    if args.changed_only {
        parts.push("--changed-only".to_string());
    }
    if let Some(changed_since) = &args.changed_since {
        parts.push("--changed-since".to_string());
        parts.push(changed_since.clone());
    }
    if args.force {
        parts.push("--force".to_string());
    }
    parts.join(" ")
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
    ctx: &homeboy::core::engine::execution_context::ExecutionContext,
    component_label: String,
    settings: Vec<(String, serde_json::Value)>,
) -> CmdResult<LintCommandOutput> {
    let selected_files = if args.changed_only {
        let changes = git::get_uncommitted_changes(&ctx.component.local_path)?;
        let mut files = Vec::new();
        files.extend(changes.staged);
        files.extend(changes.unstaged);
        files.extend(changes.untracked);
        Some(files)
    } else {
        None
    };

    let lint_options = LintSourceOptions {
        selected_files,
        file: args.file.clone(),
        glob: args.glob.clone(),
        sniff_filters: args.sniff_filters.to_lint_sniff_filters(),
        category: args.category.clone(),
    };

    let mut request = lint_refactor_request(
        ctx.component.clone(),
        ctx.source_path.clone(),
        settings_to_legacy_strings(settings),
        lint_options,
        true,
    );
    request.changed_since = args.changed_since.clone();
    request.force = args.force;

    let run = collect_refactor_sources(request)?;

    Ok(report::from_lint_fix(component_label, run))
}

fn settings_to_legacy_strings(settings: Vec<(String, serde_json::Value)>) -> Vec<(String, String)> {
    settings
        .into_iter()
        .map(|(key, value)| {
            let value = match value {
                serde_json::Value::String(value) => value,
                value => value.to_string(),
            };
            (key, value)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{LintArgs, LintObservationAdapter};
    use crate::commands::utils::observed_workflow::WorkflowObservationAdapter;
    use clap::{CommandFactory, Parser};
    use homeboy::core::component::Component;
    use homeboy::core::engine::run_dir::RunDir;
    use homeboy::core::extension::lint as extension_lint;
    use homeboy::core::extension::lint::report;
    use homeboy::core::refactor::plan::{
        lint_refactor_request, LintSourceOptions, RefactorSourceRun, SourceTotals,
    };

    #[derive(Parser)]
    struct TestCli {
        #[command(flatten)]
        lint: LintArgs,
    }

    #[test]
    fn parses_one_shot_extension_override() {
        let cli = TestCli::try_parse_from([
            "lint",
            "--path",
            "/tmp/repo",
            "--extension",
            "fixture-lint",
            "--changed-since",
            "origin/main",
        ])
        .expect("lint should parse --extension override");

        assert_eq!(cli.lint.extension_override.extensions, vec!["fixture-lint"]);
        assert_eq!(cli.lint.changed_since.as_deref(), Some("origin/main"));
    }

    #[test]
    fn parses_json_summary_flag() {
        let cli = TestCli::try_parse_from(["lint", "homeboy", "--json-summary"])
            .expect("lint should parse --json-summary");

        assert!(cli.lint.json_summary);
    }

    #[test]
    fn parses_ci_job_flag() {
        let cli = TestCli::try_parse_from(["lint", "homeboy", "--ci-job", "lint-typecheck"])
            .expect("lint should parse --ci-job");

        assert_eq!(cli.lint.ci_job.as_deref(), Some("lint-typecheck"));
    }

    #[test]
    fn lint_help_documents_changed_since_once_as_read_only_scope() {
        let help = TestCli::command().render_long_help().to_string();

        assert_eq!(help.matches("--changed-since").count(), 1, "{help}");
        assert!(
            help.contains("Lint only files changed since a git ref (branch, tag, or SHA)"),
            "{help}"
        );
        assert!(
            help.contains(
                "Apply auto-fixable lint findings in place using the lint fixer pipeline"
            ),
            "{help}"
        );
        assert!(
            !help.contains("--from lint --write"),
            "lint --help should not describe --changed-since or --fix as a refactor --write alias: {help}"
        );
    }

    #[test]
    fn lint_observation_keeps_run_dir_out_of_initial_metadata() {
        let dir = tempfile::tempdir().expect("temp dir");
        let run_dir = RunDir::create().expect("run dir");
        let adapter = LintObservationAdapter::new(
            "homeboy".to_string(),
            dir.path(),
            "homeboy lint homeboy".to_string(),
            Some(&run_dir),
        );

        let record = <LintObservationAdapter as WorkflowObservationAdapter<
            extension_lint::LintRunWorkflowResult,
        >>::start_record(&adapter);

        assert!(record.metadata_json.get("run_dir").is_none());
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
                sniff_filters: homeboy::core::extension::lint::LintSniffFilters {
                    errors_only: true,
                    sniffs: Some("WordPress.Security".to_string()),
                    exclude_sniffs: Some("WordPress.WhiteSpace".to_string()),
                },
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
