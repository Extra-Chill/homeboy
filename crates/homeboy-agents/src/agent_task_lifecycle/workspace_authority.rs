use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::*;

const WORKSPACE_TERMINAL_AUTHORITY_SCHEMA: &str = "homeboy/workspace-terminal-authority/v1";
const WORKSPACE_TERMINAL_AUTHORITY_RELEASE_SCHEMA: &str =
    "homeboy/workspace-terminal-authority-release/v1";

/// Terminal authority retained independently from compactable run records until
/// the runner workspace that it protects has been durably removed.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkspaceTerminalAuthorityReceipt {
    pub schema: String,
    pub run_id: String,
    pub runner_id: String,
    pub runner_job_id: String,
    pub remote_workspace: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct WorkspaceTerminalAuthorityRelease {
    schema: String,
    run_id: String,
    runner_id: String,
    remote_workspace: String,
    state: String,
}

fn authority_digest(run_id: &str, runner_id: &str, remote_workspace: &str) -> String {
    let mut digest = Sha256::new();
    digest.update(run_id.as_bytes());
    digest.update([0]);
    digest.update(runner_id.as_bytes());
    digest.update([0]);
    digest.update(remote_workspace.as_bytes());
    format!("{:x}", digest.finalize())
}

fn receipt_path(run_id: &str, runner_id: &str, remote_workspace: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("workspace-terminal-authority")
        .join(format!(
            "{}.json",
            authority_digest(run_id, runner_id, remote_workspace)
        )))
}

fn release_path(run_id: &str, runner_id: &str, remote_workspace: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("workspace-terminal-authority")
        .join(format!(
            "{}.release.json",
            authority_digest(run_id, runner_id, remote_workspace)
        )))
}

pub(crate) fn persist_terminal_from_record(record: &AgentTaskRunRecord) -> Result<()> {
    if !record.state.is_terminal() {
        return Ok(());
    }
    let Some(identity) = accepted_lab_runner_job_identity_from_record(record) else {
        return Ok(());
    };
    let Some(remote_workspace) = record
        .metadata
        .get("remote_workspace")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    else {
        return Ok(());
    };
    let receipt = WorkspaceTerminalAuthorityReceipt {
        schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
        run_id: record.run_id.clone(),
        runner_id: identity.runner_id,
        runner_job_id: identity.runner_job_id,
        remote_workspace: remote_workspace.to_string(),
    };
    persist_workspace_terminal_authority(receipt)
}

fn persist_workspace_terminal_authority(receipt: WorkspaceTerminalAuthorityReceipt) -> Result<()> {
    let path = receipt_path(
        &receipt.run_id,
        &receipt.runner_id,
        &receipt.remote_workspace,
    )?;
    let release_path = release_path(
        &receipt.run_id,
        &receipt.runner_id,
        &receipt.remote_workspace,
    )?;
    homeboy_core::config::with_config_lock(|| {
        if release_path.exists() {
            read_release(
                &release_path,
                &receipt.run_id,
                &receipt.runner_id,
                &receipt.remote_workspace,
            )?;
            return Ok(());
        }
        if path.exists() {
            let existing: WorkspaceTerminalAuthorityReceipt =
                serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(path.display().to_string()))
                })?)
                .map_err(|error| {
                    Error::internal_json(error.to_string(), Some(path.display().to_string()))
                })?;
            if existing == receipt {
                return Ok(());
            }
            return Err(Error::validation_invalid_argument(
                "workspace_terminal_authority",
                "workspace terminal authority receipt conflicts with existing immutable receipt",
                Some(path.display().to_string()),
                None,
            ));
        }
        homeboy_core::engine::local_files::write_json_file_owner_only(&path, &receipt)
    })
}

