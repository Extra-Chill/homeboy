//! Controller-independent upgrade worker: a queued binary upgrade must not
//! count as an active job on the daemon it is going to replace.

use std::process::{Command, Stdio};

use homeboy_core::error::{Error, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::helpers::{run_upgrade_with_operation, select_durable_release_tag};
use super::operation::UpgradeOperation;
use super::types::InstallMethod;

const REQUEST_SCHEMA: &str = "homeboy/detached-upgrade-request/v1";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DetachedUpgradeRequest {
    schema: String,
    force: bool,
    method: InstallMethod,
    release_tag: String,
    skip_extensions: bool,
    skip_runners: bool,
    skip_services: bool,
}

/// Queue the exact release before the caller can reach a pin-wait timeout.
/// The worker is independent of the daemon job ledger so that its own upgrade
/// can pass the daemon's active-work restart fence once foreign jobs drain.
pub fn start_detached_upgrade(
    force: bool,
    method: InstallMethod,
    pinned_version: Option<&str>,
    skip_services: bool,
) -> Result<Value> {
    let request = DetachedUpgradeRequest {
        schema: REQUEST_SCHEMA.to_string(),
        force,
        method,
        release_tag: select_durable_release_tag(method, pinned_version)?,
        skip_extensions: true,
        skip_runners: true,
        skip_services,
    };
    let mut operation = UpgradeOperation::start_durable("homeboy upgrade")?;
    operation.prepare_detached(
        serde_json::to_value(&request)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )?;
    let id = operation.id().expect("durable operation").to_string();
    let binary = std::env::current_exe().map_err(|error| {
        Error::internal_unexpected(format!("resolve current Homeboy executable: {error}"))
    })?;
    let mut command = Command::new(binary);
    command
        .args(["upgrade", "continue", &id])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    let child = command.spawn().map_err(|error| {
        Error::internal_unexpected(format!("start detached upgrade worker: {error}"))
    })?;
    operation.handoff_detached(child.id())?;
    Ok(json!({
        "schema": "homeboy/detached-upgrade-admission/v1",
        "status": "queued",
        "operation_id": id,
        "selected_release_tag": request.release_tag,
        "inspect_command": format!("homeboy upgrade status {id}"),
    }))
}

/// Run only from the worker whose PID and start identity own this operation.
/// A hard worker crash becomes an interrupted observation on status read; no
/// second process may claim the same operation and replay a binary mutation.
pub fn continue_detached_upgrade(id: &str) -> Result<()> {
    // The parent records the child identity immediately after spawn. A short
    // wait is needed only for that handoff, not for release pins to drain.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    let mut operation = loop {
        match UpgradeOperation::resume_detached(id)? {
            Some(operation) => break operation,
            None if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(25));
            }
            None => {
                return Err(Error::internal_unexpected(format!(
                    "detached upgrade worker handoff timed out: {id}"
                )))
            }
        }
    };
    let request: DetachedUpgradeRequest =
        serde_json::from_value(operation.detached_request().clone())
            .map_err(|error| Error::internal_json(error.to_string(), None))?;
    if request.schema != REQUEST_SCHEMA
        || !matches!(
            request.method,
            InstallMethod::Binary | InstallMethod::Secondary
        )
        || !request.skip_extensions
        || !request.skip_runners
    {
        return Err(Error::validation_invalid_argument(
            "operation_id",
            "invalid detached binary upgrade request",
            Some(id.to_string()),
            None,
        ));
    }
    operation.set_phase_durable("detached_worker_running")?;
    run_upgrade_with_operation(
        request.force,
        Some(request.method),
        true,
        true,
        request.skip_services,
        &[],
        None,
        Some(&request.release_tag),
        operation,
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use homeboy_core::observation::{ObservationStore, RunStatus};

    #[test]
    fn request_rejects_unrecognized_fields_and_is_exact_release_bound() {
        let mut request = serde_json::to_value(DetachedUpgradeRequest {
            schema: REQUEST_SCHEMA.to_string(),
            force: false,
            method: InstallMethod::Binary,
            release_tag: "v0.398.0".to_string(),
            skip_extensions: true,
            skip_runners: true,
            skip_services: false,
        })
        .expect("serialize request");
        assert_eq!(request["release_tag"], "v0.398.0");
        request["unexpected"] = json!(true);
        assert!(serde_json::from_value::<DetachedUpgradeRequest>(request).is_err());
    }

    #[test]
    fn parent_exit_after_handoff_keeps_one_running_observation_for_worker() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut parent = UpgradeOperation::start_durable("homeboy upgrade")?;
            let id = parent.id().expect("run id").to_string();
            parent.prepare_detached(json!({ "release_tag": "v0.398.0" }))?;
            parent.handoff_detached(std::process::id())?;
            drop(parent);

            let status = super::super::load_upgrade_operation_status(Some(&id))?;
            assert_eq!(status.status, RunStatus::Running.as_str());
            assert_eq!(status.owner_pid, Some(std::process::id()));
            let mut worker =
                UpgradeOperation::resume_detached(&id)?.expect("worker owns operation");
            assert_eq!(worker.detached_request()["release_tag"], "v0.398.0");
            worker.set_phase_durable("waiting_for_foreign_generation_pins")?;
            let run = ObservationStore::open_initialized()?
                .get_run(&id)?
                .expect("run");
            assert_eq!(
                run.metadata_json["phase"],
                "waiting_for_foreign_generation_pins"
            );
            Ok::<_, Error>(())
        })
        .expect("durable detached worker takes ownership");
    }

    #[test]
    fn reused_worker_pid_cannot_claim_or_keep_a_running_upgrade() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut parent = UpgradeOperation::start_durable("homeboy upgrade")?;
            let id = parent.id().expect("run id").to_string();
            parent.prepare_detached(json!({ "release_tag": "v0.398.0" }))?;
            parent.handoff_detached(std::process::id())?;
            drop(parent);
            let store = ObservationStore::open_initialized()?;
            let mut metadata = store.get_run(&id)?.expect("run").metadata_json;
            metadata["homeboy_run_owner"]["process_start_identity"] = json!({
                "platform": "macos", "start_seconds": 0, "start_microseconds": 0
            });
            store.update_running_run_metadata(&id, metadata)?;
            assert!(UpgradeOperation::resume_detached(&id).is_err());
            let status = super::super::load_upgrade_operation_status(Some(&id))?;
            assert_eq!(status.phase, "interrupted");
            assert!(status.failed);
            Ok::<_, Error>(())
        })
        .expect("reused worker identity fails closed");
    }
}
