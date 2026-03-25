//! project_directory_operations — extracted from mod.rs.

use crate::config::{self, ConfigEntity};
use crate::engine::local_files::{self, FileSystem};
use crate::error::{Error, Result};
use crate::paths;
use std::path::PathBuf;
use crate::component::ScopedExtensionConfig;
use crate::output::{CreateOutput, MergeOutput, RemoveResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use crate::core::project::PinType;
use crate::core::project::PinnedRemoteFile;
use crate::core::project::PinnedRemoteLog;


/// Initialize a project directory at `~/.config/homeboy/projects/{id}/`.
///
/// Creates the directory structure and an initial `{id}.json` config file.
/// If the project already exists as a flat file, migrates it to directory form.
pub fn init_project_dir(id: &str) -> Result<PathBuf> {
    let dir = paths::project_dir(id)?;
    let config_path = paths::project_config(id)?;

    // If directory config already exists, nothing to do
    if config_path.exists() {
        return Err(Error::validation_invalid_argument(
            "id",
            format!("Project directory '{}' already exists", id),
            Some(id.to_string()),
            None,
        ));
    }

    // Check if a flat-file project exists that should be migrated
    let flat_path = paths::projects()?.join(format!("{}.json", id));
    if flat_path.exists() {
        return migrate_to_directory(id);
    }

    // Check the project exists in the registry
    if !exists(id) {
        return Err(Error::validation_invalid_argument(
            "id",
            format!(
                "Project '{}' does not exist. Create it first with `homeboy project create`",
                id
            ),
            Some(id.to_string()),
            None,
        ));
    }

    // Load, then re-save — save() now creates the directory via config_path()
    let project = load(id)?;
    // Delete the old flat file if it exists
    if flat_path.exists() {
        let _ = std::fs::remove_file(&flat_path);
    }
    // Force the directory path for the new save
    local_files::local().ensure_dir(&dir)?;
    let content = config::to_string_pretty(&project)?;
    local_files::local().write(&config_path, &content)?;

    Ok(dir)
}

/// Migrate a project from flat file `{id}.json` to directory `{id}/{id}.json`.
pub(crate) fn migrate_to_directory(id: &str) -> Result<PathBuf> {
    let flat_path = paths::projects()?.join(format!("{}.json", id));
    let dir = paths::project_dir(id)?;
    let config_path = paths::project_config(id)?;

    // Create the project directory
    local_files::local().ensure_dir(&dir)?;

    // Move the flat file into the directory with the correct name
    std::fs::rename(&flat_path, &config_path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("migrate project '{}' to directory", id)),
        )
    })?;

    Ok(dir)
}

/// Check if a project is using the directory-based config layout.
pub fn is_directory_based(id: &str) -> bool {
    paths::project_config(id)
        .map(|p| p.exists())
        .unwrap_or(false)
}

/// Check if a project is still using the legacy flat-file layout.
pub fn needs_directory_migration(id: &str) -> bool {
    let flat_exists = paths::projects()
        .map(|p| p.join(format!("{}.json", id)).exists())
        .unwrap_or(false);
    flat_exists && !is_directory_based(id)
}

/// Migrate all flat-file projects to directory-based layout.
///
/// Called during `homeboy upgrade` to transparently move projects from
/// `projects/{id}.json` to `projects/{id}/{id}.json`. Returns a list
/// of (project_id, success) tuples.
pub fn migrate_all_to_directories() -> Vec<(String, bool, String)> {
    let project_ids = match list_ids() {
        Ok(ids) => ids,
        Err(_) => return vec![],
    };

    let mut results = Vec::new();

    for id in &project_ids {
        if !needs_directory_migration(id) {
            continue;
        }

        match migrate_to_directory(id) {
            Ok(dir) => {
                results.push((id.clone(), true, format!("migrated to {}", dir.display())));
            }
            Err(e) => {
                results.push((id.clone(), false, e.message.clone()));
            }
        }
    }

    results
}

/// Get the project directory path for a given project ID.
/// Returns the directory path regardless of whether the project uses
/// directory-based or flat-file config.
pub fn project_dir_path(id: &str) -> Result<PathBuf> {
    paths::project_dir(id)
}

pub fn pin(project_id: &str, pin_type: PinType, path: &str, options: PinOptions) -> Result<()> {
    let mut project = load(project_id)?;

    match pin_type {
        PinType::File => {
            if project
                .remote_files
                .pinned_files
                .iter()
                .any(|f| f.path == path)
            {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "File is already pinned",
                    Some(project_id.to_string()),
                    Some(vec![path.to_string()]),
                ));
            }
            project.remote_files.pinned_files.push(PinnedRemoteFile {
                path: path.to_string(),
                label: options.label,
            });
        }
        PinType::Log => {
            if project
                .remote_logs
                .pinned_logs
                .iter()
                .any(|l| l.path == path)
            {
                return Err(Error::validation_invalid_argument(
                    "path",
                    "Log is already pinned",
                    Some(project_id.to_string()),
                    Some(vec![path.to_string()]),
                ));
            }
            project.remote_logs.pinned_logs.push(PinnedRemoteLog {
                path: path.to_string(),
                label: options.label,
                tail_lines: options.tail_lines,
            });
        }
    }

    save(&project)?;
    Ok(())
}

pub fn unpin(project_id: &str, pin_type: PinType, path: &str) -> Result<()> {
    let mut project = load(project_id)?;

    let (before, after, type_name) = match pin_type {
        PinType::File => {
            let before = project.remote_files.pinned_files.len();
            project.remote_files.pinned_files.retain(|f| f.path != path);
            (before, project.remote_files.pinned_files.len(), "file")
        }
        PinType::Log => {
            let before = project.remote_logs.pinned_logs.len();
            project.remote_logs.pinned_logs.retain(|l| l.path != path);
            (before, project.remote_logs.pinned_logs.len(), "log")
        }
    };

    if after == before {
        return Err(Error::validation_invalid_argument(
            "path",
            format!("{} is not pinned", type_name),
            Some(project_id.to_string()),
            Some(vec![path.to_string()]),
        ));
    }

    save(&project)?;
    Ok(())
}
