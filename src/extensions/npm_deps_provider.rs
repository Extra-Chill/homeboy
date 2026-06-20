use crate::core::component::Component;
use crate::core::deps::{DependencyCommandResult, DependencyPackage, DependencyUpdateResult};
use crate::core::{Error, Result};
use crate::extensions::deps_provider::ProviderDependencyStatus;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

pub fn npm_command_args(package: &str, constraint: Option<&str>) -> Vec<String> {
    match constraint {
        Some(constraint) => vec!["install".to_string(), format!("{package}@{constraint}")],
        None => vec!["update".to_string(), package.to_string()],
    }
}

pub(crate) struct NpmDependencyProvider;

impl NpmDependencyProvider {
    pub(crate) fn supports(path: &Path) -> bool {
        path.join("package.json").is_file()
    }

    pub(crate) fn status(
        &self,
        path: &Path,
        package_filter: Option<&str>,
    ) -> Result<ProviderDependencyStatus> {
        Ok(ProviderDependencyStatus {
            package_manager: "npm".to_string(),
            dependency_identities: npm_identities(path)?,
            packages: read_npm_packages(path, package_filter)?,
        })
    }

    pub(crate) fn manages_package(&self, path: &Path, package: &str) -> Result<bool> {
        Ok(npm_package_snapshot(path, package)?.is_some())
    }

    pub(crate) fn update(
        &self,
        component: &Component,
        path: &Path,
        package: &str,
        constraint: Option<&str>,
    ) -> Result<DependencyUpdateResult> {
        let before = npm_package_snapshot(path, package)?;
        let args = npm_command_args(package, constraint);
        let output = Command::new("npm")
            .args(&args)
            .current_dir(path)
            .output()
            .map_err(|e| {
                Error::internal_io(e.to_string(), Some("run dependency provider".to_string()))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "dependency_provider",
                format!(
                    "Dependency provider command failed with status {}: {}",
                    output.status,
                    first_non_empty_line(&stderr)
                        .or_else(|| first_non_empty_line(&stdout))
                        .unwrap_or("no output")
                ),
                None,
                Some(vec![format!(
                    "Run manually in {}: npm {}",
                    path.display(),
                    args.join(" ")
                )]),
            ));
        }

        let after = npm_package_snapshot(path, package)?;

