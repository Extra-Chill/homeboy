//! Autofix safety guards.
//!
//! Checks whether autofix (refactor --write in CI) should proceed or bail.
//! Guards detect situations where autofix previously produced broken output:
//!
//! - A prior autofix commit was reverted on the current branch
//! - A prior autofix commit was force-pushed away (detected via GitHub API)
//! - The PR has an `autofix-disabled` label (persistent kill switch)
//!
//! These guards run early in the write path and short-circuit the pipeline
//! when autofix should not proceed. The action reads the structured output
//! to determine the skip reason.

use serde::Serialize;
use std::process::Command;

/// Prefix used for all autofix commits. Must match the action's prefix.
const AUTOFIX_COMMIT_PREFIX: &str = "chore(ci): homeboy autofix";

/// Label that permanently disables autofix on a PR.
const AUTOFIX_DISABLED_LABEL: &str = "autofix-disabled";

/// Bot identity for authored commits.
const BOT_NAME: &str = "homeboy-ci[bot]";

/// Why autofix was blocked.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "reason", rename_all = "kebab-case")]
pub enum GuardBlock {
    /// A prior autofix commit was reverted on this branch.
    Reverted,
    /// A prior autofix commit was force-pushed away from this branch.
    ForcePushed,
    /// The PR has the `autofix-disabled` label.
    DisabledLabel,
    /// HEAD commit was authored by the autofix bot.
    HeadIsBotCommit,
    /// Too many consecutive autofix commits at HEAD.
    CapReached { count: usize, max: usize },
}

impl GuardBlock {
    /// Machine-readable status string for the action to consume.
    pub fn status(&self) -> &'static str {
        match self {
            Self::Reverted => "skipped-reverted",
            Self::ForcePushed => "skipped-force-pushed",
            Self::DisabledLabel => "skipped-disabled-label",
            Self::HeadIsBotCommit => "skipped-head-bot-author",
            Self::CapReached { .. } => "skipped-cap-reached",
        }
    }

    /// Human-readable explanation.
    pub fn message(&self) -> String {
        match self {
            Self::Reverted => "a previous autofix commit was reverted on this branch".to_string(),
            Self::ForcePushed => {
                "a previous autofix commit was force-pushed away from this branch".to_string()
            }
            Self::DisabledLabel => format!(
                "PR has the '{}' label — autofix permanently disabled",
                AUTOFIX_DISABLED_LABEL
            ),
            Self::HeadIsBotCommit => "HEAD commit was authored by the autofix bot".to_string(),
            Self::CapReached { count, max } => format!(
                "autofix cap reached: {} consecutive bot commits (max {})",
                count, max
            ),
        }
    }
}

/// Result of running autofix guards.
#[derive(Debug)]
pub enum GuardResult {
    /// All guards passed — safe to proceed with writes.
    Proceed,
    /// One or more guards blocked — do not write.
    Blocked(GuardBlock),
}

/// Configuration for autofix guards, read from environment or config.
#[derive(Debug, Clone)]
pub struct GuardConfig {
    /// Maximum consecutive autofix commits before blocking (default: 2).
    pub max_commits: usize,
    /// GitHub repository (owner/repo). Read from GITHUB_REPOSITORY.
    pub github_repo: Option<String>,
    /// PR number. Read from GitHub event payload or env.
    pub pr_number: Option<u64>,
    /// GitHub token for API calls. Read from GH_TOKEN or GITHUB_TOKEN.
    pub github_token: Option<String>,
    /// Base ref for commit range scoping (e.g. origin/main).
    pub base_ref: Option<String>,
}

impl GuardConfig {
    /// Build config from environment variables (CI context).
    pub fn from_env() -> Self {
        let github_repo = std::env::var("GITHUB_REPOSITORY").ok();
        let github_token = std::env::var("GH_TOKEN")
            .or_else(|_| std::env::var("GITHUB_TOKEN"))
            .ok();
        let pr_number = Self::resolve_pr_number();
        let base_ref = Self::resolve_base_ref();

        Self {
            max_commits: std::env::var("AUTOFIX_MAX_COMMITS")
                .ok()
                .and_then(|v| v.parse().ok())
                .unwrap_or(2),
            github_repo,
            pr_number,
            github_token,
            base_ref,
        }
    }

