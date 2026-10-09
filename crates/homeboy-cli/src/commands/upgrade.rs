use clap::{Args, Subcommand};
use homeboy_upgrade::upgrade;
use serde_json::Value;
use std::path::PathBuf;

use crate::commands::CmdResult;

#[derive(Args)]
pub struct UpgradeArgs {
    /// Check for updates without installing
    #[arg(long)]
    pub check: bool,

    /// Force upgrade even if already at latest version
    #[arg(long)]
    pub force: bool,

    /// Skip extension updates (only upgrade the binary)
    #[arg(long)]
    pub skip_extensions: bool,

    /// Skip configured runner upgrades after the local upgrade
    #[arg(long)]
    pub skip_runners: bool,

    /// Skip restarting declared binary-resident services after the binary swap.
    /// They will be reported as pending with their recovery commands instead.
    #[arg(long)]
    pub no_restart_services: bool,

    /// Select the configured runner to converge with the controller. Repeat to target multiple runners.
    #[arg(
        long = "upgrade-runner",
        value_name = "RUNNER_ID",
        conflicts_with = "skip_runners"
    )]
    pub runners: Vec<String>,

    /// Refresh selected runners without promoting the controller.
    #[arg(long, requires = "runners")]
    pub runner_only: bool,

    /// Override install method detection (homebrew|cargo|source|binary)
    #[arg(long)]
    pub method: Option<String>,

    /// Homeboy source checkout to use with --method source
    #[arg(long, value_name = "PATH")]
    pub source_path: Option<PathBuf>,

    /// Pin a published release tag; infers --method binary when omitted
    #[arg(long = "version", value_name = "TAG", conflicts_with = "check")]
    pub pin_version: Option<String>,

    #[command(subcommand)]
    pub command: Option<UpgradeCommand>,
}

#[derive(Subcommand)]
pub enum UpgradeCommand {
    /// Inspect a persisted upgrade operation
    Status {
        /// Operation id from a previous `homeboy upgrade`. Defaults to the latest upgrade run.
        id: Option<String>,
    },
    /// Resume an admitted upgrade from its detached worker process.
    #[command(hide = true)]
    Continue { id: String },
}

pub fn run(args: UpgradeArgs) -> CmdResult<Value> {
    if let Some(UpgradeCommand::Status { id }) = args.command.as_ref() {
        let result = upgrade::load_upgrade_operation_status(id.as_deref())?;
        let json = serde_json::to_value(result)
            .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
        return Ok((json, 0));
    }

    if let Some(UpgradeCommand::Continue { id }) = args.command.as_ref() {
        upgrade::continue_detached_upgrade(&id)?;
        return Ok((
            serde_json::json!({ "operation_id": id, "status": "completed" }),
            0,
        ));
    }

    if args.check {
        let result = upgrade::check_for_updates()?;
        // A check that quietly withholds an update because the newest release
        // has no asset for this platform is indistinguishable from "you are
        // current". Say which release was passed over and why (#11750).
        if let Some(notice) = result.notice.as_deref() {
            homeboy::log_status!("upgrade", "{}", notice);
        }
        let json = serde_json::to_value(result)
            .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;
        return Ok((json, 0));
    }

    let method_override = args
        .method
        .as_deref()
        .map(|m| {
            let secondary = homeboy::core::defaults::secondary_install_method_key();
            match m {
                "homebrew" => Ok(upgrade::InstallMethod::Homebrew),
                "source" => Ok(upgrade::InstallMethod::Source),
                "binary" => Ok(upgrade::InstallMethod::Binary),
                other if other == secondary => Ok(upgrade::InstallMethod::Secondary),
                other => Err(homeboy::core::Error::validation_invalid_argument(
                    "method",
                    format!("Unknown method: {}", other),
                    Some(other.to_string()),
                    None,
                )),
            }
        })
        .transpose()?;

    if !args.runner_only && args.skip_extensions && args.skip_runners && args.source_path.is_none()
    {
        let method = method_override.unwrap_or_else(|| {
            if args.pin_version.is_some() {
                upgrade::InstallMethod::Binary
            } else {
                upgrade::detect_install_method()
            }
        });
        if matches!(
            method,
            upgrade::InstallMethod::Binary | upgrade::InstallMethod::Secondary
        ) {
            let admission = upgrade::start_detached_upgrade(
                args.force,
                method,
                args.pin_version.as_deref(),
                args.no_restart_services,
            )?;
            return Ok((admission, 0));
        }
    }

    let result = upgrade::run_upgrade_with_method(
        args.force,
        method_override,
        args.skip_extensions,
        args.skip_runners,
        args.no_restart_services,
        args.runner_only,
        &args.runners,
        args.source_path.as_deref(),
        args.pin_version.as_deref(),
    )?;
    let json = serde_json::to_value(&result)
        .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;

    // Surface any runner left version-degraded by the upgrade with a prominent
    // warning and its exact remediation command, instead of burying the drift in
    // the JSON payload (only ever rediscovered via `homeboy self status`).
    warn_degraded_runners(&result);
    warn_dangling_extensions(&result);

    Ok((json, upgrade_exit_code(&result, args.runner_only)))
}

