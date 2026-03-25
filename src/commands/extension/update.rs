//! update — extracted from extension.rs.

use homeboy::extension::{
    self, extension_ready_status, is_extension_linked, load_extension, run_setup, ExtensionSummary,
    UpdateEntry,
};
use crate::commands::CmdResult;
use clap::{Args, Subcommand};
use serde::Serialize;
use homeboy::project::{self, Project};


pub(crate) fn update_extension(
    extension_id: Option<&str>,
    all: bool,
    force: bool,
) -> CmdResult<ExtensionOutput> {
    if all {
        return update_all_extensions(force);
    }

    let extension_id = extension_id.ok_or_else(|| {
        homeboy::Error::validation_invalid_argument(
            "extension_id",
            "Provide a extension ID or use --all to update all extensions",
            None,
            None,
        )
    })?;

    // Capture version before update
    let old_version = load_extension(extension_id).ok().map(|m| m.version.clone());

    let result = extension::update(extension_id, force)?;

    // Capture version after update
    let new_version = load_extension(&result.extension_id)
        .ok()
        .map(|m| m.version.clone());

    Ok((
        ExtensionOutput::Update {
            extension_id: result.extension_id,
            url: result.url,
            path: result.path.to_string_lossy().to_string(),
            old_version,
            new_version,
        },
        0,
    ))
}

pub(crate) fn update_all_extensions(force: bool) -> CmdResult<ExtensionOutput> {
    let result = extension::update_all(force);

    Ok((
        ExtensionOutput::UpdateAll {
            updated: result.updated,
            skipped: result.skipped,
        },
        0,
    ))
}
