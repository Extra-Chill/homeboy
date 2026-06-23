//! Auto-fix hint assembly — renders the `homeboy lint --fix` / `homeboy
//! refactor --from lint --write` CTAs while preserving the active scope flags.

use super::types::LintRunWorkflowArgs;
use crate::core::engine::shell;

pub(super) fn build_autofix_hint(args: &LintRunWorkflowArgs) -> String {
    let lint_command = lint_autofix_command(args);

    if refactor_can_preserve_scope(args) {
        let refactor_command = refactor_autofix_command(args);
        format!("Auto-fix: {lint_command} (or {refactor_command})")
    } else {
        format!("Auto-fix: {lint_command}")
    }
}

fn lint_autofix_command(args: &LintRunWorkflowArgs) -> String {
    let mut parts = vec![
        "homeboy".to_string(),
        "lint".to_string(),
        args.component_label.clone(),
    ];

    append_common_scope_args(&mut parts, args);
    parts.push("--fix".to_string());

    shell::quote_args(&parts)
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

fn refactor_can_preserve_scope(args: &LintRunWorkflowArgs) -> bool {
    args.file.is_none() && args.glob.is_none() && !args.changed_only
}

fn append_common_scope_args(parts: &mut Vec<String>, args: &LintRunWorkflowArgs) {
    append_path_and_changed_since_args(parts, args);
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
