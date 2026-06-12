use crate::core::defaults;
use crate::core::error::{Error, Result};
use crate::core::{extension, git};
use std::path::Path;
use std::process::Command;

use super::constants::{CRATES_IO_API, GITHUB_RELEASES_API, VERSION};
use super::execution::execute_upgrade;
use super::planning::resolve_binary_on_path;
use super::runners;
use super::types::*;
use super::validation::check_for_updates;

pub fn current_version() -> &'static str {
    VERSION
}

pub(crate) fn fetch_latest_crates_io_version() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".to_string())))?;

    let response: CratesIoResponse = client
        .get(CRATES_IO_API)
        .send()
        .map_err(|e| Error::internal_io(e.to_string(), Some("query crates.io".to_string())))?
        .json()
        .map_err(|e| {
            Error::internal_json(e.to_string(), Some("parse crates.io response".to_string()))
        })?;

    Ok(response.crate_info.newest_version)
}

pub(crate) fn fetch_latest_github_version() -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".to_string())))?;

    let response: GitHubRelease = client
        .get(GITHUB_RELEASES_API)
        .send()
        .map_err(|e| Error::internal_io(e.to_string(), Some("query GitHub releases".to_string())))?
        .json()
        .map_err(|e| {
            Error::internal_json(
                e.to_string(),
                Some("parse GitHub release response".to_string()),
            )
        })?;

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

    detect_install_method_from_exe_path(&exe_path, |cmd, args| {
        Command::new(cmd)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    })
}

pub(crate) fn detect_install_method_from_exe_path<F>(
    exe_path: &str,
    mut list_command_succeeds: F,
) -> InstallMethod
where
    F: FnMut(&str, &[&str]) -> bool,
{
    let defaults = defaults::load_defaults();

    // Prefer the active executable path over unrelated installed copies.
    for pattern in &defaults.install_methods.homebrew.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Homebrew;
        }
    }
    for pattern in &defaults.install_methods.cargo.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Cargo;
        }
    }
    for pattern in &defaults.install_methods.source.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Source;
        }
    }
    for pattern in &defaults.install_methods.binary.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Binary;
        }
    }

    // Fall back to Homebrew presence only when the active path is not recognized.
    if let Some(list_cmd) = &defaults.install_methods.homebrew.list_command {
        let parts: Vec<&str> = list_cmd.split_whitespace().collect();
        if let Some((cmd, args)) = parts.split_first() {
            if list_command_succeeds(cmd, args) {
                return InstallMethod::Homebrew;
            }
        }
    }

    InstallMethod::Unknown
}

pub(crate) fn version_is_newer(latest: &str, current: &str) -> bool {
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

pub fn run_upgrade_with_method(
    force: bool,
    method_override: Option<InstallMethod>,
    skip_extensions: bool,
    skip_runners: bool,
    runner_targets: &[String],
    source_path: Option<&Path>,
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
            // Even when no binary update is needed, still run extension updates.
            let (extensions_updated, extensions_skipped) = if skip_extensions {
                (vec![], vec![])
            } else {
                update_all_extensions()
            };
            let (runners_updated, runners_skipped) = if skip_runners {
                (vec![], vec![])
            } else {
                runners::upgrade_configured_runners(
                    force,
                    method_override,
                    source_path,
                    runner_targets,
                    &extensions_updated,
                )?
            };
            return Ok(UpgradeResult {
                command: "upgrade".to_string(),
                install_method,
                previous_version: previous_version.clone(),
                new_version: Some(previous_version),
                upgraded: false,
                message: "Already at latest version".to_string(),
                restart_required: false,
                extensions_updated,
                extensions_skipped,
                runners_updated,
                runners_skipped,
            });
        }
    }

    // Execute the upgrade
    let (success, new_version) = execute_upgrade(install_method, source_path)?;
    let upgrade_completed = should_sync_after_upgrade(new_version.as_deref());

    // Auto-update all installed extensions after the upgrade command completes.
    // This prevents CI/local extension version drift that causes baseline
    // mismatches and inconsistent audit findings.
    let (extensions_updated, extensions_skipped) = if upgrade_completed && !skip_extensions {
        update_all_extensions()
    } else {
        (vec![], vec![])
    };

    let (runners_updated, runners_skipped) = if upgrade_completed && !skip_runners {
        runners::upgrade_configured_runners(
            force,
            method_override,
            source_path,
            runner_targets,
            &extensions_updated,
        )?
    } else {
        (vec![], vec![])
    };

    Ok(UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        previous_version,
        new_version: new_version.clone(),
        upgraded: success,
        message: if success {
            format!("Upgraded to {}", new_version.as_deref().unwrap_or("latest"))
        } else if let Some(version) = &new_version {
            format!(
                "Upgrade command completed but active binary is still {}",
                version
            )
        } else {
            "Upgrade command completed but active binary version could not be verified".to_string()
        },
        restart_required: success && matches!(install_method, InstallMethod::Source),
        extensions_updated,
        extensions_skipped,
        runners_updated,
        runners_skipped,
    })
}

