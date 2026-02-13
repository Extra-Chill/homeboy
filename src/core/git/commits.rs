use regex::Regex;
use serde::Serialize;

use crate::error::Result;
use crate::utils::command;

// Docs file patterns for categorizing commits
const DOCS_FILE_EXTENSIONS: [&str; 1] = [".md"];
const DOCS_DIRECTORIES: [&str; 1] = ["docs/"];

#[derive(Debug, Clone, Serialize)]

pub struct CommitInfo {
    pub hash: String,
    pub subject: String,
    pub category: CommitCategory,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub enum CommitCategory {
    Breaking,
    Feature,
    Fix,
    Docs,
    Chore,
    Merge,
    Other,
}

impl CommitCategory {
    pub fn prefix(&self) -> Option<&'static str> {
        match self {
            CommitCategory::Breaking => Some("BREAKING"),
            CommitCategory::Feature => Some("feat"),
            CommitCategory::Fix => Some("fix"),
            CommitCategory::Docs => Some("docs"),
            CommitCategory::Chore => Some("chore"),
            CommitCategory::Merge => None,
            CommitCategory::Other => None,
        }
    }

    /// Map commit category to changelog entry type.
    /// Returns None for categories that should be skipped (docs, chore, merge).
    pub fn to_changelog_entry_type(&self) -> Option<&'static str> {
        match self {
            CommitCategory::Feature => Some("added"),
            CommitCategory::Fix => Some("fixed"),
            CommitCategory::Breaking => Some("changed"),
            CommitCategory::Docs => None,
            CommitCategory::Chore => None,
            CommitCategory::Merge => None,
            CommitCategory::Other => Some("changed"),
        }
    }
}

/// Parse a commit subject into a category based on conventional commit format.
/// Falls back to Other if no pattern matches - this is fine, commits still get included.
pub fn parse_conventional_commit(subject: &str) -> CommitCategory {
    let lower = subject.to_lowercase();

    // Detect merge commits first - they should be filtered out
    if lower.starts_with("merge pull request")
        || lower.starts_with("merge branch")
        || lower.starts_with("merge remote-tracking")
    {
        return CommitCategory::Merge;
    }

    if lower.contains("breaking change") || subject.contains("!:") {
        CommitCategory::Breaking
    } else if lower.starts_with("feat") {
        CommitCategory::Feature
    } else if lower.starts_with("fix") {
        CommitCategory::Fix
    } else if lower.starts_with("docs") {
        CommitCategory::Docs
    } else if lower.starts_with("chore") {
        CommitCategory::Chore
    } else {
        CommitCategory::Other
    }
}

