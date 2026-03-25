//! helpers — extracted from files.rs.

use crate::context::{require_project_base_path, resolve_project_ssh_with_base_path};
use crate::defaults;
use crate::engine::executor::execute_for_project;
use crate::engine::{command, shell};
use crate::error::{Error, Result};
use crate::paths::{self as base_path, resolve_path_string};
use crate::project;
use std::path::Path;
use std::process::Command;
use serde::Serialize;
use std::io::{self, Read};
use super::RenameResult;
use super::DownloadResult;


/// Rename or move file.
pub fn rename(project_id: &str, old_path: &str, new_path: &str) -> Result<RenameResult> {
    let project = project::load(project_id)?;
    let project_base_path = require_project_base_path(project_id, &project)?;
    let full_old = base_path::join_remote_path(Some(&project_base_path), old_path)?;
    let full_new = base_path::join_remote_path(Some(&project_base_path), new_path)?;
    let command = format!(
        "mv {} {}",
        shell::quote_path(&full_old),
        shell::quote_path(&full_new)
    );
    let output = execute_for_project(&project, &command)?;
    command::require_success(output.success, &output.stderr, "RENAME")?;

    Ok(RenameResult {
        base_path: Some(project_base_path),
        old_path: full_old,
        new_path: full_new,
    })
}

/// Download a file or directory from remote server via SCP.
pub fn download(
    project_id: &str,
    remote_path: &str,
    local_path: &str,
    recursive: bool,
) -> Result<DownloadResult> {
    let (ctx, project_base_path) = resolve_project_ssh_with_base_path(project_id)?;
    let full_remote_path = base_path::join_remote_path(Some(&project_base_path), remote_path)?;

    // Create local parent directories if needed
    let local = Path::new(local_path);
    if let Some(parent) = local.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            std::fs::create_dir_all(parent).map_err(|e| {
                Error::internal_io(
                    format!("Failed to create local directory: {}", e),
                    Some("create local directory".to_string()),
                )
            })?;
        }
    }

    let deploy_defaults = defaults::load_defaults().deploy;
    let mut scp_args: Vec<String> = deploy_defaults.scp_flags.clone();

    if recursive {
        scp_args.push("-r".to_string());
    }

    if let Some(identity_file) = &ctx.client.identity_file {
        scp_args.extend(["-i".to_string(), identity_file.clone()]);
    }

    if ctx.client.port != deploy_defaults.default_ssh_port {
        scp_args.extend(["-P".to_string(), ctx.client.port.to_string()]);
    }

    // Remote source (reverse of upload)
    scp_args.push(format!(
        "{}@{}:{}",
        ctx.client.user,
        ctx.client.host,
        shell::quote_path(&full_remote_path)
    ));
    scp_args.push(local_path.to_string());

    let label = if recursive { "directory" } else { "file" };
    log_status!(
        "download",
        "Downloading {}: {}@{}:{} -> {}",
        label,
        ctx.client.user,
        ctx.client.host,
        full_remote_path,
        local_path
    );

    let output = Command::new("scp").args(&scp_args).output();
    match output {
        Ok(output) if output.status.success() => Ok(DownloadResult {
            remote_path: full_remote_path,
            local_path: local_path.to_string(),
            recursive,
            success: true,
            exit_code: 0,
            error: None,
        }),
        Ok(output) => {
            let exit_code = output.status.code().unwrap_or(1);
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            Ok(DownloadResult {
                remote_path: full_remote_path,
                local_path: local_path.to_string(),
                recursive,
                success: false,
                exit_code,
                error: Some(stderr),
            })
        }
        Err(err) => Ok(DownloadResult {
            remote_path: full_remote_path,
            local_path: local_path.to_string(),
            recursive,
            success: false,
            exit_code: 1,
            error: Some(err.to_string()),
        }),
    }
}
