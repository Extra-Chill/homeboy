//! Bounded, conflict-safe Git subtree publication.

use std::path::Path;
use std::time::Duration;

use crate::component::SubtreePublicationConfig;
use crate::error::{Error, Result};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SubtreePublicationEvidence {
    pub prefix: String,
    pub source_ref: String,
    pub split_commit: String,
    pub split_tree: String,
    pub remote: String,
    pub branch: String,
    pub tag: Option<String>,
    pub preview: bool,
    pub branch_action: String,
    pub tag_action: Option<String>,
}

/// Split `source_ref` and publish it without force-updating any destination ref.
pub fn publish_subtree(
    repo: &Path,
    config: &SubtreePublicationConfig,
    source_ref: &str,
    release_tag: Option<&str>,
    preview: bool,
) -> Result<SubtreePublicationEvidence> {
    validate_config(config)?;
    if source_ref.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "source_ref",
            "Subtree publication requires an exact source ref",
            None,
            None,
        ));
    }

    run(
        repo,
        &["rev-parse", "--verify", source_ref],
        "resolve subtree source",
    )?;
    let split = run(
        repo,
        &["subtree", "split", "--prefix", &config.prefix, source_ref],
        "split subtree",
    )?;
    let split_commit = split.lines().last().unwrap_or_default().trim().to_string();
    if split_commit.is_empty() {
        return Err(Error::git_command_failed(
            "git subtree split returned no commit",
        ));
    }
    let split_tree = run(
        repo,
        &["rev-parse", &format!("{split_commit}^{{tree}}")],
        "resolve subtree tree",
    )?;
    let split_tree = split_tree.trim().to_string();
    let refs = remote_refs(repo, config, release_tag)?;
    let branch_action = match refs.branch.as_deref() {
        None => "create".to_string(),
        Some(commit) => {
            let remote_tree = fetch_tree(repo, &config.remote, commit)?;
            if remote_tree != split_tree {
                return Err(conflict(
                    "branch",
                    &config.branch,
                    commit,
                    &remote_tree,
                    &split_tree,
                ));
            }
            "already-identical".to_string()
        }
    };
    let tag_action = if config.tag {
        let tag = release_tag.ok_or_else(|| {
            Error::validation_invalid_argument(
                "release_tag",
                "Subtree tag publication requires the exact release tag",
                None,
                None,
            )
        })?;
        Some(match refs.tag.as_deref() {
            None => "create".to_string(),
            Some(commit) if commit == split_commit => "already-identical".to_string(),
            Some(commit) => {
                return Err(Error::validation_invalid_argument(
                    "release_tag",
                    format!("Subtree tag {tag} already points to {commit}, refusing to move it"),
                    None,
                    None,
                ));
            }
        })
    } else {
        None
    };

    if !preview && (branch_action == "create" || tag_action.as_deref() == Some("create")) {
        let branch_ref = format!("{split_commit}:refs/heads/{}", config.branch);
        let tag_ref = release_tag.map(|tag| format!("{split_commit}:refs/tags/{tag}"));
        let mut args = vec!["push", &config.remote];
        if branch_action == "create" {
            args.push(&branch_ref);
        }
        if config.tag && tag_action.as_deref() == Some("create") {
            args.push(tag_ref.as_deref().expect("tag ref"));
        }
        run(repo, &args, "publish subtree")?;
    }

    Ok(SubtreePublicationEvidence {
        prefix: config.prefix.clone(),
        source_ref: source_ref.to_string(),
        split_commit,
        split_tree,
        remote: config.remote.clone(),
        branch: config.branch.clone(),
        tag: config.tag.then(|| release_tag.unwrap().to_string()),
        preview,
        branch_action,
        tag_action,
    })
}

struct RemoteRefs {
    branch: Option<String>,
    tag: Option<String>,
}

fn remote_refs(
    repo: &Path,
    config: &SubtreePublicationConfig,
    tag: Option<&str>,
) -> Result<RemoteRefs> {
    let branch_ref = format!("refs/heads/{}", config.branch);
    let mut args = vec!["ls-remote", config.remote.as_str(), branch_ref.as_str()];
    let tag_ref = tag.map(|tag| format!("refs/tags/{tag}"));
    let peeled_tag_ref = tag.map(|tag| format!("refs/tags/{tag}^{{}}"));
    if let Some(peeled_tag_ref) = peeled_tag_ref.as_deref() {
        args.push(peeled_tag_ref);
    }
    if let Some(tag_ref) = tag_ref.as_deref() {
        args.push(tag_ref);
    }
    let output = run(repo, &args, "inspect subtree destination")?;
    let mut branch = None;
    let mut found_tag = None;
    for line in output.lines() {
        let Some((sha, reference)) = line.split_once('\t') else {
            continue;
        };
        if reference == branch_ref {
            branch = Some(sha.to_string());
        }
        if peeled_tag_ref.as_deref() == Some(reference) || tag_ref.as_deref() == Some(reference) {
            found_tag = Some(sha.to_string());
        }
    }
    Ok(RemoteRefs {
        branch,
        tag: found_tag,
    })
}

