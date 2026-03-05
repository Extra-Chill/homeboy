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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_with_changes_returns_preview_status() {
        let outcome = standard_outcome(
            AutofixMode::DryRun,
            3,
            Some("homeboy test homeboy --analyze".to_string()),
            vec!["base hint".to_string()],
        );

        assert_eq!(outcome.status, "auto_fix_preview");
        assert!(!outcome.rerun_recommended);
        assert!(outcome.hints.iter().any(|h| h.contains("Dry-run only")));
    }

    #[test]
    fn write_with_changes_recommends_rerun() {
        let outcome = standard_outcome(
            AutofixMode::Write,
            2,
            Some("homeboy test homeboy --analyze".to_string()),
            vec![],
        );

        assert_eq!(outcome.status, "auto_fixed");
        assert!(outcome.rerun_recommended);
        assert!(outcome
            .hints
            .iter()
            .any(|h| h.contains("Re-run checks: homeboy test homeboy --analyze")));
    }

    #[test]
    fn no_changes_returns_noop() {
        let outcome = standard_outcome(AutofixMode::Write, 0, None, vec![]);

        assert_eq!(outcome.status, "auto_fix_noop");
        assert!(!outcome.rerun_recommended);
    }
}
