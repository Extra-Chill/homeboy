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
        .map(|m| match m {
            "homebrew" => Ok(upgrade::InstallMethod::Homebrew),
            "cargo" => Ok(upgrade::InstallMethod::Cargo),
            "source" => Ok(upgrade::InstallMethod::Source),
            "binary" => Ok(upgrade::InstallMethod::Binary),
            other => Err(homeboy::core::Error::validation_invalid_argument(
                "method",
                format!("Unknown method: {}", other),
                Some(other.to_string()),
                None,
            )),
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

    Ok((json, 0))
}
