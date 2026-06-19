use crate::core::defaults;
use crate::core::error::{Error, Result};
use crate::core::{build_identity, extension, git};
use std::path::{Path, PathBuf};
use std::process::Command;

use super::constants::{CRATES_IO_API, GITHUB_RELEASES_API, VERSION};
use super::execution::{execute_upgrade, resolve_source_workspace};
use super::planning::resolve_binary_on_path;
use super::runners;
use super::services;
use super::types::*;
use super::validation::check_for_updates;

pub fn current_version() -> &'static str {
    VERSION
}

pub fn current_build_version() -> String {
    build_identity::current().display
}

pub(crate) fn fetch_latest_secondary_version() -> Result<String> {
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
        InstallMethod::Secondary => fetch_latest_secondary_version(),
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
    for pattern in &defaults.install_methods.secondary.path_patterns {
        if exe_path.contains(pattern) {
            return InstallMethod::Secondary;
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

#[allow(clippy::too_many_arguments)]
pub fn run_upgrade_with_method(
    force: bool,
    method_override: Option<InstallMethod>,
    skip_extensions: bool,
    skip_runners: bool,
    skip_services: bool,
    runner_targets: &[String],
    source_path: Option<&Path>,
) -> Result<UpgradeResult> {
    let install_method = method_override.unwrap_or_else(detect_install_method);
    let previous_version = current_version().to_string();
    let previous_build_identity = Some(build_identity::current().display);

    if install_method == InstallMethod::Unknown {
        return Err(Error::validation_invalid_argument(
            "install_method",
            "Could not detect installation method",
            None,
            None,
        )
        .with_hint("Try: homeboy upgrade --method binary")
        .with_hint("Or reinstall using: brew install homeboy")
        .with_hint(format!(
            "Or: {} install homeboy",
            defaults::secondary_install_method_key()
        )));
    }
    let source_upgrade_path = source_upgrade_path_for_method(install_method, source_path)?;
    let runner_method_override = runner_method_override_for_method(method_override, install_method);

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
                    runner_method_override,
                    source_upgrade_path.as_deref(),
                    runner_targets,
                    &extensions_updated,
                )?
            };
            return Ok(UpgradeResult {
                command: "upgrade".to_string(),
                install_method,
                previous_version: previous_version.clone(),
                new_version: Some(previous_version),
                previous_build_identity,
                new_build_identity: None,
                upgraded: false,
                message: "Already at latest version".to_string(),
                restart_required: false,
                extensions_unrefreshed: warn_unrefreshed_symlinked_extensions(&extensions_updated),
                extensions_updated,
                extensions_skipped,
                runners_updated,
                runners_skipped,
                // No binary swap happened, so resident services already run the
                // current binary and need no restart.
                services_restarted: Vec::new(),
                services_pending_restart: Vec::new(),
            });
        }
    }

    // Execute the upgrade
    let (success, new_version, new_build_identity) = execute_upgrade(
        install_method,
        source_upgrade_path.as_deref(),
        force,
        previous_build_identity.as_deref(),
    )?;
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
            runner_method_override,
            source_upgrade_path.as_deref(),
            runner_targets,
            &extensions_updated,
        )?
    } else {
        (vec![], vec![])
    };

    // After a verified binary swap, long-running services still hold the old
    // binary in memory until restarted. Restart each declared resident service
    // (config-driven; nothing is hardcoded in core) unless restarts were
    // skipped, in which case report each as pending with its recovery command.
    let (services_restarted, services_pending_restart) =
        restart_resident_services_after_swap(success, skip_services);

    Ok(UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        previous_version,
        new_version: new_version.clone(),
        previous_build_identity,
        new_build_identity: new_build_identity.clone(),
        upgraded: success,
        message: if success {
            if let Some(identity) = &new_build_identity {
                format!(
                    "Upgraded to {} ({})",
                    new_version.as_deref().unwrap_or("latest"),
                    identity
                )
            } else {
                format!("Upgraded to {}", new_version.as_deref().unwrap_or("latest"))
            }
        } else if let Some(version) = &new_version {
            format!(
                "Upgrade command completed but active binary is still {}",
                version
            )
        } else {
            "Upgrade command completed but active binary version could not be verified".to_string()
        },
        restart_required: success && matches!(install_method, InstallMethod::Source),
        extensions_unrefreshed: warn_unrefreshed_symlinked_extensions(&extensions_updated),
        extensions_updated,
        extensions_skipped,
        runners_updated,
        runners_skipped,
        services_restarted,
        services_pending_restart,
    })
}

