//! One admission transaction for a managed Cook destination. Caller-owned
//! checkouts and authenticated promoted candidates must not enter this path.
use std::path::Path;

use crate::{git, Error, Result};

/// Preserve candidate history by routing it through PR rework, never reset it.
/// Local-only repositories have no PR service to query.
pub fn require_issue_destination_without_pr(path: &Path, branch: &str) -> Result<()> {
    let Some(origin) = git::output_optional(path, &["remote", "get-url", "origin"]) else {
        return Ok(());
    };
    if !origin.contains("github.com") {
        return Ok(());
    }
    if let Some(url) = open_pull_request(path, branch, None)? {
        return Err(pr_route(branch, Some(&url)));
    }
    Ok(())
}

/// Resolve the actual destination before consulting GitHub. Managed records
/// can live in a different source clone from the requested repository.
pub fn derived_open_pull_request(
    repository: Option<&Path>,
    handle: &str,
    branch: &str,
    issue_url: &str,
) -> Result<Option<String>> {
    let managed = match super::resolve_workspace_ref_if_present(handle)? {
        Some(super::WorkspaceRefRecord::Task(record))
            if record.state == super::TaskWorktreeState::Active =>
        {
            [record.worktree_path, record.source_checkout]
                .into_iter()
                .map(std::path::PathBuf::from)
                .find(|path| path.is_dir())
        }
        _ => None,
    };
    let Some(path) = managed
        .as_deref()
        .or(repository)
        .filter(|path| path.is_dir())
    else {
        return Ok(None);
    };
    if managed.is_none() && super::stale_destination::adoptable_branch_ref(path, branch).is_none() {
        return Ok(None);
    }
    open_pull_request(path, branch, Some(issue_url))
}

/// Canonical open-PR lookup for destination admission and issue-to-PR adoption.
/// An explicit issue URL supplies repository authority even in local mirrors.
pub fn open_pull_request(
    path: &Path,
    branch: &str,
    issue_url: Option<&str>,
) -> Result<Option<String>> {
    let mut command = std::process::Command::new("gh");
    command.args([
        "pr", "list", "--state", "open", "--head", branch, "--json", "url",
    ]);
    if let Some(repository) = issue_url
        .and_then(|url| url.strip_prefix("https://github.com/"))
        .and_then(|url| url.split_once("/issues/").map(|(repo, _)| repo))
    {
        command.args(["--repo", repository]);
    }
    let output = command
        .current_dir(path)
        .output()
        .map_err(|error| infrastructure(error.to_string()))?;
    if !output.status.success() {
        return Err(infrastructure(
            String::from_utf8_lossy(&output.stderr).into_owned(),
        ));
    }
    parse_open_pr(&output.stdout)
}

fn parse_open_pr(response: &[u8]) -> Result<Option<String>> {
    let prs: serde_json::Value =
        serde_json::from_slice(response).map_err(|error| infrastructure(error.to_string()))?;
    match prs.as_array().map(Vec::as_slice) {
        Some([]) => Ok(None),
        Some([pr]) => pr["url"]
            .as_str()
            .filter(|url| url.contains("/pull/"))
            .map(|url| Some(url.to_string()))
            .ok_or_else(|| infrastructure("invalid open PR URL".into())),
        _ => Err(infrastructure(
            "ambiguous or invalid PR list response".into(),
        )),
    }
}

/// A canonical PR-URL task may keep its own head, even when behind base.
/// Use the same source-HEAD/base snapshot semantics as ahead-only candidates;
/// never reset, force-push, or route a PR's rework back to itself.
pub fn is_pr_rework(path: &Path, branch: &str, task_url: Option<&str>) -> Result<bool> {
    let Some(task_url) = task_url.filter(|url| url.contains("/pull/")) else {
        return Ok(false);
    };
    Ok(open_pull_request(path, branch, None)?.as_deref() == Some(task_url))
}

