//! Installed rig source lifecycle.

use crate::core::error::{Error, Result};
use crate::core::git;
use crate::core::paths;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use super::install::{
    discover_stacks, link_or_copy_file, write_source_metadata, write_stack_source_metadata,
    RigSourceMetadata, StackSourceMetadata,
};

mod types;
pub use types::{
    FailedRigSourceUpdate, InvalidRigSourceMetadata, RemovedRigSourceRig, RemovedRigSourceStack,
    RigSourceGroup, RigSourceListResult, RigSourceRemoveResult, RigSourceRig, RigSourceStack,
    RigSourceUpdateResult, RigSourceUpdatedRig, RigSourceUpdatedStack, SkippedRigSourceRig,
    SkippedRigSourceStack, SkippedRigSourceUpdate,
};

pub fn list_sources() -> Result<RigSourceListResult> {
    read_source_entries().map(group_source_entries)
}

pub fn remove_source(selector: &str) -> Result<RigSourceRemoveResult> {
    let list = list_sources()?;
    let matches = list
        .sources
        .into_iter()
        .filter(|source| source_matches(source, selector))
        .collect::<Vec<_>>();

    if matches.is_empty() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("No installed rig source matches '{}'", selector),
            Some(selector.to_string()),
            None,
        ));
    }
    if matches.len() > 1 {
        let tried = matches
            .iter()
            .map(|source| source.package_id.clone())
            .collect::<Vec<_>>();
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "Selector '{}' matches multiple rig sources; use the full source or package path",
                selector
            ),
            Some(selector.to_string()),
            Some(tried),
        ));
    }

    let source = matches.into_iter().next().expect("checked non-empty");
    let mut removed = Vec::new();
    let mut removed_stacks = Vec::new();
    let mut skipped = Vec::new();
    let mut skipped_stacks = Vec::new();
    for rig in &source.rigs {
        let metadata_path = paths::rig_source_metadata(&rig.id)?;
        let config_path = PathBuf::from(&rig.config_path);
        let remove_config = rig.config_present && rig.config_owned;
        if remove_config {
            fs::remove_file(&config_path)
                .map_err(|e| Error::internal_io(e.to_string(), Some("remove rig config".into())))?;
        }

        remove_source_metadata(&metadata_path)?;
        if remove_config || !rig.config_present {
            removed.push(removed_rig_source_rig(rig, &metadata_path));
        } else {
            skipped.push(SkippedRigSourceRig {
                id: rig.id.clone(),
                config_path: rig.config_path.clone(),
                reason: "config file no longer points at the recorded rig source".to_string(),
            });
        }
    }
    for stack in &source.stacks {
        let metadata_path = paths::stack_source_metadata(&stack.id)?;
        let config_path = PathBuf::from(&stack.config_path);
        let remove_config = stack.config_present && stack.config_owned;
        if remove_config {
            fs::remove_file(&config_path).map_err(|e| {
                Error::internal_io(e.to_string(), Some("remove stack config".into()))
            })?;
        }

        remove_source_metadata(&metadata_path)?;
        if remove_config || !stack.config_present {
            removed_stacks.push(RemovedRigSourceStack {
                id: stack.id.clone(),
                config_path: stack.config_path.clone(),
                metadata_path: metadata_path.to_string_lossy().to_string(),
            });
        } else {
            skipped_stacks.push(SkippedRigSourceStack {
                id: stack.id.clone(),
                config_path: stack.config_path.clone(),
                reason: "config file no longer points at the recorded stack source".to_string(),
            });
        }
    }

    let removed_package_path = if !source.linked {
        let source_root = PathBuf::from(&source.source_root);
        if source_root.exists() {
            fs::remove_dir_all(&source_root).map_err(|e| {
                Error::internal_io(e.to_string(), Some("remove rig package".into()))
            })?;
            Some(source.source_root.clone())
        } else {
            None
        }
    } else {
        None
    };

    Ok(RigSourceRemoveResult {
        selector: selector.to_string(),
        source,
        removed,
        removed_stacks,
        skipped,
        skipped_stacks,
        removed_package_path,
    })
}

