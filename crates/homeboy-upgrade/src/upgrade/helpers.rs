use homeboy_core::defaults;
use homeboy_core::error::{Error, Result};
use homeboy_core::extension::catalog::{discover_extensions, DiscoveredExtension};
use homeboy_core::extension::lifecycle;
use homeboy_core::extension::lifecycle::is_git_url;
use homeboy_core::{build_identity, git};
use semver::Version;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

use super::constants::{GITHUB_RELEASES_API, VERSION};
use super::execution::{
    active_binary_path, execute_upgrade, installed_target_build_identity,
    installed_target_build_identity_from_disk, prepare_source_workspace_for_upgrade,
    resolve_source_workspace,
};
use super::operation::{
    persist_extension_progress, persist_upgrade_heartbeat, run_with_upgrade_heartbeats,
    UpgradeOperation, UPGRADE_PROGRESS_HEARTBEAT_INTERVAL,
};
use super::release_catalog::{self, InstallableSelection, ReleaseEntry, SelectedRelease};
use super::services;
use super::types::*;
use super::update_check::RuntimeCompatibility;
use super::validation::check_for_updates;

const CONTROLLER_UPGRADE_PROMOTION_WAIT_TIMEOUT: Duration = Duration::from_secs(20 * 60);

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

/// Read release-declared durable compatibility from the same daily update
/// source. This is deliberately not called by Cook admission: the cache is the
/// only runtime input there, so a durable command never adds a network edge.
pub fn fetch_latest_runtime_compatibility(
    expected_version: &str,
) -> Result<Option<RuntimeCompatibility>> {
    let client = reqwest::blocking::Client::builder()
        .user_agent(format!("homeboy/{}", VERSION))
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| Error::internal_io(e.to_string(), Some("create HTTP client".to_string())))?;
    let release: GitHubRelease = client
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
    if release.tag_name.trim_start_matches('v') != expected_version.trim_start_matches('v') {
        return Ok(None);
    }
    runtime_compatibility_from_release_notes(&release.body)
}

const RUNTIME_COMPATIBILITY_MARKER: &str = "<!-- homeboy-runtime-compatibility: ";

fn runtime_compatibility_from_release_notes(body: &str) -> Result<Option<RuntimeCompatibility>> {
    let Some(start) = body.find(RUNTIME_COMPATIBILITY_MARKER) else {
        return Ok(None);
    };
    let value = &body[start + RUNTIME_COMPATIBILITY_MARKER.len()..];
    let Some(json) = value.split_once(" -->").map(|(json, _)| json) else {
        return Ok(None);
    };
    serde_json::from_str(json).map(Some).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse release runtime compatibility".to_string()),
        )
    })
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

