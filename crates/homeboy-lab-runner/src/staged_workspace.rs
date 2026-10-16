//! Runner-side authority for private `@file` command inputs and staged
//! workspaces consumed by the runner daemon's exec driver.

use homeboy_engine_primitives::content_hash;
#[cfg(unix)]
use std::{
    fs::File,
    io::Read,
    os::fd::{AsRawFd, FromRawFd},
    os::unix::{fs::OpenOptionsExt, process::CommandExt},
    process::Command,
};

use homeboy_core::error::{Error, Result};
use homeboy_core::runner_execution_envelope::RunnerExecutionEnvelope;

#[cfg(test)]
#[path = "staged_workspace_tests.rs"]
mod tests;

/// Private `@file` paths carry their SHA-256 in the runner-resident filename.
/// Verify them after a durable claim and before starting the child, then remove
/// them after execution so retries can resume before claim without retaining
/// plaintext after consumption.
#[derive(Debug)]
pub(crate) struct PrivateAtFileCleanup {
    paths: Vec<std::path::PathBuf>,
    directories: Vec<std::path::PathBuf>,
}

impl Drop for PrivateAtFileCleanup {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

impl PrivateAtFileCleanup {
    fn cleanup(&mut self) -> std::result::Result<(), String> {
        let mut failures = Vec::new();
        self.paths.retain(|path| match std::fs::remove_file(path) {
            Ok(()) => false,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
            Err(err) => {
                failures.push(format!("{}: {err}", path.display()));
                true
            }
        });
        self.directories
            .retain(|path| match std::fs::remove_dir_all(path) {
                Ok(()) => false,
                Err(err) if err.kind() == std::io::ErrorKind::NotFound => false,
                Err(err) => {
                    failures.push(format!("{}: {err}", path.display()));
                    true
                }
            });
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join(", "))
        }
    }
}

fn verify_private_at_files(
    envelope: &mut homeboy_core::runner_execution_envelope::RunnerExecutionEnvelope,
) -> Result<PrivateAtFileCleanup> {
    #[cfg(not(unix))]
    {
        if envelope.dispatch.as_ref().is_some_and(|dispatch| {
            dispatch
                .command
                .iter()
                .any(|argument| is_private_at_file(argument))
        }) {
            return Err(Error::validation_invalid_argument(
                "at_file",
                "private detached runner @files require Unix owner-only filesystem guarantees",
                None,
                None,
            ));
        }
    }

    let Some(command) = envelope
        .dispatch
        .as_mut()
        .map(|dispatch| &mut dispatch.command)
    else {
        return Ok(PrivateAtFileCleanup {
            paths: Vec::new(),
            directories: Vec::new(),
        });
    };
    verify_private_command_arguments(command)
}

