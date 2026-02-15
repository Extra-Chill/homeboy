use crate::defaults;
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const CRATES_IO_API: &str = "https://crates.io/api/v1/crates/homeboy";
const GITHUB_RELEASES_API: &str =
    "https://api.github.com/repos/Extra-Chill/homeboy/releases/latest";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InstallMethod {
    Homebrew,
    Cargo,
    Source,
    /// Downloaded release binary (e.g. ~/bin/homeboy, /usr/local/bin/homeboy)
    Binary,
    Unknown,
}

impl InstallMethod {
    pub fn as_str(&self) -> &'static str {
        match self {
            InstallMethod::Homebrew => "homebrew",
            InstallMethod::Cargo => "cargo",
            InstallMethod::Source => "source",
            InstallMethod::Binary => "binary",
            InstallMethod::Unknown => "unknown",
        }
    }

    pub fn upgrade_instructions(&self) -> String {
        let defaults = defaults::load_defaults();
        match self {
            InstallMethod::Homebrew => defaults.install_methods.homebrew.upgrade_command.clone(),
            InstallMethod::Cargo => defaults.install_methods.cargo.upgrade_command.clone(),
            InstallMethod::Source => defaults.install_methods.source.upgrade_command.clone(),
            InstallMethod::Binary => defaults.install_methods.binary.upgrade_command.clone(),
            InstallMethod::Unknown => "Please reinstall using Homebrew, Cargo, or a release binary".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct VersionCheck {
    pub command: String,
    pub current_version: String,
    pub latest_version: Option<String>,
    pub update_available: bool,
    pub install_method: InstallMethod,
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct UpgradeResult {
    pub command: String,
    pub install_method: InstallMethod,
    pub previous_version: String,
    pub new_version: Option<String>,
    pub upgraded: bool,
    pub message: String,
    pub restart_required: bool,
}

#[derive(Deserialize)]
struct CratesIoResponse {
    #[serde(rename = "crate")]
    crate_info: CrateInfo,
}

#[derive(Deserialize)]
struct CrateInfo {
    newest_version: String,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

pub fn current_version() -> &'static str {
    VERSION
}

fn fetch_latest_crates_io_version() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::other(format!("Failed to create HTTP client: {}", e)))?;

    let response: CratesIoResponse = client
        .get(CRATES_IO_API)
        .send()
        .map_err(|e| Error::other(format!("Failed to query crates.io: {}", e)))?
        .json()
        .map_err(|e| Error::other(format!("Failed to parse crates.io response: {}", e)))?;

    Ok(response.crate_info.newest_version)
}

fn fetch_latest_github_version() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::other(format!("Failed to create HTTP client: {}", e)))?;

    let response: GitHubRelease = client
        .get(GITHUB_RELEASES_API)
        .send()
        .map_err(|e| Error::other(format!("Failed to query GitHub releases: {}", e)))?
        .json()
        .map_err(|e| Error::other(format!("Failed to parse GitHub release response: {}", e)))?;

    // Strip "v" prefix if present (e.g., "v0.15.0" -> "0.15.0")
    let version = response
        .tag_name
        .strip_prefix('v')
        .unwrap_or(&response.tag_name);
    Ok(version.to_string())
}

pub fn fetch_latest_version(method: InstallMethod) -> Result<String> {
    match method {
        InstallMethod::Cargo => fetch_latest_crates_io_version(),
        InstallMethod::Homebrew
        | InstallMethod::Source
        | InstallMethod::Binary
        | InstallMethod::Unknown => fetch_latest_github_version(),
    }
}

pub fn detect_install_method() -> InstallMethod {
    let exe_path = match std::env::current_exe() {
        Ok(path) => path.to_string_lossy().to_string(),
        Err(_) => return InstallMethod::Unknown,
    };

    let defaults = defaults::load_defaults();

    // Check for Homebrew installation via path patterns
    for pattern in &defaults.install_methods.homebrew.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Homebrew;
        }
    }

    // Alternative Homebrew check: brew list (if list_command configured)
    if let Some(list_cmd) = &defaults.install_methods.homebrew.list_command {
        let parts: Vec<&str> = list_cmd.split_whitespace().collect();
        if let Some((cmd, args)) = parts.split_first() {
            if Command::new(cmd)
                .args(args)
                .output()
                .map(|o| o.status.success())
                .unwrap_or(false)
            {
                return InstallMethod::Homebrew;
            }
        }
    }

    // Check for Cargo installation via path patterns
    for pattern in &defaults.install_methods.cargo.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Cargo;
        }
    }

    // Check for source installation via path patterns
    for pattern in &defaults.install_methods.source.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Source;
        }
    }

    // Check for downloaded release binary via path patterns
    for pattern in &defaults.install_methods.binary.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Binary;
        }
    }

    InstallMethod::Unknown
}

pub fn check_for_updates() -> Result<VersionCheck> {
    let install_method = detect_install_method();
    let current = current_version().to_string();

    let latest = fetch_latest_version(install_method).ok();
    let update_available = latest
        .as_ref()
        .map(|l| version_is_newer(l, &current))
        .unwrap_or(false);

    Ok(VersionCheck {
        command: "upgrade.check".to_string(),
        current_version: current,
        latest_version: latest,
        update_available,
        install_method,
    })
}

fn version_is_newer(latest: &str, current: &str) -> bool {
    let parse = |v: &str| -> Option<(u32, u32, u32)> {
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() >= 3 {
            Some((
                parts[0].parse().ok()?,
                parts[1].parse().ok()?,
                parts[2].parse().ok()?,
            ))
        } else {
            None
        }
    };

    match (parse(latest), parse(current)) {
        (Some(l), Some(c)) => l > c,
        _ => latest != current,
    }
}

