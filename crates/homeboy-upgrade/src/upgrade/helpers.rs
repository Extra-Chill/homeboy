use homeboy_core::defaults;
use homeboy_core::error::{Error, Result};
use homeboy_core::{build_identity, git};
use homeboy_extension as extension;
use semver::Version;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::constants::{GITHUB_RELEASES_API, VERSION};
use super::execution::{
    execute_upgrade, installed_target_build_identity, prepare_source_workspace_for_upgrade,
    resolve_source_workspace,
};
use super::services;
use super::types::*;
use super::validation::check_for_updates;

pub fn current_version() -> &'static str {
    VERSION
}

pub fn current_build_version() -> String {
    build_identity::current().display
}

fn fetch_latest_github_version_at(url: &str) -> Result<String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".to_string())))?;

    let response: GitHubRelease = client
        .get(url)
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
    fetch_latest_github_version_at(latest_version_endpoint(method))
}

fn latest_version_endpoint(_method: InstallMethod) -> &'static str {
    GITHUB_RELEASES_API
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

pub fn version_is_newer(latest: &str, current: &str) -> bool {
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
    runner_only: bool,
    runner_targets: &[String],
    source_path: Option<&Path>,
) -> Result<UpgradeResult> {
    if runner_only {
        return run_targeted_runner_upgrade(force, method_override, runner_targets, source_path);
    }

    let promotion_lease = homeboy_core::runtime_promotion::acquire(
        "controller upgrade",
        source_path
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "active controller".to_string()),
    )?;
    let install_method = method_override.unwrap_or_else(detect_install_method);

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

    // Source upgrades converge on the requested build, not merely the latest
    // released semver. A distinct, verifiable source build at the same semver
    // must therefore bypass the release-version no-op path.
    //
    // The decision compares the source against the *installed target* binary,
    // not the invoking candidate's in-process identity. A source-built candidate
    // invoking itself would otherwise always match its own identity and no-op,
    // leaving the older installed controller in place — the promotion-policy
    // bootstrap catch-22 in #9371. When the invoking binary *is* the installed
    // target (the normal path) the target identity is the current identity, so
    // this is a no-op for ordinary upgrades.
    let (target_version, target_identity) = resolve_source_upgrade_target(
        install_method == InstallMethod::Source && !force && source_path.is_some(),
        current_version(),
    );
    // Report the *replaced* controller as the previous identity, so a candidate
    // that bootstraps over an older installed target surfaces the target it
    // superseded rather than its own build.
    let previous_version = target_version.clone();
    let previous_build_identity = Some(target_identity.display.clone());
    let source_upgrade_decision =
        (install_method == InstallMethod::Source && !force && source_path.is_some())
            .then(|| {
                let source_path = source_upgrade_path
                    .as_deref()
                    .expect("explicit source path");
                // Validate before the release gate and any extension or runner
                // work. A dirty source tree is never an acceptable controller.
                prepare_source_workspace_for_upgrade(source_path)?;
                Ok(source_upgrade_decision(
                    &target_version,
                    &target_identity,
                    source_path,
                ))
            })
            .transpose()?;

    if let Some(decision) = source_upgrade_decision {
        if !decision.upgrades() {
            return Ok(source_upgrade_noop_result(
                install_method,
                previous_version,
                previous_build_identity,
                decision,
            ));
        }
    }

    // An approved explicit source build is the controller target, regardless of
    // whether the latest published release has the same semantic version.
    let replacement_approved =
        controller_replacement_proceeds(force, source_upgrade_decision, false);

    // Check if a release update is available unless an explicit source target
    // has already been approved for controller replacement.
    if !replacement_approved {
        let check = check_for_updates()?;
        if !controller_replacement_proceeds(force, source_upgrade_decision, check.update_available)
        {
            // Even when no binary update is needed, still run extension updates.
            let (extensions_updated, extensions_skipped) = if skip_extensions {
                (vec![], vec![])
            } else {
                update_all_extensions()
            };
            let (runners_updated, runners_skipped) = if skip_runners {
                (vec![], vec![])
            } else {
                super::with_runner_upgrade(|p| {
                    p.upgrade_configured_runners_with_explicit_source_path(
                        force,
                        runner_method_override,
                        source_upgrade_path.as_deref(),
                        source_path.is_some(),
                        None,
                        runner_targets,
                        &extensions_updated,
                    )
                })?
            };
            let partial = runner_convergence_failed(
                &runners_updated,
                &runners_skipped,
                Some(previous_version.as_str()),
            );
            let runner_disposition = runner_convergence_disposition(
                skip_runners,
                &runners_updated,
                &runners_skipped,
                Some(previous_version.as_str()),
            );
            return Ok(UpgradeResult {
                command: "upgrade".to_string(),
                install_method,
                previous_version: previous_version.clone(),
                new_version: Some(previous_version),
                previous_build_identity,
                new_build_identity: None,
                source_revision: None,
                upgraded: false,
                partial,
                runner_convergence: Some(runner_disposition),
                message: match runner_disposition {
                    RunnerConvergenceDisposition::Partial => {
                        "PARTIAL: controller is already current, but configured runners did not converge"
                            .to_string()
                    }
                    RunnerConvergenceDisposition::Skipped => {
                        "Already at latest version; runner convergence skipped".to_string()
                    }
                    RunnerConvergenceDisposition::NoRunnersConfigured => {
                        "Already at latest version; no configured runners".to_string()
                    }
                    RunnerConvergenceDisposition::Converged => {
                        "Already at latest version; configured runners converged".to_string()
                    }
                },
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
    promotion_lease.assert_generation()?;
    let (success, new_version, new_build_identity, source_revision) = execute_upgrade(
        install_method,
        source_upgrade_path.as_deref(),
        source_path.is_some(),
        force,
        previous_build_identity.as_deref(),
    )?;
    let upgrade_completed = should_sync_after_upgrade(new_version.as_deref());
    if upgrade_completed {
        // This is deliberately a short switch, not a drain: records admitted
        // before it retain their immutable runtime pin and remain executable.
        let installed_executable = super::execution::active_binary_path()?;
        homeboy_core::controller_runtime::activate_installed_generation(&installed_executable)?;
    }

    // Auto-update all installed extensions after the upgrade command completes.
    // This prevents CI/local extension version drift that causes baseline
    // mismatches and inconsistent audit findings.
    let (extensions_updated, extensions_skipped) = if upgrade_completed && !skip_extensions {
        upgrade_phase("refreshing installed extensions");
        update_all_extensions()
    } else {
        (vec![], vec![])
    };

    promotion_lease.assert_generation()?;
    let (runners_updated, runners_skipped) = if upgrade_completed && !skip_runners {
        upgrade_phase("refreshing configured runners");
        super::with_runner_upgrade(|p| {
            p.upgrade_configured_runners_with_explicit_source_path(
                force,
                runner_method_override,
                source_upgrade_path.as_deref(),
                source_path.is_some(),
                None,
                runner_targets,
                &extensions_updated,
            )
        })?
    } else {
        (vec![], vec![])
    };

    // After a verified binary swap, long-running services still hold the old
    // binary in memory until restarted. Restart each declared resident service
    // (config-driven; nothing is hardcoded in core) unless restarts were
    // skipped, in which case report each as pending with its recovery command.
    let (services_restarted, services_pending_restart) =
        restart_resident_services_after_swap(success, skip_services);

    let runner_disposition = runner_convergence_disposition(
        skip_runners,
        &runners_updated,
        &runners_skipped,
        new_version.as_deref(),
    );

    Ok(UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        previous_version,
        new_version: new_version.clone(),
        previous_build_identity,
        new_build_identity: new_build_identity.clone(),
        source_revision,
        upgraded: success,
        partial: runner_convergence_failed(
            &runners_updated,
            &runners_skipped,
            new_version.as_deref(),
        ),
        runner_convergence: Some(runner_disposition),
        message: upgrade_message(
            success,
            new_version.as_deref(),
            new_build_identity.as_deref(),
            runner_disposition,
            &runners_updated,
            &runners_skipped,
        ),
        // Source replacement updates the on-disk executable, but this command
        // exits immediately afterwards. Re-execing only `--version` skips the
        // normal completion path and provides no lifecycle benefit.
        restart_required: false,
        extensions_unrefreshed: warn_unrefreshed_symlinked_extensions(&extensions_updated),
        extensions_updated,
        extensions_skipped,
        runners_updated,
        runners_skipped,
        services_restarted,
        services_pending_restart,
    })
}

fn source_upgrade_noop_result(
    install_method: InstallMethod,
    previous_version: String,
    previous_build_identity: Option<String>,
    decision: SourceUpgradeDecision,
) -> UpgradeResult {
    UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        new_version: Some(previous_version.clone()),
        previous_version,
        previous_build_identity,
        new_build_identity: None,
        source_revision: None,
        upgraded: false,
        partial: false,
        // No upgrade occurred, so there is no runner-convergence claim to make.
        runner_convergence: None,
        message: decision.no_op_message(),
        restart_required: false,
        extensions_updated: Vec::new(),
        extensions_skipped: Vec::new(),
        runners_updated: Vec::new(),
        runners_skipped: Vec::new(),
        extensions_unrefreshed: Vec::new(),
        services_restarted: Vec::new(),
        services_pending_restart: Vec::new(),
    }
}

fn source_upgrade_bypasses_release_gate(decision: Option<SourceUpgradeDecision>) -> bool {
    decision.is_some_and(SourceUpgradeDecision::upgrades)
}

fn controller_replacement_proceeds(
    force: bool,
    source_decision: Option<SourceUpgradeDecision>,
    release_update_available: bool,
) -> bool {
    force || source_upgrade_bypasses_release_gate(source_decision) || release_update_available
}

/// Upgrade only explicitly selected runners without promoting the controller.
/// Source controllers pin their checkout identity; packaged controllers retain
/// the runner's existing install-method contract.
fn run_targeted_runner_upgrade(
    force: bool,
    method_override: Option<InstallMethod>,
    runner_targets: &[String],
    source_path: Option<&Path>,
) -> Result<UpgradeResult> {
    let previous_version = current_version().to_string();
    let previous_build_identity = build_identity::current().display;
    let install_method = method_override.unwrap_or_else(detect_install_method);
    if install_method == InstallMethod::Unknown {
        return Err(Error::validation_invalid_argument(
            "install_method",
            "Could not detect installation method",
            None,
            None,
        )
        .with_hint("Pass --method to select the runner upgrade policy."));
    }
    let runner_method_override = runner_method_override_for_method(method_override, install_method);
    let source_checkout = if runner_method_override == Some(InstallMethod::Source) {
        Some(initiating_controller_source_checkout(
            source_path,
            &previous_build_identity,
        )?)
    } else {
        None
    };
    let (runners_updated, runners_skipped) = super::with_runner_upgrade(|provider| {
        provider.upgrade_configured_runners_with_explicit_source_path(
            force,
            runner_method_override,
            source_checkout.as_deref(),
            source_checkout.is_some(),
            Some(&previous_build_identity),
            runner_targets,
            &installed_extension_catalog(),
        )
    })?;

    Ok(UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        previous_version: previous_version.clone(),
        new_version: Some(previous_version.clone()),
        previous_build_identity: Some(previous_build_identity.clone()),
        new_build_identity: Some(previous_build_identity),
        source_revision: None,
        upgraded: false,
        partial: runner_convergence_failed(
            &runners_updated,
            &runners_skipped,
            Some(previous_version.as_str()),
        ),
        // Targeted runner upgrade always attempts convergence (never skipped).
        runner_convergence: Some(runner_convergence_disposition(
            false,
            &runners_updated,
            &runners_skipped,
            Some(previous_version.as_str()),
        )),
        message: targeted_runner_message(
            source_checkout.is_some(),
            &previous_version,
            runner_targets,
            &runners_updated,
            &runners_skipped,
        ),
        restart_required: false,
        extensions_updated: Vec::new(),
        extensions_skipped: Vec::new(),
        runners_updated,
        runners_skipped,
        extensions_unrefreshed: Vec::new(),
        services_restarted: Vec::new(),
        services_pending_restart: Vec::new(),
    })
}

