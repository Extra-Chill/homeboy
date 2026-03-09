//! Shared autofix outcome primitives.
//!
//! Commands with `--fix` behavior can use this to return consistent status and
//! next-step hints without reimplementing decision logic.
//!
//! ## Extension fix results protocol
//!
//! Extensions report what they fixed via a sidecar JSON file. The calling
//! command sets `HOMEBOY_FIX_RESULTS_FILE` to a temp path; the extension writes
//! a JSON array of [`FixApplied`] entries. After execution, the command reads
//! the file with [`parse_fix_results_file`] and includes the structured data in
//! its output.
//!
//! Planning uses the same shape via `HOMEBOY_FIX_PLAN_FILE` so callers can
//! inspect proposed fixes without mutating the real working tree.

use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::Path;

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

// ============================================================================
// Extension fix results sidecar
// ============================================================================

/// A single fix applied by an extension.
///
/// Extensions write an array of these to `HOMEBOY_FIX_RESULTS_FILE`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixApplied {
    /// File that was modified (relative to component root).
    pub file: String,

    /// Rule or fixer that produced this fix (e.g., "phpcs:WordPress.Security.EscapeOutput",
    /// "yoda-condition", "phpcbf").
    pub rule: String,

    /// What the fixer did (e.g., "rewrite", "add-ignore", "remove").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<String>,
}

/// Aggregate summary of extension fix results.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FixResultsSummary {
    /// Total number of individual fixes applied.
    pub fixes_applied: usize,

    /// Number of distinct files modified.
    pub files_modified: usize,

    /// Distinct rules that produced fixes, with count per rule.
    pub rules: Vec<RuleFixCount>,
}

/// Fix count for a single rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RuleFixCount {
    pub rule: String,
    pub count: usize,
}

/// Read and parse the extension fix results sidecar file.
///
/// Returns an empty vec if the file doesn't exist or is empty — this keeps
/// backward compatibility with extensions that don't write fix results yet.
pub fn parse_fix_results_file(path: &Path) -> Vec<FixApplied> {
    if !path.exists() {
        return Vec::new();
    }

    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    if content.trim().is_empty() {
        return Vec::new();
    }

    serde_json::from_str(&content).unwrap_or_default()
}

/// Read and parse the extension fix plan sidecar file.
///
/// The plan format intentionally matches [`FixApplied`] so fix planners and
/// applied fix summaries can share the same transport shape.
pub fn parse_fix_plan_file(path: &Path) -> Vec<FixApplied> {
    parse_fix_results_file(path)
}

/// Summarize a list of fix results into aggregate counts.
pub fn summarize_fix_results(fixes: &[FixApplied]) -> FixResultsSummary {
    use std::collections::{BTreeMap, HashSet};

    let mut files = HashSet::new();
    let mut rule_counts: BTreeMap<String, usize> = BTreeMap::new();

    for fix in fixes {
        files.insert(fix.file.clone());
        *rule_counts.entry(fix.rule.clone()).or_insert(0) += 1;
    }

    let rules = rule_counts
        .into_iter()
        .map(|(rule, count)| RuleFixCount { rule, count })
        .collect();

    FixResultsSummary {
        fixes_applied: fixes.len(),
        files_modified: files.len(),
        rules,
    }
}

/// Bridge a Rust-native `code_audit::fixer::FixResult` into the universal
/// `FixResultsSummary` format. This lets audit --fix output the same summary
/// structure as lint --fix and test --fix (which use extension sidecars).
pub fn summarize_audit_fix_result(
    fix_result: &crate::code_audit::fixer::FixResult,
) -> FixResultsSummary {
    use std::collections::{BTreeMap, HashSet};

    let mut files = HashSet::new();
    let mut rule_counts: BTreeMap<String, usize> = BTreeMap::new();
    let mut total_fixes = 0usize;

    for fix in &fix_result.fixes {
        if !fix.applied {
            continue;
        }
        files.insert(fix.file.clone());
        for insertion in &fix.insertions {
            if insertion.auto_apply {
                let rule = format!("{:?}", insertion.finding).to_lowercase();
                *rule_counts.entry(rule).or_insert(0) += 1;
                total_fixes += 1;
            }
        }
    }

    for new_file in &fix_result.new_files {
        if new_file.written {
            files.insert(new_file.file.clone());
            let rule = format!("{:?}", new_file.finding).to_lowercase();
            *rule_counts.entry(rule).or_insert(0) += 1;
            total_fixes += 1;
        }
    }

    let rules = rule_counts
        .into_iter()
        .map(|(rule, count)| RuleFixCount { rule, count })
        .collect();

    FixResultsSummary {
        fixes_applied: total_fixes,
        files_modified: files.len(),
        rules,
    }
}

/// Generate a unique temp file path for fix results sidecar.
pub fn fix_results_temp_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "homeboy-fix-results-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

/// Generate a unique temp file path for fix plan sidecar.
pub fn fix_plan_temp_path() -> std::path::PathBuf {
    std::env::temp_dir().join(format!(
        "homeboy-fix-plan-{}-{}.json",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ))
}