#[cfg(test)]
#[test]
fn release_notes_runtime_compatibility_is_explicit_and_versioned() {
    let compatibility = runtime_compatibility_from_release_notes(
        "release\n<!-- homeboy-runtime-compatibility: {\"schema\":\"homeboy/runtime-compatibility/v1\",\"required_contracts\":{\"snapshot\":2}} -->",
    )
    .expect("parse declaration")
    .expect("declaration");
    assert_eq!(compatibility.required_contracts["snapshot"], 2);
    assert!(runtime_compatibility_from_release_notes("release")
        .unwrap()
        .is_none());
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
    pinned_version: Option<&str>,
) -> Result<UpgradeResult> {
    if runner_only {
        if pinned_version.is_some() {
            return Err(pinned_version_scope_error(
                "--runner-only refreshes runners without promoting the controller, so there is no controller release to pin",
            ));
        }
        return run_targeted_runner_upgrade(force, method_override, runner_targets, source_path);
    }

    let mut operation = UpgradeOperation::start_durable("homeboy upgrade")?;
    let upgrade_result = run_controller_upgrade_with_operation(
        force,
        method_override,
        skip_extensions,
        skip_runners,
        skip_services,
        runner_targets,
        source_path,
        pinned_version,
        &mut operation,
    );
    match upgrade_result {
        Ok(result) => complete_upgrade_operation(operation, result),
        Err(mut error) => {
            let operation_id = operation.id().map(str::to_string);
            let terminal_result = operation.finish_failed_durable(&error);
            if let Some(operation_id) = operation_id {
                error.details["operation_id"] = serde_json::Value::String(operation_id.clone());
                if let Err(mut terminal_error) = terminal_result {
                    terminal_error.details["operation_id"] =
                        serde_json::Value::String(operation_id);
                    terminal_error.details["upgrade_error"] = serde_json::json!({
                        "code": format!("{:?}", error.code),
                        "message": error.message,
                    });
                    return Err(terminal_error);
                }
            }
            Err(error)
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn run_controller_upgrade_with_operation(
    force: bool,
    method_override: Option<InstallMethod>,
    skip_extensions: bool,
    skip_runners: bool,
    skip_services: bool,
    runner_targets: &[String],
    source_path: Option<&Path>,
    pinned_version: Option<&str>,
    operation: &mut UpgradeOperation,
) -> Result<UpgradeResult> {
    let (install_method, inferred_from_pin) =
        resolve_install_method(method_override, pinned_version, detect_install_method);

    if inferred_from_pin {
        upgrade_phase("install method: binary (inferred from --version)");
    }

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
    // Packaged and source candidates are verified before they run their own
    // target admission. Legacy admission cannot classify provenance added by a
    // selected candidate, so it must not prevent building or staging it.
    if !matches!(
        install_method,
        InstallMethod::Binary | InstallMethod::Source
    ) {
        operation.set_phase_durable("running_candidate_admission")?;
        ensure_controller_upgrade_admission()?;
    }
    validate_pinned_version(pinned_version, install_method)?;
    // Pinning is as deliberate as `--force`: it names one release, possibly an
    // older one, and must not be silently discarded by the "already at the
    // latest release" gate (#11750).
    let deliberate = controller_replacement_is_deliberate(force, pinned_version);
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
                Ok::<_, Error>(source_upgrade_decision(
                    &target_version,
                    &target_identity,
                    source_path,
                ))
            })
            .transpose()?;

    let source_noop = source_upgrade_decision.is_some_and(|decision| !decision.upgrades());

    // An approved explicit source build is the controller target, regardless of
    // whether the latest published release has the same semantic version.
    let replacement_approved =
        controller_replacement_proceeds(deliberate, source_upgrade_decision, false);

    // Resolve the binary candidate and validate installed extension sources
    // before the no-op branch can refresh extensions without swapping the
    // controller. This keeps every extension mutation behind the same gate.
    let selected_release = if install_method == InstallMethod::Binary {
        Some(resolve_binary_release(pinned_version)?)
    } else {
        None
    };
    let candidate_version = if source_noop {
        target_version.clone()
    } else if let Some(release) = selected_release.as_ref() {
        release.version.clone()
    } else if install_method == InstallMethod::Source {
        source_upgrade_path
            .as_deref()
            .and_then(source_build_identity)
            .map(|identity| identity.version)
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "source_path",
                    "Selected source checkout has no readable package version for extension compatibility preflight",
                    source_upgrade_path
                        .as_ref()
                        .map(|path| path.display().to_string()),
                    None,
                )
            })?
    } else {
        current_version().to_string()
    };
    let (convergence_runner_method, convergence_source_path) = convergence_inputs(
        source_noop,
        runner_method_override,
        source_upgrade_path.as_deref(),
    );
    let extension_preflight = (!skip_extensions)
        .then(|| preflight_extensions_for_upgrade(&candidate_version))
        .unwrap_or_default();
    if !extension_preflight.is_empty() {
        return Ok(extension_preflight_failure_result(
            install_method,
            previous_version,
            previous_build_identity,
            &candidate_version,
            extension_preflight,
        ));
    }
    let runner_preflight = if skip_runners {
        Vec::new()
    } else {
        super::with_runner_upgrade(|provider| {
            provider.preflight_configured_runners_for_upgrade(
                convergence_runner_method,
                convergence_source_path,
                // A resolved source workspace is the controller-selected build
                // input, even when it was inferred from the source install.
                convergence_source_path.is_some(),
                runner_targets,
                &candidate_version,
            )
        })?
    };
    if !runner_preflight.is_empty() {
        return Ok(runner_preflight_failure_result(
            install_method,
            previous_version,
            previous_build_identity,
            runner_preflight,
        ));
    }

    // Check if a release update is available unless an explicit source target
    // has already been approved for controller replacement.
    if source_noop || !replacement_approved {
        let replacement_now_required = if source_noop {
            controller_replacement_required_after_discovery(
                true,
                deliberate,
                source_upgrade_decision,
                false,
            )
        } else {
            let check = check_for_updates()?;
            controller_replacement_required_after_discovery(
                false,
                deliberate,
                source_upgrade_decision,
                check.update_available,
            )
        };
        if !replacement_now_required {
            // Even when no binary update is needed, still run extension updates.
            let selection_guard =
                acquire_controller_selection_guard(operation, "controller extension refresh")?;
            let initial_completion = reconcile_controller_identity(
                observed_installed_controller_identity()?,
                previous_build_identity.as_deref(),
                Some(previous_version.as_str()),
            )?;
            let (mut extensions_updated, mut extension_skips) =
                if !controller_allows_extension_refresh(false, &initial_completion) {
                    operation.mark_extensions(
                        "not_run",
                        "controller changed before extension refresh admission",
                    );
                    (vec![], vec![])
                } else if skip_extensions {
                    operation.mark_extensions("skipped", "extension refresh skipped");
                    (vec![], vec![])
                } else {
                    operation.set_phase_durable("refreshing installed extensions")?;
                    let (updated, skipped) = update_all_extensions(operation.id())?;
                    let status = extension_component_status(true, false, &updated, &skipped);
                    operation.mark_extensions(&status.status, &status.summary);
                    (updated, skipped)
                };
            let mut completion;
            drop(selection_guard);
            let (mut runners_updated, mut runners_skipped) = if skip_runners {
                (vec![], vec![])
            } else {
                let promotion_lease =
                    acquire_controller_upgrade_lease(operation, "controller upgrade completion")?;
                completion = reconcile_controller_identity(
                    observed_installed_controller_identity()?,
                    previous_build_identity.as_deref(),
                    Some(previous_version.as_str()),
                )?;
                if completion.superseded {
                    (vec![], vec![])
                } else {
                    operation.set_phase_durable("refreshing configured runners")?;
                    super::with_runner_upgrade(|p| {
                        p.upgrade_configured_runners_with_explicit_source_path(
                            force,
                            convergence_runner_method,
                            convergence_source_path,
                            convergence_source_path.is_some(),
                            None,
                            runner_targets,
                            &extensions_updated,
                            Some(&promotion_lease),
                        )
                    })?
                }
            };
            let final_selection_guard =
                acquire_controller_selection_guard(operation, "controller upgrade result")?;
            completion = reconcile_controller_identity(
                observed_installed_controller_identity()?,
                previous_build_identity.as_deref(),
                Some(previous_version.as_str()),
            )?;
            if completion.superseded {
                extensions_updated.clear();
                extension_skips.clear();
                runners_updated.clear();
                runners_skipped.clear();
            }
            let partial = runner_convergence_failed(
                &runners_updated,
                &runners_skipped,
                completion.version.as_deref(),
            );
            let runner_disposition = runner_convergence_disposition(
                runner_completion_is_skipped(skip_runners, true, completion.superseded),
                &runners_updated,
                &runners_skipped,
                completion.version.as_deref(),
            );
            let extensions_status = if completion.superseded {
                superseded_extension_status()
            } else {
                extension_component_status(
                    !skip_extensions,
                    skip_extensions,
                    &extensions_updated,
                    &extension_skips,
                )
            };
            let mut result = UpgradeResult {
                command: "upgrade".to_string(),
                install_method,
                previous_version: previous_version.clone(),
                new_version: completion.version.clone(),
                previous_build_identity,
                new_build_identity: completion.build_identity.clone(),
                source_revision: None,
                upgraded: false,
                outcome: Some(
                    if completion.superseded {
                        "controller_superseded"
                    } else {
                        "controller_unchanged"
                    }
                    .to_string(),
                ),
                preflight: None,
                controller: Some(component_status(
                    if completion.superseded {
                        "superseded"
                    } else {
                        "unchanged"
                    },
                    if completion.superseded {
                        "controller changed while optional extension refresh was running"
                    } else {
                        "controller already current"
                    },
                )),
                extensions: Some(extensions_status.clone()),
                runners: Some(if completion.superseded {
                    superseded_runner_status()
                } else {
                    runner_component_status(
                        runner_disposition,
                        &runners_updated,
                        &runners_skipped,
                        false,
                    )
                }),
                partial,
                runner_convergence: Some(runner_disposition),
                message: if completion.superseded {
                    superseded_completion_message(completion.build_identity.as_deref())
                } else {
                    format!(
                        "{}{}",
                        match runner_disposition {
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
                                "Already at latest version; configured runners converged"
                                    .to_string()
                            }
                        },
                        extension_partial_clause(Some(&extensions_status)),
                    )
                },
                restart_required: false,
                extensions_unrefreshed: warn_unrefreshed_symlinked_extensions(&extensions_updated),
                extensions_updated,
                extensions_skipped: extension_skips
                    .iter()
                    .map(|skip| skip.extension_id.clone())
                    .collect(),
                extension_skips,
                runners_updated,
                runners_skipped,
                // No binary swap happened, so resident services already run the
                // current binary and need no restart.
                services_restarted: Vec::new(),
                services_pending_restart: Vec::new(),
                operation_id: None,
            };
            if completion.superseded {
                invalidate_superseded_evidence(&mut result);
            }
            finish_completed_under_selection(operation, &result, final_selection_guard)?;
            return Ok(result);
        }
    }

    // Keep the same promotion authority from compatibility revalidation through
    // the controller swap. Source builds retain their isolated build target, but
    // their final install shares this lease rather than opening a TOCTOU window.
    let controller_mutation_lease =
        acquire_controller_upgrade_lease(operation, "controller upgrade")?;
    super::operation::freeze_prior_pending_replacements(
        operation
            .id()
            .expect("controller upgrades require a durable operation"),
    )?;
    controller_mutation_lease.assert_generation()?;
    let extension_revalidation = (!skip_extensions)
        .then(|| preflight_extensions_for_upgrade(&candidate_version))
        .unwrap_or_default();
    if !extension_revalidation.is_empty() {
        let result = extension_preflight_failure_result(
            install_method,
            previous_version,
            previous_build_identity,
            &candidate_version,
            extension_revalidation,
        );
        return Ok(result);
    }
    operation.set_phase_durable("mutating controller")?;
    let controller_upgrade = run_controller_mutation_after_runner_preflight(
        runner_preflight,
        || {
            controller_mutation_lease.assert_generation()?;
            if skip_runners {
                Ok(Vec::new())
            } else {
                super::with_runner_upgrade(|provider| {
                    provider.preflight_configured_runners_for_upgrade(
                        runner_method_override,
                        source_upgrade_path.as_deref(),
                        source_upgrade_path.is_some(),
                        runner_targets,
                        &candidate_version,
                    )
                })
            }
        },
        || {
            let operation_id = operation
                .id()
                .expect("controller upgrades require a durable operation")
                .to_string();
            let lease_heartbeat_failure = std::sync::Mutex::new(None);
            let progress_heartbeat_failure = std::sync::Mutex::new(None);
            let phase_sync = std::sync::Mutex::new(());
            let execution_started = std::time::Instant::now();
            let result = run_with_upgrade_heartbeats(
                UPGRADE_PROGRESS_HEARTBEAT_INTERVAL,
                |elapsed| {
                    let _phase = phase_sync.lock().expect("serialize upgrade progress");
                    if let Err(error) = controller_mutation_lease.heartbeat() {
                        let mut failure = lease_heartbeat_failure
                            .lock()
                            .expect("record lease heartbeat failure");
                        if failure.is_none() {
                            *failure = Some(error);
                        }
                    }
                    if let Err(error) = persist_upgrade_heartbeat(&operation_id, elapsed) {
                        let mut failure = progress_heartbeat_failure
                            .lock()
                            .expect("record progress heartbeat failure");
                        if failure.is_none() {
                            *failure = Some(error);
                        }
                    }
                },
                || {
                    let operation = std::cell::RefCell::new(&mut *operation);
                    let mut report_phase = |phase: &str| {
                        let _phase = phase_sync.lock().expect("serialize upgrade progress");
                        let result = operation.borrow_mut().set_phase_durable(phase);
                        if result.is_ok() {
                            *progress_heartbeat_failure
                                .lock()
                                .expect("clear recovered progress heartbeat failure") = None;
                        }
                        result
                    };
                    let mut replacement_checkpoint =
                        |checkpoint: &super::execution::ReplacementCheckpoint| {
                            let _phase = phase_sync.lock().expect("serialize upgrade progress");
                            operation
                                .borrow_mut()
                                .record_replacement_checkpoint_durable(checkpoint)
                        };
                    execute_upgrade(
                        install_method,
                        source_upgrade_path.as_deref(),
                        source_path.is_some(),
                        force,
                        deliberate,
                        previous_build_identity.as_deref(),
                        selected_release.as_ref(),
                        Some(&controller_mutation_lease),
                        &mut report_phase,
                        &mut replacement_checkpoint,
                    )
                },
            );
            controller_mutation_lease.assert_generation()?;
            *lease_heartbeat_failure
                .lock()
                .expect("clear recovered lease heartbeat failure") = None;
            persist_upgrade_heartbeat(&operation_id, execution_started.elapsed())?;
            *progress_heartbeat_failure
                .lock()
                .expect("clear recovered progress heartbeat failure") = None;
            if let Some(error) = lease_heartbeat_failure
                .into_inner()
                .expect("lease heartbeat failures are not poisoned")
            {
                return Err(error);
            }
            if let Some(error) = progress_heartbeat_failure
                .into_inner()
                .expect("progress heartbeat failures are not poisoned")
            {
                return Err(error);
            }
            result
        },
    )?;
    let (success, new_version, new_build_identity, source_revision, superseded) =
        match controller_upgrade {
            Ok(result) => result,
            Err(runners_skipped) => {
                let result = runner_preflight_failure_result(
                    install_method,
                    previous_version,
                    previous_build_identity,
                    runners_skipped,
                );
                return Ok(result);
            }
        };
    controller_mutation_lease.assert_generation()?;
    let upgrade_completed = !superseded && should_sync_after_upgrade(new_version.as_deref());
    if upgrade_completed {
        // This is deliberately a short switch, not a drain: records admitted
        // before it retain their immutable runtime pin and remain executable.
        operation.set_phase_durable("rotating_controller_generation")?;
        let installed_executable = super::execution::active_binary_path()?;
        homeboy_core::controller_runtime::activate_installed_generation(&installed_executable)?;
        if success {
            operation.mark_controller_promoted_durable("controller installation completed")?;
        }
    } else if superseded {
        operation.mark_controller_durable(
            "superseded",
            "controller promotion was superseded by the active build",
        )?;
    } else {
        operation.mark_controller_durable("failed", "controller installation did not complete")?;
    }
    drop(controller_mutation_lease);

    let selection_guard =
        acquire_controller_selection_guard(operation, "controller extension refresh")?;
    let initial_completion = reconcile_controller_identity(
        observed_installed_controller_identity()?,
        new_build_identity.as_deref(),
        new_version.as_deref(),
    )?;

    // Auto-update all installed extensions after the upgrade command completes.
    // This prevents CI/local extension version drift that causes baseline
    // mismatches and inconsistent audit findings.
    let (mut extensions_updated, mut extension_skips) =
        if !controller_allows_extension_refresh(superseded, &initial_completion) {
            operation.mark_extensions(
                "not_run",
                "controller changed before extension refresh admission",
            );
            (vec![], vec![])
        } else if upgrade_completed && !skip_extensions {
            operation.set_phase_durable("refreshing installed extensions")?;
            let (updated, skipped) = update_all_extensions(operation.id())?;
            let status = extension_component_status(true, false, &updated, &skipped);
            operation.mark_extensions(&status.status, &status.summary);
            (updated, skipped)
        } else if skip_extensions {
            operation.mark_extensions("skipped", "extension refresh skipped");
            (vec![], vec![])
        } else {
            operation.mark_extensions("not_run", "extension refresh was not attempted");
            (vec![], vec![])
        };

    drop(selection_guard);

    let (mut runners_updated, mut runners_skipped) = if upgrade_completed && !skip_runners {
        let promotion_lease =
            acquire_controller_upgrade_lease(operation, "controller upgrade completion")?;
        promotion_lease.assert_generation()?;
        let completion = reconcile_controller_identity(
            observed_installed_controller_identity()?,
            new_build_identity.as_deref(),
            new_version.as_deref(),
        )?;
        let runner_completion_superseded = superseded || completion.superseded;
        if runner_completion_superseded {
            operation.mark_controller_durable(
                "superseded",
                "controller changed before configured runner convergence",
            )?;
            (vec![], vec![])
        } else {
            operation.set_phase_durable("refreshing configured runners")?;
            super::with_runner_upgrade(|p| {
                p.upgrade_configured_runners_with_explicit_source_path(
                    force,
                    runner_method_override,
                    source_upgrade_path.as_deref(),
                    source_upgrade_path.is_some(),
                    new_build_identity.as_deref(),
                    runner_targets,
                    &extensions_updated,
                    Some(&promotion_lease),
                )
            })?
        }
    } else {
        (vec![], vec![])
    };

    // After a verified binary swap, long-running services still hold the old
    // binary in memory until restarted. Restart each declared resident service
    // (config-driven; nothing is hardcoded in core) unless restarts were
    // skipped, in which case report each as pending with its recovery command.
    let (services_restarted, services_pending_restart) =
        restart_resident_services_after_swap(success, skip_services);

    let final_selection_guard =
        acquire_controller_selection_guard(operation, "controller upgrade result")?;
    let completion = reconcile_controller_identity(
        observed_installed_controller_identity()?,
        new_build_identity.as_deref(),
        new_version.as_deref(),
    )?;
    let completion_superseded = superseded || completion.superseded;
    if completion_superseded {
        operation.mark_controller_durable(
            "superseded",
            "controller changed before upgrade completion was recorded",
        )?;
        extensions_updated.clear();
        extension_skips.clear();
        runners_updated.clear();
        runners_skipped.clear();
    }

    let runner_disposition = runner_convergence_disposition(
        runner_completion_is_skipped(skip_runners, upgrade_completed, completion_superseded),
        &runners_updated,
        &runners_skipped,
        completion.version.as_deref(),
    );

    let extensions_status = if completion_superseded {
        superseded_extension_status()
    } else {
        extension_component_status(
            upgrade_completed,
            skip_extensions,
            &extensions_updated,
            &extension_skips,
        )
    };

    let mut result = UpgradeResult {
        command: "upgrade".to_string(),
        install_method,
        previous_version,
        new_version: completion.version.clone(),
        previous_build_identity,
        new_build_identity: completion.build_identity.clone(),
        source_revision,
        upgraded: success,
        outcome: Some(if completion_superseded {
            "controller_superseded".to_string()
        } else {
            upgrade_outcome(success, runner_disposition).to_string()
        }),
        preflight: None,
        controller: Some(component_status(
            if completion_superseded {
                "superseded"
            } else if success {
                "updated"
            } else {
                "failed"
            },
            if completion_superseded {
                "controller promotion was superseded by the active build"
            } else if success {
                "controller installation completed"
            } else {
                "controller installation did not complete"
            },
        )),
        extensions: Some(extensions_status.clone()),
        runners: Some(if completion_superseded {
            superseded_runner_status()
        } else {
            runner_component_status(
                runner_disposition,
                &runners_updated,
                &runners_skipped,
                !upgrade_completed && !skip_runners,
            )
        }),
        partial: runner_convergence_failed(
            &runners_updated,
            &runners_skipped,
            completion.version.as_deref(),
        ),
        runner_convergence: Some(runner_disposition),
        message: if completion_superseded {
            superseded_completion_message(completion.build_identity.as_deref())
        } else {
            upgrade_message(
                success,
                completion.version.as_deref(),
                completion.build_identity.as_deref(),
                runner_disposition,
                &runners_updated,
                &runners_skipped,
                Some(&extensions_status),
            )
        },
        // Source replacement updates the on-disk executable, but this command
        // exits immediately afterwards. Re-execing only `--version` skips the
        // normal completion path and provides no lifecycle benefit.
        restart_required: false,
        extensions_unrefreshed: warn_unrefreshed_symlinked_extensions(&extensions_updated),
        extensions_updated,
        extensions_skipped: extension_skips
            .iter()
            .map(|skip| skip.extension_id.clone())
            .collect(),
        extension_skips,
        runners_updated,
        runners_skipped,
        services_restarted,
        services_pending_restart,
        operation_id: None,
    };
    if completion_superseded {
        invalidate_superseded_evidence(&mut result);
    }
    finish_completed_under_selection(operation, &result, final_selection_guard)?;
    Ok(result)
}

