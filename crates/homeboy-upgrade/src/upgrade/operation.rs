//! Durable upgrade operation identity on the existing observation run store.
//!
//! Binary promotion and optional post-install refresh are independent phases.
//! Persisting them before mutation lets a timed-out client inspect whether the
//! controller actually promoted, instead of guessing from a silent hang.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use homeboy_core::error::{Error, Result};
use homeboy_core::observation::{
    run_owner_pid, running_status_note, ActiveObservation, NewRunRecord, ObservationStore,
    RunListFilter, RunRecord, RunStatus,
};

use super::types::{UpgradeComponentStatus, UpgradeResult};

pub const UPGRADE_OPERATION_KIND: &str = "upgrade";
pub const UPGRADE_OPERATION_SCHEMA: &str = "homeboy/upgrade-operation/v1";
pub const UPGRADE_PROGRESS_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradeOperationStatus {
    pub command: String,
    pub operation_id: String,
    pub status: String,
    pub phase: String,
    pub elapsed_seconds: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller: Option<UpgradeComponentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extensions: Option<UpgradeComponentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runners: Option<UpgradeComponentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inspect_command: Option<String>,
}

pub struct UpgradeOperation {
    observation: Option<ActiveObservation>,
    metadata: Value,
    started: Instant,
    finished: bool,
    controller_promoted: bool,
}

impl UpgradeOperation {
    pub fn start(command: impl Into<String>) -> Self {
        let metadata = json!({
            "schema": UPGRADE_OPERATION_SCHEMA,
            "phase": "admitted",
            "elapsed_seconds": 0,
            "controller": component("pending", "controller mutation has not started"),
            "extensions": component("pending", "extension refresh has not started"),
            "runners": component("pending", "runner refresh has not started"),
            "homeboy_run_owner": { "pid": std::process::id() },
        });
        let observation = ActiveObservation::start_best_effort(
            NewRunRecord::builder(UPGRADE_OPERATION_KIND)
                .component_id("homeboy")
                .command(command.into())
                .current_homeboy_version()
                .metadata(metadata.clone())
                .build(),
        );
        let operation = Self {
            observation,
            metadata,
            started: Instant::now(),
            finished: false,
            controller_promoted: false,
        };
        if let Some(id) = operation.id() {
            emit_upgrade_phase(&format!("operation={id}"));
        }
        operation
    }

    pub fn id(&self) -> Option<&str> {
        self.observation.as_ref().map(ActiveObservation::run_id)
    }

    pub fn set_phase(&mut self, phase: &str) {
        emit_upgrade_phase(phase);
        self.metadata["phase"] = json!(phase);
        self.metadata["elapsed_seconds"] = json!(self.started.elapsed().as_secs());
        self.persist();
    }

    pub fn mark_controller_promoted(&mut self, summary: &str) {
        self.controller_promoted = true;
        self.metadata["controller"] = component("updated", summary);
        self.set_phase(
            "controller installation verified; continuing with optional post-install refresh",
        );
    }

    pub fn mark_controller(&mut self, status: &str, summary: &str) {
        if status == "updated" {
            self.controller_promoted = true;
        }
        self.metadata["controller"] = component(status, summary);
        self.persist();
    }

    pub fn mark_extensions(&mut self, status: &str, summary: &str) {
        self.metadata["extensions"] = component(status, summary);
        self.persist();
    }

    pub fn finish_completed(&mut self, result: &UpgradeResult) {
        if let Some(status) = &result.controller {
            self.metadata["controller"] = json!(status);
            if status.status == "updated" {
                self.controller_promoted = true;
            }
        }
        if let Some(status) = &result.extensions {
            self.metadata["extensions"] = json!(status);
        }
        if let Some(status) = &result.runners {
            self.metadata["runners"] = json!(status);
        }
        self.metadata["phase"] = json!("completed");
        self.metadata["elapsed_seconds"] = json!(self.started.elapsed().as_secs());
        let status = if result
            .controller
            .as_ref()
            .is_some_and(|controller| controller.status == "failed")
        {
            RunStatus::Fail
        } else {
            RunStatus::Pass
        };
        self.finish(status);
    }

