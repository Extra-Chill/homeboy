//! GitHub's commit-associated pull-request endpoint supplies the missing
//! provenance for a squash merge: it associates the installed PR commit with
//! the merged PR even when that commit is not an ancestor of the merge.

use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct PullRequest {
    merged_at: Option<String>,
    merge_commit_sha: Option<String>,
    base: Repository,
}

#[derive(Debug, Deserialize)]
struct Repository {
    repo: Option<RepoName>,
}

#[derive(Debug, Deserialize)]
struct RepoName {
    full_name: Option<String>,
}

/// Resolve a trusted merge commit associated by GitHub with `candidate`.
/// The PR must target the repository named by the configured source URL.
pub(super) fn associated_merge_commit(
    source: &str,
    candidate: &str,
    mut get: impl FnMut(&str) -> Result<String, String>,
) -> Result<String, String> {
    if !is_commit(candidate) {
        return Err("installed Homeboy identity is not a full commit SHA".into());
    }
    let Some((owner, repo)) = github_repository(source) else {
        return Err("source remote is not a supported GitHub repository URL".into());
    };
    let expected = format!("{owner}/{repo}");
    let url = format!("https://api.github.com/repos/{owner}/{repo}/commits/{candidate}/pulls");
    let body = get(&url)?;
    let pulls: Vec<PullRequest> = serde_json::from_str(&body)
        .map_err(|error| format!("GitHub returned invalid commit-associated PR data: {error}"))?;
    let merged = pulls.into_iter().find_map(|pull| {
        let target = pull.base.repo?.full_name?;
        if !target.eq_ignore_ascii_case(&expected)
            || pull.merged_at.as_deref().map_or(true, str::is_empty)
        {
            return None;
        }
        let merge = pull.merge_commit_sha?;
        is_commit(&merge).then_some(merge)
    });
    merged.ok_or_else(|| {
        format!("GitHub found no merged PR for commit {candidate} targeting {expected}")
    })
}

pub(super) fn proves_forward(
    source: &str,
    candidate: &str,
    destination: &str,
    get: impl FnMut(&str) -> Result<String, String>,
    mut is_ancestor: impl FnMut(&str, &str) -> Result<bool, String>,
) -> Result<bool, String> {
    let merge = associated_merge_commit(source, candidate, get)?;
    if is_ancestor(&merge, destination)? {
        Ok(true)
    } else {
        Err(format!(
            "merged PR commit {merge} is not an ancestor of requested destination {destination}"
        ))
    }
}

pub(super) fn github_api_get(url: &str) -> Result<String, String> {
    let response = reqwest::blocking::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .user_agent(concat!("homeboy/", env!("CARGO_PKG_VERSION")))
        .build()
        .and_then(|client| {
            client
                .get(url)
                .header("Accept", "application/vnd.github+json")
                .send()
        })
        .map_err(|error| format!("GitHub PR provenance lookup failed: {error}"))?;
    if !response.status().is_success() {
        return Err(format!(
            "GitHub PR provenance lookup returned HTTP {}",
            response.status()
        ));
    }
    response
        .text()
        .map_err(|error| format!("could not read GitHub PR provenance response: {error}"))
}

fn github_repository(source: &str) -> Option<(String, String)> {
    let value = source.trim().trim_end_matches('/').trim_end_matches(".git");
    let path = if let Some(rest) = value.strip_prefix("https://github.com/") {
        rest
    } else if let Some(rest) = value.strip_prefix("ssh://git@github.com/") {
        rest
    } else if let Some(rest) = value.strip_prefix("git@github.com:") {
        rest
    } else {
        return None;
    };
    let mut parts = path.split('/');
    let owner = parts.next()?.to_string();
    let repo = parts.next()?.to_string();
    let valid = |part: &str| {
        !part.is_empty()
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
    };
    (parts.next().is_none() && valid(&owner) && valid(&repo)).then_some((owner, repo))
}

pub(super) fn is_canonical_homeboy_source(source: &str) -> bool {
    github_repository(source).is_some_and(|(owner, repo)| {
        owner.eq_ignore_ascii_case("Extra-Chill") && repo.eq_ignore_ascii_case("homeboy")
    })
}

