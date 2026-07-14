use sha2::{Digest, Sha256};
use std::path::Path;
use std::process::Command;

use crate::core::error::{Error, Result};

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskCandidateFingerprint {
    pub schema: String,
    pub target_path: String,
    pub head: String,
    pub base: String,
    pub changed_files: Vec<String>,
    pub sha256: String,
}

/// A provider can promote to a non-Git workspace. That capability is valid for
/// promotion, but deliberately cannot authorize GitHub PR finalization.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentTaskPromotionCandidate {
    Git {
        fingerprint: AgentTaskCandidateFingerprint,
    },
    NonGit {
        disposition: String,
    },
}

/// Hash the exact Git candidate state. Index and worktree diffs remain separate
/// so staged and unstaged overlap cannot collapse into one candidate state.
pub fn candidate_fingerprint(path: &str) -> Result<AgentTaskPromotionCandidate> {
    let root = Path::new(path);
    let metadata = match std::fs::metadata(root) {
        Ok(metadata) => metadata,
        // External providers may return an opaque target path that is not
        // materialized on this host. It remains promotable, but is never PR
        // finalizable because it has no Git candidate proof.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(AgentTaskPromotionCandidate::NonGit {
                disposition: "target_not_materialized".to_string(),
            });
        }
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some(path.to_string()),
            ));
        }
    };
    if !metadata.is_dir() {
        return Err(Error::validation_invalid_argument(
            "candidate",
            "promotion target must be a directory",
            Some(path.to_string()),
            None,
        ));
    }
    if !is_git_worktree(path)? {
        return Ok(AgentTaskPromotionCandidate::NonGit {
            disposition: "not_a_git_worktree".to_string(),
        });
    }

    let git = |args: &[&str]| git_checked(path, args);
    let head = text(git(&["rev-parse", "HEAD"])?).trim().to_string();
    let parents = text(git(&["rev-list", "--parents", "-n", "1", "HEAD"])?);
    let parent_ids = parents.split_whitespace().collect::<Vec<_>>();
    if parent_ids.first() != Some(&head.as_str()) {
        return Err(Error::git_command_failed(
            "git rev-list returned a HEAD different from git rev-parse".to_string(),
        ));
    }
    // A root commit has no parent; unlike `HEAD^` failure, this is unambiguous.
    let base = parent_ids.get(1).copied().unwrap_or(&head).to_string();
    reject_gitlinks(&git)?;
    let staged = git(&["diff", "--binary", "--cached"])?;
    let unstaged = git(&["diff", "--binary"])?;
    let untracked = paths(git(&["ls-files", "--others", "--exclude-standard", "-z"])?);
    let mut changed_files = changed_paths(&git, &untracked)?;
    let mut hasher = Sha256::new();
    hash_record(&mut hasher, b"staged", &staged);
    hash_record(&mut hasher, b"unstaged", &unstaged);
    for relative in &untracked {
        hash_untracked(&mut hasher, path, relative)?;
    }
    changed_files.sort();
    changed_files.dedup();
    Ok(AgentTaskPromotionCandidate::Git {
        fingerprint: AgentTaskCandidateFingerprint {
            schema: "homeboy/agent-task-candidate-fingerprint/v1".to_string(),
            target_path: std::fs::canonicalize(root)
                .map_err(|error| Error::internal_io(error.to_string(), Some(path.to_string())))?
                .display()
                .to_string(),
            head,
            base,
            changed_files,
            sha256: format!("{:x}", hasher.finalize()),
        },
    })
}

