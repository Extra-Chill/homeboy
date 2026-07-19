use std::path::{Path, PathBuf};

use crate::agent_runtime_manifest::AgentRuntimeManifest;
use crate::config;
use crate::engine::identifier;
use crate::engine::local_files;
use crate::error::{Error, Result};
use crate::io::{copy_tree, EntryPolicy};
use crate::{git, paths};

#[derive(Debug, Clone)]
pub struct RuntimePackageRefreshResult {
    pub runtime_id: String,
    pub source: String,
    pub path: PathBuf,
    pub manifest_path: PathBuf,
    pub source_revision: Option<String>,
    pub replaced_existing: bool,
}

pub fn refresh(
    runtime_id: &str,
    source: &str,
    revision: Option<&str>,
) -> Result<RuntimePackageRefreshResult> {
    let runtime_id = identifier::slugify_id(runtime_id, "runtime_id")?;
    let runtime_root = paths::agent_runtimes()?;
    local_files::ensure_app_dirs()?;
    let runtime_parent = runtime_root.parent().ok_or_else(|| {
        Error::internal_io(
            "runtime package directory has no parent".to_string(),
            Some("prepare runtime package directory".to_string()),
        )
    })?;
    std::fs::create_dir_all(runtime_parent).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("prepare runtime package directory".to_string()),
        )
    })?;

    let temp_dir = runtime_parent.join(format!(".refresh-tmp-{runtime_id}"));
    remove_path_if_exists(&temp_dir, "clean stale runtime package refresh temp")?;

    let (source_root, source_revision) = if crate::extension_update_check::is_git_url(source) {
        git::clone_repo_at_ref(source, &temp_dir, revision)?;
        let source_revision = git::short_head_revision(&temp_dir);
        (temp_dir.as_path(), source_revision)
    } else {
        if revision.is_some() {
            return Err(Error::validation_invalid_argument(
                "ref",
                "--ref is only supported for git URL runtime package sources",
                revision.map(str::to_string),
                None,
            ));
        }
        let source_path = Path::new(source);
        let source_revision = git::short_head_revision(source_path);
        (source_path, source_revision)
    };

    let package_source = resolve_runtime_package_source(source_root, &runtime_id)?;
    validate_runtime_package(&package_source, &runtime_id)?;

    let target = runtime_root.join(&runtime_id);
    let staged = runtime_parent.join(format!(".refresh-stage-{runtime_id}"));
    let backup = runtime_parent.join(format!(".refresh-backup-{runtime_id}"));
    remove_path_if_exists(&staged, "clean stale runtime package refresh stage")?;
    remove_path_if_exists(&backup, "clean stale runtime package refresh backup")?;

    copy_dir_recursive(&package_source, &staged)?;
    write_source_metadata(&staged, source, source_revision.as_deref())?;

    if is_symlink(&runtime_root) {
        let replaced_existing = path_exists_or_symlink(&target);
        return replace_symlinked_runtime_root(
            &runtime_root,
            &runtime_id,
            &staged,
            &temp_dir,
            source,
            source_revision,
            replaced_existing,
        );
    }

    std::fs::create_dir_all(&runtime_root).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("prepare runtime package directory".to_string()),
        )
    })?;

    let replaced_existing = path_exists_or_symlink(&target);
    if replaced_existing {
        rename_path(&target, &backup, "backup runtime package")?;
    }

    if let Err(err) = rename_path(&staged, &target, "install runtime package") {
        if replaced_existing {
            let _ = rename_path(&backup, &target, "restore runtime package backup");
        }
        let _ = remove_path_if_exists(&staged, "clean failed runtime package stage");
        let _ = remove_path_if_exists(&temp_dir, "clean runtime package refresh temp");
        return Err(err);
    }

    remove_path_if_exists(&backup, "remove runtime package backup")?;
    remove_path_if_exists(&temp_dir, "clean runtime package refresh temp")?;

    Ok(RuntimePackageRefreshResult {
        runtime_id: runtime_id.clone(),
        source: source.to_string(),
        path: target.clone(),
        manifest_path: target.join(format!("{runtime_id}.json")),
        source_revision,
        replaced_existing,
    })
}

