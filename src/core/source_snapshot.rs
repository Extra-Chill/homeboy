use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::core::git;

const SOURCE_SYNC_EXCLUDES_ENV: &str = "HOMEBOY_SOURCE_SYNC_EXCLUDES";

const DEFAULT_SYNC_EXCLUDES: &[&str] = &[
    ".git/",
    "node_modules/",
    "target/",
    "vendor/",
    ".homeboy-build/",
    ".homeboy-bin/",
    ".homeboy/",
    ".DS_Store",
    "._*",
    "**/._*",
    ".env",
    ".env.*",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshotPolicy {
    pub sync_excludes: Vec<String>,
}

impl SourceSnapshotPolicy {
    pub fn from_env() -> Self {
        let mut policy = Self::default();
        policy
            .sync_excludes
            .extend(split_env_list(SOURCE_SYNC_EXCLUDES_ENV));
        policy
    }

    pub fn with_sync_excludes<I, S>(mut self, excludes: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.sync_excludes
            .extend(excludes.into_iter().map(Into::into));
        self
    }
}

impl Default for SourceSnapshotPolicy {
    fn default() -> Self {
        Self {
            sync_excludes: default_sync_excludes(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SourceSnapshot {
    pub runner_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub local_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remote_path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub workspace_root: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    pub dirty: bool,
    pub sync_mode: String,
    pub snapshot_hash: String,
    pub synced_at: String,
    pub sync_excludes: Vec<String>,
}

impl SourceSnapshot {
    pub fn collect_local(
        runner_id: &str,
        path: &Path,
        remote_path: Option<&str>,
        sync_mode: &str,
    ) -> Self {
        let policy = SourceSnapshotPolicy::from_env();
        Self::collect_local_with_policy(runner_id, path, remote_path, sync_mode, &policy)
    }

    pub(crate) fn collect_local_with_policy(
        runner_id: &str,
        path: &Path,
        remote_path: Option<&str>,
        sync_mode: &str,
        policy: &SourceSnapshotPolicy,
    ) -> Self {
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

        Self {
            runner_id: runner_id.to_string(),
            local_path: Some(local_path),
            remote_path: remote_path.map(str::to_string),
            workspace_root: git_root,
            git_branch,
            git_sha,
            dirty,
            sync_mode: sync_mode.to_string(),
            snapshot_hash,
            synced_at: chrono::Utc::now().to_rfc3339(),
            sync_excludes: policy.sync_excludes.clone(),
        }
    }

    pub fn existing_remote(
        runner_id: &str,
        remote_path: &str,
        workspace_root: Option<&str>,
    ) -> Self {
        let policy = SourceSnapshotPolicy::from_env();
        Self::existing_remote_with_policy(runner_id, remote_path, workspace_root, &policy)
    }

    pub(crate) fn existing_remote_with_policy(
        runner_id: &str,
        remote_path: &str,
        workspace_root: Option<&str>,
        policy: &SourceSnapshotPolicy,
    ) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(b"existing_remote\0");
        hasher.update(runner_id.as_bytes());
        hasher.update(b"\0");
        hasher.update(remote_path.as_bytes());
        if let Some(workspace_root) = workspace_root {
            hasher.update(b"\0");
            hasher.update(workspace_root.as_bytes());
        }

        Self {
            runner_id: runner_id.to_string(),
            local_path: None,
            remote_path: Some(remote_path.to_string()),
            workspace_root: workspace_root.map(str::to_string),
            git_branch: None,
            git_sha: None,
            dirty: false,
            sync_mode: "existing_remote".to_string(),
            snapshot_hash: format!("sha256:{:x}", hasher.finalize()),
            synced_at: chrono::Utc::now().to_rfc3339(),
            sync_excludes: policy.sync_excludes.clone(),
        }
    }
}

pub(crate) fn default_sync_excludes() -> Vec<String> {
    DEFAULT_SYNC_EXCLUDES
        .iter()
        .map(|value| value.to_string())
        .collect()
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
        assert!(excludes.contains(&"node_modules/".to_string()));
        assert!(excludes.contains(&".homeboy-build/".to_string()));
        assert!(excludes.contains(&".env".to_string()));
        assert!(!excludes.contains(&".sampleplugin/".to_string()));
    }

    #[test]
    fn source_snapshot_policy_allows_product_specific_excludes() {
        let policy = SourceSnapshotPolicy::default().with_sync_excludes([".sampleplugin/"]);
        let snapshot = SourceSnapshot::existing_remote_with_policy(
            "lab",
            "/srv/homeboy/repo",
            Some("/srv/homeboy"),
            &policy,
        );

        assert!(snapshot.sync_excludes.contains(&".git/".to_string()));
        assert!(snapshot
            .sync_excludes
            .contains(&".sampleplugin/".to_string()));
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

        let snapshot = SourceSnapshot::collect_local(
            "lab-local",
            source_path,
            Some("/srv/homeboy/repo"),
            "snapshot",
        );

        assert_eq!(snapshot.runner_id, "lab-local");
        assert_eq!(snapshot.remote_path.as_deref(), Some("/srv/homeboy/repo"));
        assert_eq!(snapshot.sync_mode, "snapshot");
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

        let snapshot = SourceSnapshot::collect_local(
            "lab-local",
            tempdir.path(),
            Some("/srv/homeboy/repo"),
            "snapshot",
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
        let snapshot =
            SourceSnapshot::existing_remote("lab", "/srv/homeboy/repo", Some("/srv/homeboy"));

        assert_eq!(snapshot.runner_id, "lab");
        assert_eq!(snapshot.remote_path.as_deref(), Some("/srv/homeboy/repo"));
        assert_eq!(snapshot.workspace_root.as_deref(), Some("/srv/homeboy"));
        assert_eq!(snapshot.sync_mode, "existing_remote");
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