fn runner_convergence_failed(
    updated: &[RunnerUpgradeEntry],
    skipped: &[RunnerUpgradeEntry],
    controller_version: Option<&str>,
) -> bool {
    updated.iter().chain(skipped).any(|runner| {
        !runner.success
            || controller_version
                .is_some_and(|version| runner.new_version.as_deref() != Some(version))
    })
}

/// Classify runner convergence from intent (`--skip-runners`) and evidence.
///
/// Distinguishes an explicit skip, zero configured runners, verified
/// convergence, and partial convergence, so the rendered message and structured
/// output never claim convergence that was not actually verified (#9842).
fn runner_convergence_disposition(
    runners_skipped_by_flag: bool,
    updated: &[RunnerUpgradeEntry],
    skipped: &[RunnerUpgradeEntry],
    controller_version: Option<&str>,
) -> RunnerConvergenceDisposition {
    if runners_skipped_by_flag {
        return RunnerConvergenceDisposition::Skipped;
    }
    if runner_convergence_failed(updated, skipped, controller_version) {
        return RunnerConvergenceDisposition::Partial;
    }
    if updated.is_empty() && skipped.is_empty() {
        return RunnerConvergenceDisposition::NoRunnersConfigured;
    }
    RunnerConvergenceDisposition::Converged
}