/// Verify private command inputs and retain
/// immutable owner-only snapshots until the accepted child is reaped.
pub(crate) fn verify_private_command_arguments(
    command: &mut [String],
) -> Result<PrivateAtFileCleanup> {
    #[cfg(not(unix))]
    if command.iter().any(|argument| is_private_at_file(argument)) {
        return Err(Error::invalid_argument(
            "at_file",
            "private runner inputs require Unix owner-only filesystem guarantees",
        ));
    }
    let mut cleanup = PrivateAtFileCleanup {
        paths: Vec::new(),
        directories: Vec::new(),
    };
    let mut snapshot_directory = None;
    for argument in command.iter_mut() {
        let value = at_file_path(argument);
        let Some(path) = value else {
            continue;
        };
        let Some(filename) = std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
        else {
            continue;
        };
        let Some(expected) = filename
            .strip_prefix("private-sha256-")
            .and_then(|value| value.split_once('-').map(|(digest, _)| digest))
        else {
            continue;
        };
        // Register the controller-owned source before inspecting it so every
        // rejected private input receives an owner-safe cleanup attempt.
        cleanup.paths.push(std::path::PathBuf::from(path));
        if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return private_at_file_error(
                &mut cleanup,
                "private runner @file has an invalid SHA-256 identity",
                path,
            );
        }
        let metadata = match std::fs::symlink_metadata(path) {
            Ok(metadata) => metadata,
            Err(err) => {
                return private_at_file_error(
                    &mut cleanup,
                    &format!("stat private runner @file {path}: {err}"),
                    path,
                );
            }
        };
        if !metadata.file_type().is_file() {
            return private_at_file_error(
                &mut cleanup,
                "private runner @file is not a regular file",
                path,
            );
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o077 != 0 {
                return private_at_file_error(
                    &mut cleanup,
                    "private runner @file is readable by group or other users",
                    path,
                );
            }
        }
        let mut file = match open_private_at_file(path) {
            Ok(file) => file,
            Err(err) => {
                return private_at_file_error(
                    &mut cleanup,
                    &format!("read private runner @file {path}: {err}"),
                    path,
                );
            }
        };
        let opened_metadata = match file.metadata() {
            Ok(metadata) => metadata,
            Err(err) => {
                return private_at_file_error(
                    &mut cleanup,
                    &format!("stat opened private runner @file {path}: {err}"),
                    path,
                );
            }
        };
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if opened_metadata.permissions().mode() & 0o077 != 0 {
                return private_at_file_error(
                    &mut cleanup,
                    "opened private runner @file is readable by group or other users",
                    path,
                );
            }
        }
        let content = match read_private_at_file(&mut file) {
            Ok(content) => content,
            Err(err) => {
                return private_at_file_error(
                    &mut cleanup,
                    &format!("read private runner @file {path}: {err}"),
                    path,
                );
            }
        };
        if content_hash::sha256_hex(&content) != expected {
            return private_at_file_error(
                &mut cleanup,
                "private runner @file content does not match its SHA-256 identity",
                path,
            );
        }
        if snapshot_directory.is_none() {
            let directory = match private_at_file_snapshot_directory() {
                Ok(directory) => directory,
                Err(err) => {
                    return private_at_file_error(
                        &mut cleanup,
                        &format!("create private runner @file snapshot directory: {err}"),
                        path,
                    );
                }
            };
            cleanup.directories.push(directory.clone());
            snapshot_directory = Some(directory);
        }
        let directory = snapshot_directory
            .as_deref()
            .expect("private snapshot directory");
        let snapshot = match write_private_at_file_snapshot(directory, &content) {
            Ok(snapshot) => snapshot,
            Err(err) => {
                return private_at_file_error(
                    &mut cleanup,
                    &format!("write verified private runner @file snapshot: {err}"),
                    path,
                );
            }
        };
        cleanup.paths.push(snapshot.clone());
        rewrite_at_file_argument(argument, &snapshot.display().to_string());
    }
    Ok(cleanup)
}

#[cfg(not(unix))]
fn is_private_at_file(argument: &str) -> bool {
    at_file_path(argument).is_some_and(|path| {
        std::path::Path::new(path)
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with("private-sha256-"))
    })
}

fn at_file_path(argument: &str) -> Option<&str> {
    argument.strip_prefix('@').or_else(|| {
        argument
            .split_once('=')
            .and_then(|(_, value)| value.strip_prefix('@'))
    })
}

fn rewrite_at_file_argument(argument: &mut String, snapshot: &str) {
    if argument.starts_with('@') {
        *argument = format!("@{snapshot}");
    } else if let Some((name, _)) = argument.split_once('=') {
        *argument = format!("{name}=@{snapshot}");
    }
}

fn private_at_file_error(
    cleanup: &mut PrivateAtFileCleanup,
    message: &str,
    path: &str,
) -> Result<PrivateAtFileCleanup> {
    let cleanup_result = match cleanup.cleanup() {
        Ok(()) => "private input cleanup succeeded".to_string(),
        Err(err) => format!("private input cleanup failed: {err}"),
    };
    Err(Error::validation_invalid_argument(
        "at_file",
        format!("{message}; {cleanup_result}"),
        Some(path.to_string()),
        None,
    ))
}

fn open_private_at_file(path: &str) -> std::io::Result<std::fs::File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW);
    }
    options.open(path)
}

fn read_private_at_file(file: &mut std::fs::File) -> std::io::Result<Vec<u8>> {
    use std::io::Read;

    let mut content = Vec::new();
    file.read_to_end(&mut content)?;
    Ok(content)
}

