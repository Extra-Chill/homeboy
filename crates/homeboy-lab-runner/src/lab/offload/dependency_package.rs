//! Sealed, controller-owned dependency packages for offline runner hydration.

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use homeboy_engine_primitives::content_hash;
use serde::Serialize;
use sha2::{Digest, Sha256};
use zip::write::FileOptions;

use homeboy_core::{Error, Result};

const SCHEMA: &str = "homeboy/controller-dependency-package/v2";
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

struct DependencyEntry {
    path: PathBuf,
    mode: u32,
    kind: DependencyEntryKind,
}

enum DependencyEntryKind {
    File,
    Directory,
    Symlink(crate::runner_staging_operation::SourcePackageSymlinkVerdict),
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
    if plan.is_empty() {
        return Ok(None);
    }
    for step in plan {
        if step.outputs.is_empty() {
            return Ok(None);
        }
        for output in &step.outputs {
            let path =
                homeboy_core::deps::dependency_install_step_output_path(workspace, step, output)?;
            let exists = match output.kind {
                homeboy_core::deps::DependencyInstallOutputKind::Path => path.exists(),
                homeboy_core::deps::DependencyInstallOutputKind::File => path.is_file(),
                homeboy_core::deps::DependencyInstallOutputKind::Directory => path.is_dir(),
            };
            if !exists {
                return Ok(None);
            }
        }
    }
    let outputs = output_paths(workspace, plan)?;
    let entries = dependency_entries(workspace, &outputs)?;
    let lockfiles = lockfile_identity(workspace)?;
    let outputs_identity = output_identity(workspace, &entries)?;
    let key = content_hash::sha256_hex(&serde_json::to_vec(&(SCHEMA, plan, lockfiles)).map_err(
        |error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize dependency package identity".to_string()),
            )
        },
    )?);
    let key = content_hash::sha256_hex(format!("{key}\0{outputs_identity}").as_bytes());
    let root = data_root.join("cache/dependency-packages/v2");
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
    let result = create_archive(workspace, &entries, &staging);
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

