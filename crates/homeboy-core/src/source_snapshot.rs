//! Source-snapshot collection behavior.
//!
//! The `SourceSnapshot` / `SourceSnapshotPolicy` data types live in the leaf
//! `homeboy-source-snapshot-contract` crate. The collection behavior below
//! (git status/sha hashing, component/extension/gitignore-driven exclude
//! discovery, env-driven policy) reaches into git, the component inventory, the
//! extension store, and the filesystem, so it stays in core as free functions.

use std::fs;
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

pub use homeboy_source_snapshot_contract::source_snapshot::{
    default_sync_excludes, SourceSnapshot, SourceSnapshotPolicy,
};

use crate::git;
use crate::runner_execution_envelope::PATH_MATERIALIZATION_MODE_EXISTING_REMOTE;
#[cfg(test)]
use crate::runner_execution_envelope::PATH_MATERIALIZATION_MODE_SNAPSHOT;

const SOURCE_SYNC_EXCLUDES_ENV: &str = "HOMEBOY_SOURCE_SYNC_EXCLUDES";

/// Build a policy from the base defaults plus env-configured excludes.
pub fn policy_from_env() -> SourceSnapshotPolicy {
    let mut policy = SourceSnapshotPolicy::default();
    policy.extend_sync_excludes(split_env_list(SOURCE_SYNC_EXCLUDES_ENV));
    policy
}

/// Build a policy for a specific path: env excludes plus component-extension and
/// gitignore-derived excludes discovered from that path.
pub fn policy_for_path(path: &Path) -> SourceSnapshotPolicy {
    let mut policy = policy_from_env();
    policy.extend_sync_excludes(component_extension_sync_excludes(path));
    policy.extend_sync_excludes(gitignore_sync_excludes(path));
    policy
}

pub fn collect_local(
    runner_id: &str,
    path: &Path,
    remote_path: Option<&str>,
    sync_mode: &str,
) -> SourceSnapshot {
    let policy = policy_for_path(path);
    collect_local_with_policy(runner_id, path, remote_path, sync_mode, &policy)
}

pub(crate) fn collect_local_with_policy(
    runner_id: &str,
    path: &Path,
    remote_path: Option<&str>,
    sync_mode: &str,
    policy: &SourceSnapshotPolicy,
) -> SourceSnapshot {
    let local_path = path.display().to_string();
    let git_root = git::toplevel(path);
    let git_branch = git::current_branch(path)
        .filter(|branch| !branch.is_empty())
        .or_else(|| git::output_optional(path, &["rev-parse", "--abbrev-ref", "HEAD"]));
    let git_sha = git::head_sha(path);
    let status = git::status_porcelain_bytes(path).unwrap_or_default();
    let dirty = !status.is_empty();
    let snapshot_hash = if git_sha.is_some() {
        git_snapshot_hash(path, git_sha.as_deref(), &status)
    } else {
        generic_snapshot_hash(&local_path)
    };

    SourceSnapshot {
        runner_id: runner_id.to_string(),
        local_path: Some(local_path),
        remote_path: remote_path.map(str::to_string),
        workspace_root: git_root,
        git_branch,
        git_sha,
        dirty,
        sync_mode: sync_mode.to_string(),
        workspace_snapshot_identity: None,
        synthetic_checkout_commit: None,
        synthetic_checkout_ref: None,
        synthetic_checkout_tree: None,
        snapshot_hash,
        synced_at: chrono::Utc::now().to_rfc3339(),
        sync_excludes: policy.sync_excludes.clone(),
    }
}

pub fn existing_remote(
    runner_id: &str,
    remote_path: &str,
    workspace_root: Option<&str>,
) -> SourceSnapshot {
    let policy = policy_from_env();
    existing_remote_with_policy(runner_id, remote_path, workspace_root, &policy)
}

pub(crate) fn existing_remote_with_policy(
    runner_id: &str,
    remote_path: &str,
    workspace_root: Option<&str>,
    policy: &SourceSnapshotPolicy,
) -> SourceSnapshot {
    let mut hasher = Sha256::new();
    hasher.update(PATH_MATERIALIZATION_MODE_EXISTING_REMOTE.as_bytes());
    hasher.update(b"\0");
    hasher.update(runner_id.as_bytes());
    hasher.update(b"\0");
    hasher.update(remote_path.as_bytes());
    if let Some(workspace_root) = workspace_root {
        hasher.update(b"\0");
        hasher.update(workspace_root.as_bytes());
    }

    SourceSnapshot {
        runner_id: runner_id.to_string(),
        local_path: None,
        remote_path: Some(remote_path.to_string()),
        workspace_root: workspace_root.map(str::to_string),
        git_branch: None,
        git_sha: None,
        dirty: false,
        sync_mode: PATH_MATERIALIZATION_MODE_EXISTING_REMOTE.to_string(),
        workspace_snapshot_identity: None,
        synthetic_checkout_commit: None,
        synthetic_checkout_ref: None,
        synthetic_checkout_tree: None,
        snapshot_hash: format!("sha256:{:x}", hasher.finalize()),
        synced_at: chrono::Utc::now().to_rfc3339(),
        sync_excludes: policy.sync_excludes.clone(),
    }
}

