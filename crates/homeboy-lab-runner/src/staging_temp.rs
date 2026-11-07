//! Invocation-scoped scratch for runner materialization. Controller callers retain
//! their system-temp behavior; runner endpoints install the workspace root.
use std::cell::RefCell;
use std::path::{Path, PathBuf};

thread_local! {
    static ROOT: RefCell<Option<PathBuf>> = const { RefCell::new(None) };
}

pub(crate) struct Scope(Option<PathBuf>);

/// Shared staging reserve used by admission and runner doctor.
pub const MIN_STAGING_FREE_BYTES: u64 = 1024 * 1024 * 1024;
pub const MIN_STAGING_FREE_RATIO: f64 = 0.01;

impl Scope {
    pub(crate) fn new(workspace_root: &Path) -> Self {
        Self(ROOT.with(|root| root.replace(Some(workspace_root.join(".homeboy-tmp")))))
    }
}

impl Drop for Scope {
    fn drop(&mut self) {
        ROOT.with(|root| root.replace(self.0.take()));
    }
}

fn directory() -> std::io::Result<Option<PathBuf>> {
    ROOT.with(|root| {
        let path = root.borrow().clone();
        if let Some(path) = &path {
            std::fs::create_dir_all(path).map_err(|error| {
                std::io::Error::new(error.kind(), format!("{error} at {}", path.display()))
            })?;
        }
        Ok(path)
    })
}

pub(crate) fn file() -> std::io::Result<tempfile::NamedTempFile> {
    match directory()? {
        Some(path) => tempfile::NamedTempFile::new_in(path),
        None => tempfile::NamedTempFile::new(),
    }
}

pub(crate) fn dir() -> std::io::Result<tempfile::TempDir> {
    match directory()? {
        Some(path) => tempfile::tempdir_in(path),
        None => tempfile::tempdir(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn git_evidence_spools_below_runner_workspace_root() {
        let workspace = tempfile::tempdir().unwrap();
        let _scope = Scope::new(workspace.path());
        let output = crate::workspace::WorkspaceControl::default()
            .output_exact(
                std::process::Command::new("sh").args(["-c", "readlink /proc/self/fd/1"]),
                "read workspace Git evidence",
            )
            .unwrap();
        let staged = PathBuf::from(String::from_utf8(output.stdout).unwrap().trim());
        assert_eq!(
            staged.parent().unwrap(),
            workspace.path().join(".homeboy-tmp")
        );
        assert!(!staged.exists());
    }

    #[test]
    fn materialization_scratch_is_workspace_owned_and_removed_on_drop() {
        let workspace = tempfile::tempdir().unwrap();
        let _scope = Scope::new(workspace.path());
        let file = file().unwrap();
        let dir = dir().unwrap();
        let path = file.path().to_owned();
        let directory = dir.path().to_owned();
        assert_eq!(
            path.parent().unwrap(),
            workspace.path().join(".homeboy-tmp")
        );
        assert_eq!(directory.parent().unwrap(), path.parent().unwrap());
        drop((file, dir));
        assert!(!path.exists());
        assert!(!directory.exists());
    }
}
