//! Filesystem pipeline steps — `symlink` and `shared-path` ensure/verify/cleanup.

use std::path::{Path, PathBuf};

use super::super::expand::expand_vars;
use super::super::spec::{RigSpec, SharedPathOp, SharedPathSpec, SymlinkOp, SymlinkSpec};
use super::super::state::{now_rfc3339, RigState, SharedPathState};
use crate::core::error::{Error, Result};

pub(super) fn cleanup_shared_paths(rig: &RigSpec) -> Result<()> {
    run_shared_path_step(rig, SharedPathOp::Cleanup)
}

pub(super) fn run_symlink_step(rig: &RigSpec, op: SymlinkOp) -> Result<()> {
    for link in &rig.symlinks {
        match op {
            SymlinkOp::Ensure => ensure_symlink(rig, link)?,
            SymlinkOp::Verify => verify_symlink(rig, link)?,
        }
    }
    Ok(())
}

fn ensure_symlink(rig: &RigSpec, link: &SymlinkSpec) -> Result<()> {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));

    if let Some(parent) = link_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Error::rig_pipeline_failed(
                &rig.id,
                "symlink",
                format!("create parent of {}: {}", link_path.display(), e),
            )
        })?;
    }

    if link_path.exists() || link_path.is_symlink() {
        if let Ok(current) = std::fs::read_link(&link_path) {
            if current == target_path {
                return Ok(());
            }
        }
        std::fs::remove_file(&link_path).map_err(|e| {
            Error::rig_pipeline_failed(
                &rig.id,
                "symlink",
                format!("remove existing {}: {}", link_path.display(), e),
            )
        })?;
    }

    create_symlink(&target_path, &link_path).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!(
                "create {} → {}: {}",
                link_path.display(),
                target_path.display(),
                e
            ),
        )
    })?;
    Ok(())
}

#[cfg(unix)]
fn create_symlink(target: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(not(unix))]
fn create_symlink(_target: &Path, _link: &Path) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "rig symlinks are not supported on this platform (Unix only)",
    ))
}

fn verify_symlink(rig: &RigSpec, link: &SymlinkSpec) -> Result<()> {
    let link_path = PathBuf::from(expand_vars(rig, &link.link));
    let target_path = PathBuf::from(expand_vars(rig, &link.target));

    if !link_path.is_symlink() {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!("{} is not a symlink", link_path.display()),
        ));
    }
    let current = std::fs::read_link(&link_path).map_err(|e| {
        Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!("read {}: {}", link_path.display(), e),
        )
    })?;
    if current != target_path {
        return Err(Error::rig_pipeline_failed(
            &rig.id,
            "symlink",
            format!(
                "{} points at {}, expected {}",
                link_path.display(),
                current.display(),
                target_path.display()
            ),
        ));
    }
    Ok(())
}

pub(super) fn run_shared_path_step(rig: &RigSpec, op: SharedPathOp) -> Result<()> {
    if rig.shared_paths.is_empty() {
        return Ok(());
    }

    if op == SharedPathOp::Verify {
        for shared in &rig.shared_paths {
            verify_shared_path(rig, shared)?;
        }
        return Ok(());
    }

    let mut state = RigState::load(&rig.id)?;
    let mut state_changed = false;

    for shared in &rig.shared_paths {
        match op {
            SharedPathOp::Ensure => {
                ensure_shared_path(rig, shared, &mut state, &mut state_changed)?
            }
            SharedPathOp::Verify => verify_shared_path(rig, shared)?,
            SharedPathOp::Cleanup => {
                cleanup_shared_path(rig, shared, &mut state, &mut state_changed)?
            }
        }
    }

    if state_changed {
        state.save(&rig.id)?;
    }
    Ok(())
}