    fn resolve_pr_number() -> Option<u64> {
        // Try direct env var first (set by action)
        if let Ok(n) = std::env::var("PR_NUMBER") {
            if let Ok(n) = n.parse() {
                return Some(n);
            }
        }
        // Fall back to GitHub event payload
        if let Ok(event_path) = std::env::var("GITHUB_EVENT_PATH") {
            if let Ok(content) = std::fs::read_to_string(&event_path) {
                if let Ok(event) = serde_json::from_str::<serde_json::Value>(&content) {
                    if let Some(n) = event
                        .get("pull_request")
                        .and_then(|pr| pr.get("number"))
                        .and_then(|n| n.as_u64())
                    {
                        return Some(n);
                    }
                }
            }
        }
        None
    }

    fn resolve_base_ref() -> Option<String> {
        // SCOPE_BASE_REF is set by the action's scope resolution
        if let Ok(base) = std::env::var("SCOPE_BASE_REF") {
            if !base.is_empty() {
                return Some(base);
            }
        }
        // Fall back to GITHUB_BASE_REF (set by GitHub for PRs)
        if let Ok(base) = std::env::var("GITHUB_BASE_REF") {
            if !base.is_empty() {
                return Some(format!("origin/{}", base));
            }
        }
        None
    }

    /// Whether we're running in a CI context with enough info to check guards.
    pub fn is_ci(&self) -> bool {
        self.github_repo.is_some()
    }
}

/// Run all autofix guards. Returns `Proceed` if safe to write, or `Blocked`
/// with the reason if autofix should be skipped.
///
/// Guards are checked in priority order — the first block wins.
pub fn check_guards(path: &str, config: &GuardConfig) -> GuardResult {
    // Outside CI, no guards apply — local dev always proceeds.
    if !config.is_ci() {
        return GuardResult::Proceed;
    }

    // 1. PR label — persistent kill switch (survives force pushes)
    if let Some(block) = check_disabled_label(config) {
        return GuardResult::Blocked(block);
    }

    // 2. HEAD is bot commit — don't autofix on top of autofix
    if let Some(block) = check_head_is_bot(path) {
        return GuardResult::Blocked(block);
    }

    // 3. Cap — too many consecutive autofix commits
    if let Some(block) = check_cap(path, config) {
        return GuardResult::Blocked(block);
    }

    // 4. Revert — a prior autofix was reverted in git history
    if let Some(block) = check_reverted(path, config) {
        // Also apply the label so future runs skip faster
        apply_disabled_label(config);
        return GuardResult::Blocked(block);
    }

    // 5. Force push — bot commits in GitHub PR data but missing from branch
    if let Some(block) = check_force_pushed(path, config) {
        apply_disabled_label(config);
        return GuardResult::Blocked(block);
    }

    GuardResult::Proceed
}

// ── Individual guards ────────────────────────────────────────────────

fn check_disabled_label(config: &GuardConfig) -> Option<GuardBlock> {
    let repo = config.github_repo.as_deref()?;
    let pr_number = config.pr_number?;
    let token = config.github_token.as_deref()?;

    let url = format!(
        "https://api.github.com/repos/{}/issues/{}/labels",
        repo, pr_number
    );

    let client = reqwest::blocking::Client::builder()
        .user_agent("homeboy")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .send()
        .ok()?;

    let labels: Vec<serde_json::Value> = response.json().ok()?;
    let has_label = labels.iter().any(|label| {
        label
            .get("name")
            .and_then(|n| n.as_str())
            .is_some_and(|name| name == AUTOFIX_DISABLED_LABEL)
    });

    if has_label {
        Some(GuardBlock::DisabledLabel)
    } else {
        None
    }
}

fn check_head_is_bot(path: &str) -> Option<GuardBlock> {
    let output = Command::new("git")
        .args(["log", "-1", "--format=%an"])
        .current_dir(path)
        .output()
        .ok()?;

    let author = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if author == BOT_NAME {
        Some(GuardBlock::HeadIsBotCommit)
    } else {
        None
    }
}

fn check_cap(path: &str, config: &GuardConfig) -> Option<GuardBlock> {
    let output = Command::new("git")
        .args([
            "log",
            "--format=%s",
            &format!("-n{}", config.max_commits + 1),
        ])
        .current_dir(path)
        .output()
        .ok()?;

    let subjects = String::from_utf8_lossy(&output.stdout);
    let mut count = 0;
    for line in subjects.lines() {
        if line.starts_with(AUTOFIX_COMMIT_PREFIX) {
            count += 1;
        } else {
            break;
        }
    }

    if count >= config.max_commits {
        Some(GuardBlock::CapReached {
            count,
            max: config.max_commits,
        })
    } else {
        None
    }
}