fn is_commit(value: &str) -> bool {
    value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    const HEAD: &str = "fdc76d461388c5fca38b2a3743fdb33487347cef";
    const MERGE: &str = "07b0fa3d9b9464dc7efe8d7c9d96dda51d37c324";
    const PR: &str = r#"[{"merged_at":"2026-09-25T11:26:31Z","merge_commit_sha":"07b0fa3d9b9464dc7efe8d7c9d96dda51d37c324","base":{"repo":{"full_name":"Extra-Chill/homeboy"}}}]"#;

    fn git(dir: &std::path::Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(dir)
            .output()
            .expect("run git");
        assert!(
            output.status.success(),
            "git {:?}: {}",
            args,
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn commit(dir: &std::path::Path, message: &str, contents: &str) -> String {
        std::fs::write(dir.join("file"), contents).expect("write fixture");
        git(dir, &["add", "file"]);
        git(dir, &["commit", "--quiet", "-m", message]);
        git(dir, &["rev-parse", "HEAD"])
    }

    #[test]
    fn resolves_commit_associated_squash_pr_and_requires_target_ancestry() {
        let merge =
            associated_merge_commit("https://github.com/Extra-Chill/homeboy.git", HEAD, |_| {
                Ok(PR.into())
            })
            .expect("merged PR");
        assert_eq!(merge, MERGE);
        // The caller must additionally prove this merge commit is an ancestor
        // of the requested release; an older/unrelated target cannot converge.
        let get = |_: &str| -> Result<String, String> { Ok(PR.to_string()) };
        let dag = |ancestor: &str, target: &str| -> Result<bool, String> {
            Ok(ancestor == MERGE && (target == "release-descendant" || target == MERGE))
        };
        assert!(proves_forward(
            "https://github.com/Extra-Chill/homeboy.git",
            HEAD,
            "release-descendant",
            get,
            dag
        )
        .unwrap());
        let older = proves_forward(
            "https://github.com/Extra-Chill/homeboy.git",
            HEAD,
            "older-release",
            |_| Ok(PR.to_string()),
            dag,
        )
        .expect_err("unintegrated destination has a typed comparison cause");
        assert!(older.contains("not an ancestor"));
    }

    #[test]
    fn squash_merge_proof_uses_actual_git_ancestry_and_rejects_older_target() {
        let dir = tempfile::tempdir().expect("git fixture");
        git(dir.path(), &["init", "--quiet", "--initial-branch=main"]);
        git(
            dir.path(),
            &["config", "user.email", "homeboy@example.test"],
        );
        git(dir.path(), &["config", "user.name", "Homeboy Test"]);
        let base = commit(dir.path(), "base", "base\n");
        git(dir.path(), &["checkout", "--quiet", "-b", "pr"]);
        let pr_head = commit(dir.path(), "PR change", "pr change\n");
        git(dir.path(), &["checkout", "--quiet", "main"]);
        let squash = commit(dir.path(), "squash PR #123", "pr change\n");
        let release = commit(dir.path(), "release", "release\n");
        let response = format!(
            r#"[{{"merged_at":"2026-09-25T11:26:31Z","merge_commit_sha":"{squash}","base":{{"repo":{{"full_name":"Extra-Chill/homeboy"}}}}}}]"#
        );
        let is_ancestor = |ancestor: &str, target: &str| {
            let output = Command::new("git")
                .args([
                    "-C",
                    dir.path().to_str().unwrap(),
                    "merge-base",
                    "--is-ancestor",
                    ancestor,
                    target,
                ])
                .status()
                .expect("git ancestry");
            Ok(output.success())
        };
        assert!(proves_forward(
            "https://github.com/Extra-Chill/homeboy.git",
            &pr_head,
            &release,
            |_: &str| Ok(response.clone()),
            is_ancestor
        )
        .expect("proof"));
        let older = proves_forward(
            "https://github.com/Extra-Chill/homeboy.git",
            &pr_head,
            &base,
            |_: &str| Ok(response.clone()),
            is_ancestor,
        )
        .expect_err("older target is not integrated");
        assert!(older.contains("not an ancestor"));
    }

    #[test]
    fn rejects_unmerged_wrong_repository_and_unavailable_api() {
        let unmerged = PR.replace(
            "\"merged_at\":\"2026-09-25T11:26:31Z\"",
            "\"merged_at\":null",
        );
        let unmerged_error =
            associated_merge_commit("https://github.com/Extra-Chill/homeboy", HEAD, |_: &str| {
                Ok(unmerged.clone())
            })
            .expect_err("unmerged PR is not integration provenance");
        assert!(unmerged_error.contains("no merged PR"));
        let wrong_repo = PR.replace("Extra-Chill/homeboy", "other/homeboy");
        assert!(associated_merge_commit(
            "git@github.com:Extra-Chill/homeboy.git",
            HEAD,
            |_: &str| Ok(wrong_repo.clone())
        )
        .is_err());
        assert!(
            associated_merge_commit("https://github.com/Extra-Chill/homeboy", HEAD, |_| Err(
                "offline".into()
            ))
            .is_err()
        );
        assert!(
            associated_merge_commit("https://example.test/homeboy", HEAD, |_| Ok(PR.into()))
                .is_err()
        );
        assert!(is_canonical_homeboy_source(
            "git@github.com:Extra-Chill/homeboy.git"
        ));
        assert!(!is_canonical_homeboy_source(
            "https://github.com/untrusted/homeboy.git"
        ));
    }
}
