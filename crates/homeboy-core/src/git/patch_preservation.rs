use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};

use homeboy_engine_primitives::content_hash;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

const EVIDENCE_SCHEMA: &str = "homeboy/git-patch-preservation/v1";
const STORE_DIRECTORY: &str = "homeboy/patch-preservations";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PatchPreservationState {
    Captured,
    Cleaned,
    Restored,
    CleanupFailed,
    RestoreFailed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PreservedPatchArtifact {
    pub path: String,
    pub sha256: String,
    pub bytes: u64,
}

/// Durable evidence for one operation-owned working-tree patch.
///
/// The evidence and patch streams live below this worktree's own Git directory,
/// not in repository-global refs. The explicit operation id remains stable
/// across process restarts and never depends on stack position.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PatchPreservationEvidence {
    pub schema: String,
    pub operation_id: String,
    pub worktree_git_dir: String,
    pub captured_head: String,
    pub state: PatchPreservationState,
    pub staged_patch: PreservedPatchArtifact,
    pub unstaged_patch: PreservedPatchArtifact,
    pub untracked_patch: PreservedPatchArtifact,
    pub untracked_paths: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
}

impl PatchPreservationEvidence {
    pub fn evidence_path(&self) -> PathBuf {
        Path::new(&self.worktree_git_dir)
            .join(STORE_DIRECTORY)
            .join(&self.operation_id)
            .join("evidence.json")
    }
}

/// Capture all staged, unstaged, and untracked changes as exact binary patch
/// streams, durably publish their evidence, and leave the worktree clean.
pub fn preserve_worktree_patch(
    worktree: &Path,
    operation_id: &str,
) -> Result<PatchPreservationEvidence> {
    validate_operation_id(operation_id)?;
    let git_dir = worktree_git_dir(worktree)?;
    let operation_dir = git_dir.join(STORE_DIRECTORY).join(operation_id);
    if operation_dir.exists() {
        return Err(Error::validation_invalid_argument(
            "operation_id",
            "patch preservation evidence already exists for this worktree",
            Some(operation_id.to_string()),
            None,
        ));
    }
    fs::create_dir_all(&operation_dir).map_err(io("create patch preservation directory"))?;

    let staged = git_output(
        worktree,
        &[
            "diff",
            "--cached",
            "--binary",
            "--full-index",
            "--no-ext-diff",
        ],
        "capture staged patch",
    )?;
    let unstaged = git_output(
        worktree,
        &["diff", "--binary", "--full-index", "--no-ext-diff"],
        "capture unstaged patch",
    )?;
    let untracked_paths = untracked_paths(worktree)?;
    let untracked = untracked_patch(worktree, &untracked_paths)?;

    let mut evidence = PatchPreservationEvidence {
        schema: EVIDENCE_SCHEMA.to_string(),
        operation_id: operation_id.to_string(),
        worktree_git_dir: git_dir.display().to_string(),
        captured_head: git_text(worktree, &["rev-parse", "HEAD"], "resolve patch base")?,
        state: PatchPreservationState::Captured,
        staged_patch: write_patch(&operation_dir, "staged.patch", &staged)?,
        unstaged_patch: write_patch(&operation_dir, "unstaged.patch", &unstaged)?,
        untracked_patch: write_patch(&operation_dir, "untracked.patch", &untracked)?,
        untracked_paths,
        failure: None,
    };
    write_evidence(&evidence)?;

    if let Err(error) = clean_worktree(worktree, &evidence.untracked_paths) {
        evidence.state = PatchPreservationState::CleanupFailed;
        evidence.failure = Some(error.to_string());
        write_evidence(&evidence)?;
        return Err(error);
    }
    evidence.state = PatchPreservationState::Cleaned;
    write_evidence(&evidence)?;
    Ok(evidence)
}

