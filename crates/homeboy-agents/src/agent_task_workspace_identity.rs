use std::path::Path;

use homeboy_core::{Error, Result};
use serde_json::Value;

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
    let git_content = std::fs::read_to_string(&git_file).map_err(|error| {
        Error::internal_io(error.to_string(), Some(git_file.display().to_string()))
    })?;
    let gitdir_target = git_content
        .strip_prefix("gitdir: ")
        .map(str::trim)
        .and_then(|target| std::fs::canonicalize(canonical.join(target)).ok());
    Ok(serde_json::json!({
        "canonical_path": canonical,
        "device": metadata.dev(),
        "inode": metadata.ino(),
        "git_file_is_file": git_metadata.file_type().is_file(),
        "git_file_content": git_content,
        "gitdir_target": gitdir_target,
    }))
}

#[cfg(not(unix))]
pub(crate) fn attest_workspace(path: &Path) -> Result<Value> {
    let canonical = std::fs::canonicalize(path)
        .map_err(|error| Error::internal_io(error.to_string(), Some(path.display().to_string())))?;
    Ok(serde_json::json!({ "canonical_path": canonical }))
}

#[cfg(unix)]
pub(crate) fn workspace_matches_attestation(path: &Path, attestation: &Value) -> bool {
    use std::os::unix::fs::MetadataExt;

    let Ok(supplied_metadata) = std::fs::symlink_metadata(path) else {
        return false;
    };
    if supplied_metadata.file_type().is_symlink() || !supplied_metadata.is_dir() {
        return false;
    }
    let Ok(canonical) = std::fs::canonicalize(path) else {
        return false;
    };
    let Ok(metadata) = std::fs::symlink_metadata(&canonical) else {
        return false;
    };
    !metadata.file_type().is_symlink()
        && attestation["canonical_path"].as_str() == canonical.to_str()
        && attestation["device"].as_u64() == Some(metadata.dev())
        && attestation["inode"].as_u64() == Some(metadata.ino())
        && linked_git_metadata_matches(&canonical, attestation)
}

#[cfg(unix)]
fn linked_git_metadata_matches(worktree: &Path, attestation: &Value) -> bool {
    let git_file = worktree.join(".git");
    let Ok(metadata) = std::fs::symlink_metadata(&git_file) else {
        return false;
    };
    if !metadata.file_type().is_file() || attestation["git_file_is_file"] != true {
        return false;
    }
    let Ok(content) = std::fs::read_to_string(&git_file) else {
        return false;
    };
    if attestation["git_file_content"].as_str() != Some(content.as_str()) {
        return false;
    }
    let target = content
        .strip_prefix("gitdir: ")
        .map(str::trim)
        .and_then(|target| std::fs::canonicalize(worktree.join(target)).ok());
    target.as_deref().and_then(|path| path.to_str()) == attestation["gitdir_target"].as_str()
}

#[cfg(not(unix))]
pub(crate) fn workspace_matches_attestation(path: &Path, attestation: &Value) -> bool {
    std::fs::canonicalize(path)
        .ok()
        .and_then(|path| path.to_str().map(str::to_string))
        .as_deref()
        == attestation["canonical_path"].as_str()
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
}