fn upgrade_message(
    success: bool,
    new_version: Option<&str>,
    new_build_identity: Option<&str>,
    disposition: RunnerConvergenceDisposition,
    updated: &[RunnerUpgradeEntry],
    skipped: &[RunnerUpgradeEntry],
) -> String {
    let controller = new_version.unwrap_or("unverified");
    let identity = new_build_identity
        .map(|identity| format!(" ({identity})"))
        .unwrap_or_default();
    if disposition == RunnerConvergenceDisposition::Partial {
        return format!(
            "PARTIAL: controller upgraded to {controller}{identity}, but {} selected configured runner(s) did not converge",
            updated.len() + skipped.len()
        );
    }
    if success {
        // Report the runner disposition honestly: an explicit skip is never
        // rendered as convergence, and a fleet with no configured runners is
        // distinguished from a verified convergence (#9842).
        let runner_clause = match disposition {
            RunnerConvergenceDisposition::Skipped => "runner convergence skipped",
            RunnerConvergenceDisposition::NoRunnersConfigured => "no configured runners",
            _ => "configured runners converged",
        };
        format!("Controller upgraded to {controller}{identity}; {runner_clause}")
    } else if new_version.is_some() {
        format!("Upgrade command completed but active controller is still {controller}")
    } else {
        "Upgrade command completed but active controller version could not be verified".to_string()
    }
}

fn targeted_runner_message(
    source_checkout: bool,
    controller_version: &str,
    runner_targets: &[String],
    updated: &[RunnerUpgradeEntry],
    skipped: &[RunnerUpgradeEntry],
) -> String {
    let next = format!(
        "homeboy upgrade --force{}",
        runner_targets
            .iter()
            .map(|runner| format!(" --upgrade-runner {runner}"))
            .collect::<String>()
    );
    let prefix = if source_checkout {
        "TARGETED RUNNER SCOPE: selected runners refreshed from the initiating controller source identity"
    } else {
        "TARGETED RUNNER SCOPE: selected runners refreshed without promoting the initiating controller"
    };
    if runner_convergence_failed(updated, skipped, Some(controller_version)) {
        format!("PARTIAL: {prefix}; controller remains {controller_version}. Next convergence command: {next}")
    } else {
        format!("{prefix}; controller remains {controller_version}. To promote controller and reconverge: {next}")
    }
}

/// Read extension provenance without refreshing local extension sources. Entries
/// lacking a reproducible URL or revision are forwarded as unrefreshable so the
/// runner can report that condition explicitly.
fn installed_extension_catalog() -> Vec<ExtensionUpgradeEntry> {
    installed_extension_catalog_for(&extension::available_extension_ids())
}

fn installed_extension_catalog_for(extension_ids: &[String]) -> Vec<ExtensionUpgradeEntry> {
    extension_ids
        .iter()
        .cloned()
        .map(
            |extension_id| match extension::load_extension(&extension_id) {
                Ok(manifest) => {
                    let (source_url, update_note) =
                        match extension::resolve_source_url_read_only(&extension_id) {
                            Ok(source_url) if extension::is_git_url(&source_url) => {
                                (Some(source_url), None)
                            }
                            Ok(source_url) => (
                                None,
                                Some(format!(
                                    "unrefreshable extension provenance: local source `{source_url}` cannot be materialized on a runner"
                                )),
                            ),
                            Err(err) => (
                                None,
                                Some(format!(
                                    "unrefreshable extension provenance: {}",
                                    err.message
                                )),
                            ),
                        };
                    ExtensionUpgradeEntry {
                        extension_id: extension_id.clone(),
                        old_version: manifest.version.clone(),
                        new_version: manifest.version,
                        linked: extension::is_extension_linked(&extension_id),
                        source_path: manifest.extension_path,
                        git_root: None,
                        source_url,
                        source_revision: homeboy_core::extension_update_check::read_source_revision(&extension_id),
                        source_update: homeboy_extension::ExtensionSourceUpdate {
                            update_note,
                            ..Default::default()
                        },
                    }
                }
                Err(err) => ExtensionUpgradeEntry {
                    extension_id,
                    old_version: String::new(),
                    new_version: String::new(),
                    linked: false,
                    source_path: None,
                    git_root: None,
                    source_url: None,
                    source_revision: None,
                    source_update: homeboy_extension::ExtensionSourceUpdate {
                        update_note: Some(format!(
                            "unrefreshable extension manifest: {}",
                            err.message
                        )),
                        ..Default::default()
                    },
                },
            },
        )
        .collect()
}