    fn persist(&mut self) {
        let Some(observation) = &self.observation else {
            return;
        };
        self.metadata["elapsed_seconds"] = json!(self.started.elapsed().as_secs());
        let _ = observation
            .store()
            .update_run_metadata(observation.run_id(), self.metadata.clone());
    }

    fn finish(&mut self, status: RunStatus) {
        if self.finished {
            return;
        }
        self.finished = true;
        let Some(observation) = &self.observation else {
            return;
        };
        if let Ok(Some(run)) = observation.store().get_run(observation.run_id()) {
            merge_component_fields(&mut self.metadata, &run.metadata_json);
            if let Some(elapsed) = run.metadata_json.get("elapsed_seconds") {
                self.metadata["elapsed_seconds"] = elapsed.clone();
            }
        }
        self.metadata["elapsed_seconds"] = json!(self.started.elapsed().as_secs());
        let _ = observation.store().finish_running_run(
            observation.run_id(),
            status,
            Some(self.metadata.clone()),
        );
    }
}

impl Drop for UpgradeOperation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        if self.controller_promoted {
            self.metadata["phase"] = json!("interrupted_after_controller");
            if self.metadata["extensions"]["status"] == "pending"
                || self.metadata["extensions"]["status"] == "running"
            {
                self.metadata["extensions"] = component(
                    "interrupted",
                    "optional refresh did not finish after controller promotion",
                );
            }
            self.finish(RunStatus::Pass);
            return;
        }
        self.metadata["phase"] = json!("interrupted");
        self.finish(RunStatus::Error);
    }
}

pub fn load_upgrade_operation_status(id: Option<&str>) -> Result<UpgradeOperationStatus> {
    let store = ObservationStore::open_initialized()?;
    let run = match id {
        Some(id) => store.get_run(id)?.ok_or_else(|| {
            Error::validation_invalid_argument(
                "id",
                format!("upgrade operation not found: {id}"),
                Some(id.to_string()),
                None,
            )
        })?,
        None => store
            .latest_run(RunListFilter {
                kind: Some(UPGRADE_OPERATION_KIND.to_string()),
                limit: Some(1),
                ..RunListFilter::default()
            })?
            .ok_or_else(|| {
                Error::validation_invalid_argument(
                    "id",
                    "no upgrade operation has been recorded",
                    None,
                    None,
                )
            })?,
    };
    status_from_run(&run)
}

pub fn persist_extension_progress(
    run_id: &str,
    extension_id: &str,
    current: usize,
    total: usize,
    elapsed: Duration,
) {
    emit_upgrade_phase(&upgrade_extension_progress_message(
        extension_id,
        current,
        total,
        elapsed,
    ));
    patch_metadata(run_id, |metadata| {
        metadata["phase"] = json!("refreshing_installed_extensions");
        metadata["elapsed_seconds"] = json!(elapsed.as_secs());
        metadata["extensions"] = component(
            "running",
            format!("refreshing {extension_id} ({current}/{total})"),
        );
    });
}

pub fn run_with_upgrade_heartbeats<T>(
    interval: Duration,
    heartbeat: impl Fn(Duration) + Send + Sync,
    operation: impl FnOnce() -> T,
) -> T {
    let finished = AtomicBool::new(false);
    let started = Instant::now();
    std::thread::scope(|scope| {
        let finished = &finished;
        let heartbeat = &heartbeat;
        let monitor = scope.spawn(move || {
            while !finished.load(Ordering::Acquire) {
                std::thread::park_timeout(interval);
                if !finished.load(Ordering::Acquire) {
                    heartbeat(started.elapsed());
                }
            }
        });

        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(operation));
        finished.store(true, Ordering::Release);
        monitor.thread().unpark();
        monitor.join().expect("upgrade heartbeat monitor panicked");
        match result {
            Ok(result) => result,
            Err(payload) => std::panic::resume_unwind(payload),
        }
    })
}