/// Extract version number from a git tag.
/// Handles formats: v1.0.0, 1.0.0, component-v1.0.0
pub(crate) fn extract_version_from_tag(tag: &str) -> Option<String> {
    let version_pattern = Regex::new(r"v?(\d+\.\d+(?:\.\d+)?)").ok()?;
    version_pattern
        .captures(tag)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// Get the latest git tag in the repository.
/// Returns None if no tags exist.
pub fn get_latest_tag(path: &str) -> Result<Option<String>> {
    Ok(command::run_in_optional(path, "git", &["describe", "--tags", "--abbrev=0"]))
}

/// Find the most recent commit containing a version number in its message.
/// Matches strict patterns: v1.0.0, bump to X, release X, version X
/// Returns the commit hash if found, None otherwise.
pub fn find_version_commit(path: &str) -> Result<Option<String>> {
    let stdout = command::run_in(path, "git", &["log", "-200", "--format=%h|%s"], "git log")?;

    let version_pattern = Regex::new(
        r"(?i)(?:^v|^version\s+(?:bump\s+(?:to\s+)?)?v?|^bump\s+(?:version\s+)?(?:to\s+)?v?|^(?:chore\([^)]*\):\s*)?release:?\s*v?)(\d+\.\d+(?:\.\d+)?)",
    )
    .expect("Invalid regex pattern");

    for line in stdout.lines() {
        if let Some((hash, subject)) = line.split_once('|') {
            if version_pattern.is_match(subject) {
                return Ok(Some(hash.to_string()));
            }
        }
    }

    Ok(None)
}

/// Find the commit that released a specific version.
/// Uses strict patterns to avoid false positives - only matches commits that
/// clearly mark a release (e.g., "release: v0.2.0"), not mentions of releases.
pub fn find_version_release_commit(path: &str, version: &str) -> Result<Option<String>> {
    let Some(stdout) = command::run_in_optional(path, "git", &["log", "-200", "--format=%h|%s"])
    else {
        return Ok(None);
    };

    let escaped_version = regex::escape(version);
    let patterns = [
        format!(r"(?i)^(?:chore\([^)]*\):\s*)?release:?\s*v?{}(?:\s|$)", escaped_version),
        format!(r"(?i)^v?{}\s*$", escaped_version),
        format!(r"(?i)^bump\s+(?:version\s+)?(?:to\s+)?v?{}(?:\s|$)", escaped_version),
        // Match "Version X.Y.Z" or "Version bump to X.Y.Z"
        format!(r"(?i)^version\s+(?:bump\s+(?:to\s+)?)?v?{}(?:\s|:|-|$)", escaped_version),
    ];

    for line in stdout.lines() {
        if let Some((hash, subject)) = line.split_once('|') {
            for pattern in &patterns {
                if Regex::new(pattern).map(|re| re.is_match(subject)).unwrap_or(false) {
                    return Ok(Some(hash.to_string()));
                }
            }
        }
    }
    Ok(None)
}

/// Get the last N commits from the repository.
pub fn get_last_n_commits(path: &str, n: usize) -> Result<Vec<CommitInfo>> {
    let stdout = command::run_in(path, "git", &["log", &format!("-{}", n), "--format=%h|%s"], "git log")?;

    let commits = stdout
        .lines()
        .filter_map(|line| {
            let (hash, subject) = line.split_once('|')?;
            Some(CommitInfo {
                hash: hash.to_string(),
                subject: subject.to_string(),
                category: parse_conventional_commit(subject),
            })
        })
        .collect();

    Ok(commits)
}

/// Get commits since a given tag (or all commits if tag is None).
/// Returns commits in reverse chronological order (newest first).
pub fn get_commits_since_tag(path: &str, tag: Option<&str>) -> Result<Vec<CommitInfo>> {
    let range = tag.map(|t| format!("{}..HEAD", t)).unwrap_or_else(|| "HEAD".to_string());
    let stdout = command::run_in(path, "git", &["log", &range, "--format=%h|%s"], "git log")?;

    let commits = stdout
        .lines()
        .filter_map(|line| {
            let (hash, subject) = line.split_once('|')?;
            Some(CommitInfo {
                hash: hash.to_string(),
                subject: subject.to_string(),
                category: parse_conventional_commit(subject),
            })
        })
        .collect();

    Ok(commits)
}

/// Counts of commits categorized by type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CommitCounts {
    pub total: u32,
    pub code: u32,
    pub docs_only: u32,
}

/// Get the list of files changed by a specific commit.
pub fn get_commit_files(path: &str, commit_hash: &str) -> Result<Vec<String>> {
    let stdout = command::run_in(
        path,
        "git",
        &["diff-tree", "--no-commit-id", "--name-only", "-r", commit_hash],
        "git diff-tree",
    )?;

    Ok(stdout.lines().filter(|l| !l.is_empty()).map(String::from).collect())
}

/// Check if a file path is considered a docs file.
/// Returns true for *.md files and files in docs/ directories.
fn is_docs_file(file_path: &str) -> bool {
    // Check file extension
    for ext in DOCS_FILE_EXTENSIONS {
        if file_path.ends_with(ext) {
            return true;
        }
    }

    // Check if in docs/ directory (at any depth)
    for dir in DOCS_DIRECTORIES {
        if file_path.starts_with(dir) || file_path.contains(&format!("/{}", dir)) {
            return true;
        }
    }

    false
}

/// Check if a commit only touches documentation files.
/// Uses belt-and-suspenders approach:
/// 1. Fast path: commits with `docs:` prefix (CommitCategory::Docs) are docs-only
/// 2. Fallback: check all changed files match docs patterns
pub fn is_docs_only_commit(path: &str, commit: &CommitInfo) -> bool {
    // Fast path: conventional commit prefix
    if commit.category == CommitCategory::Docs {
        return true;
    }

    // Fallback: check actual file changes
    let files = match get_commit_files(path, &commit.hash) {
        Ok(f) => f,
        Err(_) => return false,
    };

    // Empty file list shouldn't count as docs-only
    if files.is_empty() {
        return false;
    }

    // All files must be docs files
    files.iter().all(|f| is_docs_file(f))
}

/// Categorize commits into code vs docs-only.
pub fn categorize_commits(path: &str, commits: &[CommitInfo]) -> CommitCounts {
    let mut counts = CommitCounts {
        total: commits.len() as u32,
        code: 0,
        docs_only: 0,
    };

    for commit in commits {
        if is_docs_only_commit(path, commit) {
            counts.docs_only += 1;
        } else {
            counts.code += 1;
        }
    }

    counts
}

