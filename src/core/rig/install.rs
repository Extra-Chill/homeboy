//! Rig package install lifecycle.
//!
//! A rig package is a directory or git repository with specs at
//! `rigs/<id>/rig.json` (or a single rig directory containing `rig.json`).
//! Installed rigs stay loadable through the existing flat rig config path by
//! linking `~/.config/homeboy/rigs/<id>.json` to the package spec.

use crate::core::error::{Error, Result};
use crate::core::{extension, git, paths, stack};
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};

const EXTENDS_FIELD: &str = "extends";

pub use super::discovery::{discover_rigs, discover_stacks, DiscoveredRig, DiscoveredStack};
use super::discovery::{discover_rigs_for_install, select_rigs};

#[derive(Debug, Clone, Serialize)]
pub struct RigInstallResult {
    pub source: String,
    pub source_root: PathBuf,
    pub package_path: PathBuf,
    pub linked: bool,
    pub installed: Vec<InstalledRig>,
    pub installed_stacks: Vec<InstalledStack>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedSource {
    pub source: String,
    pub source_root: PathBuf,
    pub package_path: PathBuf,
    pub discovery_path: PathBuf,
    pub linked: bool,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledRig {
    pub id: String,
    pub description: String,
    pub path: PathBuf,
    pub spec_path: PathBuf,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct InstalledStack {
    pub id: String,
    pub description: String,
    pub path: PathBuf,
    pub spec_path: PathBuf,
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigSourceMetadata {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    pub package_path: String,
    pub rig_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_path: Option<String>,
    pub linked: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub materialized: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackSourceMetadata {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_root: Option<String>,
    pub package_path: String,
    pub stack_path: String,
    pub discovery_path: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

pub fn install(source: &str, id: Option<&str>, all: bool) -> Result<RigInstallResult> {
    let prepared = prepare_source(source)?;
    let discovered = discover_rigs_for_install(&prepared.discovery_path, id, all)?;
    let selected = select_rigs(discovered, id, all, source)?;
    let discovered_stacks = if id.is_some() && !all {
        Vec::new()
    } else {
        discover_stacks(&prepared.discovery_path)?
    };

    fs::create_dir_all(paths::rigs()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create rigs dir".into())))?;
    fs::create_dir_all(paths::stacks()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create stacks dir".into())))?;
    fs::create_dir_all(paths::rig_sources()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create rig sources dir".into())))?;
    fs::create_dir_all(paths::stack_sources()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create stack sources dir".into())))?;

    for stack in &discovered_stacks {
        let target = paths::stack_config(&stack.id)?;
        if target.exists() || fs::symlink_metadata(&target).is_ok() {
            ensure_stack_refreshable(stack, &target)?;
        }
    }

    let mut installed = Vec::new();
    for rig in selected {
        let target = paths::rig_config(&rig.id)?;
        materialize_rig_spec(&rig.rig_path, &prepared.source_root)?;
        if target.exists() || fs::symlink_metadata(&target).is_ok() {
            ensure_rig_refreshable(&rig, &target)?;
            remove_existing_config(&target, "replace rig config link")?;
        }

        let materialized =
            write_rig_config_from_source(&rig.rig_path, &target, &prepared.source_root)?;

        let metadata = RigSourceMetadata {
            source: prepared.source.clone(),
            source_root: Some(prepared.source_root.to_string_lossy().to_string()),
            package_path: prepared.package_path.to_string_lossy().to_string(),
            rig_path: rig.rig_path.to_string_lossy().to_string(),
            discovery_path: Some(prepared.discovery_path.to_string_lossy().to_string()),
            linked: prepared.linked,
            materialized,
            source_revision: prepared.source_revision.clone(),
        };
        write_source_metadata(&rig.id, &metadata)?;

        installed.push(InstalledRig {
            id: rig.id,
            description: rig.description,
            path: target,
            spec_path: rig.rig_path,
            source_revision: prepared.source_revision.clone(),
        });
    }

    let mut installed_stacks = Vec::new();
    for stack in discovered_stacks {
        let target = paths::stack_config(&stack.id)?;
        if target.exists() || fs::symlink_metadata(&target).is_ok() {
            remove_existing_config(&target, "replace stack config link")?;
        }
        link_or_copy_file(&stack.stack_path, &target)?;

        let metadata = StackSourceMetadata {
            source: prepared.source.clone(),
            source_root: Some(prepared.source_root.to_string_lossy().to_string()),
            package_path: prepared.package_path.to_string_lossy().to_string(),
            stack_path: stack.stack_path.to_string_lossy().to_string(),
            discovery_path: prepared.discovery_path.to_string_lossy().to_string(),
            linked: prepared.linked,
            source_revision: prepared.source_revision.clone(),
        };
        write_stack_source_metadata(&stack.id, &metadata)?;

        installed_stacks.push(InstalledStack {
            id: stack.id,
            description: stack.description,
            path: target,
            spec_path: stack.stack_path,
            source_revision: prepared.source_revision.clone(),
        });
    }

    Ok(RigInstallResult {
        source: prepared.source,
        source_root: prepared.source_root,
        package_path: prepared.package_path,
        linked: prepared.linked,
        installed,
        installed_stacks,
    })
}

pub(crate) fn write_rig_config_from_source(
    source: &Path,
    target: &Path,
    source_root: &Path,
) -> Result<bool> {
    match materialize_rig_spec(source, source_root)? {
        Some(value) => {
            let content = serde_json::to_string_pretty(&value).map_err(|e| {
                Error::internal_json(
                    e.to_string(),
                    Some("serialize materialized rig spec".into()),
                )
            })?;
            fs::write(target, format!("{}\n", content)).map_err(|e| {
                Error::internal_io(e.to_string(), Some("write materialized rig spec".into()))
            })?;
            Ok(true)
        }
        None => {
            link_or_copy_file(source, target)?;
            Ok(false)
        }
    }
}

pub(crate) fn materialize_rig_spec(
    path: &Path,
    source_root: &Path,
) -> Result<Option<serde_json::Value>> {
    let value = read_json_value(path)?;
    if value.get(EXTENDS_FIELD).is_none() {
        return Ok(None);
    }
    materialize_rig_spec_value(path, source_root, value, &mut Vec::new()).map(Some)
}

fn materialize_rig_spec_value(
    path: &Path,
    source_root: &Path,
    mut value: serde_json::Value,
    stack: &mut Vec<PathBuf>,
) -> Result<serde_json::Value> {
    let canonical = path.canonicalize().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("resolve rig spec {}", path.display())),
        )
    })?;
    if stack.contains(&canonical) {
        return Err(Error::validation_invalid_argument(
            EXTENDS_FIELD,
            format!("Rig template extends cycle includes {}", path.display()),
            Some(path.to_string_lossy().to_string()),
            None,
        ));
    }
    stack.push(canonical);

    let parents = extends_paths(&value, path, source_root)?;
    remove_extends(&mut value);

    let mut merged = serde_json::Value::Object(serde_json::Map::new());
    for parent in parents {
        let parent_value = read_json_value(&parent)?;
        let parent_materialized =
            materialize_rig_spec_value(&parent, source_root, parent_value, stack)?;
        merge_json(&mut merged, parent_materialized);
    }
    merge_json(&mut merged, value);

    stack.pop();
    Ok(merged)
}

fn read_json_value(path: &Path) -> Result<serde_json::Value> {
    let content = fs::read_to_string(path).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!("read rig spec {}", path.display())),
        )
    })?;
    serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse rig spec {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })
}

fn extends_paths(
    value: &serde_json::Value,
    path: &Path,
    source_root: &Path,
) -> Result<Vec<PathBuf>> {
    let Some(extends) = value.get(EXTENDS_FIELD) else {
        return Ok(Vec::new());
    };

    let raw_paths = match extends {
        serde_json::Value::String(path) => vec![path.as_str()],
        serde_json::Value::Array(paths) => paths
            .iter()
            .map(|entry| {
                entry.as_str().ok_or_else(|| {
                    Error::validation_invalid_argument(
                        EXTENDS_FIELD,
                        "Rig template extends entries must be strings",
                        Some(entry.to_string()),
                        None,
                    )
                })
            })
            .collect::<Result<Vec<_>>>()?,
        _ => {
            return Err(Error::validation_invalid_argument(
                EXTENDS_FIELD,
                "Rig template extends must be a string or array of strings",
                Some(extends.to_string()),
                None,
            ));
        }
    };

    let source_root = source_root.canonicalize().map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some(format!(
                "resolve rig package root {}",
                source_root.display()
            )),
        )
    })?;
    let base = path.parent().unwrap_or_else(|| Path::new("."));
    let mut paths = Vec::new();
    for raw_path in raw_paths {
        if raw_path.trim().is_empty() || Path::new(raw_path).is_absolute() {
            return Err(Error::validation_invalid_argument(
                EXTENDS_FIELD,
                "Rig template extends paths must be non-empty relative paths",
                Some(raw_path.to_string()),
                None,
            ));
        }
        let candidate = base.join(raw_path);
        let canonical = candidate.canonicalize().map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some(format!("resolve rig template {}", candidate.display())),
            )
        })?;
        if !canonical.starts_with(&source_root) {
            return Err(Error::validation_invalid_argument(
                EXTENDS_FIELD,
                "Rig template extends paths must stay inside the rig package source root",
                Some(raw_path.to_string()),
                None,
            ));
        }
        paths.push(canonical);
    }
    Ok(paths)
}

