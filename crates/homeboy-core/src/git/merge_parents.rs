//! The merges a candidate carries beyond its first parent.
//!
//! Cook publishes a candidate as a tree on the destination base. A candidate
//! that merged another line of history (trunk, an upstream) must keep that
//! line as a parent, or the published branch conflicts again with exactly
//! what was merged (#17346).

use std::path::Path;
use std::process::Command;

use crate::error::{Error, Result};

/// The commits merged into `head` since `base` that `base` does not already
/// contain: every later parent of a merge in `base..head`, reduced to the
/// newest of each line of history, in first-seen order.
pub fn merged_parents(repo: &Path, base: &str, head: &str) -> Result<Vec<String>> {
    let range = format!("{base}..{head}");
    let merges = git(repo, &["rev-list", "--merges", "--parents", &range])?;
    let mut merged: Vec<String> = Vec::new();
    for parent in merges
        .lines()
        .flat_map(|line| line.split_whitespace().skip(2))
    {
        if !merged.iter().any(|seen| seen == parent) && !is_ancestor(repo, parent, base)? {
            merged.push(parent.to_string());
        }
    }
    let mut newest = Vec::with_capacity(merged.len());
    for parent in &merged {
        let mut superseded = false;
        for other in merged.iter().filter(|other| *other != parent) {
            if is_ancestor(repo, parent, other)? {
                superseded = true;
                break;
            }
        }
        if !superseded {
            newest.push(parent.clone());
        }
    }
    Ok(newest)
}

/// Express `parents` as the checkout's pending merge, in Git's own terms:
/// `MERGE_HEAD` lists them and `MERGE_MODE` keeps every parent (`no-ff`), so
/// committing the staged tree there produces the merge Cook publishes. No
/// parents clears it.
pub fn record_pending_merge(repo: &Path, parents: &[String]) -> Result<()> {
    for (name, contents) in [
        ("MERGE_HEAD", format!("{}\n", parents.join("\n"))),
        ("MERGE_MODE", "no-ff".to_string()),
    ] {
        let path = git(
            repo,
            &["rev-parse", "--path-format=absolute", "--git-path", name],
        )?;
        let path = Path::new(&path);
        let result = if parents.is_empty() {
            std::fs::remove_file(path).or_else(|error| {
                (error.kind() == std::io::ErrorKind::NotFound)
                    .then_some(())
                    .ok_or(error)
            })
        } else {
            std::fs::write(path, &contents)
        };
        result.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("record {}", path.display())),
            )
        })?;
    }
    Ok(())
}

fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = Command::new("git")
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .current_dir(repo)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(Error::git_command_failed(format!(
            "git merge-base --is-ancestor {ancestor} {descendant}: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))),
    }
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if !output.status.success() {
        return Err(Error::git_command_failed(format!(
            "git {}: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        )));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(repo: &Path, args: &[&str]) -> String {
        git(repo, args).unwrap()
    }

    /// A branch that merged trunk, then merged a newer trunk, carries only
    /// the newest trunk; history already in the base is not a merged parent.
    #[test]
    fn merged_parents_are_the_newest_lines_merged_since_the_base() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        run(repo, &["init", "-q", "-b", "trunk"]);
        run(repo, &["config", "user.email", "t@t"]);
        run(repo, &["config", "user.name", "t"]);
        let commit = |name: &str| {
            std::fs::write(repo.join(name), name).unwrap();
            run(repo, &["add", name]);
            run(repo, &["commit", "-qm", name]);
            run(repo, &["rev-parse", "HEAD"])
        };
        commit("root");
        run(repo, &["checkout", "-qb", "branch"]);
        let base = commit("branch");
        run(repo, &["checkout", "-q", "trunk"]);
        commit("trunk1");
        run(repo, &["checkout", "-q", "branch"]);
        run(repo, &["merge", "-q", "--no-edit", "trunk"]);
        run(repo, &["checkout", "-q", "trunk"]);
        let trunk2 = commit("trunk2");
        run(repo, &["checkout", "-q", "branch"]);
        run(repo, &["merge", "-q", "--no-edit", "trunk"]);
        let head = run(repo, &["rev-parse", "HEAD"]);

        assert_eq!(
            merged_parents(repo, &base, &head).unwrap(),
            vec![trunk2.clone()]
        );
        assert!(merged_parents(repo, &head, &head).unwrap().is_empty());

        record_pending_merge(repo, &[trunk2.clone()]).unwrap();
        assert_eq!(run(repo, &["rev-parse", "MERGE_HEAD"]), trunk2);
        // Committing there keeps both parents even where Git could fast-forward.
        run(repo, &["reset", "-q", "--soft", &base]);
        record_pending_merge(repo, &[trunk2.clone()]).unwrap();
        run(repo, &["commit", "-qm", "staged merge"]);
        assert_eq!(
            run(repo, &["rev-parse", "HEAD^1", "HEAD^2"]),
            format!("{base}\n{trunk2}")
        );
        record_pending_merge(repo, &[]).unwrap();
        assert!(git(repo, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_err());
    }
}
