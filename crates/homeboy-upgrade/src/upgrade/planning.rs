use std::collections::HashSet;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

/// Resolve the `homeboy` binary via $PATH, returning the first match that
/// exists on disk. This avoids the stale `/proc/self/exe` problem on Linux
/// where the old inode is deleted after the upgrade replaces the binary.
pub fn resolve_binary_on_path() -> Option<PathBuf> {
    let path_var = std::env::var("PATH").ok()?;
    resolve_binary_on_path_var(&path_var)
}

pub(crate) fn resolve_binary_on_path_var(path_var: &str) -> Option<PathBuf> {
    for dir in path_var.split(':') {
        let candidate = PathBuf::from(dir).join("homeboy");
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

/// Describe PATH lookup independently of the controller selected for upgrade.
/// Do not run other binaries: shadowing is about PATH order, not version order.
pub(super) fn path_shadowing_warning(path: &OsStr) -> Option<String> {
    let binary_name = format!("homeboy{}", std::env::consts::EXE_SUFFIX);
    let mut seen = HashSet::new();
    let mut installs = std::env::split_paths(path)
        .map(|dir| dir.join(&binary_name))
        .filter(|candidate| {
            is_executable_file(candidate)
                && candidate
                    .canonicalize()
                    .is_ok_and(|target| seen.insert(target))
        });
    let first = installs.next()?;
    let shadowed: Vec<_> = installs
        .map(|path| format!("`{}`", path.display()))
        .collect();
    if shadowed.is_empty() {
        return None;
    }

    Some(format!(
        "WARNING: Homeboy PATH shadowing: `{}` is first on PATH and shadows {}. Use the intended binary's full path or adjust PATH order.",
        first.display(),
        shadowed.join(", ")
    ))
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = path.metadata() else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn path_shadowing_warning_names_first_executable_and_later_install() {
        let root = tempfile::tempdir().unwrap();
        let brew = root.path().join("homebrew/bin");
        let cargo = root.path().join(".cargo/bin");
        let cellar = root.path().join("Cellar/homeboy");
        for dir in [&brew, &cargo, &cellar] {
            fs::create_dir_all(dir).unwrap();
        }
        let brew_binary = brew.join("homeboy");
        let cargo_binary = cargo.join("homeboy");
        let cellar_binary = cellar.join("homeboy");
        for binary in [&cargo_binary, &cellar_binary] {
            fs::write(binary, "#!/bin/sh\nexit 1\n").unwrap();
            fs::set_permissions(binary, fs::Permissions::from_mode(0o755)).unwrap();
        }
        symlink(&cellar_binary, &brew_binary).unwrap();

        let path = std::env::join_paths([&brew, &cargo, &cellar, &cargo]).unwrap();
        let warning = path_shadowing_warning(&path).unwrap();
        assert!(warning.contains(&format!(
            "`{}` is first on PATH and shadows `{}`.",
            brew_binary.display(),
            cargo_binary.display()
        )));
    }

    #[test]
    fn path_shadowing_warning_ignores_aliases_and_non_executables() {
        let root = tempfile::tempdir().unwrap();
        let install = root.path().join("bin");
        let alias = root.path().join("alias");
        let nonexec = root.path().join("nonexec");
        let missing = root.path().join("missing");
        for dir in [&install, &alias, &nonexec, &missing] {
            fs::create_dir(dir).unwrap();
        }
        let binary = install.join("homeboy");
        fs::write(&binary, "#!/bin/sh\n").unwrap();
        fs::set_permissions(&binary, fs::Permissions::from_mode(0o755)).unwrap();
        symlink(&binary, alias.join("homeboy")).unwrap();
        let nonexec_binary = nonexec.join("homeboy");
        fs::write(&nonexec_binary, "not executable\n").unwrap();
        fs::set_permissions(nonexec_binary, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(root.path().join("gone"), missing.join("homeboy")).unwrap();

        let path = std::env::join_paths([&nonexec, &missing, &install, &install, &alias]).unwrap();
        assert!(path_shadowing_warning(&path).is_none());
    }
}