// ============================================================================
// Git change tracking for autofix file-count reporting
// ============================================================================

/// Snapshot uncommitted files in a working tree. Called before and after a fix
/// pass to compute how many files the fixer modified.
///
/// In test builds the real git call is replaced with a stub that returns an
/// empty set for existing directories (avoids needing a real git repo).
#[cfg(not(test))]
pub fn changed_file_set(local_path: &str) -> crate::Result<HashSet<String>> {
    let uncommitted = crate::git::get_uncommitted_changes(local_path)?;
    let mut files = HashSet::new();
    files.extend(uncommitted.staged);
    files.extend(uncommitted.unstaged);
    files.extend(uncommitted.untracked);
    Ok(files)
}

#[cfg(test)]
pub fn changed_file_set(local_path: &str) -> crate::Result<HashSet<String>> {
    let path = Path::new(local_path);
    if path.exists() {
        Ok(HashSet::new())
    } else {
        crate::git::get_uncommitted_changes(local_path).map(|changes| {
            let mut files = HashSet::new();
            files.extend(changes.staged);
            files.extend(changes.unstaged);
            files.extend(changes.untracked);
            files
        })
    }
}

/// Count files that appeared after a fix pass (present in `after` but not `before`).
pub fn count_newly_changed(before: &HashSet<String>, after: &HashSet<String>) -> usize {
    after.difference(before).count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn parse_fix_results_missing_file() {
        let results = parse_fix_results_file(Path::new("/tmp/definitely-missing-fix-results.json"));
        assert!(results.is_empty());
    }

    #[test]
    fn parse_fix_results_empty_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fix-results.json");
        std::fs::write(&path, "").expect("write");
        let results = parse_fix_results_file(&path);
        assert!(results.is_empty());
    }

    #[test]
    fn parse_fix_results_valid_json() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fix-results.json");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"[
                {{"file": "src/foo.php", "rule": "yoda-condition", "action": "rewrite"}},
                {{"file": "src/bar.php", "rule": "phpcbf"}},
                {{"file": "src/foo.php", "rule": "phpcbf"}}
            ]"#
        )
        .expect("write");

        let results = parse_fix_results_file(&path);
        assert_eq!(results.len(), 3);
        assert_eq!(results[0].file, "src/foo.php");
        assert_eq!(results[0].rule, "yoda-condition");
        assert_eq!(results[0].action.as_deref(), Some("rewrite"));
        assert!(results[1].action.is_none());
    }

    #[test]
    fn summarize_fix_results_aggregates_correctly() {
        let fixes = vec![
            FixApplied {
                file: "src/a.php".into(),
                rule: "yoda".into(),
                action: None,
            },
            FixApplied {
                file: "src/b.php".into(),
                rule: "phpcbf".into(),
                action: None,
            },
            FixApplied {
                file: "src/a.php".into(),
                rule: "phpcbf".into(),
                action: None,
            },
        ];

        let summary = summarize_fix_results(&fixes);
        assert_eq!(summary.fixes_applied, 3);
        assert_eq!(summary.files_modified, 2); // a.php and b.php
        assert_eq!(summary.rules.len(), 2); // phpcbf and yoda
                                            // BTreeMap ordering: phpcbf before yoda
        assert_eq!(summary.rules[0].rule, "phpcbf");
        assert_eq!(summary.rules[0].count, 2);
        assert_eq!(summary.rules[1].rule, "yoda");
        assert_eq!(summary.rules[1].count, 1);
    }

    #[test]
    fn parse_fix_results_malformed_json_returns_empty() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("fix-results.json");
        std::fs::write(&path, "not valid json").expect("write");
        let results = parse_fix_results_file(&path);
        assert!(results.is_empty());
    }

    #[test]
    fn standard_outcome_write_with_fixes() {
        let outcome = standard_outcome(
            AutofixMode::Write,
            3,
            Some("homeboy lint foo".into()),
            vec![],
        );
        assert_eq!(outcome.status, "auto_fixed");
        assert!(outcome.rerun_recommended);
    }

    #[test]
    fn standard_outcome_noop() {
        let outcome = standard_outcome(AutofixMode::Write, 0, None, vec![]);
        assert_eq!(outcome.status, "auto_fix_noop");
        assert!(!outcome.rerun_recommended);
    }

    #[test]
    fn count_newly_changed_only_counts_new_entries() {
        let before = HashSet::from([
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "README.md".to_string(),
        ]);
        let after = HashSet::from([
            "src/a.rs".to_string(),
            "src/b.rs".to_string(),
            "README.md".to_string(),
            "src/c.rs".to_string(),
            "tests/a_test.rs".to_string(),
        ]);

        assert_eq!(count_newly_changed(&before, &after), 2);
    }

    #[test]
    fn changed_file_set_returns_empty_for_existing_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().to_string_lossy().to_string();
        let result = changed_file_set(&path);
        assert!(result.is_ok());
        assert!(result.unwrap().is_empty());
    }
}
