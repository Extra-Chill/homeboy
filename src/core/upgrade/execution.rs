use crate::core::defaults;
use crate::core::error::{Error, Result};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::helpers::{current_version, version_is_newer};
use super::planning::resolve_binary_on_path;
use super::types::InstallMethod;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ActiveBinaryInfo {
    pub version: Option<String>,
    pub build_identity: Option<String>,
}

pub(crate) fn execute_upgrade(
    method: InstallMethod,
    source_path: Option<&Path>,
    force: bool,
    previous_build_identity: Option<&str>,
) -> Result<(bool, Option<String>, Option<String>)> {
    let defaults = defaults::load_defaults();

    let output = match method {
        InstallMethod::Homebrew => {
            let cmd = &defaults.install_methods.homebrew.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run homebrew upgrade".to_string()))
            })?
        }
        InstallMethod::Secondary => {
            let cmd = &defaults.install_methods.secondary.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run secondary upgrade".to_string()))
            })?
        }
        InstallMethod::Source => {
            let workspace_root = resolve_source_workspace(source_path)?;

            // Execute the upgrade command from defaults
            let cmd = &defaults.install_methods.source.upgrade_command;
            Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&workspace_root)
                .output()
                .map_err(|e| {
                    Error::internal_io(e.to_string(), Some("run source upgrade".to_string()))
                })?
        }
        InstallMethod::Binary => {
            let cmd = &defaults.install_methods.binary.upgrade_command;
            Command::new("sh").args(["-c", cmd]).output().map_err(|e| {
                Error::internal_io(e.to_string(), Some("run binary upgrade".to_string()))
            })?
        }
        InstallMethod::Unknown => {
            return Err(Error::validation_invalid_argument(
                "install_method",
                "Cannot upgrade: unknown installation method",
                None,
                None,
            ));
        }
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let error_detail = if !stderr.trim().is_empty() {
            stderr.trim().to_string()
        } else if !stdout.trim().is_empty() {
            stdout.trim().to_string()
        } else {
            format!("exit code {}", output.status.code().unwrap_or(1))
        };
        return Err(upgrade_failure_error(method, &error_detail));
    }

    let active_binary = active_binary_info().ok().flatten();
    let new_version = active_binary.as_ref().and_then(|info| info.version.clone());
    let new_build_identity = active_binary.and_then(|info| info.build_identity);
    let success = upgrade_verification_result(
        method,
        force,
        current_version(),
        new_version.as_deref(),
        previous_build_identity,
        new_build_identity.as_deref(),
    );

    Ok((success, new_version, new_build_identity))
}

fn upgrade_failure_error(method: InstallMethod, error_detail: &str) -> Error {
    let mut error = Error::internal_io(
        format!("{} upgrade failed: {}", method.as_str(), error_detail),
        Some("execute upgrade".to_string()),
    );

    if method == InstallMethod::Binary && error_detail.contains("404") {
        error = error
            .with_hint("No release asset was found for this Homeboy version.")
            .with_hint("Try: homeboy upgrade --method source --source-path <PATH>");
    } else if method == InstallMethod::Secondary && error_detail.contains("not found") {
        error = error
            .with_hint("Required executable is not installed or is not on PATH.")
            .with_hint(
                "Install the required toolchain, or use: homeboy upgrade --method source --source-path <PATH>",
            );
    }

    error
}

pub(crate) fn resolve_source_workspace(source_path: Option<&Path>) -> Result<PathBuf> {
    let mut candidates = Vec::new();

    if let Some(path) = source_path {
        candidates.push(path.to_path_buf());
    } else {
        if let Ok(current_dir) = std::env::current_dir() {
            candidates.push(current_dir);
        }

        if let Ok(exe_path) = std::env::current_exe() {
            if let Some(workspace_root) = workspace_from_exe_path(&exe_path) {
                candidates.push(workspace_root);
            }
        }
    }

    for candidate in candidates {
        if let Some(checkout) = find_homeboy_source_checkout(&candidate) {
            return Ok(checkout);
        }
    }

    let id = source_path
        .map(|path| path.to_string_lossy().to_string())
        .or_else(|| {
            std::env::current_dir()
                .ok()
                .map(|path| path.to_string_lossy().to_string())
        });

    Err(Error::validation_invalid_argument(
        "source_path",
        "Could not find a Homeboy source workspace for source build",
        id,
        None,
    )
    .with_hint("Run from the Homeboy source workspace, or pass: homeboy upgrade --method source --source-path <PATH>"))
}