pub(crate) fn should_sync_after_upgrade(new_version: Option<&str>) -> bool {
    new_version.is_some()
}

/// Update all installed extensions. Best-effort — failures are logged and
/// the extension is added to the skipped list.
fn update_all_extensions() -> (Vec<ExtensionUpgradeEntry>, Vec<String>) {
    let extension_ids = extension::available_extension_ids();
    if extension_ids.is_empty() {
        return (vec![], vec![]);
    }

    log_status!(
        "upgrade",
        "Updating {} installed extension(s)...",
        extension_ids.len()
    );

    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for id in &extension_ids {
        let old_version = extension::load_extension(id)
            .ok()
            .map(|m| m.version.clone())
            .unwrap_or_default();

        match extension::update(id, false) {
            Ok(result) => {
                let new_version = extension::load_extension(id)
                    .ok()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();
                let source_url = portable_extension_source_url(&result);
                let source_revision = result
                    .source_update
                    .new_source_revision
                    .clone()
                    .or_else(|| extension::read_source_revision(id));

                if result.linked {
                    let branch_detail = match (
                        &result.source_update.old_branch,
                        &result.source_update.new_branch,
                    ) {
                        (Some(old), Some(new)) if old != new => format!(" ({} → {})", old, new),
                        (Some(branch), _) => format!(" ({})", branch),
                        _ => String::new(),
                    };
                    log_status!(
                        "upgrade",
                        "  {} {} → {} linked source updated{}",
                        id,
                        old_version,
                        new_version,
                        branch_detail
                    );
                } else if old_version != new_version {
                    log_status!("upgrade", "  {} {} → {}", id, old_version, new_version);
                } else {
                    log_status!("upgrade", "  {} {} (up to date)", id, new_version);
                }

                updated.push(ExtensionUpgradeEntry {
                    extension_id: id.clone(),
                    old_version,
                    new_version,
                    linked: result.linked,
                    source_path: result
                        .source_path
                        .map(|path| path.to_string_lossy().to_string()),
                    git_root: result
                        .git_root
                        .map(|path| path.to_string_lossy().to_string()),
                    source_url,
                    source_revision,
                    source_update: result.source_update,
                });
            }
            Err(e) => {
                log_status!("upgrade", "  {} skipped: {}", id, e.message);
                skipped.push(id.clone());
            }
        }
    }

    (updated, skipped)
}

fn portable_extension_source_url(result: &crate::core::extension::UpdateResult) -> Option<String> {
    if let Some(git_root) = result.git_root.as_ref() {
        return git::remote_origin_url(git_root);
    }

    if extension::is_git_url(&result.url) {
        Some(result.url.clone())
    } else {
        None
    }
}

#[cfg(not(unix))]
pub fn restart_with_new_binary() {
    log_status!("upgrade", "Please restart homeboy to use the new version.");
}

#[cfg(unix)]
pub fn restart_with_new_binary() -> ! {
    use std::os::unix::process::CommandExt;

    // After an upgrade, std::env::current_exe() reads /proc/self/exe which
    // points to the *deleted* old binary inode. Resolve the fresh binary from
    // $PATH instead so we exec into the newly-installed version.
    let binary = resolve_binary_on_path()
        .unwrap_or_else(|| std::env::current_exe().expect("Failed to get current executable path"));

    let err = Command::new(&binary).arg("--version").exec();

    // exec() only returns on error — fall back to a clean exit instead of panicking
    eprintln!(
        "Warning: could not restart automatically ({}). Please run `homeboy --version` to confirm the upgrade.",
        err
    );
    std::process::exit(0);
}
