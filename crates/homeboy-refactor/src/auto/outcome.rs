use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AutofixMode {
    DryRun,
    Write,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AutofixOutcome {
    pub status: String,
    pub rerun_recommended: bool,
    pub hints: Vec<String>,
}

#[derive(Debug, Clone)]
pub(crate) struct AutofixSidecarFiles {
    pub results_file: PathBuf,
}

/// A single fix applied by an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct FixApplied {
    pub file: String,
    pub rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive: Option<String>,
}

/// Aggregate summary of extension fix results.
pub(crate) use homeboy_refactor_contract::{FixResultsSummary, PrimitiveFixCount, RuleFixCount};

pub(crate) fn standard_outcome(
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
