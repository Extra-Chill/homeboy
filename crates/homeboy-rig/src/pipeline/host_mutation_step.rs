//! Host mutation lifecycle pipeline step execution.

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::super::expand::expand_vars;
use super::super::spec::{HostMutationOp, RigSpec};
use homeboy_core::error::{Error, Result};
use homeboy_core::host_mutation_lifecycle::{
    HostMutation, HostMutationLifecycle, HostMutationRecord, HostMutationRevertStrategy,
    HostMutationStatus,
};

pub(super) fn run_host_mutation_step(
    rig: &RigSpec,
    op: HostMutationOp,
    dry_run: bool,
    lifecycle: &HostMutationLifecycle,
) -> Result<()> {
    let lifecycle = expand_lifecycle(rig, lifecycle);
    lifecycle.validate()?;

    if dry_run || op == HostMutationOp::Validate {
        return Ok(());
    }

    match op {
        HostMutationOp::Validate => Ok(()),
        HostMutationOp::Apply => {
            for record in &lifecycle.mutations {
                apply_record(rig, record)?;
            }
            Ok(())
        }
        HostMutationOp::Revert => {
            for record in lifecycle.mutations.iter().rev() {
                revert_record(rig, record)?;
            }
            Ok(())
        }
    }
}

fn expand_lifecycle(rig: &RigSpec, lifecycle: &HostMutationLifecycle) -> HostMutationLifecycle {
    let mut lifecycle = lifecycle.clone();
    for record in &mut lifecycle.mutations {
        record.actor = expand_vars(rig, &record.actor);
        record.mutation = expand_mutation(rig, &record.mutation);
        record.revert.backup_path = record
            .revert
            .backup_path
            .as_deref()
            .map(|value| expand_vars(rig, value));
        record.revert.target_path = record
            .revert
            .target_path
            .as_deref()
            .map(|value| expand_vars(rig, value));
    }
    lifecycle
}

fn expand_mutation(rig: &RigSpec, mutation: &HostMutation) -> HostMutation {
    match mutation {
        HostMutation::Symlink {
            link_path,
            target_path,
            previous_target_path,
        } => HostMutation::Symlink {
            link_path: expand_vars(rig, link_path),
            target_path: expand_vars(rig, target_path),
            previous_target_path: previous_target_path
                .as_deref()
                .map(|value| expand_vars(rig, value)),
        },
        HostMutation::TempDir { path, purpose } => HostMutation::TempDir {
            path: expand_vars(rig, path),
            purpose: purpose.clone(),
        },
        HostMutation::FileBackup {
            original_path,
            backup_path,
        } => HostMutation::FileBackup {
            original_path: expand_vars(rig, original_path),
            backup_path: expand_vars(rig, backup_path),
        },
        HostMutation::PackageManifestRewrite {
            manifest_path,
            package_manager,
            changes,
        } => HostMutation::PackageManifestRewrite {
            manifest_path: expand_vars(rig, manifest_path),
            package_manager: package_manager.clone(),
            changes: changes.clone(),
        },
    }
}

fn apply_record(rig: &RigSpec, record: &HostMutationRecord) -> Result<()> {
    if matches!(
        record.status,
        HostMutationStatus::Reverted | HostMutationStatus::Failed
    ) {
        return Err(step_error(
            rig,
            format!(
                "mutation '{}' has status {:?} and cannot be applied",
                record.id, record.status
            ),
        ));
    }

    match &record.mutation {
        HostMutation::Symlink {
            link_path,
            target_path,
            ..
        } => apply_symlink(rig, Path::new(link_path), Path::new(target_path)),
        HostMutation::TempDir { path, .. } => std::fs::create_dir_all(path)
            .map_err(|e| step_error(rig, format!("create temp dir {}: {}", path, e))),
        HostMutation::FileBackup {
            original_path,
            backup_path,
        } => {
            if Path::new(backup_path).exists() {
                Ok(())
            } else {
                copy_file(rig, original_path, backup_path, "backup file")
            }
        }
        HostMutation::PackageManifestRewrite { manifest_path, .. } => {
            let backup_path = record.revert.backup_path.as_deref().ok_or_else(|| {
                step_error(
                    rig,
                    format!(
                        "mutation '{}' requires revert.backup_path before package manifest rewrite",
                        record.id
                    ),
                )
            })?;
            if !Path::new(backup_path).exists() {
                copy_file(rig, manifest_path, backup_path, "backup package manifest")?;
            }
            rewrite_package_manifest(rig, record)
        }
    }
}