#[cfg(test)]
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
        outcome: Some("controller_unchanged".to_string()),
        preflight: None,
        controller: Some(component_status("unchanged", "controller was not promoted")),
        extensions: Some(component_status("skipped", "extensions were not refreshed")),
        runners: Some(component_status(
            "not_run",
            "runner convergence was not attempted",
        )),
        partial: false,
        // No upgrade occurred, so there is no runner-convergence claim to make.
        runner_convergence: None,
        message: decision.no_op_message(),
        restart_required: false,
        extensions_updated: Vec::new(),
        extensions_skipped: Vec::new(),
        extension_skips: Vec::new(),
        runners_updated: Vec::new(),
        runners_skipped: Vec::new(),
        extensions_unrefreshed: Vec::new(),
        services_restarted: Vec::new(),
        services_pending_restart: Vec::new(),
        operation_id: None,
    }
}

fn run_controller_mutation_after_runner_preflight<T>(
    runner_preflight: Vec<RunnerUpgradeEntry>,
    revalidate_runners: impl FnOnce() -> Result<Vec<RunnerUpgradeEntry>>,
    mutate_controller: impl FnOnce() -> Result<T>,
) -> Result<std::result::Result<T, Vec<RunnerUpgradeEntry>>> {
    if !runner_preflight.is_empty() {
        return Ok(Err(runner_preflight));
    }
    let revalidation = revalidate_runners()?;
    if !revalidation.is_empty() {
        return Ok(Err(revalidation));
    }
    mutate_controller().map(Ok)
}

fn runner_preflight_failure_result(
    install_method: InstallMethod,
    previous_version: String,
    previous_build_identity: Option<String>,
    runners_skipped: Vec<RunnerUpgradeEntry>,
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
        outcome: Some("runner_preflight_failed".to_string()),
        preflight: None,
        controller: Some(component_status(
            "runner_preflight_failed",
            "controller was not updated because selected runner preflight failed",
        )),
        extensions: Some(component_status(
            "not_run",
            "extensions were not refreshed because runner preflight failed",
        )),
        runners: Some(component_status(
            "runner_preflight_failed",
            "one or more selected runners could not prepare the source checkout",
        )),
        partial: true,
        runner_convergence: Some(RunnerConvergenceDisposition::Partial),
        message: "runner_preflight_failed: controller was not updated".to_string(),
        restart_required: false,
        extensions_updated: Vec::new(),
        extensions_skipped: Vec::new(),
        extension_skips: Vec::new(),
        runners_updated: Vec::new(),
        runners_skipped,
        extensions_unrefreshed: Vec::new(),
        services_restarted: Vec::new(),
        services_pending_restart: Vec::new(),
        operation_id: None,
    }
}

fn extension_preflight_failure_result(
    install_method: InstallMethod,
    previous_version: String,
    previous_build_identity: Option<String>,
    candidate_version: &str,
    extension_blockers: Vec<ExtensionPreflightBlocker>,
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
        outcome: Some("extension_preflight_failed".to_string()),
        preflight: Some(UpgradePreflight {
            candidate_version: candidate_version.to_string(),
            extension_blockers,
        }),
        controller: Some(component_status(
            "extension_preflight_failed",
            "controller was not updated because extension preflight failed",
        )),
        extensions: Some(component_status(
            "extension_preflight_failed",
            "one or more installed extensions cannot be safely refreshed",
        )),
        runners: Some(component_status(
            "not_run",
            "runner convergence was not attempted because extension preflight failed",
        )),
        partial: true,
        runner_convergence: None,
        message: "extension_preflight_failed: controller was not updated".to_string(),
        restart_required: false,
        extensions_updated: Vec::new(),
        extensions_skipped: Vec::new(),
        extension_skips: Vec::new(),
        runners_updated: Vec::new(),
        runners_skipped: Vec::new(),
        extensions_unrefreshed: Vec::new(),
        services_restarted: Vec::new(),
        services_pending_restart: Vec::new(),
        operation_id: None,
    }
}

/// Check deterministic local extension conditions before the controller swap.
/// Network refresh, setup, and runner convergence remain in their established
/// phases; this gate only rejects failures that cannot be repaired by a retry.
fn preflight_extensions_for_upgrade(candidate_version: &str) -> Vec<ExtensionPreflightBlocker> {
    discover_extensions()
        .into_iter()
        .filter_map(|discovered| match discovered {
            DiscoveredExtension::Invalid(failure) => {
                let extension_id = failure.id;
                Some(ExtensionPreflightBlocker {
                    extension_id: extension_id.clone(),
                    classification: failure.category.to_string(),
                    detail: failure.diagnostic.to_string(),
                    recovery_command: format!("homeboy extension show {extension_id}"),
                })
            }
            DiscoveredExtension::Valid(manifest) => {
                let extension_id = manifest.id.clone();
                let source_path = manifest.extension_path.as_deref().map(Path::new)?;
                if homeboy_core::extension::catalog::is_extension_linked(&extension_id) {
                    if let Some(blocker) =
                        linked_extension_source_blocker(&extension_id, source_path)
                    {
                        return Some(blocker);
                    }
                }
                let requires = manifest
                    .requires
                    .as_ref()
                    .and_then(|requirements| requirements.homeboy.as_deref());
                match homeboy_extension_contract::evaluate_core_compatibility_for_version(
                    requires,
                    homeboy_core::extension::lifecycle::read_source_revision(&extension_id),
                    candidate_version,
                ) {
                    Ok(report) if report.status != "incompatible" => None,
                    Ok(report) => Some(ExtensionPreflightBlocker {
                        extension_id,
                        classification: "controller_version_incompatible".to_string(),
                        detail: format!(
                            "requires homeboy {} but selected controller is {}",
                            report
                                .requires_homeboy
                                .unwrap_or_else(|| "<undeclared>".to_string()),
                            candidate_version
                        ),
                        recovery_command: "homeboy upgrade --skip-extensions".to_string(),
                    }),
                    Err(error) => Some(ExtensionPreflightBlocker {
                        extension_id,
                        classification: "manifest_compatibility_invalid".to_string(),
                        detail: error.message,
                        recovery_command: "homeboy extension show <extension-id>".to_string(),
                    }),
                }
            }
        })
        .collect()
}

/// A linked extension is refreshable from either a Git checkout or Homeboy's
/// registered durable source. Canonical paths make moved config roots and
/// symlinked source roots deterministic while rejecting links that escape the
/// registered extension workspace.
fn linked_extension_source_blocker(
    extension_id: &str,
    extension_path: &Path,
) -> Option<ExtensionPreflightBlocker> {
    if git::get_git_root(&extension_path.to_string_lossy()).is_ok() {
        return None;
    }

    let registered_root = homeboy_core::paths::extension_source_root(extension_id)
        .ok()
        .and_then(|root| root.canonicalize().ok());
    let source_path = extension_path.canonicalize().ok();
    let is_registered_source = matches!(
        (registered_root.as_deref(), source_path.as_deref()),
        (Some(root), Some(source)) if source.starts_with(root)
    );
    if is_registered_source
        && homeboy_core::extension::lifecycle::read_source_revision(extension_id).is_some()
    {
        return None;
    }

    let (classification, detail) = if source_path.is_none() {
        (
            "linked_source_missing",
            "linked extension source cannot be resolved",
        )
    } else if registered_root.is_none() {
        (
            "linked_source_root_unrecognized",
            "linked extension source is not registered in the local extension workspace",
        )
    } else if !is_registered_source {
        (
            "linked_source_root_unrecognized",
            "linked extension source escapes its registered local extension workspace",
        )
    } else {
        (
            "linked_source_revision_missing",
            "registered linked extension source has no resolvable installed revision",
        )
    };
    Some(ExtensionPreflightBlocker {
        extension_id: extension_id.to_string(),
        classification: classification.to_string(),
        detail: format!("{detail}: {}", extension_path.display()),
        recovery_command: format!("homeboy extension relink {extension_id} <path>"),
    })
}

fn upgrade_outcome(
    success: bool,
    runner_disposition: RunnerConvergenceDisposition,
) -> &'static str {
    if success && runner_disposition == RunnerConvergenceDisposition::Partial {
        "controller_updated_runner_failed"
    } else if success {
        "controller_updated"
    } else {
        "controller_update_failed"
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

/// A pinned release is an explicit instruction, including an explicit move to
/// an older release. Treating it like `--force` at the release gate is what
/// makes `--version <TAG>` mean anything: otherwise pinning a release that is
/// not newer than the running one reports "already at latest" and installs
/// nothing (#11750).
fn controller_replacement_is_deliberate(force: bool, pinned_version: Option<&str>) -> bool {
    force || pinned_version.is_some()
}

/// A published release pin identifies the binary transport when the operator
/// leaves method selection implicit. Explicit methods remain authoritative so
/// incompatible combinations reach validation and fail closed.
fn resolve_install_method<F>(
    method_override: Option<InstallMethod>,
    pinned_version: Option<&str>,
    detect: F,
) -> (InstallMethod, bool)
where
    F: FnOnce() -> InstallMethod,
{
    match method_override {
        Some(method) => (method, false),
        None if pinned_version.is_some() => (InstallMethod::Binary, true),
        None => (detect(), false),
    }
}

/// `--version` pins a published *release asset*, so it is only meaningful for
/// the install method that downloads one. Silently ignoring it for a source or
/// Homebrew install would be the same class of defect as the 404: the operator
/// asks for one release and gets another.
fn validate_pinned_version(pinned_version: Option<&str>, method: InstallMethod) -> Result<()> {
    let Some(requested) = pinned_version else {
        return Ok(());
    };
    if method == InstallMethod::Binary {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "version",
        format!(
            "--version pins a published release asset, which the {} install method does not download",
            method.as_str()
        ),
        Some(requested.to_string()),
        None,
    )
    .with_hint(format!(
        "Pin a release binary explicitly with: homeboy upgrade --method binary --version {requested}"
    ))
    .with_hint("Source installs converge on a checkout, not a release: homeboy upgrade --method source --source-path <PATH>"))
}

fn pinned_version_scope_error(problem: &str) -> Error {
    Error::validation_invalid_argument("version", problem, None, None)
        .with_hint("Drop --version, or run the controller upgrade without --runner-only.")
}

/// Decide which published release a binary upgrade installs.
///
/// Without a pin this is the newest release that ships an asset for the running
/// target — not simply the newest release. A release published without this
/// platform's asset used to make `upgrade` 404 with no way past it (#11750);
/// falling back one release is the difference between a stuck controller and a
/// controller five minor versions ahead.
fn resolve_binary_release(pinned_version: Option<&str>) -> Result<SelectedRelease> {
    let target = release_catalog::running_target_triple();
    let releases = release_catalog::fetch_release_catalog()?;

    if let Some(requested) = pinned_version {
        return resolve_pinned_release(&releases, requested, target);
    }

    let selection = release_catalog::select_installable(&releases, target);
    let Some(installable) = selection.installable.clone() else {
        return Err(no_installable_release_error(&selection, target));
    };

    match target {
        Some(target) => {
            if let Some(notice) = selection.upgrade_fallback_notice(target) {
                homeboy_core::log_status!("upgrade", "{}", notice);
            }
        }
        None => warn_unverified_target(),
    }

    Ok(SelectedRelease::new(installable, target))
}

