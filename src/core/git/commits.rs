use regex::Regex;
use serde::Serialize;

use crate::engine::command;
use crate::error::Result;

// Docs file patterns for categorizing commits
const DOCS_FILE_EXTENSIONS: [&str; 1] = [".md"];
const DOCS_DIRECTORIES: [&str; 1] = ["docs/"];

// Git log field/record separators — ASCII control characters that won't appear in commit text.
// Used to reliably split multi-line commit bodies from hash and subject.
const FIELD_SEP: char = '\x1e'; // ASCII Record Separator — separates hash|subject|body
const RECORD_SEP: char = '\x1f'; // ASCII Unit Separator — separates commits

/// Context for a component that lives inside a monorepo.
///
/// When a component's `local_path` is a subdirectory of the git root,
/// release operations need to scope commits and tags to that subdirectory.
#[derive(Debug, Clone)]
pub struct MonorepoContext {
    /// The git repository root path.
    pub git_root: String,
    /// Relative path from git root to the component (e.g. "wordpress").
    pub path_prefix: String,
    /// Tag prefix for this component (e.g. "wordpress"), used to create
    /// tags like `wordpress-v1.0.0`.
    pub tag_prefix: String,
}

impl MonorepoContext {
    /// Detect whether a component lives in a monorepo.
    ///
    /// Returns Some(context) if the component's path is a subdirectory of
    /// the git root, None if it IS the root (single-repo component).
    pub fn detect(local_path: &str, component_id: &str) -> Option<Self> {
        let path_prefix = super::get_component_path_prefix(local_path)?;
        let git_root = super::get_git_root(local_path).ok()?;

        Some(MonorepoContext {
            git_root,
            path_prefix: path_prefix.clone(),
            // Use component_id for tag prefix — it's the canonical name
            tag_prefix: component_id.to_string(),
        })
    }

    /// Format a version as a component-scoped tag name.
    /// e.g. "1.2.3" -> "wordpress-v1.2.3"
    pub fn format_tag(&self, version: &str) -> String {
        format!("{}-v{}", self.tag_prefix, version)
    }
}

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
    Release,
    Other,
}

/// Semantic version bump levels in ascending order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SemverBump {
    Patch,
    Minor,
    Major,
}

impl SemverBump {
    pub fn as_str(&self) -> &'static str {
        match self {
            SemverBump::Patch => "patch",
            SemverBump::Minor => "minor",
            SemverBump::Major => "major",
        }
    }

    pub fn rank(&self) -> u8 {
        match self {
            SemverBump::Patch => 1,
            SemverBump::Minor => 2,
            SemverBump::Major => 3,
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "patch" => Some(SemverBump::Patch),
            "minor" => Some(SemverBump::Minor),
            "major" => Some(SemverBump::Major),
            _ => None,
        }
    }
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
            CommitCategory::Release => None,
            CommitCategory::Other => None,
        }
    }

    /// Map commit category to changelog entry type.
    /// Returns None for categories that should be skipped (docs, chore, merge, release).
    pub fn to_changelog_entry_type(&self) -> Option<&'static str> {
        match self {
            CommitCategory::Feature => Some("added"),
            CommitCategory::Fix => Some("fixed"),
            CommitCategory::Breaking => Some("changed"),
            CommitCategory::Docs => None,
            CommitCategory::Chore => None,
            CommitCategory::Merge => None,
            CommitCategory::Release => None,
            CommitCategory::Other => Some("changed"),
        }
    }
}

/// Parse a commit subject into a category based on conventional commit format.
/// Subject-only variant — delegates to `classify_commit` with no body.
/// Kept for backwards compatibility and test convenience.
#[cfg(test)]
pub(crate) fn parse_conventional_commit(subject: &str) -> CommitCategory {
    classify_commit(subject, None)
}