pub fn pr_route(branch: &str, url: Option<&str>) -> Error {
    let command = url
        .map(|url| format!("homeboy agent-task cook --task-url {url}"))
        .unwrap_or_else(|| {
            format!(
                "gh pr create --head {}",
                homeboy_engine_primitives::shell::quote_arg(branch)
            )
        });
    let mut error = Error::validation_invalid_argument(
        "task_url",
        "Cook destination contains existing PR work; use the PR-URL rework flow",
        Some(branch.to_string()),
        Some(if url.is_some() {
            vec![command.clone()]
        } else {
            vec![
                command.clone(),
                "After creating a PR, rerun Cook with --task-url <the PR URL>.".to_string(),
            ]
        }),
    );
    error.details["destination_convergence"] = serde_json::json!({
        "next_action": "cook_pr_url", "command": command, "pr_url": url,
    });
    error
}

/// Converge a clean issue-owned linked worktree to an immutable base. Dirt is
/// accepted only after the controller has authenticated and adopted it.
pub fn converge_destination(
    path: &Path,
    base: &str,
    adopted: bool,
    issue_owned: bool,
    task_url: Option<&str>,
) -> Result<()> {
    super::verify_linked_worktree_root(path)?;
    let branch = git::run_git(
        path,
        &["symbolic-ref", "--short", "HEAD"],
        "identify Cook destination branch",
    )?;
    if issue_owned {
        require_issue_destination_without_pr(path, branch.trim())?;
    }
    let status = git::run_git(
        path,
        &["status", "--porcelain=v1", "-z", "--untracked-files=all"],
        "inspect Cook destination",
    )?;
    if !status.is_empty() && !adopted {
        let files: Vec<_> = status.split('\0').filter(|s| !s.is_empty()).collect();
        let mut error = Error::validation_invalid_argument(
            "workspace",
            format!(
                "Cook cannot attribute uncommitted destination changes: {}",
                files.join(", ")
            ),
            Some(path.display().to_string()),
            Some(vec![
                "Preserve these files with git stash push --include-untracked, then rerun Cook."
                    .into(),
            ]),
        );
        error.details["dirty_paths"] = serde_json::json!(files);
        return Err(error);
    }
    let ahead = git::run_git(
        path,
        &["rev-list", "--count", &format!("{base}..HEAD")],
        "inspect unique destination commits",
    )?;
    if issue_owned && ahead.trim() != "0" {
        return Err(pr_route(branch.trim(), None));
    }
    let behind = git::run_git(
        path,
        &["rev-list", "--count", &format!("HEAD..{base}")],
        "inspect destination base",
    )?;
    if ahead.trim() != "0" && behind.trim() != "0" && !is_pr_rework(path, branch.trim(), task_url)?
    {
        require_issue_destination_without_pr(path, branch.trim())?;
        return Err(pr_route(branch.trim(), None));
    }

    // An authenticated earlier attempt stays recoverable even when HEAD
    // already equals the pin. Providers always start from a clean checkout.
    if !status.is_empty() {
        git::run_git(
            path,
            &[
                "stash",
                "push",
                "--include-untracked",
                "-m",
                "homeboy: preserve earlier Cook attempt before base convergence",
            ],
            "preserve Cook candidate before convergence",
        )?;
    }
    if ahead.trim() == "0" && behind.trim() != "0" {
        git::run_git(
            path,
            &["reset", "--hard", base],
            "converge Cook destination",
        )?;
    }
    Ok(())
}

fn infrastructure(message: String) -> Error {
    let mut error = Error::git_command_failed(format!("{message}; retry with GitHub access or use --task-url https://github.com/<owner>/<repo>/pull/<n> before provider execution"));
    error.retryable = Some(true);
    error.details["failure_class"] = serde_json::json!("infrastructure");
    error
}

