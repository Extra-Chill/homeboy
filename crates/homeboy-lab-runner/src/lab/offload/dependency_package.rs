//! Sealed, controller-owned dependency packages for offline runner hydration.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use homeboy_engine_primitives::content_hash;
use serde::Serialize;
use sha2::{Digest, Sha256};
use zip::write::FileOptions;

use homeboy_core::{Error, Result};

const SCHEMA: &str = "homeboy/controller-dependency-package/v1";
pub(super) const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_PACKAGE_FILES: usize = 100_000;

#[derive(Debug, Clone, Serialize)]
pub(super) struct DependencyPackage {
    pub key: String,
    pub path: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub files: usize,
}

/// Seal (or reuse) a controller dependency package below an explicitly injected
/// data root.
///
/// Reuse and publication share the one root: the `is_file` hit, the staging
/// file, and the final rename all derive from `root` below, so a package can
/// never be read out of one home and republished into another (#7505).
pub(super) fn prepare_in_roots(
    data_root: &Path,
    workspace: &Path,
    plan: &[homeboy_core::deps::DependencyInstallPlanStep],
) -> Result<Option<DependencyPackage>> {
    let outputs = output_paths(plan);
    if outputs.is_empty() || outputs.iter().any(|path| !workspace.join(path).exists()) {
        return Ok(None);
    }
    let lockfiles = lockfile_identity(workspace)?;
    let outputs_identity = output_identity(workspace, &outputs)?;
    let key = content_hash::sha256_hex(&serde_json::to_vec(&(SCHEMA, plan, lockfiles)).map_err(
        |error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize dependency package identity".to_string()),
            )
        },
    )?);
    let key = content_hash::sha256_hex(format!("{key}\0{outputs_identity}").as_bytes());
    let root = data_root.join("cache/dependency-packages/v1");
    let path = root.join(format!("{key}.zip"));
    if path.is_file() {
        let bytes = fs::metadata(&path)
            .map_err(io_error("inspect dependency package"))?
            .len();
        if bytes <= MAX_PACKAGE_BYTES {
            return Ok(Some(DependencyPackage {
                key,
                sha256: content_hash::sha256_file(&path)?,
                path,
                bytes,
                files: 0,
            }));
        }
        let _ = fs::remove_file(&path);
    }
    fs::create_dir_all(&root).map_err(io_error("create dependency package cache"))?;
    let staging = root.join(format!(".{key}.{}.tmp", std::process::id()));
    let result = create_archive(workspace, &outputs, &staging);
    if result.is_err() {
        let _ = fs::remove_file(&staging);
    }
    let (bytes, files) = result?;
    fs::rename(&staging, &path).map_err(io_error("publish dependency package"))?;
    Ok(Some(DependencyPackage {
        key,
        sha256: content_hash::sha256_file(&path)?,
        path,
        bytes,
        files,
    }))
}

fn create_archive(workspace: &Path, outputs: &[String], path: &Path) -> Result<(u64, usize)> {
    let file = File::create(path).map_err(io_error("create dependency package archive"))?;
    let mut archive = zip::ZipWriter::new(file);
    let options = FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let mut files = Vec::new();
    for output in outputs {
        collect_files(&workspace.join(output), workspace, &mut files)?;
    }
    files.sort();
    files.dedup();
    if files.len() > MAX_PACKAGE_FILES {
        return Err(bound_error(
            "file_count",
            files.len() as u64,
            MAX_PACKAGE_FILES as u64,
        ));
    }
    let mut total = 0;
    let file_count = files.len();
    for source in files {
        let metadata =
            fs::metadata(&source).map_err(io_error("inspect dependency package file"))?;
        total += metadata.len();
        if total > MAX_PACKAGE_BYTES {
            return Err(bound_error("bytes", total, MAX_PACKAGE_BYTES));
        }
        let name = source
            .strip_prefix(workspace)
            .map_err(|_| Error::internal_unexpected("dependency package path escaped workspace"))?;
        archive
            .start_file(name.to_string_lossy(), options)
            .map_err(zip_error("write dependency package header"))?;
        let mut input = File::open(&source).map_err(io_error("open dependency package file"))?;
        std::io::copy(&mut input, &mut archive)
            .map_err(io_error("write dependency package file"))?;
    }
    archive
        .finish()
        .map_err(zip_error("finish dependency package archive"))?;
    let bytes = fs::metadata(path)
        .map_err(io_error("inspect dependency package archive"))?
        .len();
    if bytes > MAX_PACKAGE_BYTES {
        return Err(bound_error("archive_bytes", bytes, MAX_PACKAGE_BYTES));
    }
    Ok((bytes, file_count))
}