pub fn update_source_for_rig(id: &str) -> Result<RigSourceUpdateResult> {
    let metadata = super::install::read_source_metadata(id).ok_or_else(|| {
        Error::validation_invalid_argument(
            "rig_id",
            format!("Rig '{}' has no installed source metadata", id),
            Some(id.to_string()),
            None,
        )
    })?;
    if metadata.linked {
        return Err(Error::validation_invalid_argument(
            "rig_id",
            format!("Rig '{}' was installed from a linked local source; reinstall or edit the source directly", id),
            Some(id.to_string()),
            None,
        ));
    }

    let list = list_sources()?;
    let source = list
        .sources
        .into_iter()
        .find(|source| source.rigs.iter().any(|rig| rig.id == id))
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "rig_id",
                format!("No installed rig source contains '{}'", id),
                Some(id.to_string()),
                None,
            )
        })?;

    update_group(source)
}

pub fn update_all_sources() -> Result<RigSourceUpdateResult> {
    let mut aggregate = RigSourceUpdateResult {
        updated: Vec::new(),
        updated_stacks: Vec::new(),
        skipped: Vec::new(),
        failed: Vec::new(),
    };
    for source in list_sources()?.sources {
        if source.linked {
            for rig in source.rigs {
                aggregate.skipped.push(SkippedRigSourceUpdate {
                    id: rig.id,
                    source: source.source.clone(),
                    reason: "linked local sources are updated in place outside homeboy".to_string(),
                });
            }
            for stack in source.stacks {
                aggregate.skipped.push(SkippedRigSourceUpdate {
                    id: stack.id,
                    source: source.source.clone(),
                    reason: "linked local sources are updated in place outside homeboy".to_string(),
                });
            }
            continue;
        }
        match update_group(source.clone()) {
            Ok(result) => {
                aggregate.updated.extend(result.updated);
                aggregate.updated_stacks.extend(result.updated_stacks);
                aggregate.skipped.extend(result.skipped);
                aggregate.failed.extend(result.failed);
            }
            Err(error) => {
                aggregate.failed.push(FailedRigSourceUpdate {
                    package_id: source.package_id,
                    source: source.source,
                    package_path: source.package_path,
                    reason: error.message,
                });
            }
        }
    }
    Ok(aggregate)
}

pub fn update_source(selector: Option<&str>) -> Result<RigSourceUpdateResult> {
    let Some(selector) = selector else {
        return update_all_sources();
    };

    let matches = list_sources()?
        .sources
        .into_iter()
        .filter(|source| source_matches(source, selector))
        .collect::<Vec<_>>();

    if matches.is_empty() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("No installed rig source matches '{}'", selector),
            Some(selector.to_string()),
            None,
        ));
    }
    if matches.len() > 1 {
        let tried = matches
            .iter()
            .map(|source| source.package_id.clone())
            .collect::<Vec<_>>();
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "Selector '{}' matches multiple rig sources; use the full source or package path",
                selector
            ),
            Some(selector.to_string()),
            Some(tried),
        ));
    }

    let source = matches.into_iter().next().expect("checked non-empty");
    if source.linked {
        let mut skipped = Vec::new();
        for rig in source.rigs {
            skipped.push(SkippedRigSourceUpdate {
                id: rig.id,
                source: source.source.clone(),
                reason: "linked local sources are updated in place outside homeboy".to_string(),
            });
        }
        for stack in source.stacks {
            skipped.push(SkippedRigSourceUpdate {
                id: stack.id,
                source: source.source.clone(),
                reason: "linked local sources are updated in place outside homeboy".to_string(),
            });
        }
        return Ok(RigSourceUpdateResult {
            updated: Vec::new(),
            updated_stacks: Vec::new(),
            skipped,
            failed: Vec::new(),
        });
    }

    update_group(source)
}