pub fn declared_sync_excludes_for_path(path: &Path) -> Vec<String> {
    let mut excludes = Vec::new();
    append_unique(&mut excludes, component_extension_sync_excludes(path));
    append_unique(&mut excludes, gitignore_sync_excludes(path));
    excludes
}

fn component_extension_sync_excludes(path: &Path) -> Vec<String> {
    let Ok(path) = fs::canonicalize(path) else {
        return Vec::new();
    };
    let Ok(components) = crate::component::inventory() else {
        return Vec::new();
    };

    let mut excludes = Vec::new();
    for component in components {
        let component_path = PathBuf::from(shellexpand::tilde(&component.local_path).into_owned());
        let Ok(component_path) = fs::canonicalize(component_path) else {
            continue;
        };
        if component_path != path {
            continue;
        }
        let Some(extensions) = component.extensions.as_ref() else {
            continue;
        };
        let mut extension_ids = extensions.keys().collect::<Vec<_>>();
        extension_ids.sort();
        for extension_id in extension_ids {
            let Ok(extension) = crate::extension_store::load_extension(extension_id) else {
                continue;
            };
            if let Some(source_snapshot) = extension.source_snapshot {
                append_unique(&mut excludes, source_snapshot.sync_excludes);
            }
        }
    }
    excludes
}

pub(crate) fn gitignore_sync_excludes(path: &Path) -> Vec<String> {
    let mut excludes = Vec::new();
    if let Ok(contents) = fs::read_to_string(path.join(".gitignore")) {
        for line in contents.lines().map(str::trim) {
            if line.is_empty() || line.starts_with('#') || line.starts_with('!') {
                continue;
            }
            append_gitignore_exclude(&mut excludes, line);
        }
    }

    let Some(output) = git::output_optional(
        path,
        &[
            "ls-files",
            "--others",
            "--ignored",
            "--exclude-standard",
            "--directory",
        ],
    ) else {
        return excludes;
    };

    for line in output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        append_gitignore_exclude(&mut excludes, line);
    }
    excludes
}

fn append_gitignore_exclude(excludes: &mut Vec<String>, raw: &str) {
    let pattern = raw.trim_start_matches("./");
    let pattern = if let Some(pattern) = pattern.strip_prefix('/') {
        format!("./{pattern}")
    } else {
        pattern.to_string()
    };
    if pattern.is_empty() {
        return;
    }
    if pattern.ends_with('/') {
        let base = pattern.trim_end_matches('/');
        append_unique(excludes, [base.to_string(), format!("{base}/**")]);
    } else {
        append_unique(excludes, [pattern]);
    }
}

fn append_unique<I, S>(values: &mut Vec<String>, items: I)
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    for item in items.into_iter().map(Into::into) {
        if !item.trim().is_empty() && !values.contains(&item) {
            values.push(item);
        }
    }
}

