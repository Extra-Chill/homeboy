use std::path::{Path, PathBuf};
use std::process::Command;

use crate::core::observation::{LAB_OFFLOAD_METADATA_ENV, SOURCE_SNAPSHOT_METADATA_ENV};
use crate::core::source_snapshot::SourceSnapshot;

use super::workspace_content_hash;

const LAB_SOURCE_SNAPSHOT_SYNC_MODE: &str = "lab_offload";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct VerifiedLabWorkspaceProvenance {
    pub source_revision: String,
    pub materialization_mode: String,
    pub runner_id: String,
    pub workspace_identity: String,
    pub snapshot_hash: String,
}

/// Creates a deterministic Git root for a verified Lab snapshot so generic
/// candidate harvesting can use the same commit and diff paths as Git syncs.
pub(crate) fn materialize_verified_lab_snapshot_git_baseline_from_env(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
) -> std::result::Result<String, String> {
    let snapshot: SourceSnapshot = env_json(SOURCE_SNAPSHOT_METADATA_ENV)
        .ok_or_else(|| "is missing source snapshot transport metadata".to_string())?;
    let lab: serde_json::Value = env_json(LAB_OFFLOAD_METADATA_ENV)
        .ok_or_else(|| "is missing Lab dispatch transport metadata".to_string())?;
    materialize_verified_lab_snapshot_git_baseline(
        expected_remote_component_path,
        materialized_workspace_path,
        snapshot,
        lab,
    )
}

pub(crate) fn materialize_verified_lab_snapshot_git_baseline(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
    snapshot: SourceSnapshot,
    lab: serde_json::Value,
) -> std::result::Result<String, String> {
    let provenance = verify_lab_workspace(
        expected_remote_component_path,
        materialized_workspace_path,
        snapshot,
        lab,
    )?;
    if provenance.materialization_mode == "git" {
        return Err(
            "does not require a synthetic Git baseline for git materialization".to_string(),
        );
    }
    if materialized_workspace_path.join(".git").exists() {
        return Err("snapshot workspace unexpectedly contains root .git metadata".to_string());
    }
    if let Some(path) = nested_git_metadata(materialized_workspace_path)? {
        return Err(format!(
            "snapshot workspace contains nested Git metadata at {}",
            path.display()
        ));
    }

    git(
        materialized_workspace_path,
        &[
            "init",
            "--quiet",
            "--initial-branch=homeboy-snapshot-baseline",
        ],
    )?;
    git(materialized_workspace_path, &["add", "--all"])?;
    let tree = git(materialized_workspace_path, &["write-tree"])?;
    let message = format!(
        "homeboy snapshot baseline\n\nsource-revision: {}\nworkspace-identity: {}\nsnapshot-hash: {}",
        provenance.source_revision, provenance.workspace_identity, provenance.snapshot_hash,
    );
    let commit = git_with_env(
        materialized_workspace_path,
        &["commit-tree", &tree, "-m", &message],
        &[
            ("GIT_AUTHOR_NAME", "Homeboy Snapshot"),
            ("GIT_AUTHOR_EMAIL", "snapshot@homeboy.invalid"),
            ("GIT_COMMITTER_NAME", "Homeboy Snapshot"),
            ("GIT_COMMITTER_EMAIL", "snapshot@homeboy.invalid"),
            ("GIT_AUTHOR_DATE", "1970-01-01T00:00:00Z"),
            ("GIT_COMMITTER_DATE", "1970-01-01T00:00:00Z"),
        ],
    )?;
    git(
        materialized_workspace_path,
        &[
            "update-ref",
            "refs/heads/homeboy-snapshot-baseline",
            &commit,
        ],
    )?;
    Ok(commit)
}

