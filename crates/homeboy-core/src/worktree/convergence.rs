//! One admission transaction for a managed Cook destination. Caller-owned
//! checkouts and authenticated promoted candidates must not enter this path.
use std::path::Path;

use crate::{git, Error, Result};

/// A destination branch's PR history, from one `gh pr list --state all`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrHistory {
    /// No PR, or no GitHub origin to ask.
    None,
    /// Exactly one open PR heads the branch; it owns every rework.
    Open(String),
    /// Only merged or closed PRs: the branch's generation is retired.
    Finished,
}

/// The one PR lookup for destination admission. Without an explicit GitHub
/// `owner/repo`, local-only and non-GitHub origins have no PR service. Query
/// failures are admission errors, never evidence a branch is free or retired.
pub fn pr_history(path: &Path, branch: &str, repository: Option<&str>) -> Result<PrHistory> {
    if repository.is_none() {
        let Some(origin) = git::output_optional(path, &["remote", "get-url", "origin"]) else {
            return Ok(PrHistory::None);
        };
        if !origin.contains("github.com") {
            return Ok(PrHistory::None);
        }
    }
    let mut command = std::process::Command::new("gh");
    command.args([
        "pr",
        "list",
        "--state",
        "all",
        "--head",
        branch,
        "--json",
        "state,url",
    ]);
    if let Some(repository) = repository {
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
    parse_pr_history(&output.stdout)
}

/// What derivation needs to pick the next fresh generation of `base`
/// (`base` is generation 1, `base-N` is N).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchGenerations {
    /// Highest generation any local or origin branch, or any PR head in any
    /// state, already uses; 0 when none does. Every higher generation has no
    /// branch ref. Search results can lag, so a higher generation may still
    /// have PR history: check it with [`pr_history`].
    pub highest_used: u32,
    /// `owner/repo` of a github.com origin, the repository to ask for PR
    /// history; `None` when origin has no PR service.
    pub github_repository: Option<String>,
}