/// Restart declared binary-resident services after a successful binary swap.
///
/// Returns `(restarted, pending)`. When the swap did not succeed there is no
/// new binary to load, so nothing is restarted. When `skip_services` is set,
/// every declared service is reported as pending with its recovery command.
fn restart_resident_services_after_swap(
    swap_succeeded: bool,
    skip_services: bool,
) -> (Vec<ServiceRestartEntry>, Vec<ServiceRestartEntry>) {
    if !swap_succeeded {
        return (Vec::new(), Vec::new());
    }

    let resident_services = defaults::load_config().resident_services;
    if resident_services.is_empty() {
        return (Vec::new(), Vec::new());
    }

    if skip_services {
        let pending = services::pending_when_skipped(&resident_services);
        warn_pending_resident_services(&pending);
        return (Vec::new(), pending);
    }

    log_status!(
        "upgrade",
        "Restarting {} binary-resident service(s) to load the new binary...",
        resident_services.len()
    );

    let (restarted, pending) =
        services::restart_resident_services(&resident_services, services::run_restart_command);

    for entry in &restarted {
        log_status!("upgrade", "  {} restarted", entry.service_id);
    }
    warn_pending_resident_services(&pending);

    (restarted, pending)
}

/// Surface declared resident services that still hold the old binary, with the
/// exact recovery command, instead of failing the upgrade silently.
fn warn_pending_resident_services(pending: &[ServiceRestartEntry]) {
    for entry in pending {
        let detail = entry.detail.as_deref().unwrap_or("restart required");
        if entry.restart_command.is_empty() {
            log_status!(
                "upgrade",
                "  WARNING: {} not restarted ({})",
                entry.service_id,
                detail,
            );
        } else {
            log_status!(
                "upgrade",
                "  WARNING: {} not restarted ({}). Run: {}",
                entry.service_id,
                detail,
                entry.restart_command,
            );
        }
    }
}

fn source_upgrade_path_for_method(
    install_method: InstallMethod,
    source_path: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if install_method == InstallMethod::Source {
        return resolve_source_workspace(source_path).map(Some);
    }

    Ok(source_path.map(Path::to_path_buf))
}

