//! Shared autofix outcome primitives.
//!
//! Commands with `--fix` behavior can use this to return consistent status and
//! next-step hints without reimplementing decision logic.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AutofixMode {
    DryRun,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutofixOutcome {
    pub status: String,
    pub rerun_recommended: bool,
    pub hints: Vec<String>,
}

pub fn standard_outcome(
    mode: AutofixMode,
    replacements: usize,
    rerun_command: Option<String>,
    mut hints: Vec<String>,
) -> AutofixOutcome {
    let status = if replacements > 0 {
        match mode {
            AutofixMode::Write => "auto_fixed",
            AutofixMode::DryRun => "auto_fix_preview",
        }
    } else {
        "auto_fix_noop"
    }
    .to_string();

    let rerun_recommended = mode == AutofixMode::Write && replacements > 0;

    if replacements > 0 {
        match mode {
            AutofixMode::DryRun => {
                hints.push(
                    "Dry-run only. Re-run with --write to apply generated fixes.".to_string(),
                );
            }
            AutofixMode::Write => {
                if let Some(cmd) = rerun_command {
                    hints.push(format!("Re-run checks: {}", cmd));
                }
            }
        }
    }

    AutofixOutcome {
        status,
        rerun_recommended,
        hints,
    }
}
