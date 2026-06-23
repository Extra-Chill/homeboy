//! Lab offload argument rewriting: resolve the offload source path and strip /
//! rewrite controller-only flags so a command can run on the remote runner.

use std::path::PathBuf;

use crate::core::worktree;
use crate::core::{Error, Result};

use super::path_remap::{remap_local_path, LabPathRemap};
use super::EXPLICIT_PASSTHROUGH_SENTINEL;

pub(in crate::core::runner) fn lab_offload_source_path(args: &[String]) -> Result<PathBuf> {
    let mut iter = args.iter().skip(1).peekable();
    while let Some(arg) = iter.next() {
        if arg == "--" {
            break;
        }
        if arg == "--path" || arg == "--cwd" {
            let value = iter.next().ok_or_else(|| {
                let field = arg.trim_start_matches("--");
                Error::validation_invalid_argument(
                    field,
                    format!("{arg} requires a value before Lab offload can sync the workspace"),
                    None,
                    None,
                )
            })?;
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if arg == "--to-worktree" {
            let value = iter.next().ok_or_else(|| {
                Error::validation_invalid_argument(
                    "to_worktree",
                    "--to-worktree requires a value before Lab offload can sync the target worktree",
                    None,
                    None,
                )
            })?;
            return worktree::resolve(value).map(|record| PathBuf::from(record.worktree_path));
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--cwd=") {
            return Ok(PathBuf::from(shellexpand::tilde(value).to_string()));
        }
        if let Some(value) = arg.strip_prefix("--to-worktree=") {
            return worktree::resolve(value).map(|record| PathBuf::from(record.worktree_path));
        }
    }

    std::env::current_dir()
        .map_err(|err| Error::internal_io(err.to_string(), Some("read cwd".to_string())))
}

pub(in crate::core::runner) fn rewrite_lab_offload_args(
    args: &[String],
    remote_path: &str,
    mappings: &[LabPathRemap],
) -> Vec<String> {
    let mut ordered: Vec<&LabPathRemap> = mappings.iter().collect();
    ordered.sort_by_key(|mapping| {
        (
            std::cmp::Reverse(mapping.local.len()),
            std::cmp::Reverse(mapping.remote.len()),
        )
    });
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    let has_force_hot = args.iter().any(|arg| arg == "--force-hot");
    while let Some(arg) = iter.next() {
        if arg == EXPLICIT_PASSTHROUGH_SENTINEL {
            continue;
        }
        if passthrough {
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--path" || arg == "--cwd" {
            stripped.push(arg.clone());
            let value = iter.next();
            stripped.push(
                value
                    .and_then(|value| remap_local_path(value, &ordered))
                    .unwrap_or_else(|| remote_path.to_string()),
            );
            continue;
        }
        if let Some(value) = arg.strip_prefix("--path=") {
            let rewritten =
                remap_local_path(value, &ordered).unwrap_or_else(|| remote_path.to_string());
            stripped.push(format!("--path={rewritten}"));
            continue;
        }
        if let Some(value) = arg.strip_prefix("--cwd=") {
            let rewritten =
                remap_local_path(value, &ordered).unwrap_or_else(|| remote_path.to_string());
            stripped.push(format!("--cwd={rewritten}"));
            continue;
        }
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        if arg == "--lab-only" || arg == "--no-local-execution" {
            continue;
        }
        if arg == "--output" || arg == "--artifact-root" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--output=") || arg.starts_with("--artifact-root=") {
            continue;
        }
        stripped.push(remap_lab_offload_arg(arg, &ordered));
    }
    if !has_force_hot {
        stripped.insert(1, "--force-hot".to_string());
    }
    stripped
}

fn remap_lab_offload_arg(arg: &str, mappings: &[&LabPathRemap]) -> String {
    if let Some(raw_path) = arg.strip_prefix('@') {
        return remap_local_path(raw_path, mappings)
            .map(|remapped| format!("@{remapped}"))
            .unwrap_or_else(|| arg.to_string());
    }

    if let Some((prefix, value)) = arg.split_once('=') {
        if let Some(raw_path) = value.strip_prefix('@') {
            return remap_local_path(raw_path, mappings)
                .map(|remapped| format!("{prefix}=@{remapped}"))
                .unwrap_or_else(|| arg.to_string());
        }
        return remap_local_path(value, mappings)
            .map(|remapped| format!("{prefix}={remapped}"))
            .unwrap_or_else(|| arg.to_string());
    }

    remap_local_path(arg, mappings).unwrap_or_else(|| arg.to_string())
}

pub(in crate::core::runner) fn rewrite_runner_resident_lab_offload_args(
    args: &[String],
) -> Vec<String> {
    // A runner-side `tunnel service expose` should not require a separate
    // server declaration for the selected runner: in that context the runner
    // itself is the server (#4606). Drop the controller-side `--server` value
    // and mark the runner-side declaration as runner-local instead.
    let is_service_expose = is_tunnel_service_command(args, "expose");
    let already_runner_local = args.iter().any(|arg| arg == "--runner-local");
    let mut stripped = Vec::with_capacity(args.len());
    let mut iter = args.iter().peekable();
    let mut passthrough = false;
    let has_force_hot = args.iter().any(|arg| arg == "--force-hot");
    while let Some(arg) = iter.next() {
        if arg == EXPLICIT_PASSTHROUGH_SENTINEL {
            continue;
        }
        if passthrough {
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--" {
            passthrough = true;
            stripped.push(arg.clone());
            continue;
        }
        if arg == "--runner" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--runner=") {
            continue;
        }
        if arg == "--lab-only" || arg == "--no-local-execution" {
            continue;
        }
        if arg == "--output" || arg == "--artifact-root" {
            let _ = iter.next();
            continue;
        }
        if arg.starts_with("--output=") || arg.starts_with("--artifact-root=") {
            continue;
        }
        if is_service_expose && arg == "--server" {
            let _ = iter.next();
            continue;
        }
        if is_service_expose && arg.starts_with("--server=") {
            continue;
        }
        stripped.push(arg.clone());
    }
    if is_service_expose && !already_runner_local {
        stripped.push("--runner-local".to_string());
    }
    if !has_force_hot {
        stripped.insert(1, "--force-hot".to_string());
    }
    stripped
}

/// Returns true when `args` invoke `tunnel service <subcommand>`.
fn is_tunnel_service_command(args: &[String], subcommand: &str) -> bool {
    args.windows(3).any(|window| {
        matches!(
            window,
            [first, second, third]
                if first == "tunnel" && second == "service" && third == subcommand
        )
    })
}
