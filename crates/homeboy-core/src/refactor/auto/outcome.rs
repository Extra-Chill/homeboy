use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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

#[derive(Debug, Clone)]
pub struct AutofixSidecarFiles {
    pub results_file: PathBuf,
}

#[derive(Debug, Clone)]
pub struct AppliedAutofixCapture {
    pub files_modified: usize,
    pub fix_results: Vec<FixApplied>,
    pub fix_summary: Option<FixResultsSummary>,
}

/// A single fix applied by an extension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixApplied {
    pub file: String,
    pub rule: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primitive: Option<String>,
}

/// Aggregate summary of extension fix results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixResultsSummary {
    pub fixes_applied: usize,
    pub files_modified: usize,
    pub rules: Vec<RuleFixCount>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub primitives: Vec<PrimitiveFixCount>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFixCount {
    pub rule: String,
    pub count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrimitiveFixCount {
    pub primitive: String,
    pub count: usize,
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