fn initiating_controller_source_checkout(
    source_path: Option<&Path>,
    expected_identity: &str,
) -> Result<PathBuf> {
    let checkout = if let Some(path) = source_path {
        resolve_source_workspace(Some(path))?
    } else {
        let executable_checkout = std::env::current_exe()
            .ok()
            .and_then(|path| workspace_from_executable_path(&path));
        executable_checkout
            .or_else(|| resolve_source_workspace(None).ok())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "upgrade-runner",
                    "Runner-only upgrade requires the source checkout that built the initiating controller",
                    None,
                    None,
                )
                .with_hint("Run from the controller source checkout or pass --source-path <PATH>.")
            })?
    };
    let identity =
        super::with_runner_upgrade(|provider| provider.source_checkout_build_identity(&checkout))
            .ok_or_else(|| {
            Error::validation_invalid_argument(
                "source_path",
                "Runner-only upgrade source checkout has no verifiable build identity",
                Some(checkout.display().to_string()),
                None,
            )
        })?;
    if identity != expected_identity {
        return Err(Error::validation_invalid_argument(
            "source_path",
            format!(
                "Runner-only upgrade source identity `{identity}` does not match initiating controller `{expected_identity}`"
            ),
            Some(checkout.display().to_string()),
            None,
        )
        .with_hint("Pass the exact source checkout that built the initiating controller."));
    }
    Ok(checkout)
}

