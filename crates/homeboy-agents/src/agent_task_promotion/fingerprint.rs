use homeboy_core::error::{Error, Result};
use std::path::Path;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct AgentTaskCandidateFingerprint {
    pub schema: String,
    pub target_path: String,
    pub head: String,
    pub changed_files: Vec<String>,
    /// Full workspace content, without changing the real index.
    pub tree: String,
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

/// Identify the Git workspace by HEAD and its final content tree.
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

    let git = |args: &[&str]| run_git(path, args);
    let head = text(git(&["rev-parse", "HEAD"])?).trim().to_string();
    reject_gitlinks(&git)?;
    let untracked = paths(git(&["ls-files", "--others", "--exclude-standard", "-z"])?);
    let mut changed_files = changed_paths(&git, &untracked)?;
    for relative in &untracked {
        validate_untracked(path, relative)?;
    }
    let tree = candidate_tree(path)?;
    changed_files.sort();
    changed_files.dedup();
    Ok(AgentTaskPromotionCandidate::Git {
        fingerprint: AgentTaskCandidateFingerprint {
            schema: "homeboy/agent-task-candidate-fingerprint/v2".to_string(),
            target_path: std::fs::canonicalize(root)
                .map_err(|error| Error::internal_io(error.to_string(), Some(path.to_string())))?
                .display()
                .to_string(),
            head,
            changed_files,
            tree,
        },
    })
}

/// The candidate tree overlays the working tree on the real index's content. It
/// authenticates a candidate across time: a promotion records it, and
/// moving-base recovery re-derives it to prove the destination still holds the
/// exact promoted content.
///
/// The scratch index is seeded from the real index's tree, not from `HEAD`, so
/// force-added ignored paths absent from `HEAD` stay in the tree. That tree is
/// written from a copy of the real index: `git write-tree` rewrites the index
/// it reads and needs its lock, and a fingerprint must neither mutate the
/// workspace nor fail while another Git process holds `index.lock`.
///
/// Only the tree crosses into the scratch index, never the copied stat cache.
/// `git add` skips any file whose stat still matches a cached entry, so a file
/// modified without a stat change would keep its stale blob and the tree would
/// describe content that is not on disk. `read-tree` writes entries with no
/// stat data, so every working-tree file is re-hashed.
fn candidate_tree(path: &str) -> Result<String> {
    let scratch = |context: &str| {
        tempfile::NamedTempFile::new()
            .map_err(|error| Error::internal_io(error.to_string(), Some(context.to_string())))
    };
    let real_index = text(run_git(
        path,
        &["rev-parse", "--path-format=absolute", "--git-path", "index"],
    )?);
    let real_index = Path::new(real_index.trim());
    let candidate = scratch(path)?;
    let git = |index: &tempfile::NamedTempFile, args: &[&str], context: &str| {
        homeboy_core::git::run_git_bytes_with_env(
            Path::new(path),
            args,
            context,
            &[(
                "GIT_INDEX_FILE".to_string(),
                index.path().display().to_string(),
            )],
        )
    };
    // A checkout without an index (for example `worktree add --no-checkout`)
    // has staged nothing beyond `HEAD`.
    let staged_tree = if real_index.exists() {
        let staged = scratch(path)?;
        std::fs::copy(real_index, staged.path()).map_err(|error| {
            Error::internal_io(error.to_string(), Some(real_index.display().to_string()))
        })?;
        text(git(&staged, &["write-tree"], "write real index tree")?)
    } else {
        "HEAD".to_string()
    };
    git(
        &candidate,
        &["read-tree", staged_tree.trim()],
        "seed candidate tree index from real index tree",
    )?;
    git(&candidate, &["add", "-A"], "stage candidate tree")?;
    Ok(
        text(git(&candidate, &["write-tree"], "write candidate tree")?)
            .trim()
            .to_string(),
    )
}

