//! Toolchain environment helpers for rig command steps.
//!
//! The set of bin directories a rig's `command` steps should see is *policy*,
//! not orchestrator behavior, so it is expressed declaratively on the rig spec
//! (`RigSpec::toolchain`). A rig that declares nothing gets
//! [`builtin_default_spec`], which reproduces Homeboy's historical hardcoded
//! list exactly.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::path::{Path, PathBuf};

use crate::spec::{PathDiscoverySort, PathDiscoverySpec, RigSpec, ToolchainSpec};

/// Homeboy's built-in toolchain discovery, used when a rig declares no
/// `toolchain` of its own.
///
/// These entries are language- and product-specific (`.cargo/bin` is Rust,
/// `.nvm/versions/node` is Node, `.kimaki/bin` is one particular third-party
/// product) and do not belong in a generic orchestrator. They stay here only
/// because removing them would silently break every host that depends on
/// today's behavior. `RigSpec::toolchain` is the migration path: move these
/// into host/rig configuration, then shrink this default.
pub fn builtin_default_spec() -> ToolchainSpec {
    ToolchainSpec {
        prepend_paths: vec![
            "~/.local/bin".to_string(),
            "~/.cargo/bin".to_string(),
            "~/.kimaki/bin".to_string(),
        ],
        discover: vec![PathDiscoverySpec {
            root: "~/.nvm/versions/node".to_string(),
            glob: None,
            bin_subdir: Some("bin".to_string()),
            sort: PathDiscoverySort::Descending,
        }],
        append_paths: vec![
            "/opt/homebrew/bin".to_string(),
            "/usr/local/bin".to_string(),
        ],
    }
}

/// Builds the PATH for rig `command` steps.
///
/// `rig` supplies the toolchain declaration; `None` (or a rig without a
/// `toolchain` field) uses [`builtin_default_spec`]. Declared directories are
/// prepended before the inherited PATH; missing ones are skipped so the result
/// stays portable across hosts.
pub fn command_step_path(rig: Option<&RigSpec>) -> Option<OsString> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let existing_path = std::env::var_os("PATH");
    build_command_step_path(rig, home.as_deref(), existing_path.as_deref())
}

pub(crate) fn build_command_step_path(
    rig: Option<&RigSpec>,
    home: Option<&Path>,
    existing_path: Option<&OsStr>,
) -> Option<OsString> {
    match rig.and_then(|rig| rig.toolchain.as_ref()) {
        // A rig-declared spec is expanded with the full rig vocabulary
        // (`~`, `${env.NAME}`, `${components.<id>.path}`, `${package.root}`).
        Some(spec) => {
            let rig = rig.expect("toolchain spec implies a rig");
            let resolve = |raw: &str| {
                let expanded = crate::expand::expand_vars(rig, raw);
                // An unresolved `~` means no home is known. Skip rather than
                // emitting a literal `~` directory that can never exist.
                if expanded.starts_with('~') {
                    None
                } else {
                    Some(PathBuf::from(expanded))
                }
            };
            join_paths(collect_paths(spec, &resolve), existing_path)
        }
        None => build_command_step_path_with_spec(&builtin_default_spec(), home, existing_path),
    }
}

/// Builds a PATH from an explicit spec with `~` resolved against `home`.
///
/// The built-in default path and the test seam both route through here so the
/// default is exercised as a real spec rather than a parallel code path.
pub(crate) fn build_command_step_path_with_spec(
    spec: &ToolchainSpec,
    home: Option<&Path>,
    existing_path: Option<&OsStr>,
) -> Option<OsString> {
    let resolve = |raw: &str| resolve_home_relative(home, raw);
    join_paths(collect_paths(spec, &resolve), existing_path)
}

fn collect_paths(spec: &ToolchainSpec, resolve: &dyn Fn(&str) -> Option<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    let mut paths = Vec::new();

    for raw in &spec.prepend_paths {
        if let Some(path) = resolve(raw.as_str()) {
            push_existing_path(&mut paths, &mut seen, path);
        }
    }

    for discovery in &spec.discover {
        if let Some(root) = resolve(discovery.root.as_str()) {
            push_discovered_paths(&mut paths, &mut seen, &root, discovery);
        }
    }

    for raw in &spec.append_paths {
        if let Some(path) = resolve(raw.as_str()) {
            push_existing_path(&mut paths, &mut seen, path);
        }
    }

    paths
}