/// Full commit classification with optional body text.
pub(crate) fn classify_commit(subject: &str, body: Option<&str>) -> CommitCategory {
    let lower = subject.to_lowercase();

    // Detect merge commits first - they should be filtered out
    if lower.starts_with("merge pull request")
        || lower.starts_with("merge branch")
        || lower.starts_with("merge remote-tracking")
    {
        return CommitCategory::Merge;
    }

    // Detect version bump / release commits - these are release infrastructure noise.
    // Uses the same patterns as find_version_commit() and find_version_release_commit().
    if is_release_commit(&lower) {
        return CommitCategory::Release;
    }

    // Check subject for breaking change markers
    if lower.contains("breaking change") || subject.contains("!:") {
        return CommitCategory::Breaking;
    }

    // Check body for BREAKING CHANGE: or BREAKING-CHANGE: footer (conventional commits spec)
    if let Some(body_text) = body {
        let body_lower = body_text.to_lowercase();
        if body_lower.contains("breaking change:") || body_lower.contains("breaking-change:") {
            return CommitCategory::Breaking;
        }
    }

    if lower.starts_with("feat") {
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

/// Check if a lowercased commit subject looks like a version bump or release commit.
/// Matches patterns like: "v0.2.3", "bump version to 0.2.3", "release: v1.0.0",
/// "version 0.2.2", "release v0.4.0", "chore(release): v1.0.0".
fn is_release_commit(lower: &str) -> bool {
    // Bare version tag: "v0.2.3" or "0.2.3" (entire subject is just a version)
    if BARE_VERSION_RE.is_match(lower) {
        return true;
    }

    // "bump version to 0.2.3", "bump to v0.2.3", "version bump to 0.2.3"
    if lower.starts_with("bump")
        || lower.starts_with("version bump")
        || lower.starts_with("version ")
    {
        if VERSION_NUMBER_RE.is_match(lower) {
            return true;
        }
    }

    // "release: v0.2.3", "release v0.2.3", "chore(release): v0.2.3"
    if RELEASE_PREFIX_RE.is_match(lower) {
        return true;
    }

    false
}

// Lazy regex patterns for release commit detection.
// These mirror the patterns in find_version_commit() and find_version_release_commit()
// but are compiled once for use in the hot path of classify_commit().
use std::sync::LazyLock;

/// Matches a subject that is just a version number: "v0.2.3", "0.2.3"
static BARE_VERSION_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^v?\d+\.\d+(?:\.\d+)?$").expect("Invalid regex"));

/// Matches any string containing a semver-like version number
static VERSION_NUMBER_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\d+\.\d+(?:\.\d+)?").expect("Invalid regex"));

/// Matches release-prefixed subjects: "release: v0.2.3", "release v0.2.3",
/// "chore(release): v0.2.3"
static RELEASE_PREFIX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(?:chore\([^)]*\):\s*v?\d+\.\d+(?:\.\d+)?|release:?\s*v?\d+\.\d+(?:\.\d+)?)")
        .expect("Invalid regex")
});

/// Parse raw git log output that uses FIELD_SEP / RECORD_SEP delimiters
/// into a list of CommitInfo structs with body-aware category classification.
fn parse_commit_records(raw: &str) -> Vec<CommitInfo> {
    raw.split(RECORD_SEP)
        .filter_map(|record| {
            let record = record.trim();
            if record.is_empty() {
                return None;
            }
            let mut parts = record.splitn(3, FIELD_SEP);
            let hash = parts.next()?.trim().to_string();
            let subject = parts.next()?.trim().to_string();
            let body = parts.next().map(|b| b.trim()).filter(|b| !b.is_empty());
            if hash.is_empty() || subject.is_empty() {
                return None;
            }
            let category = classify_commit(&subject, body);
            Some(CommitInfo {
                hash,
                subject,
                category,
            })
        })
        .collect()
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
///
/// When `tag_prefix` is provided (e.g. "wordpress"), only matches tags starting
/// with that prefix (e.g. `wordpress-v*`). This enables independent component
/// versioning in monorepos where each component has its own tag namespace.
pub fn get_latest_tag(path: &str) -> Result<Option<String>> {
    get_latest_tag_with_prefix(path, None)
}

/// Get the latest git tag, optionally filtered by a component prefix.
///
/// With prefix "wordpress", matches tags like `wordpress-v1.0.0`.
/// Without prefix, matches any tag (backward compatible).
pub fn get_latest_tag_with_prefix(path: &str, tag_prefix: Option<&str>) -> Result<Option<String>> {
    match tag_prefix {
        Some(prefix) => {
            let match_pattern = format!("{}-v*", prefix);
            Ok(command::run_in_optional(
                path,
                "git",
                &[
                    "describe",
                    "--tags",
                    "--abbrev=0",
                    "--match",
                    &match_pattern,
                ],
            ))
        }
        None => Ok(command::run_in_optional(
            path,
            "git",
            &["describe", "--tags", "--abbrev=0"],
        )),
    }
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
        format!(
            r"(?i)^(?:chore\([^)]*\):\s*)?release:?\s*v?{}(?:\s|$)",
            escaped_version
        ),
        format!(r"(?i)^v?{}\s*$", escaped_version),
        format!(
            r"(?i)^bump\s+(?:version\s+)?(?:to\s+)?v?{}(?:\s|$)",
            escaped_version
        ),
        // Match "Version X.Y.Z" or "Version bump to X.Y.Z"
        format!(
            r"(?i)^version\s+(?:bump\s+(?:to\s+)?)?v?{}(?:\s|:|-|$)",
            escaped_version
        ),
    ];

    for line in stdout.lines() {
        if let Some((hash, subject)) = line.split_once('|') {
            for pattern in &patterns {
                if Regex::new(pattern)
                    .map(|re| re.is_match(subject))
                    .unwrap_or(false)
                {
                    return Ok(Some(hash.to_string()));
                }
            }
        }
    }
    Ok(None)
}