fn is_git_worktree(path: &str) -> Result<bool> {
    let args = ["rev-parse", "--is-inside-work-tree"];
    let context = "check candidate is a Git worktree";
    let output = homeboy_core::git::run_git_output(Path::new(path), &args, context)?;
    if output.status.success() {
        return Ok(text(output.stdout).trim() == "true");
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    // A .git entry means this was intended as Git. Its failure is malformed or
    // inaccessible state, not a portable non-Git provider target.
    if stderr.contains("not a git repository") && !Path::new(path).join(".git").exists() {
        return Ok(false);
    }
    // Re-run through the checked path so the failure carries its full evidence.
    homeboy_core::git::run_git_bytes_with_env(Path::new(path), &args, context, &[]).map(|_| false)
}

fn reject_gitlinks(git: &impl Fn(&[&str]) -> Result<Vec<u8>>) -> Result<()> {
    for args in [
        ["diff", "--raw", "-z", "--cached"].as_slice(),
        ["diff", "--raw", "-z"].as_slice(),
    ] {
        if text(git(args)?)
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

fn validate_untracked(root: &str, relative: &str) -> Result<()> {
    let file = Path::new(root).join(relative);
    let metadata = std::fs::symlink_metadata(&file)
        .map_err(|error| Error::internal_io(error.to_string(), Some(file.display().to_string())))?;
    let kind = metadata.file_type();
    if kind.is_symlink() || !kind.is_file() {
        return Err(Error::validation_invalid_argument("candidate", "untracked candidate entries must be regular files; symlinks, directories, and special files are unsupported", Some(relative.to_string()), None));
    }
    Ok(())
}

fn text(bytes: Vec<u8>) -> String {
    String::from_utf8_lossy(&bytes).to_string()
}
fn run_git(path: &str, args: &[&str]) -> Result<Vec<u8>> {
    homeboy_core::git::run_git_bytes_with_env(
        Path::new(path),
        args,
        "fingerprint Cook candidate",
        &[],
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::Command;
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
    fn a_silent_git_failure_still_names_its_command_and_exit() {
        // `git diff --quiet` exits 1 on a dirty tree with no output at all, the
        // shape that used to surface as an empty pre-execution failure (#15226).
        let dir = repo();
        commit(&dir, "a");
        std::fs::write(dir.path().join("a"), "two").unwrap();

        let error = run_git(dir.path().to_str().unwrap(), &["diff", "--quiet"])
            .expect_err("dirty tree makes diff --quiet fail");

        assert!(!error.message.trim().is_empty());
        assert_eq!(error.details["command"], "git diff --quiet");
        assert_eq!(error.details["exit_code"], 1);
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
    fn untracked_content_changes_tree() {
        let dir = repo();
        commit(&dir, "base");
        std::fs::write(dir.path().join("a"), "one").unwrap();
        let first = fingerprint(&dir);
        std::fs::write(dir.path().join("a"), "two").unwrap();
        assert_ne!(first.tree, fingerprint(&dir).tree);
    }

    #[test]
    fn fingerprint_leaves_a_locked_real_index_untouched() {
        let dir = repo();
        commit(&dir, "base");
        std::fs::write(dir.path().join("base"), "edited").unwrap();
        let index = dir.path().join(".git/index");
        let before = std::fs::read(&index).unwrap();
        std::fs::write(dir.path().join(".git/index.lock"), "").unwrap();
        let locked = fingerprint(&dir);
        assert_eq!(before, std::fs::read(&index).unwrap());
        std::fs::remove_file(dir.path().join(".git/index.lock")).unwrap();
        assert_eq!(locked, fingerprint(&dir));
    }

    #[test]
    fn added_content_changes_tree() {
        for (name, ignored, staged) in [
            ("staged", false, true),
            ("untracked", false, false),
            ("ignored", true, true),
        ] {
            let dir = repo();
            commit(&dir, "base");
            if ignored {
                std::fs::write(dir.path().join(".git/info/exclude"), name).unwrap();
            }
            let clean = fingerprint(&dir);
            std::fs::write(dir.path().join(name), "one").unwrap();
            if staged {
                assert!(Command::new("git")
                    .args(["add", "-f", name])
                    .current_dir(dir.path())
                    .status()
                    .unwrap()
                    .success());
            }
            let added = fingerprint(&dir);
            assert_ne!(clean.tree, added.tree, "{name}");
            std::fs::write(dir.path().join(name), "two").unwrap();
            assert_ne!(added.tree, fingerprint(&dir).tree, "{name} edited");
        }
    }

    #[test]
    fn staged_then_unstaged_edits_change_tree() {
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
        assert_ne!(staged.tree, fingerprint(&dir).tree);
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
        assert_ne!(first.tree, fingerprint(&dir).tree);
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
        assert_ne!(clean.tree, fingerprint(&dir).tree);
    }

    #[test]
    fn rename_changes_paths_and_fingerprint() {
        let dir = repo();
        commit(&dir, "old");
        let clean = fingerprint(&dir);
        std::fs::rename(dir.path().join("old"), dir.path().join("new")).unwrap();
        let renamed = fingerprint(&dir);
        assert_ne!(clean.tree, renamed.tree);
        assert_eq!(renamed.changed_files, vec!["new", "old"]);
    }

    #[test]
    fn malformed_git_repository_fails_closed() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(".git"), "not a gitdir").unwrap();
        assert!(candidate_fingerprint(dir.path().to_str().unwrap()).is_err());
    }
}