/// Verifies that a Lab Git materialization owns exactly the requested root and
/// remains at the source revision carried by the verified snapshot contract.
pub(crate) fn verify_lab_workspace_git_root(
    workspace: &Path,
    provenance: &VerifiedLabWorkspaceProvenance,
) -> std::result::Result<(), String> {
    if provenance.materialization_mode != "git" {
        return Err(format!(
            "snapshot materialization mode `{}` must be gitless",
            provenance.materialization_mode
        ));
    }
    let root = workspace
        .canonicalize()
        .map_err(|error| format!("could not canonicalize workspace: {error}"))?;
    let git_root = git(workspace, &["rev-parse", "--show-toplevel"])?;
    let git_root = PathBuf::from(git_root)
        .canonicalize()
        .map_err(|error| format!("could not canonicalize Git root: {error}"))?;
    if root != git_root {
        return Err("Git top-level does not exactly match the managed workspace root".to_string());
    }
    let head = git(workspace, &["rev-parse", "HEAD"])?;
    if head != provenance.source_revision {
        return Err("Git HEAD does not match the verified source revision".to_string());
    }
    if !git(workspace, &["status", "--porcelain"])?.is_empty() {
        return Err("Git workspace is not clean".to_string());
    }
    Ok(())
}

/// Verifies a Lab-materialized workspace against its declared provenance.
/// Snapshot modes require byte-for-byte content parity; Git mode validates its
/// checkout identity separately because checkout normalization can change bytes.
pub(crate) fn verify_lab_workspace_from_env(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
) -> std::result::Result<VerifiedLabWorkspaceProvenance, String> {
    let snapshot: SourceSnapshot = env_json(SOURCE_SNAPSHOT_METADATA_ENV)
        .ok_or_else(|| "is missing source snapshot transport metadata".to_string())?;
    let lab: serde_json::Value = env_json(LAB_OFFLOAD_METADATA_ENV)
        .ok_or_else(|| "is missing Lab dispatch transport metadata".to_string())?;
    verify_lab_workspace(
        expected_remote_component_path,
        materialized_workspace_path,
        snapshot,
        lab,
    )
}

