use serde::Serialize;
use std::path::Path;
use std::process::Command;

use crate::core::error::{Error, Result};

use super::resolve_target;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrRefreshStrategy {
    Auto,
    Rebase,
    Merge,
    FfOnly,
}

impl PrRefreshStrategy {
    pub fn as_str(self) -> &'static str {
        match self {
            PrRefreshStrategy::Auto => "auto",
            PrRefreshStrategy::Rebase => "rebase",
            PrRefreshStrategy::Merge => "merge",
            PrRefreshStrategy::FfOnly => "ff-only",
        }
    }
}

#[derive(Debug, Clone)]
pub struct PrRefreshOptions {
    pub pr: String,
    pub strategy: PrRefreshStrategy,
    pub push: bool,
    pub checks: Vec<String>,
    pub path: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrRefreshOutput {
    pub component_id: String,
    pub path: String,
    pub action: String,
    pub success: bool,
    pub number: u64,
    pub url: String,
    pub base: String,
    pub head: String,
    pub strategy: String,
    pub pushed: bool,
    pub clean: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub merge_state: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub conflict_files: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub checks: Vec<PrRefreshCheck>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub blockers: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PrRefreshCheck {
    pub command: String,
    pub success: bool,
    pub exit_code: i32,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty")]
    pub stderr: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct GhPrView {
    number: u64,
    url: String,
    state: String,
    base_ref_name: String,
    head_ref_name: String,
    merge_state_status: Option<String>,
}

pub fn pr_refresh(
    component_id: Option<&str>,
    options: PrRefreshOptions,
) -> Result<PrRefreshOutput> {
    let (id, path) = resolve_target(component_id, options.path.as_deref())?;
    let root = Path::new(&path);

    if !status_porcelain(root)?.is_empty() {
        return Err(Error::validation_invalid_argument(
            "worktree",
            "refusing to refresh a dirty worktree before making any changes",
            None,
            Some(vec![
                "Commit, stash, or discard local changes before running pr refresh".to_string(),
                "This helper never runs destructive cleanup operations for you".to_string(),
            ]),
        ));
    }

    let pr = view_pr(root, &options.pr)?;
    if pr.state != "OPEN" {
        return Err(Error::validation_invalid_argument(
            "pr",
            format!("PR #{} is not open", pr.number),
            Some(pr.state),
            None,
        ));
    }

    checkout_pr_branch(root, pr.number)?;
    let branch = git_stdout(root, &["rev-parse", "--abbrev-ref", "HEAD"], "git branch")?;
    if branch == pr.base_ref_name {
        return Err(Error::validation_invalid_argument(
            "branch",
            "refusing to refresh the PR base branch",
            Some(branch),
            Some(vec![
                "Run this helper from the PR head branch or let gh check it out".to_string(),
                "Primary/default branch checkouts are not valid refresh targets".to_string(),
            ]),
        ));
    }

    git_checked(
        root,
        &["fetch", "origin", &pr.base_ref_name],
        "git fetch base",
    )?;

    let strategy = resolve_strategy(root, options.strategy, &branch)?;
    let update = match strategy {
        PrRefreshStrategy::Rebase => {
            git_output(root, &["rebase", &format!("origin/{}", pr.base_ref_name)])
        }
        PrRefreshStrategy::Merge => git_output(
            root,
            &[
                "merge",
                "--no-edit",
                &format!("origin/{}", pr.base_ref_name),
            ],
        ),
        PrRefreshStrategy::FfOnly => git_output(
            root,
            &[
                "merge",
                "--ff-only",
                &format!("origin/{}", pr.base_ref_name),
            ],
        ),
        PrRefreshStrategy::Auto => unreachable!("auto strategy resolved before update"),
    }?;

    let mut blockers = Vec::new();
    if !update.status.success() {
        blockers.push(format!(
            "{} failed with exit code {}",
            strategy.as_str(),
            update.status.code().unwrap_or(1)
        ));
    }

    let conflict_files = conflict_files(root)?;
    if !conflict_files.is_empty() {
        blockers.push(format!(
            "{} conflicted file(s) require manual resolution",
            conflict_files.len()
        ));
    }

    let mut check_results = Vec::new();
    if blockers.is_empty() {
        for check in effective_checks(&options.checks) {
            let result = run_check(root, &check)?;
            if !result.success {
                blockers.push(format!("check failed: {}", result.command));
            }
            check_results.push(result);
        }
    }

    let clean = status_porcelain(root)?.is_empty();
    if !clean && blockers.is_empty() {
        blockers.push("worktree is not clean after refresh".to_string());
    }

    let mut pushed = false;
    if options.push {
        if blockers.is_empty() && clean {
            let refspec = format!("HEAD:{}", pr.head_ref_name);
            git_checked(
                root,
                &["push", "--force-with-lease", "origin", &refspec],
                "git push --force-with-lease",
            )?;
            pushed = true;
        } else {
            blockers.push("push requested but refresh is not clean".to_string());
        }
    }

    Ok(PrRefreshOutput {
        component_id: id,
        path,
        action: "pr.refresh".to_string(),
        success: blockers.is_empty(),
        number: pr.number,
        url: pr.url,
        base: pr.base_ref_name,
        head: pr.head_ref_name,
        strategy: strategy.as_str().to_string(),
        pushed,
        clean,
        merge_state: pr.merge_state_status,
        conflict_files,
        checks: check_results,
        blockers,
        warnings: if options.push {
            Vec::new()
        } else {
            vec!["branch was not pushed; pass --push to publish a clean refresh".to_string()]
        },
    })
}

fn effective_checks(checks: &[String]) -> Vec<String> {
    if checks.is_empty() {
        vec!["git diff --check".to_string()]
    } else {
        checks.to_vec()
    }
}

fn view_pr(root: &Path, pr: &str) -> Result<GhPrView> {
    let pr_ref = normalize_pr_ref(pr)?;
    let output = Command::new("gh")
        .args([
            "pr",
            "view",
            &pr_ref,
            "--json",
            "number,url,state,baseRefName,headRefName,mergeStateStatus",
        ])
        .current_dir(root)
        .output()
        .map_err(|e| Error::git_command_failed(format!("gh pr view failed: {e}")))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "gh pr view failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| {
        Error::validation_invalid_argument(
            "pr",
            format!("failed to parse gh pr view output: {e}"),
            None,
            None,
        )
    })
}

fn checkout_pr_branch(root: &Path, number: u64) -> Result<()> {
    git_checked(root, &["status", "--porcelain=v1"], "git status")?;
    let output = Command::new("gh")
        .args(["pr", "checkout", &number.to_string()])
        .current_dir(root)
        .output()
        .map_err(|e| Error::git_command_failed(format!("gh pr checkout failed: {e}")))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(Error::git_command_failed(format!(
            "gh pr checkout failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn resolve_strategy(
    root: &Path,
    requested: PrRefreshStrategy,
    branch: &str,
) -> Result<PrRefreshStrategy> {
    if requested != PrRefreshStrategy::Auto {
        return Ok(requested);
    }

    let branch_key = format!("branch.{branch}.rebase");
    for key in [branch_key.as_str(), "pull.rebase"] {
        if let Some(value) = git_stdout_optional(root, &["config", "--get", key])? {
            return Ok(match value.as_str() {
                "true" | "merges" | "interactive" => PrRefreshStrategy::Rebase,
                "false" => PrRefreshStrategy::Merge,
                _ => PrRefreshStrategy::Rebase,
            });
        }
    }

    Ok(PrRefreshStrategy::Rebase)
}

fn conflict_files(root: &Path) -> Result<Vec<String>> {
    let status = status_porcelain(root)?;
    Ok(conflict_files_from_porcelain(&status))
}

fn conflict_files_from_porcelain(status: &str) -> Vec<String> {
    let mut files = Vec::new();
    for line in status.lines() {
        let Some(code) = line.get(0..2) else { continue };
        let conflicted = matches!(code, "DD" | "AU" | "UD" | "UA" | "DU" | "AA" | "UU");
        if conflicted {
            if let Some(path) = line.get(3..) {
                files.push(path.to_string());
            }
        }
    }
    files
}

fn run_check(root: &Path, command: &str) -> Result<PrRefreshCheck> {
    let output = Command::new("sh")
        .arg("-c")
        .arg(command)
        .current_dir(root)
        .output()
        .map_err(|e| Error::git_command_failed(format!("check failed to start: {e}")))?;
    Ok(PrRefreshCheck {
        command: command.to_string(),
        success: output.status.success(),
        exit_code: output.status.code().unwrap_or(1),
        stdout: String::from_utf8_lossy(&output.stdout).to_string(),
        stderr: String::from_utf8_lossy(&output.stderr).to_string(),
    })
}

fn normalize_pr_ref(pr: &str) -> Result<String> {
    let trimmed = pr.trim();
    if trimmed.is_empty() {
        return Err(Error::validation_invalid_argument(
            "pr",
            "PR number or URL is required",
            None,
            None,
        ));
    }
    if trimmed.chars().all(|c| c.is_ascii_digit()) {
        return Ok(trimmed.to_string());
    }
    if let Some((_, number)) = trimmed.rsplit_once("/pull/") {
        let digits = number
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>();
        if !digits.is_empty() {
            return Ok(digits);
        }
    }
    Ok(trimmed.to_string())
}

fn status_porcelain(root: &Path) -> Result<String> {
    git_stdout(
        root,
        &["status", "--porcelain=v1"],
        "git status --porcelain",
    )
}

fn git_stdout_optional(root: &Path, args: &[&str]) -> Result<Option<String>> {
    let output = git_output(root, args)?;
    if output.status.success() {
        Ok(Some(
            String::from_utf8_lossy(&output.stdout).trim().to_string(),
        ))
    } else {
        Ok(None)
    }
}

fn git_stdout(root: &Path, args: &[&str], label: &str) -> Result<String> {
    let output = git_checked(root, args, label)?;
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn git_checked(root: &Path, args: &[&str], label: &str) -> Result<std::process::Output> {
    let output = git_output(root, args)?;
    if output.status.success() {
        Ok(output)
    } else {
        Err(Error::git_command_failed(format!(
            "{label} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

fn git_output(root: &Path, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(root)
        .output()
        .map_err(|e| Error::git_command_failed(format!("git failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_pr_numbers_and_urls() {
        assert_eq!(normalize_pr_ref("123").unwrap(), "123");
        assert_eq!(
            normalize_pr_ref("https://github.com/Extra-Chill/homeboy/pull/5806").unwrap(),
            "5806"
        );
    }

    #[test]
    fn detects_conflict_files_from_porcelain() {
        let files = conflict_files_from_porcelain("UU src/lib.rs\n M README.md\nAA docs/a.md\n");
        assert_eq!(files, vec!["src/lib.rs", "docs/a.md"]);
    }

    #[test]
    fn default_check_is_git_diff_check() {
        assert_eq!(effective_checks(&[]), vec!["git diff --check"]);
    }
}
