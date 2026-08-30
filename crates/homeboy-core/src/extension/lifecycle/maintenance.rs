use homeboy_core::error::Result;
use std::collections::HashMap;

use super::{update, update_linked_group, UpdateResult};
use homeboy_core::extension::catalog::{available_extension_ids, load_extension};
use homeboy_extension_contract::update_output::{
    SourceMetadataRepairEntry, UpdateAllResult, UpdateEntry, UpdateSkippedEntry,
};

/// Update all installed extensions through the same path used by single-extension updates.
pub fn update_all(force: bool) -> UpdateAllResult {
    let extension_ids = available_extension_ids();
    let mut updated = Vec::new();
    let mut skipped = Vec::new();
    let mut skipped_details = Vec::new();
    let mut repaired_source_metadata = Vec::new();
    let mut linked_groups: HashMap<String, Vec<String>> = HashMap::new();
    // One resolution for the whole sweep. Grouping asks whether several
    // extensions share a Git root, which is only a meaningful question if every
    // member was resolved against the same installation (#7505).
    //
    // An unresolvable config root yields no groups, exactly as the per-item
    // form did when its `paths::extension` call failed.
    let config_root = homeboy_core::paths::homeboy().ok();
    for id in &extension_ids {
        let Some(config_root) = config_root.as_deref() else {
            break;
        };
        if homeboy_core::extension::catalog::is_extension_linked_in_root(config_root, id) {
            let path = homeboy_core::paths::extension_in_root(config_root, id);
            if let Ok(source) = std::fs::canonicalize(path) {
                if let Ok(root) = homeboy_core::git::get_git_root(&source.to_string_lossy()) {
                    linked_groups.entry(root).or_default().push(id.clone());
                }
            }
        }
    }
    let mut grouped_results: HashMap<String, Result<UpdateResult>> = HashMap::new();

    for id in &extension_ids {
        let old_version = load_extension(id).ok().map(|m| m.version.clone());
        let old_source_revision = homeboy_core::extension::lifecycle::read_source_revision(id);

        let update_result = linked_group_result(id, force, &linked_groups, &mut grouped_results);
        match update_result.unwrap_or_else(|| update(id, force)) {
            Ok(mut result) => {
                let new_version = load_extension(id)
                    .ok()
                    .map(|m| m.version.clone())
                    .unwrap_or_default();
                let repaired = result.repaired_source_metadata;
                // Copied installs persist revisions in metadata while linked
                // sources report them directly. Normalize both into before/after
                // evidence for extension-only convergence.
                if result.source_update.old_source_revision.is_none() {
                    result.source_update.old_source_revision = old_source_revision;
                }
                if result.source_update.new_source_revision.is_none() {
                    result.source_update.new_source_revision =
                        homeboy_core::extension::lifecycle::read_source_revision(id);
                }

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
                        .as_ref()
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

/// A linked monorepo is refreshed once per Git root. Every extension still gets
/// its own compatibility/setup validation and inherits the root's exact revision
/// transition for independent service targeting.
fn linked_group_result(
    id: &str,
    force: bool,
    groups: &HashMap<String, Vec<String>>,
    results: &mut HashMap<String, Result<UpdateResult>>,
) -> Option<Result<UpdateResult>> {
    let path = homeboy_core::paths::extension(id).ok()?;
    let source = std::fs::canonicalize(path).ok()?;
    let root = homeboy_core::git::get_git_root(&source.to_string_lossy()).ok()?;
    let ids = groups.get(&root)?;
    if !results.contains_key(id) {
        match update_linked_group(ids, force) {
            Ok(entries) => {
                for entry in entries {
                    results.insert(entry.extension_id.clone(), Ok(entry));
                }
            }
            Err(error) => {
                for member in ids {
                    results.insert(member.clone(), Err(error.clone()));
                }
            }
        }
    }
    results.get(id).cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_all() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let result = update_all(false);

            assert!(result.updated.is_empty());
            assert!(result.skipped.is_empty());
            assert!(result.skipped_details.is_empty());
            assert!(result.repaired_source_metadata.is_empty());
        });
    }
}