fn resolve_pinned_release(
    releases: &[ReleaseEntry],
    requested: &str,
    target: Option<&str>,
) -> Result<SelectedRelease> {
    let Some(release) = release_catalog::find_release(releases, requested) else {
        return Err(Error::validation_invalid_argument(
            "version",
            format!("No published Homeboy release matches {requested}"),
            Some(requested.to_string()),
            Some(release_catalog::installable_tags(releases, target, 5)),
        )
        .with_hint(
            "Releases are tagged `v<major>.<minor>.<patch>`; both `v0.332.0` and `0.332.0` are accepted.",
        )
        .with_hint("List what can be installed here with: homeboy upgrade --check"));
    };

    match target {
        Some(target) => {
            if release_catalog::release_installs_on(releases, requested, Some(target))
                == Some(false)
            {
                return Err(Error::validation_invalid_argument(
                    "version",
                    format!("Release {} ships no {target} asset", release.tag),
                    Some(release.tag.clone()),
                    Some(release_catalog::installable_tags(releases, Some(target), 5)),
                )
                .with_hint(format!(
                    "Looked for asset: {}",
                    release_catalog::asset_archive_name(target)
                ))
                .with_hint("Pin one of the releases listed above, or build from source with: homeboy upgrade --method source --source-path <PATH>"));
            }
        }
        None => warn_unverified_target(),
    }

    Ok(SelectedRelease::new(release, target))
}

/// An undetermined running target is reported, not guessed. Every downstream
/// message then says asset availability was *not verified*, instead of
/// implying a triple nobody established.
fn warn_unverified_target() {
    homeboy_core::log_status!(
        "upgrade",
        "The running target triple could not be determined ({}); release asset availability was not verified.",
        release_catalog::running_platform_description()
    );
}

fn no_installable_release_error(selection: &InstallableSelection, target: Option<&str>) -> Error {
    let newest = selection
        .newest
        .as_ref()
        .map(|release| release.tag.clone())
        .unwrap_or_else(|| "none published".to_string());
    let described = target
        .map(str::to_string)
        .unwrap_or_else(release_catalog::running_platform_description);

    Error::internal_unexpected(format!(
        "no published Homeboy release ships an installable asset for {described} (newest release: {newest})"
    ))
    .with_hint("Inspect what this platform can install with: homeboy upgrade --check")
    .with_hint("Build from source instead with: homeboy upgrade --method source --source-path <PATH>")
}

/// Reject only ownership that remains live or cannot be verified. Stale records
/// keep their durable audit trail and pinned runtime without blocking a binary
/// replacement or being silently reconciled.
fn ensure_controller_upgrade_admission() -> Result<()> {
    let admission = super::ensure_controller_upgrade_admission()?;
    if admission.allows_controller_replacement() {
        let unhealthy_records = ["malformed", "legacy", "conflicting"]
            .into_iter()
            .map(|key| admission.record_health[key].as_u64().unwrap_or(0))
            .sum::<u64>();
        if unhealthy_records != 0 {
            homeboy_core::log_status!(
                "upgrade",
                "agent-task record health: {} malformed, {} legacy, {} conflicting (samples capped)",
                admission.record_health["malformed"].as_u64().unwrap_or(0),
                admission.record_health["legacy"].as_u64().unwrap_or(0),
                admission.record_health["conflicting"].as_u64().unwrap_or(0),
            );
        }
        return Ok(());
    }
    unreachable!("failed admission returns before this point")
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
            None,
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
        outcome: Some("runner_only".to_string()),
        preflight: None,
        controller: Some(component_status(
            "unchanged",
            "targeted runner operation did not promote controller",
        )),
        extensions: Some(component_status(
            "skipped",
            "controller extensions were not refreshed",
        )),
        runners: Some(runner_component_status(
            runner_convergence_disposition(
                false,
                &runners_updated,
                &runners_skipped,
                Some(previous_version.as_str()),
            ),
            &runners_updated,
            &runners_skipped,
            false,
        )),
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
        extension_skips: Vec::new(),
        runners_updated,
        runners_skipped,
        extensions_unrefreshed: Vec::new(),
        services_restarted: Vec::new(),
        services_pending_restart: Vec::new(),
        operation_id: None,
    })
}

fn component_status(status: &str, summary: &str) -> UpgradeComponentStatus {
    UpgradeComponentStatus {
        status: status.to_string(),
        summary: summary.to_string(),
    }
}

fn superseded_extension_status() -> UpgradeComponentStatus {
    component_status(
        "not_run",
        "extension evidence was invalidated after controller supersession",
    )
}

fn superseded_runner_status() -> UpgradeComponentStatus {
    component_status(
        "not_run",
        "runner evidence was invalidated after controller supersession",
    )
}

fn superseded_completion_message(active_identity: Option<&str>) -> String {
    match active_identity {
        Some(identity) => format!(
            "Controller operation was superseded by active build {identity}; extension and runner evidence was invalidated"
        ),
        None => "Controller operation was superseded; extension and runner evidence was invalidated"
            .to_string(),
    }
}

fn invalidate_superseded_evidence(result: &mut UpgradeResult) {
    result.extensions = Some(superseded_extension_status());
    result.runners = Some(superseded_runner_status());
    result.partial = false;
    result.runner_convergence = Some(RunnerConvergenceDisposition::Skipped);
    result.message = superseded_completion_message(result.new_build_identity.as_deref());
    result.extensions_updated.clear();
    result.extensions_skipped.clear();
    result.extension_skips.clear();
    result.extensions_unrefreshed.clear();
    result.runners_updated.clear();
    result.runners_skipped.clear();
}

fn extension_component_status(
    attempted: bool,
    skipped_by_flag: bool,
    updated: &[ExtensionUpgradeEntry],
    skipped: &[ExtensionUpgradeSkip],
) -> UpgradeComponentStatus {
    let status = if skipped_by_flag {
        "skipped"
    } else if !attempted {
        "not_run"
    } else if skipped.is_empty() {
        "completed"
    } else {
        "partial"
    };
    // A bare count buries a real failure: when anything was skipped, the
    // summary carries each extension id with the reason it was skipped, so a
    // `partial` extension outcome is self-explanatory (#12181).
    let summary = if skipped.is_empty() {
        format!("{} updated, {} skipped", updated.len(), skipped.len())
    } else {
        let detail = skipped
            .iter()
            .map(|skip| format!("{}: {}", skip.extension_id, skip.reason))
            .collect::<Vec<_>>()
            .join("; ");
        format!(
            "{} updated, {} skipped ({})",
            updated.len(),
            skipped.len(),
            detail
        )
    };
    component_status(status, &summary)
}