fn update_group(source: RigSourceGroup) -> Result<RigSourceUpdateResult> {
    let source_root = PathBuf::from(&source.source_root);
    if !source_root.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "Installed rig source root is missing: {}",
                source.source_root
            ),
            Some(source.source),
            Some(vec![
                format!("source root: {}", source.source_root),
                format!("resolved package root: {}", source.package_path),
            ]),
        ));
    }

    let previous_revision = git::short_head_revision(&source_root);
    git::pull_repo(&source_root)?;
    let source_revision = git::short_head_revision(&source_root);

    let mut updated = Vec::new();
    let mut updated_stacks = Vec::new();
    let mut skipped = Vec::new();
    for rig in source.rigs {
        let rig_path = PathBuf::from(&rig.rig_path);
        if !rig_path.is_file() {
            skipped.push(SkippedRigSourceUpdate {
                id: rig.id,
                source: source.source.clone(),
                reason: format!("rig spec missing after update: {}", rig_path.display()),
            });
            continue;
        }
        if !rig.config_owned {
            skipped.push(SkippedRigSourceUpdate {
                id: rig.id,
                source: source.source.clone(),
                reason: "config file no longer points at the recorded rig source".to_string(),
            });
            continue;
        }

        let config_path = PathBuf::from(&rig.config_path);
        if config_path.exists() || fs::symlink_metadata(&config_path).is_ok() {
            fs::remove_file(&config_path).map_err(|e| {
                Error::internal_io(e.to_string(), Some("replace rig config link".into()))
            })?;
        }
        link_or_copy_file(&rig_path, &config_path)?;

        let metadata = RigSourceMetadata {
            source: source.source.clone(),
            source_root: Some(source.source_root.clone()),
            package_path: source.package_path.clone(),
            rig_path: rig.rig_path.clone(),
            discovery_path: Some(source.discovery_path.clone()),
            linked: false,
            source_revision: source_revision.clone(),
        };
        write_source_metadata(&rig.id, &metadata)?;

        updated.push(RigSourceUpdatedRig {
            id: rig.id,
            source: source.source.clone(),
            path: rig.config_path,
            spec_path: rig.rig_path,
            previous_revision: previous_revision.clone(),
            source_revision: source_revision.clone(),
        });
    }

    let current_stacks = discover_stacks(Path::new(&source.discovery_path))?;
    for stack in current_stacks {
        let existing = source.stacks.iter().find(|entry| entry.id == stack.id);
        let config_path = paths::stack_config(&stack.id)?;
        if config_path.exists() || fs::symlink_metadata(&config_path).is_ok() {
            if existing.is_none_or(|entry| !entry.config_owned) {
                skipped.push(SkippedRigSourceUpdate {
                    id: stack.id,
                    source: source.source.clone(),
                    reason: "config file no longer points at the recorded stack source".to_string(),
                });
                continue;
            }
            fs::remove_file(&config_path).map_err(|e| {
                Error::internal_io(e.to_string(), Some("replace stack config link".into()))
            })?;
        }
        link_or_copy_file(&stack.stack_path, &config_path)?;

        let metadata = StackSourceMetadata {
            source: source.source.clone(),
            source_root: Some(source.source_root.clone()),
            package_path: source.package_path.clone(),
            stack_path: stack.stack_path.to_string_lossy().to_string(),
            discovery_path: source.discovery_path.clone(),
            linked: false,
            source_revision: source_revision.clone(),
        };
        write_stack_source_metadata(&stack.id, &metadata)?;

        updated_stacks.push(RigSourceUpdatedStack {
            id: stack.id,
            source: source.source.clone(),
            path: config_path.to_string_lossy().to_string(),
            spec_path: stack.stack_path.to_string_lossy().to_string(),
            previous_revision: previous_revision.clone(),
            source_revision: source_revision.clone(),
        });
    }

    Ok(RigSourceUpdateResult {
        updated,
        updated_stacks,
        skipped,
        failed: Vec::new(),
    })
}

fn removed_rig_source_rig(rig: &RigSourceRig, metadata_path: &Path) -> RemovedRigSourceRig {
    RemovedRigSourceRig {
        id: rig.id.clone(),
        config_path: rig.config_path.clone(),
        metadata_path: metadata_path.to_string_lossy().to_string(),
    }
}

fn remove_source_metadata(path: &Path) -> Result<()> {
    fs::remove_file(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("remove rig source metadata".into())))
}

#[derive(Debug)]
struct RigSourceEntry {
    id: String,
    metadata: RigSourceMetadata,
}

