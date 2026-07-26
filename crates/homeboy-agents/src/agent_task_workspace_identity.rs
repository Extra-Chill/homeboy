use std::path::Path;

use homeboy_core::{Error, Result};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkspaceAttestationMatch {
    Matched,
    Mismatched,
    GitRepresentationDrift,
}

#[cfg(unix)]
pub(crate) fn attest_workspace(path: &Path) -> Result<Value> {
    use std::os::unix::fs::MetadataExt;

    let supplied_metadata = std::fs::symlink_metadata(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    if supplied_metadata.file_type().is_symlink() || !supplied_metadata.is_dir() {
        return Err(Error::validation_invalid_argument(
            "workspace",
            "Cook workspace must be a non-symlink directory",
            Some(path.display().to_string()),
            None,
        ));
    }
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    let metadata = std::fs::symlink_metadata(&canonical).map_err(|error| {
        Error::internal_io(error.to_string(), Some(canonical.display().to_string()))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::validation_invalid_argument(
            "workspace",
            "Cook workspace must be a non-symlink directory",
            Some(canonical.display().to_string()),
            None,
        ));
    }
    let git_file = canonical.join(".git");
    let git_metadata = std::fs::symlink_metadata(&git_file).map_err(|error| {
        Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let workspace_identity = serde_json::json!({
        "canonical_path": canonical,
        "device": metadata.dev(),
        "inode": metadata.ino(),
    });
    if git_metadata.file_type().is_file() {
        let git_content = std::fs::read_to_string(&git_file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
        })?;
        let gitdir_target = git_content
            .strip_prefix("gitdir: ")
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .and_then(|target| std::fs::canonicalize(canonical.join(target)).ok())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "workspace",
                    "Cook workspace .git pointer must reference an existing Git directory",
                    Some(git_file.display().to_string()),
                    None,
                )
            })?;
        return Ok(serde_json::json!({
            "canonical_path": workspace_identity["canonical_path"],
            "device": workspace_identity["device"],
            "inode": workspace_identity["inode"],
            "git_representation": "pointer_file",
            // Retain these fields so persisted controller worktree attestations
            // continue to verify after this representation discriminator lands.
            "git_file_is_file": true,
            "git_file_content": git_content,
            "gitdir_target": gitdir_target,
        }));
    }
    if git_metadata.file_type().is_dir() {
        let git_directory = std::fs::canonicalize(&git_file).map_err(|error| {
            Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
        })?;
        let git_directory_metadata =
            std::fs::symlink_metadata(&git_directory).map_err(|error| {
                Error::internal_io(error.to_string(), Some(git_directory.display().to_string()))
            })?;
        return Ok(serde_json::json!({
            "canonical_path": workspace_identity["canonical_path"],
            "device": workspace_identity["device"],
            "inode": workspace_identity["inode"],
            "git_representation": "directory",
            "git_dir_canonical_path": git_directory,
            "git_dir_device": git_directory_metadata.dev(),
            "git_dir_inode": git_directory_metadata.ino(),
        }));
    }
    Err(Error::validation_invalid_argument(
        "workspace",
        "Cook workspace .git must be a regular pointer file or directory",
        Some(git_file.display().to_string()),
        None,
    ))
}

#[cfg(not(unix))]
pub(crate) fn attest_workspace(path: &Path) -> Result<Value> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    Ok(serde_json::json!({ "canonical_path": canonical }))
}

#[cfg(unix)]
pub fn workspace_matches_attestation(path: &Path, attestation: &Value) -> bool {
    workspace_attestation_match(path, attestation) == WorkspaceAttestationMatch::Matched
}

#[cfg(unix)]
pub fn workspace_attestation_match(path: &Path, attestation: &Value) -> WorkspaceAttestationMatch {
    use std::os::unix::fs::MetadataExt;

    let Ok(supplied_metadata) = std::fs::symlink_metadata(path) else {
        return WorkspaceAttestationMatch::Mismatched;
    };
    if supplied_metadata.file_type().is_symlink() || !supplied_metadata.is_dir() {
        return WorkspaceAttestationMatch::Mismatched;
    }
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return WorkspaceAttestationMatch::Mismatched;
    };
    let Ok(metadata) = std::fs::symlink_metadata(&canonical) else {
        return WorkspaceAttestationMatch::Mismatched;
    };
    if metadata.file_type().is_symlink()
        || attestation["canonical_path"].as_str() != canonical.to_str()
        || attestation["device"].as_u64() != Some(metadata.dev())
        || attestation["inode"].as_u64() != Some(metadata.ino())
    {
        return WorkspaceAttestationMatch::Mismatched;
    }
    git_metadata_match(&canonical, attestation)
}