        Ok(DependencyUpdateResult {
            component_id: component.id.clone(),
            component_path: path.display().to_string(),
            package_manager: "npm".to_string(),
            package: package.to_string(),
            requested_constraint: constraint.map(str::to_string),
            command: std::iter::once("npm".to_string()).chain(args).collect(),
            before,
            after,
            stdout,
            stderr,
            install: None,
            rebuild: None,
        })
    }

    pub(crate) fn install(
        &self,
        _component: &Component,
        path: &Path,
    ) -> Result<Option<DependencyCommandResult>> {
        let install = npm_install_command(path);
        let output = Command::new(&install.binary)
            .args(&install.args)
            .current_dir(&install.cwd)
            .output()
            .map_err(|e| {
                Error::internal_io(e.to_string(), Some("run dependency provider".to_string()))
            })?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let status = output.status.code();
        if !output.status.success() {
            return Err(Error::validation_invalid_argument(
                "dependency_provider",
                format!(
                    "Dependency provider install failed with status {}: {}",
                    output.status,
                    first_non_empty_line(&stderr)
                        .or_else(|| first_non_empty_line(&stdout))
                        .unwrap_or("no output")
                ),
                None,
                Some(vec![format!(
                    "Run manually in {}: {} {}",
                    install.cwd.display(),
                    install.binary,
                    install.args.join(" ")
                )]),
            ));
        }

        Ok(Some(DependencyCommandResult {
            command: std::iter::once(install.binary).chain(install.args).collect(),
            skipped: false,
            status,
            stdout,
            stderr,
        }))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct NpmInstallCommand {
    binary: String,
    args: Vec<String>,
    cwd: PathBuf,
}

fn npm_install_command(path: &Path) -> NpmInstallCommand {
    if let Some(root) = find_pnpm_root(path) {
        return NpmInstallCommand {
            binary: "pnpm".to_string(),
            args: vec!["install".to_string(), "--frozen-lockfile".to_string()],
            cwd: root,
        };
    }

    let args = if path.join("package-lock.json").is_file() {
        vec!["ci".to_string()]
    } else {
        vec!["install".to_string()]
    };

    NpmInstallCommand {
        binary: "npm".to_string(),
        args,
        cwd: path.to_path_buf(),
    }
}

fn find_pnpm_root(path: &Path) -> Option<PathBuf> {
    let mut current = Some(path);
    while let Some(dir) = current {
        if dir.join("pnpm-workspace.yaml").is_file() || dir.join("pnpm-lock.yaml").is_file() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

fn npm_package_snapshot(path: &Path, package: &str) -> Result<Option<DependencyPackage>> {
    Ok(read_npm_packages(path, Some(package))?.into_iter().next())
}

fn npm_identities(path: &Path) -> Result<Vec<String>> {
    let manifest = read_json_file(&path.join("package.json"))?;
    Ok(manifest
        .get("name")
        .and_then(Value::as_str)
        .map(|name| vec![name.to_string()])
        .unwrap_or_default())
}

fn read_npm_packages(path: &Path, package_filter: Option<&str>) -> Result<Vec<DependencyPackage>> {
    let manifest = read_json_file(&path.join("package.json"))?;
    let lock = read_optional_json_file(&path.join("package-lock.json"))?;
    let mut direct = BTreeMap::new();

    for section in [
        "dependencies",
        "devDependencies",
        "optionalDependencies",
        "peerDependencies",
    ] {
        collect_manifest_section(&manifest, section, &mut direct);
    }

    let locked = lock
        .as_ref()
        .map(collect_npm_locked_packages)
        .unwrap_or_default();

    let mut names: BTreeSet<String> = direct.keys().cloned().collect();
    names.extend(locked.keys().cloned());

    let packages = names
        .into_iter()
        .filter(|name| package_filter.map(|filter| filter == name).unwrap_or(true))
        .map(|name| {
            let (manifest_section, constraint) = direct
                .get(&name)
                .cloned()
                .map(|(section, constraint)| (Some(section), Some(constraint)))
                .unwrap_or((None, None));
            let locked = locked.get(&name);
            DependencyPackage {
                name,
                manifest_section,
                constraint,
                locked_version: locked.and_then(|p| p.version.clone()),
                locked_reference: locked.and_then(|p| p.reference.clone()),
            }
        })
        .collect();

    Ok(packages)
}

fn collect_manifest_section(
    manifest: &Value,
    section: &str,
    direct: &mut BTreeMap<String, (String, String)>,
) {
    let Some(entries) = manifest.get(section).and_then(Value::as_object) else {
        return;
    };

    for (name, constraint) in entries {
        if let Some(constraint) = constraint.as_str() {
            direct.insert(name.clone(), (section.to_string(), constraint.to_string()));
        }
    }
}

#[derive(Debug, Clone, Default)]
struct LockedPackage {
    version: Option<String>,
    reference: Option<String>,
}

fn collect_npm_locked_packages(lock: &Value) -> BTreeMap<String, LockedPackage> {
    let mut packages = BTreeMap::new();

    if let Some(entries) = lock.get("packages").and_then(Value::as_object) {
        for (path, entry) in entries {
            let Some(name) = npm_lock_package_name(path, entry) else {
                continue;
            };
            packages.insert(name, npm_locked_package(entry, entries));
        }
    }

    if let Some(entries) = lock.get("dependencies").and_then(Value::as_object) {
        for (name, entry) in entries {
            packages
                .entry(name.to_string())
                .or_insert_with(|| npm_locked_package(entry, &serde_json::Map::new()));
        }
    }

    packages
}

fn npm_lock_package_name(path: &str, entry: &Value) -> Option<String> {
    if path.is_empty() {
        return None;
    }
    entry
        .get("name")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            path.strip_prefix("node_modules/")
                .or_else(|| {
                    path.rsplit_once("/node_modules/")
                        .map(|(_, package)| package)
                })
                .map(str::to_string)
        })
}

fn npm_locked_package(entry: &Value, packages: &serde_json::Map<String, Value>) -> LockedPackage {
    let resolved_entry = entry
        .get("resolved")
        .and_then(Value::as_str)
        .and_then(|resolved| packages.get(resolved));

    LockedPackage {
        version: entry
            .get("version")
            .or_else(|| resolved_entry.and_then(|entry| entry.get("version")))
            .and_then(Value::as_str)
            .map(str::to_string),
        reference: entry
            .get("resolved")
            .or_else(|| entry.get("integrity"))
            .and_then(Value::as_str)
            .map(str::to_string),
    }
}

fn read_json_file(path: &Path) -> Result<Value> {
    let raw = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(path.display().to_string())))?;
    serde_json::from_str(&raw)
        .map_err(|e| Error::validation_invalid_json(e, Some(path.display().to_string()), Some(raw)))
}