/// Restore a patch by its operation id after verifying every persisted byte.
pub fn restore_worktree_patch(
    worktree: &Path,
    operation_id: &str,
) -> Result<PatchPreservationEvidence> {
    validate_operation_id(operation_id)?;
    let git_dir = worktree_git_dir(worktree)?;
    let evidence_path = git_dir
        .join(STORE_DIRECTORY)
        .join(operation_id)
        .join("evidence.json");
    let mut evidence = read_evidence(&evidence_path)?;
    if Path::new(&evidence.worktree_git_dir) != git_dir {
        return Err(Error::validation_invalid_argument(
            "operation_id",
            "patch preservation belongs to a different worktree",
            Some(operation_id.to_string()),
            None,
        ));
    }
    if evidence.state != PatchPreservationState::Cleaned {
        return Err(Error::validation_invalid_argument(
            "operation_id",
            format!(
                "patch preservation is not ready to restore (state: {:?})",
                evidence.state
            ),
            Some(operation_id.to_string()),
            None,
        ));
    }

    let operation_dir = evidence_path.parent().expect("evidence has parent");
    let staged = verified_patch(operation_dir, &evidence.staged_patch)?;
    let unstaged = verified_patch(operation_dir, &evidence.unstaged_patch)?;
    let untracked = verified_patch(operation_dir, &evidence.untracked_patch)?;
    let restore = || -> Result<()> {
        apply_patch(worktree, &staged, true, "restore staged patch")?;
        apply_patch(worktree, &unstaged, false, "restore unstaged patch")?;
        apply_patch(worktree, &untracked, false, "restore untracked patch")?;
        Ok(())
    };
    if let Err(error) = restore() {
        evidence.state = PatchPreservationState::RestoreFailed;
        evidence.failure = Some(error.to_string());
        write_evidence(&evidence)?;
        return Err(error);
    }
    evidence.state = PatchPreservationState::Restored;
    evidence.failure = None;
    write_evidence(&evidence)?;
    Ok(evidence)
}

fn validate_operation_id(operation_id: &str) -> Result<()> {
    let valid = !operation_id.is_empty()
        && operation_id.len() <= 128
        && operation_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
        && operation_id != "."
        && operation_id != "..";
    if valid {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "operation_id",
            "expected 1-128 ASCII letters, digits, dots, dashes, or underscores",
            Some(operation_id.to_string()),
            None,
        ))
    }
}

fn worktree_git_dir(worktree: &Path) -> Result<PathBuf> {
    let path = git_text(
        worktree,
        &["rev-parse", "--absolute-git-dir"],
        "resolve worktree Git directory",
    )?;
    fs::canonicalize(&path).map_err(io("canonicalize worktree Git directory"))
}

fn untracked_paths(worktree: &Path) -> Result<Vec<String>> {
    let bytes = git_output(
        worktree,
        &["ls-files", "--others", "--exclude-standard", "-z"],
        "enumerate untracked paths",
    )?;
    bytes
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
        .map(|path| {
            let path = std::str::from_utf8(path).map_err(|_| {
                Error::validation_invalid_argument(
                    "worktree",
                    "patch preservation requires UTF-8 worktree paths",
                    None,
                    None,
                )
            })?;
            validate_relative_path(path)?;
            Ok(path.to_string())
        })
        .collect()
}

fn untracked_patch(worktree: &Path, paths: &[String]) -> Result<Vec<u8>> {
    let mut patch = Vec::new();
    for path in paths {
        let output = Command::new("git")
            .args([
                "diff",
                "--no-index",
                "--binary",
                "--full-index",
                "--no-ext-diff",
                "--",
                "/dev/null",
                path,
            ])
            .current_dir(worktree)
            .output()
            .map_err(|error| Error::git_command_failed(error.to_string()))?;
        if !matches!(output.status.code(), Some(0 | 1)) {
            return Err(git_failure("capture untracked patch", &output.stderr));
        }
        patch.extend_from_slice(&output.stdout);
    }
    Ok(patch)
}

fn clean_worktree(worktree: &Path, untracked_paths: &[String]) -> Result<()> {
    git_output(
        worktree,
        &["reset", "--hard", "HEAD"],
        "clean tracked patch",
    )?;
    for relative in untracked_paths {
        let path = worktree.join(relative);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_dir() => {
                fs::remove_dir_all(&path).map_err(io("remove preserved untracked directory"))?
            }
            Ok(_) => fs::remove_file(&path).map_err(io("remove preserved untracked file"))?,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(io("inspect preserved untracked path")(error)),
        }
        remove_empty_parents(path.parent(), worktree)?;
    }
    Ok(())
}

fn remove_empty_parents(mut path: Option<&Path>, worktree: &Path) -> Result<()> {
    while let Some(parent) = path {
        if parent == worktree || !parent.starts_with(worktree) {
            break;
        }
        match fs::remove_dir(parent) {
            Ok(()) => path = parent.parent(),
            Err(error) if error.kind() == std::io::ErrorKind::DirectoryNotEmpty => break,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => path = parent.parent(),
            Err(error) => return Err(io("remove empty untracked parent")(error)),
        }
    }
    Ok(())
}

fn write_patch(directory: &Path, name: &str, bytes: &[u8]) -> Result<PreservedPatchArtifact> {
    let path = directory.join(name);
    let mut file = File::create(&path).map_err(io("create preserved patch"))?;
    file.write_all(bytes).map_err(io("write preserved patch"))?;
    file.sync_all().map_err(io("sync preserved patch"))?;
    Ok(PreservedPatchArtifact {
        path: name.to_string(),
        sha256: content_hash::sha256_hex(bytes),
        bytes: bytes.len() as u64,
    })
}

