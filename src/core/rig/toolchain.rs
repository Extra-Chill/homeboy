//! Toolchain environment helpers for rig command steps.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

const HOME_BIN_DIRS: &[&str] = &[".local/bin", ".cargo/bin", ".kimaki/bin"];
const ABSOLUTE_BIN_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Builds the default PATH for rig `command` steps.
///
/// Existing developer-tool locations are prepended before the inherited PATH.
/// Missing directories are ignored so the result stays portable across hosts.
pub fn command_step_path() -> Option<OsString> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let existing_path = std::env::var_os("PATH");
    build_command_step_path(home.as_deref(), existing_path.as_deref())
}

pub(crate) fn build_command_step_path(
    home: Option<&Path>,
    existing_path: Option<&OsStr>,
) -> Option<OsString> {
    build_command_step_path_with_absolute_dirs(home, existing_path, ABSOLUTE_BIN_DIRS)
}

fn build_command_step_path_with_absolute_dirs(
    home: Option<&Path>,
    existing_path: Option<&OsStr>,
    absolute_dirs: &[&str],
) -> Option<OsString> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    if let Some(home) = home {
        for rel in HOME_BIN_DIRS {
            push_existing_path(&mut paths, &mut seen, home.join(rel));
        }
        push_nvm_node_bins(&mut paths, &mut seen, home);
    }

    for path in absolute_dirs {
        push_existing_path(&mut paths, &mut seen, PathBuf::from(path));
    }

    if let Some(existing_path) = existing_path {
        for path in std::env::split_paths(existing_path) {
            push_path(&mut paths, &mut seen, path);
        }
    }

    if paths.is_empty() {
        None
    } else {
        std::env::join_paths(paths).ok()
    }
}

fn push_existing_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if path.exists() {
        push_path(paths, seen, path);
    }
}

fn push_nvm_node_bins(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, home: &Path) {
    let versions_dir = home.join(".nvm/versions/node");
    let Ok(entries) = fs::read_dir(versions_dir) else {
        return;
    };

    let mut bins = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path().join("bin")))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    bins.sort();
    bins.reverse();

    for bin in bins {
        push_path(paths, seen, bin);
    }
}

fn push_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if seen.insert(path.clone()) {
        paths.push(path);
    }
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::fs;
    use std::path::PathBuf;

    use super::build_command_step_path_with_absolute_dirs;

    #[test]
    fn test_command_step_path_prepends_existing_toolchain_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let cargo = home.join(".cargo/bin");
        fs::create_dir_all(&local).expect("local bin");
        fs::create_dir_all(&cargo).expect("cargo bin");

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path_with_absolute_dirs(Some(&home), Some(&inherited), &[])
            .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], local);
        assert_eq!(parts[1], cargo);
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
        assert!(parts.contains(&PathBuf::from("/bin")));
        assert!(!parts.contains(&home.join(".kimaki/bin")));
    }

    #[test]
    fn test_command_step_path_prepends_nvm_node_bins() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let node_20 = home.join(".nvm/versions/node/v20.0.0/bin");
        let node_24 = home.join(".nvm/versions/node/v24.13.1/bin");
        fs::create_dir_all(&node_20).expect("node 20 bin");
        fs::create_dir_all(&node_24).expect("node 24 bin");

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path_with_absolute_dirs(Some(&home), Some(&inherited), &[])
            .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], node_24);
        assert_eq!(parts[1], node_20);
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn test_command_step_path_keeps_existing_path_without_home() {
        let inherited = OsString::from("/usr/bin:/bin");
        let path =
            build_command_step_path_with_absolute_dirs(None, Some(&inherited), &[]).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(
            parts,
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
    }

    #[test]
    fn test_command_step_path_deduplicates_entries() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        fs::create_dir_all(&local).expect("local bin");

        let inherited = OsString::from(local.to_string_lossy().into_owned());
        let path = build_command_step_path_with_absolute_dirs(Some(&home), Some(&inherited), &[])
            .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts, vec![local]);
    }

    #[test]
    fn test_command_step_path_prepends_existing_absolute_toolchain_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let homebrew = tmp.path().join("opt-homebrew-bin");
        let missing = tmp.path().join("missing-bin");
        fs::create_dir_all(&homebrew).expect("homebrew bin");

        let inherited = OsString::from("/usr/bin:/bin");
        let absolute_dirs = [homebrew.to_str().unwrap(), missing.to_str().unwrap()];
        let path =
            build_command_step_path_with_absolute_dirs(None, Some(&inherited), &absolute_dirs)
                .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], homebrew);
        assert!(!parts.contains(&missing));
    }
}