fn workspace_from_executable_path(exe_path: &Path) -> Option<PathBuf> {
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

// Upgrade output must remain visible when a controller captures stdout/stderr.
// `homeboy_core::log_status!` intentionally only writes to an interactive terminal.
fn upgrade_phase(phase: &str) {
    eprintln!("[upgrade] {phase}");
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

    homeboy_core::log_status!(
        "upgrade",
        "Restarting {} binary-resident service(s) to load the new binary...",
        resident_services.len()
    );

    let (restarted, pending) =
        services::restart_resident_services(&resident_services, services::run_restart_command);

    for entry in &restarted {
        homeboy_core::log_status!("upgrade", "  {} restarted", entry.service_id);
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
            homeboy_core::log_status!(
                "upgrade",
                "  WARNING: {} not restarted ({})",
                entry.service_id,
                detail,
            );
        } else {
            homeboy_core::log_status!(
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

    homeboy_core::log_status!(
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
                    .or_else(|| homeboy_core::extension_update_check::read_source_revision(id));

                if result.linked {
                    let branch_detail = match (
                        &result.source_update.old_branch,
                        &result.source_update.new_branch,
                    ) {
                        (Some(old), Some(new)) if old != new => format!(" ({} → {})", old, new),
                        (Some(branch), _) => format!(" ({})", branch),
                        _ => String::new(),
                    };
                    homeboy_core::log_status!(
                        "upgrade",
                        "  {} {} → {} linked source updated{}",
                        id,
                        old_version,
                        new_version,
                        branch_detail
                    );
                } else if old_version != new_version {
                    homeboy_core::log_status!(
                        "upgrade",
                        "  {} {} → {}",
                        id,
                        old_version,
                        new_version
                    );
                } else {
                    homeboy_core::log_status!("upgrade", "  {} {} (up to date)", id, new_version);
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
                homeboy_core::log_status!("upgrade", "  {} skipped: {}", id, e.message);
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

    homeboy_core::log_status!(
        "upgrade",
        "WARNING: {} symlinked extension clone(s) owned by '{}' were NOT refreshed by this privileged upgrade.",
        warnings.len(),
        warnings
            .first()
            .map(|w| w.invoking_user.as_str())
            .unwrap_or("the invoking user"),
    );
    homeboy_core::log_status!(
        "upgrade",
        "  Extension resolution is $HOME-scoped, so a sudo upgrade only refreshes root's copies. Run each recovery command below to bring the symlinked clone(s) current:",
    );
    for w in &warnings {
        let behind = w
            .behind
            .map(|n| format!("{} commit(s) behind", n))
            .unwrap_or_else(|| "behind upstream".to_string());
        homeboy_core::log_status!(
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
        .join(homeboy_product_identity::PRODUCT_IDENTITY.config_dirname)
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

fn portable_extension_source_url(result: &homeboy_extension::UpdateResult) -> Option<String> {
    if let Some(git_root) = result.git_root.as_ref() {
        return git::remote_origin_url(git_root);
    }

    if extension::is_git_url(&result.url) {
        Some(result.url.clone())
    } else {
        None
    }
}

/// Deterministic source-upgrade contract:
/// - a newer source semver upgrades;
/// - an equal source semver upgrades only when both builds expose a commit and
///   dirty-state identity and those identities differ;
/// - an identical, older, or unverifiable candidate is a safe no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SourceUpgradeDecision {
    NewerVersion,
    DifferentIdentity,
    SameIdentity,
    OlderVersion,
    IdentityUnavailable,
}

impl SourceUpgradeDecision {
    fn upgrades(self) -> bool {
        matches!(self, Self::NewerVersion | Self::DifferentIdentity)
    }

    fn no_op_message(self) -> String {
        match self {
            Self::OlderVersion => "Source checkout is older than the active binary; skipping downgrade".to_string(),
            Self::IdentityUnavailable => "Source checkout cannot be compared to the active build; skipping unverifiable replacement".to_string(),
            Self::SameIdentity => "Source checkout matches the active build identity".to_string(),
            Self::NewerVersion | Self::DifferentIdentity => "Source checkout requires upgrade".to_string(),
        }
    }
}

/// Resolve the (version, identity) pair the source-upgrade decision compares
/// against. This is the *installed target* controller, so a candidate binary
/// invoking itself evaluates replacement of the binary it is trying to promote
/// over rather than comparing against its own identity (#9371).
///
/// Falls back to the in-process identity when the source path is not engaged, or
/// when the installed target cannot be identified — both keep the existing,
/// safe behavior. The fallback for an unverifiable target still routes through
/// the decision's `IdentityUnavailable` no-op rather than promoting blindly.
fn resolve_source_upgrade_target(
    source_upgrade_engaged: bool,
    previous_version: &str,
) -> (String, build_identity::BuildIdentity) {
    let current = build_identity::current();
    if !source_upgrade_engaged {
        return (previous_version.to_string(), current);
    }

    match installed_target_build_identity() {
        Ok(Some(target)) => (target.version.clone(), target),
        // No verifiable installed target: keep comparing against the in-process
        // identity. The decision then reports SameIdentity/IdentityUnavailable
        // and no-ops, which is the safe outcome for an unknown target.
        Ok(None) | Err(_) => (previous_version.to_string(), current),
    }
}

fn source_upgrade_decision(
    active_version: &str,
    active_identity: &build_identity::BuildIdentity,
    source_path: &Path,
) -> SourceUpgradeDecision {
    let Some(source) = source_build_identity(source_path) else {
        return SourceUpgradeDecision::IdentityUnavailable;
    };
    source_upgrade_decision_for_identity(active_version, active_identity, &source)
}

fn source_upgrade_decision_for_identity(
    active_version: &str,
    active_identity: &build_identity::BuildIdentity,
    source: &SourceBuildIdentity,
) -> SourceUpgradeDecision {
    let Ok(active_version) = Version::parse(active_version.trim_start_matches('v')) else {
        return SourceUpgradeDecision::IdentityUnavailable;
    };
    let Ok(source_version) = Version::parse(&source.version) else {
        return SourceUpgradeDecision::IdentityUnavailable;
    };

    if source_version > active_version {
        return SourceUpgradeDecision::NewerVersion;
    }
    if source_version < active_version {
        return SourceUpgradeDecision::OlderVersion;
    }

    if source.is_snapshot {
        return SourceUpgradeDecision::DifferentIdentity;
    }

    match (
        active_identity.git_commit.as_deref(),
        active_identity.git_dirty,
        source.git_commit.as_deref(),
        source.git_dirty,
    ) {
        (Some(active_commit), Some(active_dirty), Some(source_commit), Some(source_dirty)) => {
            if active_commit == source_commit && active_dirty == source_dirty {
                SourceUpgradeDecision::SameIdentity
            } else {
                SourceUpgradeDecision::DifferentIdentity
            }
        }
        _ => SourceUpgradeDecision::IdentityUnavailable,
    }
}

#[derive(Debug)]
struct SourceBuildIdentity {
    version: String,
    git_commit: Option<String>,
    git_dirty: Option<bool>,
    is_snapshot: bool,
}

fn source_build_identity(source_path: &Path) -> Option<SourceBuildIdentity> {
    let manifest = std::fs::read_to_string(source_path.join("Cargo.toml")).ok()?;
    let package = manifest.split("[package]").nth(1)?;
    let version = package
        .lines()
        .find_map(|line| line.trim().strip_prefix("version = "))?
        .trim_matches('"')
        .to_string();
    let is_git_checkout =
        git::output_allow_empty(source_path, &["rev-parse", "--is-inside-work-tree"]).as_deref()
            == Some("true");
    let (git_commit, git_dirty) =
        if let Some((commit, dirty)) = synthetic_snapshot_provenance(source_path) {
            (Some(commit), Some(dirty))
        } else {
            (
                git::output_allow_empty(source_path, &["rev-parse", "--short=12", "HEAD"])
                    .filter(|commit| !commit.is_empty()),
                git::output_allow_empty(source_path, &["status", "--porcelain"])
                    .map(|status| !status.is_empty()),
            )
        };

    Some(SourceBuildIdentity {
        version,
        git_commit,
        git_dirty,
        is_snapshot: !is_git_checkout,
    })
}

/// Synthetic snapshots are one-commit repositories whose Git commit identifies
/// the transport artifact. Their immutable controller identity is the recorded
/// source head in the signed snapshot note, matching product build metadata.
fn synthetic_snapshot_provenance(source_path: &Path) -> Option<(String, bool)> {
    let head = git::output_allow_empty(source_path, &["rev-parse", "HEAD"])?;
    let parents =
        git::output_allow_empty(source_path, &["rev-list", "--parents", "-n", "1", "HEAD"])?;
    let actors = git::output_allow_empty(
        source_path,
        &["show", "-s", "--format=%an <%ae>|%cn <%ce>", "HEAD"],
    )?;
    let timestamps =
        git::output_allow_empty(source_path, &["show", "-s", "--format=%at|%ct", "HEAD"])?;
    if parents.split_whitespace().count() != 1
        || actors != "Homeboy Snapshot <homeboy-snapshot@localhost>|Homeboy Snapshot <homeboy-snapshot@localhost>"
        || timestamps != "0|0"
    {
        return None;
    }
    let message = git::output_allow_empty(source_path, &["log", "-1", "--format=%B"])?;
    let snapshot = message.strip_prefix("Homeboy snapshot ")?;
    if snapshot.len() != 25
        || !snapshot.starts_with("snapshot:")
        || !is_lowercase_hex(&snapshot[9..], 16)
    {
        return None;
    }
    let note = git::output_allow_empty(
        source_path,
        &["notes", "--ref=homeboy-snapshot", "show", &head],
    )?;
    let source_head = note.strip_prefix(&format!("snapshot_identity={snapshot}\nsource_head="))?;
    let (source_head, dirty) = source_head.split_once("\nsource_dirty=")?;
    if !is_lowercase_hex(source_head, 40) {
        return None;
    }
    let dirty = match dirty {
        "true" => true,
        "false" => false,
        _ => return None,
    };
    Some((source_head[..12].to_string(), dirty))
}

// Match homeboy-product-identity's build-time snapshot provenance contract.
fn is_lowercase_hex(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

#[cfg(test)]
mod runner_source_upgrade_tests {
    use super::*;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn every_install_method_discovers_versions_from_github_releases() {
        for method in [
            InstallMethod::Homebrew,
            InstallMethod::Secondary,
            InstallMethod::Source,
            InstallMethod::Binary,
            InstallMethod::Unknown,
        ] {
            assert_eq!(latest_version_endpoint(method), GITHUB_RELEASES_API);
        }
    }

    fn write_extension(dir: &Path, id: &str, manifest: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(dir.join(format!("{id}.json")), manifest).unwrap();
    }

    #[test]
    fn installed_extension_catalog_reads_provenance_without_mutating_metadata() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extensions = home.path().join(".config/homeboy/extensions");
            write_extension(
                &extensions.join("manifest"),
                "manifest",
                r#"{"name":"manifest","version":"1.0.0","source_url":" https://example.test/manifest.git "}"#,
            );
            write_extension(
                &extensions.join("alias"),
                "alias",
                r#"{"name":"alias","version":"1.0.0","sourceUrl":"https://example.test/alias.git"}"#,
            );
            write_extension(
                &extensions.join("sidecar"),
                "sidecar",
                r#"{"name":"sidecar","version":"1.0.0"}"#,
            );
            std::fs::write(
                extensions.join("sidecar/.source-url"),
                "https://example.test/sidecar.git\n",
            )
            .unwrap();
            write_extension(
                &extensions.join("local"),
                "local",
                r#"{"name":"local","version":"1.0.0","source_url":"/Users/chris/Developer/local-extension"}"#,
            );

            let catalog = installed_extension_catalog();
            assert_eq!(catalog.len(), 4);
            assert_eq!(
                catalog
                    .iter()
                    .find(|entry| entry.extension_id == "manifest")
                    .unwrap()
                    .source_url
                    .as_deref(),
                Some("https://example.test/manifest.git")
            );
            assert_eq!(
                catalog
                    .iter()
                    .find(|entry| entry.extension_id == "alias")
                    .unwrap()
                    .source_url
                    .as_deref(),
                Some("https://example.test/alias.git")
            );
            assert_eq!(
                catalog
                    .iter()
                    .find(|entry| entry.extension_id == "sidecar")
                    .unwrap()
                    .source_url
                    .as_deref(),
                Some("https://example.test/sidecar.git")
            );
            let local = catalog
                .iter()
                .find(|entry| entry.extension_id == "local")
                .unwrap();
            assert!(local.source_url.is_none());
            assert!(local
                .source_update
                .update_note
                .as_deref()
                .unwrap()
                .contains("cannot be materialized on a runner"));
            assert!(catalog.iter().all(|entry| entry.source_revision.is_none()));
            assert!(!extensions.join("manifest/.source-url").exists());
            assert!(!extensions.join("alias/.source-url").exists());
            assert!(!extensions.join("local/.source-url").exists());
            assert_eq!(
                std::fs::read_to_string(extensions.join("sidecar/.source-url")).unwrap(),
                "https://example.test/sidecar.git\n"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn installed_extension_catalog_keeps_linked_and_broken_extensions_unrefreshable() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extensions = home.path().join(".config/homeboy/extensions");
            let linked = home.path().join("linked");
            write_extension(&linked, "linked", r#"{"name":"linked","version":"1.0.0"}"#);
            git(&linked, &["init"]);
            git(
                &linked,
                &["remote", "add", "origin", "https://example.test/linked.git"],
            );
            std::fs::create_dir_all(&extensions).unwrap();
            std::os::unix::fs::symlink(&linked, extensions.join("linked")).unwrap();
            std::os::unix::fs::symlink(home.path().join("missing"), extensions.join("broken"))
                .unwrap();

            let catalog =
                installed_extension_catalog_for(&["linked".to_string(), "broken".to_string()]);
            let linked = catalog
                .iter()
                .find(|entry| entry.extension_id == "linked")
                .unwrap();
            assert_eq!(
                linked.source_url.as_deref(),
                Some("https://example.test/linked.git")
            );
            assert!(linked.source_revision.is_none());
            let broken = catalog
                .iter()
                .find(|entry| entry.extension_id == "broken")
                .unwrap();
            assert!(broken.source_url.is_none());
            assert!(broken
                .source_update
                .update_note
                .as_deref()
                .unwrap()
                .contains("unrefreshable extension manifest"));
        });
    }

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
        for method in [
            InstallMethod::Homebrew,
            InstallMethod::Secondary,
            InstallMethod::Binary,
        ] {
            assert_eq!(runner_method_override_for_method(None, method), None);
            assert_eq!(
                runner_method_override_for_method(Some(method), method),
                Some(method)
            );
        }
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

    #[test]
    fn source_upgrade_identity_contract_is_deterministic() {
        let active = build_identity::BuildIdentity {
            version: "1.2.3".to_string(),
            git_commit: Some("active-commit".to_string()),
            git_dirty: Some(false),
            display: "homeboy 1.2.3+active-commit".to_string(),
        };
        let source =
            |version: &str, commit: Option<&str>, dirty: Option<bool>| SourceBuildIdentity {
                version: version.to_string(),
                git_commit: commit.map(str::to_string),
                git_dirty: dirty,
                is_snapshot: false,
            };

        assert_eq!(
            source_upgrade_decision_for_identity(
                "1.2.3",
                &active,
                &source("1.2.3", Some("active-commit"), Some(false)),
            ),
            SourceUpgradeDecision::SameIdentity
        );
        assert_eq!(
            source_upgrade_decision_for_identity(
                "1.2.3",
                &active,
                &source("1.2.3", Some("new-commit"), Some(false)),
            ),
            SourceUpgradeDecision::DifferentIdentity
        );
        assert_eq!(
            source_upgrade_decision_for_identity(
                "1.2.3",
                &active,
                &source("1.2.4", Some("new-commit"), Some(false)),
            ),
            SourceUpgradeDecision::NewerVersion
        );
        assert_eq!(
            source_upgrade_decision_for_identity(
                "1.2.3",
                &active,
                &source("1.2.2", Some("new-commit"), Some(false)),
            ),
            SourceUpgradeDecision::OlderVersion
        );
        assert_eq!(
            source_upgrade_decision_for_identity("1.2.3", &active, &source("1.2.3", None, None),),
            SourceUpgradeDecision::IdentityUnavailable
        );
    }

    fn write_source_manifest(path: &Path, version: &str) {
        std::fs::write(
            path.join("Cargo.toml"),
            format!("[package]\nname = \"homeboy\"\nversion = \"{version}\"\n"),
        )
        .expect("source manifest");
        std::fs::write(path.join("homeboy.json"), r#"{"id":"homeboy"}"#).expect("homeboy manifest");
    }

    fn init_git_source(path: &Path, version: &str) {
        git(path, &["init"]);
        write_source_manifest(path, version);
        git(path, &["add", "."]);
        git(
            path,
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "source",
            ],
        );
    }

    fn active_identity(commit: &str) -> build_identity::BuildIdentity {
        build_identity::BuildIdentity {
            version: "1.2.3".to_string(),
            git_commit: Some(commit.to_string()),
            git_dirty: Some(false),
            display: format!("homeboy 1.2.3+{commit}"),
        }
    }

    #[test]
    fn explicit_source_build_bypasses_release_gate_for_controller_replacement() {
        let source = tempdir().expect("source");
        init_git_source(source.path(), "1.2.3");
        assert!(source_build_identity(source.path()).is_some());

        let decision =
            source_upgrade_decision("1.2.3", &active_identity("different-build"), source.path());

        assert_eq!(decision, SourceUpgradeDecision::DifferentIdentity);
        assert!(source_upgrade_bypasses_release_gate(Some(decision)));
        assert!(controller_replacement_proceeds(
            false,
            Some(decision),
            false
        ));
    }

    #[test]
    fn source_matching_candidate_still_promotes_over_a_different_installed_target() {
        // #9371 catch-22: a source-built candidate B invoking itself compared
        // its source against its own in-process identity, reported SameIdentity,
        // and no-op'd — leaving the older installed target A in place. The
        // decision must instead compare against the *installed target*.
        let source = tempdir().expect("source");
        init_git_source(source.path(), "0.298.1");
        let candidate_commit = source_build_identity(source.path())
            .expect("source identity")
            .git_commit
            .expect("source commit");

        // Comparing the source against the CANDIDATE's own identity (the buggy
        // pre-#9371 behavior) is a SameIdentity no-op: the source built this
        // very binary.
        let against_candidate = source_upgrade_decision(
            "0.298.1",
            &active_identity(&candidate_commit),
            source.path(),
        );
        assert_eq!(against_candidate, SourceUpgradeDecision::SameIdentity);
        assert!(!against_candidate.upgrades());

        // Comparing against the older INSTALLED TARGET (A, a different commit)
        // correctly promotes — this is the managed bootstrap path.
        let against_target = source_upgrade_decision(
            "0.298.1",
            &active_identity("installed-target-a"),
            source.path(),
        );
        assert_eq!(against_target, SourceUpgradeDecision::DifferentIdentity);
        assert!(against_target.upgrades());
        assert!(controller_replacement_proceeds(
            false,
            Some(against_target),
            false
        ));
    }

    #[test]
    fn resolve_source_upgrade_target_falls_back_to_current_identity_when_not_engaged() {
        // When source upgrade is not engaged, the target resolution must not
        // re-exec any binary and simply reports the in-process identity so
        // ordinary (non-source) upgrades are unchanged.
        let (version, identity) = resolve_source_upgrade_target(false, "9.9.9");
        assert_eq!(version, "9.9.9");
        assert_eq!(identity, build_identity::current());
    }

    #[test]
    fn gitless_source_snapshot_is_an_executable_same_version_candidate() {
        let source = tempdir().expect("source");
        write_source_manifest(source.path(), "1.2.3");
        assert!(source_build_identity(source.path()).is_some());

        let decision =
            source_upgrade_decision("1.2.3", &active_identity("active-build"), source.path());

        assert_eq!(decision, SourceUpgradeDecision::DifferentIdentity);
        assert!(source_upgrade_bypasses_release_gate(Some(decision)));
    }

    #[test]
    fn dirty_explicit_source_is_rejected_before_controller_side_effects() {
        let source = tempdir().expect("source");
        init_git_source(source.path(), "1.2.3");
        std::fs::write(source.path().join("dirty"), "uncommitted").expect("dirty source");

        let error = prepare_source_workspace_for_upgrade(source.path())
            .expect_err("dirty source must be rejected during preflight");

        assert!(error.message.contains("uncommitted changes"));
    }

    #[test]
    fn synthetic_snapshot_uses_recorded_source_identity_for_parity() {
        let source = tempdir().expect("source");
        git(source.path(), &["init"]);
        write_source_manifest(source.path(), "1.2.3");
        git(source.path(), &["add", "."]);
        let commit = Command::new("git")
            .arg("-C")
            .arg(source.path())
            .args([
                "-c",
                "user.name=Homeboy Snapshot",
                "-c",
                "user.email=homeboy-snapshot@localhost",
                "commit",
                "-m",
                "Homeboy snapshot snapshot:0123456789abcdef",
            ])
            .env("GIT_AUTHOR_DATE", "1970-01-01T00:00:00 +0000")
            .env("GIT_COMMITTER_DATE", "1970-01-01T00:00:00 +0000")
            .output()
            .expect("create synthetic snapshot");
        assert!(
            commit.status.success(),
            "{}",
            String::from_utf8_lossy(&commit.stderr)
        );
        let source_head = "a".repeat(40);
        git(
            source.path(),
            &[
                "notes",
                "--ref=homeboy-snapshot",
                "add",
                "-m",
                &format!("snapshot_identity=snapshot:0123456789abcdef\nsource_head={source_head}\nsource_dirty=false"),
            ],
        );

        let decision =
            source_upgrade_decision("1.2.3", &active_identity(&source_head[..12]), source.path());

        assert_eq!(decision, SourceUpgradeDecision::SameIdentity);
        assert!(!source_upgrade_bypasses_release_gate(Some(decision)));

        for invalid_source_head in ["A".repeat(40), "g".repeat(40)] {
            git(
                source.path(),
                &[
                    "notes",
                    "--ref=homeboy-snapshot",
                    "add",
                    "-f",
                    "-m",
                    &format!("snapshot_identity=snapshot:0123456789abcdef\nsource_head={invalid_source_head}\nsource_dirty=false"),
                ],
            );

            assert!(synthetic_snapshot_provenance(source.path()).is_none());
            assert_ne!(
                source_upgrade_decision(
                    "1.2.3",
                    &active_identity(&invalid_source_head[..12]),
                    source.path(),
                ),
                SourceUpgradeDecision::SameIdentity
            );
        }
    }

    fn git(path: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(path)
            .args(args)
            .output()
            .expect("run git fixture command");
        assert!(
            output.status.success(),
            "git {:?} failed: {}{}",
            args,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
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

#[cfg(test)]
mod convergence_tests {
    use super::*;

    fn runner(previous: &str, current: &str, upgraded: bool) -> RunnerUpgradeEntry {
        RunnerUpgradeEntry {
            runner_id: "lab".to_string(),
            homeboy_path: "/opt/homeboy/homeboy".to_string(),
            success: true,
            upgraded,
            previous_version: Some(previous.to_string()),
            new_version: Some(current.to_string()),
            bare_homeboy_version: None,
            path_drift: None,
            recovery_commands: Vec::new(),
            extensions_synced: Vec::new(),
            extensions_skipped: Vec::new(),
            extensions_failed: Vec::new(),
            stale_daemon: None,
            daemon_previous_version: None,
            daemon_new_version: None,
            exit_code: 0,
            detail: String::new(),
        }
    }

    #[test]
    fn successful_but_stale_child_is_partial_controller_convergence() {
        let runner = runner("0.301.2", "0.301.2", false);

        assert!(runner_convergence_failed(
            &[runner.clone()],
            &[],
            Some("0.304.0")
        ));
        let disposition =
            runner_convergence_disposition(false, &[runner.clone()], &[], Some("0.304.0"));
        assert_eq!(disposition, RunnerConvergenceDisposition::Partial);
        assert!(
            upgrade_message(true, Some("0.304.0"), None, disposition, &[runner], &[])
                .starts_with("PARTIAL: controller upgraded to 0.304.0")
        );
    }

    #[test]
    fn skip_runners_reports_skipped_not_converged() {
        // #9842: --skip-runners must never claim convergence.
        let disposition = runner_convergence_disposition(true, &[], &[], Some("0.310.0"));
        assert_eq!(disposition, RunnerConvergenceDisposition::Skipped);
        let message = upgrade_message(true, Some("0.310.0"), None, disposition, &[], &[]);
        assert!(message.contains("runner convergence skipped"), "{message}");
        assert!(!message.contains("converged"), "{message}");
    }

    #[test]
    fn no_configured_runners_is_distinct_from_converged() {
        let disposition = runner_convergence_disposition(false, &[], &[], Some("0.310.0"));
        assert_eq!(
            disposition,
            RunnerConvergenceDisposition::NoRunnersConfigured
        );
        let message = upgrade_message(true, Some("0.310.0"), None, disposition, &[], &[]);
        assert!(message.contains("no configured runners"), "{message}");
    }

    #[test]
    fn converged_runners_still_report_convergence() {
        let runner = runner("0.301.2", "0.310.0", true);
        let disposition =
            runner_convergence_disposition(false, &[runner.clone()], &[], Some("0.310.0"));
        assert_eq!(disposition, RunnerConvergenceDisposition::Converged);
        let message = upgrade_message(true, Some("0.310.0"), None, disposition, &[runner], &[]);
        assert!(
            message.contains("configured runners converged"),
            "{message}"
        );
    }

    #[test]
    fn failed_child_is_partial_even_when_controller_is_already_current() {
        let mut runner = runner("0.304.0", "0.304.0", false);
        runner.success = false;

        assert!(runner_convergence_failed(&[], &[runner], None));
    }

    #[test]
    fn targeted_runner_only_scope_names_exact_convergence_command() {
        let message = targeted_runner_message(
            false,
            "0.301.2",
            &["lab".to_string()],
            &[runner("0.301.2", "0.301.2", false)],
            &[],
        );

        assert!(message.starts_with("TARGETED RUNNER SCOPE"));
        assert!(message.contains("controller remains 0.301.2"));
        assert!(message.contains("homeboy upgrade --force --upgrade-runner lab"));
    }

    #[test]
    fn targeted_runner_only_scope_is_partial_when_runner_remains_stale() {
        let message = targeted_runner_message(
            false,
            "0.301.2",
            &["lab".to_string()],
            &[runner("0.299.0", "0.299.0", false)],
            &[],
        );

        assert!(message.starts_with("PARTIAL: TARGETED RUNNER SCOPE"));
    }

    #[test]
    fn changed_runner_is_partial_when_final_version_still_misses_controller() {
        let runner = runner("0.299.0", "0.300.0", true);

        assert!(runner_convergence_failed(&[runner], &[], Some("0.301.2")));
    }
}