fn write_private_at_file_snapshot(
    directory: &std::path::Path,
    content: &[u8],
) -> std::io::Result<std::path::PathBuf> {
    use std::io::Write;

    let snapshot = directory.join(format!("verified-{}", uuid::Uuid::new_v4()));
    let temporary = snapshot.with_extension("tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options.open(&temporary)?;
    if let Err(err) = file.write_all(content).and_then(|_| file.sync_all()) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }
    if let Err(err) = std::fs::rename(&temporary, &snapshot) {
        let _ = std::fs::remove_file(&temporary);
        return Err(err);
    }
    Ok(snapshot)
}

#[cfg(unix)]
fn private_at_file_snapshot_directory() -> std::io::Result<std::path::PathBuf> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let root = std::env::var_os("XDG_DATA_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| {
            homeboy_core::paths::home_root()
                .ok()
                .map(|home| home.join(".local/share"))
        })
        .unwrap_or_else(std::env::temp_dir)
        .join("homeboy/reverse-runner-private");
    std::fs::create_dir_all(&root)?;
    std::fs::set_permissions(&root, std::fs::Permissions::from_mode(0o700))?;
    let root_metadata = std::fs::metadata(&root)?;
    if root_metadata.permissions().mode() & 0o077 != 0
        || root_metadata.uid() != unsafe { libc::geteuid() }
    {
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private runner snapshot root is not worker-owned 0700",
        ));
    }
    let directory = root.join(format!("run-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory)?;
    std::fs::set_permissions(&directory, std::fs::Permissions::from_mode(0o700))?;
    let directory_metadata = std::fs::metadata(&directory)?;
    if directory_metadata.permissions().mode() & 0o077 != 0
        || directory_metadata.uid() != unsafe { libc::geteuid() }
    {
        let _ = std::fs::remove_dir(&directory);
        return Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "private runner snapshot directory is not worker-owned 0700",
        ));
    }
    Ok(directory)
}

#[cfg(not(unix))]
fn private_at_file_snapshot_directory() -> std::io::Result<std::path::PathBuf> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "private runner snapshots require Unix owner-only filesystem guarantees",
    ))
}

/// A staged job's source is runner-owned durable storage, not a controller path.
/// Verify and extract it before the normal queue executor consumes the command.
pub(crate) fn materialize_staged_source_artifact(
    runner_id: &str,
    job_id: &str,
    envelope: &mut RunnerExecutionEnvelope,
) -> Result<Option<StagedWorkspaceDirectory>> {
    if let Some(workspace) = envelope
        .metadata
        .get("staged_workspace_materialization")
        .filter(|workspace| !workspace.is_null())
    {
        let workspace: crate::runner_staging_operation::ControllerWorkspaceMaterialization =
            serde_json::from_value(workspace.clone()).map_err(|error| {
                Error::validation_invalid_argument(
                    "staged_workspace_materialization",
                    format!("invalid staged workspace materialization: {error}"),
                    None,
                    None,
                )
            })?;
        let dispatch = envelope.dispatch.as_mut().ok_or_else(|| {
            Error::internal_unexpected("staged runner job has no execution dispatch")
        })?;
        if dispatch.cwd.as_deref() != Some(workspace.remote_cwd.as_str()) {
            return Err(Error::validation_invalid_argument(
                "staged_workspace_materialization",
                "staged runner workspace does not match the claimed execution target",
                Some(workspace.workspace_id),
                None,
            ));
        }
        let mut directory = verify_controller_workspace(runner_id, &workspace, None)?;
        verify_staged_workspace_path(&directory, &workspace.remote_cwd)?;
        directory._private_plans = Some(bind_materialized_cook_workspace_identity(
            envelope,
            &workspace.remote_cwd,
        )?);
        verify_staged_workspace_path(&directory, &workspace.remote_cwd)?;
        return Ok(Some(directory));
    }
    let Some(source) = envelope
        .metadata
        .get("staged_source_artifact")
        .filter(|source| !source.is_null())
    else {
        return Ok(None);
    };
    let source = serde_json::from_value(source.clone()).map_err(|error| {
        Error::validation_invalid_argument(
            "staged_source_artifact",
            format!("invalid staged source artifact: {error}"),
            None,
            None,
        )
    })?;
    let session_path = homeboy_core::paths::runner_session_file(runner_id)?;
    let store_path = session_path.with_file_name(format!("{runner_id}-staging/store.json"));
    let workspace_root = session_path
        .with_file_name(format!("{runner_id}-staging/workloads"))
        .join(job_id);
    let workspace = crate::runner_staging_store::extract_staged_source_artifact(
        store_path,
        &source,
        &workspace_root,
    )?;
    let dispatch = envelope
        .dispatch
        .as_mut()
        .ok_or_else(|| Error::internal_unexpected("staged runner job has no execution dispatch"))?;
    dispatch.cwd = Some(workspace.display().to_string());
    Ok(Some(open_staged_workspace_directory(&workspace)?))
}

