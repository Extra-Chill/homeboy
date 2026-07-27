//! Filesystem pipeline steps — `symlink` and `shared-path` ensure/verify/cleanup.

use std::path::{Path, PathBuf};

use super::super::expand::expand_vars;
use super::super::spec::{RigSpec, SharedPathOp, SharedPathSpec, SymlinkOp, SymlinkSpec};
use super::super::state::{now_rfc3339, RigState, SharedPathState};
use homeboy_core::error::{Error, Result};

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

/// What a single ownership-checked shared-path repair did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SharedPathRepairStatus {
    /// Drift corrected using the declared `ensure` / `cleanup` ops.
    Repaired,
    /// Already healthy — nothing to do.
    Unchanged,
    /// Drift this rig must not fix on its own; needs manual attention.
    Blocked,
}

/// One shared path's repair outcome, shaped for `RepairResourceReport`.
#[derive(Debug, Clone)]
pub(crate) struct SharedPathRepair {
    pub link: String,
    pub target: String,
    pub previous_target: Option<String>,
    pub status: SharedPathRepairStatus,
    pub detail: Option<String>,
    pub error: Option<String>,
}

/// Repair declared shared paths without running the `up` pipeline.
///
/// This is the same ownership contract as `SharedPathOp::Ensure` /
/// `SharedPathOp::Cleanup`: only links this rig created and still owns are
/// removed, real files and directories are never touched, and a symlink owned
/// by something else is reported rather than replaced.
pub(crate) fn repair_shared_paths(rig: &RigSpec) -> Result<Vec<SharedPathRepair>> {
    if rig.shared_paths.is_empty() {
        return Ok(Vec::new());
    }

    let mut state = RigState::load(&rig.id)?;
    let mut state_changed = false;
    let mut repairs = Vec::with_capacity(rig.shared_paths.len());

    for shared in &rig.shared_paths {
        repairs.push(repair_shared_path(
            rig,
            shared,
            &mut state,
            &mut state_changed,
        )?);
    }

    if state_changed {
        state.save(&rig.id)?;
    }
    Ok(repairs)
}

fn repair_shared_path(
    rig: &RigSpec,
    shared: &SharedPathSpec,
    state: &mut RigState,
    state_changed: &mut bool,
) -> Result<SharedPathRepair> {
    let (link_path, target_path) = resolve_shared_paths(rig, shared);
    let key = shared_path_key(&link_path);
    let link = link_path.to_string_lossy().into_owned();
    let target = target_path.to_string_lossy().into_owned();
    let owned_target = state
        .shared_paths
        .get(&key)
        .map(|owned| PathBuf::from(&owned.target));

    let outcome = |status, previous_target, detail: &str, error: Option<String>| SharedPathRepair {
        link: link.clone(),
        target: target.clone(),
        previous_target,
        status,
        detail: Some(detail.to_string()),
        error,
    };

    match std::fs::symlink_metadata(&link_path) {
        Ok(metadata) if metadata.file_type().is_symlink() => {
            let current = std::fs::read_link(&link_path).map_err(|e| {
                Error::rig_pipeline_failed(
                    &rig.id,
                    "shared-path",
                    format!("read {}: {}", link_path.display(), e),
                )
            })?;
            let rig_owns_link = owned_target.as_ref().is_some_and(|owned| owned == &current);
            let previous = current.to_string_lossy().into_owned();

            if current == target_path {
                if target_path.exists() {
                    return Ok(outcome(
                        SharedPathRepairStatus::Unchanged,
                        Some(previous),
                        "shared path links to its declared target",
                        None,
                    ));
                }
                if !rig_owns_link {
                    return Ok(outcome(
                        SharedPathRepairStatus::Blocked,
                        Some(previous),
                        "shared target is missing behind a link this rig does not own",
                        Some(format!(
                            "shared target {} does not exist; repair only removes links it created",
                            target_path.display()
                        )),
                    ));
                }
                // Rig-owned link to a target that vanished: `cleanup` removes
                // only what we created, leaving an honest missing dependency
                // instead of a dangling link.
                cleanup_shared_path(rig, shared, state, state_changed)?;
                return Ok(outcome(
                    SharedPathRepairStatus::Repaired,
                    Some(previous),
                    "removed rig-owned link to a target that no longer exists",
                    None,
                ));
            }

            if !rig_owns_link {
                return Ok(outcome(
                    SharedPathRepairStatus::Blocked,
                    Some(previous),
                    "shared path points somewhere this rig did not link it",
                    Some(format!(
                        "{} points at {}, expected {}; repair will not replace a symlink it does not own",
                        link_path.display(),
                        current.display(),
                        target_path.display()
                    )),
                ));
            }

            // Declared target moved. Drop the link we own, then re-ensure.
            cleanup_shared_path(rig, shared, state, state_changed)?;
            match ensure_after_cleanup(rig, shared, &target_path, state, state_changed) {
                Ok(()) => Ok(outcome(
                    SharedPathRepairStatus::Repaired,
                    Some(previous),
                    "relinked rig-owned shared path to its declared target",
                    None,
                )),
                Err(error) => Ok(outcome(
                    SharedPathRepairStatus::Blocked,
                    Some(previous),
                    "removed the drifted rig-owned link but could not relink it",
                    Some(error),
                )),
            }
        }
        Ok(_) => {
            // A real file or directory satisfies the dependency. Never remove
            // it — just drop a stale ownership record if we still hold one.
            if state.shared_paths.remove(&key).is_some() {
                *state_changed = true;
                return Ok(outcome(
                    SharedPathRepairStatus::Repaired,
                    None,
                    "cleared a stale ownership record for a real path at the link location",
                    None,
                ));
            }
            Ok(outcome(
                SharedPathRepairStatus::Unchanged,
                None,
                "a real path already satisfies the shared dependency",
                None,
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let stale_record = state.shared_paths.remove(&key).is_some();
            if stale_record {
                *state_changed = true;
            }
            match ensure_after_cleanup(rig, shared, &target_path, state, state_changed) {
                Ok(()) => Ok(outcome(
                    SharedPathRepairStatus::Repaired,
                    None,
                    "recreated the missing shared path link",
                    None,
                )),
                Err(error) => Ok(outcome(
                    SharedPathRepairStatus::Blocked,
                    None,
                    "shared path is missing and cannot be recreated safely",
                    Some(error),
                )),
            }
        }
        Err(e) => Ok(outcome(
            SharedPathRepairStatus::Blocked,
            None,
            "shared path could not be inspected",
            Some(format!("stat {}: {}", link_path.display(), e)),
        )),
    }
}

/// Run the declared `ensure` op for one shared path, reporting why it could not
/// run instead of failing the whole repair.
fn ensure_after_cleanup(
    rig: &RigSpec,
    shared: &SharedPathSpec,
    target_path: &Path,
    state: &mut RigState,
    state_changed: &mut bool,
) -> std::result::Result<(), String> {
    if !target_path.exists() {
        return Err(format!(
            "shared target {} does not exist",
            target_path.display()
        ));
    }
    ensure_shared_path(rig, shared, state, state_changed).map_err(|error| error.to_string())
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