pub fn run_upgrade(force: bool) -> Result<UpgradeResult> {
    run_upgrade_with_method(force, None)
}

pub fn run_upgrade_with_method(
    force: bool,
    method_override: Option<InstallMethod>,
) -> Result<UpgradeResult> {
    let install_method = method_override.unwrap_or_else(detect_install_method);
    let previous_version = current_version().to_string();

    if install_method == InstallMethod::Unknown {
        return Err(Error::validation_invalid_argument(
            "install_method",
            "Could not detect installation method",
            None,
            None,
        )
        .with_hint("Try: homeboy upgrade --method binary")
        .with_hint("Or reinstall using: brew install homeboy")
        .with_hint("Or: cargo install homeboy"));
    }

    // Check if update is available (unless forcing)
    if !force {
        let check = check_for_updates()?;
        if !check.update_available {
            return Ok(UpgradeResult {
                command: "upgrade".to_string(),
                install_method,
                previous_version: previous_version.clone(),
                new_version: Some(previous_version),
                upgraded: false,
                message: "Already at latest version".to_string(),
                restart_required: false,
            });
        }
    }

    // Execute the upgrade
    let (success, new_version) = execute_upgrade(install_method)?;

    Ok(UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        previous_version,
        new_version: new_version.clone(),
        upgraded: success,
        message: if success {
            format!("Upgraded to {}", new_version.as_deref().unwrap_or("latest"))
        } else {
            "Upgrade command completed but version unchanged".to_string()
        },
        restart_required: success,
    })
}

fn execute_upgrade(method: InstallMethod) -> Result<(bool, Option<String>)> {
    let defaults = defaults::load_defaults();

    let (shell_cmd, success) = match method {
        InstallMethod::Homebrew => {
            let cmd = &defaults.install_methods.homebrew.upgrade_command;
            let status = Command::new("sh")
                .args(["-c", cmd])
                .status()
                .map_err(|e| Error::other(format!("Failed to run upgrade: {}", e)))?;
            (cmd.clone(), status.success())
        }
        InstallMethod::Cargo => {
            let cmd = &defaults.install_methods.cargo.upgrade_command;
            let status = Command::new("sh")
                .args(["-c", cmd])
                .status()
                .map_err(|e| Error::other(format!("Failed to run upgrade: {}", e)))?;
            (cmd.clone(), status.success())
        }
        InstallMethod::Source => {
            // For source builds, we need to find the git root
            let exe_path = std::env::current_exe()
                .map_err(|e| Error::other(format!("Failed to get current exe: {}", e)))?;

            // Navigate up from target/release/homeboy to find the workspace root
            let mut workspace_root = exe_path.clone();
            for _ in 0..3 {
                workspace_root = workspace_root
                    .parent()
                    .map(|p| p.to_path_buf())
                    .unwrap_or(workspace_root);
            }

            // Check if this looks like a git repo
            let git_dir = workspace_root.join(".git");
            if !git_dir.exists() {
                return Err(Error::validation_invalid_argument(
                    "source_path",
                    "Could not find git repository for source build",
                    Some(workspace_root.to_string_lossy().to_string()),
                    None,
                ));
            }

            // Execute the upgrade command from defaults
            let cmd = &defaults.install_methods.source.upgrade_command;
            let status = Command::new("sh")
                .args(["-c", cmd])
                .current_dir(&workspace_root)
                .status()
                .map_err(|e| Error::other(format!("Failed to run upgrade: {}", e)))?;

            (cmd.clone(), status.success())
        }
        InstallMethod::Binary => {
            let cmd = &defaults.install_methods.binary.upgrade_command;
            let status = Command::new("sh")
                .args(["-c", cmd])
                .status()
                .map_err(|e| Error::other(format!("Failed to run upgrade: {}", e)))?;
            (cmd.clone(), status.success())
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

    if !success {
        return Err(Error::other(format!(
            "Upgrade command failed: {}",
            shell_cmd
        )));
    }

    // Try to fetch the new version
    let new_version = fetch_latest_version(method).ok();

    Ok((true, new_version))
}

#[cfg(unix)]
pub fn restart_with_new_binary() -> ! {
    use std::os::unix::process::CommandExt;

    let binary = std::env::current_exe().expect("Failed to get current executable path");

    let err = Command::new(&binary).arg("--version").exec();

    // exec() only returns on error
    panic!("Failed to exec into new binary: {}", err);
}

#[cfg(not(unix))]
pub fn restart_with_new_binary() {
    // On Windows, just print a message
    eprintln!("Please restart homeboy to use the new version.");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_version_comparison() {
        assert!(version_is_newer("0.12.0", "0.11.0"));
        assert!(version_is_newer("1.0.0", "0.99.99"));
        assert!(version_is_newer("0.11.1", "0.11.0"));
        assert!(!version_is_newer("0.11.0", "0.11.0"));
        assert!(!version_is_newer("0.10.0", "0.11.0"));
    }

    #[test]
    fn test_current_version() {
        let version = current_version();
        assert!(!version.is_empty());
        assert!(version.contains('.'));
    }

    #[test]
    fn test_install_method_strings() {
        assert_eq!(InstallMethod::Homebrew.as_str(), "homebrew");
        assert_eq!(InstallMethod::Cargo.as_str(), "cargo");
        assert_eq!(InstallMethod::Source.as_str(), "source");
        assert_eq!(InstallMethod::Binary.as_str(), "binary");
        assert_eq!(InstallMethod::Unknown.as_str(), "unknown");
    }
}