#[cfg(any(test, feature = "test-support"))]
pub fn persist_workspace_terminal_authority_for_test(
    run_id: &str,
    runner_id: &str,
    runner_job_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    persist_workspace_terminal_authority(WorkspaceTerminalAuthorityReceipt {
        schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
        run_id: run_id.to_string(),
        runner_id: runner_id.to_string(),
        runner_job_id: runner_job_id.to_string(),
        remote_workspace: remote_workspace.to_string(),
    })
}

pub fn resolve_workspace_terminal_authority(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    job_id: Option<&str>,
) -> Result<Option<WorkspaceTerminalAuthorityReceipt>> {
    let path = receipt_path(run_id, runner_id, remote_workspace)?;
    let release_path = release_path(run_id, runner_id, remote_workspace)?;
    if release_path.exists() {
        let release = read_release(&release_path, run_id, runner_id, remote_workspace)?;
        return match release.state.as_str() {
            "pending" | "released" => Err(Error::validation_invalid_argument(
                "workspace_terminal_authority",
                format!("workspace terminal authority release is {}", release.state),
                Some(release_path.display().to_string()),
                None,
            )),
            _ => Err(invalid_release_error(&release_path)),
        };
    }
    if !path.exists() {
        return Ok(None);
    }
    let receipt: WorkspaceTerminalAuthorityReceipt =
        serde_json::from_slice(&std::fs::read(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?)
        .map_err(|error| {
            Error::internal_json(error.to_string(), Some(path.display().to_string()))
        })?;
    let valid = receipt.schema == WORKSPACE_TERMINAL_AUTHORITY_SCHEMA
        && receipt.run_id == run_id
        && receipt.runner_id == runner_id
        && receipt.remote_workspace == remote_workspace
        && !receipt.runner_job_id.is_empty()
        && job_id.is_none_or(|job_id| job_id == receipt.runner_job_id);
    valid.then_some(receipt).map(Some).ok_or_else(|| {
        Error::validation_invalid_argument(
            "workspace_terminal_authority",
            "workspace terminal authority receipt is malformed or contradicts workspace ownership",
            Some(path.display().to_string()),
            None,
        )
    })
}

pub fn workspace_terminal_authority_release_is_pending(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<bool> {
    let path = release_path(run_id, runner_id, remote_workspace)?;
    if !path.exists() {
        return Ok(false);
    }
    Ok(read_release(&path, run_id, runner_id, remote_workspace)?.state == "pending")
}

pub fn begin_workspace_terminal_authority_release(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    let path = release_path(run_id, runner_id, remote_workspace)?;
    homeboy_core::config::with_config_lock(|| {
        if path.exists() {
            let release = read_release(&path, run_id, runner_id, remote_workspace)?;
            return match release.state.as_str() {
                "pending" => Ok(()),
                "released" => Err(Error::validation_invalid_argument(
                    "workspace_terminal_authority",
                    "workspace terminal authority was already released",
                    Some(path.display().to_string()),
                    None,
                )),
                _ => Err(invalid_release_error(&path)),
            };
        }
        write_release(&path, run_id, runner_id, remote_workspace, "pending")
    })
}

pub fn abort_workspace_terminal_authority_release(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    let path = release_path(run_id, runner_id, remote_workspace)?;
    homeboy_core::config::with_config_lock(|| {
        if !path.exists() {
            return Ok(());
        }
        let release = read_release(&path, run_id, runner_id, remote_workspace)?;
        if release.state != "pending" {
            return Err(invalid_release_error(&path));
        }
        std::fs::remove_file(&path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })
    })
}

pub fn remove_workspace_terminal_authority(
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<()> {
    let path = receipt_path(run_id, runner_id, remote_workspace)?;
    let release_path = release_path(run_id, runner_id, remote_workspace)?;
    homeboy_core::config::with_config_lock(|| {
        write_release(
            &release_path,
            run_id,
            runner_id,
            remote_workspace,
            "released",
        )?;
        if path.exists() {
            std::fs::remove_file(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
        }
        Ok(())
    })
}

fn write_release(
    path: &std::path::Path,
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
    state: &str,
) -> Result<()> {
    homeboy_core::engine::local_files::write_json_file_owner_only(
        path,
        &WorkspaceTerminalAuthorityRelease {
            schema: WORKSPACE_TERMINAL_AUTHORITY_RELEASE_SCHEMA.to_string(),
            run_id: run_id.to_string(),
            runner_id: runner_id.to_string(),
            remote_workspace: remote_workspace.to_string(),
            state: state.to_string(),
        },
    )
}

fn read_release(
    path: &std::path::Path,
    run_id: &str,
    runner_id: &str,
    remote_workspace: &str,
) -> Result<WorkspaceTerminalAuthorityRelease> {
    let release: WorkspaceTerminalAuthorityRelease =
        serde_json::from_slice(&std::fs::read(path).map_err(|error| {
            Error::internal_io(error.to_string(), Some(path.display().to_string()))
        })?)
        .map_err(|error| {
            Error::internal_json(error.to_string(), Some(path.display().to_string()))
        })?;
    (release.schema == WORKSPACE_TERMINAL_AUTHORITY_RELEASE_SCHEMA
        && release.run_id == run_id
        && release.runner_id == runner_id
        && release.remote_workspace == remote_workspace
        && matches!(release.state.as_str(), "pending" | "released"))
    .then_some(release)
    .ok_or_else(|| invalid_release_error(path))
}

fn invalid_release_error(path: &std::path::Path) -> Error {
    Error::validation_invalid_argument(
        "workspace_terminal_authority",
        "workspace terminal authority release marker is malformed or contradicts workspace ownership",
        Some(path.display().to_string()),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn receipt_survives_run_record_compaction_and_requires_exact_binding() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let receipt = WorkspaceTerminalAuthorityReceipt {
                schema: WORKSPACE_TERMINAL_AUTHORITY_SCHEMA.to_string(),
                run_id: "run-1".to_string(),
                runner_id: "reverse-or-direct".to_string(),
                runner_job_id: "job-1".to_string(),
                remote_workspace: "/runner/_lab_workspaces/workspace-1".to_string(),
            };
            persist_workspace_terminal_authority(receipt.clone()).expect("persist receipt");
            persist_workspace_terminal_authority(receipt)
                .expect("terminal progression is idempotent");

            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .expect("resolve after lifecycle compaction")
            .is_some());
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("other-job")
            )
            .is_err());
            begin_workspace_terminal_authority_release(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("begin workspace cleanup");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .is_err());
            assert!(workspace_terminal_authority_release_is_pending(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("pending cleanup authority"));
            abort_workspace_terminal_authority_release(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("abort failed workspace cleanup");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .expect("authority restored after cleanup abort")
            .is_some());
            remove_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("remove after workspace cleanup");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .is_err());
            persist_workspace_terminal_authority_for_test(
                "run-1",
                "reverse-or-direct",
                "job-1",
                "/runner/_lab_workspaces/workspace-1",
            )
            .expect("late terminal projection is suppressed");
            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "reverse-or-direct",
                "/runner/_lab_workspaces/workspace-1",
                Some("job-1")
            )
            .is_err());
        });
    }

    #[test]
    fn malformed_or_mixed_version_receipts_fail_closed() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let path =
                receipt_path("run-1", "runner-1", "/runner/workspace").expect("receipt path");
            homeboy_core::engine::local_files::write_json_file_owner_only(
                &path,
                &serde_json::json!({
                    "schema": "homeboy/workspace-terminal-authority/v2",
                    "run_id": "run-1",
                    "runner_id": "runner-1",
                    "runner_job_id": "job-1",
                    "remote_workspace": "/runner/workspace"
                }),
            )
            .expect("persist unsupported receipt");

            assert!(resolve_workspace_terminal_authority(
                "run-1",
                "runner-1",
                "/runner/workspace",
                Some("job-1")
            )
            .is_err());
        });
    }
}