pub(crate) struct StagedWorkspaceDirectory {
    #[cfg(unix)]
    directory: File,
    _private_plans: Option<PrivateAtFileCleanup>,
}

/// Controller filesystem identity ends at verified snapshot materialization.
/// Bind the runner identity only inside the existing staged-workspace authority,
/// retaining private command files for the lifetime of that authority.
fn bind_materialized_cook_workspace_identity(
    envelope: &mut RunnerExecutionEnvelope,
    workspace_root: &str,
) -> Result<PrivateAtFileCleanup> {
    let mut cleanup = verify_private_at_files(envelope)?;
    let Some(dispatch) = envelope.dispatch.as_mut() else {
        return Ok(cleanup);
    };
    let authority_root = std::fs::canonicalize(workspace_root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(workspace_root.to_string())))?;
    let mut plan_flag = false;
    for argument in &mut dispatch.command {
        if argument == "--" {
            break;
        }
        if matches!(argument.as_str(), "--plan" | "--attempt-plan") {
            plan_flag = true;
            continue;
        }
        let spec = if plan_flag {
            argument.as_str()
        } else if let Some(spec) = argument
            .strip_prefix("--plan=")
            .or_else(|| argument.strip_prefix("--attempt-plan="))
        {
            spec
        } else {
            continue;
        };
        plan_flag = false;
        let raw = homeboy_core::config::read_json_spec_to_string(spec)?;
        let mut plan: serde_json::Value = serde_json::from_str(&raw).map_err(|error| {
            Error::validation_invalid_json(error, Some("bind staged Cook plan".to_string()), None)
        })?;
        let mut changed = false;
        for task in plan["tasks"].as_array_mut().into_iter().flatten() {
            if task.pointer("/metadata/cook_workspace_identity").is_none() {
                continue;
            }
            let root = task
                .pointer("/workspace/root")
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| {
                    Error::validation_invalid_argument(
                        "workspace",
                        "staged Cook identity requires a workspace root",
                        None,
                        None,
                    )
                })?;
            let identity = homeboy_agents::agent_task_workspace_identity::attest_workspace(
                std::path::Path::new(root),
            )?;
            let canonical = identity["canonical_path"].as_str().ok_or_else(|| {
                Error::internal_unexpected("workspace attestation has no canonical path")
            })?;
            if !std::path::Path::new(canonical).starts_with(&authority_root) {
                return Err(Error::validation_invalid_argument(
                    "workspace",
                    "Cook identity lies outside the verified staged workspace",
                    Some(root.to_string()),
                    None,
                ));
            }
            task["metadata"]["cook_workspace_identity"] = identity;
            changed = true;
        }
        if changed {
            let directory = private_at_file_snapshot_directory().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("staged Cook private plan".to_string()),
                )
            })?;
            cleanup.directories.push(directory.clone());
            let content = serde_json::to_vec(&plan).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize staged Cook plan".to_string()),
                )
            })?;
            let path = write_private_at_file_snapshot(&directory, &content).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("write staged Cook private plan".to_string()),
                )
            })?;
            cleanup.paths.push(path.clone());
            *argument = if argument.starts_with("--") {
                let flag = argument.split_once('=').expect("inline plan option").0;
                format!("{flag}=@{}", path.display())
            } else {
                format!("@{}", path.display())
            };
        }
    }
    Ok(cleanup)
}

