//! Auto-fix hint assembly — renders the `homeboy review lint --fix` CTA while
//! preserving the active scope flags.

use super::types::LintRunWorkflowArgs;
use homeboy_engine_primitives::shell;

pub(super) fn build_autofix_hint(args: &LintRunWorkflowArgs) -> String {
    format!("Auto-fix: {}", lint_fix_command(args))
}

fn lint_fix_command(args: &LintRunWorkflowArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "review".to_string(),
        "lint".to_string(),
        args.component_label.clone(),
    ];

    if let Some(path) = &args.path_override {
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
    parts.push("--fix".to_string());

    shell::quote_args(&parts)
}