fn check_reverted(path: &str, config: &GuardConfig) -> Option<GuardBlock> {
    let revert_pattern = format!("Revert \"{}", AUTOFIX_COMMIT_PREFIX);

    let output = if let Some(base) = config.base_ref.as_deref() {
        Command::new("git")
            .args(["log", "--format=%s", &format!("{}..HEAD", base)])
            .current_dir(path)
            .output()
            .ok()?
    } else {
        Command::new("git")
            .args(["log", "--format=%s", "-n20"])
            .current_dir(path)
            .output()
            .ok()?
    };

    let subjects = String::from_utf8_lossy(&output.stdout);
    if subjects
        .lines()
        .any(|line| line.starts_with(&revert_pattern))
    {
        Some(GuardBlock::Reverted)
    } else {
        None
    }
}

fn check_force_pushed(path: &str, config: &GuardConfig) -> Option<GuardBlock> {
    let repo = config.github_repo.as_deref()?;
    let pr_number = config.pr_number?;
    let token = config.github_token.as_deref()?;

    let url = format!(
        "https://api.github.com/repos/{}/pulls/{}/commits?per_page=100",
        repo, pr_number
    );

    let client = reqwest::blocking::Client::builder()
        .user_agent("homeboy")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .ok()?;

    let response = client
        .get(&url)
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .send()
        .ok()?;

    let commits: Vec<serde_json::Value> = response.json().ok()?;

    // Find bot commit SHAs that GitHub recorded on this PR
    let bot_shas: Vec<&str> = commits
        .iter()
        .filter(|c| {
            c.get("commit")
                .and_then(|commit| commit.get("author"))
                .and_then(|author| author.get("name"))
                .and_then(|n| n.as_str())
                .is_some_and(|name| name == BOT_NAME)
        })
        .filter_map(|c| c.get("sha").and_then(|s| s.as_str()))
        .collect();

    if bot_shas.is_empty() {
        return None;
    }

    // Check if any bot commits are missing from current branch history
    for sha in &bot_shas {
        let check = Command::new("git")
            .args(["cat-file", "-t", sha])
            .current_dir(path)
            .output()
            .ok()?;

        if !check.status.success() {
            eprintln!(
                "[guard] bot commit {} is no longer in branch history (force-pushed away)",
                &sha[..10.min(sha.len())]
            );
            return Some(GuardBlock::ForcePushed);
        }
    }

    None
}