pub(crate) fn emit_upgrade_phase(phase: &str) {
    eprintln!("[upgrade] {phase}");
}

pub(crate) fn upgrade_extension_progress_message(
    id: &str,
    current: usize,
    total: usize,
    elapsed: Duration,
) -> String {
    format!(
        "phase=refreshing_installed_extensions extension={id} item={current}/{total} elapsed={}s",
        elapsed.as_secs()
    )
}

fn component(status: impl Into<String>, summary: impl Into<String>) -> Value {
    json!({
        "status": status.into(),
        "summary": summary.into(),
    })
}

fn patch_metadata(run_id: &str, patch: impl FnOnce(&mut Value)) {
    let Ok(store) = ObservationStore::open_initialized() else {
        return;
    };
    let Ok(Some(run)) = store.get_run(run_id) else {
        return;
    };
    if run.kind != UPGRADE_OPERATION_KIND {
        return;
    }
    let mut metadata = run.metadata_json;
    patch(&mut metadata);
    let _ = store.update_run_metadata(run_id, metadata);
}

fn merge_component_fields(target: &mut Value, source: &Value) {
    for key in ["controller", "extensions", "runners", "phase"] {
        if let Some(value) = source.get(key) {
            if target.get(key).is_none() {
                target[key] = value.clone();
            }
        }
    }
}