fn remove_extends(value: &mut serde_json::Value) {
    if let Some(object) = value.as_object_mut() {
        object.remove(EXTENDS_FIELD);
    }
}

fn merge_json(target: &mut serde_json::Value, overlay: serde_json::Value) {
    match (target, overlay) {
        (serde_json::Value::Object(target), serde_json::Value::Object(overlay)) => {
            for (key, value) in overlay {
                match target.get_mut(&key) {
                    Some(existing) => merge_json(existing, value),
                    None => {
                        target.insert(key, value);
                    }
                }
            }
        }
        (target, overlay) => *target = overlay,
    }
}

fn ensure_rig_refreshable(rig: &DiscoveredRig, target: &Path) -> Result<()> {
    let content = match fs::read_to_string(target) {
        Ok(content) => content,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(Error::internal_io(
                error.to_string(),
                Some("read existing rig spec".into()),
            ));
        }
    };
    let mut spec: super::RigSpec = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse existing rig spec {}", target.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    if spec.id.is_empty() {
        spec.id = rig.id.clone();
    }
    let existing_id = extension::slugify_id(&spec.id)?;
    if existing_id == rig.id {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "rig_id",
        format!(
            "Rig '{}' already exists at {} but declares '{}'; refusing to replace it",
            rig.id,
            target.display(),
            existing_id
        ),
        Some(rig.id.clone()),
        None,
    ))
}