/// Get the last N commits from the repository.
pub fn get_last_n_commits(path: &str, n: usize) -> Result<Vec<CommitInfo>> {
    let stdout = command::run_in(
        path,
        "git",
        &[
            "log",
            &format!("-{}", n),
            &format!("--format=%h{}%s{}%b{}", FIELD_SEP, FIELD_SEP, RECORD_SEP),
        ],
        "git log",
    )?;

    Ok(parse_commit_records(&stdout))
}

/// Get commits since a given tag (or all commits if tag is None).
/// Returns commits in reverse chronological order (newest first).
pub fn get_commits_since_tag(path: &str, tag: Option<&str>) -> Result<Vec<CommitInfo>> {
    get_commits_since_tag_for_path(path, tag, None)
}

/// Get commits since a given tag, optionally filtered to only those touching files
/// under `path_prefix` (relative to the git root).
///
/// In a monorepo, `path_prefix` scopes commit collection to a specific component
/// directory (e.g. "wordpress/") so that only commits touching that component's
/// files are included.
pub fn get_commits_since_tag_for_path(
    path: &str,
    tag: Option<&str>,
    path_prefix: Option<&str>,
) -> Result<Vec<CommitInfo>> {
    let range = tag
        .map(|t| format!("{}..HEAD", t))
        .unwrap_or_else(|| "HEAD".to_string());

    let format_str = format!("--format=%h{}%s{}%b{}", FIELD_SEP, FIELD_SEP, RECORD_SEP);
    let mut args = vec!["log".to_string(), range, format_str];

    // Add path filter for monorepo scoping: `git log <range> -- <path_prefix>`
    if let Some(prefix) = path_prefix {
        args.push("--".to_string());
        args.push(prefix.to_string());
    }

    let args_refs: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
    let stdout = command::run_in(path, "git", &args_refs, "git log")?;

    Ok(parse_commit_records(&stdout))
}

/// Counts of commits categorized by type.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CommitCounts {
    pub total: u32,
    pub code: u32,
    pub docs_only: u32,
}