fn create_archive(
    workspace: &Path,
    entries: &[DependencyEntry],
    path: &Path,
) -> Result<(u64, usize)> {
    let file = File::create(path).map_err(io_error("create dependency package archive"))?;
    let mut archive = zip::ZipWriter::new(file);
    let options = FileOptions::default()
        .compression_method(zip::CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default());
    let mut total = 0;
    let file_count = entries.len();
    for entry in entries {
        let name = entry
            .path
            .strip_prefix(workspace)
            .map_err(|_| Error::internal_unexpected("dependency package path escaped workspace"))?
            .to_string_lossy()
            .into_owned();
        let options = options.unix_permissions(entry.mode);
        match &entry.kind {
            DependencyEntryKind::Directory => archive
                .add_directory(name, options)
                .map_err(zip_error("write dependency package directory"))?,
            DependencyEntryKind::Symlink(link) => {
                total += link.size_bytes;
                if total > MAX_PACKAGE_BYTES {
                    return Err(bound_error("bytes", total, MAX_PACKAGE_BYTES));
                }
                archive
                    .add_symlink(name, link.target.as_str(), options)
                    .map_err(zip_error("write dependency package symlink"))?;
            }
            DependencyEntryKind::File => {
                use std::io::Read;
                archive
                    .start_file(name, options)
                    .map_err(zip_error("write dependency package header"))?;
                let input =
                    File::open(&entry.path).map_err(io_error("open dependency package file"))?;
                total +=
                    std::io::copy(&mut input.take(MAX_PACKAGE_BYTES - total + 1), &mut archive)
                        .map_err(io_error("write dependency package file"))?;
                if total > MAX_PACKAGE_BYTES {
                    return Err(bound_error("bytes", total, MAX_PACKAGE_BYTES));
                }
            }
        }
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

fn output_identity(workspace: &Path, entries: &[DependencyEntry]) -> Result<String> {
    let mut hasher = Sha256::new();
    for entry in entries {
        let name = entry
            .path
            .strip_prefix(workspace)
            .map_err(|_| Error::internal_unexpected("dependency output escaped workspace"))?
            .to_string_lossy();
        let (kind, identity) = match &entry.kind {
            DependencyEntryKind::File => ("file", content_hash::sha256_file(&entry.path)?),
            DependencyEntryKind::Directory => ("directory", String::new()),
            DependencyEntryKind::Symlink(link) => ("symlink", link.sha256.clone()),
        };
        hasher.update(
            serde_json::to_vec(&(name.as_ref(), kind, entry.mode, identity)).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize dependency output identity".to_string()),
                )
            })?,
        );
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn output_paths(
    workspace: &Path,
    plan: &[homeboy_core::deps::DependencyInstallPlanStep],
) -> Result<Vec<String>> {
    let mut paths = Vec::new();
    for step in plan {
        for output in &step.outputs {
            let path =
                homeboy_core::deps::dependency_install_step_output_path(workspace, step, output)?;
            paths.push(
                path.strip_prefix(workspace)
                    .map_err(|_| Error::internal_unexpected("dependency output escaped workspace"))?
                    .display()
                    .to_string(),
            );
        }
    }
    paths.sort();
    paths.dedup();
    Ok(paths)
}

fn dependency_entries(workspace: &Path, outputs: &[String]) -> Result<Vec<DependencyEntry>> {
    let mut entries = Vec::new();
    for output in outputs {
        let root = workspace.join(output);
        collect_entries(&root, &root, &mut entries)?;
    }
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    entries.dedup_by(|left, right| left.path == right.path);
    if entries.len() > MAX_PACKAGE_FILES {
        return Err(bound_error(
            "file_count",
            entries.len() as u64,
            MAX_PACKAGE_FILES as u64,
        ));
    }
    Ok(entries)
}

fn collect_entries(path: &Path, root: &Path, entries: &mut Vec<DependencyEntry>) -> Result<()> {
    let metadata =
        fs::symlink_metadata(path).map_err(io_error("inspect dependency package output"))?;
    let kind = if metadata.file_type().is_symlink() {
        DependencyEntryKind::Symlink(dependency_symlink(path, root)?)
    } else if metadata.is_file() {
        DependencyEntryKind::File
    } else if metadata.is_dir() {
        for entry in fs::read_dir(path).map_err(io_error("read dependency package output"))? {
            collect_entries(
                &entry
                    .map_err(io_error("read dependency package entry"))?
                    .path(),
                root,
                entries,
            )?;
        }
        DependencyEntryKind::Directory
    } else {
        return Err(Error::validation_invalid_argument(
            "dependency_package",
            "dependency package outputs must be files or directories",
            Some(path.display().to_string()),
            None,
        ));
    };
    entries.push(DependencyEntry {
        path: path.to_path_buf(),
        mode: file_mode(&metadata),
        kind,
    });
    Ok(())
}

fn dependency_symlink(
    path: &Path,
    root: &Path,
) -> Result<crate::runner_staging_operation::SourcePackageSymlinkVerdict> {
    let invalid = |reason: &str| {
        Error::validation_invalid_argument(
            "dependency_package",
            reason,
            Some(path.display().to_string()),
            None,
        )
    };
    if path == root {
        return Err(invalid(
            "declared dependency output roots cannot be symbolic links",
        ));
    }
    let relative = path
        .strip_prefix(root)
        .map_err(|_| invalid("dependency link escaped its output root"))?;
    let target = fs::read_link(path).map_err(io_error("read dependency package symlink"))?;
    let target = target
        .to_str()
        .ok_or_else(|| invalid("dependency link target must be valid UTF-8"))?;
    let link = crate::runner_staging_operation::source_package_symlink_verdict(
        &relative.to_string_lossy(),
        target,
    )
    .map_err(|_| {
        invalid(
            "dependency link target must be relative and contained within its declared output root",
        )
    })?;
    let resolved = path
        .canonicalize()
        .map_err(|_| invalid("dependency link target is dangling or cyclic"))?;
    let root = root
        .canonicalize()
        .map_err(io_error("resolve dependency output root"))?;
    if !resolved.starts_with(root) || (!resolved.is_file() && !resolved.is_dir()) {
        return Err(invalid(
            "dependency link target must resolve inside its declared output root",
        ));
    }
    Ok(link)
}

#[cfg(unix)]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o777
}

