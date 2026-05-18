use crate::core::error::{Error, Result};

use super::exec_context;
use super::lifecycle::update;
use super::registry::{available_extension_ids, load_extension};
use super::update_output::{
    SourceMetadataRepairEntry, UpdateAllResult, UpdateEntry, UpdateSkippedEntry,
};

/// Update all installed extensions through the same path used by single-extension updates.
pub fn update_all(force: bool) -> UpdateAllResult {
    let extension_ids = available_extension_ids();
    let mut updated = Vec::new();
    let mut skipped = Vec::new();
    let mut skipped_details = Vec::new();
    let mut repaired_source_metadata = Vec::new();

    for id in &extension_ids {
        let old_version = load_extension(id).ok().map(|m| m.version.clone());

        match update(id, force) {
            Ok(result) => {
                let new_version = load_extension(id)
                    .ok()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();
                let repaired = result.repaired_source_metadata;

                if let Some(repair) = repaired.clone() {
                    repaired_source_metadata.push(SourceMetadataRepairEntry {
                        extension_id: id.clone(),
                        repair,
                    });
                }

                updated.push(UpdateEntry {
                    extension_id: id.clone(),
                    old_version: old_version.unwrap_or_default(),
                    new_version,
                    linked: result.linked,
                    source_path: result
                        .source_path
                        .map(|path| path.to_string_lossy().to_string()),
                    git_root: result
                        .git_root
                        .map(|path| path.to_string_lossy().to_string()),
                    source_update: result.source_update,
                    repaired_source_metadata: repaired,
                });
            }
            Err(err) => {
                skipped.push(id.clone());
                skipped_details.push(UpdateSkippedEntry {
                    extension_id: id.clone(),
                    reason: err.message,
                    hints: err.hints.into_iter().map(|hint| hint.message).collect(),
                });
            }
        }
    }

    UpdateAllResult {
        updated,
        skipped,
        skipped_details,
        repaired_source_metadata,
    }
}

/// Execute a tool from an extension's vendor directory.
///
/// Sets up PATH with the extension's vendor/bin and node_modules/.bin,
/// resolves the working directory from an optional component, and runs
/// the command interactively.
pub fn exec_tool(extension_id: &str, component_id: Option<&str>, args: &[String]) -> Result<i32> {
    use crate::core::server::execute_local_command_interactive;

    let extension = load_extension(extension_id)?;
    let ext_path = extension
        .extension_path
        .as_deref()
        .ok_or_else(|| Error::config_missing_key("extension_path", Some(extension_id.into())))?;

    // Resolve working directory
    let working_dir = if let Some(cid) = component_id {
        let comp = crate::core::component::load(cid)?;
        comp.local_path.clone()
    } else {
        std::env::current_dir()
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_else(|_| ".".to_string())
    };

    // Build PATH with extension vendor directories prepended
    let vendor_bin = format!("{}/vendor/bin", ext_path);
    let node_bin = format!("{}/node_modules/.bin", ext_path);
    let current_path = std::env::var("PATH").unwrap_or_default();
    let enriched_path = format!("{}:{}:{}", vendor_bin, node_bin, current_path);

    let env = vec![
        ("PATH", enriched_path.as_str()),
        (exec_context::EXTENSION_PATH, ext_path),
        (exec_context::EXTENSION_ID, extension_id),
    ];

    let command = args.join(" ");
    Ok(execute_local_command_interactive(
        &command,
        Some(&working_dir),
        Some(&env),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_all() {
        crate::test_support::with_isolated_home(|_| {
            let result = update_all(false);

            assert!(result.updated.is_empty());
            assert!(result.skipped.is_empty());
            assert!(result.skipped_details.is_empty());
            assert!(result.repaired_source_metadata.is_empty());
        });
    }

    #[test]
    fn test_exec_tool() {
        let _exec_tool: fn(&str, Option<&str>, &[String]) -> Result<i32> = exec_tool;
    }
}
