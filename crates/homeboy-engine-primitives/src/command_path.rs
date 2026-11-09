//! The `PATH` Homeboy hands to commands it runs.
//!
//! One builder discovers a host's installed tools: home-relative bin
//! directories, then Node version bins, then a caller-selected list of
//! absolute bin directories, deduplicated against the inherited `PATH`.
//! Missing directories are skipped so the result stays portable across hosts.
//!
//! - [`command_step_path`] serves rig `command` steps, lifecycle phases, and
//!   extension executions on the local host. The caller's `PATH` keeps its
//!   order and wins; discovered directories are only appended as fallbacks,
//!   so the toolchain the operator selected (e.g. `actions/setup-node`) is the
//!   one a child resolves.
//! - [`runner_tool_path`] serves Lab runner commands and gates, which must
//!   resolve the runner user's installed tools (npm, node, composer, ...)
//!   even when the command's `HOME` is isolated.
//!
//! This is a pure function of the host, so every caller reaches it directly.

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

/// Home-relative Node install roots and the child-name prefix each requires.
/// Every matching `<child>/bin` is added after [`HOME_PREPEND_DIRS`], newest
/// version (descending name order) first within each root.
const NODE_VERSION_ROOTS: &[(&str, &str)] = &[(".local/opt", "node-"), (".nvm/versions/node", "")];

/// Absolute bin directories for local command steps, appended after the
/// inherited `PATH` and Node discovery.
const COMMAND_STEP_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Absolute bin directories for runner commands. A runner's inherited `PATH`
/// may be a minimal service-manager value, so the system directories are
/// placed explicitly ahead of it.
const RUNNER_SYSTEM_DIRS: &[&str] = &[
    "/opt/homebrew/bin",
    "/usr/local/bin",
    "/usr/bin",
    "/bin",
    "/usr/sbin",
    "/sbin",
];

/// Builds the PATH for local command steps and extension executions.
///
/// The caller's inherited `PATH` comes first, verbatim and in order; discovered
/// tool directories are appended only as fallbacks for a minimal caller PATH.
pub fn command_step_path() -> Option<OsString> {
    let home = homeboy_paths::home_root().ok();
    let existing_path = std::env::var_os("PATH");
    build_caller_first_path(home.as_deref(), COMMAND_STEP_DIRS, existing_path.as_deref())
}

/// Resolve `command` the way a child process given `path` would: the first
/// executable file named `command` in `path` order.
pub fn resolve_command_on_path(command: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .filter(|directory| !directory.as_os_str().is_empty())
        .map(|directory| directory.join(command))
        .find(|candidate| is_executable_file(candidate))
}

#[cfg(unix)]
fn is_executable_file(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    fs::metadata(path)
        .map(|metadata| metadata.is_file() && metadata.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable_file(path: &Path) -> bool {
    path.is_file()
}

/// Builds the PATH for commands a Lab runner executes on its own host.
///
/// `home` is the runner user's real home: pass it explicitly so a command
/// whose `HOME` is isolated still resolves the runner's installed tools.
pub fn runner_tool_path(home: Option<&Path>, existing_path: Option<&OsStr>) -> Option<OsString> {
    build_tool_path(home, RUNNER_SYSTEM_DIRS, existing_path)
}

/// Assembles a PATH that keeps the inherited PATH first and in order, then
/// appends discovered tool directories it does not already contain.
fn build_caller_first_path(
    home: Option<&Path>,
    absolute_dirs: &[&str],
    existing_path: Option<&OsStr>,
) -> Option<OsString> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    push_inherited_path(&mut paths, &mut seen, existing_path);
    push_discovered_dirs(&mut paths, &mut seen, home, absolute_dirs);
    join_tool_paths(paths)
}

/// Assembles a tool PATH against an explicit home, absolute dir list, and
/// inherited PATH. `~`-relative entries are dropped when no home is known.
fn build_tool_path(
    home: Option<&Path>,
    absolute_dirs: &[&str],
    existing_path: Option<&OsStr>,
) -> Option<OsString> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();
    push_discovered_dirs(&mut paths, &mut seen, home, absolute_dirs);
    push_inherited_path(&mut paths, &mut seen, existing_path);
    join_tool_paths(paths)
}