fn ensure_stack_refreshable(stack: &DiscoveredStack, target: &Path) -> Result<()> {
    if config_matches_source(target, &stack.stack_path) {
        return Ok(());
    }

    let existing = normalized_stack_spec(target, "parse existing stack spec")?;
    let incoming = normalized_stack_spec(&stack.stack_path, "parse incoming stack spec")?;
    if existing == incoming {
        return Ok(());
    }

    Err(Error::validation_invalid_argument(
        "stack_id",
        format!(
            "Stack '{}' already exists at {} with different content; refusing to replace it",
            stack.id,
            target.display()
        ),
        Some(stack.id.clone()),
        None,
    ))
}

fn normalized_stack_spec(path: &Path, context: &'static str) -> Result<serde_json::Value> {
    let content = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some(context.into())))?;
    let mut spec: stack::StackSpec = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("{} {}", context, path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    if spec.id.is_empty() {
        spec.id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or_default()
            .to_string();
    }
    serde_json::to_value(spec)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize stack spec".into())))
}

fn config_matches_source(config_path: &Path, source_path: &Path) -> bool {
    if fs::read_link(config_path).is_ok_and(|target| target == source_path) {
        return true;
    }

    match (config_path.canonicalize(), source_path.canonicalize()) {
        (Ok(config), Ok(source)) if config == source => true,
        (Ok(_), Ok(_)) => super::files_match(config_path, source_path),
        _ => false,
    }
}

fn remove_existing_config(target: &Path, context: &'static str) -> Result<()> {
    fs::remove_file(target).map_err(|e| Error::internal_io(e.to_string(), Some(context.into())))
}