fn status_from_run(run: &RunRecord) -> Result<UpgradeOperationStatus> {
    if run.kind != UPGRADE_OPERATION_KIND {
        return Err(Error::validation_invalid_argument(
            "id",
            format!("run {} is not an upgrade operation", run.id),
            Some(run.id.clone()),
            None,
        ));
    }
    let schema = run
        .metadata_json
        .get("schema")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !schema.is_empty() && schema != UPGRADE_OPERATION_SCHEMA {
        return Err(Error::validation_invalid_argument(
            "id",
            format!(
                "upgrade operation {} uses unsupported schema {schema}",
                run.id
            ),
            Some(run.id.clone()),
            None,
        ));
    }
    Ok(UpgradeOperationStatus {
        command: "upgrade.status".to_string(),
        operation_id: run.id.clone(),
        status: run.status.clone(),
        phase: run
            .metadata_json
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        elapsed_seconds: run
            .metadata_json
            .get("elapsed_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        controller: component_from_metadata(&run.metadata_json, "controller"),
        extensions: component_from_metadata(&run.metadata_json, "extensions"),
        runners: component_from_metadata(&run.metadata_json, "runners"),
        owner_pid: run_owner_pid(run),
        note: running_status_note(run),
        inspect_command: Some(format!("homeboy upgrade status {}", run.id)),
    })
}

fn component_from_metadata(metadata: &Value, key: &str) -> Option<UpgradeComponentStatus> {
    let value = metadata.get(key)?;
    Some(UpgradeComponentStatus {
        status: value.get("status")?.as_str()?.to_string(),
        summary: value.get("summary")?.as_str()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn extension_progress_identifies_the_blocked_item_and_elapsed_time() {
        assert_eq!(
            upgrade_extension_progress_message("wordpress", 2, 3, Duration::from_secs(45)),
            "phase=refreshing_installed_extensions extension=wordpress item=2/3 elapsed=45s"
        );
    }

    #[test]
    fn blocked_upgrade_work_emits_a_heartbeat_before_it_completes() {
        let (heartbeat_tx, heartbeat_rx) = mpsc::channel();
        let result = run_with_upgrade_heartbeats(
            Duration::from_millis(1),
            move |elapsed| heartbeat_tx.send(elapsed).expect("record heartbeat"),
            || {
                heartbeat_rx
                    .recv_timeout(Duration::from_secs(1))
                    .expect("heartbeat while operation remains blocked");
                42
            },
        );

        assert_eq!(result, 42);
    }

    #[test]
    fn completed_upgrade_work_does_not_emit_a_late_heartbeat() {
        let (heartbeat_tx, heartbeat_rx) = mpsc::channel();
        run_with_upgrade_heartbeats(
            Duration::from_secs(1),
            move |elapsed| heartbeat_tx.send(elapsed).expect("record heartbeat"),
            || (),
        );

        assert!(heartbeat_rx.try_recv().is_err());
    }

    #[test]
    fn start_persists_a_running_operation_before_mutation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.operation_id, id);
            assert_eq!(status.status, RunStatus::Running.as_str());
            assert_eq!(status.phase, "admitted");
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("pending")
            );
        });
    }

    #[test]
    fn controller_promotion_stays_inspectable_while_extension_refresh_runs() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation.mark_controller_promoted("controller installation completed");
            persist_extension_progress(&id, "wordpress", 2, 3, Duration::from_secs(45));

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.status, RunStatus::Running.as_str());
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("updated")
            );
            assert_eq!(
                status
                    .extensions
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("running")
            );
            assert!(status
                .extensions
                .as_ref()
                .map(|component| component.summary.as_str())
                .unwrap_or_default()
                .contains("wordpress"));
            assert_eq!(status.elapsed_seconds, 45);
        });
    }

    #[test]
    fn interrupting_after_controller_promotion_is_not_a_failed_install() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let id = {
                let mut operation = UpgradeOperation::start("homeboy upgrade");
                let id = operation.id().expect("persisted operation").to_string();
                operation.mark_controller_promoted("controller installation completed");
                persist_extension_progress(&id, "wordpress", 1, 2, Duration::from_secs(12));
                id
            };

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.status, RunStatus::Pass.as_str());
            assert_eq!(status.phase, "interrupted_after_controller");
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("updated")
            );
            assert_eq!(
                status
                    .extensions
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("interrupted")
            );
        });
    }

    #[test]
    fn latest_status_reports_the_most_recent_upgrade_operation() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let first = UpgradeOperation::start("homeboy upgrade");
            let first_id = first.id().expect("first").to_string();
            drop(first);
            let second = UpgradeOperation::start("homeboy upgrade");
            let second_id = second.id().expect("second").to_string();
            drop(second);

            let status = load_upgrade_operation_status(None).expect("latest");
            assert_eq!(status.operation_id, second_id);
            assert_ne!(status.operation_id, first_id);
        });
    }

    #[test]
    fn completed_result_is_terminal_pass_with_independent_component_statuses() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            let result = UpgradeResult {
                command: "upgrade".to_string(),
                install_method: super::super::types::InstallMethod::Binary,
                previous_version: "0.1.0".to_string(),
                new_version: Some("0.2.0".to_string()),
                previous_build_identity: None,
                new_build_identity: None,
                source_revision: None,
                upgraded: true,
                outcome: Some("controller_updated".to_string()),
                preflight: None,
                controller: Some(UpgradeComponentStatus {
                    status: "updated".to_string(),
                    summary: "controller installation completed".to_string(),
                }),
                extensions: Some(UpgradeComponentStatus {
                    status: "completed".to_string(),
                    summary: "1 updated, 0 skipped".to_string(),
                }),
                runners: Some(UpgradeComponentStatus {
                    status: "skipped".to_string(),
                    summary: "runner convergence skipped".to_string(),
                }),
                partial: false,
                runner_convergence: None,
                message: "Upgraded".to_string(),
                restart_required: false,
                extensions_updated: Vec::new(),
                extensions_skipped: Vec::new(),
                extension_skips: Vec::new(),
                runners_updated: Vec::new(),
                runners_skipped: Vec::new(),
                extensions_unrefreshed: Vec::new(),
                services_restarted: Vec::new(),
                services_pending_restart: Vec::new(),
                operation_id: None,
            };
            operation.finish_completed(&result);

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.status, RunStatus::Pass.as_str());
            assert_eq!(status.phase, "completed");
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("updated")
            );
            assert_eq!(
                status
                    .extensions
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("completed")
            );
        });
    }
}