fn revert_record(rig: &RigSpec, record: &HostMutationRecord) -> Result<()> {
    match record.revert.strategy {
        HostMutationRevertStrategy::RemovePath => match &record.mutation {
            HostMutation::Symlink { link_path, .. } => remove_path(rig, link_path),
            HostMutation::TempDir { path, .. } => remove_path(rig, path),
            _ => Err(step_error(
                rig,
                format!(
                    "mutation '{}' cannot be reverted by removing a path",
                    record.id
                ),
            )),
        },
        HostMutationRevertStrategy::RestoreBackup => match &record.mutation {
            HostMutation::FileBackup { original_path, .. } => {
                let backup_path = record.revert.backup_path.as_deref().ok_or_else(|| {
                    step_error(
                        rig,
                        format!("mutation '{}' is missing backup_path", record.id),
                    )
                })?;
                copy_file(rig, backup_path, original_path, "restore backup")
            }
            _ => Err(step_error(
                rig,
                format!("mutation '{}' is not a file backup", record.id),
            )),
        },
        HostMutationRevertStrategy::RestoreSymlink => match &record.mutation {
            HostMutation::Symlink {
                link_path,
                previous_target_path,
                ..
            } => {
                let target_path = record
                    .revert
                    .target_path
                    .as_deref()
                    .or(previous_target_path.as_deref())
                    .ok_or_else(|| {
                        step_error(
                            rig,
                            format!("mutation '{}' is missing target_path", record.id),
                        )
                    })?;
                apply_symlink(rig, Path::new(link_path), Path::new(target_path))
            }
            _ => Err(step_error(
                rig,
                format!("mutation '{}' is not a symlink", record.id),
            )),
        },
        HostMutationRevertStrategy::RestorePackageManifest => match &record.mutation {
            HostMutation::PackageManifestRewrite { manifest_path, .. } => {
                let backup_path = record.revert.backup_path.as_deref().ok_or_else(|| {
                    step_error(
                        rig,
                        format!("mutation '{}' is missing backup_path", record.id),
                    )
                })?;
                copy_file(rig, backup_path, manifest_path, "restore package manifest")
            }
            _ => Err(step_error(
                rig,
                format!("mutation '{}' is not a package manifest rewrite", record.id),
            )),
        },
        HostMutationRevertStrategy::Manual => Err(step_error(
            rig,
            format!(
                "mutation '{}' requires manual revert: {}",
                record.id,
                record.revert.notes.as_deref().unwrap_or("no notes")
            ),
        )),
    }
}

fn apply_symlink(rig: &RigSpec, link_path: &Path, target_path: &Path) -> Result<()> {
    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            step_error(
                rig,
                format!("create parent of {}: {}", link_path.display(), e),
            )
        })?;
    }

    match std::fs::symlink_metadata(link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(link_path)
                .map_err(|e| step_error(rig, format!("read {}: {}", link_path.display(), e)))?;
            if current == target_path {
                return Ok(());
            }
            std::fs::remove_file(link_path)
                .map_err(|e| step_error(rig, format!("remove {}: {}", link_path.display(), e)))?;
        }
        Ok(_) => {
            return Err(step_error(
                rig,
                format!(
                    "{} exists and is not a symlink; refusing to replace it",
                    link_path.display()
                ),
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => {
            return Err(step_error(
                rig,
                format!("stat {}: {}", link_path.display(), e),
            ));
        }
    }

    create_symlink(target_path, link_path).map_err(|e| {
        step_error(
            rig,
            format!(
                "create {} -> {}: {}",
                link_path.display(),
                target_path.display(),
                e
            ),
        )
    })
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "host mutation symlinks are not supported on this platform",
    ))
}

