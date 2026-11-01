use homeboy_core::error::Result;
use std::collections::HashMap;

use super::{update, update_linked_group, UpdateResult};
use homeboy_core::extension::catalog::{available_extension_ids, load_extension};
use homeboy_extension_contract::update_output::{
    SourceMetadataRepairEntry, UpdateAllResult, UpdateEntry, UpdateSkippedEntry,
};

/// Update installed extensions, refreshing linked sources once per Git root.
///
/// `progress` runs before each extension's result is processed. Callback errors
/// abort the sweep without starting another batch; already-started units finish.
/// Extension update errors remain best-effort skips.
pub fn update_all(
    force: bool,
    mut progress: impl FnMut(&str, usize, usize) -> Result<()>,
) -> Result<UpdateAllResult> {
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
    // Keep each linked root in one unit, ordered by its first extension.
    let mut units = Vec::new();
    let mut grouped = HashMap::new();
    for ids in linked_groups.into_values() {
        for id in &ids {
            grouped.insert(id.clone(), ids.clone());
        }
    }
    let mut scheduled = std::collections::HashSet::new();
    for id in &extension_ids {
        if scheduled.contains(id) {
            continue;
        }
        let ids = grouped.get(id).cloned().unwrap_or_else(|| vec![id.clone()]);
        scheduled.extend(ids.iter().cloned());
        units.push((grouped.contains_key(id), ids));
    }
    let mut units = units.chunks(4);
    let mut results: HashMap<String, Result<UpdateResult>> = HashMap::new();
    let mut before = HashMap::new();
    let revisions = if force {
        HashMap::new()
    } else {
        super::resolve_update_revisions(&extension_ids)
    };

    for (index, id) in extension_ids.iter().enumerate() {
        progress(id, index + 1, extension_ids.len())?;
        if !results.contains_key(id) {
            let batch = units.next().expect("one update unit per extension");
            // Git, extraction, and setup use child-process env/current_dir,
            // never process-global set_var or set_current_dir. Independent
            // units write distinct installs; shared linked roots stay together.
            std::thread::scope(|scope| {
                let handles: Vec<_> = batch
                    .iter()
                    .map(|(linked, ids)| {
                        let revisions = &revisions;
                        scope.spawn(move || {
                            // Snapshot every member before this unit changes its root.
                            let before: Vec<_> = ids
                                .iter()
                                .map(|id| {
                                    (id.clone(), (
                                    load_extension(id).ok().map(|m| m.version.clone()),
                                    homeboy_core::extension::lifecycle::read_source_revision(id),
                                ))
                                })
                                .collect();
                            let results = if *linked {
                                match update_linked_group(ids, force) {
                                    Ok(entries) => entries
                                        .into_iter()
                                        .map(|entry| (entry.extension_id.clone(), Ok(entry)))
                                        .collect::<Vec<_>>(),
                                    Err(error) => ids
                                        .iter()
                                        .map(|id| (id.clone(), Err(error.clone())))
                                        .collect(),
                                }
                            } else {
                                let id = &ids[0];
                                vec![(
                                    id.clone(),
                                    update(id, force, revisions.get(id).map(|r| r.as_deref())),
                                )]
                            };
                            (before, results)
                        })
                    })
                    .collect();
                for handle in handles {
                    let (snapshots, entries) =
                        handle.join().expect("extension update worker panicked");
                    before.extend(snapshots);
                    results.extend(entries);
                }
            });
        }
        let (old_version, old_source_revision) = before.remove(id).unwrap();
        match results.remove(id).expect("update unit returned extension") {
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

                let source_url = if let Some(root) = result.git_root.as_ref() {
                    homeboy_core::git::remote_origin_url(root)
                } else if super::is_git_url(&result.url) {
                    Some(result.url.clone())
                } else {
                    None
                };
                updated.push(UpdateEntry {
                    source_url,
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

    Ok(UpdateAllResult {
        updated,
        skipped,
        skipped_details,
        repaired_source_metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_update_all() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let result = update_all(false, |_, _, _| Ok(())).unwrap();

            assert!(result.updated.is_empty());
            assert!(result.skipped.is_empty());
            assert!(result.skipped_details.is_empty());
            assert!(result.repaired_source_metadata.is_empty());
        });
    }
}