#[cfg(not(unix))]
fn file_mode(metadata: &fs::Metadata) -> u32 {
    if metadata.is_dir() {
        0o755
    } else {
        0o644
    }
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
            workspace_relative_root: String::new(),
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
    fn packages_nested_component_outputs_from_their_provider_root() {
        let data_root = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let output = root.path().join("php-transformer/deps");
        fs::create_dir_all(&output).unwrap();
        fs::write(output.join("a"), "ok").unwrap();
        let mut plan = plan();
        plan[0].workspace_relative_root = "php-transformer".to_string();

        assert!(prepare_in_roots(data_root.path(), root.path(), &plan)
            .unwrap()
            .is_some());
    }

    #[cfg(unix)]
    #[test]
    fn package_identity_covers_link_targets_and_executable_permissions() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let data_root = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        let deps = root.path().join("deps");
        fs::create_dir_all(deps.join("empty")).unwrap();
        fs::write(deps.join("first"), "same bytes").unwrap();
        fs::write(deps.join("second"), "same bytes").unwrap();
        fs::set_permissions(deps.join("second"), fs::Permissions::from_mode(0o755)).unwrap();
        symlink("first", deps.join("tool")).unwrap();
        symlink("empty", deps.join("directory-link")).unwrap();
        let first = prepare_in_roots(data_root.path(), root.path(), &plan())
            .unwrap()
            .unwrap();
        fs::remove_file(deps.join("tool")).unwrap();
        symlink("second", deps.join("tool")).unwrap();
        let second = prepare_in_roots(data_root.path(), root.path(), &plan())
            .unwrap()
            .unwrap();
        assert_ne!(
            first.key, second.key,
            "different link topology must not reuse a cached package"
        );
        fs::set_permissions(deps.join("second"), fs::Permissions::from_mode(0o644)).unwrap();
        let third = prepare_in_roots(data_root.path(), root.path(), &plan())
            .unwrap()
            .unwrap();
        assert_ne!(
            second.key, third.key,
            "executable mode is part of package identity"
        );

        let restored = tempfile::tempdir().unwrap();
        let status = std::process::Command::new("unzip")
            .args([
                "-oq",
                second.path.to_str().unwrap(),
                "-d",
                restored.path().to_str().unwrap(),
            ])
            .status()
            .expect("restore sealed dependency package");
        assert!(status.success());
        assert_eq!(
            fs::read_link(restored.path().join("deps/tool")).unwrap(),
            PathBuf::from("second")
        );
        assert!(
            restored.path().join("deps/directory-link").is_dir(),
            "empty directory target must be materialized"
        );
        assert_eq!(
            fs::metadata(restored.path().join("deps/second"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o755
        );
        fs::remove_file(&second.path).unwrap();
        fs::set_permissions(deps.join("second"), fs::Permissions::from_mode(0o755)).unwrap();
        let repeated = prepare_in_roots(data_root.path(), root.path(), &plan())
            .unwrap()
            .unwrap();
        assert_eq!(
            second.sha256, repeated.sha256,
            "identical topology and modes seal deterministically"
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_absolute_escaping_dangling_and_cyclic_dependency_links() {
        use std::os::unix::fs::symlink;
        for scenario in ["absolute", "escaping", "dangling", "cyclic", "root"] {
            let data_root = tempfile::tempdir().unwrap();
            let root = tempfile::tempdir().unwrap();
            let deps = root.path().join("deps");
            fs::create_dir(&deps).unwrap();
            fs::write(deps.join("target"), "dependency").unwrap();
            fs::write(root.path().join("outside"), "not admitted").unwrap();
            match scenario {
                "absolute" => symlink(deps.join("target"), deps.join("link")).unwrap(),
                "escaping" => symlink("../outside", deps.join("link")).unwrap(),
                "dangling" => symlink("missing", deps.join("link")).unwrap(),
                "cyclic" => {
                    symlink("other", deps.join("link")).unwrap();
                    symlink("link", deps.join("other")).unwrap();
                }
                "root" => {
                    fs::rename(&deps, root.path().join("real-deps")).unwrap();
                    symlink("real-deps", &deps).unwrap();
                }
                _ => unreachable!(),
            }
            let error =
                prepare_in_roots(data_root.path(), root.path(), &plan()).expect_err(scenario);
            assert_eq!(
                error.code.as_str(),
                "validation.invalid_argument",
                "{scenario}"
            );
            assert_eq!(error.details["field"], "dependency_package", "{scenario}");
            assert!(
                !data_root
                    .path()
                    .join("cache/dependency-packages/v2")
                    .exists(),
                "{scenario}: invalid topology must not publish an archive"
            );
        }
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
    fn refuses_partial_packages_when_any_provider_omits_outputs() {
        let data_root = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::create_dir(root.path().join("deps")).unwrap();
        fs::write(root.path().join("deps/a"), "ok").unwrap();
        let mut plan = plan();
        plan.push(DependencyInstallPlanStep {
            provider_id: "outputless".to_string(),
            invocation: DependencyInstallInvocation::Argv {
                argv: vec!["test".to_string()],
            },
            workspace_relative_root: String::new(),
            outputs: Vec::new(),
        });

        assert!(prepare_in_roots(data_root.path(), root.path(), &plan)
            .unwrap()
            .is_none());
    }

    #[test]
    fn validates_declared_output_kind_before_packaging() {
        let data_root = tempfile::tempdir().unwrap();
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("deps"), "not a directory").unwrap();

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