fn replace_symlinked_runtime_root(
    runtime_root: &Path,
    runtime_id: &str,
    staged: &Path,
    temp_dir: &Path,
    source: &str,
    source_revision: Option<String>,
    replaced_existing: bool,
) -> Result<RuntimePackageRefreshResult> {
    let runtime_parent = runtime_root.parent().expect("runtime root has parent");
    let materialized = runtime_parent.join(format!(".refresh-root-stage-{runtime_id}"));
    let root_backup = runtime_parent.join(format!(".refresh-root-backup-{runtime_id}"));
    remove_path_if_exists(&materialized, "clean stale runtime root refresh stage")?;
    remove_path_if_exists(&root_backup, "clean stale runtime root refresh backup")?;

    copy_tree(
        runtime_root,
        &materialized,
        "copy runtime packages for root materialization",
        EntryPolicy::CopyAnyNonDir,
    )?;
    let target = materialized.join(runtime_id);
    remove_path_if_exists(&target, "replace staged runtime package")?;
    rename_path(
        staged,
        &target,
        "stage runtime package in materialized root",
    )?;

    rename_path(
        runtime_root,
        &root_backup,
        "backup symlinked runtime package root",
    )?;
    if let Err(err) = rename_path(
        &materialized,
        runtime_root,
        "install materialized runtime package root",
    ) {
        let _ = rename_path(
            &root_backup,
            runtime_root,
            "restore symlinked runtime package root",
        );
        let _ = remove_path_if_exists(&materialized, "clean failed runtime root stage");
        let _ = remove_path_if_exists(temp_dir, "clean runtime package refresh temp");
        return Err(err);
    }

    remove_path_if_exists(&root_backup, "remove symlinked runtime package root backup")?;
    remove_path_if_exists(temp_dir, "clean runtime package refresh temp")?;

    Ok(RuntimePackageRefreshResult {
        runtime_id: runtime_id.to_string(),
        source: source.to_string(),
        path: runtime_root.join(runtime_id),
        manifest_path: runtime_root
            .join(runtime_id)
            .join(format!("{runtime_id}.json")),
        source_revision,
        replaced_existing,
    })
}

fn resolve_runtime_package_source<'a>(source_root: &'a Path, runtime_id: &str) -> Result<PathBuf> {
    let direct_manifest = source_root.join(format!("{runtime_id}.json"));
    if direct_manifest.is_file() {
        return Ok(source_root.to_path_buf());
    }

    let monorepo_package = source_root.join("agent-runtimes").join(runtime_id);
    if monorepo_package
        .join(format!("{runtime_id}.json"))
        .is_file()
    {
        return Ok(monorepo_package);
    }

    Err(Error::validation_invalid_argument(
        "source",
        format!(
            "No runtime package manifest '{}.json' found at source root or agent-runtimes/{}",
            runtime_id, runtime_id
        ),
        Some(source_root.display().to_string()),
        None,
    ))
}

fn validate_runtime_package(package_dir: &Path, runtime_id: &str) -> Result<()> {
    let manifest_path = package_dir.join(format!("{runtime_id}.json"));
    let content = local_files::local().read(&manifest_path)?;
    let manifest: AgentRuntimeManifest = config::from_str(&content)?;
    if manifest.id != runtime_id {
        return Err(Error::validation_invalid_argument(
            "runtime_id",
            format!(
                "Runtime package manifest id '{}' does not match requested id '{}'",
                manifest.id, runtime_id
            ),
            Some(runtime_id.to_string()),
            None,
        ));
    }
    Ok(())
}

fn write_source_metadata(
    package_dir: &Path,
    source: &str,
    source_revision: Option<&str>,
) -> Result<()> {
    std::fs::write(package_dir.join(".source-url"), source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("write runtime package source".to_string()),
        )
    })?;
    if let Some(revision) = source_revision {
        std::fs::write(package_dir.join(".source-revision"), revision).map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("write runtime package source revision".to_string()),
            )
        })?;
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, target: &Path) -> Result<()> {
    std::fs::create_dir_all(target).map_err(|e| {
        Error::internal_io(e.to_string(), Some("create runtime package".to_string()))
    })?;

    for entry in std::fs::read_dir(source).map_err(|e| {
        Error::internal_io(
            e.to_string(),
            Some("read runtime package source".to_string()),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("read runtime package entry".to_string()),
            )
        })?;
        let source_path = entry.path();
        let target_path = target.join(entry.file_name());
        let metadata = entry.metadata().map_err(|e| {
            Error::internal_io(
                e.to_string(),
                Some("inspect runtime package entry".to_string()),
            )
        })?;
        if metadata.is_dir() {
            copy_dir_recursive(&source_path, &target_path)?;
        } else if metadata.is_file() {
            std::fs::copy(&source_path, &target_path).map_err(|e| {
                Error::internal_io(e.to_string(), Some("copy runtime package file".to_string()))
            })?;
        }
    }

    Ok(())
}

fn rename_path(from: &Path, to: &Path, context: &str) -> Result<()> {
    std::fs::rename(from, to)
        .map_err(|e| Error::internal_io(e.to_string(), Some(context.to_string())))
}

fn remove_path_if_exists(path: &Path, context: &str) -> Result<()> {
    let Ok(metadata) = std::fs::symlink_metadata(path) else {
        return Ok(());
    };
    let result = if metadata.file_type().is_symlink() || metadata.is_file() {
        std::fs::remove_file(path)
    } else {
        std::fs::remove_dir_all(path)
    };
    result.map_err(|e| Error::internal_io(e.to_string(), Some(context.to_string())))
}

fn path_exists_or_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