fn workspace_from_exe_path(exe_path: &Path) -> Option<PathBuf> {
    let parent = exe_path.parent()?;
    let build_dir = parent.file_name()?.to_string_lossy();
    if build_dir != "release" && build_dir != "debug" {
        return None;
    }

    let target_dir = parent.parent()?;
    if target_dir.file_name()?.to_string_lossy() != "target" {
        return None;
    }

    target_dir.parent().map(Path::to_path_buf)
}

fn is_homeboy_source_checkout(path: &Path) -> bool {
    let manifest = path.join("homeboy.json");
    let Ok(contents) = std::fs::read_to_string(manifest) else {
        return false;
    };

    let is_homeboy_manifest = serde_json::from_str::<serde_json::Value>(&contents)
        .ok()
        .and_then(|value| {
            value
                .get("id")
                .and_then(|id| id.as_str())
                .map(str::to_string)
        })
        .as_deref()
        == Some("homeboy");

    is_homeboy_manifest && is_homeboy_source_package(path)
}

fn is_homeboy_source_package(path: &Path) -> bool {
    let package_manifest = ["Car", "go.toml"].concat();
    let Ok(contents) = std::fs::read_to_string(path.join(package_manifest)) else {
        return false;
    };

    toml::from_str::<toml::Value>(&contents)
        .ok()
        .and_then(|value| {
            value
                .get("package")
                .and_then(|package| package.get("name"))
                .and_then(|name| name.as_str())
                .map(str::to_string)
        })
        .as_deref()
        == Some("homeboy")
}

fn find_homeboy_source_checkout(path: &Path) -> Option<PathBuf> {
    path.ancestors()
        .find(|candidate| is_homeboy_source_checkout(candidate))
        .map(Path::to_path_buf)
}

fn active_binary_info() -> Result<Option<ActiveBinaryInfo>> {
    let exe_path = active_binary_path()?;

    let output = Command::new(exe_path)
        .arg("--version")
        .output()
        .map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("verify active binary version".to_string()),
            )
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    Ok(Some(parse_cli_version_info(&String::from_utf8_lossy(
        &output.stdout,
    ))))
}

fn active_binary_path() -> Result<PathBuf> {
    if let Some(path) = resolve_binary_on_path() {
        return Ok(path);
    }

    std::env::current_exe().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("get current executable path".to_string()),
        )
    })
}

pub(crate) fn upgrade_verification_result(
    method: InstallMethod,
    force: bool,
    previous_version: &str,
    active_version: Option<&str>,
    previous_build_identity: Option<&str>,
    active_build_identity: Option<&str>,
) -> bool {
    let Some(active_version) = active_version else {
        return false;
    };

    if version_is_newer(active_version, previous_version) {
        return true;
    }

    method == InstallMethod::Source
        && force
        && active_version == previous_version
        && previous_build_identity.is_some()
        && active_build_identity.is_some()
        && previous_build_identity != active_build_identity
}

fn parse_cli_version_info(output: &str) -> ActiveBinaryInfo {
    ActiveBinaryInfo {
        version: parse_cli_version_output(output),
        build_identity: parse_cli_build_identity_output(output),
    }
}

fn parse_cli_version_output(output: &str) -> Option<String> {
    let re = regex::Regex::new(r"(\d+\.\d+\.\d+)").ok()?;
    re.find(output).map(|m| m.as_str().to_string())
}