fn remove_path(rig: &RigSpec, path: &str) -> Result<()> {
    let path = PathBuf::from(path);
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_symlink() || metadata.is_file() => {
            std::fs::remove_file(&path)
                .map_err(|e| step_error(rig, format!("remove {}: {}", path.display(), e)))
        }
        Ok(metadata) if metadata.is_dir() => std::fs::remove_dir_all(&path)
            .map_err(|e| step_error(rig, format!("remove directory {}: {}", path.display(), e))),
        Ok(_) => Err(step_error(
            rig,
            format!(
                "{} exists but is not a file, symlink, or directory",
                path.display()
            ),
        )),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(step_error(rig, format!("stat {}: {}", path.display(), e))),
    }
}

fn copy_file(rig: &RigSpec, from: &str, to: &str, action: &str) -> Result<()> {
    let from = Path::new(from);
    let to = Path::new(to);
    if let Some(parent) = to.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| step_error(rig, format!("create parent of {}: {}", to.display(), e)))?;
    }
    std::fs::copy(from, to).map(|_| ()).map_err(|e| {
        step_error(
            rig,
            format!("{} {} -> {}: {}", action, from.display(), to.display(), e),
        )
    })
}

fn rewrite_package_manifest(rig: &RigSpec, record: &HostMutationRecord) -> Result<()> {
    let HostMutation::PackageManifestRewrite {
        manifest_path,
        changes,
        ..
    } = &record.mutation
    else {
        return Err(step_error(
            rig,
            "mutation is not a package manifest rewrite",
        ));
    };

    let raw = std::fs::read_to_string(manifest_path)
        .map_err(|e| step_error(rig, format!("read {}: {}", manifest_path, e)))?;
    let mut manifest: Value = serde_json::from_str(&raw)
        .map_err(|e| step_error(rig, format!("parse {} as JSON: {}", manifest_path, e)))?;

    for change in changes {
        apply_package_change(rig, record, &mut manifest, change)?;
    }

    let rendered = serde_json::to_string_pretty(&manifest)
        .map_err(|e| step_error(rig, format!("serialize {}: {}", manifest_path, e)))?;
    std::fs::write(manifest_path, format!("{}\n", rendered))
        .map_err(|e| step_error(rig, format!("write {}: {}", manifest_path, e)))
}

fn apply_package_change(
    rig: &RigSpec,
    record: &HostMutationRecord,
    manifest: &mut Value,
    change: &homeboy_core::host_mutation_lifecycle::PackageManifestChange,
) -> Result<()> {
    const SECTIONS: &[&str] = &[
        "dependencies",
        "devDependencies",
        "peerDependencies",
        "optionalDependencies",
    ];

    for section in SECTIONS {
        let Some(dependencies) = manifest.get_mut(*section).and_then(Value::as_object_mut) else {
            continue;
        };
        if !dependencies.contains_key(&change.package) {
            continue;
        }
        let current = dependencies
            .get(&change.package)
            .and_then(Value::as_str)
            .map(str::to_string);
        if change.before.as_ref() != current.as_ref() {
            return Err(step_error(
                rig,
                format!(
                    "mutation '{}' expected {} in {} to be {:?}, found {:?}",
                    record.id, change.package, section, change.before, current
                ),
            ));
        }
        match &change.after {
            Some(after) => {
                dependencies.insert(change.package.clone(), Value::String(after.clone()));
            }
            None => {
                dependencies.remove(&change.package);
            }
        }
        return Ok(());
    }

    if change.before.is_some() {
        return Err(step_error(
            rig,
            format!(
                "mutation '{}' expected package {} to exist before rewrite",
                record.id, change.package
            ),
        ));
    }

    let dependencies = manifest
        .as_object_mut()
        .ok_or_else(|| step_error(rig, "package manifest root must be a JSON object"))?
        .entry("dependencies")
        .or_insert_with(|| Value::Object(Default::default()))
        .as_object_mut()
        .ok_or_else(|| step_error(rig, "package manifest dependencies must be a JSON object"))?;
    match &change.after {
        Some(after) => {
            dependencies.insert(change.package.clone(), Value::String(after.clone()));
            Ok(())
        }
        None => Ok(()),
    }
}

fn step_error(rig: &RigSpec, reason: impl Into<String>) -> Error {
    Error::rig_pipeline_failed(&rig.id, "host-mutation", reason)
}