fn verified_patch(directory: &Path, artifact: &PreservedPatchArtifact) -> Result<Vec<u8>> {
    validate_relative_path(&artifact.path)?;
    let bytes = fs::read(directory.join(&artifact.path)).map_err(io("read preserved patch"))?;
    if bytes.len() as u64 != artifact.bytes || content_hash::sha256_hex(&bytes) != artifact.sha256 {
        return Err(Error::validation_invalid_argument(
            "patch",
            format!(
                "preserved patch {} failed byte identity verification",
                artifact.path
            ),
            None,
            None,
        ));
    }
    Ok(bytes)
}

fn write_evidence(evidence: &PatchPreservationEvidence) -> Result<()> {
    let path = evidence.evidence_path();
    let temporary = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(evidence)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    let mut file = File::create(&temporary).map_err(io("create patch evidence"))?;
    file.write_all(&bytes).map_err(io("write patch evidence"))?;
    file.write_all(b"\n").map_err(io("write patch evidence"))?;
    file.sync_all().map_err(io("sync patch evidence"))?;
    fs::rename(&temporary, &path).map_err(io("publish patch evidence"))?;
    File::open(path.parent().expect("evidence has parent"))
        .and_then(|directory| directory.sync_all())
        .map_err(io("sync patch evidence directory"))
}

fn read_evidence(path: &Path) -> Result<PatchPreservationEvidence> {
    let bytes = fs::read(path).map_err(io("read patch evidence"))?;
    let evidence: PatchPreservationEvidence = serde_json::from_slice(&bytes)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    if evidence.schema != EVIDENCE_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "patch",
            "unsupported patch preservation evidence schema",
            Some(evidence.schema),
            None,
        ));
    }
    Ok(evidence)
}

fn apply_patch(worktree: &Path, patch: &[u8], index: bool, context: &str) -> Result<()> {
    if patch.is_empty() {
        return Ok(());
    }
    let mut command = Command::new("git");
    command.args(["apply", "--binary"]);
    if index {
        command.arg("--index");
    }
    let mut child = command
        .arg("-")
        .current_dir(worktree)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(patch)
        .map_err(io("write patch to git apply"))?;
    let output = child
        .wait_with_output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(git_failure(context, &output.stderr))
    }
}

fn git_output(worktree: &Path, args: &[&str], context: &str) -> Result<Vec<u8>> {
    let output = Command::new("git")
        .args(args)
        .current_dir(worktree)
        .output()
        .map_err(|error| Error::git_command_failed(error.to_string()))?;
    if output.status.success() {
        Ok(output.stdout)
    } else {
        Err(git_failure(context, &output.stderr))
    }
}

fn git_text(worktree: &Path, args: &[&str], context: &str) -> Result<String> {
    let bytes = git_output(worktree, args, context)?;
    Ok(String::from_utf8_lossy(&bytes).trim().to_string())
}

fn git_failure(context: &str, stderr: &[u8]) -> Error {
    Error::git_command_failed(format!(
        "{context} failed: {}",
        String::from_utf8_lossy(stderr).trim()
    ))
}

fn validate_relative_path(path: &str) -> Result<()> {
    let path = Path::new(path);
    if !path.is_absolute()
        && path
            .components()
            .all(|component| matches!(component, Component::Normal(_)))
    {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "patch",
            "preserved patch contains an unsafe relative path",
            Some(path.display().to_string()),
            None,
        ))
    }
}

