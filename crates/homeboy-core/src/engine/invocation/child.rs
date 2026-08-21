use crate::engine::resource::ChildProcessIdentity;
use crate::error::{Error, Result};
use crate::paths;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use super::{json_excerpt, parse_context};

const CHILD_RECORD_DIR: &str = "invocation-children";
const CHILD_CLEANUP_GRACE: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub struct InvocationChildGuard {
    pub(crate) path: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InvocationChildRecord {
    pub invocation_id: String,
    pub owner_pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_started_at: Option<String>,
    #[serde(flatten)]
    pub child: ChildProcessIdentity,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_started_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pgid: Option<i32>,
    pub started_at: String,
}

impl Drop for InvocationChildGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

pub fn register_child_process(
    invocation_id: &str,
    root_pid: u32,
    pgid: Option<i32>,
    command_label: String,
) -> Result<InvocationChildGuard> {
    register_child_process_in_root(
        &paths::homeboy()?,
        invocation_id,
        root_pid,
        pgid,
        command_label,
    )
}

/// [`register_child_process`] against an already-resolved config root.
///
/// The guard retains the *resolved* record path, so release is already
/// root-stable: `Drop` removes the file this call created rather than
/// re-deriving one.
pub fn register_child_process_in_root(
    config_root: &Path,
    invocation_id: &str,
    root_pid: u32,
    pgid: Option<i32>,
    command_label: String,
) -> Result<InvocationChildGuard> {
    let dir = InvocationChildRecord::children_dir_in_root(config_root, invocation_id);
    fs::create_dir_all(&dir).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to create invocation child directory {}: {}",
                dir.display(),
                e
            ),
            Some("invocation.child.dir".to_string()),
        )
    })?;

    let record = InvocationChildRecord {
        invocation_id: invocation_id.to_string(),
        owner_pid: std::process::id(),
        owner_started_at: InvocationChildRecord::process_started_at(std::process::id()),
        child: ChildProcessIdentity {
            root_pid,
            command_label,
        },
        root_started_at: InvocationChildRecord::process_started_at(root_pid),
        pgid,
        started_at: chrono::Utc::now().to_rfc3339(),
    };
    let path = InvocationChildRecord::record_path_in_root(config_root, invocation_id, root_pid);
    let json = serde_json::to_string_pretty(&record).map_err(|e| {
        Error::internal_unexpected(format!(
            "Failed to serialize invocation child record: {}",
            e
        ))
    })?;
    fs::write(&path, json).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to write invocation child record {}: {}",
                path.display(),
                e
            ),
            Some("invocation.child.write".to_string()),
        )
    })?;

    Ok(InvocationChildGuard { path })
}

pub fn cleanup_invocation_children_in_root(
    config_root: &Path,
    invocation_id: &str,
) -> Result<usize> {
    let mut cleaned = 0;
    for path in InvocationChildRecord::files_for_invocation_in_root(config_root, invocation_id)? {
        let Some(record) = InvocationChildRecord::decode(&path)? else {
            continue;
        };
        if record.cleanup() {
            cleaned += 1;
        }
        let _ = fs::remove_file(path);
    }
    Ok(cleaned)
}

pub fn cleanup_stale_child_records_in_root(config_root: &Path) -> Result<usize> {
    let mut cleaned = 0;
    let root = InvocationChildRecord::root_in_root(config_root);
    if !root.exists() {
        return Ok(0);
    }

    for entry in fs::read_dir(&root).map_err(|e| {
        Error::internal_io(
            format!(
                "Failed to read invocation child root {}: {}",
                root.display(),
                e
            ),
            Some("invocation.child.read".to_string()),
        )
    })? {
        let entry = entry.map_err(|e| {
            Error::internal_io(e.to_string(), Some("invocation.child.entry".to_string()))
        })?;
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        for child_path in InvocationChildRecord::files_in_dir(&path)? {
            let Some(record) = InvocationChildRecord::decode(&child_path)? else {
                let _ = fs::remove_file(child_path);
                continue;
            };
            if record.owner_is_gone() {
                if record.cleanup() {
                    cleaned += 1;
                }
                let _ = fs::remove_file(child_path);
            }
        }
    }
    Ok(cleaned)
}

impl InvocationChildRecord {
    /// Child-record root below an already-resolved config root.
    fn root_in_root(config_root: &Path) -> PathBuf {
        config_root.join(CHILD_RECORD_DIR)
    }

    /// Per-invocation child-record directory below an already-resolved config
    /// root.
    ///
    /// There is deliberately no ambient sibling: every caller either already
    /// holds the root it registered under, or is the ambient wrapper that
    /// resolved it once for the whole operation.
    pub(crate) fn children_dir_in_root(config_root: &Path, invocation_id: &str) -> PathBuf {
        Self::root_in_root(config_root).join(paths::sanitize_path_segment(invocation_id))
    }

    /// One child record below an already-resolved config root.
    pub(crate) fn record_path_in_root(
        config_root: &Path,
        invocation_id: &str,
        root_pid: u32,
    ) -> PathBuf {
        Self::children_dir_in_root(config_root, invocation_id).join(format!("{}.json", root_pid))
    }

    fn files_for_invocation_in_root(
        config_root: &Path,
        invocation_id: &str,
    ) -> Result<Vec<PathBuf>> {
        Self::files_in_dir(&Self::children_dir_in_root(config_root, invocation_id))
    }

