//! Rig package install lifecycle.
//!
//! A rig package is a directory or git repository with specs at
//! `rigs/<id>/rig.json` (or a single rig directory containing `rig.json`).
//! Installed rigs stay loadable through the existing flat rig config path by
//! linking `~/.config/homeboy/rigs/<id>.json` to the package spec.

use crate::error::{Error, Result};
use crate::{extension, git, paths};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Debug, Clone, Serialize)]
pub struct DiscoveredRig {
    pub id: String,
    pub description: String,
    pub rig_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct RigInstallResult {
    pub source: String,
    pub package_path: PathBuf,
    pub linked: bool,
    pub installed: Vec<InstalledRig>,
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

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RigSourceMetadata {
    pub source: String,
    pub package_path: String,
    pub rig_path: String,
    pub linked: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
}

pub fn install(source: &str, id: Option<&str>, all: bool) -> Result<RigInstallResult> {
    let prepared = prepare_source(source)?;
    let discovered = discover_rigs(&prepared.discovery_path)?;
    let selected = select_rigs(discovered, id, all, source)?;

    fs::create_dir_all(paths::rigs()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create rigs dir".into())))?;
    fs::create_dir_all(paths::rig_sources()?)
        .map_err(|e| Error::internal_io(e.to_string(), Some("create rig sources dir".into())))?;

    let mut installed = Vec::new();
    for rig in selected {
        let target = paths::rig_config(&rig.id)?;
        if target.exists() || fs::symlink_metadata(&target).is_ok() {
            return Err(Error::validation_invalid_argument(
                "rig_id",
                format!("Rig '{}' already exists at {}", rig.id, target.display()),
                Some(rig.id),
                None,
            ));
        }

        link_or_copy_file(&rig.rig_path, &target)?;

        let metadata = RigSourceMetadata {
            source: prepared.source.clone(),
            package_path: prepared.package_path.to_string_lossy().to_string(),
            rig_path: rig.rig_path.to_string_lossy().to_string(),
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

    Ok(RigInstallResult {
        source: prepared.source,
        package_path: prepared.package_path,
        linked: prepared.linked,
        installed,
    })
}

pub fn read_source_metadata(id: &str) -> Option<RigSourceMetadata> {
    let path = paths::rig_source_metadata(id).ok()?;
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
    let source_revision = short_head_revision(&package_path);
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
                rigs.push(discovered_from_path(&rig_path, path.file_name())?);
            }
        }
    }

    rigs.sort_by(|a, b| a.id.cmp(&b.id));
    rigs.dedup_by(|a, b| a.id == b.id);

    if rigs.is_empty() {
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

#[cfg(test)]
#[path = "../../../tests/core/rig/install_test.rs"]
mod install_test;
