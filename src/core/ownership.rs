use std::path::Path;
use std::process::Command;

use crate::core::error::{Error, Result};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileOwner {
    uid: u32,
    gid: u32,
}

impl FileOwner {
    fn is_root(self) -> bool {
        self.uid == 0 && self.gid == 0
    }

    fn spec(self) -> String {
        format!("{}:{}", self.uid, self.gid)
    }
}

#[cfg(unix)]
pub(crate) fn owner_for_path_or_ancestor(path: &Path) -> Result<Option<FileOwner>> {
    for candidate in path.ancestors() {
        if !candidate.exists() {
            continue;
        }
        let metadata = candidate.metadata().map_err(|err| {
            Error::internal_io(
                err.to_string(),
                Some(format!("read ownership for {}", candidate.display())),
            )
        })?;
        return Ok(Some(FileOwner {
            uid: metadata.uid(),
            gid: metadata.gid(),
        }));
    }
    Ok(None)
}

#[cfg(not(unix))]
pub(crate) fn owner_for_path_or_ancestor(_path: &Path) -> Result<Option<FileOwner>> {
    Ok(None)
}

#[cfg(unix)]
pub(crate) fn normalize_created_path(
    path: &Path,
    owner: Option<FileOwner>,
    recursive: bool,
    action: &str,
) -> Result<()> {
    let Some(owner) = owner else {
        return Ok(());
    };
    if unsafe { libc::geteuid() } != 0 || owner.is_root() || !path.exists() {
        return Ok(());
    }

    let mut command = Command::new("chown");
    if recursive {
        command.arg("-R");
    }
    let output = command
        .arg(owner.spec())
        .arg(path)
        .output()
        .map_err(|err| {
            Error::internal_io(err.to_string(), Some(format!("chown after {action}")))
        })?;
    if output.status.success() {
        return Ok(());
    }

    Err(Error::internal_unexpected(format!(
        "failed to restore ownership after {action}: {}",
        String::from_utf8_lossy(&output.stderr).trim()
    )))
}

#[cfg(not(unix))]
pub(crate) fn normalize_created_path(
    _path: &Path,
    _owner: Option<FileOwner>,
    _recursive: bool,
    _action: &str,
) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn owner_resolution_uses_nearest_existing_ancestor() {
        let root = tempfile::tempdir().expect("tempdir");
        let owner =
            owner_for_path_or_ancestor(&root.path().join("missing").join("child").join("file.txt"))
                .expect("owner lookup")
                .expect("owner");
        let root_metadata = root.path().metadata().expect("metadata");

        assert_eq!(owner.uid, root_metadata.uid());
        assert_eq!(owner.gid, root_metadata.gid());
    }
}