/// Configured runners that the upgrade left version-degraded (PATH/version drift
/// relative to the upgraded controller). Reuses the drift detection already
/// captured on each entry's `path_drift` — the same signal `homeboy self status`
/// reports — so this stays a presentation-only surface.
fn degraded_runners(result: &upgrade::UpgradeResult) -> Vec<&upgrade::RunnerUpgradeEntry> {
    result
        .runners_updated
        .iter()
        .chain(result.runners_skipped.iter())
        .filter(|entry| entry.path_drift.is_some())
        .collect()
}

/// Human-readable warning lines for any runner left version-degraded after the
/// upgrade. Empty when every configured runner is aligned with the controller.
fn degraded_runner_warning_lines(result: &upgrade::UpgradeResult) -> Vec<String> {
    let degraded = degraded_runners(result);
    if degraded.is_empty() {
        return Vec::new();
    }

    let controller_version = result
        .new_version
        .as_deref()
        .unwrap_or(result.previous_version.as_str());

    let mut lines = Vec::new();
    lines.push(format!(
        "DEGRADED: {} configured runner(s) remain version-degraded after upgrading the controller to {}",
        degraded.len(),
        controller_version
    ));
    for entry in degraded {
        lines.push(format!(
            "  {}: {}",
            entry.runner_id,
            entry
                .path_drift
                .as_deref()
                .unwrap_or("runner version drift detected")
        ));
        let remediation = if entry.recovery_commands.is_empty() {
            format!(
                "homeboy upgrade --force --upgrade-runner {}",
                entry.runner_id
            )
        } else {
            entry.recovery_commands.join(" && ")
        };
        lines.push(format!("    remediate: {}", remediation));
    }
    lines
}

/// Emit the post-upgrade degraded-runner warning to the upgrade status channel.
fn warn_degraded_runners(result: &upgrade::UpgradeResult) {
    for line in degraded_runner_warning_lines(result) {
        homeboy::log_status!("upgrade", "{}", line);
    }
}

fn extension_warning_lines(result: &upgrade::UpgradeResult) -> Vec<String> {
    result
        .extension_skips
        .iter()
        .filter(|skip| skip.reason.starts_with("WARNING:"))
        .flat_map(|skip| {
            let mut lines = vec![format!("{}: {}", skip.extension_id, skip.reason)];
            lines.extend(
                skip.recovery_commands
                    .iter()
                    .map(|command| format!("  action: {command}")),
            );
            lines
        })
        .collect()
}

fn warn_dangling_extensions(result: &upgrade::UpgradeResult) {
    for line in extension_warning_lines(result) {
        homeboy::log_status!("upgrade", "{}", line);
    }
}