fn join_paths(mut paths: Vec<PathBuf>, existing_path: Option<&OsStr>) -> Option<OsString> {
    let mut seen = paths.iter().cloned().collect::<HashSet<_>>();
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

/// Resolves a spec entry against an explicit home directory.
///
/// `~`-relative entries are dropped when no home is known, matching the
/// historical behavior where home bin dirs were skipped without `HOME`.
fn resolve_home_relative(home: Option<&Path>, raw: &str) -> Option<PathBuf> {
    let Some(relative) = raw.strip_prefix('~') else {
        return Some(PathBuf::from(raw));
    };
    let relative = relative.strip_prefix('/').unwrap_or(relative);
    let home = home?;
    if relative.is_empty() {
        return Some(home.to_path_buf());
    }
    Some(home.join(relative))
}

fn push_existing_path(paths: &mut Vec<PathBuf>, seen: &mut HashSet<PathBuf>, path: PathBuf) {
    if path.exists() {
        push_path(paths, seen, path);
    }
}

fn push_discovered_paths(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<PathBuf>,
    root: &Path,
    discovery: &PathDiscoverySpec,
) {
    let Ok(entries) = fs::read_dir(root) else {
        return;
    };

    let mut discovered = entries
        .filter_map(|entry| entry.ok())
        .filter(|entry| match discovery.glob.as_deref() {
            Some(glob) => wildcard_match(glob, &entry.file_name().to_string_lossy()),
            None => true,
        })
        .map(|entry| match discovery.bin_subdir.as_deref() {
            Some(bin_subdir) => entry.path().join(bin_subdir),
            None => entry.path(),
        })
        .filter(|path| path.exists())
        .collect::<Vec<_>>();

    match discovery.sort {
        PathDiscoverySort::Ascending => discovered.sort(),
        PathDiscoverySort::Descending => {
            discovered.sort();
            discovered.reverse();
        }
        PathDiscoverySort::Unsorted => {}
    }

    for path in discovered {
        push_path(paths, seen, path);
    }
}

/// Minimal `*`-wildcard matcher. `?` and character classes are intentionally
/// unsupported: discovery filters version directory names, not arbitrary trees.
fn wildcard_match(pattern: &str, value: &str) -> bool {
    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == value;
    }
    let first = parts[0];
    let last = parts[parts.len() - 1];
    if !value.starts_with(first)
        || value.len() < first.len() + last.len()
        || !value[first.len()..].ends_with(last)
    {
        return false;
    }

    let mut remainder = &value[first.len()..value.len() - last.len()];
    for part in parts[1..parts.len() - 1].iter().copied() {
        let Some(index) = remainder.find(part) else {
            return false;
        };
        remainder = &remainder[index + part.len()..];
    }
    true
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
        build_command_step_path, build_command_step_path_with_spec, builtin_default_spec,
        wildcard_match,
    };
    use crate::spec::{PathDiscoverySort, PathDiscoverySpec, RigSpec, ToolchainSpec};

    /// The built-in default without its absolute entries, so assertions do not
    /// depend on whether the host actually has `/usr/local/bin`.
    fn default_spec_without_absolute_dirs() -> ToolchainSpec {
        ToolchainSpec {
            append_paths: Vec::new(),
            ..builtin_default_spec()
        }
    }

    #[test]
    fn test_build_command_step_path_prepends_existing_toolchain_dirs() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let cargo = home.join(".cargo/bin");
        fs::create_dir_all(&local).expect("local bin");
        fs::create_dir_all(&cargo).expect("cargo bin");

        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path_with_spec(
            &default_spec_without_absolute_dirs(),
            Some(&home),
            Some(&inherited),
        )
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
        let path = build_command_step_path_with_spec(
            &default_spec_without_absolute_dirs(),
            Some(&home),
            Some(&inherited),
        )
        .expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], node_24);
        assert_eq!(parts[1], node_20);
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
    }

    #[test]
    fn test_command_step_path_keeps_existing_path_without_home() {
        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path_with_spec(
            &default_spec_without_absolute_dirs(),
            None,
            Some(&inherited),
        )
        .expect("path");
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
        let path = build_command_step_path_with_spec(
            &default_spec_without_absolute_dirs(),
            Some(&home),
            Some(&inherited),
        )
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

        let spec = ToolchainSpec {
            prepend_paths: Vec::new(),
            discover: Vec::new(),
            append_paths: vec![
                homebrew.to_string_lossy().into_owned(),
                missing.to_string_lossy().into_owned(),
            ],
        };
        let inherited = OsString::from("/usr/bin:/bin");
        let path = build_command_step_path_with_spec(&spec, None, Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], homebrew);
        assert!(!parts.contains(&missing));
    }

    /// A rig authored before `toolchain` existed must see the exact PATH it saw
    /// before: home bin dirs in their historical order, then version-manager
    /// discovery, then the inherited PATH.
    #[test]
    fn test_rig_without_toolchain_field_keeps_builtin_default() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let local = home.join(".local/bin");
        let cargo = home.join(".cargo/bin");
        let node = home.join(".nvm/versions/node/v20.0.0/bin");
        for dir in [&local, &cargo, &node] {
            fs::create_dir_all(dir).expect("bin");
        }
        let inherited = OsString::from("/usr/bin:/bin");

        let rig = RigSpec {
            id: "no-toolchain".to_string(),
            ..Default::default()
        };
        assert!(rig.toolchain.is_none(), "fixture declares no toolchain");

        let path =
            build_command_step_path(Some(&rig), Some(&home), Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], local);
        assert_eq!(parts[1], cargo);
        assert_eq!(parts[2], node, "nvm discovery still precedes system dirs");
        assert!(parts.contains(&PathBuf::from("/usr/bin")));
        assert!(parts.contains(&PathBuf::from("/bin")));
        assert!(
            !parts.contains(&home.join(".kimaki/bin")),
            "missing default dirs are still skipped"
        );

        let absent = build_command_step_path(None, Some(&home), Some(&inherited));
        assert_eq!(
            absent,
            Some(path),
            "no rig at all resolves to the same built-in default"
        );
    }

    #[test]
    fn test_declared_prepend_paths_come_first() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let home = tmp.path().join("home");
        let declared = tmp.path().join("declared-bin");
        let local = home.join(".local/bin");
        fs::create_dir_all(&declared).expect("declared bin");
        fs::create_dir_all(&local).expect("local bin");

        let rig = RigSpec {
            id: "declared".to_string(),
            toolchain: Some(ToolchainSpec {
                prepend_paths: vec![declared.to_string_lossy().into_owned()],
                ..Default::default()
            }),
            ..Default::default()
        };
        let inherited = OsString::from("/usr/bin:/bin");
        let path =
            build_command_step_path(Some(&rig), Some(&home), Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(parts[0], declared);
        assert!(
            !parts.contains(&local),
            "a declared toolchain replaces the built-in default rather than extending it"
        );
    }

    #[test]
    fn test_declared_discovery_resolves_versioned_bin_dirs_in_sort_order() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().join("versions");
        let v1 = root.join("v1.0.0/bin");
        let v2 = root.join("v2.0.0/bin");
        let v3 = root.join("v3.0.0/bin");
        let ignored = root.join("not-a-version/bin");
        for dir in [&v1, &v2, &v3, &ignored] {
            fs::create_dir_all(dir).expect("version bin");
        }
        // A matching entry with no bin subdir must be skipped, not emitted bare.
        fs::create_dir_all(root.join("v4.0.0")).expect("version without bin");

        let discovery = PathDiscoverySpec {
            root: root.to_string_lossy().into_owned(),
            glob: Some("v*".to_string()),
            bin_subdir: Some("bin".to_string()),
            sort: PathDiscoverySort::Descending,
        };
        let rig = |sort| RigSpec {
            id: "discover".to_string(),
            toolchain: Some(ToolchainSpec {
                discover: vec![PathDiscoverySpec {
                    sort,
                    ..discovery.clone()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let inherited = OsString::from("/usr/bin");

        let descending = rig(PathDiscoverySort::Descending);
        let path =
            build_command_step_path(Some(&descending), None, Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();
        assert_eq!(parts[0], v3);
        assert_eq!(parts[1], v2);
        assert_eq!(parts[2], v1);
        assert!(
            !parts.contains(&ignored),
            "glob filters non-matching entries"
        );
        assert!(!parts.contains(&root.join("v4.0.0")));

        let ascending = rig(PathDiscoverySort::Ascending);
        let path = build_command_step_path(Some(&ascending), None, Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();
        assert_eq!(parts[0], v1);
        assert_eq!(parts[1], v2);
        assert_eq!(parts[2], v3);
    }

    #[test]
    fn test_declared_append_paths_follow_discovery() {
        let tmp = tempfile::tempdir().expect("tmpdir");
        let root = tmp.path().join("versions");
        let v1 = root.join("v1.0.0/bin");
        let appended = tmp.path().join("appended-bin");
        let prepended = tmp.path().join("prepended-bin");
        for dir in [&v1, &appended, &prepended] {
            fs::create_dir_all(dir).expect("bin");
        }

        let rig = RigSpec {
            id: "ordered".to_string(),
            toolchain: Some(ToolchainSpec {
                prepend_paths: vec![prepended.to_string_lossy().into_owned()],
                discover: vec![PathDiscoverySpec {
                    root: root.to_string_lossy().into_owned(),
                    glob: None,
                    bin_subdir: Some("bin".to_string()),
                    sort: PathDiscoverySort::Descending,
                }],
                append_paths: vec![appended.to_string_lossy().into_owned()],
            }),
            ..Default::default()
        };
        let inherited = OsString::from("/usr/bin");
        let path = build_command_step_path(Some(&rig), None, Some(&inherited)).expect("path");
        let parts = std::env::split_paths(&path).collect::<Vec<_>>();

        assert_eq!(
            parts,
            vec![prepended, v1, appended, PathBuf::from("/usr/bin")]
        );
    }

    #[test]
    fn test_wildcard_match() {
        assert!(wildcard_match("v*", "v20.0.0"));
        assert!(!wildcard_match("v*", "node-20"));
        assert!(wildcard_match("*", "anything"));
        assert!(wildcard_match("v*.0.0", "v20.0.0"));
        assert!(!wildcard_match("v*.0.0", "v20.1.2"));
        assert!(wildcard_match("exact", "exact"));
        assert!(!wildcard_match("exact", "exactly"));
    }
}