#[derive(Debug)]
struct SourceEntries {
    valid: Vec<RigSourceEntry>,
    valid_stacks: Vec<StackSourceEntry>,
    invalid: Vec<InvalidRigSourceMetadata>,
}

#[derive(Debug)]
struct StackSourceEntry {
    id: String,
    metadata: StackSourceMetadata,
}

fn read_source_entries() -> Result<SourceEntries> {
    let dir = paths::rig_sources()?;
    let rig_entries = super::json_config::read_json_configs::<RigSourceMetadata>(
        &dir,
        "read rig sources dir",
        "read rig source entry",
    )?;
    let mut invalid = rig_entries
        .invalid
        .into_iter()
        .map(invalid_source_metadata)
        .collect::<Vec<_>>();
    let valid = rig_entries
        .valid
        .into_iter()
        .map(|entry| RigSourceEntry {
            id: entry.id,
            metadata: entry.value,
        })
        .collect::<Vec<_>>();

    let valid_stacks = read_stack_source_entries(&mut invalid)?;
    invalid.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(SourceEntries {
        valid,
        valid_stacks,
        invalid,
    })
}

fn read_stack_source_entries(
    invalid: &mut Vec<InvalidRigSourceMetadata>,
) -> Result<Vec<StackSourceEntry>> {
    let dir = paths::stack_sources()?;
    let entries = super::json_config::read_json_configs::<StackSourceMetadata>(
        &dir,
        "read stack sources dir",
        "read stack source entry",
    )?;
    invalid.extend(entries.invalid.into_iter().map(invalid_source_metadata));
    Ok(entries
        .valid
        .into_iter()
        .map(|entry| StackSourceEntry {
            id: entry.id,
            metadata: entry.value,
        })
        .collect())
}

fn invalid_source_metadata(
    entry: super::json_config::InvalidJsonConfig,
) -> InvalidRigSourceMetadata {
    InvalidRigSourceMetadata {
        id: entry.id,
        metadata_path: entry.path.to_string_lossy().to_string(),
        error: entry.error,
    }
}

fn group_source_entries(entries: SourceEntries) -> RigSourceListResult {
    let mut groups: BTreeMap<String, RigSourceGroup> = BTreeMap::new();
    for entry in entries.valid {
        let key = format!("{}\0{}", entry.metadata.source, entry.metadata.package_path);
        let config_path = paths::rig_config(&entry.id).ok();
        let config_present = config_path.as_ref().is_some_and(|path| path.exists());
        let config_owned = config_path
            .as_ref()
            .is_some_and(|path| rig_config_matches_source(path, &entry.metadata.rig_path));

        groups
            .entry(key)
            .or_insert_with(|| new_rig_source_group(&entry.metadata))
            .rigs
            .push(source_rig(entry, config_path, config_present, config_owned));
    }

    for entry in entries.valid_stacks {
        let key = format!("{}\0{}", entry.metadata.source, entry.metadata.package_path);
        let config_path = paths::stack_config(&entry.id).ok();
        let config_present = config_path.as_ref().is_some_and(|path| path.exists());
        let config_owned = config_path
            .as_ref()
            .is_some_and(|path| rig_config_matches_source(path, &entry.metadata.stack_path));

        groups
            .entry(key)
            .or_insert_with(|| new_stack_source_group(&entry.metadata))
            .stacks
            .push(source_stack(
                entry,
                config_path,
                config_present,
                config_owned,
            ));
    }

    let mut sources = groups.into_values().collect::<Vec<_>>();
    for source in &mut sources {
        source.rigs.sort_by(|a, b| a.id.cmp(&b.id));
        source.stacks.sort_by(|a, b| a.id.cmp(&b.id));
    }

    RigSourceListResult {
        sources,
        invalid: entries.invalid,
    }
}

fn infer_discovery_path(rig_path: &str) -> String {
    let path = Path::new(rig_path);
    match path
        .parent()
        .and_then(|parent| parent.file_name())
        .and_then(|name| name.to_str())
    {
        Some("rigs") => path
            .parent()
            .unwrap()
            .parent()
            .unwrap_or(path)
            .to_string_lossy()
            .to_string(),
        _ => path.parent().unwrap_or(path).to_string_lossy().to_string(),
    }
}

