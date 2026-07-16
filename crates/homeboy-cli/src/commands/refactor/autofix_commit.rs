use std::path::Path;

use homeboy::core::git::{
    commit_staged_with_author, configure_identity, has_staged_changes, parse_git_identity,
    stage_all,
};

const AUTOFIX_PREFIX: &str = "chore(ci): homeboy autofix";

/// Stage all changes and create a commit after refactor --write.
pub(super) fn commit_refactor_sources(
    path: &str,
    sources: &homeboy::core::refactor::plan::RefactorSourceRun,
    git_identity: Option<&str>,
) -> homeboy::core::Result<()> {
    stage_all(Path::new(path))?;

    // Check if there's anything staged (fixes may have been no-ops after formatting)
    if !has_staged_changes(Path::new(path))? {
        eprintln!("[refactor] No staged changes after git add — skipping commit");
        return Ok(());
    }

    let identity = parse_git_identity(git_identity);
    configure_identity(path, &identity)?;

    let message = build_autofix_commit_message(sources);
    let author = format!("{} <{}>", identity.name, identity.email);
    commit_staged_with_author(Path::new(path), &message, &author)?;

    eprintln!(
        "[refactor] Committed autofix: {} files changed",
        sources.files_modified
    );
    Ok(())
}

/// Build a structured commit message from refactor results.
///
/// Format matches the homeboy-action convention:
/// ```text
/// chore(ci): homeboy autofix — refactor (5 files, 12 fixes)
///
/// Unused imports removed: 5 fixes (3 files)
/// Dead code removed: 4 fixes (2 files)
/// ...
/// ```
fn build_autofix_commit_message(
    sources: &homeboy::core::refactor::plan::RefactorSourceRun,
) -> String {
    let source_labels: Vec<&str> = sources.sources.iter().map(|s| s.as_str()).collect();
    let source_desc = source_labels.join(", ");

    let total_fixes = sources
        .fix_summary
        .as_ref()
        .map(|s| s.fixes_applied)
        .unwrap_or(0);

    let subject = if total_fixes > 0 {
        format!(
            "{AUTOFIX_PREFIX} — {source_desc} ({} files, {total_fixes} fixes)",
            sources.files_modified
        )
    } else {
        format!(
            "{AUTOFIX_PREFIX} — {source_desc} ({} files)",
            sources.files_modified
        )
    };

    let mut body_lines = Vec::new();
    if let Some(ref summary) = sources.fix_summary {
        for rule in &summary.rules {
            body_lines.push(format!("{}: {} fixes", rule.rule, rule.count));
        }
    }

    if body_lines.is_empty() {
        for file in &sources.changed_files {
            body_lines.push(file.clone());
        }
    }

    if body_lines.is_empty() {
        subject
    } else {
        format!("{subject}\n\n{}", body_lines.join("\n"))
    }
}
