use clap::Args;
use homeboy::upgrade;
use serde_json::Value;

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

    /// Override install method detection (homebrew|cargo|source|binary)
    #[arg(long)]
    pub method: Option<String>,

    /// Accept --json for compatibility (output is JSON by default)
    #[arg(long, hide = true)]
    pub json: bool,
}

pub fn run(args: UpgradeArgs, _global: &GlobalArgs) -> CmdResult<Value> {
    if args.check {
        let result = upgrade::check_for_updates()?;
        let json = serde_json::to_value(result)
            .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
        return Ok((json, 0));
    }

    let method_override = args.method.as_deref().map(|m| match m {
        "homebrew" => Ok(upgrade::InstallMethod::Homebrew),
        "cargo" => Ok(upgrade::InstallMethod::Cargo),
        "source" => Ok(upgrade::InstallMethod::Source),
        "binary" => Ok(upgrade::InstallMethod::Binary),
        other => Err(homeboy::Error::validation_invalid_argument(
            "method",
            format!("Unknown method: {}", other),
            Some(other.to_string()),
            None,
        )),
    }).transpose()?;

    let result = upgrade::run_upgrade_with_method(args.force, method_override)?;
    let json = serde_json::to_value(&result)
        .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;

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
        eprintln!("Please restart homeboy to use the new version.");
    }

    Ok((json, 0))
}