fn fetch_tree(repo: &Path, remote: &str, commit: &str) -> Result<String> {
    run(
        repo,
        &["fetch", "--no-tags", remote, commit],
        "fetch subtree destination",
    )?;
    run(
        repo,
        &["rev-parse", &format!("{commit}^{{tree}}")],
        "verify subtree destination tree",
    )
    .map(|tree| tree.trim().to_string())
}

fn validate_config(config: &SubtreePublicationConfig) -> Result<()> {
    let prefix = Path::new(&config.prefix);
    if config.prefix.trim().is_empty()
        || prefix.is_absolute()
        || config
            .prefix
            .split('/')
            .any(|part| part == ".." || part.is_empty())
    {
        return Err(Error::validation_invalid_argument(
            "prefix",
            "Subtree prefix must be a non-empty relative path",
            None,
            None,
        ));
    }
    if config.remote.trim().is_empty()
        || config.branch.trim().is_empty()
        || config.branch.contains("..")
        || config.branch.starts_with('/')
    {
        return Err(Error::validation_invalid_argument(
            "subtree",
            "Subtree remote and branch must be non-empty safe refs",
            None,
            None,
        ));
    }
    Ok(())
}

fn conflict(kind: &str, name: &str, commit: &str, actual_tree: &str, expected_tree: &str) -> Error {
    Error::validation_invalid_argument(
        "subtree",
        format!("Subtree {kind} {name} conflicts: {commit} has tree {actual_tree}, expected {expected_tree}; refusing force push"),
        None,
        None,
    )
}

fn run(repo: &Path, args: &[&str], context: &str) -> Result<String> {
    let output =
        crate::git::run_git_output_with_env_timeout(repo, args, context, &[], GIT_TIMEOUT)?;
    if !output.status.success() {
        return Err(Error::git_command_failed_with_details(
            format!("{context} failed"),
            crate::error::GitCommandFailedDetails {
                command: format!("git {}", args.join(" ")),
                cwd: repo.display().to_string(),
                exit_code: output.status.code(),
                stdout: String::from_utf8_lossy(&output.stdout).trim().to_string(),
                stderr: String::from_utf8_lossy(&output.stderr).trim().to_string(),
                io_error: None,
            },
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;

    fn git(dir: &Path, args: &[&str]) {
        assert!(
            Command::new("git")
                .args(args)
                .current_dir(dir)
                .output()
                .unwrap()
                .status
                .success(),
            "git {:?}",
            args
        );
    }

    #[test]
    fn publishes_nested_subtree_and_retries_identically() {
        let root = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.path().join("a/b")).unwrap();
        std::fs::write(root.path().join("a/b/file"), "one").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release"]);
        git(root.path(), &["tag", "v1"]);
        let config = SubtreePublicationConfig {
            prefix: "a/b".into(),
            remote: remote.path().display().to_string(),
            branch: "main".into(),
            tag: true,
        };
        let first =
            publish_subtree(root.path(), &config, "refs/tags/v1", Some("v1"), false).unwrap();
        assert_eq!(first.branch_action, "create");
        let retry =
            publish_subtree(root.path(), &config, "refs/tags/v1", Some("v1"), false).unwrap();
        assert_eq!(retry.branch_action, "already-identical");
    }

    #[test]
    fn preview_does_not_publish() {
        let root = tempfile::tempdir().unwrap();
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.path().join("pkg")).unwrap();
        std::fs::write(root.path().join("pkg/file"), "one").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release"]);
        git(root.path(), &["tag", "v1"]);
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        let config = SubtreePublicationConfig {
            prefix: "pkg".into(),
            remote: remote.path().display().to_string(),
            branch: "main".into(),
            tag: false,
        };
        let evidence = publish_subtree(root.path(), &config, "v1", None, true).unwrap();
        assert!(evidence.preview);
        assert!(std::fs::read_dir(remote.path().join("refs/heads"))
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn rejects_destination_tree_conflict_without_force() {
        let root = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.path().join("pkg")).unwrap();
        std::fs::write(root.path().join("pkg/file"), "source").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release"]);
        git(root.path(), &["tag", "v1"]);
        let config = SubtreePublicationConfig {
            prefix: "pkg".into(),
            remote: remote.path().display().to_string(),
            branch: "main".into(),
            tag: false,
        };
        publish_subtree(root.path(), &config, "v1", None, false).unwrap();
        let other = tempfile::tempdir().unwrap();
        git(
            other.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        git(other.path(), &["config", "user.email", "test@example.com"]);
        git(other.path(), &["config", "user.name", "Test"]);
        std::fs::write(other.path().join("file"), "conflict").unwrap();
        git(other.path(), &["add", "."]);
        git(other.path(), &["commit", "-qm", "conflict"]);
        git(other.path(), &["push", "origin", "main"]);
        let error = publish_subtree(root.path(), &config, "v1", None, false)
            .expect_err("different destination tree must be rejected");
        assert!(error.message.contains("refusing force push"));
    }
}