fn output_identity(workspace: &Path, outputs: &[String]) -> Result<String> {
    let mut files = Vec::new();
    for output in outputs {
        collect_files(&workspace.join(output), workspace, &mut files)?;
    }
    files.sort();
    let mut hasher = Sha256::new();
    for file in files {
        hasher.update(
            file.strip_prefix(workspace)
                .map_err(|_| Error::internal_unexpected("dependency output escaped workspace"))?
                .to_string_lossy()
                .as_bytes(),
        );
        hasher.update(content_hash::sha256_file(&file)?.as_bytes());
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn output_paths(plan: &[homeboy_core::deps::DependencyInstallPlanStep]) -> Vec<String> {
    let mut paths = plan
        .iter()
        .flat_map(|step| step.outputs.iter().map(|output| output.path.clone()))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths
}

fn collect_files(path: &Path, workspace: &Path, files: &mut Vec<PathBuf>) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(io_error("inspect dependency package output"))?;
    if metadata.file_type().is_symlink() {
        return Err(Error::validation_invalid_argument(
            "dependency_package",
            "dependency packages cannot contain symbolic links",
            Some(path.display().to_string()),
            None,
        ));
    }
    if metadata.is_file() {
        files.push(path.to_path_buf());
    } else if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(io_error("read dependency package output"))? {
            collect_files(
                &entry
                    .map_err(io_error("read dependency package entry"))?
                    .path(),
                workspace,
                files,
            )?;
        }
    } else {
        return Err(Error::validation_invalid_argument(
            "dependency_package",
            "dependency package outputs must be files or directories",
            Some(path.display().to_string()),
            None,
        ));
    }
    let _ = workspace;
    Ok(())
}

fn lockfile_identity(workspace: &Path) -> Result<Vec<(String, String)>> {
    let names = [
        "Cargo.lock",
        "composer.lock",
        "Gemfile.lock",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
    ];
    let mut values = Vec::new();
    collect_lockfiles(workspace, workspace, &names, &mut values)?;
    values.sort();
    Ok(values)
}

fn collect_lockfiles(
    root: &Path,
    path: &Path,
    names: &[&str],
    values: &mut Vec<(String, String)>,
) -> Result<()> {
    for entry in fs::read_dir(path).map_err(io_error("read dependency package lockfiles"))? {
        let path = entry
            .map_err(io_error("read dependency package lockfile"))?
            .path();
        let metadata =
            fs::symlink_metadata(&path).map_err(io_error("inspect dependency package lockfile"))?;
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_dir() {
            if path
                .file_name()
                .is_some_and(|name| name == "node_modules" || name == "vendor" || name == ".git")
            {
                continue;
            }
            collect_lockfiles(root, &path, names, values)?;
        } else if metadata.is_file()
            && path
                .file_name()
                .is_some_and(|name| names.iter().any(|candidate| name == *candidate))
        {
            values.push((
                path.strip_prefix(root)
                    .unwrap_or(&path)
                    .to_string_lossy()
                    .to_string(),
                content_hash::sha256_file(&path)?,
            ));
        }
    }
    Ok(())
}

pub(super) fn verify(path: &Path, expected: &str) -> Result<()> {
    let actual = content_hash::sha256_file(path)?;
    if actual == expected {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "dependency_package",
        "dependency package SHA-256 mismatch",
        Some(path.display().to_string()),
        None,
    ))
}

fn bound_error(bound: &str, actual: u64, maximum: u64) -> Error {
    Error::validation_invalid_argument(
        "dependency_package",
        format!("dependency package exceeds configured {bound} bound ({actual} > {maximum})"),
        None,
        None,
    )
}
fn io_error(context: &str) -> impl FnOnce(std::io::Error) -> Error + '_ {
    move |error| Error::internal_io(error.to_string(), Some(context.to_string()))
}
fn zip_error(context: &str) -> impl FnOnce(zip::result::ZipError) -> Error + '_ {
    move |error| Error::internal_io(error.to_string(), Some(context.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::deps::{
        DependencyInstallInvocation, DependencyInstallOutput, DependencyInstallOutputKind,
        DependencyInstallPlanStep,
    };

    fn plan() -> Vec<DependencyInstallPlanStep> {
        vec![DependencyInstallPlanStep {
            provider_id: "test".to_string(),
            invocation: DependencyInstallInvocation::Argv {
                argv: vec!["test".to_string()],
            },
            outputs: vec![DependencyInstallOutput {
                path: "deps".to_string(),
                kind: DependencyInstallOutputKind::Directory,
            }],
        }]
    }
    #[test]
    fn packages_and_reuses_dependency_outputs() {
        // No `with_isolated_home`: `prepare_in_roots` consults `data_root` for
        // the package cache and nothing else, so an explicit root is the whole
        // isolation this test needs. It no longer serializes behind
        // `home_lock`, which is the point of #7505.
        let data_root = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("deps")).unwrap();
        fs::write(root.path().join("deps/a"), "ok").unwrap();
        let first = prepare_in_roots(data_root.path(), root.path(), &plan())
            .unwrap()
            .unwrap();
        fs::remove_file(&first.path).unwrap();
        let second = prepare_in_roots(data_root.path(), root.path(), &plan())
            .unwrap()
            .unwrap();
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(first.path, second.path);
    }
    #[test]
    fn rejects_hash_mismatch() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), "actual").unwrap();
        assert!(verify(file.path(), "wrong").is_err());
    }
    #[test]
    fn reports_missing_outputs_without_a_package() {
        let data_root = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        assert!(prepare_in_roots(data_root.path(), root.path(), &plan())
            .unwrap()
            .is_none());
    }

    #[test]
    fn rejects_an_oversized_package_before_transfer() {
        let error = bound_error("bytes", MAX_PACKAGE_BYTES + 1, MAX_PACKAGE_BYTES);
        assert!(error.message.contains("configured bytes bound"));
    }
}