/// One ref listing and one PR search cover every generation of `base`, so a
/// long-lived issue whose earlier generations all merged costs neither a
/// ref probe nor a PR lookup per generation.
pub fn branch_generations(path: &Path, base: &str) -> Result<BranchGenerations> {
    let prefixes = ["refs/heads/", "refs/remotes/origin/"];
    let patterns: Vec<String> = prefixes
        .iter()
        .flat_map(|prefix| [format!("{prefix}{base}"), format!("{prefix}{base}-*")])
        .collect();
    let mut args = vec!["for-each-ref", "--format=%(refname)"];
    args.extend(patterns.iter().map(String::as_str));
    let mut heads: Vec<String> = git::output_optional(path, &args)
        .map(|output| {
            output
                .lines()
                .filter_map(|reference| {
                    prefixes
                        .iter()
                        .find_map(|prefix| reference.strip_prefix(prefix))
                })
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let github_repository = git::output_optional(path, &["remote", "get-url", "origin"])
        .and_then(|origin| git::release_download::parse_github_url(origin.trim()))
        .filter(|repo| repo.host == "github.com")
        .map(|repo| format!("{}/{}", repo.owner, repo.repo));
    if let Some(repository) = &github_repository {
        let output = std::process::Command::new("gh")
            .args(["pr", "list", "--state", "all", "--limit", "1000"])
            .args(["--repo", repository])
            .args(["--search", &format!("head:{base}"), "--json", "headRefName"])
            .current_dir(path)
            .output()
            .map_err(|error| infrastructure(error.to_string()))?;
        if !output.status.success() {
            return Err(infrastructure(
                String::from_utf8_lossy(&output.stderr).into_owned(),
            ));
        }
        #[derive(serde::Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct PullRequest {
            head_ref_name: String,
        }
        let prs: Vec<PullRequest> = serde_json::from_slice(&output.stdout)
            .map_err(|error| infrastructure(error.to_string()))?;
        heads.extend(prs.into_iter().map(|pr| pr.head_ref_name));
    }
    Ok(BranchGenerations {
        highest_used: heads
            .iter()
            .filter_map(|head| branch_generation(base, head))
            .max()
            .unwrap_or(0),
        github_repository,
    })
}

fn branch_generation(base: &str, branch: &str) -> Option<u32> {
    if branch == base {
        return Some(1);
    }
    let suffix = branch.strip_prefix(base)?.strip_prefix('-')?;
    suffix
        .bytes()
        .all(|byte| byte.is_ascii_digit())
        .then(|| suffix.parse().ok())
        .flatten()
}

/// Preserve candidate history by routing it through PR rework, never reset it.
pub fn require_issue_destination_without_pr(path: &Path, branch: &str) -> Result<()> {
    match pr_history(path, branch, None)? {
        PrHistory::Open(url) => Err(pr_route(branch, Some(&url))),
        _ => Ok(()),
    }
}

fn parse_pr_history(response: &[u8]) -> Result<PrHistory> {
    #[derive(serde::Deserialize)]
    #[serde(rename_all = "UPPERCASE")]
    enum State {
        Open,
        Closed,
        Merged,
    }
    #[derive(serde::Deserialize)]
    struct PullRequest {
        state: State,
        url: String,
    }
    let prs: Vec<PullRequest> =
        serde_json::from_slice(response).map_err(|error| infrastructure(error.to_string()))?;
    let open: Vec<&PullRequest> = prs
        .iter()
        .filter(|pr| matches!(pr.state, State::Open))
        .collect();
    match open.as_slice() {
        [] if prs.is_empty() => Ok(PrHistory::None),
        [] => Ok(PrHistory::Finished),
        [pr] if pr.url.contains("/pull/") => Ok(PrHistory::Open(pr.url.clone())),
        [_] => Err(infrastructure("invalid open PR URL".into())),
        _ => Err(infrastructure("ambiguous open PRs for one branch".into())),
    }
}

/// A canonical PR-URL task may keep its own head, even when behind base.
/// Use the same source-HEAD/base snapshot semantics as ahead-only candidates;
/// never reset, force-push, or route a PR's rework back to itself.
pub fn is_pr_rework(path: &Path, branch: &str, task_url: Option<&str>) -> Result<bool> {
    // The PR URL names its repository, so mirrors need no GitHub origin.
    let Some((task_url, repository)) = task_url.and_then(|url| {
        url.strip_prefix("https://github.com/")?
            .split_once("/pull/")
            .map(|(repository, _)| (url, repository))
    }) else {
        return Ok(false);
    };
    Ok(pr_history(path, branch, Some(repository))? == PrHistory::Open(task_url.to_string()))
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

/// The refusal for a destination that diverged from its base and is not the
/// rework of an open PR named by `task_url`. Naming a PR this branch does not
/// head (another branch's, or a closed one) is its own mistake; pointing that caller at the PR-URL flow it
/// already used would send it in a circle.
pub fn diverged_route(branch: &str, task_url: Option<&str>) -> Error {
    let Some(url) = task_url.filter(|url| url.contains("/pull/")) else {
        return pr_route(branch, None);
    };
    let mut error = Error::validation_invalid_argument(
        "task_url",
        format!(
            "Cook destination branch `{branch}` is not the head of an open PR at {url}; run Cook on that PR's head branch, or open a PR for `{branch}` and pass its URL"
        ),
        Some(branch.to_string()),
        Some(vec![format!(
            "gh pr create --head {}",
            homeboy_engine_primitives::shell::quote_arg(branch)
        )]),
    );
    error.details["destination_convergence"] = serde_json::json!({
        "next_action": "pr_head_mismatch", "branch": branch, "task_url": url,
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
        return Err(diverged_route(branch.trim(), task_url));
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

/// How long a Git network operation keeps retrying transport failures. A
/// resolver or Wi-Fi blip lasts seconds; a sub-second budget turned each one
/// into a failed Cook continuation and an operator fallback.
pub const TRANSPORT_RETRY_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// Retry only transport failures: errors the operation already typed as
/// retryable, or Git transport text. Deterministic Git errors (missing refs,
/// permissions, local conflicts) must not spend the infrastructure budget.
pub fn retry_fetch<T>(fetch: impl FnMut() -> Result<T>) -> Result<T> {
    retry_fetch_within(TRANSPORT_RETRY_WINDOW, fetch)
}

/// [`retry_fetch`] with an explicit window. Transport failures back off
/// exponentially (capped) until the next retry would start past `window`.
pub fn retry_fetch_within<T>(
    window: std::time::Duration,
    mut fetch: impl FnMut() -> Result<T>,
) -> Result<T> {
    const FIRST_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
    const MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(5);
    let started = std::time::Instant::now();
    let mut delay = FIRST_DELAY;
    let mut attempts = 0u32;
    loop {
        attempts += 1;
        let mut error = match fetch() {
            Ok(value) => return Ok(value),
            Err(error) => error,
        };
        let text = format!("{error:?}").to_ascii_lowercase();
        if error.retryable != Some(true)
            && ![
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
        if started.elapsed() + delay > window {
            error.retryable = Some(true);
            error.details["failure_class"] = serde_json::json!("infrastructure");
            error.details["transport_retry"] = serde_json::json!({
                "attempts": attempts,
                "window_ms": window.as_millis() as u64,
            });
            return Err(error);
        }
        std::thread::sleep(delay);
        delay = (delay * 2).min(MAX_DELAY);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn a_diverged_branch_names_the_pr_it_does_not_head() {
        let url = "https://github.com/o/r/pull/7";
        let mismatch = diverged_route("feature", Some(url));
        assert!(mismatch
            .message
            .contains("`feature` is not the head of an open PR at https://github.com/o/r/pull/7"));
        assert_eq!(
            mismatch.details["destination_convergence"]["next_action"],
            "pr_head_mismatch"
        );
        for task_url in [None, Some("https://github.com/o/r/issues/7")] {
            let route = diverged_route("feature", task_url);
            assert!(route.message.contains("existing PR work"));
        }
    }

    #[test]
    fn pr_history_classifies_states_and_fails_closed() {
        let pr = |state: &str, n: u32| serde_json::json!({ "state": state, "url": format!("https://github.com/o/r/pull/{n}") });
        let parse = |prs: serde_json::Value| parse_pr_history(prs.to_string().as_bytes());
        assert_eq!(parse(serde_json::json!([])).unwrap(), PrHistory::None);
        for state in ["MERGED", "CLOSED"] {
            assert_eq!(
                parse(serde_json::json!([pr(state, 1)])).unwrap(),
                PrHistory::Finished
            );
        }
        assert_eq!(
            parse(serde_json::json!([pr("MERGED", 1), pr("OPEN", 2)])).unwrap(),
            PrHistory::Open("https://github.com/o/r/pull/2".into())
        );
        assert!(parse(serde_json::json!([pr("OPEN", 1), pr("OPEN", 2)])).is_err());
        for response in [
            b"not json".as_slice(),
            b"{}",
            b"[{}]",
            br#"[{"state":"UNKNOWN","url":"https://github.com/o/r/pull/1"}]"#,
            br#"[{"state":"OPEN","url":"https://github.com/o/r/issues/1"}]"#,
        ] {
            assert!(parse_pr_history(response).is_err());
        }
    }

    struct Fixture {
        _root: tempfile::TempDir,
        source: PathBuf,
        destination: PathBuf,
        base: String,
    }

    use homeboy_test_support::git::output as git;

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
            std::fs::write(&gh, "#!/bin/sh\nprintf '[{\"state\":\"OPEN\",\"url\":\"https://github.com/Extra-Chill/homeboy/pull/42\"}]'\n").unwrap();
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
        // A transport outage is retried until the window is spent, not
        // abandoned after a fixed sub-second burst.
        let window = std::time::Duration::from_millis(1_000);
        let started = std::time::Instant::now();
        let error = retry_fetch_within::<()>(window, || {
            Err(Error::git_command_failed(
                "Could not resolve host: github.com",
            ))
        })
        .unwrap_err();
        assert!(started.elapsed() >= std::time::Duration::from_millis(750));
        assert!(started.elapsed() <= window);
        assert_eq!(error.retryable, Some(true));
        assert_eq!(error.details["failure_class"], "infrastructure");
        assert_eq!(error.details["transport_retry"]["attempts"], 3);
        let mut attempts = 0;
        let _ = retry_fetch::<()>(|| {
            attempts += 1;
            Err(Error::git_command_failed("missing ref"))
        });
        assert_eq!(attempts, 1);
    }
}