fn split_env_list(name: &str) -> Vec<String> {
    std::env::var(name)
        .ok()
        .into_iter()
        .flat_map(|raw| {
            raw.split(',')
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

fn git_snapshot_hash(path: &Path, git_sha: Option<&str>, status: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"homeboy-source-snapshot-v1\0");
    if let Some(git_sha) = git_sha {
        hasher.update(git_sha.as_bytes());
    }
    hasher.update(b"\0status\0");
    hasher.update(status);

    if status.is_empty() {
        if let Some(tree) = git::output_optional(path, &["rev-parse", "HEAD^{tree}"]) {
            hasher.update(b"\0tree\0");
            hasher.update(tree.as_bytes());
        }
    } else {
        if let Some(diff) = git::output_optional_bytes(path, &["diff", "--binary", "HEAD"]) {
            hasher.update(b"\0diff\0");
            hasher.update(diff);
        }
        if let Some(untracked) =
            git::output_optional_bytes(path, &["ls-files", "--others", "--exclude-standard", "-z"])
        {
            hasher.update(b"\0untracked\0");
            for relative in untracked
                .split(|byte| *byte == 0)
                .filter(|entry| !entry.is_empty())
            {
                hasher.update(relative);
                hasher.update(b"\0");
                if let Ok(relative) = std::str::from_utf8(relative) {
                    let file = PathBuf::from(path).join(relative);
                    if let Ok(bytes) = fs::read(&file) {
                        hasher.update(bytes);
                    }
                }
            }
        }
    }

    format!("sha256:{:x}", hasher.finalize())
}

fn generic_snapshot_hash(identity: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"non_git_path\0");
    hasher.update(identity.as_bytes());
    format!("sha256:{:x}", hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::process::Command;

    #[test]
    fn test_default_sync_excludes() {
        let excludes = default_sync_excludes();

        assert!(excludes.contains(&".git/".to_string()));
        assert!(excludes.contains(&".homeboy-build/".to_string()));
        assert!(excludes.contains(&".env".to_string()));
        assert!(!excludes.contains(&"node_modules/".to_string()));
        assert!(!excludes.contains(&"target/".to_string()));
        assert!(!excludes.contains(&"vendor/".to_string()));
        assert!(!excludes.contains(&".sampleplugin/".to_string()));
    }

    #[test]
    fn source_snapshot_policy_allows_product_specific_excludes() {
        let policy = SourceSnapshotPolicy::default().with_sync_excludes([".sampleplugin/"]);
        let snapshot =
            existing_remote_with_policy("lab", "/srv/homeboy/repo", Some("/srv/homeboy"), &policy);

        assert!(snapshot.sync_excludes.contains(&".git/".to_string()));
        assert!(snapshot
            .sync_excludes
            .contains(&".sampleplugin/".to_string()));
    }

    #[test]
    fn gitignore_root_anchored_directory_excludes_remain_root_anchored() {
        let tempdir = tempfile::tempdir().expect("creates source fixture");
        fs::write(tempdir.path().join(".gitignore"), "/dist\ndist\n").expect("writes gitignore");

        assert_eq!(
            gitignore_sync_excludes(tempdir.path()),
            vec!["./dist".to_string(), "dist".to_string()]
        );
    }

    #[test]
    fn test_collect_local() {
        let tempdir = tempfile::tempdir().expect("creates temp source fixture");
        let source_path = tempdir.path();
        fs::write(source_path.join("homeboy.txt"), "source fixture")
            .expect("writes source fixture file");
        git_test_command(source_path, &["init"]);
        git_test_command(source_path, &["add", "."]);
        git_test_command(
            source_path,
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=homeboy@example.test",
                "commit",
                "-m",
                "Initial fixture",
            ],
        );

        let snapshot = collect_local(
            "lab-local",
            source_path,
            Some("/srv/homeboy/repo"),
            PATH_MATERIALIZATION_MODE_SNAPSHOT,
        );

        assert_eq!(snapshot.runner_id, "lab-local");
        assert_eq!(snapshot.remote_path.as_deref(), Some("/srv/homeboy/repo"));
        assert_eq!(snapshot.sync_mode, PATH_MATERIALIZATION_MODE_SNAPSHOT);
        assert_eq!(
            snapshot.local_path.as_deref(),
            Some(source_path.to_str().unwrap())
        );
        let workspace_root = Path::new(
            snapshot
                .workspace_root
                .as_deref()
                .expect("git fixture reports workspace root"),
        );
        assert_eq!(
            fs::canonicalize(workspace_root).expect("canonicalizes git workspace root"),
            fs::canonicalize(source_path).expect("canonicalizes temp source path")
        );
        assert!(snapshot.git_sha.is_some());
        assert!(snapshot.snapshot_hash.starts_with("sha256:"));
    }

    #[test]
    fn collect_local_allows_non_git_paths_without_workspace_root() {
        let tempdir = tempfile::tempdir().expect("creates temp source fixture");
        fs::write(tempdir.path().join("homeboy.txt"), "source fixture")
            .expect("writes source fixture file");

        let snapshot = collect_local(
            "lab-local",
            tempdir.path(),
            Some("/srv/homeboy/repo"),
            PATH_MATERIALIZATION_MODE_SNAPSHOT,
        );

        assert_eq!(
            snapshot.local_path.as_deref(),
            Some(tempdir.path().to_str().unwrap())
        );
        assert_eq!(snapshot.remote_path.as_deref(), Some("/srv/homeboy/repo"));
        assert!(snapshot.workspace_root.is_none());
        assert!(snapshot.git_sha.is_none());
        assert!(!snapshot.dirty);
        assert!(snapshot.snapshot_hash.starts_with("sha256:"));
    }

    #[test]
    fn existing_remote_snapshot_is_explicit() {
        let snapshot = existing_remote("lab", "/srv/homeboy/repo", Some("/srv/homeboy"));

        assert_eq!(snapshot.runner_id, "lab");
        assert_eq!(snapshot.remote_path.as_deref(), Some("/srv/homeboy/repo"));
        assert_eq!(snapshot.workspace_root.as_deref(), Some("/srv/homeboy"));
        assert_eq!(
            snapshot.sync_mode,
            PATH_MATERIALIZATION_MODE_EXISTING_REMOTE
        );
        assert!(!snapshot.dirty);
        assert!(snapshot.snapshot_hash.starts_with("sha256:"));
        assert!(snapshot.sync_excludes.contains(&".git/".to_string()));
    }

    fn git_test_command(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(path)
            .stdin(std::process::Stdio::null())
            .output()
            .expect("runs git fixture command");

        if !output.status.success() {
            std::io::stderr()
                .write_all(&output.stderr)
                .expect("writes git fixture stderr");
            panic!("git fixture command failed: git {}", args.join(" "));
        }
    }
}