fn new_source_group(
    source: &str,
    source_root: &str,
    package_path: &str,
    discovery_path: String,
    linked: bool,
    source_revision: Option<String>,
) -> RigSourceGroup {
    let package_present = Path::new(package_path).exists();
    RigSourceGroup {
        source: source.to_string(),
        source_root: source_root.to_string(),
        package_id: package_id_from_path(package_path),
        package_path: package_path.to_string(),
        package_present,
        discovery_path,
        linked,
        source_revision,
        stale_reason: stale_source_reason(linked, package_present),
        rigs: Vec::new(),
        stacks: Vec::new(),
    }
}

fn new_rig_source_group(metadata: &RigSourceMetadata) -> RigSourceGroup {
    let source_root = metadata
        .source_root
        .as_deref()
        .unwrap_or(&metadata.package_path);
    new_source_group(
        &metadata.source,
        source_root,
        &metadata.package_path,
        metadata
            .discovery_path
            .clone()
            .unwrap_or_else(|| infer_discovery_path(&metadata.rig_path)),
        metadata.linked,
        metadata.source_revision.clone(),
    )
}

fn new_stack_source_group(metadata: &StackSourceMetadata) -> RigSourceGroup {
    let source_root = metadata
        .source_root
        .as_deref()
        .unwrap_or(&metadata.package_path);
    new_source_group(
        &metadata.source,
        source_root,
        &metadata.package_path,
        metadata.discovery_path.clone(),
        metadata.linked,
        metadata.source_revision.clone(),
    )
}

fn source_rig(
    entry: RigSourceEntry,
    config_path: Option<PathBuf>,
    config_present: bool,
    config_owned: bool,
) -> RigSourceRig {
    let rig_present = Path::new(&entry.metadata.rig_path).is_file();
    RigSourceRig {
        id: entry.id,
        rig_path: entry.metadata.rig_path,
        rig_present,
        config_path: source_config_path(config_path),
        config_present,
        config_owned,
        stale_reason: stale_config_reason(rig_present, config_present),
    }
}

fn source_stack(
    entry: StackSourceEntry,
    config_path: Option<PathBuf>,
    config_present: bool,
    config_owned: bool,
) -> RigSourceStack {
    let stack_present = Path::new(&entry.metadata.stack_path).is_file();
    RigSourceStack {
        id: entry.id,
        stack_path: entry.metadata.stack_path,
        stack_present,
        config_path: source_config_path(config_path),
        config_present,
        config_owned,
        stale_reason: stale_config_reason(stack_present, config_present),
    }
}

fn stale_source_reason(linked: bool, package_present: bool) -> Option<String> {
    (linked && !package_present).then(|| {
        "linked local rig source path is missing; restore it or run `homeboy rig sources remove <source>` and reinstall".to_string()
    })
}

fn stale_config_reason(spec_present: bool, config_present: bool) -> Option<String> {
    if !spec_present {
        Some(
            "recorded source spec path is missing; refresh/remove the rig source and reinstall"
                .to_string(),
        )
    } else if !config_present {
        Some(
            "installed rig config is missing or points at a missing linked source path".to_string(),
        )
    } else {
        None
    }
}

fn source_config_path(config_path: Option<PathBuf>) -> String {
    config_path
        .map(|path| path.to_string_lossy().to_string())
        .unwrap_or_default()
}

fn source_matches(source: &RigSourceGroup, selector: &str) -> bool {
    source.source == selector
        || source.source_root == selector
        || source.package_path == selector
        || source.package_id == selector
}

fn package_id_from_path(path: &str) -> String {
    Path::new(path)
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(path)
        .to_string()
}

fn rig_config_matches_source(config_path: &Path, rig_path: &str) -> bool {
    let rig_path = Path::new(rig_path);
    if fs::read_link(config_path).is_ok_and(|target| target == rig_path) {
        return true;
    }

    match (config_path.canonicalize(), rig_path.canonicalize()) {
        (Ok(config), Ok(rig)) if config == rig => true,
        (Ok(_), Ok(_)) => super::files_match(config_path, rig_path),
        _ => false,
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/source_test.rs"]
mod source_test;
