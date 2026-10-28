//! Cross-process upgrade admission window. Unlike daemon stop, this survives
//! daemon replacement and continues to permit work belonging to draining Cooks.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
struct DrainRecord {
    operation_id: String,
    pid: u32,
    identity: Option<crate::process::ProcessStartIdentity>,
    runs: Vec<String>,
}

pub struct UpgradeDrainGuard {
    path: PathBuf,
    _lock: fs::File,
}

impl Drop for UpgradeDrainGuard {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Reserve admission before inspecting live work. The independent lock is not
/// the promotion lock: existing executions must still acquire promotion pins
/// and publish their completions while the upgrade waits for them.
pub fn begin_upgrade_drain(operation_id: &str, runs: Vec<String>) -> Result<UpgradeDrainGuard> {
    let root = paths::runtime_promotion_dir()?;
    fs::create_dir_all(&root).map_err(io("create upgrade drain directory"))?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join("upgrade-drain.lock"))
        .map_err(io("open upgrade drain lock"))?;
    if !FileExt::try_lock_exclusive(&lock).map_err(io("lock upgrade drain"))? {
        return Err(Error::validation_invalid_argument(
            "upgrade",
            "another upgrade is draining this controller",
            None,
            None,
        )
        .with_hint("Wait for the owning operation: homeboy upgrade status"));
    }
    let path = root.join("upgrade-drain.json");
    let record = DrainRecord {
        operation_id: operation_id.to_string(),
        pid: std::process::id(),
        identity: Some(
            crate::process::process_start_identity(std::process::id())
                .map_err(Error::internal_unexpected)?
                .ok_or_else(|| {
                    Error::internal_unexpected("upgrade process identity unavailable")
                })?,
        ),
        runs,
    };
    crate::engine::local_files::write_json_file(&path, &record)?;
    Ok(UpgradeDrainGuard { path, _lock: lock })
}

fn active_drain() -> Result<Option<DrainRecord>> {
    let path = paths::runtime_promotion_dir()?.join("upgrade-drain.json");
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(io("read upgrade drain")(error)),
    };
    let record: DrainRecord = serde_json::from_slice(&bytes)
        .map_err(|error| Error::internal_json(error.to_string(), None))?;
    if matches!(
        crate::process::process_identity_state_with_start_identity(
            record.pid,
            None,
            record.identity.as_ref()
        ),
        crate::process::ProcessIdentityState::Dead
            | crate::process::ProcessIdentityState::IdentityMismatch
    ) {
        return Ok(None);
    }
    Ok(Some(record))
}

pub fn upgrade_drain_operation() -> Result<Option<String>> {
    Ok(active_drain()?
        .filter(|record| record.pid == std::process::id())
        .map(|record| record.operation_id))
}

pub fn ensure_upgrade_drain_admission(run_id: Option<&str>) -> Result<()> {
    let Some(record) = active_drain()? else {
        return Ok(());
    };
    if run_id.is_some_and(|run| record.runs.iter().any(|id| id == run)) {
        return Ok(());
    }
    let kind = "controller_draining";
    let mut error = Error::validation_invalid_argument(
        "admission",
        kind,
        None,
        Some(vec![format!(
            "homeboy upgrade status {}",
            record.operation_id
        )]),
    );
    error.details["kind"] = serde_json::json!(kind);
    Err(error.with_hint(format!(
        "Wait for `homeboy upgrade status {}` to report completion",
        record.operation_id
    )))
}

/// Workflow identities for actionable drain diagnostics.
pub fn upgrade_blocking_workflows(
    store: &crate::api_jobs::JobStore,
    jobs: &[crate::api_jobs::Job],
) -> Vec<String> {
    store
        .controller_jobs_of_type(crate::daemon::workflow_job::WORKFLOW_JOB_TYPE)
        .into_iter()
        .filter(|(job, _)| jobs.iter().any(|live| live.id == job.id))
        .filter_map(|(_, state)| state.request["instance"].as_str().map(str::to_string))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_promotion_competing_upgrade_hint_is_read_only_and_executable() {
        crate::test_support::with_isolated_home(|_| {
            let guard = begin_upgrade_drain("upgrade-owner", vec![]).unwrap();
            let error = match begin_upgrade_drain("upgrade-contender", vec![]) {
                Ok(_) => panic!("a second upgrade must not own admission"),
                Err(error) => error,
            };
            let hints = serde_json::to_string(&error.hints).unwrap();
            assert!(hints.contains("homeboy upgrade status"), "{hints}");
            assert!(!hints.contains("--force"), "{hints}");
            // The advertised read-only command is permitted by the fence.
            assert!(crate::paths::runtime_promotion_dir()
                .unwrap()
                .join("upgrade-drain.json")
                .exists());
            assert_eq!(
                upgrade_drain_operation().unwrap().as_deref(),
                Some("upgrade-owner")
            );
            drop(guard);
            assert!(begin_upgrade_drain("upgrade-contender", vec![]).is_ok());
        });
    }

    #[test]
    fn runtime_promotion_upgrade_drain_survives_clients_and_preserves_existing_cooks() {
        crate::test_support::with_isolated_home(|_| {
            let guard = begin_upgrade_drain("upgrade-1", vec!["live-cook".into()]).unwrap();
            assert!(ensure_upgrade_drain_admission(Some("live-cook")).is_ok());
            let error = ensure_upgrade_drain_admission(Some("new-cook")).unwrap_err();
            assert_eq!(error.details["kind"], "controller_draining");
            let hints = serde_json::to_string(&error.hints).unwrap();
            assert!(hints.contains("homeboy upgrade status upgrade-1"));
            assert!(!hints.contains("--force"));
            drop(guard);
            assert!(ensure_upgrade_drain_admission(None).is_ok());
        });
    }
}