fn read_optional_json_file(path: &Path) -> Result<Option<Value>> {
    if !path.exists() {
        return Ok(None);
    }
    read_json_file(path).map(Some)
}

fn first_non_empty_line(output: &str) -> Option<&str> {
    output.lines().find(|line| !line.trim().is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn npm_install_installs_full_dependency_tree_for_build_lifecycle() {
        let _guard = crate::test_support::home_env_guard();
        let old_path = std::env::var("PATH").unwrap_or_default();
        let bin = tempfile::tempdir().expect("bin tempdir");
        let project = tempfile::tempdir().expect("project tempdir");
        let npm = bin.path().join("npm");
        std::fs::write(&npm, "#!/bin/sh\nprintf '%s\n' \"$@\" > npm-args.txt\n").expect("fake npm");
        let mode = std::fs::metadata(&npm)
            .expect("fake npm metadata")
            .permissions();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mut mode = mode;
            mode.set_mode(0o755);
            std::fs::set_permissions(&npm, mode).expect("chmod fake npm");
        }
        std::env::set_var("PATH", format!("{}:{old_path}", bin.path().display()));
        std::fs::write(project.path().join("package.json"), "{}").expect("package json");

        let result = NpmDependencyProvider
            .install(&Component::default(), project.path())
            .expect("npm install");

        std::env::set_var("PATH", old_path);
        let result = result.expect("install result");
        assert_eq!(result.command, vec!["npm", "install"]);
        assert_eq!(
            std::fs::read_to_string(project.path().join("npm-args.txt")).unwrap(),
            "install\n"
        );
    }

    #[test]
    fn npm_install_command_uses_pnpm_workspace_root_above_package() {
        let workspace = tempfile::tempdir().expect("workspace tempdir");
        let plugin = workspace.path().join("plugins/woocommerce");
        std::fs::create_dir_all(&plugin).expect("plugin dir");
        std::fs::write(workspace.path().join("pnpm-workspace.yaml"), "packages:\n  - plugins/*\n")
            .expect("workspace file");
        std::fs::write(workspace.path().join("pnpm-lock.yaml"), "lockfileVersion: '9.0'\n")
            .expect("lock file");
        std::fs::write(plugin.join("package.json"), "{}").expect("package json");

        let command = npm_install_command(&plugin);

        assert_eq!(command.binary, "pnpm");
        assert_eq!(
            command.args,
            vec!["install".to_string(), "--frozen-lockfile".to_string()]
        );
        assert_eq!(command.cwd, workspace.path());
    }

    #[test]
    fn npm_install_command_keeps_npm_for_plain_package() {
        let project = tempfile::tempdir().expect("project tempdir");
        std::fs::write(project.path().join("package.json"), "{}").expect("package json");

        let command = npm_install_command(project.path());

        assert_eq!(command.binary, "npm");
        assert_eq!(command.args, vec!["install".to_string()]);
        assert_eq!(command.cwd, project.path());
    }
}
