//! Auto-fix hint assembly — renders the `homeboy refactor --from lint --write`
//! CTA while preserving the active scope flags.

use super::types::LintRunWorkflowArgs;
use homeboy_engine_primitives::shell;

pub(super) fn build_autofix_hint(args: &LintRunWorkflowArgs) -> String {
    format!("Auto-fix: {}", refactor_autofix_command(args))
}

fn refactor_autofix_command(args: &LintRunWorkflowArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "refactor".to_string(),
        args.component_label.clone(),
    ];

    append_path_and_changed_since_args(&mut parts, args);
    parts.extend([
        "--from".to_string(),
        "lint".to_string(),
        "--write".to_string(),
    ]);

    shell::quote_args(&parts)
}

fn append_path_and_changed_since_args(parts: &mut Vec<String>, args: &LintRunWorkflowArgs) {
    if let Some(path) = &args.path_override {
        parts.push("--path".to_string());
        parts.push(path.clone());
    }
    if let Some(changed_since) = &args.changed_since {
        parts.push("--changed-since".to_string());
        parts.push(changed_since.clone());
    }
}