/// Get the list of files changed by a specific commit.
pub(crate) fn get_commit_files(path: &str, commit_hash: &str) -> Result<Vec<String>> {
    let stdout = command::run_in(
        path,
        "git",
        &[
            "diff-tree",
            "--no-commit-id",
            "--name-only",
            "-r",
            commit_hash,
        ],
        "git diff-tree",
    )?;

    Ok(stdout
        .lines()
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect())
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
pub(crate) fn is_docs_only_commit(path: &str, commit: &CommitInfo) -> bool {
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

/// Recommend minimum semver bump required by commits in a release range.
///
/// Rules:
/// - Breaking => major
/// - Feature => minor
/// - Fix/Other => patch
/// - Docs/Chore/Merge => ignored for bump floor
pub fn recommended_bump_from_commits(commits: &[CommitInfo]) -> Option<SemverBump> {
    let mut recommended: Option<SemverBump> = None;

    for commit in commits {
        let bump = match commit.category {
            CommitCategory::Breaking => SemverBump::Major,
            CommitCategory::Feature => SemverBump::Minor,
            CommitCategory::Fix | CommitCategory::Other => SemverBump::Patch,
            CommitCategory::Docs
            | CommitCategory::Chore
            | CommitCategory::Merge
            | CommitCategory::Release => continue,
        };

        recommended = match recommended {
            None => Some(bump),
            Some(existing) if bump.rank() > existing.rank() => Some(bump),
            Some(existing) => Some(existing),
        };
    }

    recommended
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
            .all(|c| c.is_alphanumeric() || c == '(' || c == ')' || c == '!' || c == '#')
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
    fn classify_commit_breaking_change_in_body() {
        // BREAKING CHANGE: in commit body should be detected even with a non-breaking subject
        assert_eq!(
            classify_commit(
                "feat: refactor handler API",
                Some("BREAKING CHANGE: Handler subclasses removed")
            ),
            CommitCategory::Breaking
        );

        // BREAKING-CHANGE: (hyphenated form per conventional commits spec)
        assert_eq!(
            classify_commit(
                "feat: update config format",
                Some("BREAKING-CHANGE: old config format no longer supported")
            ),
            CommitCategory::Breaking
        );

        // Case insensitive in body
        assert_eq!(
            classify_commit(
                "feat: migrate API",
                Some("breaking change: removed deprecated endpoints")
            ),
            CommitCategory::Breaking
        );

        // Body without breaking change — should use subject classification
        assert_eq!(
            classify_commit(
                "feat: add new feature",
                Some("This is a regular body with no breaking changes")
            ),
            CommitCategory::Feature
        );

        // No body — should work like parse_conventional_commit
        assert_eq!(
            classify_commit("feat: add feature", None),
            CommitCategory::Feature
        );
    }

    #[test]
    fn parse_commit_records_with_body() {
        // Simulate git log output with field/record separators
        let raw = format!(
            "abc123{}feat: add feature{}Some body text{}def456{}fix: bug fix{}{}ghi789{}refactor: big change{}BREAKING CHANGE: removed old API{}",
            FIELD_SEP, FIELD_SEP, RECORD_SEP,
            FIELD_SEP, FIELD_SEP, RECORD_SEP,
            FIELD_SEP, FIELD_SEP, RECORD_SEP,
        );

        let commits = parse_commit_records(&raw);
        assert_eq!(commits.len(), 3);

        assert_eq!(commits[0].hash, "abc123");
        assert_eq!(commits[0].subject, "feat: add feature");
        assert_eq!(commits[0].category, CommitCategory::Feature);

        assert_eq!(commits[1].hash, "def456");
        assert_eq!(commits[1].subject, "fix: bug fix");
        assert_eq!(commits[1].category, CommitCategory::Fix);

        // This one has BREAKING CHANGE in the body
        assert_eq!(commits[2].hash, "ghi789");
        assert_eq!(commits[2].subject, "refactor: big change");
        assert_eq!(commits[2].category, CommitCategory::Breaking);
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
    fn strip_conventional_prefix_handles_issue_number_scope() {
        assert_eq!(
            strip_conventional_prefix("feat(#741): delete AgentType class"),
            "delete AgentType class"
        );
        assert_eq!(
            strip_conventional_prefix("fix(#730): queue-add uses unified check-duplicate"),
            "queue-add uses unified check-duplicate"
        );
        assert_eq!(
            strip_conventional_prefix("feat(#123): add new feature"),
            "add new feature"
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
        assert!(!is_docs_file("lib/extension.js"));
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
    fn classify_release_bare_version() {
        // Bare version tags: "v0.2.3", "0.2.3"
        assert_eq!(parse_conventional_commit("v0.2.3"), CommitCategory::Release);
        assert_eq!(parse_conventional_commit("0.2.3"), CommitCategory::Release);
        assert_eq!(parse_conventional_commit("v1.0.0"), CommitCategory::Release);
        assert_eq!(parse_conventional_commit("1.0"), CommitCategory::Release);
    }

    #[test]
    fn classify_release_bump_patterns() {
        // "Bump version to X.Y.Z" and variants
        assert_eq!(
            parse_conventional_commit("Bump version to 0.2.2"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("bump to v0.3.0"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("Bump version to 0.2.2 and add error logging"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("version bump to 0.2.1"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("Version 0.4.0"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("Bump version to 0.2.1"),
            CommitCategory::Release
        );
    }

    #[test]
    fn classify_release_prefix_patterns() {
        // "release: vX.Y.Z" and variants
        assert_eq!(
            parse_conventional_commit("release: v0.2.3"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("release v0.4.0"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("chore(release): v1.0.0"),
            CommitCategory::Release
        );
        assert_eq!(
            parse_conventional_commit("Release 0.5.0"),
            CommitCategory::Release
        );
    }

    #[test]
    fn classify_release_does_not_match_feature_commits() {
        // These should NOT be classified as Release
        assert_eq!(
            parse_conventional_commit("feat: add version display"),
            CommitCategory::Feature
        );
        assert_eq!(
            parse_conventional_commit("fix: version parsing bug"),
            CommitCategory::Fix
        );
        assert_eq!(
            parse_conventional_commit("Update plugin URI"),
            CommitCategory::Other
        );
        assert_eq!(
            parse_conventional_commit("added claude.md"),
            CommitCategory::Other
        );
        assert_eq!(
            parse_conventional_commit("Initial plan"),
            CommitCategory::Other
        );
        // chore: without a version number should still be Chore
        assert_eq!(
            parse_conventional_commit("chore: cleanup"),
            CommitCategory::Chore
        );
        // chore(deps) with a version-like number should still be Chore
        // (only chore(release)-style with a bare version after colon is Release)
        assert_eq!(
            parse_conventional_commit("chore(deps): bump lodash to 4.17.21"),
            CommitCategory::Chore
        );
    }

    #[test]
    fn release_category_skipped_in_changelog() {
        assert!(CommitCategory::Release.to_changelog_entry_type().is_none());
    }

    #[test]
    fn merge_and_release_categories_skipped_in_changelog() {
        assert!(CommitCategory::Merge.to_changelog_entry_type().is_none());
        assert!(CommitCategory::Docs.to_changelog_entry_type().is_none());
        assert!(CommitCategory::Chore.to_changelog_entry_type().is_none());
        assert!(CommitCategory::Release.to_changelog_entry_type().is_none());
        assert!(CommitCategory::Feature.to_changelog_entry_type().is_some());
    }

    #[test]
    fn recommended_bump_prefers_highest_severity() {
        let commits = vec![
            CommitInfo {
                hash: "a1".to_string(),
                subject: "fix: patch fix".to_string(),
                category: CommitCategory::Fix,
            },
            CommitInfo {
                hash: "b2".to_string(),
                subject: "feat: add feature".to_string(),
                category: CommitCategory::Feature,
            },
            CommitInfo {
                hash: "c3".to_string(),
                subject: "refactor!: break API".to_string(),
                category: CommitCategory::Breaking,
            },
        ];

        assert_eq!(
            recommended_bump_from_commits(&commits),
            Some(SemverBump::Major)
        );
    }

    #[test]
    fn recommended_bump_ignores_docs_and_chore() {
        let commits = vec![
            CommitInfo {
                hash: "a1".to_string(),
                subject: "docs: update".to_string(),
                category: CommitCategory::Docs,
            },
            CommitInfo {
                hash: "b2".to_string(),
                subject: "chore: cleanup".to_string(),
                category: CommitCategory::Chore,
            },
        ];

        assert_eq!(recommended_bump_from_commits(&commits), None);
    }

    #[test]
    fn recommended_bump_from_fix_and_other_is_patch() {
        let commits = vec![
            CommitInfo {
                hash: "a1".to_string(),
                subject: "random commit".to_string(),
                category: CommitCategory::Other,
            },
            CommitInfo {
                hash: "b2".to_string(),
                subject: "fix: bug".to_string(),
                category: CommitCategory::Fix,
            },
        ];

        assert_eq!(
            recommended_bump_from_commits(&commits),
            Some(SemverBump::Patch)
        );
    }

    #[test]
    fn monorepo_context_format_tag() {
        let ctx = MonorepoContext {
            git_root: "/repo".to_string(),
            path_prefix: "wordpress".to_string(),
            tag_prefix: "wordpress".to_string(),
        };
        assert_eq!(ctx.format_tag("1.2.3"), "wordpress-v1.2.3");
        assert_eq!(ctx.format_tag("0.1.0"), "wordpress-v0.1.0");
    }

    #[test]
    fn extract_version_from_component_prefixed_tag() {
        assert_eq!(
            extract_version_from_tag("wordpress-v1.2.3"),
            Some("1.2.3".to_string())
        );
        assert_eq!(
            extract_version_from_tag("github-v0.5.0"),
            Some("0.5.0".to_string())
        );
        // Standard tags still work
        assert_eq!(
            extract_version_from_tag("v1.0.0"),
            Some("1.0.0".to_string())
        );
    }
}