    fn files_in_dir(dir: &Path) -> Result<Vec<PathBuf>> {
        if !dir.exists() {
            return Ok(Vec::new());
        }
        let mut files = Vec::new();
        for entry in fs::read_dir(dir).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to read invocation child directory {}: {}",
                    dir.display(),
                    e
                ),
                Some("invocation.child.read".to_string()),
            )
        })? {
            let entry = entry.map_err(|e| {
                Error::internal_io(e.to_string(), Some("invocation.child.entry".to_string()))
            })?;
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "json") {
                files.push(path);
            }
        }
        files.sort();
        Ok(files)
    }

    fn decode(path: &Path) -> Result<Option<Self>> {
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(path).map_err(|e| {
            Error::internal_io(
                format!(
                    "Failed to read invocation child record {}: {}",
                    path.display(),
                    e
                ),
                Some("invocation.child.read".to_string()),
            )
        })?;
        if content.trim().is_empty() {
            return Ok(None);
        }
        serde_json::from_str::<Self>(&content)
            .map(Some)
            .map_err(|e| {
                Error::validation_invalid_json(
                    e,
                    Some(parse_context(path)),
                    Some(json_excerpt(&content)),
                )
            })
    }

    fn owner_is_gone(&self) -> bool {
        !Self::process_identity_matches(self.owner_pid, self.owner_started_at.as_deref())
    }

    fn cleanup(&self) -> bool {
        if !Self::process_identity_matches(self.child.root_pid, self.root_started_at.as_deref()) {
            return false;
        }

        #[cfg(unix)]
        if let Some(pgid) = self.pgid {
            if pgid <= 0 || pgid as u32 != self.child.root_pid {
                return false;
            }
            Self::cleanup_process_group(pgid as libc::pid_t);
            return true;
        }

        false
    }

    fn process_identity_matches(pid: u32, started_at: Option<&str>) -> bool {
        if !crate::process::pid_is_running(pid) {
            return false;
        }
        match (started_at, Self::process_started_at(pid)) {
            (Some(expected), Some(actual)) => expected == actual,
            (Some(_), None) => false,
            (None, _) => true,
        }
    }

    pub(crate) fn process_started_at(pid: u32) -> Option<String> {
        let output = std::process::Command::new("ps")
            .args(["-o", "lstart=", "-p", &pid.to_string()])
            .output()
            .ok()?;
        if !output.status.success() {
            return None;
        }
        let started = String::from_utf8_lossy(&output.stdout).trim().to_string();
        (!started.is_empty()).then_some(started)
    }

    #[cfg(unix)]
    fn cleanup_process_group(pgid: libc::pid_t) {
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
        std::thread::sleep(CHILD_CLEANUP_GRACE);
        if crate::process::process_group_is_running(pgid) {
            unsafe {
                libc::kill(-pgid, libc::SIGKILL);
            }
        }
    }
}

#[cfg(test)]
mod audit_coverage_tests {
    use super::*;
    use crate::test_support::with_isolated_home;

    /// The config root of the isolated home each test below installs.
    ///
    /// The ambient `cleanup_invocation_children` / `cleanup_stale_child_records`
    /// wrappers existed only to serve these two assertions. The test is its own
    /// boundary, so it resolves here and calls the rooted form directly (#7505).
    fn test_config_root() -> std::path::PathBuf {
        paths::homeboy().expect("config root")
    }

    #[test]
    fn test_register_child_process() {
        with_isolated_home(|_| {
            let guard = register_child_process(
                "inv-audit",
                std::process::id(),
                None,
                "audit-child".to_string(),
            )
            .expect("child record");
            assert!(guard.path.exists());
        });
    }

    #[test]
    fn test_children_dir() {
        with_isolated_home(|_| {
            let config_root = crate::paths::homeboy().expect("config root");
            let dir = InvocationChildRecord::children_dir_in_root(&config_root, "inv/../audit");

            assert!(dir.ends_with("inv____audit"));
        });
    }

    #[test]
    fn test_record_path() {
        with_isolated_home(|_| {
            let config_root = crate::paths::homeboy().expect("config root");
            let dir = InvocationChildRecord::children_dir_in_root(&config_root, "inv/../audit");
            let path =
                InvocationChildRecord::record_path_in_root(&config_root, "inv/../audit", 1234);

            assert_eq!(
                path.file_name().and_then(|name| name.to_str()),
                Some("1234.json")
            );
            assert!(path.starts_with(&dir));
        });
    }

    #[test]
    fn test_process_started_at_handles_current_and_missing_processes() {
        assert!(InvocationChildRecord::process_started_at(std::process::id()).is_some());
        assert!(InvocationChildRecord::process_started_at(u32::MAX).is_none());
    }

    #[test]
    fn test_cleanup_invocation_children() {
        with_isolated_home(|_| {
            assert_eq!(
                cleanup_invocation_children_in_root(&test_config_root(), "inv-empty").unwrap(),
                0
            );
        });
    }

    #[test]
    fn test_cleanup_stale_child_records() {
        with_isolated_home(|_| {
            assert_eq!(
                cleanup_stale_child_records_in_root(&test_config_root()).unwrap(),
                0
            );
        });
    }
}