fn upgrade_exit_code(result: &upgrade::UpgradeResult, targeted_runner_upgrade: bool) -> i32 {
    if result.partial
        || (targeted_runner_upgrade && result.runners_skipped.iter().any(|runner| !runner.success))
    {
        return 1;
    }

    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn targeted_runner_failures_return_non_zero_status() {
        let mut result = base_upgrade_result();
        result.runners_skipped.push(upgrade::RunnerUpgradeEntry {
            runner_id: "homeboy-lab".to_string(),
            homeboy_path: format!(
                "/home/user/.{}/bin/homeboy",
                homeboy::core::defaults::secondary_install_method_key()
            ),
            success: false,
            upgraded: true,
            previous_version: Some("0.228.6".to_string()),
            new_version: Some("0.228.7".to_string()),
            bare_homeboy_version: Some("0.222.17".to_string()),
            path_drift: Some("bare `homeboy` reports 0.222.17".to_string()),
            recovery_commands: vec![
                "homeboy upgrade --force --upgrade-runner homeboy-lab".to_string()
            ],
            extensions_synced: Vec::new(),
            extensions_skipped: Vec::new(),
            extensions_failed: Vec::new(),
            stale_daemon: None,
            daemon_previous_version: None,
            daemon_new_version: None,
            exit_code: 0,
            detail: "extension sync failed".to_string(),
        });

        assert_eq!(upgrade_exit_code(&result, true), 1);
    }

    #[test]
    fn partial_convergence_returns_non_zero_status() {
        let mut result = base_upgrade_result();
        result.partial = true;

        assert_eq!(upgrade_exit_code(&result, false), 1);
    }

    #[test]
    fn partial_result_serializes_independent_component_statuses() {
        let mut result = base_upgrade_result();
        result.partial = true;
        result.controller = Some(upgrade::UpgradeComponentStatus {
            status: "updated".to_string(),
            summary: "controller installation completed".to_string(),
        });
        result.extensions = Some(upgrade::UpgradeComponentStatus {
            status: "completed".to_string(),
            summary: "0 updated, 0 skipped".to_string(),
        });
        result.runners = Some(upgrade::UpgradeComponentStatus {
            status: "partial".to_string(),
            summary: "0 converged, 1 require repair".to_string(),
        });

        let json = serde_json::to_value(result).expect("upgrade result serializes");
        assert_eq!(json["controller"]["status"], "updated");
        assert_eq!(json["extensions"]["status"], "completed");
        assert_eq!(json["runners"]["status"], "partial");
    }

    #[test]
    fn non_targeted_runner_failures_keep_best_effort_upgrade_status() {
        let mut result = base_upgrade_result();
        result.runners_skipped.push(upgrade::RunnerUpgradeEntry {
            runner_id: "homeboy-lab".to_string(),
            homeboy_path: "homeboy".to_string(),
            success: false,
            upgraded: false,
            previous_version: None,
            new_version: None,
            bare_homeboy_version: None,
            path_drift: None,
            recovery_commands: vec![
                "homeboy upgrade --force --upgrade-runner homeboy-lab".to_string()
            ],
            extensions_synced: Vec::new(),
            extensions_skipped: Vec::new(),
            extensions_failed: Vec::new(),
            stale_daemon: None,
            daemon_previous_version: None,
            daemon_new_version: None,
            exit_code: 1,
            detail: "runner unavailable".to_string(),
        });

        assert_eq!(upgrade_exit_code(&result, false), 0);
    }

    #[test]
    fn degraded_runner_emits_warning_with_remediation() {
        let mut result = base_upgrade_result();
        result.new_version = Some("0.265.2".to_string());
        result.runners_skipped.push(upgrade::RunnerUpgradeEntry {
            runner_id: "homeboy-lab".to_string(),
            homeboy_path: "/home/user/homeboy-main/target/release/homeboy".to_string(),
            success: false,
            upgraded: false,
            previous_version: Some("0.265.2".to_string()),
            new_version: Some("0.265.2".to_string()),
            bare_homeboy_version: Some("0.255.8".to_string()),
            path_drift: Some(
                "configured runner executable reports 0.265.2, but bare `homeboy` reports 0.255.8"
                    .to_string(),
            ),
            recovery_commands: vec![
                "homeboy upgrade --force --upgrade-runner homeboy-lab".to_string()
            ],
            extensions_synced: Vec::new(),
            extensions_skipped: Vec::new(),
            extensions_failed: Vec::new(),
            stale_daemon: None,
            daemon_previous_version: None,
            daemon_new_version: None,
            exit_code: 0,
            detail: "runner remains degraded".to_string(),
        });

        let lines = degraded_runner_warning_lines(&result);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("DEGRADED"));
        assert!(lines[0].contains("0.265.2"));
        assert!(lines[1].contains("homeboy-lab"));
        assert!(lines[2].contains("remediate"));
        assert!(lines[2].contains("homeboy upgrade --force --upgrade-runner homeboy-lab"));
    }

    #[test]
    fn aligned_runners_emit_no_degraded_warning() {
        let mut result = base_upgrade_result();
        result.runners_updated.push(upgrade::RunnerUpgradeEntry {
            runner_id: "homeboy-lab".to_string(),
            homeboy_path: "homeboy".to_string(),
            success: true,
            upgraded: true,
            previous_version: Some("0.265.1".to_string()),
            new_version: Some("0.265.2".to_string()),
            bare_homeboy_version: Some("0.265.2".to_string()),
            path_drift: None,
            recovery_commands: Vec::new(),
            extensions_synced: Vec::new(),
            extensions_skipped: Vec::new(),
            extensions_failed: Vec::new(),
            stale_daemon: None,
            daemon_previous_version: None,
            daemon_new_version: None,
            exit_code: 0,
            detail: "upgraded".to_string(),
        });

        assert!(degraded_runner_warning_lines(&result).is_empty());
    }

    #[test]
    fn dangling_extension_warning_emits_relink_and_uninstall_actions() {
        let mut result = base_upgrade_result();
        result.extension_skips.push(upgrade::ExtensionUpgradeSkip {
            extension_id: "discord".to_string(),
            reason: "WARNING: linked target is missing; relink or uninstall this extension"
                .to_string(),
            recovery_commands: vec![
                "homeboy extension relink discord <path>".to_string(),
                "homeboy extension uninstall discord".to_string(),
            ],
        });

        let lines = extension_warning_lines(&result);
        assert_eq!(lines.len(), 3);
        assert!(lines[0].contains("discord"));
        assert!(lines[1].contains("extension relink discord"));
        assert!(lines[2].contains("extension uninstall discord"));
    }

    fn base_upgrade_result() -> upgrade::UpgradeResult {
        upgrade::UpgradeResult {
            command: "upgrade".to_string(),
            install_method: upgrade::InstallMethod::Secondary,
            previous_version: "0.228.6".to_string(),
            new_version: Some("0.228.7".to_string()),
            previous_build_identity: None,
            new_build_identity: None,
            source_revision: None,
            upgraded: true,
            outcome: None,
            preflight: None,
            controller: None,
            extensions: None,
            runners: None,
            partial: false,
            runner_convergence: None,
            message: "Upgraded to 0.228.7".to_string(),
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
}
