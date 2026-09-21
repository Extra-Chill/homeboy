//! Bounded, conflict-safe Git subtree publication.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::component::{SubtreeBranchPolicy, SubtreePublicationConfig};
use crate::error::{Error, Result};

const GIT_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SubtreePublicationEvidence {
    pub prefix: String,
    pub source_repo_root: String,
    pub source_ref: String,
    pub source_commit: String,
    pub split_commit: String,
    pub split_tree: String,
    pub remote: String,
    pub branch: String,
    pub destination_branch_ref: String,
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
    release_version: Option<&str>,
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

    let repo_root = PathBuf::from(
        run(
            repo,
            &["rev-parse", "--show-toplevel"],
            "resolve subtree repository root",
        )?
        .trim(),
    );
    let source_commit = run(
        &repo_root,
        &["rev-parse", "--verify", source_ref],
        "resolve subtree source",
    )?;
    let source_commit = run(
        &repo_root,
        &[
            "rev-parse",
            "--verify",
            &format!("{}^{{commit}}", source_commit.trim()),
        ],
        "peel subtree source commit",
    )?
    .trim()
    .to_string();
    let source_tree = if config.prefix == "." {
        run(
            &repo_root,
            &["rev-parse", &format!("{source_commit}^{{tree}}")],
            "resolve source tree",
        )?
    } else {
        run(
            &repo_root,
            &["rev-parse", &format!("{source_commit}:{}", config.prefix)],
            "resolve source subtree tree",
        )?
    };
    let source_tree = source_tree.trim().to_string();
    let split = run(
        &repo_root,
        &[
            "subtree",
            "split",
            "--prefix",
            &config.prefix,
            &source_commit,
        ],
        "split subtree",
    )?;
    let split_commit = split.lines().last().unwrap_or_default().trim().to_string();
    if split_commit.is_empty() {
        return Err(Error::git_command_failed(
            "git subtree split returned no commit",
        ));
    }
    let split_tree = run(
        &repo_root,
        &["rev-parse", &format!("{split_commit}^{{tree}}")],
        "resolve subtree tree",
    )?;
    let split_tree = split_tree.trim().to_string();
    if split_tree != source_tree {
        return Err(Error::validation_invalid_argument(
            "subtree",
            format!("Subtree split tree {split_tree} does not equal source commit subtree tree {source_tree}; refusing publication"),
            None,
            None,
        ));
    }
    let destination_tag = destination_tag(config, release_version)?;
    let refs = remote_refs(&repo_root, config, destination_tag.as_deref())?;
    let destination_branch_ref = format!("refs/heads/{}", config.branch);
    let branch_action = match refs.branch.as_deref() {
        None if config.branch_policy == SubtreeBranchPolicy::TagOnly => "tag-only".to_string(),
        None => "create".to_string(),
        Some(commit) => {
            let remote_tree = fetch_tree(&repo_root, &config.remote, commit)?;
            if config.branch_policy == SubtreeBranchPolicy::TagOnly {
                "tag-only".to_string()
            } else if remote_tree == split_tree {
                "already-identical".to_string()
            } else if is_ancestor(&repo_root, commit, &split_commit)? {
                "update".to_string()
            } else {
                return Err(conflict(
                    "branch",
                    &config.branch,
                    commit,
                    &remote_tree,
                    &split_tree,
                ));
            }
        }
    };
    let tag_action = if config.tag {
        let tag = destination_tag.as_deref().expect("destination tag");
        Some(match refs.tag.as_deref() {
            None => "create".to_string(),
            Some(commit) if commit == split_commit => "already-identical".to_string(),
            Some(commit) => {
                return Err(Error::validation_invalid_argument(
                    "destination_tag",
                    format!("Subtree tag {tag} already points to {commit}, refusing to move it"),
                    None,
                    None,
                ));
            }
        })
    } else {
        None
    };

    if !preview
        && (branch_action == "create"
            || branch_action == "update"
            || tag_action.as_deref() == Some("create"))
    {
        let branch_ref = format!("{split_commit}:refs/heads/{}", config.branch);
        let tag_ref = destination_tag
            .as_deref()
            .map(|tag| format!("{split_commit}:refs/tags/{tag}"));
        let mut args = vec!["push", "--atomic", &config.remote];
        if branch_action == "create" || branch_action == "update" {
            args.push(&branch_ref);
        }
        if config.tag && tag_action.as_deref() == Some("create") {
            args.push(tag_ref.as_deref().expect("tag ref"));
        }
        run(&repo_root, &args, "publish subtree")?;
    }

    Ok(SubtreePublicationEvidence {
        prefix: config.prefix.clone(),
        source_repo_root: repo_root.display().to_string(),
        source_ref: source_ref.to_string(),
        source_commit,
        split_commit,
        split_tree,
        remote: config.remote.clone(),
        branch: config.branch.clone(),
        destination_branch_ref,
        tag: destination_tag,
        preview,
        branch_action,
        tag_action,
    })
}