impl StagedWorkspaceDirectory {
    #[cfg(unix)]
    pub(crate) fn fd(&self) -> std::os::fd::RawFd {
        self.directory.as_raw_fd()
    }
}

pub(crate) fn verify_staged_workspace_before_execution(
    runner_id: &str,
    envelope: &RunnerExecutionEnvelope,
    authority: Option<&StagedWorkspaceDirectory>,
) -> Result<()> {
    let Some(raw) = envelope
        .metadata
        .get("staged_workspace_materialization")
        .filter(|workspace| !workspace.is_null())
    else {
        if envelope
            .metadata
            .get("staged_source_artifact")
            .filter(|source| !source.is_null())
            .is_some()
        {
            let authority = authority.ok_or_else(|| {
                Error::internal_unexpected(
                    "staged source workspace authority was not retained until execution",
                )
            })?;
            let cwd = envelope
                .dispatch
                .as_ref()
                .and_then(|dispatch| dispatch.cwd.as_deref())
                .ok_or_else(|| {
                    Error::internal_unexpected("staged runner job has no execution cwd")
                })?;
            verify_staged_workspace_path(authority, cwd)?;
        }
        return Ok(());
    };
    let workspace = serde_json::from_value(raw.clone()).map_err(|error| {
        Error::validation_invalid_argument(
            "staged_workspace_materialization",
            format!("invalid staged workspace materialization: {error}"),
            None,
            None,
        )
    })?;
    let authority = authority.ok_or_else(|| {
        Error::internal_unexpected("staged workspace authority was not retained until execution")
    })?;
    verify_controller_workspace(runner_id, &workspace, Some(authority))?;
    Ok(())
}

#[cfg(unix)]
fn open_staged_workspace_directory(path: &std::path::Path) -> Result<StagedWorkspaceDirectory> {
    use std::fs::OpenOptions;

    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    let directory = options.open(path).map_err(|error| {
        Error::validation_invalid_argument(
            "staged_source_artifact",
            format!("open extracted runner workspace without following links: {error}"),
            Some(path.display().to_string()),
            None,
        )
    })?;
    Ok(StagedWorkspaceDirectory {
        directory,
        _private_plans: None,
    })
}

#[cfg(not(unix))]
fn open_staged_workspace_directory(_path: &std::path::Path) -> Result<StagedWorkspaceDirectory> {
    Err(Error::validation_invalid_argument(
        "staged_source_artifact",
        "staged source workspaces require Unix no-follow verification",
        None,
        None,
    ))
}