fn push_discovered_dirs(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    home: Option<&Path>,
    absolute_dirs: &[&str],
) {
    if let Some(home) = home {
        for dir in HOME_PREPEND_DIRS {
            push_existing_path(paths, seen, home.join(dir));
        }
        for (root, prefix) in NODE_VERSION_ROOTS {
            push_node_version_bins(paths, seen, &home.join(root), prefix);
        }
    }

    for dir in absolute_dirs {
        push_existing_path(paths, seen, PathBuf::from(dir));
    }
}

fn push_inherited_path(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    existing_path: Option<&OsStr>,
) {
    if let Some(existing_path) = existing_path {
        for path in std::env::split_paths(existing_path) {
            push_path(paths, seen, path);
        }
    }
}

fn join_tool_paths(paths: Vec<PathBuf>) -> Option<OsString> {
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

fn push_node_version_bins(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    root: &Path,
    prefix: &str,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    let mut discovered = entries
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with(prefix))
        })
        .map(|path| path.join("bin"))
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

    use super::{
        build_caller_first_path, build_tool_path, resolve_command_on_path, runner_tool_path,
    };

    #[test]
    fn caller_first_path_keeps_inherited_order_ahead_of_discovered_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let nvm_node = home.join(".nvm/versions/node/v20.0.0/bin");
        let system = tmp.path().join("usr-local-bin");
        let caller_node = tmp.path().join("setup-node/bin");
        for dir in [&local, &nvm_node, &system, &caller_node] {
            fs::create_dir_all(dir).expect("bin");
        }
        let system_dir = system.to_string_lossy().into_owned();
        let inherited =
            std::env::join_paths([caller_node.clone(), system.clone(), PathBuf::from("/bin")])
                .expect("inherited");

        let path = build_caller_first_path(Some(&home), &[system_dir.as_str()], Some(&inherited))
            .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(
            parts,
            vec![caller_node, system, PathBuf::from("/bin"), local, nvm_node]
        );
    }

    #[cfg(unix)]
    #[test]
    fn resolve_command_on_path_returns_first_executable_in_order() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().expect("tmpdir");
        let non_exec = tmp.path().join("a");
        let first = tmp.path().join("b");
        let second = tmp.path().join("c");
        for (dir, mode) in [(&non_exec, 0o644), (&first, 0o755), (&second, 0o755)] {
            fs::create_dir_all(dir).expect("dir");
            let tool = dir.join("tool");
            fs::write(&tool, "#!/bin/sh\n").expect("tool");
            fs::set_permissions(&tool, fs::Permissions::from_mode(mode)).expect("chmod");
        }
        let path = std::env::join_paths([&non_exec, &first, &second]).expect("path");

        assert_eq!(
            resolve_command_on_path("tool", &path),
            Some(first.join("tool"))
        );
        assert_eq!(resolve_command_on_path("missing", &path), None);
    }

    #[test]
    fn test_build_tool_path_prepends_existing_toolchain_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let cargo = home.join(".cargo/bin");
        fs::create_dir_all(&local).expect("local bin");
        fs::create_dir_all(&cargo).expect("cargo bin");

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_tool_path(Some(&home), &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], local);
        assert_eq!(parts[1], cargo);
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
        assert!(parts.contains(&PathBuf::from("/bin")));
        assert!(!parts.contains(&home.join(".kimaki/bin")));
    }

    #[test]
    fn test_tool_path_prepends_nvm_node_bins() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let node_20 = home.join(".nvm/versions/node/v20.0.0/bin");
        let node_24 = home.join(".nvm/versions/node/v24.13.1/bin");
        fs::create_dir_all(&node_20).expect("node 20 bin");
        fs::create_dir_all(&node_24).expect("node 24 bin");
        // A version directory without a bin subdir is skipped, not emitted bare.
        fs::create_dir_all(home.join(".nvm/versions/node/v18.0.0")).expect("node 18");

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_tool_path(Some(&home), &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], node_24);
        assert_eq!(parts[1], node_20);
        assert!(!parts.contains(&home.join(".nvm/versions/node/v18.0.0")));
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn test_tool_path_discovers_only_node_prefixed_local_opt_installs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let node = home.join(".local/opt/node-v24.13.1-linux-x64/bin");
        let other = home.join(".local/opt/other-tool/bin");
        fs::create_dir_all(&node).expect("node bin");
        fs::create_dir_all(&other).expect("other bin");

        let path = build_tool_path(Some(&home), &[], None).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts, vec![node]);
    }

    #[test]
    fn test_node_discovery_follows_home_bin_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let cargo = home.join(".cargo/bin");
        let local_node = home.join(".local/opt/node-v24.13.1-linux-x64/bin");
        let nvm_node = home.join(".nvm/versions/node/v20.0.0/bin");
        for dir in [&local, &cargo, &local_node, &nvm_node] {
            fs::create_dir_all(dir).expect("bin");
        }

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_tool_path(Some(&home), &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], local);
        assert_eq!(parts[1], cargo);
        assert_eq!(parts[2], local_node);
        assert_eq!(
            parts[3], nvm_node,
            "node discovery still precedes system dirs"
        );
    }

    #[test]
    fn test_tool_path_keeps_existing_path_without_home() {
        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_tool_path(None, &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(
            parts,
            vec![PathBuf::from("/usr/bin"), PathBuf::from("/bin")]
        );
    }

    #[test]
    fn test_tool_path_deduplicates_entries() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        fs::create_dir_all(&local).expect("local bin");

        let inherited = OsString::from(local.to_string_lossy().into_owned());
        let path = build_tool_path(Some(&home), &[], Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts, vec![local]);
    }

    #[test]
    fn test_tool_path_appends_existing_absolute_toolchain_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let homebrew = tmp.path().join("opt-homebrew-bin");
        let missing = tmp.path().join("missing-bin");
        fs::create_dir_all(&homebrew).expect("homebrew bin");

        let homebrew_dir = homebrew.to_string_lossy().into_owned();
        let missing_dir = missing.to_string_lossy().into_owned();
        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_tool_path(
            None,
            &[homebrew_dir.as_str(), missing_dir.as_str()],
            Some(&inherited),
        )
        .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], homebrew);
        assert!(!parts.contains(&missing));
    }

    #[test]
    fn test_runner_tool_path_puts_user_tools_before_system_dirs_and_appends_inherited_path() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local_bin = home.join(".local/bin");
        let cargo_bin = home.join(".cargo/bin");
        let kimaki_bin = home.join(".kimaki/bin");
        let local_node = home.join(".local/opt/node-v24.13.1-linux-x64/bin");
        let nvm_node = home.join(".nvm/versions/node/v20.0.0/bin");
        for dir in [&local_bin, &cargo_bin, &kimaki_bin, &local_node, &nvm_node] {
            fs::create_dir_all(dir).expect("bin");
        }
        let inherited_extra = tmp.path().join("inherited/bin");
        let inherited = std::env::join_paths([
            PathBuf::from("/usr/bin"),
            inherited_extra.clone(),
            local_bin.clone(),
        ])
        .expect("inherited");

        let path = runner_tool_path(Some(&home), Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(
            &parts[..5],
            &[
                local_bin.clone(),
                cargo_bin,
                kimaki_bin,
                local_node,
                nvm_node
            ]
        );
        let usr_bin = parts
            .iter()
            .position(|part| part == &PathBuf::from("/usr/bin"))
            .expect("system dir present");
        assert!(usr_bin >= 5, "user tools precede system dirs");
        assert_eq!(
            parts.last(),
            Some(&inherited_extra),
            "inherited PATH appended"
        );
        for entry in [&local_bin, &PathBuf::from("/usr/bin")] {
            assert_eq!(
                parts.iter().filter(|part| *part == entry).count(),
                1,
                "{} deduplicated",
                entry.display()
            );
        }
    }
}