fn runner_component_status(
    disposition: RunnerConvergenceDisposition,
    updated: &[RunnerUpgradeEntry],
    skipped: &[RunnerUpgradeEntry],
    blocked_by_controller: bool,
) -> UpgradeComponentStatus {
    if blocked_by_controller {
        return component_status(
            "not_run",
            "controller installation prevented runner convergence",
        );
    }
    let status = match disposition {
        RunnerConvergenceDisposition::Converged => "converged",
        RunnerConvergenceDisposition::Partial => "partial",
        RunnerConvergenceDisposition::Skipped => "skipped",
        RunnerConvergenceDisposition::NoRunnersConfigured => "not_configured",
    };
    component_status(
        status,
        &format!(
            "{} converged, {} require repair",
            updated.len(),
            skipped.len()
        ),
    )
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

/// Human clause appended to the upgrade banner when extensions were only
/// partially refreshed, so a real extension failure is prominent rather than a
/// passing count after a success banner (#12181). Empty when the extension
/// outcome is anything other than `partial` (deliberately skipped, not run,
/// completed).
fn extension_partial_clause(extensions: Option<&UpgradeComponentStatus>) -> String {
    match extensions.filter(|status| status.status == "partial") {
        Some(status) => format!(". EXTENSIONS PARTIAL: {}", status.summary),
        None => String::new(),
    }
}

fn upgrade_message(
    success: bool,
    new_version: Option<&str>,
    new_build_identity: Option<&str>,
    disposition: RunnerConvergenceDisposition,
    updated: &[RunnerUpgradeEntry],
    skipped: &[RunnerUpgradeEntry],
    extensions: Option<&UpgradeComponentStatus>,
) -> String {
    let controller = new_version.unwrap_or("unverified");
    let identity = new_build_identity
        .map(|identity| format!(" ({identity})"))
        .unwrap_or_default();
    let base = if disposition == RunnerConvergenceDisposition::Partial {
        format!(
            "PARTIAL: controller upgraded to {controller}{identity}, but {} selected configured runner(s) did not converge",
            updated.len() + skipped.len()
        )
    } else if success {
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
    };
    let clause = extension_partial_clause(extensions);
    if clause.is_empty() {
        base
    } else if base.ends_with('.') {
        // Avoid a doubled period when the base already ends in one.
        format!("{}{}", base.trim_end_matches('.'), clause)
    } else {
        format!("{base}{clause}")
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
    installed_extension_catalog_for(&homeboy_core::extension::catalog::available_extension_ids())
}

fn installed_extension_catalog_for(extension_ids: &[String]) -> Vec<ExtensionUpgradeEntry> {
    extension_ids
        .iter()
        .cloned()
        .map(
            |extension_id| match homeboy_core::extension::catalog::load_extension(&extension_id) {
                Ok(manifest) => {
                    let (source_url, update_note) =
                        match lifecycle::source_metadata::resolve_source_url_read_only(&extension_id)
                        {
                            Ok(source_url) if is_git_url(&source_url) => {
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
                        linked: homeboy_core::extension::catalog::is_extension_linked(&extension_id),
                        source_path: manifest.extension_path,
                        git_root: None,
                        source_url,
                        source_revision: homeboy_core::extension::lifecycle::read_source_revision(&extension_id),
                        source_update: homeboy_extension_contract::update_output::ExtensionSourceUpdate {
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
                    source_update: homeboy_extension_contract::update_output::ExtensionSourceUpdate {
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

fn convergence_inputs(
    source_noop: bool,
    runner_method: Option<InstallMethod>,
    source_path: Option<&Path>,
) -> (Option<InstallMethod>, Option<&Path>) {
    if source_noop {
        (None, None)
    } else {
        (runner_method, source_path)
    }
}

pub(crate) fn should_sync_after_upgrade(new_version: Option<&str>) -> bool {
    new_version.is_some()
}

#[derive(Debug, PartialEq, Eq)]
struct ControllerCompletionIdentity {
    superseded: bool,
    version: Option<String>,
    build_identity: Option<String>,
}

fn reconcile_controller_identity(
    active: Option<build_identity::BuildIdentity>,
    expected_build_identity: Option<&str>,
    expected_version: Option<&str>,
) -> Result<ControllerCompletionIdentity> {
    let active = active.ok_or_else(|| {
        Error::internal_unexpected(
            "active controller identity is unavailable after synchronized refresh",
        )
        .with_hint("Retry after the PATH-active controller reports a readable build identity.")
    })?;
    let matches_expected = match expected_build_identity {
        Some(expected) => active.display == expected,
        None => expected_version.is_some_and(|expected| active.version == expected),
    };
    Ok(ControllerCompletionIdentity {
        superseded: !matches_expected,
        version: Some(active.version),
        build_identity: Some(active.display),
    })
}

fn observed_installed_controller_identity() -> Result<Option<build_identity::BuildIdentity>> {
    let target = active_binary_path()?;
    installed_target_build_identity_from_disk(&target)
}

fn controller_allows_extension_refresh(
    already_superseded: bool,
    completion: &ControllerCompletionIdentity,
) -> bool {
    !already_superseded && !completion.superseded
}

fn runner_completion_is_skipped(
    skip_runners: bool,
    upgrade_completed: bool,
    completion_superseded: bool,
) -> bool {
    skip_runners || !upgrade_completed || completion_superseded
}

fn controller_replacement_required_after_discovery(
    source_noop: bool,
    deliberate: bool,
    source_decision: Option<SourceUpgradeDecision>,
    update_available: bool,
) -> bool {
    !source_noop && controller_replacement_proceeds(deliberate, source_decision, update_available)
}

fn complete_upgrade_operation(
    mut operation: UpgradeOperation,
    mut result: UpgradeResult,
) -> Result<UpgradeResult> {
    let operation_id = operation.id().map(str::to_string);
    result.operation_id = operation_id.clone();
    if let Err(mut error) = operation.finish_completed_durable(&result) {
        if let Some(operation_id) = operation_id {
            error.details["operation_id"] = serde_json::Value::String(operation_id);
        }
        return Err(error);
    }
    Ok(result)
}

fn finish_completed_under_selection(
    operation: &mut UpgradeOperation,
    result: &UpgradeResult,
    selection_guard: homeboy_core::runtime_promotion::RuntimeSelectionGuard,
) -> Result<()> {
    let terminal = match operation.finish_completed_durable(result) {
        Ok(()) => Ok(()),
        Err(completion_error) => {
            match operation.replace_pending_terminal_with_failure(&completion_error) {
                Ok(()) => Err(completion_error),
                Err(mut terminal_error) => {
                    terminal_error.details["completion_error"] = serde_json::json!({
                        "code": format!("{:?}", completion_error.code),
                        "message": completion_error.message,
                    });
                    Err(terminal_error)
                }
            }
        }
    };
    drop(selection_guard);
    terminal
}

fn acquire_controller_selection_guard(
    operation: &mut UpgradeOperation,
    operation_name: &str,
) -> Result<homeboy_core::runtime_promotion::RuntimeSelectionGuard> {
    let operation_id = operation
        .id()
        .ok_or_else(|| Error::internal_unexpected("controller selection requires an operation"))?
        .to_string();
    let guard = homeboy_core::runtime_promotion::protect_runtime_selection_waiting_with_status(
        operation_name,
        "active controller",
        homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
            status_command: format!("homeboy upgrade status {operation_id}"),
            operation_id,
        },
        CONTROLLER_UPGRADE_PROMOTION_WAIT_TIMEOUT,
        |event| operation.record_promotion_wait(&event),
    )?;
    operation.clear_promotion_wait_durable()?;
    Ok(guard)
}

fn acquire_controller_upgrade_lease(
    operation: &mut UpgradeOperation,
    operation_name: &str,
) -> Result<homeboy_core::runtime_promotion::RuntimePromotionLease> {
    let lease = if let Some(operation_id) = operation.id().map(str::to_string) {
        let status_command = format!("homeboy upgrade status {operation_id}");
        homeboy_core::runtime_promotion::acquire_waiting_for_target_with_status(
            operation_name,
            "active controller",
            homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
                operation_id,
                status_command,
            },
            CONTROLLER_UPGRADE_PROMOTION_WAIT_TIMEOUT,
            |event| operation.record_promotion_wait(&event),
        )?
    } else {
        homeboy_core::runtime_promotion::acquire_waiting_for_compatible(
            operation_name,
            "active controller",
            CONTROLLER_UPGRADE_PROMOTION_WAIT_TIMEOUT,
            |event| operation.record_promotion_wait(&event),
        )?
    };
    operation.clear_promotion_wait_durable()?;
    operation.take_persistence_error()?;
    Ok(lease)
}

/// Update all installed extensions. Best-effort — failures are logged, and the
/// extension is added to the skipped list carrying its error reason so the
/// structured result can say *why* it was skipped (#12181).
fn update_all_extensions(
    operation_id: Option<&str>,
) -> Result<(Vec<ExtensionUpgradeEntry>, Vec<ExtensionUpgradeSkip>)> {
    let extension_ids = homeboy_core::extension::catalog::available_extension_ids();
    if extension_ids.is_empty() {
        return Ok((vec![], vec![]));
    }

    homeboy_core::log_status!(
        "upgrade",
        "Updating {} installed extension(s)...",
        extension_ids.len()
    );

    let mut updated = Vec::new();
    let mut skipped = Vec::new();

    for (index, id) in extension_ids.iter().enumerate() {
        let old_version = homeboy_core::extension::catalog::load_extension(id)
            .ok()
            .map(|m| m.version.clone())
            .unwrap_or_default();

        let current = index + 1;
        let total = extension_ids.len();
        report_extension_progress(operation_id, id, current, total, Duration::ZERO)?;
        let progress_failure = std::sync::Mutex::new(None);
        let update_result = run_with_upgrade_heartbeats(
            UPGRADE_PROGRESS_HEARTBEAT_INTERVAL,
            |elapsed| {
                if let Err(error) =
                    report_extension_progress(operation_id, id, current, total, elapsed)
                {
                    let mut failure = progress_failure
                        .lock()
                        .expect("record extension progress failure");
                    if failure.is_none() {
                        *failure = Some(error);
                    }
                }
            },
            || lifecycle::update(id, false),
        );
        if let Some(error) = progress_failure
            .into_inner()
            .expect("extension progress failures are not poisoned")
        {
            return Err(error);
        }

        match update_result {
            Ok(result) => {
                let new_version = homeboy_core::extension::catalog::load_extension(id)
                    .ok()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();
                let source_url = portable_extension_source_url(&result);
                let source_revision = result
                    .source_update
                    .new_source_revision
                    .clone()
                    .or_else(|| homeboy_core::extension::lifecycle::read_source_revision(id));

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
                skipped.push(ExtensionUpgradeSkip {
                    extension_id: id.clone(),
                    reason: e.message,
                });
            }
        }
    }

    Ok((updated, skipped))
}

fn report_extension_progress(
    operation_id: Option<&str>,
    extension_id: &str,
    current: usize,
    total: usize,
    elapsed: Duration,
) -> Result<()> {
    if let Some(run_id) = operation_id {
        return persist_extension_progress(run_id, extension_id, current, total, elapsed);
    }
    upgrade_phase(&super::operation::upgrade_extension_progress_message(
        extension_id,
        current,
        total,
        elapsed,
    ));
    Ok(())
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
        "  Extension resolution is $HOME-scoped, so a sudo upgrade only refreshes root's copies. Inspect each clone below and run the recovery command shown:",
    );
    for w in &warnings {
        let behind = w
            .behind
            .map(|n| format!("{} commit(s) behind", n))
            .unwrap_or_else(|| "behind upstream".to_string());
        if w.dirty {
            let detail = if w.dirty_paths.is_empty() {
                "worktree state could not be verified".to_string()
            } else {
                w.dirty_paths.join(", ")
            };
            homeboy_core::log_status!(
                "upgrade",
                "  {} ({}, BLOCKED by uncommitted changes: {}) -> {}: {}",
                w.extension_id,
                behind,
                detail,
                w.source_path,
                w.recovery_command,
            );
            homeboy_core::log_status!(
                "upgrade",
                "  Resolve the uncommitted changes in the {} clone before refreshing; a `git pull --ff-only` cannot succeed while the checkout holds them.",
                w.extension_id,
            );
        } else {
            homeboy_core::log_status!(
                "upgrade",
                "  {} ({}) -> {}: {}",
                w.extension_id,
                behind,
                w.source_path,
                w.recovery_command,
            );
        }
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
/// recovery command. Detection is read-only: it may fetch and inspect, and it
/// never resets, stashes, checks out, or otherwise mutates a worktree owned by
/// another user. Returns an empty vec when not running under `sudo`, when the
/// invoking user's config dir is absent, or when nothing is stale.
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
    detect_unrefreshed_in_extensions_dir(&extensions_dir, &sudo_user, refreshed)
}

/// The symlink-scan half of [`detect_unrefreshed_symlinked_extensions`],
/// separated so tests can drive detection against an isolated fixture dir
/// instead of a real user's home (#12181).
fn detect_unrefreshed_in_extensions_dir(
    extensions_dir: &Path,
    sudo_user: &str,
    refreshed: &[ExtensionUpgradeEntry],
) -> Vec<UnrefreshedExtensionWarning> {
    let Ok(entries) = std::fs::read_dir(extensions_dir) else {
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
            // A `git pull --ff-only` is guaranteed to abort when the worktree
            // holds uncommitted changes, so before emitting that recovery
            // command check the same generated-metadata tolerance the update
            // gate uses. A dirty clone is reported as blocked with its
            // offending paths named; an unreadable status is treated as
            // blocked rather than risking a command we cannot verify (#12181).
            let dirty_paths = lifecycle::extension_update_dirty_paths(&git_root, &target);
            // Unknown status is blocked (like the update gate) rather than
            // risking a recovery command we cannot verify.
            let dirty = dirty_paths.as_ref().is_none_or(|paths| !paths.is_empty());
            let dirty_paths = dirty_paths.unwrap_or_default();
            warnings.push(UnrefreshedExtensionWarning {
                extension_id,
                invoking_user: sudo_user.to_string(),
                symlink_path: symlink_path.to_string_lossy().to_string(),
                source_path: git_root.to_string_lossy().to_string(),
                behind,
                dirty,
                dirty_paths,
                recovery_command: if dirty {
                    // No single command can safely refresh a dirty clone; point
                    // the user at the state they must resolve first.
                    format!("sudo -u {} git -C {} status", sudo_user, git_root.display())
                } else {
                    format!(
                        "sudo -u {} git -C {} pull --ff-only",
                        sudo_user,
                        git_root.display()
                    )
                },
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

fn portable_extension_source_url(
    result: &homeboy_core::extension::lifecycle::UpdateResult,
) -> Option<String> {
    if let Some(git_root) = result.git_root.as_ref() {
        return git::remote_origin_url(git_root);
    }

    if is_git_url(&result.url) {
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
pub(crate) enum SourceUpgradeDecision {
    NewerVersion,
    DifferentIdentity,
    SameIdentity,
    OlderVersion,
    IdentityUnavailable,
}

impl SourceUpgradeDecision {
    pub(crate) fn upgrades(self) -> bool {
        matches!(self, Self::NewerVersion | Self::DifferentIdentity)
    }

    #[cfg(test)]
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

pub(crate) fn source_upgrade_decision(
    active_version: &str,
    active_identity: &build_identity::BuildIdentity,
    source_path: &Path,
) -> SourceUpgradeDecision {
    let Some(source) = source_build_identity(source_path) else {
        return SourceUpgradeDecision::IdentityUnavailable;
    };
    source_upgrade_decision_for_identity(active_version, active_identity, &source)
}

/// Re-evaluate an already-built source candidate immediately before promotion.
/// A Git candidate may replace an installed Git revision only when the active
/// revision is an ancestor of its source HEAD; divergent or older candidates
/// are safe no-ops unless the caller explicitly forces a downgrade.
pub(crate) fn source_promotion_decision(
    active_identity: &build_identity::BuildIdentity,
    source_path: &Path,
) -> SourceUpgradeDecision {
    let decision = source_upgrade_decision(&active_identity.version, active_identity, source_path);
    if decision != SourceUpgradeDecision::DifferentIdentity
        || git::output_allow_empty(source_path, &["rev-parse", "--is-inside-work-tree"]).as_deref()
            != Some("true")
    {
        return decision;
    }

    let Some(active_commit) = active_identity.git_commit.as_deref() else {
        return SourceUpgradeDecision::IdentityUnavailable;
    };
    let active_object = format!("{active_commit}^{{commit}}");
    let object_exists = Command::new("git")
        .arg("-C")
        .arg(source_path)
        .args(["cat-file", "-e", &active_object])
        .status()
        .is_ok_and(|status| status.success());
    if !object_exists {
        return SourceUpgradeDecision::IdentityUnavailable;
    }

    Command::new("git")
        .arg("-C")
        .arg(source_path)
        .args(["merge-base", "--is-ancestor", active_commit, "HEAD"])
        .status()
        .is_ok_and(|status| status.success())
        .then_some(SourceUpgradeDecision::DifferentIdentity)
        .unwrap_or(SourceUpgradeDecision::OlderVersion)
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
    use std::cell::RefCell;
    use std::process::Command;
    use tempfile::tempdir;

    #[test]
    fn controller_upgrade_errors_preserve_the_durable_operation_id() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let error = run_upgrade_with_method(
                false,
                Some(InstallMethod::Unknown),
                true,
                true,
                true,
                false,
                &[],
                None,
                None,
            )
            .expect_err("unknown method fails after operation admission");
            let operation_id = error.details["operation_id"]
                .as_str()
                .expect("error preserves operation identity");
            let status = super::super::operation::load_upgrade_operation_status(Some(operation_id))
                .expect("failed operation remains inspectable");
            assert_eq!(status.operation_id, operation_id);
            assert_eq!(status.status, "error");
            assert_eq!(status.phase, "failed");
        });
    }

    #[test]
    fn completion_persistence_failure_retries_the_same_operation_id() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let operation_id = operation.id().expect("durable operation").to_string();
            operation.fail_next_terminal_write();
            let result = source_upgrade_noop_result(
                InstallMethod::Source,
                "0.367.2".to_string(),
                Some("0.367.2+old".to_string()),
                SourceUpgradeDecision::SameIdentity,
            );

            let result = complete_upgrade_operation(operation, result)
                .expect("one-shot persistence failure is retried");
            assert_eq!(result.operation_id.as_deref(), Some(operation_id.as_str()));
            let status =
                super::super::operation::load_upgrade_operation_status(Some(&operation_id))
                    .expect("operation remains inspectable");
            assert_eq!(status.status, "pass");
            assert_eq!(status.phase, "completed");
        });
    }

    #[test]
    fn final_identity_and_terminal_cas_block_the_exact_controller_contender_interleaving() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let operation_id = operation.id().expect("durable operation").to_string();
            let selection_guard =
                homeboy_core::runtime_promotion::protect_runtime_selection_with_status(
                    "controller upgrade result",
                    "active controller",
                    homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
                        operation_id: operation_id.clone(),
                        status_command: format!("homeboy upgrade status {operation_id}"),
                    },
                )
                .expect("protect final identity");
            let result = source_upgrade_noop_result(
                InstallMethod::Source,
                "0.367.2".to_string(),
                Some("0.367.2+selected".to_string()),
                SourceUpgradeDecision::SameIdentity,
            );
            let (waiting_tx, waiting_rx) = std::sync::mpsc::channel();
            let acquired = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let contender_acquired = acquired.clone();
            let contender = std::thread::spawn(move || {
                let lease =
                    homeboy_core::runtime_promotion::acquire_waiting_for_target_with_status(
                        "contending controller upgrade",
                        "active controller",
                        homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
                            operation_id: "contender".to_string(),
                            status_command: "homeboy upgrade status contender".to_string(),
                        },
                        std::time::Duration::from_secs(2),
                        |event| {
                            if event.wait_stage == "os_lock" {
                                let _ = waiting_tx.send(());
                            }
                        },
                    )
                    .expect("contender acquires after terminal CAS");
                contender_acquired.store(true, std::sync::atomic::Ordering::Release);
                drop(lease);
            });
            let progress_id = operation_id.clone();
            let acquired_before_terminal = acquired.clone();
            operation.before_terminal_write(move || {
                waiting_rx
                    .recv_timeout(std::time::Duration::from_secs(1))
                    .expect("contender reaches final-read/terminal-CAS window");
                assert!(
                    !acquired_before_terminal.load(std::sync::atomic::Ordering::Acquire),
                    "contender cannot supersede identity before completion"
                );
                super::super::operation::persist_extension_progress(
                    &progress_id,
                    "wordpress",
                    1,
                    2,
                    std::time::Duration::from_secs(1),
                )
                .expect("force terminal metadata CAS retry");
            });

            finish_completed_under_selection(&mut operation, &result, selection_guard)
                .expect("terminal CAS completes under selection guard");
            contender.join().expect("contender exits");
            assert!(acquired.load(std::sync::atomic::Ordering::Acquire));
            let status =
                super::super::operation::load_upgrade_operation_status(Some(&operation_id))
                    .expect("load terminal operation");
            assert_eq!(status.status, "pass");
            assert_eq!(status.phase, "completed");
        });
    }

    #[test]
    fn exhausted_completion_cas_is_frozen_as_failure_before_selection_release() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let operation_id = operation.id().expect("durable operation").to_string();
            operation.fail_next_terminal_writes(3);
            let selection_guard =
                homeboy_core::runtime_promotion::protect_runtime_selection_with_status(
                    "controller upgrade result",
                    "active controller",
                    homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
                        operation_id: operation_id.clone(),
                        status_command: format!("homeboy upgrade status {operation_id}"),
                    },
                )
                .expect("protect final identity");
            let result = source_upgrade_noop_result(
                InstallMethod::Source,
                "0.367.2".to_string(),
                Some("0.367.2+selected".to_string()),
                SourceUpgradeDecision::SameIdentity,
            );

            finish_completed_under_selection(&mut operation, &result, selection_guard)
                .expect_err("completion CAS exhaustion remains an operation error");
            let status =
                super::super::operation::load_upgrade_operation_status(Some(&operation_id))
                    .expect("load terminal operation");
            assert_eq!(status.status, "error");
            assert_eq!(status.phase, "failed");
        });
    }

    #[test]
    fn exhausted_completion_and_failure_cas_retain_a_retryable_failure_intent() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let operation_id = operation.id().expect("durable operation").to_string();
            operation.fail_next_terminal_writes(6);
            let selection_guard =
                homeboy_core::runtime_promotion::protect_runtime_selection_with_status(
                    "controller upgrade result",
                    "active controller",
                    homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
                        operation_id: operation_id.clone(),
                        status_command: format!("homeboy upgrade status {operation_id}"),
                    },
                )
                .expect("protect final identity");
            let result = source_upgrade_noop_result(
                InstallMethod::Source,
                "0.367.2".to_string(),
                Some("0.367.2+selected".to_string()),
                SourceUpgradeDecision::SameIdentity,
            );

            let terminal_error =
                finish_completed_under_selection(&mut operation, &result, selection_guard)
                    .expect_err("both bounded terminal attempts are exhausted");
            operation
                .finish_failed_durable(&terminal_error)
                .expect("outer error handling retries the retained failure intent");
            let status =
                super::super::operation::load_upgrade_operation_status(Some(&operation_id))
                    .expect("load terminal operation");
            assert_eq!(status.status, "error");
            assert_eq!(status.phase, "failed");
        });
    }

    #[test]
    fn supersession_reports_the_actual_active_controller_identity() {
        let completion = reconcile_controller_identity(
            Some(build_identity::BuildIdentity {
                version: "0.369.0".to_string(),
                git_commit: Some("replacement".to_string()),
                git_dirty: Some(false),
                display: "0.369.0+replacement".to_string(),
            }),
            Some("0.368.0+candidate"),
            Some("0.368.0"),
        )
        .expect("active replacement identity is readable");

        assert!(completion.superseded);
        assert_eq!(completion.version.as_deref(), Some("0.369.0"));
        assert_eq!(
            completion.build_identity.as_deref(),
            Some("0.369.0+replacement")
        );
        assert!(!controller_allows_extension_refresh(false, &completion));
    }

    #[test]
    fn skip_runners_supersession_uses_the_active_post_refresh_identity() {
        let completion = reconcile_controller_identity(
            Some(build_identity::BuildIdentity {
                version: "0.369.0".to_string(),
                git_commit: Some("replacement".to_string()),
                git_dirty: Some(false),
                display: "0.369.0+replacement".to_string(),
            }),
            Some("0.368.0+baseline"),
            Some("0.368.0"),
        )
        .expect("active replacement identity is readable");

        assert!(runner_completion_is_skipped(
            true,
            true,
            completion.superseded
        ));
        assert_eq!(completion.version.as_deref(), Some("0.369.0"));
        assert_eq!(
            completion.build_identity.as_deref(),
            Some("0.369.0+replacement")
        );
    }

    #[test]
    fn missing_active_identity_is_unknown_not_supersession() {
        let error = reconcile_controller_identity(None, Some("0.368.0+candidate"), Some("0.368.0"))
            .expect_err("missing identity cannot prove supersession");
        assert!(error.message.contains("identity is unavailable"));
    }

    #[test]
    fn source_noop_uses_the_refresh_and_convergence_path() {
        assert!(!controller_replacement_required_after_discovery(
            true,
            false,
            Some(SourceUpgradeDecision::SameIdentity),
            true,
        ));
        assert!(!controller_replacement_required_after_discovery(
            true,
            false,
            Some(SourceUpgradeDecision::OlderVersion),
            true,
        ));
        assert_eq!(
            convergence_inputs(
                true,
                Some(InstallMethod::Source),
                Some(Path::new("/tmp/rejected-source")),
            ),
            (None, None),
            "a rejected source checkout must not drive runner convergence"
        );
    }

    #[test]
    fn changed_runner_compatibility_revalidation_precedes_controller_mutation() {
        let events = RefCell::new(Vec::new());
        let changed_manifest_requires_homeboy = ">=3.0.0";
        let result = run_controller_mutation_after_runner_preflight(
            Vec::new(),
            || {
                events.borrow_mut().push("runner manifest revalidation");
                let compatible =
                    homeboy_extension_contract::evaluate_core_compatibility_for_version(
                        Some(changed_manifest_requires_homeboy),
                        None,
                        "2.1.0",
                    )?
                    .status
                        != "incompatible";
                Ok((!compatible)
                    .then(|| RunnerUpgradeEntry {
                        runner_id: "lab-a".to_string(),
                        homeboy_path: "homeboy".to_string(),
                        success: false,
                        upgraded: false,
                        previous_version: None,
                        new_version: None,
                        bare_homeboy_version: None,
                        path_drift: None,
                        recovery_commands: Vec::new(),
                        extensions_synced: Vec::new(),
                        extensions_skipped: Vec::new(),
                        extensions_failed: Vec::new(),
                        stale_daemon: None,
                        daemon_previous_version: None,
                        daemon_new_version: None,
                        exit_code: 1,
                        detail: "runner manifest changed to an incompatible requirement"
                            .to_string(),
                    })
                    .into_iter()
                    .collect())
            },
            || {
                events.borrow_mut().push("controller mutation");
                Ok(())
            },
        )
        .expect("preflight evaluation");

        let runners_skipped = result.unwrap_err();
        assert_eq!(*events.borrow(), ["runner manifest revalidation"]);
        assert_eq!(runners_skipped[0].runner_id, "lab-a");
        let upgrade = runner_preflight_failure_result(
            InstallMethod::Source,
            "0.1.0".to_string(),
            None,
            runners_skipped,
        );
        assert_eq!(upgrade.outcome.as_deref(), Some("runner_preflight_failed"));
    }

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

    #[cfg(unix)]
    fn link_registered_extension(home: &Path, id: &str, source: &Path) {
        let extensions = home.join(".config/homeboy/extensions");
        write_extension(source, id, r#"{"name":"linked","version":"1.0.0"}"#);
        std::fs::create_dir_all(&extensions).expect("extensions directory");
        std::os::unix::fs::symlink(source, extensions.join(id)).expect("linked extension");
        std::fs::write(
            extensions.join(format!(".{id}.source-revision")),
            "fixture-revision",
        )
        .expect("registered revision");
    }

    #[test]
    fn extension_preflight_returns_bounded_manifest_blocker() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extensions = home.path().join(".config/homeboy/extensions");
            write_extension(&extensions.join("broken"), "broken", "{ malformed");

            let blockers = preflight_extensions_for_upgrade("1.0.0");
            assert_eq!(blockers.len(), 1);
            assert_eq!(blockers[0].extension_id, "broken");
            assert_eq!(blockers[0].classification, "manifest_json_malformed");
            assert_eq!(
                blockers[0].recovery_command,
                "homeboy extension show broken"
            );
        });
    }

    #[test]
    fn extension_preflight_blocks_a_manifest_incompatible_with_the_candidate() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extensions = home.path().join(".config/homeboy/extensions");
            write_extension(
                &extensions.join("future"),
                "future",
                r#"{"name":"future","version":"1.0.0","requires":{"homeboy":">=999.0.0"}}"#,
            );

            let blockers = preflight_extensions_for_upgrade("2.0.0");
            assert_eq!(blockers.len(), 1);
            assert_eq!(
                blockers[0].classification,
                "controller_version_incompatible"
            );
            assert!(blockers[0].detail.contains("selected controller is 2.0.0"));
            assert_eq!(
                blockers[0].recovery_command,
                "homeboy upgrade --skip-extensions"
            );
        });
    }

    #[test]
    fn extension_preflight_allows_a_compatible_copied_manifest() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extensions = home.path().join(".config/homeboy/extensions");
            write_extension(
                &extensions.join("compatible"),
                "compatible",
                r#"{"name":"compatible","version":"1.0.0","requires":{"homeboy":">=1.0.0"}}"#,
            );

            assert!(preflight_extensions_for_upgrade("1.0.0").is_empty());
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_preflight_allows_registered_linked_local_source() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = home
                .path()
                .join(".config/homeboy/extension-sources/linked/linked");
            link_registered_extension(home.path(), "linked", &source);

            assert!(preflight_extensions_for_upgrade("1.0.0").is_empty());
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_preflight_allows_moved_registered_source_root() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let config = home.path().join(".config/homeboy");
            let moved_sources = home.path().join("moved-extension-sources");
            let source = moved_sources.join("moved/moved");
            std::fs::create_dir_all(&config).expect("config directory");
            std::os::unix::fs::symlink(&moved_sources, config.join("extension-sources"))
                .expect("moved source root");
            link_registered_extension(home.path(), "moved", &source);

            assert!(preflight_extensions_for_upgrade("1.0.0").is_empty());
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_preflight_allows_symlinked_registered_source_root() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let root = home
                .path()
                .join(".config/homeboy/extension-sources/symlinked");
            let target = home.path().join("registered-source-target");
            std::fs::create_dir_all(root.parent().expect("source parent")).expect("source parent");
            std::os::unix::fs::symlink(&target, &root).expect("registered source link");
            link_registered_extension(home.path(), "symlinked", &target.join("symlinked"));

            assert!(preflight_extensions_for_upgrade("1.0.0").is_empty());
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_preflight_rejects_unregistered_external_linked_source() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = home.path().join("external/external");
            link_registered_extension(home.path(), "external", &source);

            let blockers = preflight_extensions_for_upgrade("1.0.0");
            assert_eq!(
                blockers[0].classification,
                "linked_source_root_unrecognized"
            );
            assert!(blockers[0].detail.contains("registered"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_preflight_rejects_traversal_outside_registered_source_root() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let root = home
                .path()
                .join(".config/homeboy/extension-sources/traversal");
            let source = root.join("../outside/traversal");
            link_registered_extension(home.path(), "traversal", &source);

            let blockers = preflight_extensions_for_upgrade("1.0.0");
            assert_eq!(
                blockers[0].classification,
                "linked_source_root_unrecognized"
            );
            assert!(blockers[0].detail.contains("escapes"));
        });
    }

    #[test]
    fn extension_preflight_reports_missing_linked_source_without_mutation() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let path = home.path().join("missing-linked-source");
            let blocker = linked_extension_source_blocker("missing", &path).expect("blocker");

            assert_eq!(blocker.classification, "linked_source_missing");
            assert!(!path.exists(), "preflight must not create a missing source");
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_preflight_dry_run_does_not_mutate_registered_source() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source = home
                .path()
                .join(".config/homeboy/extension-sources/dry-run/dry-run");
            link_registered_extension(home.path(), "dry-run", &source);

            assert!(preflight_extensions_for_upgrade("1.0.0").is_empty());
            assert!(
                !source.join(".source-url").exists(),
                "preflight must not write source metadata"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn extension_preflight_resolves_a_nested_linked_source_through_its_git_root() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let source_root = home.path().join("extension-source");
            let source = source_root.join("packages/nested");
            std::fs::create_dir_all(&source).expect("nested source");
            write_source_manifest(&source_root, "1.0.0");
            write_extension(&source, "nested", r#"{"name":"nested","version":"1.0.0"}"#);
            git(&source_root, &["init"]);
            git(&source_root, &["add", "."]);
            git(
                &source_root,
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

            let extensions = home.path().join(".config/homeboy/extensions");
            std::fs::create_dir_all(&extensions).expect("extensions directory");
            std::os::unix::fs::symlink(&source, extensions.join("nested"))
                .expect("linked extension");

            assert!(preflight_extensions_for_upgrade("1.0.0").is_empty());
        });
    }

    #[test]
    fn extension_preflight_failure_does_not_invoke_controller_mutation() {
        homeboy_core::test_support::with_isolated_home(|home| {
            let extensions = home.path().join(".config/homeboy/extensions");
            write_extension(
                &extensions.join("incompatible"),
                "incompatible",
                r#"{"name":"incompatible","version":"1.0.0","requires":{"homeboy":">=3.0.0"}}"#,
            );

            let mut controller_mutated = false;
            let blockers = preflight_extensions_for_upgrade("2.1.0");
            let result = if blockers.is_empty() {
                controller_mutated = true;
                Ok(())
            } else {
                Err(extension_preflight_failure_result(
                    InstallMethod::Binary,
                    "2.0.0".to_string(),
                    None,
                    "2.1.0",
                    blockers,
                ))
            };

            assert!(!controller_mutated, "controller installation must not run");
            let result = result.expect_err("incompatible extension blocks upgrade");
            assert_eq!(
                result.outcome.as_deref(),
                Some("extension_preflight_failed")
            );
            assert_eq!(result.preflight.unwrap().extension_blockers.len(), 1);
        });
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
    fn source_promotion_requires_candidate_descend_from_active_revision() {
        let source = tempdir().expect("source");
        init_git_source(source.path(), "1.2.3");
        let first =
            git::output_allow_empty(source.path(), &["rev-parse", "HEAD"]).expect("first revision");
        std::fs::write(source.path().join("next"), "next").expect("next source");
        git(source.path(), &["add", "."]);
        git(
            source.path(),
            &[
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.test",
                "commit",
                "-m",
                "next",
            ],
        );
        let second = git::output_allow_empty(source.path(), &["rev-parse", "HEAD"])
            .expect("second revision");

        assert_eq!(
            source_promotion_decision(&active_identity(&first), source.path()),
            SourceUpgradeDecision::DifferentIdentity
        );

        git(source.path(), &["reset", "--hard", &first]);
        assert_eq!(
            source_promotion_decision(&active_identity(&second), source.path()),
            SourceUpgradeDecision::OlderVersion
        );
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
        // `git notes add` below authors an object, so it needs an identity just
        // as much as `git commit` does. Set it repo-locally right after `init`
        // rather than per-command: a CI runner has no global git identity, so
        // any authoring command that misses an inline `-c` fails with "Author
        // identity unknown" / "empty ident name".
        git(source.path(), &["config", "user.name", "Homeboy Snapshot"]);
        git(
            source.path(),
            &["config", "user.email", "homeboy-snapshot@localhost"],
        );
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

    fn git_fixture(path: &Path, args: &[&str]) {
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

    /// Build a local remote plus a clone of it that is exactly one commit
    /// behind, and a second clone used only to advance the remote. Returns the
    /// tempdir (kept alive by the caller) and the behind clone path.
    fn behind_clone_fixture() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().expect("tempdir");
        let remote = dir.path().join("remote.git");
        git_fixture(
            dir.path(),
            &["init", "--bare", "-b", "main", &remote.to_string_lossy()],
        );

        let clone = dir.path().join("clone");
        git_fixture(dir.path(), &["clone", &remote.to_string_lossy(), "clone"]);
        git_fixture(&clone, &["config", "user.email", "test@example.com"]);
        git_fixture(&clone, &["config", "user.name", "test"]);
        std::fs::write(clone.join("wordpress.txt"), "initial").expect("write initial");
        git_fixture(&clone, &["add", "-A"]);
        git_fixture(&clone, &["commit", "-m", "initial"]);
        git_fixture(&clone, &["push", "-u", "origin", "main"]);

        let upstream = dir.path().join("upstream");
        git_fixture(
            dir.path(),
            &["clone", &remote.to_string_lossy(), "upstream"],
        );
        git_fixture(&upstream, &["config", "user.email", "test@example.com"]);
        git_fixture(&upstream, &["config", "user.name", "test"]);
        std::fs::write(upstream.join("wordpress.txt"), "advanced").expect("write advanced");
        git_fixture(&upstream, &["add", "-A"]);
        git_fixture(&upstream, &["commit", "-m", "advance"]);
        git_fixture(&upstream, &["push", "origin", "main"]);

        (dir, clone)
    }

    #[test]
    fn clean_behind_symlink_gets_pull_ff_only_recovery() {
        let (dir, clone) = behind_clone_fixture();
        let extensions_dir = dir.path().join("extensions");
        std::fs::create_dir_all(&extensions_dir).expect("extensions dir");
        std::os::unix::fs::symlink(&clone, extensions_dir.join("wordpress"))
            .expect("symlink extension");

        let warnings = detect_unrefreshed_in_extensions_dir(&extensions_dir, "alice", &[]);

        assert_eq!(warnings.len(), 1);
        let warning = &warnings[0];
        assert_eq!(warning.extension_id, "wordpress");
        assert!(!warning.dirty, "{:?}", warning);
        assert!(warning.dirty_paths.is_empty(), "{:?}", warning);
        assert!(
            warning.recovery_command.contains("pull --ff-only"),
            "{}",
            warning.recovery_command
        );
    }

    #[test]
    fn dirty_behind_symlink_never_emits_pull_ff_only_and_names_paths() {
        let (dir, clone) = behind_clone_fixture();
        let extensions_dir = dir.path().join("extensions");
        std::fs::create_dir_all(&extensions_dir).expect("extensions dir");
        std::os::unix::fs::symlink(&clone, extensions_dir.join("wordpress"))
            .expect("symlink extension");

        // The user's clone holds an uncommitted change a pull would refuse to
        // fast-forward over (#12181).
        std::fs::write(clone.join("notes.txt"), "local edit").expect("write local edit");

        let warnings = detect_unrefreshed_in_extensions_dir(&extensions_dir, "alice", &[]);

        assert_eq!(warnings.len(), 1);
        let warning = &warnings[0];
        assert!(warning.dirty, "{:?}", warning);
        assert_eq!(warning.dirty_paths, vec!["notes.txt".to_string()]);
        assert!(
            !warning.recovery_command.contains("pull --ff-only"),
            "{}",
            warning.recovery_command
        );
        assert!(
            warning.recovery_command.contains(" status"),
            "{}",
            warning.recovery_command
        );
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
    fn late_supersession_invalidates_the_complete_prior_generation_result() {
        let stale_runner = runner("0.309.0", "0.310.0", true);
        let stale_extension = ExtensionUpgradeEntry {
            extension_id: "wordpress".to_string(),
            old_version: "1".to_string(),
            new_version: "2".to_string(),
            linked: false,
            source_path: None,
            git_root: None,
            source_url: None,
            source_revision: None,
            source_update: Default::default(),
        };
        let stale_skip = ExtensionUpgradeSkip {
            extension_id: "woocommerce".to_string(),
            reason: "stale generation failure".to_string(),
        };
        let mut result = UpgradeResult {
            command: "upgrade".to_string(),
            install_method: InstallMethod::Binary,
            previous_version: "0.309.0".to_string(),
            new_version: Some("0.311.0".to_string()),
            previous_build_identity: Some("0.309.0+old".to_string()),
            new_build_identity: Some("0.311.0+contender".to_string()),
            source_revision: None,
            upgraded: true,
            outcome: Some("controller_superseded".to_string()),
            preflight: None,
            controller: Some(component_status("superseded", "controller superseded")),
            extensions: Some(component_status("partial", "stale extension evidence")),
            runners: Some(component_status("partial", "stale runner evidence")),
            partial: true,
            runner_convergence: Some(RunnerConvergenceDisposition::Partial),
            message: "stale runner and extension classifications".to_string(),
            restart_required: false,
            extensions_updated: vec![stale_extension],
            extensions_skipped: vec!["woocommerce".to_string()],
            extension_skips: vec![stale_skip],
            runners_updated: vec![stale_runner.clone()],
            runners_skipped: vec![stale_runner],
            extensions_unrefreshed: vec![UnrefreshedExtensionWarning {
                extension_id: "wordpress".to_string(),
                invoking_user: "alice".to_string(),
                symlink_path: "/tmp/extensions/wordpress".to_string(),
                source_path: "/tmp/wordpress".to_string(),
                behind: Some(1),
                dirty: false,
                dirty_paths: Vec::new(),
                recovery_command: "git pull".to_string(),
            }],
            services_restarted: Vec::new(),
            services_pending_restart: Vec::new(),
            operation_id: None,
        };

        invalidate_superseded_evidence(&mut result);

        assert_eq!(result.new_version.as_deref(), Some("0.311.0"));
        assert_eq!(
            result.new_build_identity.as_deref(),
            Some("0.311.0+contender")
        );
        assert_eq!(result.outcome.as_deref(), Some("controller_superseded"));
        assert_eq!(
            result
                .controller
                .as_ref()
                .map(|status| status.status.as_str()),
            Some("superseded")
        );
        assert_eq!(
            result
                .extensions
                .as_ref()
                .map(|status| status.status.as_str()),
            Some("not_run")
        );
        assert_eq!(
            result.runners.as_ref().map(|status| status.status.as_str()),
            Some("not_run")
        );
        assert_eq!(
            result.runner_convergence,
            Some(RunnerConvergenceDisposition::Skipped)
        );
        assert!(!result.partial);
        assert!(result.message.contains("0.311.0+contender"));
        assert!(result.message.contains("evidence was invalidated"));
        let payload = serde_json::to_value(&result).expect("serialize result");
        for field in [
            "extensions_updated",
            "extensions_skipped",
            "extension_skips",
            "extensions_unrefreshed",
            "runners_updated",
            "runners_skipped",
        ] {
            assert!(
                payload.get(field).is_none(),
                "stale field survived: {field}"
            );
        }
        assert!(!payload.to_string().contains("wordpress"));
        assert!(!payload.to_string().contains("woocommerce"));
        assert!(!payload.to_string().contains("lab"));
    }

    #[test]
    fn successful_but_stale_child_is_partial_controller_convergence() {
        let runner = runner("0.301.2", "0.301.2", false);

        assert!(runner_convergence_failed(
            std::slice::from_ref(&runner),
            &[],
            Some("0.304.0")
        ));
        let disposition = runner_convergence_disposition(
            false,
            std::slice::from_ref(&runner),
            &[],
            Some("0.304.0"),
        );
        assert_eq!(disposition, RunnerConvergenceDisposition::Partial);
        assert!(upgrade_message(
            true,
            Some("0.304.0"),
            None,
            disposition,
            &[runner],
            &[],
            None
        )
        .starts_with("PARTIAL: controller upgraded to 0.304.0"));
    }

    #[test]
    fn skip_runners_reports_skipped_not_converged() {
        // #9842: --skip-runners must never claim convergence.
        let disposition = runner_convergence_disposition(true, &[], &[], Some("0.310.0"));
        assert_eq!(disposition, RunnerConvergenceDisposition::Skipped);
        let message = upgrade_message(true, Some("0.310.0"), None, disposition, &[], &[], None);
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
        let message = upgrade_message(true, Some("0.310.0"), None, disposition, &[], &[], None);
        assert!(message.contains("no configured runners"), "{message}");
    }

    #[test]
    fn converged_runners_still_report_convergence() {
        let runner = runner("0.301.2", "0.310.0", true);
        let disposition = runner_convergence_disposition(
            false,
            std::slice::from_ref(&runner),
            &[],
            Some("0.310.0"),
        );
        assert_eq!(disposition, RunnerConvergenceDisposition::Converged);
        let message = upgrade_message(
            true,
            Some("0.310.0"),
            None,
            disposition,
            &[runner],
            &[],
            None,
        );
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
    fn extension_status_distinguishes_skipped_and_not_run() {
        assert_eq!(
            extension_component_status(false, true, &[], &[]).status,
            "skipped"
        );
        assert_eq!(
            extension_component_status(false, false, &[], &[]).status,
            "not_run"
        );
        assert_eq!(
            extension_component_status(true, false, &[], &[]).status,
            "completed"
        );
    }

    #[test]
    fn extension_status_names_each_skip_reason_when_partial() {
        let skips = vec![ExtensionUpgradeSkip {
            extension_id: "wordpress".to_string(),
            reason: "Linked extension source repo has uncommitted changes".to_string(),
        }];
        let status = extension_component_status(true, false, &[], &skips);
        assert_eq!(status.status, "partial");
        assert_eq!(
            status.summary,
            "0 updated, 1 skipped (wordpress: Linked extension source repo has uncommitted changes)"
        );
    }

    #[test]
    fn partial_extension_outcome_is_prominent_in_upgrade_message() {
        let skips = vec![ExtensionUpgradeSkip {
            extension_id: "wordpress".to_string(),
            reason: "Linked extension source repo has uncommitted changes".to_string(),
        }];
        let status = extension_component_status(true, false, &[], &skips);
        let message = upgrade_message(
            true,
            Some("0.310.0"),
            None,
            RunnerConvergenceDisposition::Converged,
            &[],
            &[],
            Some(&status),
        );
        assert!(
            message.ends_with(". EXTENSIONS PARTIAL: 0 updated, 1 skipped (wordpress: Linked extension source repo has uncommitted changes)"),
            "{message}"
        );
        let plain = upgrade_message(
            true,
            Some("0.310.0"),
            None,
            RunnerConvergenceDisposition::Converged,
            &[],
            &[],
            Some(&component_status("completed", "1 updated, 0 skipped")),
        );
        assert!(!plain.contains("EXTENSIONS PARTIAL"), "{plain}");
    }

    #[test]
    fn runner_status_is_not_run_when_controller_blocks_convergence() {
        assert_eq!(
            runner_component_status(RunnerConvergenceDisposition::Skipped, &[], &[], true,).status,
            "not_run"
        );
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

/// Release pinning and the deliberate-replacement gate (#11750).
///
/// These are the pure decisions behind `--version <TAG>`: which release a pin
/// resolves to, when a pin is refused, and why a pin has to bypass the
/// "already at the latest release" gate the way `--force` does.
#[cfg(test)]
mod pinned_release_tests {
    use super::*;
    use crate::upgrade::release_catalog::{ReleaseAsset, ReleaseEntry};

    const LINUX: &str = "x86_64-unknown-linux-gnu";
    const MAC: &str = "aarch64-apple-darwin";

    fn release(tag: &str, targets: &[&str]) -> ReleaseEntry {
        let mut assets = Vec::new();
        for target in targets {
            assets.push(ReleaseAsset {
                name: format!("homeboy-{target}.tar.xz"),
            });
            assets.push(ReleaseAsset {
                name: format!("homeboy-{target}.tar.xz.sha256"),
            });
        }

        ReleaseEntry {
            tag_name: tag.to_string(),
            draft: false,
            prerelease: false,
            assets,
        }
    }

    fn catalog() -> Vec<ReleaseEntry> {
        vec![
            release("v0.333.0", &[MAC]),
            release("v0.332.0", &[LINUX, MAC]),
            release("v0.331.0", &[LINUX, MAC]),
        ]
    }

    /// The escape the issue asked for: reach `v0.332.0` deliberately while
    /// `v0.333.0` is the newest tag and has no asset for this platform.
    #[test]
    fn a_pin_resolves_to_the_requested_release_and_its_target() {
        let selected = resolve_pinned_release(&catalog(), "v0.332.0", Some(LINUX))
            .expect("pinned release resolves");

        assert_eq!(selected.tag, "v0.332.0");
        assert_eq!(selected.version, "0.332.0");
        assert_eq!(selected.target.as_deref(), Some(LINUX));
        assert_eq!(
            selected.asset_url().as_deref(),
            Some("https://github.com/Extra-Chill/homeboy/releases/download/v0.332.0/homeboy-x86_64-unknown-linux-gnu.tar.xz")
        );
    }

    #[test]
    fn a_pin_accepts_the_bare_semver_spelling() {
        let selected = resolve_pinned_release(&catalog(), "0.332.0", Some(LINUX))
            .expect("pinned release resolves");

        assert_eq!(selected.tag, "v0.332.0");
    }

    /// Pinning a release that cannot install here must fail before any binary
    /// mutation, naming the target it looked for and the releases that would
    /// work — the information the bare 404 withheld.
    #[test]
    fn a_pin_without_this_targets_asset_is_refused_with_alternatives() {
        let error = resolve_pinned_release(&catalog(), "v0.333.0", Some(LINUX))
            .expect_err("an uninstallable pin must not be attempted");

        assert!(
            error.message.contains(LINUX),
            "refusal must name the target triple: {}",
            error.message
        );
        assert!(
            error.hints.iter().any(|hint| hint
                .message
                .contains("homeboy-x86_64-unknown-linux-gnu.tar.xz")),
            "refusal must name the asset it looked for: {:?}",
            error.hints
        );
        assert!(
            error.details.to_string().contains("v0.332.0"),
            "refusal must offer an installable release: {}",
            error.details
        );
    }

    #[test]
    fn a_pin_to_an_unpublished_release_is_refused() {
        let error = resolve_pinned_release(&catalog(), "v9.9.9", Some(LINUX))
            .expect_err("an unpublished pin must not be attempted");

        assert!(error.message.contains("v9.9.9"), "{}", error.message);
        assert!(
            error.details.to_string().contains("v0.332.0"),
            "{}",
            error.details
        );
    }

    /// An undetermined target means asset availability was never verified. The
    /// pin still resolves — the operator asked for it — but nothing pretends a
    /// triple was established.
    #[test]
    fn a_pin_on_an_undetermined_target_resolves_without_claiming_verification() {
        let selected =
            resolve_pinned_release(&catalog(), "v0.333.0", None).expect("pinned release resolves");

        assert_eq!(selected.tag, "v0.333.0");
        assert!(selected.target.is_none());
        assert!(selected.asset_url().is_none());
    }

    /// Without this, `--version <older tag>` reports "already at latest" and
    /// installs nothing, which is exactly the dead end the pin exists to break.
    #[test]
    fn a_pin_is_as_deliberate_as_force_at_the_release_gate() {
        assert!(controller_replacement_is_deliberate(
            false,
            Some("v0.332.0")
        ));
        assert!(controller_replacement_is_deliberate(true, None));
        assert!(!controller_replacement_is_deliberate(false, None));

        // The gate itself must then proceed on the deliberate flag alone, with
        // no source decision and no available release update.
        assert!(controller_replacement_proceeds(
            controller_replacement_is_deliberate(false, Some("v0.332.0")),
            None,
            false
        ));
    }

    /// A pin names a release asset, so an install method that does not download
    /// one must refuse it rather than silently install something else.
    #[test]
    fn a_pin_is_refused_for_install_methods_that_download_no_release_asset() {
        assert!(validate_pinned_version(Some("v0.332.0"), InstallMethod::Binary).is_ok());
        assert!(validate_pinned_version(None, InstallMethod::Source).is_ok());

        for method in [
            InstallMethod::Source,
            InstallMethod::Homebrew,
            InstallMethod::Secondary,
        ] {
            let error = validate_pinned_version(Some("v0.332.0"), method)
                .expect_err("a pin is meaningless without a release download");
            assert!(
                error
                    .hints
                    .iter()
                    .any(|hint| hint.message.contains("--method binary")),
                "refusal must offer the method that honors a pin: {:?}",
                error.hints
            );
        }
    }

    #[test]
    fn a_pin_infers_binary_for_a_cargo_installed_controller() {
        let (method, inferred) = resolve_install_method(None, Some("v0.332.0"), || {
            panic!("a release pin must not use the detected cargo install method")
        });

        assert_eq!(method, InstallMethod::Binary);
        assert!(inferred);
        assert!(validate_pinned_version(Some("v0.332.0"), method).is_ok());
    }

    #[test]
    fn omitted_method_without_a_pin_uses_detection() {
        let (method, inferred) = resolve_install_method(None, None, || InstallMethod::Secondary);

        assert_eq!(method, InstallMethod::Secondary);
        assert!(!inferred);
    }

    #[test]
    fn an_explicit_incompatible_method_with_a_pin_fails_closed() {
        for method in [
            InstallMethod::Source,
            InstallMethod::Homebrew,
            InstallMethod::Secondary,
        ] {
            let (resolved, inferred) =
                resolve_install_method(Some(method), Some("v0.332.0"), || {
                    panic!("explicit methods must not trigger detection")
                });

            assert_eq!(resolved, method);
            assert!(!inferred);
            validate_pinned_version(Some("v0.332.0"), resolved)
                .expect_err("an explicit incompatible method must reject the pin");
        }
    }

    #[test]
    fn no_installable_release_anywhere_names_the_target_and_the_newest_tag() {
        let selection = release_catalog::select_installable(
            &[release("v0.333.0", &[MAC]), release("v0.332.0", &[MAC])],
            Some(LINUX),
        );

        let error = no_installable_release_error(&selection, Some(LINUX));

        assert!(error.message.contains(LINUX), "{}", error.message);
        assert!(error.message.contains("v0.333.0"), "{}", error.message);
    }
}