fn parse_cli_build_identity_output(output: &str) -> Option<String> {
    let identity = output.trim();
    if identity.is_empty() || !identity.contains('+') {
        None
    } else {
        Some(identity.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_homeboy_version_output() {
        assert_eq!(
            parse_cli_version_output("homeboy 0.158.0").as_deref(),
            Some("0.158.0")
        );
    }

    #[test]
    fn test_execute_upgrade() {
        assert_eq!(
            parse_cli_version_output("homeboy 0.158.0").as_deref(),
            Some("0.158.0")
        );
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            false,
            "0.157.1",
            Some("0.157.1"),
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
    }

    #[test]
    fn test_upgrade_verification_result() {
        assert!(upgrade_verification_result(
            InstallMethod::Secondary,
            false,
            "0.157.1",
            Some("0.158.0"),
            None,
            None,
        ));
        assert!(!upgrade_verification_result(
            InstallMethod::Secondary,
            true,
            "0.157.1",
            Some("0.157.1"),
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            None,
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
    }

    #[test]
    fn verification_rejects_unchanged_active_binary() {
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            Some("0.157.1"),
            Some("commit same, dirty=false"),
            Some("commit same, dirty=false"),
        ));
    }

    #[test]
    fn verification_accepts_newer_active_binary() {
        assert!(upgrade_verification_result(
            InstallMethod::Secondary,
            false,
            "0.157.1",
            Some("0.158.0"),
            None,
            None,
        ));
    }

    #[test]
    fn verification_rejects_missing_active_binary_version() {
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            None,
            Some("commit old, dirty=false"),
            Some("commit new, dirty=false"),
        ));
    }

    #[test]
    fn forced_source_upgrade_accepts_same_version_with_new_build_identity() {
        assert!(upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            Some("0.157.1"),
            Some("homeboy 0.157.1+old"),
            Some("homeboy 0.157.1+new"),
        ));
    }

    #[test]
    fn forced_source_upgrade_requires_build_identity_for_same_version() {
        assert!(!upgrade_verification_result(
            InstallMethod::Source,
            true,
            "0.157.1",
            Some("0.157.1"),
            None,
            Some("homeboy 0.157.1+new"),
        ));
    }

    #[test]
    fn parses_homeboy_version_output_with_build_identity() {
        let info = parse_cli_version_info("homeboy 0.158.0+abc123-dirty");

        assert_eq!(info.version.as_deref(), Some("0.158.0"));
        assert_eq!(
            info.build_identity.as_deref(),
            Some("homeboy 0.158.0+abc123-dirty")
        );
    }

    #[test]
    fn test_resolve_source_workspace() {
        let dir = checkout_with_package_name("homeboy");

        let resolved = resolve_source_workspace(Some(dir.path())).expect("source checkout");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn source_workspace_rejects_non_homeboy_checkout() {
        let dir = checkout_with_package_name("other");

        let err = resolve_source_workspace(Some(dir.path())).expect_err("invalid checkout");

        assert!(err.message.contains("Homeboy source workspace"));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--source-path")));
    }

    #[test]
    fn source_workspace_accepts_snapshot_without_git_metadata() {
        let dir = source_workspace_with_package_name("homeboy");

        let resolved = resolve_source_workspace(Some(dir.path())).expect("source snapshot");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn source_workspace_resolves_from_nested_checkout_path() {
        let dir = checkout_with_package_name("homeboy");
        let nested = dir.path().join("src/core");
        std::fs::create_dir_all(&nested).expect("nested dir");

        let resolved = resolve_source_workspace(Some(&nested)).expect("source checkout");

        assert_eq!(resolved, dir.path());
    }

    #[test]
    fn executable_workspace_only_resolves_target_build_paths() {
        let path = Path::new("/repo/target/release/homeboy");
        assert_eq!(
            workspace_from_exe_path(path).as_deref(),
            Some(Path::new("/repo"))
        );

        let installed = Path::new("/usr/local/bin/homeboy");
        assert!(workspace_from_exe_path(installed).is_none());
    }

    #[test]
    fn binary_404_upgrade_error_suggests_source_fallback() {
        let err = upgrade_failure_error(
            InstallMethod::Binary,
            "curl: (22) The requested URL returned error: 404",
        );

        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("No release asset")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--source-path")));
    }

    #[test]
    fn missing_tool_upgrade_error_suggests_source_fallback() {
        let err = upgrade_failure_error(
            InstallMethod::Secondary,
            &format!(
                "sh: 1: {}: not found",
                defaults::secondary_install_method_key()
            ),
        );

        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("Required executable")));
        assert!(err
            .hints
            .iter()
            .any(|hint| hint.message.contains("--source-path")));
    }

    fn checkout_with_package_name(package_name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join(".git")).expect("git dir");
        write_source_workspace_files(dir.path(), package_name);
        dir
    }

    fn source_workspace_with_package_name(package_name: &str) -> tempfile::TempDir {
        let dir = tempfile::tempdir().expect("tempdir");
        write_source_workspace_files(dir.path(), package_name);
        dir
    }

    fn write_source_workspace_files(path: &Path, package_name: &str) {
        let manifest = serde_json::json!({ "id": package_name });
        std::fs::write(path.join("homeboy.json"), manifest.to_string()).expect("manifest");
        let package_manifest = ["Car", "go.toml"].concat();
        std::fs::write(
            path.join(package_manifest),
            format!("[package]\nname = \"{package_name}\"\nversion = \"0.0.0\"\n"),
        )
        .expect("package manifest");
    }
}