pub(crate) fn verify_lab_workspace(
    expected_remote_component_path: &str,
    materialized_workspace_path: &Path,
    snapshot: SourceSnapshot,
    lab: serde_json::Value,
) -> std::result::Result<VerifiedLabWorkspaceProvenance, String> {
    snapshot
        .local_path
        .as_deref()
        .ok_or("is missing controller source path")?;
    let recorded_remote_path = snapshot
        .remote_path
        .as_deref()
        .ok_or("is missing remote path")?;
    let source_revision = snapshot
        .git_sha
        .as_deref()
        .ok_or("is missing source revision")?;
    let workspace_identity = snapshot
        .workspace_snapshot_identity
        .as_deref()
        .filter(|value| !value.trim().is_empty())
        .ok_or("is missing workspace identity")?;
    let runner_id = lab
        .get("runner_id")
        .and_then(|value| value.as_str())
        .ok_or("is missing runner identity")?;
    let lab_remote_path = lab
        .get("remote_workspace")
        .and_then(|value| value.as_str())
        .ok_or("is missing remote workspace")?;
    let materialization_mode = lab
        .get("sync_mode")
        .and_then(|value| value.as_str())
        .ok_or("is missing materialization mode")?;
    let lab_snapshot = lab
        .get("source_snapshot")
        .ok_or("is missing source snapshot evidence")?;

    if snapshot.sync_mode != LAB_SOURCE_SNAPSHOT_SYNC_MODE {
        return Err(format!(
            "has untrusted source mode `{}`",
            snapshot.sync_mode
        ));
    }
    if !matches!(materialization_mode, "git" | "snapshot" | "snapshot-git") {
        return Err(format!(
            "has untrusted workspace materialization mode `{materialization_mode}`"
        ));
    }
    if materialization_mode == "git"
        && snapshot
            .sync_excludes
            .iter()
            .any(|exclude| exclude == ".git" || exclude == ".git/")
    {
        return Err("claims git materialization while excluding .git metadata".to_string());
    }
    if snapshot.dirty {
        return Err("records a dirty source checkout".to_string());
    }
    if !is_git_revision(source_revision) {
        return Err("has an invalid source revision".to_string());
    }
    if snapshot.runner_id.trim().is_empty() || snapshot.runner_id != runner_id {
        return Err("runner identity does not match the Lab dispatch".to_string());
    }
    if !paths_equal(
        expected_remote_component_path,
        &materialized_workspace_path.to_string_lossy(),
    ) || !paths_equal(
        recorded_remote_path,
        &materialized_workspace_path.to_string_lossy(),
    ) || !paths_equal(
        lab_remote_path,
        &materialized_workspace_path.to_string_lossy(),
    ) {
        return Err("remote workspace does not match materialized path".to_string());
    }
    if lab.get("status").and_then(|value| value.as_str()) != Some("offloaded") {
        return Err("dispatch status is not `offloaded`".to_string());
    }
    if serde_json::to_value(&snapshot).ok().as_ref() != Some(lab_snapshot) {
        return Err("source snapshot does not match Lab dispatch evidence".to_string());
    }
    let verification = lab.get("workspace_verification");
    let (expected_content_hash, verification_identity) = match verification {
        Some(verification) => {
            if verification.get("schema").and_then(|value| value.as_str())
                != Some("homeboy/lab-workspace-verification/v1")
            {
                return Err("has an unsupported workspace verification schema".to_string());
            }
            let identity = verification
                .get("identity")
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace verification identity")?;
            let content_hash = verification
                .get("content_hash")
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace verification content hash")?;
            let excludes = verification
                .get("sync_excludes")
                .ok_or("is missing workspace verification sync excludes")?;
            if excludes != &serde_json::json!(snapshot.sync_excludes) {
                return Err("sync excludes do not match workspace verification".to_string());
            }
            if verification.get("source_snapshot") != Some(lab_snapshot) {
                return Err("source snapshot does not match workspace verification".to_string());
            }
            let primary_workspace = verification
                .get("primary_workspace")
                .ok_or("is missing workspace verification primary workspace")?;
            if primary_workspace
                .get("identity")
                .and_then(|value| value.as_str())
                != Some(identity)
                || primary_workspace
                    .get("remote_path")
                    .and_then(|value| value.as_str())
                    != Some(recorded_remote_path)
            {
                return Err("primary workspace does not match workspace verification".to_string());
            }
            (content_hash, identity)
        }
        None if materialization_mode == "git" => {
            let content_hash = lab
                .get("workspace_content_hash")
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace content hash")?;
            let identity = lab
                .get("workspace_materialization_plan")
                .and_then(|value| value.get("identity"))
                .and_then(|value| value.as_str())
                .ok_or("is missing workspace materialization identity")?;
            (content_hash, identity)
        }
        None => return Err("is missing workspace verification metadata".to_string()),
    };
    if workspace_identity != verification_identity {
        return Err("workspace identity does not match workspace verification".to_string());
    }
    if materialization_mode != "git" {
        let actual_content_hash =
            workspace_content_hash(materialized_workspace_path, &snapshot.sync_excludes).map_err(
                |error| format!("could not hash materialized workspace: {}", error.message),
            )?;
        if actual_content_hash != expected_content_hash {
            return Err("content hash does not match the controller materialization".to_string());
        }
    }

    Ok(VerifiedLabWorkspaceProvenance {
        source_revision: source_revision.to_string(),
        materialization_mode: materialization_mode.to_string(),
        runner_id: snapshot.runner_id,
        workspace_identity: workspace_identity.to_string(),
        snapshot_hash: snapshot.snapshot_hash,
    })
}

fn git(cwd: &Path, args: &[&str]) -> std::result::Result<String, String> {
    git_with_env(cwd, args, &[])
}

