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
}

pub fn run(args: UpgradeArgs, global: &GlobalArgs) -> CmdResult<Value> {
    if args.check {
        let result = upgrade::check_for_updates()?;
        let json = serde_json::to_value(result)
            .map_err(|e| homeboy::Error::internal_json(e.to_string(), None))?;
        return Ok((json, 0));
    }

    let result = upgrade::run_upgrade(args.force)?;
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
