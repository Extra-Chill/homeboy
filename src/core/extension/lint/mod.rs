pub mod baseline;
pub mod report;
pub mod run;

use crate::component::Component;
use crate::extension::{ExtensionCapability, ExtensionExecutionContext, ExtensionRunner};

pub use baseline::{BaselineComparison, LintBaseline, LintBaselineMetadata, LintFinding};
pub use report::LintCommandOutput;
pub use run::{
    run_main_lint_workflow, run_self_check_lint_workflow, LintRunWorkflowArgs,
    LintRunWorkflowResult,
};

use crate::engine::run_dir::RunDir;

pub fn resolve_lint_command(
    component: &Component,
) -> crate::error::Result<ExtensionExecutionContext> {
    crate::extension::resolve_execution_context(component, ExtensionCapability::Lint)
}

#[allow(clippy::too_many_arguments)]
pub fn build_lint_runner(
    component: &Component,
    path_override: Option<String>,
    settings: &[(String, String)],
    summary: bool,
    file: Option<&str>,
    glob: Option<&str>,
    errors_only: bool,
    sniffs: Option<&str>,
    exclude_sniffs: Option<&str>,
    category: Option<&str>,
    run_dir: &RunDir,
) -> crate::Result<ExtensionRunner> {
    let resolved = resolve_lint_command(component)?;

    Ok(ExtensionRunner::for_context(resolved)
        .component(component.clone())
        .path_override(path_override)
        .settings(settings)
        .with_run_dir(run_dir)
        .env_if(summary, "HOMEBOY_SUMMARY_MODE", "1")
        .env_opt("HOMEBOY_LINT_FILE", &file.map(str::to_string))
        .env_opt("HOMEBOY_LINT_GLOB", &glob.map(str::to_string))
        .env_if(errors_only, "HOMEBOY_ERRORS_ONLY", "1")
        .env_opt("HOMEBOY_SNIFFS", &sniffs.map(str::to_string))
        .env_opt(
            "HOMEBOY_EXCLUDE_SNIFFS",
            &exclude_sniffs.map(str::to_string),
        )
        .env_opt("HOMEBOY_CATEGORY", &category.map(str::to_string)))
}