fn runner_method_override_for_method(
    method_override: Option<InstallMethod>,
    install_method: InstallMethod,
) -> Option<InstallMethod> {
    method_override
        .or_else(|| (install_method == InstallMethod::Source).then_some(InstallMethod::Source))
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

/// Detect unrefreshed symlinked extension clones and log a loud, actionable
/// warning for each before returning them in the upgrade result.
fn warn_unrefreshed_symlinked_extensions(
    refreshed: &[ExtensionUpgradeEntry],
) -> Vec<UnrefreshedExtensionWarning> {
    let warnings = detect_unrefreshed_symlinked_extensions(refreshed);
    if warnings.is_empty() {
        return warnings;
    }

    log_status!(
        "upgrade",
        "WARNING: {} symlinked extension clone(s) owned by '{}' were NOT refreshed by this privileged upgrade.",
        warnings.len(),
        warnings
            .first()
            .map(|w| w.invoking_user.as_str())
            .unwrap_or("the invoking user"),
    );
    log_status!(
        "upgrade",
        "  Extension resolution is $HOME-scoped, so a sudo upgrade only refreshes root's copies. Run each recovery command below to bring the symlinked clone(s) current:",
    );
    for w in &warnings {
        let behind = w
            .behind
            .map(|n| format!("{} commit(s) behind", n))
            .unwrap_or_else(|| "behind upstream".to_string());
        log_status!(
            "upgrade",
            "  {} ({}) -> {}: {}",
            w.extension_id,
            behind,
            w.source_path,
            w.recovery_command,
        );
    }

    warnings
}

/// Detect symlinked extension clones in the *invoking* user's config dir that
/// this upgrade could not refresh.
///
/// Extension resolution is `$HOME`-scoped (`core::paths::extension`). When the
/// upgrade runs under `sudo`, `$HOME` is root's, so `update_all_extensions()`
/// only ever visits root's own extension copies. Any non-root user whose
/// extension entry is a symlink into a workspace-managed git clone is therefore
/// silently left stale — the upgraded binary ships a fix the user can never
/// execute because the extension they actually run was never updated.
///
/// We do not refresh those clones here: they are owned by a different user and
/// may carry dirty/unpushed state, so mutating them from a privileged process
/// is unsafe. Instead we emit a loud, actionable warning with the exact
/// recovery command. Returns an empty vec when not running under `sudo`, when
/// the invoking user's config dir is absent, or when nothing is stale.
fn detect_unrefreshed_symlinked_extensions(
    refreshed: &[ExtensionUpgradeEntry],
) -> Vec<UnrefreshedExtensionWarning> {
    let Some(sudo_user) = std::env::var("SUDO_USER").ok().filter(|u| !u.is_empty()) else {
        return Vec::new();
    };
    // Never re-warn about root's own extensions if SUDO_USER somehow resolves to root.
    if sudo_user == "root" {
        return Vec::new();
    }

    let Some(invoking_home) = home_dir_for_user(&sudo_user) else {
        return Vec::new();
    };
    let extensions_dir = invoking_home
        .join(".config")
        .join(crate::core::product_identity::PRODUCT_IDENTITY.config_dirname)
        .join("extensions");
    let Ok(entries) = std::fs::read_dir(&extensions_dir) else {
        return Vec::new();
    };

    // Git roots already refreshed by the privileged run (canonicalized), so we
    // don't warn about a clone the upgrade actually updated.
    let refreshed_roots: std::collections::HashSet<PathBuf> = refreshed
        .iter()
        .filter_map(|e| e.git_root.as_deref())
        .map(|root| {
            let p = PathBuf::from(root);
            p.canonicalize().unwrap_or(p)
        })
        .collect();

    let mut warnings = Vec::new();
    for entry in entries.flatten() {
        let symlink_path = entry.path();
        let Ok(meta) = std::fs::symlink_metadata(&symlink_path) else {
            continue;
        };
        if !meta.file_type().is_symlink() {
            continue;
        }
        let extension_id = match symlink_path.file_name().and_then(|n| n.to_str()) {
            Some(name) => name.to_string(),
            None => continue,
        };
        // Extension manifests live in `extensions/<id>.json`; the symlink target
        // is the source dir. Resolve the symlink to its git working tree.
        let Ok(target) = std::fs::canonicalize(&symlink_path) else {
            continue;
        };
        let Ok(git_root_str) = git::get_git_root(&target.to_string_lossy()) else {
            continue;
        };
        let git_root = PathBuf::from(&git_root_str);
        let git_root = git_root.canonicalize().unwrap_or(git_root);

        if refreshed_roots.contains(&git_root) {
            continue;
        }

        let behind = git_commits_behind_upstream(&git_root);
        // Only warn when we can confirm the clone is actually behind. If we
        // can't determine drift, stay quiet rather than crying wolf.
        if behind.is_some_and(|n| n > 0) {
            warnings.push(UnrefreshedExtensionWarning {
                extension_id,
                invoking_user: sudo_user.clone(),
                symlink_path: symlink_path.to_string_lossy().to_string(),
                source_path: git_root.to_string_lossy().to_string(),
                behind,
                recovery_command: format!(
                    "sudo -u {} git -C {} pull --ff-only",
                    sudo_user,
                    git_root.display()
                ),
            });
        }
    }

    warnings
}

/// Resolve a user's home directory without pulling in extra crates: prefer the
/// `getent passwd` database, fall back to `/home/<user>` only if it exists.
fn home_dir_for_user(user: &str) -> Option<PathBuf> {
    if let Ok(output) = Command::new("getent").args(["passwd", user]).output() {
        if output.status.success() {
            let line = String::from_utf8_lossy(&output.stdout);
            // passwd format: name:passwd:uid:gid:gecos:home:shell
            if let Some(home) = line.trim_end().split(':').nth(5) {
                if !home.is_empty() {
                    return Some(PathBuf::from(home));
                }
            }
        }
    }
    let fallback = PathBuf::from("/home").join(user);
    fallback.is_dir().then_some(fallback)
}

/// Number of commits the checked-out branch is behind its upstream, after a
/// fetch. Returns `None` when the count can't be determined (no upstream, not a
/// repo, fetch failed). Best-effort and read-only — never mutates the worktree.
fn git_commits_behind_upstream(git_root: &Path) -> Option<u32> {
    // Read-only fetch to learn upstream state without touching the worktree.
    let _ = git::run_git(git_root, &["fetch", "origin"], "git fetch origin");
    let count = git::run_git(
        git_root,
        &["rev-list", "--count", "HEAD..@{upstream}"],
        "git rev-list behind upstream",
    )
    .ok()?;
    count.trim().parse::<u32>().ok()
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

#[cfg(test)]
mod runner_source_upgrade_tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn detected_source_install_forwards_source_method_to_runners() {
        assert_eq!(
            runner_method_override_for_method(None, InstallMethod::Source),
            Some(InstallMethod::Source)
        );
        assert_eq!(
            runner_method_override_for_method(None, InstallMethod::Secondary),
            None
        );
        assert_eq!(
            runner_method_override_for_method(Some(InstallMethod::Binary), InstallMethod::Source),
            Some(InstallMethod::Binary)
        );
    }

    #[test]
    fn source_upgrade_path_resolves_explicit_checkout_for_runners() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("homeboy.json"), r#"{"id":"homeboy"}"#).unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            r#"[package]
name = "homeboy"
version = "0.0.0"
"#,
        )
        .unwrap();
        let nested = dir.path().join("target/debug");
        std::fs::create_dir_all(&nested).unwrap();

        let resolved = source_upgrade_path_for_method(InstallMethod::Source, Some(&nested))
            .expect("source path")
            .expect("resolved checkout");

        assert_eq!(resolved, dir.path());
    }
}