pub fn read_source_metadata(id: &str) -> Option<RigSourceMetadata> {
    let path = paths::rig_source_metadata(id).ok()?;
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub fn read_stack_source_metadata(id: &str) -> Option<StackSourceMetadata> {
    let path = paths::stack_source_metadata(id).ok()?;
    let content = fs::read_to_string(path).ok()?;
    serde_json::from_str(&content).ok()
}

pub(crate) fn prepare_source(source: &str) -> Result<PreparedSource> {
    if extension::is_git_url(source) || source.contains(".git//") {
        prepare_git_source(source)
    } else {
        prepare_local_source(source)
    }
}

fn prepare_git_source(source: &str) -> Result<PreparedSource> {
    let (root_source, subpath) = split_git_source_subpath(source)?;
    let trimmed = root_source.trim_end_matches('/').trim_end_matches(".git");
    let parts = trimmed.rsplit(['/', ':']).take(2).collect::<Vec<_>>();
    let package_id = if parts.len() == 2 {
        extension::slugify_id(&format!("{}-{}", parts[1], parts[0]))?
    } else {
        extension::slugify_id(parts.first().copied().unwrap_or(trimmed))?
    };
    let package_path = paths::rig_package(&package_id)?;
    if package_path.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "Rig package '{}' already exists at {}",
                package_id,
                package_path.display()
            ),
            Some(root_source.to_string()),
            None,
        ));
    }
    fs::create_dir_all(paths::rig_packages()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create rig packages dir".into())))?;
    git::clone_repo(root_source, &package_path)?;
    let source_revision = git::short_head_revision(&package_path);
    let discovery_path = match subpath {
        Some(subpath) => package_path.join(subpath),
        None => package_path.clone(),
    };
    if !discovery_path.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "Rig package path does not exist (source root: {}; resolved package root: {})",
                package_path.display(),
                discovery_path.display()
            ),
            Some(source.to_string()),
            Some(vec![
                format!("source root: {}", package_path.display()),
                format!("resolved package root: {}", discovery_path.display()),
            ]),
        ));
    }
    Ok(PreparedSource {
        source: root_source.to_string(),
        source_root: package_path.clone(),
        package_path: discovery_path.clone(),
        discovery_path,
        linked: false,
        source_revision,
    })
}

fn split_git_source_subpath(source: &str) -> Result<(&str, Option<&str>)> {
    let Some(marker) = source.find(".git//") else {
        return Ok((source, None));
    };
    let root_end = marker + ".git".len();
    let root = &source[..root_end];
    let subpath = source[root_end + 2..].trim_matches('/');
    if subpath.is_empty() || subpath.starts_with("..") || subpath.contains("/../") {
        return Err(Error::validation_invalid_argument(
            "source",
            "Rig package subpath must be a non-empty relative path",
            Some(source.to_string()),
            None,
        ));
    }
    Ok((root, Some(subpath)))
}

fn prepare_local_source(source: &str) -> Result<PreparedSource> {
    let source_path = Path::new(source);
    let package_path = if source_path.is_absolute() {
        source_path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| Error::internal_io(e.to_string(), Some("get current dir".into())))?
            .join(source_path)
    };
    if !package_path.exists() {
        return Err(Error::validation_invalid_argument(
            "source",
            format!("Path does not exist: {}", package_path.display()),
            Some(source.to_string()),
            None,
        ));
    }
    Ok(PreparedSource {
        source: package_path.to_string_lossy().to_string(),
        source_root: package_path.clone(),
        discovery_path: package_path.clone(),
        package_path,
        linked: true,
        source_revision: None,
    })
}

pub(crate) fn write_source_metadata(id: &str, metadata: &RigSourceMetadata) -> Result<()> {
    let path = paths::rig_source_metadata(id)?;
    let content = serde_json::to_string_pretty(metadata)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize rig source".into())))?;
    fs::write(&path, format!("{}\n", content))
        .map_err(|e| Error::internal_io(e.to_string(), Some("write rig source".into())))
}

pub(crate) fn write_stack_source_metadata(id: &str, metadata: &StackSourceMetadata) -> Result<()> {
    fs::create_dir_all(paths::stack_sources()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create stack sources dir".into())))?;
    let path = paths::stack_source_metadata(id)?;
    let content = serde_json::to_string_pretty(metadata)
        .map_err(|e| Error::internal_json(e.to_string(), Some("serialize stack source".into())))?;
    fs::write(&path, format!("{}\n", content))
        .map_err(|e| Error::internal_io(e.to_string(), Some("write stack source".into())))
}

pub(crate) fn link_or_copy_file(source: &Path, target: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(source, target)
            .map_err(|e| Error::internal_io(e.to_string(), Some("create rig symlink".into())))
    }

    #[cfg(windows)]
    {
        fs::copy(source, target)
            .map(|_| ())
            .map_err(|e| Error::internal_io(e.to_string(), Some("copy rig spec".into())))
    }
}

#[cfg(test)]
#[path = "../../../tests/core/rig/install_test.rs"]
mod install_test;
