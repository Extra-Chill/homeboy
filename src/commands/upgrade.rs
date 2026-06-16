use clap::Args;
use homeboy::core::upgrade;
use serde_json::Value;
use std::path::PathBuf;

use crate::commands::{CmdResult, GlobalArgs};

#[derive(Args)]
pub struct UpgradeArgs {
    /// Check for updates without installing
    #[arg(long)]
    pub check: bool,

    /// Force upgrade even if already at latest version
    #[arg(long)]
    pub force: bool,

    /// Skip automatic restart after upgrade
    #[arg(long)]
    pub no_restart: bool,

    /// Skip extension updates (only upgrade the binary)
    #[arg(long)]
    pub skip_extensions: bool,

    /// Skip configured runner upgrades after the local upgrade
    #[arg(long)]
    pub skip_runners: bool,

    /// Upgrade only the named configured runner. Repeat to target multiple runners.
    #[arg(long = "upgrade-runner", value_name = "RUNNER_ID")]
    pub runners: Vec<String>,

    /// Override install method detection (homebrew|cargo|source|binary)
    #[arg(long)]
    pub method: Option<String>,

    /// Homeboy source checkout to use with --method source
    #[arg(long, value_name = "PATH")]
    pub source_path: Option<PathBuf>,
}

pub fn run(args: UpgradeArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    if args.check {
        let result = upgrade::check_for_updates()?;
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

    let result = upgrade::run_upgrade_with_method(
        args.force,
        method_override,
        args.skip_extensions,
        args.skip_runners,
        &args.runners,
        args.source_path.as_deref(),
    )?;
    let json = serde_json::to_value(&result)
        .map_err(|e| homeboy::core::Error::internal_json(e.to_string(), None))?;

    // If upgrade succeeded and restart is needed, do it
    if result.upgraded && result.restart_required && !args.no_restart {
        // Print the result first so the user sees it
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "success": true,
                "data": json
            }))
            .unwrap_or_default()
        );

        // Restart into new binary
        #[cfg(unix)]
        upgrade::restart_with_new_binary();

        #[cfg(not(unix))]
        homeboy::log_status!("upgrade", "Please restart homeboy to use the new version.");
    }

    Ok((json, upgrade_exit_code(&result, !args.runners.is_empty())))
}

fn upgrade_exit_code(result: &upgrade::UpgradeResult, targeted_runner_upgrade: bool) -> i32 {
    if targeted_runner_upgrade && result.runners_skipped.iter().any(|runner| !runner.success) {
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
                "/home/chubes/.{}/bin/homeboy",
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
            exit_code: 0,
            detail: "extension sync failed".to_string(),
        });

        assert_eq!(upgrade_exit_code(&result, true), 1);
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
            exit_code: 1,
            detail: "runner unavailable".to_string(),
        });

        assert_eq!(upgrade_exit_code(&result, false), 0);
    }

    fn base_upgrade_result() -> upgrade::UpgradeResult {
        upgrade::UpgradeResult {
            command: "upgrade".to_string(),
            install_method: upgrade::InstallMethod::Secondary,
            previous_version: "0.228.6".to_string(),
            new_version: Some("0.228.7".to_string()),
            previous_build_identity: None,
            new_build_identity: None,
            upgraded: true,
            message: "Upgraded to 0.228.7".to_string(),
            restart_required: false,
            extensions_updated: Vec::new(),
            extensions_skipped: Vec::new(),
            runners_updated: Vec::new(),
            runners_skipped: Vec::new(),
            extensions_unrefreshed: Vec::new(),
        }
    }
}