fn ensure_shared_path(
    rig: &RigSpec,
    shared: &SharedPathSpec,
    state: &mut RigState,
    state_changed: &mut bool,
) -> Result<()> {
    let (link_path, target_path) = resolve_shared_paths(rig, shared);
    let key = shared_path_key(&link_path);

    match std::fs::symlink_metadata(&link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            if current == target_path {
                if !target_path.exists() {
                    return Err(Error::rig_pipeline_failed(
                        &rig.id,
                        "shared-path",
                        format!("shared target {} does not exist", target_path.display()),
                    ));
                }
                return Ok(());
            }
            Err(Error::rig_pipeline_failed(
                &rig.id,
                "shared-path",
                format!(
                    "{} points at {}, expected {} — refusing to replace an existing symlink",
                    link_path.display(),
                    current.display(),
                    target_path.display()
                ),
            ))
        }
        Ok(_) => {
            if state.shared_paths.remove(&key).is_some() {
                *state_changed = true;
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if !target_path.exists() {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "shared target {} does not exist for {}",
                        target_path.display(),
                        link_path.display()
                    ),
                ));
            }
            let parent = link_path.parent().ok_or_else(|| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("{} has no parent directory", link_path.display()),
                )
            })?;
            if !parent.exists() {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "parent directory {} does not exist for {}",
                        parent.display(),
                        link_path.display()
                    ),
                ));
            }

            create_symlink(&target_path, &link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "create {} → {}: {}",
                        link_path.display(),
                        target_path.display(),
                        e
                    ),
                )
            })?;
            state.shared_paths.insert(
                key,
                SharedPathState {
                    target: target_path.to_string_lossy().into_owned(),
                    created_at: now_rfc3339(),
                },
            );
            *state_changed = true;
            Ok(())
        }
        Err(e) => Err(Error::rig_pipeline_failed(
            &rig.id,
            "shared-path",
            format!("stat {}: {}", link_path.display(), e),
        )),
    }
}

fn verify_shared_path(rig: &RigSpec, shared: &SharedPathSpec) -> Result<()> {
    let (link_path, target_path) = resolve_shared_paths(rig, shared);
    match std::fs::symlink_metadata(&link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            if current != target_path {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!(
                        "{} points at {}, expected {}",
                        link_path.display(),
                        current.display(),
                        target_path.display()
                    ),
                ));
            }
            if !target_path.exists() {
                return Err(Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("shared target {} does not exist", target_path.display()),
                ));
            }
            Ok(())
        }
        Ok(_) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(Error::rig_pipeline_failed(
            &rig.id,
            "shared-path",
            format!("{} is missing", link_path.display()),
        )),
        Err(e) => Err(Error::rig_pipeline_failed(
            &rig.id,
            "shared-path",
            format!("stat {}: {}", link_path.display(), e),
        )),
    }
}

fn cleanup_shared_path(
    rig: &RigSpec,
    shared: &SharedPathSpec,
    state: &mut RigState,
    state_changed: &mut bool,
) -> Result<()> {
    let (link_path, _target_path) = resolve_shared_paths(rig, shared);
    let key = shared_path_key(&link_path);
    let Some(owned) = state.shared_paths.get(&key).cloned() else {
        return Ok(());
    };
    let owned_target = PathBuf::from(&owned.target);

    if let Ok(metadata) = std::fs::symlink_metadata(&link_path) {
        if metadata.file_type().is_symlink() {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            if current == owned_target {
                std::fs::remove_file(&link_path).map_err(|e| {
                    Error::rig_pipeline_failed(
                        &rig.id,
                        "shared-path",
                        format!("remove {}: {}", link_path.display(), e),
                    )
                })?;
            }
        }
    }

    state.shared_paths.remove(&key);
    *state_changed = true;
    Ok(())
}

fn resolve_shared_paths(rig: &RigSpec, shared: &SharedPathSpec) -> (PathBuf, PathBuf) {
    (
        PathBuf::from(expand_vars(rig, &shared.link)),
        PathBuf::from(expand_vars(rig, &shared.target)),
    )
}

fn shared_path_key(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