fn git_with_env(
    cwd: &Path,
    args: &[&str],
    env: &[(&str, &str)],
) -> std::result::Result<String, String> {
    let output = Command::new("git")
        .args(args)
        .envs(env.iter().copied())
        .current_dir(cwd)
        .output()
        .map_err(|error| format!("could not run git {}: {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn nested_git_metadata(workspace: &Path) -> std::result::Result<Option<PathBuf>, String> {
    fn visit(root: &Path, path: &Path) -> std::result::Result<Option<PathBuf>, String> {
        for entry in std::fs::read_dir(path)
            .map_err(|error| format!("could not inspect snapshot workspace: {error}"))?
        {
            let entry =
                entry.map_err(|error| format!("could not inspect snapshot entry: {error}"))?;
            let path = entry.path();
            if path.file_name().is_some_and(|name| name == ".git") && path != root.join(".git") {
                return Ok(Some(path));
            }
            if entry
                .file_type()
                .map_err(|error| format!("could not inspect snapshot entry type: {error}"))?
                .is_dir()
            {
                if let Some(found) = visit(root, &path)? {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }
    visit(workspace, workspace)
}

fn env_json<T: serde::de::DeserializeOwned>(name: &str) -> Option<T> {
    std::env::var(name)
        .ok()
        .and_then(|raw| serde_json::from_str(&raw).ok())
}

fn paths_equal(left: &str, right: &str) -> bool {
    matches!((Path::new(left).canonicalize(), Path::new(right).canonicalize()), (Ok(left), Ok(right)) if left == right)
}

fn is_git_revision(value: &str) -> bool {
    matches!(value.len(), 40 | 64) && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(path: &Path) -> SourceSnapshot {
        SourceSnapshot {
            runner_id: "lab".to_string(),
            local_path: Some("/controller/source".to_string()),
            remote_path: Some(path.display().to_string()),
            workspace_root: None,
            git_branch: Some("main".to_string()),
            git_sha: Some("a".repeat(40)),
            dirty: false,
            sync_mode: LAB_SOURCE_SNAPSHOT_SYNC_MODE.to_string(),
            workspace_snapshot_identity: Some("snapshot:verified-content".to_string()),
            snapshot_hash: "sha256:verified-source".to_string(),
            synced_at: "2026-01-01T00:00:00Z".to_string(),
            sync_excludes: vec![".git".to_string(), ".git/**".to_string()],
        }
    }

    fn lab(path: &Path, snapshot: &SourceSnapshot) -> serde_json::Value {
        let content_hash =
            workspace_content_hash(path, &snapshot.sync_excludes).expect("snapshot content hash");
        serde_json::json!({
            "runner_id": "lab",
            "remote_workspace": path.display().to_string(),
            "sync_mode": "snapshot",
            "status": "offloaded",
            "source_snapshot": snapshot,
            "workspace_verification": {
                "schema": "homeboy/lab-workspace-verification/v1",
                "identity": "snapshot:verified-content",
                "content_hash": content_hash,
                "sync_excludes": snapshot.sync_excludes,
                "source_snapshot": snapshot,
                "primary_workspace": {
                    "identity": "snapshot:verified-content",
                    "remote_path": path.display().to_string(),
                }
            }
        })
    }

    fn git_workspace() -> tempfile::TempDir {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        git(workspace.path(), &["init", "--quiet"]).expect("initialize repository");
        git(workspace.path(), &["add", "--all"]).expect("stage source");
        git(
            workspace.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "--quiet",
                "-m",
                "baseline",
            ],
        )
        .expect("commit source");
        workspace
    }

    fn git_snapshot(path: &Path) -> SourceSnapshot {
        let mut snapshot = snapshot(path);
        snapshot.git_sha = Some(git(path, &["rev-parse", "HEAD"]).expect("source revision"));
        snapshot.sync_excludes = Vec::new();
        snapshot
    }

    fn git_lab(path: &Path, snapshot: &SourceSnapshot) -> serde_json::Value {
        let mut lab = lab(path, snapshot);
        lab["sync_mode"] = serde_json::json!("git");
        lab["workspace_verification"]["content_hash"] = serde_json::json!("controller-byte-hash");
        lab
    }

    #[test]
    fn git_materialization_accepts_checkout_normalization_hash_difference() {
        let workspace = git_workspace();
        let snapshot = git_snapshot(workspace.path());
        let provenance = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            git_lab(workspace.path(), &snapshot),
        )
        .expect("Git provenance accepts non-authoritative byte hash");

        verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect("clean checkout at expected revision");
    }

    #[test]
    fn git_materialization_rejects_wrong_head_root_identity_and_dirty_workspace() {
        let workspace = git_workspace();
        let snapshot = git_snapshot(workspace.path());
        let lab = git_lab(workspace.path(), &snapshot);
        let provenance = verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab.clone(),
        )
        .expect("initial Git provenance");

        std::fs::write(workspace.path().join("file.txt"), "changed\n").expect("change source");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("dirty Git workspace must fail closed")
            .contains("not clean"));
        git(workspace.path(), &["checkout", "--", "file.txt"]).expect("restore source");
        git(
            workspace.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "--allow-empty",
                "--quiet",
                "-m",
                "wrong head",
            ],
        )
        .expect("advance head");
        assert!(verify_lab_workspace_git_root(workspace.path(), &provenance)
            .expect_err("wrong Git HEAD must fail closed")
            .contains("HEAD does not match"));

        let nested_root = workspace.path().join("nested");
        std::fs::create_dir(&nested_root).expect("nested path");
        assert!(verify_lab_workspace_git_root(&nested_root, &provenance)
            .expect_err("wrong managed root must fail closed")
            .contains("top-level does not exactly match"));

        let mut wrong_identity = lab;
        wrong_identity["workspace_verification"]["identity"] = serde_json::json!("other");
        wrong_identity["workspace_verification"]["primary_workspace"]["identity"] =
            serde_json::json!("other");
        assert!(verify_lab_workspace(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot,
            wrong_identity,
        )
        .expect_err("wrong declared identity must fail closed")
        .contains("workspace identity"));
    }

    #[test]
    fn snapshot_materializations_reject_content_hash_mismatch() {
        for mode in ["snapshot", "snapshot-git"] {
            let workspace = tempfile::tempdir().expect("workspace");
            std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
            let snapshot = snapshot(workspace.path());
            let mut lab = lab(workspace.path(), &snapshot);
            lab["sync_mode"] = serde_json::json!(mode);
            lab["workspace_verification"]["content_hash"] = serde_json::json!("wrong-hash");

            assert!(verify_lab_workspace(
                &workspace.path().display().to_string(),
                workspace.path(),
                snapshot,
                lab,
            )
            .expect_err("snapshot hash mismatch must fail closed")
            .contains("content hash"));
        }
    }

    #[test]
    fn verified_snapshot_baseline_supports_committed_and_uncommitted_candidate_harvesting() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let snapshot = snapshot(workspace.path());
        let baseline = materialize_verified_lab_snapshot_git_baseline(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab(workspace.path(), &snapshot),
        )
        .expect("verified snapshot baseline");

        assert_eq!(
            git(workspace.path(), &["rev-parse", "HEAD"]).unwrap(),
            baseline
        );
        let message = git(workspace.path(), &["log", "-1", "--format=%B"]).unwrap();
        assert!(message.contains("source-revision: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"));
        assert!(message.contains("workspace-identity: snapshot:verified-content"));
        assert!(message.contains("snapshot-hash: sha256:verified-source"));
        std::fs::write(workspace.path().join("file.txt"), "candidate\n").expect("candidate");
        git(workspace.path(), &["add", "--all"]).expect("stage candidate");
        git(
            workspace.path(),
            &[
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "-m",
                "provider candidate",
            ],
        )
        .expect("commit candidate");
        let committed_patch = git(workspace.path(), &["diff", "--binary", &baseline, "HEAD"])
            .expect("committed candidate patch");
        assert!(committed_patch.contains("-baseline"));
        assert!(committed_patch.contains("+candidate"));

        std::fs::write(workspace.path().join("file.txt"), "uncommitted candidate\n")
            .expect("uncommitted candidate");
        git(workspace.path(), &["add", "--all"]).expect("stage uncommitted candidate");
        let uncommitted_patch = git(workspace.path(), &["diff", "--cached", "--binary", "HEAD"])
            .expect("candidate patch");
        assert!(uncommitted_patch.contains("-candidate"));
        assert!(uncommitted_patch.contains("+uncommitted candidate"));
    }

    #[test]
    fn snapshot_baseline_rejects_invalid_provenance_before_creating_git_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        std::fs::write(workspace.path().join("file.txt"), "baseline\n").expect("source file");
        let mut snapshot = snapshot(workspace.path());
        snapshot.git_sha = Some("invalid".to_string());

        let error = materialize_verified_lab_snapshot_git_baseline(
            &workspace.path().display().to_string(),
            workspace.path(),
            snapshot.clone(),
            lab(workspace.path(), &snapshot),
        )
        .expect_err("invalid provenance must fail closed");

        assert!(error.contains("invalid source revision"));
        assert!(!workspace.path().join(".git").exists());
    }
}