/// Apply the `autofix-disabled` label to the PR (best-effort).
/// Creates the label repo-wide if it doesn't exist yet.
fn apply_disabled_label(config: &GuardConfig) {
    let Some(repo) = config.github_repo.as_deref() else {
        return;
    };
    let Some(pr_number) = config.pr_number else {
        return;
    };
    let Some(token) = config.github_token.as_deref() else {
        return;
    };

    let client = match reqwest::blocking::Client::builder()
        .user_agent("homeboy")
        .timeout(std::time::Duration::from_secs(10))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    // Create label if it doesn't exist (409 = already exists, fine)
    let _ = client
        .post(format!("https://api.github.com/repos/{}/labels", repo))
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .json(&serde_json::json!({
            "name": AUTOFIX_DISABLED_LABEL,
            "color": "e4e669",
            "description": "Autofix permanently disabled (bot commit was reverted or force-pushed away)"
        }))
        .send();

    // Add label to the PR
    let result = client
        .post(format!(
            "https://api.github.com/repos/{}/issues/{}/labels",
            repo, pr_number
        ))
        .header("Authorization", format!("Bearer {}", token))
        .header("Accept", "application/vnd.github+json")
        .json(&serde_json::json!({
            "labels": [AUTOFIX_DISABLED_LABEL]
        }))
        .send();

    match result {
        Ok(_) => eprintln!(
            "[guard] added '{}' label to PR #{} — autofix permanently disabled",
            AUTOFIX_DISABLED_LABEL, pr_number
        ),
        Err(e) => eprintln!("[guard] failed to add disabled label: {}", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guard_block_status_strings() {
        assert_eq!(GuardBlock::Reverted.status(), "skipped-reverted");
        assert_eq!(GuardBlock::ForcePushed.status(), "skipped-force-pushed");
        assert_eq!(GuardBlock::DisabledLabel.status(), "skipped-disabled-label");
        assert_eq!(
            GuardBlock::HeadIsBotCommit.status(),
            "skipped-head-bot-author"
        );
        assert_eq!(
            GuardBlock::CapReached { count: 3, max: 2 }.status(),
            "skipped-cap-reached"
        );
    }

    #[test]
    fn guard_config_defaults() {
        // Outside CI, no env vars set
        let config = GuardConfig {
            max_commits: 2,
            github_repo: None,
            pr_number: None,
            github_token: None,
            base_ref: None,
        };
        assert!(!config.is_ci());
    }

    #[test]
    fn check_guards_outside_ci_always_proceeds() {
        let config = GuardConfig {
            max_commits: 2,
            github_repo: None,
            pr_number: None,
            github_token: None,
            base_ref: None,
        };
        let result = check_guards(".", &config);
        assert!(matches!(result, GuardResult::Proceed));
    }

    #[test]
    fn check_head_is_bot_with_human_commit() {
        // In a real repo with human commits, this should return None
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test Human"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@example.com"])
            .current_dir(path)
            .output()
            .unwrap();
        std::fs::write(tmp.path().join("file.txt"), "hello").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "human commit"])
            .current_dir(path)
            .output()
            .unwrap();

        assert!(check_head_is_bot(path).is_none());
    }

    #[test]
    fn check_head_is_bot_with_bot_commit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", BOT_NAME])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "bot@test.com"])
            .current_dir(path)
            .output()
            .unwrap();
        std::fs::write(tmp.path().join("file.txt"), "bot change").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", &format!("{} - test", AUTOFIX_COMMIT_PREFIX)])
            .current_dir(path)
            .output()
            .unwrap();

        assert!(matches!(
            check_head_is_bot(path),
            Some(GuardBlock::HeadIsBotCommit)
        ));
    }

    #[test]
    fn check_reverted_detects_revert() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();
        std::fs::write(tmp.path().join("file.txt"), "v1").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(path)
            .output()
            .unwrap();

        // Simulate a revert commit message
        std::fs::write(tmp.path().join("file.txt"), "v2").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "commit",
                "-m",
                &format!(
                    "Revert \"{} - refactor (1 files, 3 fixes)\"",
                    AUTOFIX_COMMIT_PREFIX
                ),
            ])
            .current_dir(path)
            .output()
            .unwrap();

        let config = GuardConfig {
            max_commits: 2,
            github_repo: None,
            pr_number: None,
            github_token: None,
            base_ref: None,
        };

        assert!(matches!(
            check_reverted(path, &config),
            Some(GuardBlock::Reverted)
        ));
    }

    #[test]
    fn check_cap_blocks_at_max() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();

        // Create 2 consecutive autofix commits
        for i in 0..2 {
            std::fs::write(tmp.path().join("file.txt"), format!("v{}", i)).unwrap();
            Command::new("git")
                .args(["add", "-A"])
                .current_dir(path)
                .output()
                .unwrap();
            Command::new("git")
                .args([
                    "commit",
                    "-m",
                    &format!("{} - batch {}", AUTOFIX_COMMIT_PREFIX, i),
                ])
                .current_dir(path)
                .output()
                .unwrap();
        }

        let config = GuardConfig {
            max_commits: 2,
            github_repo: None,
            pr_number: None,
            github_token: None,
            base_ref: None,
        };

        assert!(matches!(
            check_cap(path, &config),
            Some(GuardBlock::CapReached { count: 2, max: 2 })
        ));
    }

    #[test]
    fn check_cap_allows_under_max() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().to_str().unwrap();

        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(path)
            .output()
            .unwrap();

        // One autofix commit — under cap of 2
        std::fs::write(tmp.path().join("file.txt"), "v1").unwrap();
        Command::new("git")
            .args(["add", "-A"])
            .current_dir(path)
            .output()
            .unwrap();
        Command::new("git")
            .args([
                "commit",
                "-m",
                &format!("{} - batch 0", AUTOFIX_COMMIT_PREFIX),
            ])
            .current_dir(path)
            .output()
            .unwrap();

        let config = GuardConfig {
            max_commits: 2,
            github_repo: None,
            pr_number: None,
            github_token: None,
            base_ref: None,
        };

        assert!(check_cap(path, &config).is_none());
    }
}