fn is_symlink(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::with_isolated_home;
    use std::process::Command;

    fn write_runtime_package(root: &Path, runtime_id: &str, marker: &str) {
        let package = root.join("agent-runtimes").join(runtime_id);
        std::fs::create_dir_all(&package).expect("runtime package dir");
        std::fs::write(
            package.join(format!("{runtime_id}.json")),
            format!(
                r#"{{
  "schema": "homeboy/agent-runtime-manifest/v1",
  "id": "{}"
}}"#,
                runtime_id
            ),
        )
        .expect("runtime package manifest");
        std::fs::write(package.join("marker.txt"), marker).expect("runtime package marker");
    }

    fn tree_bytes(root: &Path) -> Vec<(PathBuf, Vec<u8>)> {
        fn collect(root: &Path, path: &Path, entries: &mut Vec<(PathBuf, Vec<u8>)>) {
            let mut children = std::fs::read_dir(path)
                .expect("read source tree")
                .map(|entry| entry.expect("source tree entry"))
                .collect::<Vec<_>>();
            children.sort_by_key(|entry| entry.file_name());
            for child in children {
                let path = child.path();
                if path.is_dir() {
                    collect(root, &path, entries);
                } else {
                    entries.push((
                        path.strip_prefix(root)
                            .expect("source tree relative path")
                            .to_path_buf(),
                        std::fs::read(&path).expect("read source tree file"),
                    ));
                }
            }
        }

        let mut entries = Vec::new();
        collect(root, root, &mut entries);
        entries
    }

    fn commit_source(root: &Path) {
        for args in [
            vec!["init", "-q"],
            vec!["add", "."],
            vec![
                "-c",
                "user.name=Homeboy Test",
                "-c",
                "user.email=test@homeboy.invalid",
                "commit",
                "-q",
                "-m",
                "initial",
            ],
        ] {
            let status = Command::new("git")
                .args(["-C", root.to_str().expect("source path")])
                .args(args)
                .status()
                .expect("run git");
            assert!(status.success(), "git command succeeds");
        }
    }

    #[test]
    fn refresh_installs_runtime_package_from_monorepo_source() {
        with_isolated_home(|_| {
            let source = tempfile::TempDir::new().expect("source tempdir");
            write_runtime_package(source.path(), "neutral-runtime", "v1");

            let result = refresh("neutral-runtime", &source.path().to_string_lossy(), None)
                .expect("refresh runtime package");

            assert_eq!(result.runtime_id, "neutral-runtime");
            assert!(!result.replaced_existing);
            assert!(result.path.ends_with("agent-runtimes/neutral-runtime"));
            assert_eq!(
                std::fs::read_to_string(result.path.join("marker.txt")).unwrap(),
                "v1"
            );
        });
    }

    #[test]
    fn refresh_replaces_existing_runtime_package() {
        with_isolated_home(|_| {
            let source = tempfile::TempDir::new().expect("source tempdir");
            write_runtime_package(source.path(), "neutral-runtime", "v1");
            refresh("neutral-runtime", &source.path().to_string_lossy(), None)
                .expect("first refresh");

            write_runtime_package(source.path(), "neutral-runtime", "v2");
            let result = refresh("neutral-runtime", &source.path().to_string_lossy(), None)
                .expect("second refresh");

            assert!(result.replaced_existing);
            assert_eq!(
                std::fs::read_to_string(result.path.join("marker.txt")).unwrap(),
                "v2"
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn refresh_materializes_symlinked_runtime_root_without_mutating_source() {
        use std::os::unix::fs::symlink;

        with_isolated_home(|home| {
            let source = tempfile::TempDir::new().expect("source tempdir");
            write_runtime_package(source.path(), "neutral-runtime", "new");
            write_runtime_package(source.path(), "sibling-runtime", "sibling");
            commit_source(source.path());
            let source_before = tree_bytes(source.path());
            let source_revision =
                crate::git::short_head_revision(source.path()).expect("source revision");

            let runtime_root = home.path().join(".config/homeboy/agent-runtimes");
            std::fs::create_dir_all(runtime_root.parent().expect("runtime root parent"))
                .expect("runtime root parent");
            symlink(source.path().join("agent-runtimes"), &runtime_root)
                .expect("symlink runtime root to source tree");

            let result = refresh("neutral-runtime", &source.path().to_string_lossy(), None)
                .expect("refresh runtime package");

            assert_eq!(tree_bytes(source.path()), source_before);
            assert!(!std::fs::symlink_metadata(&runtime_root)
                .expect("materialized runtime root metadata")
                .file_type()
                .is_symlink());
            assert_eq!(
                std::fs::read_to_string(runtime_root.join("sibling-runtime/marker.txt"))
                    .expect("preserved sibling runtime"),
                "sibling"
            );
            assert_eq!(
                std::fs::read_to_string(result.path.join(".source-revision"))
                    .expect("installed revision metadata"),
                source_revision
            );

            let manifest = crate::agent_runtime_manifest::discover_agent_runtime_catalog()
                .manifests
                .into_iter()
                .find(|manifest| manifest.id == "neutral-runtime")
                .expect("discovered installed runtime");
            let plan = crate::agent_runtime_manifest::runtime_materialization_plan(
                &manifest,
                "test-provider",
            );
            assert_eq!(plan.selected_identity.revision, Some(source_revision));
        });
    }
}