fn io(context: &'static str) -> impl FnOnce(std::io::Error) -> Error {
    move |error| Error::internal_io(error.to_string(), Some(context.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;

    #[test]
    fn sibling_worktrees_preserve_and_restore_only_their_operation_patch() {
        let repository = tempfile::tempdir().expect("repository");
        let primary = repository.path().join("primary");
        run(
            None,
            &["init", "-q", "-b", "main", primary.to_str().unwrap()],
        );
        run(Some(&primary), &["config", "user.name", "Test"]);
        run(
            Some(&primary),
            &["config", "user.email", "test@example.test"],
        );
        fs::write(primary.join("base.txt"), b"base\n").unwrap();
        fs::write(primary.join("alpha.txt"), b"alpha base\n").unwrap();
        fs::write(primary.join("beta.txt"), b"beta base\n").unwrap();
        run(Some(&primary), &["add", "."]);
        run(Some(&primary), &["commit", "-qm", "base"]);

        let alpha = repository.path().join("alpha");
        let beta = repository.path().join("beta");
        run(
            Some(&primary),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "alpha",
                alpha.to_str().unwrap(),
            ],
        );
        run(
            Some(&primary),
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "beta",
                beta.to_str().unwrap(),
            ],
        );

        fs::write(alpha.join("alpha.txt"), b"alpha staged\n").unwrap();
        run(Some(&alpha), &["add", "alpha.txt"]);
        fs::write(alpha.join("alpha.txt"), b"alpha staged\nalpha unstaged\n").unwrap();
        fs::write(alpha.join("alpha.bin"), [0, 1, 2, 255]).unwrap();
        fs::write(beta.join("beta.txt"), b"beta staged\n").unwrap();
        run(Some(&beta), &["add", "beta.txt"]);
        fs::write(beta.join("beta.txt"), b"beta staged\nbeta unstaged\n").unwrap();
        fs::write(beta.join("beta.bin"), [255, 2, 1, 0]).unwrap();

        let barrier = Arc::new(Barrier::new(3));
        let alpha_thread = preserve_after_barrier(alpha.clone(), Arc::clone(&barrier));
        let beta_thread = preserve_after_barrier(beta.clone(), Arc::clone(&barrier));
        barrier.wait();
        let alpha_evidence = alpha_thread.join().unwrap();
        let beta_evidence = beta_thread.join().unwrap();

        assert_eq!(alpha_evidence.state, PatchPreservationState::Cleaned);
        assert_eq!(beta_evidence.state, PatchPreservationState::Cleaned);
        assert_ne!(
            alpha_evidence.worktree_git_dir,
            beta_evidence.worktree_git_dir
        );
        assert!(!alpha.join("alpha.bin").exists());
        assert!(!beta.join("beta.bin").exists());

        fs::write(primary.join("base.txt"), b"new base\n").unwrap();
        run(Some(&primary), &["add", "base.txt"]);
        run(Some(&primary), &["commit", "-qm", "advance base"]);
        run(Some(&alpha), &["rebase", "main"]);
        run(Some(&beta), &["rebase", "main"]);

        let beta_restored = restore_worktree_patch(&beta, "same-operation").unwrap();
        let alpha_restored = restore_worktree_patch(&alpha, "same-operation").unwrap();
        assert_eq!(beta_restored.state, PatchPreservationState::Restored);
        assert_eq!(alpha_restored.state, PatchPreservationState::Restored);
        assert_eq!(
            fs::read(alpha.join("alpha.txt")).unwrap(),
            b"alpha staged\nalpha unstaged\n"
        );
        assert_eq!(fs::read(alpha.join("alpha.bin")).unwrap(), [0, 1, 2, 255]);
        assert_eq!(
            fs::read(beta.join("beta.txt")).unwrap(),
            b"beta staged\nbeta unstaged\n"
        );
        assert_eq!(fs::read(beta.join("beta.bin")).unwrap(), [255, 2, 1, 0]);
        assert_eq!(fs::read(alpha.join("beta.txt")).unwrap(), b"beta base\n");
        assert_eq!(fs::read(beta.join("alpha.txt")).unwrap(), b"alpha base\n");
        assert!(git(&primary, &["rev-parse", "--verify", "refs/stash"]).is_err());

        let alpha_status = git(&alpha, &["status", "--porcelain=v1"]).unwrap();
        let beta_status = git(&beta, &["status", "--porcelain=v1"]).unwrap();
        assert!(alpha_status.contains("alpha.txt"));
        assert!(alpha_status.contains("alpha.bin"));
        assert!(!alpha_status.contains("beta.txt"));
        assert!(beta_status.contains("beta.txt"));
        assert!(beta_status.contains("beta.bin"));
        assert!(!beta_status.contains("alpha.txt"));
    }

    fn preserve_after_barrier(
        worktree: PathBuf,
        barrier: Arc<Barrier>,
    ) -> thread::JoinHandle<PatchPreservationEvidence> {
        thread::spawn(move || {
            barrier.wait();
            preserve_worktree_patch(&worktree, "same-operation").unwrap()
        })
    }

    fn run(worktree: Option<&Path>, args: &[&str]) {
        let mut command = Command::new("git");
        command.args(args);
        if let Some(worktree) = worktree {
            command.current_dir(worktree);
        }
        let output = command.output().unwrap();
        assert!(
            output.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn git(worktree: &Path, args: &[&str]) -> std::result::Result<String, String> {
        let output = Command::new("git")
            .args(args)
            .current_dir(worktree)
            .output()
            .unwrap();
        if output.status.success() {
            Ok(String::from_utf8_lossy(&output.stdout).to_string())
        } else {
            Err(String::from_utf8_lossy(&output.stderr).to_string())
        }
    }
}
