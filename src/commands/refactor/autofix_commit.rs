use std::process::Command;

const BOT_NAME: &str = "homeboy-ci[bot]";
const BOT_EMAIL: &str = "266378653+homeboy-ci[bot]@users.noreply.github.com";
const AUTOFIX_PREFIX: &str = "chore(ci): homeboy autofix";

/// Stage all changes and create a commit after refactor --write.
pub(super) fn commit_refactor_sources(
    path: &str,
    sources: &homeboy::core::refactor::plan::RefactorSourceRun,
    git_identity: Option<&str>,
) -> homeboy::core::Result<()> {
    let add = Command::new("git")
        .args(["add", "-A"])
        .current_dir(path)
        .output()
        .map_err(|e| homeboy::core::Error::git_command_failed(format!("git add: {e}")))?;
    if !add.status.success() {
        let stderr = String::from_utf8_lossy(&add.stderr);
        return Err(homeboy::core::Error::git_command_failed(format!(
            "git add -A failed: {stderr}"
        )));
    }

    // Check if there's anything staged (fixes may have been no-ops after formatting)
    let diff_check = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(path)
        .output()
        .map_err(|e| homeboy::core::Error::git_command_failed(format!("git diff: {e}")))?;
    if diff_check.status.success() {
        eprintln!("[refactor] No staged changes after git add — skipping commit");
        return Ok(());
    }

    let (name, email) = resolve_git_identity(git_identity);

    for (key, value) in [("user.name", name.as_str()), ("user.email", email.as_str())] {
        let config = Command::new("git")
            .args(["config", key, value])
            .current_dir(path)
            .output()
            .map_err(|e| homeboy::core::Error::git_command_failed(format!("git config: {e}")))?;
        if !config.status.success() {
            let stderr = String::from_utf8_lossy(&config.stderr);
            return Err(homeboy::core::Error::git_command_failed(format!(
                "git config {key} failed: {stderr}"
            )));
        }
    }

    let message = build_autofix_commit_message(sources);
    let author = format!("{name} <{email}>");
    let commit = Command::new("git")
        .args(["commit", "-m", &message, "--author", &author])
        .current_dir(path)
        .output()
        .map_err(|e| homeboy::core::Error::git_command_failed(format!("git commit: {e}")))?;
    if !commit.status.success() {
        let stderr = String::from_utf8_lossy(&commit.stderr);
        return Err(homeboy::core::Error::git_command_failed(format!(
            "git commit failed: {stderr}"
        )));
    }

    eprintln!(
        "[refactor] Committed autofix: {} files changed",
        sources.files_modified
    );
    Ok(())
}

/// Resolve git identity from the --git-identity flag.
/// "bot" -> default CI bot. "Name <email>" -> parsed. None -> default bot.
fn resolve_git_identity(identity: Option<&str>) -> (String, String) {
    match identity {
        None | Some("bot") => (BOT_NAME.to_string(), BOT_EMAIL.to_string()),
        Some(custom) => {
            if let Some(angle_start) = custom.find('<') {
                let name = custom[..angle_start].trim().to_string();
                let email = custom[angle_start + 1..]
                    .trim_end_matches('>')
                    .trim()
                    .to_string();
                if !name.is_empty() && !email.is_empty() {
                    return (name, email);
                }
            }
            (custom.to_string(), BOT_EMAIL.to_string())
        }
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_git_identity_bot_shorthand() {
        let (name, email) = resolve_git_identity(Some("bot"));
        assert_eq!(name, BOT_NAME);
        assert_eq!(email, BOT_EMAIL);
    }

    #[test]
    fn resolve_git_identity_none_defaults_to_bot() {
        let (name, email) = resolve_git_identity(None);
        assert_eq!(name, BOT_NAME);
        assert_eq!(email, BOT_EMAIL);
    }

    #[test]
    fn resolve_git_identity_custom_parsed() {
        let (name, email) = resolve_git_identity(Some("My Bot <my-bot@example.com>"));
        assert_eq!(name, "My Bot");
        assert_eq!(email, "my-bot@example.com");
    }

    #[test]
    fn resolve_git_identity_name_only_uses_bot_email() {
        let (name, email) = resolve_git_identity(Some("Just A Name"));
        assert_eq!(name, "Just A Name");
        assert_eq!(email, BOT_EMAIL);
    }
}