fn is_git_worktree(path: &str) -> Result<bool> {
    let output = git_output(path, &["rev-parse", "--is-inside-work-tree"])?;
    if output.status.success() {
        return Ok(text(output.stdout).trim() == "true");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A .git entry means this was intended as Git. Its failure is malformed or
    // inaccessible state, not a portable non-Git provider target.
    if stderr.contains("not a git repository") && !Path::new(path).join(".git").exists() {
        return Ok(false);
    }
    Err(Error::git_command_failed(stderr.to_string()))
}

fn reject_gitlinks(git: &impl Fn(&[&str]) -> Result<Vec<u8>>) -> Result<()> {
    for args in [
        ["diff", "--raw", "-z", "--cached"].as_slice(),
        ["diff", "--raw", "-z"].as_slice(),
    ] {
        if text(git(&args)?)
            .split('\0')
            .any(|entry| entry.starts_with(":160000 ") || entry.get(8..15) == Some("160000 "))
        {
            return Err(Error::validation_invalid_argument(
                "candidate",
                "gitlink/submodule changes are unsupported for candidate finalization",
                None,
                None,
            ));
        }
    }
    Ok(())
}

fn changed_paths(
    git: &impl Fn(&[&str]) -> Result<Vec<u8>>,
    untracked: &[String],
) -> Result<Vec<String>> {
    let mut changed = paths(git(&[
        "diff",
        "--name-only",
        "--no-renames",
        "-z",
        "--cached",
    ])?);
    changed.extend(paths(git(&["diff", "--name-only", "--no-renames", "-z"])?));
    changed.extend(untracked.iter().cloned());
    Ok(changed)
}

fn paths(bytes: Vec<u8>) -> Vec<String> {
    bytes
        .split(|byte| *byte == b'\0')
        .filter(|entry| !entry.is_empty())
        .map(|entry| String::from_utf8_lossy(entry).to_string())
        .collect()
}

fn hash_untracked(hasher: &mut Sha256, root: &str, relative: &str) -> Result<()> {
    let file = Path::new(root).join(relative);
    let metadata = std::fs::symlink_metadata(&file)
        .map_err(|error| Error::internal_io(error.to_string(), Some(file.display().to_string())))?;
    let kind = metadata.file_type();
    if kind.is_symlink() || !kind.is_file() {
        return Err(Error::validation_invalid_argument("candidate", "untracked candidate entries must be regular files; symlinks, directories, and special files are unsupported", Some(relative.to_string()), None));
    }
    hash_record(hasher, b"untracked-path", relative.as_bytes());
    #[cfg(unix)]
    hash_record(
        hasher,
        b"untracked-mode",
        format!(
            "{:o}",
            std::os::unix::fs::MetadataExt::mode(&metadata) & 0o111
        )
        .as_bytes(),
    );
    #[cfg(not(unix))]
    hash_record(hasher, b"untracked-mode", b"0");
    hash_record(
        hasher,
        b"untracked-bytes",
        &std::fs::read(&file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(file.display().to_string()))
        })?,
    );
    Ok(())
}

fn hash_record(hasher: &mut Sha256, kind: &[u8], bytes: &[u8]) {
    hasher.update((kind.len() as u64).to_be_bytes());
    hasher.update(kind);
    hasher.update((bytes.len() as u64).to_be_bytes());
    hasher.update(bytes);
}
fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).to_string()
}
fn git_checked(path: &str, args: &[&str]) -> Result<Vec<u8>> {
    let output = git_output(path, args)?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(Error::git_command_failed(
            String::from_utf8_lossy(&output.stderr).to_string(),
        ))
    }
}

