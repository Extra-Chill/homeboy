//! Installed rig source lifecycle.

use crate::error::{Error, Result};
use crate::git;
use crate::paths;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::install::{link_or_copy_file, write_source_metadata, RigSourceMetadata};

mod types;
pub use types::{
    InvalidRigSourceMetadata, RemovedRigSourceRig, RigSourceGroup, RigSourceListResult,
    RigSourceRemoveResult, RigSourceRig, RigSourceUpdateResult, RigSourceUpdatedRig,
    SkippedRigSourceRig, SkippedRigSourceUpdate,
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
    let mut skipped = Vec::new();
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

    let removed_package_path = if !source.linked {
        let package_path = PathBuf::from(&source.package_path);
        if package_path.exists() {
            fs::remove_dir_all(&package_path).map_err(|e| {
                Error::internal_io(e.to_string(), Some("remove rig package".into()))
            })?;
            Some(source.package_path.clone())
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
        skipped,
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
        skipped: Vec::new(),
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
            continue;
        }
        let result = update_group(source)?;
        aggregate.updated.extend(result.updated);
        aggregate.skipped.extend(result.skipped);
    }
    Ok(aggregate)
}

fn update_group(source: RigSourceGroup) -> Result<RigSourceUpdateResult> {
    let package_path = PathBuf::from(&source.package_path);
    if !package_path.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("Installed rig package is missing: {}", source.package_path),
            Some(source.source),
            None,
        ));
    }

    let previous_revision = short_head_revision(&package_path);
    git::pull_repo(&package_path)?;
    let source_revision = short_head_revision(&package_path);

    let mut updated = Vec::new();
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
            package_path: source.package_path.clone(),
            rig_path: rig.rig_path.clone(),
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

    Ok(RigSourceUpdateResult { updated, skipped })
}

fn short_head_revision(path: &Path) -> Option<String> {
    let output = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .current_dir(path)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let revision = String::from_utf8_lossy(&output.stdout).trim().to_string();
    (!revision.is_empty()).then_some(revision)
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
    invalid: Vec<InvalidRigSourceMetadata>,
}

fn read_source_entries() -> Result<SourceEntries> {
    let dir = paths::rig_sources()?;
    if !dir.exists() {
        return Ok(SourceEntries {
            valid: Vec::new(),
            invalid: Vec::new(),
        });
    }

    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for entry in fs::read_dir(&dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read rig sources dir".into())))?
    {
        let entry = entry
            .map_err(|e| Error::internal_io(e.to_string(), Some("read rig source entry".into())))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        let id = match path.file_stem().and_then(|s| s.to_str()) {
            Some(id) => id.to_string(),
            None => continue,
        };
        let content = match fs::read_to_string(&path) {
            Ok(content) => content,
            Err(err) => {
                invalid.push(InvalidRigSourceMetadata {
                    id,
                    metadata_path: path.to_string_lossy().to_string(),
                    error: err.to_string(),
                });
                continue;
            }
        };
        match serde_json::from_str::<RigSourceMetadata>(&content) {
            Ok(metadata) => valid.push(RigSourceEntry { id, metadata }),
            Err(err) => invalid.push(InvalidRigSourceMetadata {
                id,
                metadata_path: path.to_string_lossy().to_string(),
                error: err.to_string(),
            }),
        }
    }

    valid.sort_by(|a, b| a.id.cmp(&b.id));
    invalid.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(SourceEntries { valid, invalid })
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
            .or_insert_with(|| RigSourceGroup {
                source: entry.metadata.source.clone(),
                package_id: package_id_from_path(&entry.metadata.package_path),
                package_path: entry.metadata.package_path.clone(),
                linked: entry.metadata.linked,
                source_revision: entry.metadata.source_revision.clone(),
                rigs: Vec::new(),
            })
            .rigs
            .push(RigSourceRig {
                id: entry.id,
                rig_path: entry.metadata.rig_path,
                config_path: config_path
                    .map(|path| path.to_string_lossy().to_string())
                    .unwrap_or_default(),
                config_present,
                config_owned,
            });
    }

    let mut sources = groups.into_values().collect::<Vec<_>>();
    for source in &mut sources {
        source.rigs.sort_by(|a, b| a.id.cmp(&b.id));
    }

    RigSourceListResult {
        sources,
        invalid: entries.invalid,
    }
}

fn source_matches(source: &RigSourceGroup, selector: &str) -> bool {
    source.source == selector || source.package_path == selector || source.package_id == selector
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
        (Ok(_), Ok(_)) => files_match(config_path, rig_path),
        _ => false,
    }
}

fn files_match(left: &Path, right: &Path) -> bool {
    match (fs::read(left), fs::read(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => false,
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/source_test.rs"]
mod source_test;