#[cfg(test)]
mod symlinked_extension_tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    // Serialize SUDO_USER mutation across tests in this module.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    struct SudoUserGuard {
        prior: Option<String>,
        _guard: std::sync::MutexGuard<'static, ()>,
    }

    impl SudoUserGuard {
        fn set(value: Option<&str>) -> Self {
            let guard = env_lock().lock().unwrap_or_else(|e| e.into_inner());
            let prior = std::env::var("SUDO_USER").ok();
            match value {
                Some(v) => std::env::set_var("SUDO_USER", v),
                None => std::env::remove_var("SUDO_USER"),
            }
            Self {
                prior,
                _guard: guard,
            }
        }
    }

    impl Drop for SudoUserGuard {
        fn drop(&mut self) {
            match self.prior.take() {
                Some(v) => std::env::set_var("SUDO_USER", v),
                None => std::env::remove_var("SUDO_USER"),
            }
        }
    }

    #[test]
    fn no_warning_when_not_running_under_sudo() {
        let _guard = SudoUserGuard::set(None);
        assert!(detect_unrefreshed_symlinked_extensions(&[]).is_empty());
    }

    #[test]
    fn no_warning_when_sudo_user_is_root() {
        let _guard = SudoUserGuard::set(Some("root"));
        assert!(detect_unrefreshed_symlinked_extensions(&[]).is_empty());
    }

    #[test]
    fn no_warning_when_sudo_user_is_empty() {
        let _guard = SudoUserGuard::set(Some(""));
        assert!(detect_unrefreshed_symlinked_extensions(&[]).is_empty());
    }

    #[test]
    fn no_warning_when_invoking_user_has_no_config_dir() {
        // A user that exists in passwd but has no homeboy extensions dir must
        // not produce warnings (and must not panic on the missing directory).
        let _guard = SudoUserGuard::set(Some("definitely-not-a-real-user-xyz"));
        assert!(detect_unrefreshed_symlinked_extensions(&[]).is_empty());
    }
}