fn git_output(path: &str, args: &[&str]) -> Result<std::process::Output> {
    Command::new("git")
        .args(args)
        .current_dir(path)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn repo() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for args in [
            ["init"].as_slice(),
            ["config", "user.email", "test@example.com"].as_slice(),
            ["config", "user.name", "Test"].as_slice(),
        ] {
            Command::new("git")
                .args(args)
                .current_dir(dir.path())
                .status()
                .unwrap();
        }
        dir
    }
    fn commit(dir: &tempfile::TempDir, name: &str) {
        std::fs::write(dir.path().join(name), "one").unwrap();
        Command::new("git")
            .args(["add", name])
            .current_dir(dir.path())
            .status()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", name])
            .current_dir(dir.path())
            .status()
            .unwrap();
    }
    fn fingerprint(dir: &tempfile::TempDir) -> AgentTaskCandidateFingerprint {
        let AgentTaskPromotionCandidate::Git { fingerprint } =
            candidate_fingerprint(dir.path().to_str().unwrap()).unwrap()
        else {
            panic!("git candidate")
        };
        fingerprint
    }
    #[test]
    fn root_commit_and_repeated_state_are_stable() {
        let dir = repo();
        commit(&dir, "a");
        let first = candidate_fingerprint(dir.path().to_str().unwrap()).unwrap();
        assert_eq!(
            first,
            candidate_fingerprint(dir.path().to_str().unwrap()).unwrap()
        );
        let AgentTaskPromotionCandidate::Git { fingerprint } = first else {
            panic!("git")
        };
        assert_eq!(fingerprint.head, fingerprint.base);
    }
    #[test]
    fn non_git_target_is_an_explicit_capability() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            candidate_fingerprint(dir.path().to_str().unwrap()).unwrap(),
            AgentTaskPromotionCandidate::NonGit {
                disposition: "not_a_git_worktree".to_string()
            }
        );
    }
    #[test]
    fn untracked_content_and_mode_change_fingerprint() {
        let dir = repo();
        commit(&dir, "base");
        std::fs::write(dir.path().join("a"), "one").unwrap();
        let first = candidate_fingerprint(dir.path().to_str().unwrap()).unwrap();
        std::fs::write(dir.path().join("a"), "two").unwrap();
        assert_ne!(
            first,
            candidate_fingerprint(dir.path().to_str().unwrap()).unwrap()
        );
    }

    #[test]
    fn staged_then_unstaged_edits_change_fingerprint() {
        let dir = repo();
        commit(&dir, "tracked");
        std::fs::write(dir.path().join("tracked"), "staged").unwrap();
        Command::new("git")
            .args(["add", "tracked"])
            .current_dir(dir.path())
            .status()
            .unwrap();
        let staged = fingerprint(&dir);
        std::fs::write(dir.path().join("tracked"), "unstaged").unwrap();
        assert_ne!(staged.sha256, fingerprint(&dir).sha256);
    }

    #[cfg(unix)]
    #[test]
    fn untracked_executable_mode_changes_fingerprint() {
        use std::os::unix::fs::PermissionsExt;
        let dir = repo();
        commit(&dir, "base");
        let path = dir.path().join("script");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        let first = fingerprint(&dir);
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
        assert_ne!(first.sha256, fingerprint(&dir).sha256);
    }

    #[cfg(unix)]
    #[test]
    fn untracked_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let dir = repo();
        commit(&dir, "base");
        symlink("base", dir.path().join("link")).unwrap();
        assert!(candidate_fingerprint(dir.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn nested_untracked_directory_is_enumerated_to_regular_files() {
        let dir = repo();
        commit(&dir, "base");
        std::fs::create_dir_all(dir.path().join("nested/deeper")).unwrap();
        std::fs::write(dir.path().join("nested/deeper/file"), "new").unwrap();
        assert_eq!(fingerprint(&dir).changed_files, vec!["nested/deeper/file"]);
    }

    #[test]
    fn tracked_gitlink_is_rejected() {
        let dir = repo();
        commit(&dir, "base");
        let hash = Command::new("git")
            .args(["hash-object", "-w", "--stdin"])
            .current_dir(dir.path())
            .stdin(std::process::Stdio::piped())
            .output()
            .unwrap();
        // `hash-object` without input deterministically creates the empty blob.
        let hash = String::from_utf8(hash.stdout).unwrap();
        Command::new("git")
            .args([
                "update-index",
                "--add",
                "--cacheinfo",
                &format!("160000,{},module", hash.trim()),
            ])
            .current_dir(dir.path())
            .status()
            .unwrap();
        assert!(candidate_fingerprint(dir.path().to_str().unwrap()).is_err());
    }

    #[test]
    fn deletion_changes_fingerprint() {
        let dir = repo();
        commit(&dir, "base");
        let clean = fingerprint(&dir);
        std::fs::remove_file(dir.path().join("base")).unwrap();
        assert_ne!(clean.sha256, fingerprint(&dir).sha256);
    }

    #[test]
    fn rename_changes_paths_and_fingerprint() {
        let dir = repo();
        commit(&dir, "old");
        let clean = fingerprint(&dir);
        std::fs::rename(dir.path().join("old"), dir.path().join("new")).unwrap();
        let renamed = fingerprint(&dir);
        assert_ne!(clean.sha256, renamed.sha256);
        assert_eq!(renamed.changed_files, vec!["new", "old"]);
    }

    #[test]
    fn malformed_git_repository_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".git"), "not a gitdir").unwrap();
        assert!(candidate_fingerprint(dir.path().to_str().unwrap()).is_err());
    }
}
