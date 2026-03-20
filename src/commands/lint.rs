use clap::Args;

use homeboy::engine::execution_context::{self, ResolveOptions};
use homeboy::extension::lint::{
    report, run_main_lint_workflow, LintCommandOutput, LintRunWorkflowArgs,
};
use homeboy::extension::ExtensionCapability;

use super::utils::args::{BaselineArgs, HiddenJsonArgs, PositionalComponentArgs, SettingArgs};
use super::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct LintArgs {
    #[command(flatten)]
    comp: PositionalComponentArgs,

    /// Show compact summary instead of full output
    #[arg(long)]
    summary: bool,

    /// Lint only a single file (path relative to component root)
    #[arg(long)]
    file: Option<String>,

    /// Lint only files matching glob pattern (e.g., "inc/**/*.php")
    #[arg(long)]
    glob: Option<String>,

    /// Lint only files modified in the working tree (staged, unstaged, untracked)
    #[arg(long, conflicts_with = "changed_since")]
    changed_only: bool,

    /// Lint only files changed since a git ref (branch, tag, or SHA) — CI-friendly
    #[arg(long, conflicts_with = "changed_only")]
    changed_since: Option<String>,

    /// Show only errors, suppress warnings
    #[arg(long)]
    errors_only: bool,

    /// Only check specific sniffs (comma-separated codes)
    #[arg(long)]
    sniffs: Option<String>,

    /// Exclude sniffs from checking (comma-separated codes)
    #[arg(long)]
    exclude_sniffs: Option<String>,

    /// Filter by category: security, i18n, yoda, whitespace
    #[arg(long)]
    category: Option<String>,

    #[command(flatten)]
    setting_args: SettingArgs,

    #[command(flatten)]
    baseline_args: BaselineArgs,

    #[command(flatten)]
    _json: HiddenJsonArgs,
}

pub fn run(args: LintArgs, _global: &GlobalArgs) -> CmdResult<LintCommandOutput> {
    let ctx = execution_context::resolve(&ResolveOptions::with_capability(
        args.comp.id(),
        args.comp.path.clone(),
        ExtensionCapability::Lint,
        args.setting_args.setting.clone(),
    ))?;

    let workflow = run_main_lint_workflow(
        &ctx.component,
        &ctx.source_path,
        LintRunWorkflowArgs {
            component_label: args.comp.component.clone(),
            component_id: ctx.component_id.clone(),
            path_override: args.comp.path.clone(),
            settings: ctx
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
                .collect(),
            summary: args.summary,
            file: args.file.clone(),
            glob: args.glob.clone(),
            changed_only: args.changed_only,
            changed_since: args.changed_since.clone(),
            errors_only: args.errors_only,
            sniffs: args.sniffs.clone(),
            exclude_sniffs: args.exclude_sniffs.clone(),
            category: args.category.clone(),
            baseline: args.baseline_args.baseline,
            ignore_baseline: args.baseline_args.ignore_baseline,
        },
    )?;

    Ok(report::from_main_workflow(workflow))
}

#[cfg(test)]
mod tests {
    use homeboy::component::Component;
    use homeboy::extension::lint as extension_lint;
    use homeboy::extension::lint::baseline::{self as lint_baseline, LintFinding};
    use homeboy::refactor::lint_refactor_request;
    use homeboy::refactor::LintSourceOptions;
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
