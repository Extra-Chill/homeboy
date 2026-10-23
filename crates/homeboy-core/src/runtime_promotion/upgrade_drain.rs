//! Cross-process upgrade admission window. Unlike daemon stop, this survives
//! daemon replacement and continues to permit work belonging to draining Cooks.
use super::*;

#[derive(Debug, Serialize, Deserialize)]
struct DrainRecord {
    operation_id: String,
    pid: u32,
    identity: Option<crate::process::ProcessStartIdentity>,
    #[serde(default)]
    expires_at_ms: Option<u64>,
    runs: Vec<String>,
    runners: Vec<String>,
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
pub fn begin_upgrade_drain(
    operation_id: &str,
    runs: Vec<String>,
    runners: Vec<String>,
) -> Result<UpgradeDrainGuard> {
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
        expires_at_ms: None,
        runs,
        runners,
    };
    crate::engine::local_files::write_json_file(&path, &record)?;
    Ok(UpgradeDrainGuard { path, _lock: lock })
}

/// Authenticated daemon control endpoint: the operation token owns release.
/// A deadline bounds a lost controller's remote admission reservation.
pub fn remote_upgrade_drain(request: &serde_json::Value) -> Result<()> {
    let root = paths::runtime_promotion_dir()?;
    fs::create_dir_all(&root).map_err(io("create remote drain directory"))?;
    let token = request["operation_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| Error::internal_unexpected("drain operation identity missing"))?;
    if let Some(current) = active_drain()? {
        if current.operation_id != token {
            return Err(Error::validation_invalid_argument(
                "upgrade_drain",
                "another operation owns runner drain",
                None,
                Some(vec![format!(
                    "homeboy upgrade status {}",
                    current.operation_id
                )]),
            ));
        }
        // A shared runner can live on the controller host. Its remote control
        // must not replace the process-owned fence with an expiring record.
        if current.expires_at_ms.is_none() {
            return Ok(());
        }
    }
    let path = root.join("upgrade-drain.json");
    if request["resume"] == true {
        match fs::remove_file(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(io("resume runner drain")(error)),
        }
    }
    let timeout_ms = request["timeout_ms"]
        .as_u64()
        .unwrap_or(1_800_000)
        .min(86_400_000);
    let record = DrainRecord {
        operation_id: token.to_string(),
        pid: 0,
        identity: None,
        expires_at_ms: Some(crate::api_jobs::timestamp_ms().saturating_add(timeout_ms)),
        runs: serde_json::from_value(request["runs"].clone())
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
        runners: Vec::new(),
    };
    crate::engine::local_files::write_json_file(&path, &record)
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
    if record
        .expires_at_ms
        .is_some_and(|deadline| crate::api_jobs::timestamp_ms() >= deadline)
    {
        return Ok(None);
    }
    if record.expires_at_ms.is_none()
        && matches!(
            crate::process::process_identity_state_with_start_identity(
                record.pid,
                None,
                record.identity.as_ref()
            ),
            crate::process::ProcessIdentityState::Dead
                | crate::process::ProcessIdentityState::IdentityMismatch
        )
    {
        return Ok(None);
    }
    Ok(Some(record))
}

/// The upgrade worker itself may execute its bounded runner installation
/// commands; unrelated clients never inherit this permission.
pub fn ensure_runner_upgrade_drain_admission(run_id: Option<&str>, runner: &str) -> Result<()> {
    if active_drain()?.is_some_and(|record| record.pid == std::process::id()) {
        return Ok(());
    }
    ensure_upgrade_drain_admission(run_id, Some(runner))
}

pub fn upgrade_drain_operation() -> Result<Option<String>> {
    Ok(active_drain()?
        .filter(|record| record.pid == std::process::id())
        .map(|record| record.operation_id))
}

pub fn ensure_upgrade_drain_request(request: &serde_json::Value) -> Result<()> {
    if let Some(record) = active_drain()? {
        if request["upgrade_operation_id"].as_str() == Some(record.operation_id.as_str()) {
            return Ok(());
        }
    }
    ensure_upgrade_drain_admission(request["durable_run_id"].as_str(), None)
}

pub fn runner_is_draining(runner: &str) -> Result<bool> {
    Ok(active_drain()?.is_some_and(|record| record.runners.iter().any(|id| id == runner)))
}

pub fn ensure_upgrade_drain_admission(run_id: Option<&str>, runner: Option<&str>) -> Result<()> {
    let Some(record) = active_drain()? else {
        return Ok(());
    };
    if runner.is_some_and(|runner| !record.runners.iter().any(|id| id == runner))
        || run_id.is_some_and(|run| record.runs.iter().any(|id| id == run))
    {
        return Ok(());
    }
    let kind = if runner.is_some() {
        "runner_draining"
    } else {
        "controller_draining"
    };
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn runtime_promotion_competing_upgrade_hint_is_read_only_and_executable() {
        crate::test_support::with_isolated_home(|_| {
            let guard = begin_upgrade_drain("upgrade-owner", vec![], vec![]).unwrap();
            let error = match begin_upgrade_drain("upgrade-contender", vec![], vec![]) {
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
            assert!(begin_upgrade_drain("upgrade-contender", vec![], vec![]).is_ok());
        });
    }

    #[test]
    fn runtime_promotion_upgrade_drain_survives_clients_and_preserves_existing_cooks() {
        crate::test_support::with_isolated_home(|_| {
            let guard =
                begin_upgrade_drain("upgrade-1", vec!["live-cook".into()], vec!["lab".into()])
                    .unwrap();
            assert!(runner_is_draining("lab").unwrap());
            assert!(ensure_upgrade_drain_admission(Some("live-cook"), None).is_ok());
            let error = ensure_upgrade_drain_admission(Some("new-cook"), None).unwrap_err();
            assert_eq!(error.details["kind"], "controller_draining");
            let hints = serde_json::to_string(&error.hints).unwrap();
            assert!(hints.contains("homeboy upgrade status upgrade-1"));
            assert!(!hints.contains("--force"));
            remote_upgrade_drain(
                &serde_json::json!({"operation_id":"upgrade-1", "runs":[], "resume":false}),
            )
            .unwrap();
            assert_eq!(
                upgrade_drain_operation().unwrap().as_deref(),
                Some("upgrade-1")
            );
            assert!(ensure_upgrade_drain_admission(None, Some("lab")).is_err());
            assert!(ensure_upgrade_drain_admission(None, Some("other")).is_ok());
            drop(guard);
            assert!(ensure_upgrade_drain_admission(None, None).is_ok());
        });
    }
}
