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

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredRig {
    pub id: String,
    pub description: String,
    pub rig_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredStack {
    pub id: String,
    pub description: String,
    pub stack_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigInstallResult {
    pub source: String,
    pub package_path: PathBuf,
    pub linked: bool,
    pub installed: Vec<InstalledRig>,
    pub installed_stacks: Vec<InstalledStack>,
}

#[derive(Debug, Clone)]
pub(crate) struct PreparedSource {
    pub source: String,
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
    pub package_path: String,
    pub rig_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discovery_path: Option<String>,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StackSourceMetadata {
    pub source: String,
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
    let discovered_stacks = discover_stacks(&prepared.discovery_path)?;

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
        if target.exists() || fs::symlink_metadata(&target).is_ok() {
            ensure_rig_refreshable(&rig, &target)?;
            remove_existing_config(&target, "replace rig config link")?;
        }

        link_or_copy_file(&rig.rig_path, &target)?;

        let metadata = RigSourceMetadata {
            source: prepared.source.clone(),
            package_path: prepared.package_path.to_string_lossy().to_string(),
            rig_path: rig.rig_path.to_string_lossy().to_string(),
            discovery_path: Some(prepared.discovery_path.to_string_lossy().to_string()),
            linked: prepared.linked,
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
        package_path: prepared.package_path,
        linked: prepared.linked,
        installed,
        installed_stacks,
    })
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
    Ok(PreparedSource {
        source: root_source.to_string(),
        package_path,
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
        discovery_path: package_path.clone(),
        package_path,
        linked: true,
        source_revision: None,
    })
}

pub fn discover_rigs(package_path: &Path) -> Result<Vec<DiscoveredRig>> {
    discover_rigs_matching(package_path, None)
}

fn discover_rigs_for_install(
    package_path: &Path,
    id: Option<&str>,
    all: bool,
) -> Result<Vec<DiscoveredRig>> {
    if all {
        return discover_rigs(package_path);
    }
    let Some(id) = id else {
        return discover_rigs(package_path);
    };
    discover_rigs_matching(package_path, Some(&extension::slugify_id(id)?))
}

fn discover_rigs_matching(
    package_path: &Path,
    only_id: Option<&str>,
) -> Result<Vec<DiscoveredRig>> {
    let mut rigs = Vec::new();

    let single = package_path.join("rig.json");
    if single.is_file() {
        rigs.push(discovered_from_path(&single, package_path.file_name())?);
    }

    let rigs_dir = package_path.join("rigs");
    if rigs_dir.is_dir() {
        for entry in fs::read_dir(&rigs_dir)
            .map_err(|e| Error::internal_io(e.to_string(), Some("read rigs dir".into())))?
        {
            let entry = entry.map_err(|e| {
                Error::internal_io(e.to_string(), Some("read rig dir entry".into()))
            })?;
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let rig_path = path.join("rig.json");
            if rig_path.is_file() {
                if let Some(only_id) = only_id {
                    let candidate = path
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(extension::slugify_id)
                        .transpose()?;
                    if candidate.as_deref() != Some(only_id) {
                        continue;
                    }
                }
                rigs.push(discovered_from_path(&rig_path, path.file_name())?);
            }
        }
    }

    rigs.sort_by(|a, b| a.id.cmp(&b.id));
    rigs.dedup_by(|a, b| a.id == b.id);

    if rigs.is_empty() {
        if only_id.is_some() {
            return Ok(rigs);
        }
        return Err(Error::validation_invalid_argument(
            "source",
            format!(
                "No rig specs found at {} (expected rig.json or rigs/<id>/rig.json)",
                package_path.display()
            ),
            Some(package_path.to_string_lossy().to_string()),
            None,
        ));
    }

    Ok(rigs)
}

pub fn discover_stacks(package_path: &Path) -> Result<Vec<DiscoveredStack>> {
    let stacks_dir = package_path.join("stacks");
    if !stacks_dir.is_dir() {
        return Ok(Vec::new());
    }

    let mut stacks = Vec::new();
    for entry in fs::read_dir(&stacks_dir)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read stacks dir".into())))?
    {
        let entry = entry
            .map_err(|e| Error::internal_io(e.to_string(), Some("read stack entry".into())))?;
        let path = entry.path();
        if path.extension().and_then(|s| s.to_str()) != Some("json") {
            continue;
        }
        stacks.push(discovered_stack_from_path(&path)?);
    }

    stacks.sort_by(|a, b| a.id.cmp(&b.id));
    stacks.dedup_by(|a, b| a.id == b.id);
    Ok(stacks)
}

fn discovered_stack_from_path(path: &Path) -> Result<DiscoveredStack> {
    let content = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read stack spec".into())))?;
    let mut spec: stack::StackSpec = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse stack spec {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    if spec.id.is_empty() {
        spec.id = path
            .file_stem()
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "stack_id",
                    "Stack spec has no id and no filename fallback",
                    None,
                    None,
                )
            })?
            .to_string();
    }
    Ok(DiscoveredStack {
        id: spec.id,
        description: spec.description,
        stack_path: path.to_path_buf(),
    })
}

fn discovered_from_path(
    path: &Path,
    fallback_name: Option<&std::ffi::OsStr>,
) -> Result<DiscoveredRig> {
    let content = fs::read_to_string(path)
        .map_err(|e| Error::internal_io(e.to_string(), Some("read rig spec".into())))?;
    let mut spec: super::RigSpec = serde_json::from_str(&content).map_err(|e| {
        Error::validation_invalid_json(
            e,
            Some(format!("parse rig spec {}", path.display())),
            Some(content.chars().take(200).collect()),
        )
    })?;
    if spec.id.is_empty() {
        spec.id = fallback_name
            .and_then(|name| name.to_str())
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "rig_id",
                    "Rig spec has no id and no directory name fallback",
                    None,
                    None,
                )
            })?
            .to_string();
    }
    Ok(DiscoveredRig {
        id: extension::slugify_id(&spec.id)?,
        description: spec.description,
        rig_path: path.to_path_buf(),
    })
}

fn select_rigs(
    rigs: Vec<DiscoveredRig>,
    id: Option<&str>,
    all: bool,
    source: &str,
) -> Result<Vec<DiscoveredRig>> {
    if all {
        return Ok(rigs);
    }
    if let Some(id) = id {
        let id = extension::slugify_id(id)?;
        let found: Vec<_> = rigs.into_iter().filter(|rig| rig.id == id).collect();
        if found.is_empty() {
            return Err(Error::validation_invalid_argument(
                "id",
                format!("Rig '{}' not found in package", id),
                Some(id),
                None,
            ));
        }
        return Ok(found);
    }
    if rigs.len() == 1 {
        return Ok(rigs);
    }
    let available = rigs.iter().map(|rig| rig.id.clone()).collect::<Vec<_>>();
    Err(Error::validation_invalid_argument(
        "id",
        format!(
            "Package contains multiple rigs; pass --id <rig> or --all. Available: {}",
            available.join(", ")
        ),
        Some(source.to_string()),
        Some(available),
    ))
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