#[cfg(unix)]
fn verify_staged_workspace_path(authority: &StagedWorkspaceDirectory, path: &str) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let current = open_staged_workspace_directory(std::path::Path::new(path))?;
    let retained = authority.directory.metadata().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("stat retained staged workspace".to_string()),
        )
    })?;
    let current = current.directory.metadata().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("stat staged workspace path".to_string()),
        )
    })?;
    if retained.dev() != current.dev() || retained.ino() != current.ino() {
        return Err(Error::validation_invalid_argument(
            "staged_source_artifact",
            "staged source workspace path was replaced after materialization",
            Some(path.to_string()),
            None,
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
fn verify_staged_workspace_path(_authority: &StagedWorkspaceDirectory, _path: &str) -> Result<()> {
    unreachable!("non-Unix staged workspaces cannot be materialized")
}

#[cfg(unix)]
fn verify_controller_workspace(
    runner_id: &str,
    workspace: &crate::runner_staging_operation::ControllerWorkspaceMaterialization,
    retained: Option<&StagedWorkspaceDirectory>,
) -> Result<StagedWorkspaceDirectory> {
    use std::fs::OpenOptions;

    workspace.validate()?;
    let session_path = homeboy_core::paths::runner_session_file(runner_id)?;
    let store_path = session_path.with_file_name(format!("{runner_id}-staging/store.json"));
    crate::runner_staging_store::verify_staged_workspace_materialization(store_path, workspace)?;
    let mut options = OpenOptions::new();
    options
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_DIRECTORY);
    let opened = options.open(&workspace.remote_cwd).map_err(|error| {
        Error::validation_invalid_argument(
            "staged_workspace_materialization",
            format!("open runner-owned workspace without following links: {error}"),
            Some(workspace.remote_cwd.clone()),
            None,
        )
    })?;
    let directory = retained
        .map(|authority| &authority.directory)
        .unwrap_or(&opened);
    let metadata = read_workspace_metadata_nofollow(&directory)?;
    let actual_path = metadata
        .get("remote_path")
        .and_then(serde_json::Value::as_str);
    let snapshot = metadata
        .get("snapshot_identity")
        .and_then(serde_json::Value::as_str);
    let lease = metadata
        .get("workspace_lease")
        .and_then(serde_json::Value::as_str);
    let commit = metadata
        .get("source_commit")
        .and_then(serde_json::Value::as_str);
    let run_id = metadata.get("run_id").and_then(serde_json::Value::as_str);
    if actual_path != Some(workspace.remote_cwd.as_str())
        || snapshot != Some(workspace.source_snapshot_id.as_str())
        || lease != Some(workspace.workspace_lease.as_str())
        || commit != Some(workspace.source_commit.as_str())
        || run_id != Some(workspace.run_id.as_str())
    {
        return Err(Error::validation_invalid_argument(
            "staged_workspace_materialization",
            "runner workspace metadata no longer matches the staged source identity, lease, and run ownership",
            Some(workspace.run_id.clone()),
            None,
        ));
    }
    let fd = directory.as_raw_fd();
    let mut command = Command::new("git");
    command.args(["status", "--porcelain=v1"]);
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(fd) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let output = command.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("verify staged Git workspace".to_string()),
        )
    })?;
    if !output.status.success() || !output.stdout.is_empty() {
        return Err(Error::validation_invalid_argument(
            "staged_workspace_materialization",
            "runner workspace Git baseline is missing or dirty after staging",
            Some(workspace.run_id.clone()),
            None,
        ));
    }
    let fd = directory.as_raw_fd();
    let mut command = Command::new("git");
    command.args(["rev-parse", "HEAD"]);
    unsafe {
        command.pre_exec(move || {
            if libc::fchdir(fd) == 0 {
                Ok(())
            } else {
                Err(std::io::Error::last_os_error())
            }
        });
    }
    let output = command.output().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read staged Git baseline".to_string()),
        )
    })?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != workspace.source_commit
    {
        return Err(Error::validation_invalid_argument(
            "staged_workspace_materialization",
            "runner workspace Git baseline does not match the staged source commit",
            Some(workspace.run_id.clone()),
            None,
        ));
    }
    Ok(StagedWorkspaceDirectory {
        directory: opened,
        _private_plans: None,
    })
}

#[cfg(not(unix))]
fn verify_controller_workspace(
    _runner_id: &str,
    _workspace: &crate::runner_staging_operation::ControllerWorkspaceMaterialization,
    _retained: Option<&StagedWorkspaceDirectory>,
) -> Result<StagedWorkspaceDirectory> {
    Err(Error::validation_invalid_argument(
        "staged_workspace_materialization",
        "controller-materialized workspaces require Unix no-follow verification",
        None,
        None,
    ))
}

#[cfg(unix)]
fn read_workspace_metadata_nofollow(directory: &File) -> Result<serde_json::Value> {
    use std::ffi::CString;

    fn open_at(directory: &File, name: &str, flags: i32) -> Result<File> {
        let name = CString::new(name).expect("literal has no NUL");
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                flags | libc::O_NOFOLLOW,
            )
        };
        if fd < 0 {
            return Err(Error::validation_invalid_argument(
                "staged_workspace_materialization",
                std::io::Error::last_os_error().to_string(),
                None,
                None,
            ));
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    }

    let homeboy = open_at(directory, ".homeboy", libc::O_RDONLY | libc::O_DIRECTORY)?;
    let mut metadata = open_at(&homeboy, "runner-workspace.json", libc::O_RDONLY)?;
    let mut bytes = Vec::new();
    metadata.read_to_end(&mut bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read runner workspace metadata".to_string()),
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("parse runner workspace metadata".to_string()),
        )
    })
}