fn destination_tag(
    config: &SubtreePublicationConfig,
    release_version: Option<&str>,
) -> Result<Option<String>> {
    if !config.tag {
        return Ok(None);
    }
    let version = release_version.ok_or_else(|| {
        Error::validation_invalid_argument(
            "release_version",
            "Subtree tag publication requires the release version",
            None,
            None,
        )
    })?;
    let template = config.tag_template.as_deref().unwrap_or("v{version}");
    if !template.contains("{version}")
        || (template.contains('{')
            && template.matches('{').count() != template.matches('}').count())
    {
        return Err(Error::validation_invalid_argument(
            "tag_template",
            "Subtree tag template must contain {version}",
            Some(template.to_string()),
            None,
        ));
    }
    let tag = template.replace("{version}", version);
    if tag.is_empty() || tag.contains("..") || tag.starts_with('/') {
        return Err(Error::validation_invalid_argument(
            "tag_template",
            "Subtree tag template rendered an unsafe tag",
            Some(tag),
            None,
        ));
    }
    Ok(Some(tag))
}

fn is_ancestor(repo: &Path, ancestor: &str, descendant: &str) -> Result<bool> {
    let output = crate::git::run_git_output_with_env_timeout(
        repo,
        &["merge-base", "--is-ancestor", ancestor, descendant],
        "verify subtree fast-forward",
        &[],
        GIT_TIMEOUT,
    )?;
    Ok(output.status.success())
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
        if peeled_tag_ref.as_deref() == Some(reference) {
            found_tag = Some(sha.to_string());
        } else if found_tag.is_none() && tag_ref.as_deref() == Some(reference) {
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
            ..Default::default()
        };
        let first =
            publish_subtree(root.path(), &config, "refs/tags/v1", Some("1"), false).unwrap();
        assert_eq!(first.branch_action, "create");
        let tagger = tempfile::tempdir().unwrap();
        git(
            tagger.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        git(tagger.path(), &["config", "user.email", "test@example.com"]);
        git(tagger.path(), &["config", "user.name", "Test"]);
        git(tagger.path(), &["tag", "-d", "v1"]);
        git(tagger.path(), &["tag", "-a", "v1", "-m", "v1", "main"]);
        git(tagger.path(), &["push", "origin", ":refs/tags/v1"]);
        git(tagger.path(), &["push", "origin", "v1"]);
        let retry =
            publish_subtree(root.path(), &config, "refs/tags/v1", Some("1"), false).unwrap();
        assert_eq!(retry.branch_action, "already-identical");
        assert_eq!(retry.tag_action.as_deref(), Some("already-identical"));
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
            ..Default::default()
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
            ..Default::default()
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

    #[test]
    fn successive_releases_fast_forward_and_old_tag_replay_conflicts() {
        let root = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.path().join("packages/example")).unwrap();
        std::fs::write(root.path().join("packages/example/file"), "one").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release one"]);
        git(root.path(), &["tag", "source-v1"]);
        let config = SubtreePublicationConfig {
            prefix: "packages/example".into(),
            remote: remote.path().display().to_string(),
            branch: "main".into(),
            tag: true,
            tag_template: Some("v{version}".into()),
            ..Default::default()
        };
        let first =
            publish_subtree(root.path(), &config, "source-v1", Some("1.0.0"), false).unwrap();
        assert_eq!(first.tag.as_deref(), Some("v1.0.0"));

        std::fs::write(root.path().join("packages/example/file"), "two").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "release two"]);
        git(root.path(), &["tag", "source-v2"]);
        let second =
            publish_subtree(root.path(), &config, "source-v2", Some("2.0.0"), false).unwrap();
        assert_eq!(second.branch_action, "update");
        assert_eq!(second.tag.as_deref(), Some("v2.0.0"));

        let tag_only = SubtreePublicationConfig {
            branch_policy: SubtreeBranchPolicy::TagOnly,
            ..config.clone()
        };
        let tag_only_replay =
            publish_subtree(root.path(), &tag_only, "source-v1", Some("1.0.0"), false).unwrap();
        assert_eq!(tag_only_replay.branch_action, "tag-only");

        let replay = publish_subtree(root.path(), &config, "source-v1", Some("1.0.0"), false)
            .expect_err("replaying an old split against a newer branch must conflict");
        assert!(replay.message.contains("refusing force push"));
    }

    #[test]
    fn nested_component_invocation_excludes_unrelated_siblings() {
        let root = tempfile::tempdir().unwrap();
        let remote = tempfile::tempdir().unwrap();
        git(remote.path(), &["init", "--bare", "-q"]);
        git(root.path(), &["init", "-q", "-b", "main"]);
        git(root.path(), &["config", "user.email", "test@example.com"]);
        git(root.path(), &["config", "user.name", "Test"]);
        std::fs::create_dir_all(root.path().join("packages/example")).unwrap();
        std::fs::create_dir_all(root.path().join("packages/other")).unwrap();
        std::fs::write(root.path().join("packages/example/file"), "included").unwrap();
        std::fs::write(root.path().join("packages/other/file"), "excluded").unwrap();
        git(root.path(), &["add", "."]);
        git(root.path(), &["commit", "-qm", "nested component release"]);
        git(root.path(), &["tag", "source-v1"]);
        let config = SubtreePublicationConfig {
            prefix: "packages/example".into(),
            remote: remote.path().display().to_string(),
            branch: "main".into(),
            tag: false,
            ..Default::default()
        };
        let component_path = root.path().join("packages/example");
        let evidence = publish_subtree(&component_path, &config, "source-v1", None, false).unwrap();
        assert_eq!(evidence.source_repo_root, root.path().display().to_string());
        let clone = tempfile::tempdir().unwrap();
        git(
            clone.path(),
            &["clone", remote.path().to_str().unwrap(), "."],
        );
        assert!(clone.path().join("file").is_file());
        assert!(!clone.path().join("packages/other/file").exists());
    }
}