/// Retry only transport failures. Deterministic Git errors (missing refs,
/// permissions, local conflicts) must not spend the infrastructure budget.
pub fn retry_fetch<T>(mut fetch: impl FnMut() -> Result<T>) -> Result<T> {
    for attempt in 0..3 {
        match fetch() {
            Ok(value) => return Ok(value),
            Err(mut error) => {
                let text = format!("{error:?}").to_ascii_lowercase();
                if ![
                    "ssl",
                    "connection",
                    "timed out",
                    "could not resolve host",
                    "unable to access",
                ]
                .iter()
                .any(|needle| text.contains(needle))
                {
                    return Err(error);
                }
                if attempt == 2 {
                    error.retryable = Some(true);
                    error.details["failure_class"] = serde_json::json!("infrastructure");
                    return Err(error);
                }
                std::thread::sleep(std::time::Duration::from_millis(100 * (1 << attempt)));
            }
        }
    }
    unreachable!()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    struct Fixture {
        _root: tempfile::TempDir,
        source: PathBuf,
        destination: PathBuf,
        base: String,
    }

    fn git(path: &Path, args: &[&str]) -> String {
        crate::git::run_git(path, args, "convergence fixture")
            .unwrap()
            .trim()
            .to_string()
    }

    fn fixture() -> Fixture {
        let root = tempfile::tempdir().unwrap();
        let source = root.path().join("source");
        let destination = root.path().join("destination");
        std::fs::create_dir(&source).unwrap();
        git(&source, &["init", "-b", "main"]);
        git(&source, &["config", "user.name", "Test"]);
        git(&source, &["config", "user.email", "test@example.com"]);
        std::fs::write(source.join("file"), "base").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "base"]);
        git(
            &source,
            &[
                "worktree",
                "add",
                "-b",
                "fix/issue-1",
                destination.to_str().unwrap(),
            ],
        );
        std::fs::write(source.join("new-base"), "new base").unwrap();
        git(&source, &["add", "."]);
        git(&source, &["commit", "-m", "advance base"]);
        let base = git(&source, &["rev-parse", "HEAD"]);
        Fixture {
            _root: root,
            source,
            destination,
            base,
        }
    }

    #[test]
    fn worktree_behind_without_unique_commits_converges() {
        let f = fixture();
        converge_destination(&f.destination, &f.base, false, true, None).unwrap();
        assert_eq!(git(&f.destination, &["rev-parse", "HEAD"]), f.base);
        assert!(git(&f.destination, &["status", "--porcelain"]).is_empty());
    }

    #[test]
    fn worktree_diverged_unique_commits_routes_without_reset() {
        let f = fixture();
        std::fs::write(f.destination.join("candidate"), "work").unwrap();
        git(&f.destination, &["add", "."]);
        git(&f.destination, &["commit", "-m", "unique candidate"]);
        let head = git(&f.destination, &["rev-parse", "HEAD"]);
        let error = converge_destination(&f.destination, &f.base, false, true, None).unwrap_err();
        assert_eq!(
            error.details["destination_convergence"]["command"],
            "gh pr create --head fix/issue-1"
        );
        assert_eq!(git(&f.destination, &["rev-parse", "HEAD"]), head);
    }

    #[cfg(unix)]
    #[test]
    fn worktree_open_pr_lookup_prevents_even_clean_behind_reset() {
        crate::test_support::with_isolated_home(|_| {
            use std::os::unix::fs::PermissionsExt;
            let f = fixture();
            git(
                &f.source,
                &[
                    "remote",
                    "add",
                    "origin",
                    "https://github.com/Extra-Chill/homeboy.git",
                ],
            );
            let bin = f._root.path().join("bin");
            std::fs::create_dir(&bin).unwrap();
            let gh = bin.join("gh");
            std::fs::write(&gh, "#!/bin/sh\nprintf '[{\"url\":\"https://github.com/Extra-Chill/homeboy/pull/42\"}]'\n").unwrap();
            std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
            let _path = crate::test_support::EnvVarGuard::set(
                "PATH",
                format!(
                    "{}:{}",
                    bin.display(),
                    std::env::var("PATH").unwrap_or_default()
                ),
            );
            let head = git(&f.destination, &["rev-parse", "HEAD"]);
            let error =
                converge_destination(&f.destination, &f.base, false, true, None).unwrap_err();
            assert_eq!(
                error.details["destination_convergence"]["command"],
                "homeboy agent-task cook --task-url https://github.com/Extra-Chill/homeboy/pull/42"
            );
            assert_eq!(git(&f.destination, &["rev-parse", "HEAD"]), head);

            git(
                &f.destination,
                &["commit", "--allow-empty", "-m", "PR work"],
            );
            let candidate = git(&f.destination, &["rev-parse", "HEAD"]);
            converge_destination(
                &f.destination,
                &f.base,
                false,
                false,
                Some("https://github.com/Extra-Chill/homeboy/pull/42"),
            )
            .expect("admit diverged PR without resetting");
            assert_eq!(git(&f.destination, &["rev-parse", "HEAD"]), candidate);
            git(
                &f.source,
                &["update-ref", "refs/remotes/origin/fix/issue-1", &candidate],
            );
            git(
                &f.source,
                &["update-ref", "refs/remotes/origin/main", &f.base],
            );
            git(
                &f.source,
                &["worktree", "remove", f.destination.to_str().unwrap()],
            );
            git(&f.source, &["branch", "-D", "fix/issue-1"]);
            // Remote-only branches are the same authority creation adopts.
            let classification = super::super::stale_destination::classify_local_branch(
                &f.source,
                "repo@fix-issue-1",
                "fix/issue-1",
                "main",
            )
            .unwrap();
            assert!(matches!(
                classification,
                super::super::stale_destination::DestinationFreshness::Stale(_)
            ));
            let error = super::super::stale_destination::refuse_foreign_history(classification)
                .unwrap_err();
            assert_eq!(
                error.details["destination_convergence"]["pr_url"],
                "https://github.com/Extra-Chill/homeboy/pull/42"
            );
            for state in ["closed", "merged"] {
                std::fs::write(&gh, "#!/bin/sh\nprintf '[]'\n").unwrap();
                let classification = super::super::stale_destination::classify_local_branch(
                    &f.source,
                    "repo@fix-issue-1",
                    "fix/issue-1",
                    "main",
                )
                .unwrap();
                let error = super::super::stale_destination::refuse_foreign_history(classification)
                    .expect_err(state);
                assert_eq!(
                    error.details["destination_convergence"]["command"],
                    "gh pr create --head fix/issue-1"
                );
                assert_eq!(
                    git(&f.source, &["rev-parse", "refs/remotes/origin/fix/issue-1"]),
                    candidate
                );
            }
        });
    }

    #[test]
    fn worktree_foreign_dirt_is_refused_with_files() {
        let f = fixture();
        std::fs::write(f.destination.join("operator.txt"), "keep").unwrap();
        let error = converge_destination(&f.destination, &f.base, false, true, None).unwrap_err();
        assert!(error.to_string().contains("operator.txt"));
        assert_eq!(
            std::fs::read_to_string(f.destination.join("operator.txt")).unwrap(),
            "keep"
        );
    }

    #[test]
    fn worktree_authenticated_prior_attempt_is_stashed_before_reset() {
        let f = fixture();
        std::fs::write(f.destination.join("candidate.txt"), "keep candidate").unwrap();
        converge_destination(&f.destination, &f.base, true, true, None).unwrap();
        assert_eq!(git(&f.destination, &["rev-parse", "HEAD"]), f.base);
        assert!(git(&f.destination, &["status", "--porcelain"]).is_empty());
        git(&f.destination, &["stash", "apply"]);
        assert_eq!(
            std::fs::read_to_string(f.destination.join("candidate.txt")).unwrap(),
            "keep candidate"
        );
    }

    #[test]
    fn worktree_fetch_ssl_retries_and_exhaustion_is_infrastructure() {
        let f = fixture();
        let mut attempts = 0;
        retry_fetch(|| {
            attempts += 1;
            if attempts < 3 {
                return Err(Error::git_command_failed("SSL connection failed"));
            }
            Ok(git(&f.source, &["rev-parse", "HEAD"]))
        })
        .unwrap();
        assert_eq!(attempts, 3);
        let error = retry_fetch::<()>(|| Err(Error::git_command_failed("SSL connection failed")))
            .unwrap_err();
        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["failure_class"], "infrastructure");
        let mut attempts = 0;
        let _ = retry_fetch::<()>(|| {
            attempts += 1;
            Err(Error::git_command_failed("missing ref"))
        });
        assert_eq!(attempts, 1);
    }
}
