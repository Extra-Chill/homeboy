//! The `PATH` Homeboy hands to commands it runs.
//!
//! Rig `command` steps, lifecycle phases, and extension executions see a
//! `PATH` with Homeboy's built-in bin directories prepended to the inherited
//! one, so commonly installed tools resolve without per-rig shims.
//!
//! This is a pure function of the host, so every caller reaches it directly.
//! It used to sit behind a rig-registered provider in core, which left the
//! extension runner without it in any process that never registered the rig
//! layer.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

/// Home-relative bin directories placed ahead of everything else, in order.
///
/// These entries are language- and product-specific (`.cargo/bin` is Rust,
/// `.kimaki/bin` is one particular third-party product) and do not belong in a
/// generic orchestrator. They stay because removing them would silently break
/// every host that depends on today's behavior.
const HOME_PREPEND_DIRS: &[&str] = &[".local/bin", ".cargo/bin", ".kimaki/bin"];

/// Home-relative nvm version root. Each `<version>/bin` child is added after
/// [`HOME_PREPEND_DIRS`], newest version (descending name order) first.
const NODE_VERSIONS_DIR: &str = ".nvm/versions/node";

/// Absolute bin directories placed after nvm discovery, still ahead of the
/// inherited `PATH`.
const APPEND_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Builds the PATH for rig `command` steps.
///
/// Existing built-in directories are prepended before the inherited PATH;
/// missing ones are skipped so the result stays portable across hosts.
pub fn command_step_path() -> Option<OsString> {
    let home = homeboy_paths::home_root().ok();
    let existing_path = std::env::var_os("PATH");
    build_command_step_path(home.as_deref(), APPEND_DIRS, existing_path.as_deref())
}

/// Assembles the command-step PATH against an explicit home, append list, and
/// inherited PATH. `~`-relative entries are dropped when no home is known.
fn build_command_step_path(
    home: Option<&Path>,
    append_dirs: &[&str],
    existing_path: Option<&OsStr>,
) -> Option<OsString> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    if let Some(home) = home {
        for dir in HOME_PREPEND_DIRS {
            push_existing_path(&mut paths, &mut seen, home.join(dir));
        }
        push_node_version_bins(&mut paths, &mut seen, &home.join(NODE_VERSIONS_DIR));
    }

    for dir in append_dirs {
        push_existing_path(&mut paths, &mut seen, PathBuf::from(dir));
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

fn push_node_version_bins(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, root: &Path) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    let mut discovered = entries
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path().join("bin"))
        .filter(|path| path.exists())
        .collect::<Vec<_>>();
    discovered.sort();
    discovered.reverse();

    for path in discovered {
        push_path(paths, seen, path);
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

    use super::build_command_step_path;

    #[test]
    fn test_build_command_step_path_prepends_existing_toolchain_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let cargo = home.join(".cargo/bin");
        fs::create_dir_all(&local).expect("local bin");
        fs::create_dir_all(&cargo).expect("cargo bin");

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path(Some(&home), &[], Some(&inherited)).expect("path");
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
        // A version directory without a bin subdir is skipped, not emitted bare.
        fs::create_dir_all(home.join(".nvm/versions/node/v18.0.0")).expect("node 18");

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path(Some(&home), &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], node_24);
        assert_eq!(parts[1], node_20);
        assert!(!parts.contains(&home.join(".nvm/versions/node/v18.0.0")));
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn test_nvm_discovery_follows_home_bin_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let cargo = home.join(".cargo/bin");
        let node = home.join(".nvm/versions/node/v20.0.0/bin");
        for dir in [&local, &cargo, &node] {
            fs::create_dir_all(dir).expect("bin");
        }

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path(Some(&home), &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], local);
        assert_eq!(parts[1], cargo);
        assert_eq!(parts[2], node, "nvm discovery still precedes system dirs");
    }

    #[test]
    fn test_command_step_path_keeps_existing_path_without_home() {
        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path(None, &[], Some(&inherited)).expect("path");
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
        let path = build_command_step_path(Some(&home), &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts, vec![local]);
    }

    #[test]
    fn test_command_step_path_appends_existing_absolute_toolchain_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let homebrew = tmp.path().join("opt-homebrew-bin");
        let missing = tmp.path().join("missing-bin");
        fs::create_dir_all(&homebrew).expect("homebrew bin");

        let homebrew_dir = homebrew.to_string_lossy().into_owned();
        let missing_dir = missing.to_string_lossy().into_owned();
        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path(
            None,
            &[homebrew_dir.as_str(), missing_dir.as_str()],
            Some(&inherited),
        )
        .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], homebrew);
        assert!(!parts.contains(&missing));
    }
}