#[cfg(unix)]
fn git_metadata_match(worktree: &Path, attestation: &Value) -> WorkspaceAttestationMatch {
    use std::os::unix::fs::MetadataExt;

    let git_file = worktree.join(".git");
    let Ok(metadata) = std::fs::symlink_metadata(&git_file) else {
        return WorkspaceAttestationMatch::Mismatched;
    };
    let expected = attestation["git_representation"]
        .as_str()
        .or_else(|| (attestation["git_file_is_file"] == true).then_some("pointer_file"));
    let actual = if metadata.file_type().is_file() {
        "pointer_file"
    } else if metadata.file_type().is_dir() {
        "directory"
    } else {
        return WorkspaceAttestationMatch::Mismatched;
    };
    if expected != Some(actual) {
        return WorkspaceAttestationMatch::GitRepresentationDrift;
    }
    if actual == "pointer_file" {
        let Ok(content) = std::fs::read_to_string(&git_file) else {
            return WorkspaceAttestationMatch::Mismatched;
        };
        if attestation["git_file_content"].as_str() != Some(content.as_str()) {
            return WorkspaceAttestationMatch::Mismatched;
        }
        let target = content
            .strip_prefix("gitdir: ")
            .map(str::trim)
            .filter(|target| !target.is_empty())
            .and_then(|target| std::fs::canonicalize(worktree.join(target)).ok());
        return if target.as_deref().and_then(|path| path.to_str())
            == attestation["gitdir_target"].as_str()
        {
            WorkspaceAttestationMatch::Matched
        } else {
            WorkspaceAttestationMatch::Mismatched
        };
    }
    let Ok(canonical) = std::fs::canonicalize(&git_file) else {
        return WorkspaceAttestationMatch::Mismatched;
    };
    let Ok(directory_metadata) = std::fs::symlink_metadata(&canonical) else {
        return WorkspaceAttestationMatch::Mismatched;
    };
    if directory_metadata.file_type().is_symlink() || !directory_metadata.is_dir() {
        return WorkspaceAttestationMatch::Mismatched;
    }
    if attestation["git_dir_canonical_path"].as_str() == canonical.to_str()
        && attestation["git_dir_device"].as_u64() == Some(directory_metadata.dev())
        && attestation["git_dir_inode"].as_u64() == Some(directory_metadata.ino())
    {
        WorkspaceAttestationMatch::Matched
    } else {
        WorkspaceAttestationMatch::Mismatched
    }
}

#[cfg(not(unix))]
pub fn workspace_matches_attestation(path: &Path, attestation: &Value) -> bool {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|path| path.to_str().map(str::to_string))
        .as_deref()
        == attestation["canonical_path"].as_str()
}

#[cfg(not(unix))]
pub fn workspace_attestation_match(path: &Path, attestation: &Value) -> WorkspaceAttestationMatch {
    workspace_matches_attestation(path, attestation)
        .then_some(WorkspaceAttestationMatch::Matched)
        .unwrap_or(WorkspaceAttestationMatch::Mismatched)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn rejects_a_symlink_even_when_it_resolves_to_a_valid_workspace() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let gitdir = temp.path().join("gitdir");
        let alias = temp.path().join("workspace-alias");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&gitdir).expect("gitdir");
        std::fs::write(workspace.join(".git"), "gitdir: ../gitdir\n").expect("git file");
        let attestation = attest_workspace(&workspace).expect("attest workspace");
        symlink(&workspace, &alias).expect("workspace symlink");

        assert!(attest_workspace(&alias).is_err());
        assert!(!workspace_matches_attestation(&alias, &attestation));
    }

    #[test]
    fn attests_and_rejects_replaced_normal_git_directories() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let git_directory = workspace.join(".git");
        std::fs::create_dir_all(&git_directory).expect("git directory");
        let attestation = attest_workspace(&workspace).expect("attest workspace");

        assert_eq!(attestation["git_representation"], "directory");
        assert!(workspace_matches_attestation(&workspace, &attestation));
        std::fs::rename(&git_directory, workspace.join("replaced-git-directory"))
            .expect("replace git directory");
        std::fs::create_dir(&git_directory).expect("new git directory");
        assert!(!workspace_matches_attestation(&workspace, &attestation));
    }

    #[test]
    fn reports_pointer_to_directory_representation_drift() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let gitdir = temp.path().join("gitdir");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&gitdir).expect("gitdir");
        std::fs::write(workspace.join(".git"), "gitdir: ../gitdir\n").expect("git file");
        let attestation = attest_workspace(&workspace).expect("attest workspace");
        std::fs::remove_file(workspace.join(".git")).expect("remove git file");
        std::fs::create_dir(workspace.join(".git")).expect("git directory");

        assert_eq!(
            workspace_attestation_match(&workspace, &attestation),
            WorkspaceAttestationMatch::GitRepresentationDrift
        );
    }

    #[test]
    fn accepts_legacy_pointer_attestations_without_a_representation_discriminator() {
        let temp = tempfile::tempdir().expect("tempdir");
        let workspace = temp.path().join("workspace");
        let gitdir = temp.path().join("gitdir");
        std::fs::create_dir(&workspace).expect("workspace");
        std::fs::create_dir(&gitdir).expect("gitdir");
        std::fs::write(workspace.join(".git"), "gitdir: ../gitdir\n").expect("git file");
        let mut attestation = attest_workspace(&workspace).expect("attest workspace");
        attestation
            .as_object_mut()
            .expect("attestation object")
            .remove("git_representation");

        assert!(workspace_matches_attestation(&workspace, &attestation));
    }
}