/// Convert commits to changelog entries.
/// Strips conventional commit prefixes for cleaner changelog.
pub fn commits_to_changelog_entries(commits: &[CommitInfo]) -> Vec<String> {
    commits
        .iter()
        .map(|c| {
            // Strip conventional commit prefix if present
            let subject = strip_conventional_prefix(&c.subject);
            subject.to_string()
        })
        .collect()
}

/// Strip conventional commit prefix from a subject line.
/// "feat: Add new feature" -> "Add new feature"
/// "fix(scope): Fix bug" -> "Fix bug"
pub fn strip_conventional_prefix(subject: &str) -> &str {
    // Pattern: type(scope)?: message or type!: message
    if let Some(pos) = subject.find(": ") {
        let prefix = &subject[..pos];
        // Check if it looks like a conventional commit prefix
        if prefix
            .chars()
            .all(|c| c.is_alphanumeric() || c == '(' || c == ')' || c == '!')
        {
            return &subject[pos + 2..];
        }
    }
    subject
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_conventional_commit_feat() {
        assert_eq!(
            parse_conventional_commit("feat: Add new feature"),
            CommitCategory::Feature
        );
        assert_eq!(
            parse_conventional_commit("feat(scope): Add scoped feature"),
            CommitCategory::Feature
        );
    }

    #[test]
    fn parse_conventional_commit_fix() {
        assert_eq!(
            parse_conventional_commit("fix: Fix a bug"),
            CommitCategory::Fix
        );
    }

    #[test]
    fn parse_conventional_commit_breaking() {
        assert_eq!(
            parse_conventional_commit("feat!: Breaking change"),
            CommitCategory::Breaking
        );
        assert_eq!(
            parse_conventional_commit("BREAKING CHANGE: Something big"),
            CommitCategory::Breaking
        );
    }

    #[test]
    fn parse_conventional_commit_other() {
        assert_eq!(
            parse_conventional_commit("Random commit message"),
            CommitCategory::Other
        );
    }

    #[test]
    fn strip_conventional_prefix_works() {
        assert_eq!(
            strip_conventional_prefix("feat: Add feature"),
            "Add feature"
        );
        assert_eq!(
            strip_conventional_prefix("fix(shell): Fix escaping"),
            "Fix escaping"
        );
        assert_eq!(
            strip_conventional_prefix("Regular commit"),
            "Regular commit"
        );
    }

    #[test]
    fn is_docs_file_recognizes_markdown() {
        assert!(is_docs_file("README.md"));
        assert!(is_docs_file("CLAUDE.md"));
        assert!(is_docs_file("changelog.md"));
        assert!(is_docs_file("path/to/file.md"));
    }

    #[test]
    fn is_docs_file_recognizes_docs_directory() {
        assert!(is_docs_file("docs/guide.md"));
        assert!(is_docs_file("docs/api/reference.md"));
        assert!(is_docs_file("docs/commands/init.md"));
        assert!(is_docs_file("src/docs/readme.txt"));
        assert!(is_docs_file("path/to/docs/file.txt"));
    }

    #[test]
    fn is_docs_file_rejects_code() {
        assert!(!is_docs_file("src/main.rs"));
        assert!(!is_docs_file("lib/module.js"));
        assert!(!is_docs_file("Cargo.toml"));
        assert!(!is_docs_file("package.json"));
        assert!(!is_docs_file("src/component.tsx"));
    }

    #[test]
    fn parse_conventional_commit_docs() {
        assert_eq!(
            parse_conventional_commit("docs: Update README"),
            CommitCategory::Docs
        );
        assert_eq!(
            parse_conventional_commit("docs(api): Add endpoint docs"),
            CommitCategory::Docs
        );
    }

    #[test]
    fn parse_conventional_commit_merge() {
        assert_eq!(
            parse_conventional_commit("Merge pull request #45 from feature-branch"),
            CommitCategory::Merge
        );
        assert_eq!(
            parse_conventional_commit("Merge branch 'main' into feature"),
            CommitCategory::Merge
        );
        assert_eq!(
            parse_conventional_commit("Merge remote-tracking branch 'origin/main'"),
            CommitCategory::Merge
        );
    }

    #[test]
    fn merge_category_skipped_in_changelog() {
        assert!(CommitCategory::Merge.to_changelog_entry_type().is_none());
        assert!(CommitCategory::Docs.to_changelog_entry_type().is_none());
        assert!(CommitCategory::Chore.to_changelog_entry_type().is_none());
        assert!(CommitCategory::Feature.to_changelog_entry_type().is_some());
    }
}
