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

use super::types::{RunnerConvergenceDisposition, UpgradeComponentStatus, UpgradeResult};

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub promotion_wait: Option<UpgradePromotionWaitStatus>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UpgradePromotionWaitStatus {
    pub schema: String,
    pub state: String,
    pub resource_class: String,
    pub wait_timeout_ms: u128,
    pub waited_ms: u128,
    pub wait_stage: String,
    pub owner_pid: u32,
    pub owner_operation: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_operation_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_status_command: Option<String>,
    pub target: String,
    pub owner_generation: String,
}

pub struct UpgradeOperation {
    observation: Option<ActiveObservation>,
    metadata: Value,
    started: Instant,
    finished: bool,
    pending_terminal: Option<TerminalIntent>,
    controller_promoted: bool,
    persistence_error: Option<Error>,
    #[cfg(test)]
    fail_terminal_writes_remaining: usize,
    #[cfg(test)]
    fail_next_progress_write: bool,
    #[cfg(test)]
    before_terminal_write: Option<Box<dyn FnOnce() + Send>>,
    #[cfg(test)]
    after_promotion_wait: Option<Box<dyn FnOnce() + Send>>,
}

#[derive(Clone)]
struct TerminalIntent {
    status: RunStatus,
    metadata: Value,
    intent_id: String,
}

impl UpgradeOperation {
    #[cfg(test)]
    pub fn start(command: impl Into<String>) -> Self {
        let metadata = initial_metadata();
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
            pending_terminal: None,
            controller_promoted: false,
            persistence_error: None,
            fail_terminal_writes_remaining: 0,
            fail_next_progress_write: false,
            before_terminal_write: None,
            after_promotion_wait: None,
        };
        if let Some(id) = operation.id() {
            emit_upgrade_phase(&format!("operation={id}"));
        }
        operation
    }

    pub fn start_durable(command: impl Into<String>) -> Result<Self> {
        let metadata = initial_metadata();
        let observation = ActiveObservation::start(
            NewRunRecord::builder(UPGRADE_OPERATION_KIND)
                .component_id("homeboy")
                .command(command.into())
                .current_homeboy_version()
                .metadata(metadata.clone())
                .build(),
        )?;
        let operation = Self {
            observation: Some(observation),
            metadata,
            started: Instant::now(),
            finished: false,
            pending_terminal: None,
            controller_promoted: false,
            persistence_error: None,
            #[cfg(test)]
            fail_terminal_writes_remaining: 0,
            #[cfg(test)]
            fail_next_progress_write: false,
            #[cfg(test)]
            before_terminal_write: None,
            #[cfg(test)]
            after_promotion_wait: None,
        };
        emit_upgrade_phase(&format!(
            "operation={}",
            operation.id().expect("durable operation has an id")
        ));
        Ok(operation)
    }

    pub fn id(&self) -> Option<&str> {
        self.observation.as_ref().map(ActiveObservation::run_id)
    }

    pub fn set_phase_durable(&mut self, phase: &str) -> Result<()> {
        emit_upgrade_phase(phase);
        self.metadata["phase"] = json!(phase);
        match phase {
            "refreshing installed extensions" => {
                self.metadata["extensions"] = component("running", "extension refresh in progress");
            }
            "refreshing configured runners" => {
                self.metadata["runners"] = component("running", "runner refresh in progress");
            }
            _ => {}
        }
        self.metadata["elapsed_seconds"] = json!(self.started.elapsed().as_secs());
        self.persist_durable()
    }

    pub fn record_promotion_wait(
        &mut self,
        event: &homeboy_core::runtime_promotion::RuntimePromotionWaitEvent,
    ) {
        emit_upgrade_phase(
            &serde_json::to_string(event)
                .unwrap_or_else(|_| "runtime promotion queued".to_string()),
        );
        self.metadata["promotion_wait"] = json!(event);
        if let Err(error) = self.set_phase_durable(match event.wait_stage {
            "os_lock" => "waiting_for_controller_admission_lock",
            "foreign_generation_pins" => "waiting_for_foreign_generation_pins",
            _ => "waiting_for_compatible_controller_upgrade",
        }) {
            self.persistence_error.get_or_insert(error);
        }
        #[cfg(test)]
        if let Some(after_wait) = self.after_promotion_wait.take() {
            after_wait();
        }
    }

    pub fn take_persistence_error(&mut self) -> Result<()> {
        self.persistence_error.take().map_or(Ok(()), Err)
    }

    pub fn clear_promotion_wait_durable(&mut self) -> Result<()> {
        if let Some(metadata) = self.metadata.as_object_mut() {
            metadata.remove("promotion_wait");
        }
        self.persist_durable()
    }

    pub fn mark_controller_promoted_durable(&mut self, summary: &str) -> Result<()> {
        self.controller_promoted = true;
        self.metadata["controller"] = component("updated", summary);
        self.set_phase_durable(
            "controller installation verified; continuing with optional post-install refresh",
        )
    }

    pub fn mark_controller_durable(&mut self, status: &str, summary: &str) -> Result<()> {
        if status == "updated" {
            self.controller_promoted = true;
        }
        self.metadata["controller"] = component(status, summary);
        self.persist_durable()
    }

    pub fn record_replacement_checkpoint_durable(
        &mut self,
        checkpoint: &super::execution::ReplacementCheckpoint,
    ) -> Result<()> {
        let previous_metadata = self.metadata.clone();
        let previous_controller_promoted = self.controller_promoted;
        self.metadata["replacement"] = json!(checkpoint);
        if checkpoint.state == "applied" {
            self.controller_promoted = true;
            self.metadata["controller"] = component(
                "replacement_applied",
                "controller replacement applied; verification pending",
            );
            self.metadata["phase"] = json!("controller_replacement_applied");
        } else if checkpoint.state == "pending" {
            self.metadata["phase"] = json!("controller_replacement_pending");
        } else if checkpoint.state == "not_applied" {
            self.metadata["controller"] = component(
                "replacement_not_applied",
                "controller replacement did not change the installed target",
            );
            self.metadata["phase"] = json!("controller_replacement_not_applied");
        } else if checkpoint.state == "changed_unverified" {
            self.metadata["controller"] = component(
                "replacement_changed_unverified",
                "controller target changed, but the selected identity could not be verified",
            );
            self.metadata["phase"] = json!("controller_replacement_changed_unverified");
        } else if checkpoint.state == "evidence_unavailable" {
            self.metadata["controller"] = component(
                "replacement_evidence_unavailable",
                "controller replacement evidence could not be read",
            );
            self.metadata["phase"] = json!("controller_replacement_evidence_unavailable");
        }
        let persisted = self.persist_durable();
        if persisted.is_err() && checkpoint.state == "pending" {
            self.metadata = previous_metadata;
            self.controller_promoted = previous_controller_promoted;
        }
        persisted
    }

    pub fn mark_extensions_durable(&mut self, status: &str, summary: &str) -> Result<()> {
        self.metadata["extensions"] = component(status, summary);
        self.persist_durable()
    }

    #[cfg(test)]
    pub fn finish_completed(&mut self, result: &UpgradeResult) {
        let _ = self.finish_completed_durable(result);
    }

    pub fn finish_completed_durable(&mut self, result: &UpgradeResult) -> Result<()> {
        if let Some(metadata) = self.metadata.as_object_mut() {
            metadata.remove("promotion_wait");
        }
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
        let status = if result.runner_convergence == Some(RunnerConvergenceDisposition::Partial)
            || result.controller.as_ref().is_some_and(|controller| {
                matches!(
                    controller.status.as_str(),
                    "failed" | "runner_preflight_failed" | "extension_preflight_failed"
                )
            }) {
            RunStatus::Fail
        } else {
            RunStatus::Pass
        };
        self.finish_durable(status)
    }

    pub fn finish_failed_durable(&mut self, error: &Error) -> Result<()> {
        if let Some(metadata) = self.metadata.as_object_mut() {
            metadata.remove("promotion_wait");
        }
        self.metadata["phase"] = json!("failed");
        self.metadata["error"] = json!({
            "code": format!("{:?}", error.code),
            "message": error.message,
        });
        self.finish_durable(RunStatus::Error)
    }

    pub(crate) fn replace_pending_terminal_with_failure(&mut self, error: &Error) -> Result<()> {
        self.pending_terminal = None;
        self.finished = false;
        self.finish_failed_durable(error)
    }

    fn persist_durable(&mut self) -> Result<()> {
        let Some(observation) = &self.observation else {
            return Err(Error::internal_unexpected(
                "upgrade operation has no durable observation",
            ));
        };
        self.metadata["elapsed_seconds"] = json!(self.started.elapsed().as_secs());
        #[cfg(test)]
        if std::mem::take(&mut self.fail_next_progress_write) {
            return Err(Error::internal_unexpected(
                "injected upgrade progress persistence failure",
            ));
        }
        let updated = observation
            .store()
            .update_running_run_metadata(observation.run_id(), self.metadata.clone())?;
        updated.map(|_| ()).ok_or_else(|| {
            Error::internal_unexpected(format!(
                "upgrade operation is no longer running: {}",
                observation.run_id()
            ))
        })
    }

    fn finish(&mut self, status: RunStatus) {
        let _ = self.finish_durable(status);
    }

    fn finish_durable(&mut self, status: RunStatus) -> Result<()> {
        if self.finished {
            return Ok(());
        }
        let Some(observation) = &self.observation else {
            return Err(Error::internal_unexpected(
                "upgrade operation has no durable observation",
            ));
        };
        let run = observation
            .store()
            .get_run(observation.run_id())?
            .ok_or_else(|| {
                Error::internal_unexpected(format!(
                    "upgrade operation disappeared during terminalization: {}",
                    observation.run_id()
                ))
            })?;
        merge_component_fields(&mut self.metadata, &run.metadata_json);
        if let Some(elapsed) = run.metadata_json.get("elapsed_seconds") {
            self.metadata["elapsed_seconds"] = elapsed.clone();
        }
        self.metadata["elapsed_seconds"] = json!(self.started.elapsed().as_secs());
        if self.pending_terminal.is_none() {
            let intent_id = format!("upgrade-terminal:{}", observation.run_id());
            self.metadata["terminal_intent_id"] = json!(intent_id);
            self.pending_terminal = Some(TerminalIntent {
                status,
                metadata: self.metadata.clone(),
                intent_id,
            });
        } else if let Some(intent) = self.pending_terminal.as_mut() {
            merge_component_fields(&mut intent.metadata, &run.metadata_json);
        }
        let mut expected_metadata = run.metadata_json;
        let mut last_error = None;
        for _ in 0..3 {
            #[cfg(test)]
            if let Some(before_write) = self.before_terminal_write.take() {
                before_write();
            }
            let intent = self
                .pending_terminal
                .clone()
                .expect("terminal intent initialized");
            #[cfg(test)]
            if self.fail_terminal_writes_remaining > 0 {
                self.fail_terminal_writes_remaining -= 1;
                last_error = Some(Error::internal_unexpected(
                    "injected upgrade terminal persistence failure",
                ));
                continue;
            }
            match observation.store().finish_running_run_if_metadata(
                observation.run_id(),
                intent.status,
                intent.metadata.clone(),
                &expected_metadata,
            ) {
                Ok(Some(run)) if terminal_run_matches(&run, &intent) => {
                    self.finished = true;
                    self.pending_terminal = None;
                    return Ok(());
                }
                Ok(Some(_)) => {
                    return Err(terminal_conflict_error(observation.run_id()));
                }
                Ok(None) | Err(_) => match observation.store().get_run(observation.run_id()) {
                    Ok(Some(run)) if terminal_run_matches(&run, &intent) => {
                        self.finished = true;
                        self.pending_terminal = None;
                        return Ok(());
                    }
                    Ok(Some(run)) if run.status == RunStatus::Running.as_str() => {
                        if let Some(intent) = self.pending_terminal.as_mut() {
                            merge_component_fields(&mut intent.metadata, &run.metadata_json);
                        }
                        expected_metadata = run.metadata_json;
                        last_error = Some(Error::internal_unexpected(
                            "upgrade terminal write raced with running progress",
                        ));
                    }
                    Ok(Some(_)) => {
                        return Err(terminal_conflict_error(observation.run_id()));
                    }
                    Ok(None) => {
                        return Err(Error::internal_unexpected(format!(
                            "upgrade operation disappeared during terminalization: {}",
                            observation.run_id()
                        )));
                    }
                    Err(error) => last_error = Some(error),
                },
            }
        }
        Err(last_error.unwrap_or_else(|| {
            Error::internal_unexpected("upgrade terminalization retries were exhausted")
        }))
    }

    #[cfg(test)]
    pub(crate) fn fail_next_terminal_write(&mut self) {
        self.fail_terminal_writes_remaining = 1;
    }

    #[cfg(test)]
    pub(crate) fn fail_next_terminal_writes(&mut self, count: usize) {
        self.fail_terminal_writes_remaining = count;
    }

    #[cfg(test)]
    pub(crate) fn fail_next_progress_write(&mut self) {
        self.fail_next_progress_write = true;
    }

    #[cfg(test)]
    pub(crate) fn before_terminal_write(&mut self, callback: impl FnOnce() + Send + 'static) {
        self.before_terminal_write = Some(Box::new(callback));
    }

    #[cfg(test)]
    pub(crate) fn after_promotion_wait(&mut self, callback: impl FnOnce() + Send + 'static) {
        self.after_promotion_wait = Some(Box::new(callback));
    }
}

fn initial_metadata() -> Value {
    json!({
        "schema": UPGRADE_OPERATION_SCHEMA,
        "phase": "admitted",
        "elapsed_seconds": 0,
        "controller": component("pending", "controller mutation has not started"),
        "extensions": component("pending", "extension refresh has not started"),
        "runners": component("pending", "runner refresh has not started"),
        "homeboy_run_owner": { "pid": std::process::id() },
    })
}

impl Drop for UpgradeOperation {
    fn drop(&mut self) {
        if self.finished {
            return;
        }
        // Panic unwinding and a hard process crash must leave the same
        // lease-recoverable checkpoint for the next mutation owner to freeze.
        if self.metadata["replacement"]["state"].as_str() == Some("pending") {
            return;
        }
        if self.pending_terminal.is_some() {
            let status = self
                .pending_terminal
                .as_ref()
                .expect("pending terminal exists")
                .status;
            let _ = self.finish_durable(status);
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

fn terminal_run_matches(run: &RunRecord, intent: &TerminalIntent) -> bool {
    run.status == intent.status.as_str()
        && run.metadata_json["terminal_intent_id"].as_str() == Some(intent.intent_id.as_str())
}

fn terminal_conflict_error(run_id: &str) -> Error {
    Error::internal_unexpected(format!(
        "upgrade operation terminal state changed before completion: {run_id}"
    ))
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
) -> Result<()> {
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
    })
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

pub fn persist_upgrade_heartbeat(run_id: &str, elapsed: Duration) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let Some(run) = store.get_run(run_id)? else {
        return Err(Error::internal_unexpected(format!(
            "upgrade operation disappeared while heartbeating: {run_id}"
        )));
    };
    let mut metadata = run.metadata_json;
    metadata["elapsed_seconds"] = json!(elapsed.as_secs());
    store
        .update_running_run_metadata(run_id, metadata)?
        .map(|_| ())
        .ok_or_else(|| {
            Error::internal_unexpected(format!("upgrade operation is no longer running: {run_id}"))
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

fn patch_metadata(run_id: &str, patch: impl FnOnce(&mut Value)) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let Some(run) = store.get_run(run_id)? else {
        return Err(Error::internal_unexpected(format!(
            "upgrade operation disappeared while recording progress: {run_id}"
        )));
    };
    if run.kind != UPGRADE_OPERATION_KIND {
        return Err(Error::internal_unexpected(format!(
            "run {run_id} is not an upgrade operation"
        )));
    }
    let mut metadata = run.metadata_json;
    patch(&mut metadata);
    store
        .update_running_run_metadata(run_id, metadata)?
        .map(|_| ())
        .ok_or_else(|| {
            Error::internal_unexpected(format!("upgrade operation is no longer running: {run_id}"))
        })
}

fn merge_component_fields(target: &mut Value, source: &Value) {
    for key in ["controller", "extensions", "runners"] {
        if let Some(value) = source.get(key) {
            let target_is_pending = target
                .get(key)
                .and_then(|component| component.get("status"))
                .and_then(Value::as_str)
                == Some("pending");
            let source_is_pending = value.get("status").and_then(Value::as_str) == Some("pending");
            if target.get(key).is_none() || (target_is_pending && !source_is_pending) {
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
    let mut metadata = run.metadata_json.clone();
    reconcile_replacement_projection(&mut metadata);
    Ok(UpgradeOperationStatus {
        command: "upgrade.status".to_string(),
        operation_id: run.id.clone(),
        status: run.status.clone(),
        phase: metadata
            .get("phase")
            .and_then(Value::as_str)
            .unwrap_or("unknown")
            .to_string(),
        elapsed_seconds: metadata
            .get("elapsed_seconds")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        controller: component_from_metadata(&metadata, "controller"),
        extensions: component_from_metadata(&metadata, "extensions"),
        runners: component_from_metadata(&metadata, "runners"),
        owner_pid: run_owner_pid(run),
        note: running_status_note(run),
        inspect_command: Some(format!("homeboy upgrade status {}", run.id)),
        promotion_wait: run
            .metadata_json
            .get("promotion_wait")
            .cloned()
            .and_then(|value| serde_json::from_value(value).ok()),
    })
}

fn reconcile_replacement_projection(metadata: &mut Value) {
    let Some(replacement) = metadata.get("replacement") else {
        return;
    };
    // Only a pending checkpoint represents the crash window where disk state is
    // authoritative. Applied and terminal history must not change when a later
    // operation installs another controller.
    if replacement.get("state").and_then(Value::as_str) != Some("pending") {
        return;
    }
    let Ok(checkpoint) =
        serde_json::from_value::<super::execution::ReplacementCheckpoint>(replacement.clone())
    else {
        metadata["controller"] = component(
            "replacement_evidence_missing",
            "pending replacement has no pre-mutation byte evidence",
        );
        return;
    };
    match super::execution::replacement_was_applied(&checkpoint) {
        Ok(true) => {
            let identity = super::execution::replacement_applied_identity(&checkpoint)
                .ok()
                .flatten();
            metadata["controller"] = component(
                "replacement_applied",
                identity.map_or_else(
                    || "installed controller bytes match the selected replacement".to_string(),
                    |identity| format!("installed controller is {}", identity.display),
                ),
            );
            if metadata["phase"] == "controller_replacement_pending" {
                metadata["phase"] = json!("controller_replacement_applied");
            }
        }
        Ok(false) => {
            metadata["controller"] = component(
                "replacement_not_observed",
                "installed controller bytes do not prove this replacement was applied",
            );
        }
        Err(_) => {
            metadata["controller"] = component(
                "identity_unknown",
                "installed controller identity could not be verified",
            );
        }
    }
}

pub(crate) fn freeze_prior_pending_replacements(current_operation_id: &str) -> Result<()> {
    let store = ObservationStore::open_initialized()?;
    let runs = store.list_runs_all(RunListFilter {
        kind: Some(UPGRADE_OPERATION_KIND.to_string()),
        status: Some(RunStatus::Running.as_str().to_string()),
        ..RunListFilter::default()
    })?;
    for run in runs {
        if run.id == current_operation_id
            || run.metadata_json["replacement"]["state"].as_str() != Some("pending")
        {
            continue;
        }
        let mut metadata = run.metadata_json;
        let checkpoint = serde_json::from_value::<super::execution::ReplacementCheckpoint>(
            metadata["replacement"].clone(),
        )
        .map_err(|error| {
            Error::internal_unexpected(format!(
                "prior upgrade operation has invalid replacement evidence: {}: {error}",
                run.id
            ))
        })?;
        let state = super::execution::replacement_observed_state(&checkpoint)?;
        metadata["replacement"]["state"] = json!(state);
        match state {
            "applied" => {
                metadata["controller"] = component(
                    "replacement_applied",
                    "installed controller bytes prove the selected replacement was applied",
                );
                metadata["phase"] = json!("controller_replacement_applied");
            }
            "changed_unverified" => {
                metadata["controller"] = component(
                    "replacement_changed_unverified",
                    "controller target changed, but the selected identity could not be verified",
                );
                metadata["phase"] = json!("controller_replacement_changed_unverified");
            }
            "not_applied" => {
                metadata["controller"] = component(
                    "replacement_not_applied",
                    "controller replacement did not change the installed target",
                );
                metadata["phase"] = json!("controller_replacement_not_applied");
            }
            _ => {
                metadata["controller"] = component(
                    "replacement_evidence_unavailable",
                    "controller replacement evidence could not be read",
                );
                metadata["phase"] = json!("controller_replacement_evidence_unavailable");
            }
        }
        store
            .update_running_run_metadata(&run.id, metadata)?
            .ok_or_else(|| {
                Error::internal_unexpected(format!(
                    "prior upgrade operation changed while freezing replacement evidence: {}",
                    run.id
                ))
            })?;
    }
    Ok(())
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
    fn promotion_wait_is_visible_through_upgrade_status() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let owner_operation = UpgradeOperation::start("homeboy upgrade");
            let owner_id = owner_operation
                .id()
                .expect("persisted owner operation")
                .to_string();
            let owner_lease =
                homeboy_core::runtime_promotion::acquire_waiting_for_target_with_status(
                    "controller upgrade",
                    "active controller",
                    homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
                        operation_id: owner_id.clone(),
                        status_command: format!("homeboy upgrade status {owner_id}"),
                    },
                    Duration::from_secs(1),
                    |_| unreachable!("uncontended owner does not wait"),
                )
                .expect("owner acquires controller admission");
            let (queued, queued_event) = std::sync::mpsc::channel();
            let contender = std::thread::spawn(move || {
                let mut operation = UpgradeOperation::start("homeboy upgrade");
                let id = operation.id().expect("persisted operation").to_string();
                let lease =
                    homeboy_core::runtime_promotion::acquire_waiting_for_target_with_status(
                        "controller upgrade",
                        "active controller",
                        homeboy_core::runtime_promotion::RuntimePromotionOwnerStatus {
                            operation_id: id.clone(),
                            status_command: format!("homeboy upgrade status {id}"),
                        },
                        Duration::from_secs(1),
                        |event| {
                            operation.record_promotion_wait(&event);
                            queued.send(()).expect("report queued contender");
                        },
                    )
                    .expect("contender acquires after deterministic handoff");
                drop(lease);
                (id, operation)
            });
            queued_event
                .recv_timeout(Duration::from_secs(1))
                .expect("contender reaches durable wait state");
            drop(owner_lease);
            let (id, mut operation) = contender.join().expect("contender exits");

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.phase, "waiting_for_compatible_controller_upgrade");
            let wait = status.promotion_wait.expect("promotion wait status");
            assert_eq!(wait.wait_stage, "lease_record");
            assert_eq!(wait.owner_operation_id.as_deref(), Some(owner_id.as_str()));
            assert_eq!(
                wait.owner_status_command.as_deref(),
                Some(format!("homeboy upgrade status {owner_id}").as_str())
            );
            assert_ne!(id, owner_id);

            operation
                .clear_promotion_wait_durable()
                .expect("clear completed wait metadata");
            let status = load_upgrade_operation_status(Some(&id)).expect("reload status");
            assert!(status.promotion_wait.is_none());
        });
    }

    #[test]
    fn controller_promotion_stays_inspectable_while_extension_refresh_runs() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation
                .mark_controller_promoted_durable("controller installation completed")
                .expect("persist controller promotion");
            persist_extension_progress(&id, "wordpress", 2, 3, Duration::from_secs(45))
                .expect("persist extension progress");

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
    fn runner_refresh_phase_preserves_completed_extensions_and_started_runners() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation
                .mark_controller_promoted_durable("controller installation completed")
                .expect("persist controller promotion");
            operation
                .set_phase_durable("refreshing installed extensions")
                .expect("persist extension refresh phase");
            operation
                .mark_extensions_durable("completed", "7 updated, 0 skipped")
                .expect("persist completed extension refresh");
            operation
                .set_phase_durable("refreshing configured runners")
                .expect("persist runner refresh phase");

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.phase, "refreshing configured runners");
            assert_eq!(
                status
                    .extensions
                    .as_ref()
                    .map(|component| (component.status.as_str(), component.summary.as_str())),
                Some(("completed", "7 updated, 0 skipped"))
            );
            assert_eq!(
                status
                    .runners
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("running")
            );
        });
    }

    #[test]
    fn interrupting_after_controller_promotion_is_not_a_failed_install() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let id = {
                let mut operation = UpgradeOperation::start("homeboy upgrade");
                let id = operation.id().expect("persisted operation").to_string();
                operation
                    .mark_controller_promoted_durable("controller installation completed")
                    .expect("persist controller promotion");
                persist_extension_progress(&id, "wordpress", 1, 2, Duration::from_secs(12))
                    .expect("persist extension progress");
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
            let result = completed_upgrade_result();
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

    #[test]
    fn runner_partial_completion_is_durable_terminal_fail() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            let mut result = completed_upgrade_result();
            result.partial = true;
            result.outcome = Some("controller_updated_runner_failed".to_string());
            result.runner_convergence = Some(RunnerConvergenceDisposition::Partial);
            result.runners = Some(UpgradeComponentStatus {
                status: "partial".to_string(),
                summary: "0 converged, 1 requires repair".to_string(),
            });

            operation
                .finish_completed_durable(&result)
                .expect("persist typed partial completion");
            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.status, RunStatus::Fail.as_str());
            assert_eq!(status.phase, "completed");
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|value| value.status.as_str()),
                Some("updated")
            );
            assert_eq!(
                status.runners.as_ref().map(|value| value.status.as_str()),
                Some("partial")
            );
        });
    }

    #[test]
    fn replacement_applied_checkpoint_precedes_verification() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            let checkpoint = replacement_checkpoint("pending", "/tmp/homeboy-target");
            operation
                .record_replacement_checkpoint_durable(&checkpoint)
                .expect("persist replacement intent");
            operation
                .record_replacement_checkpoint_durable(&checkpoint.with_state("applied"))
                .expect("persist replacement application");

            let run = operation
                .observation
                .as_ref()
                .expect("observation")
                .store()
                .get_run(&id)
                .expect("read operation")
                .expect("operation exists");
            assert_eq!(run.metadata_json["replacement"]["state"], "applied");
            assert_eq!(run.metadata_json["phase"], "controller_replacement_applied");
            assert_eq!(
                run.metadata_json["controller"]["status"],
                "replacement_applied"
            );
        });
    }

    #[test]
    fn failed_pending_checkpoint_is_not_replayed_into_terminal_metadata() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            let checkpoint = replacement_checkpoint("pending", "/tmp/homeboy-target");
            operation.fail_next_progress_write();
            let checkpoint_error = operation
                .record_replacement_checkpoint_durable(&checkpoint)
                .expect_err("inject pending-checkpoint persistence failure");
            operation
                .finish_failed_durable(&checkpoint_error)
                .expect("terminalize the pre-mutation failure");

            let run = operation
                .observation
                .as_ref()
                .expect("observation")
                .store()
                .get_run(&id)
                .expect("read operation")
                .expect("operation exists");
            assert!(run.metadata_json.get("replacement").is_none());
            assert_eq!(run.status, RunStatus::Error.as_str());
        });
    }

    #[test]
    fn applied_checkpoint_survives_the_following_persistence_failure() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            let checkpoint = replacement_checkpoint("pending", "/tmp/homeboy-target");
            operation
                .record_replacement_checkpoint_durable(&checkpoint)
                .expect("persist replacement intent");
            operation.fail_next_progress_write();
            let checkpoint_error = operation
                .record_replacement_checkpoint_durable(&checkpoint.with_state("applied"))
                .expect_err("inject applied-checkpoint persistence failure");
            operation
                .finish_failed_durable(&checkpoint_error)
                .expect("terminal retry carries in-memory applied state");

            let run = operation
                .observation
                .as_ref()
                .expect("observation")
                .store()
                .get_run(&id)
                .expect("read operation")
                .expect("operation exists");
            assert_eq!(run.status, RunStatus::Error.as_str());
            assert_eq!(run.metadata_json["replacement"]["state"], "applied");
            assert_eq!(
                run.metadata_json["controller"]["status"],
                "replacement_applied"
            );
        });
    }

    #[test]
    fn failed_terminalization_preserves_concurrent_component_progress() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            persist_extension_progress(&id, "wordpress", 1, 2, Duration::from_secs(12))
                .expect("persist concurrent extension progress");

            operation
                .finish_failed_durable(&Error::internal_unexpected("injected failure"))
                .expect("terminalize failure");

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.phase, "failed");
            assert_eq!(
                status
                    .extensions
                    .as_ref()
                    .map(|value| value.status.as_str()),
                Some("running")
            );
            assert!(status
                .extensions
                .as_ref()
                .is_some_and(|value| value.summary.contains("wordpress")));
        });
    }

    #[test]
    fn terminal_cas_retries_progress_written_after_its_merge_read() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            let progress_id = id.clone();
            operation.before_terminal_write(move || {
                persist_extension_progress(
                    &progress_id,
                    "woocommerce",
                    2,
                    3,
                    Duration::from_secs(18),
                )
                .expect("persist progress in terminal CAS window");
            });

            operation
                .finish_failed_durable(&Error::internal_unexpected("injected failure"))
                .expect("terminal CAS retries merged progress");

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_eq!(status.status, RunStatus::Error.as_str());
            assert_eq!(status.phase, "failed");
            assert!(status
                .extensions
                .as_ref()
                .is_some_and(|value| value.summary.contains("woocommerce")));
        });
    }

    #[test]
    fn applied_replacement_history_is_not_reconciled_against_current_disk() {
        let mut metadata = json!({
            "phase": "completed",
            "controller": component("updated", "controller installation completed"),
            "replacement": {
                "state": "applied",
                "target": "/definitely/not/the/current/controller",
                "expected_version": "0.2.0"
            }
        });

        reconcile_replacement_projection(&mut metadata);

        assert_eq!(metadata["phase"], "completed");
        assert_eq!(metadata["controller"]["status"], "updated");
    }

    #[cfg(unix)]
    #[test]
    fn crash_before_byte_swap_does_not_reconcile_same_version_replacement() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let target_dir = tempfile::tempdir().expect("target directory");
            let target = target_dir.path().join("homeboy");
            write_executable(&target, "#!/bin/sh\necho 'homeboy 0.2.0'\n# old\n");
            let checkpoint = super::super::execution::ReplacementCheckpoint::pending(
                &target,
                Some("0.2.0"),
                None,
                None,
            )
            .expect("capture pre-mutation evidence");

            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation
                .record_replacement_checkpoint_durable(&checkpoint)
                .expect("persist replacement intent");

            let status = load_upgrade_operation_status(Some(&id)).expect("reconcile status");
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("replacement_not_observed")
            );
            assert_eq!(status.phase, "controller_replacement_pending");

            freeze_prior_pending_replacements("next-controller-operation")
                .expect("freeze crash evidence before admitting the next mutation");
            write_executable(&target, "#!/bin/sh\necho 'homeboy 0.2.0'\n# later retry\n");
            let frozen = load_upgrade_operation_status(Some(&id)).expect("reload frozen status");
            assert_eq!(
                frozen
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("replacement_not_applied")
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn crash_after_same_version_byte_swap_reconciles_deliberate_replacement() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let target_dir = tempfile::tempdir().expect("target directory");
            let target = target_dir.path().join("homeboy");
            write_executable(&target, "#!/bin/sh\necho 'homeboy 0.2.0'\n# old\n");
            let checkpoint = super::super::execution::ReplacementCheckpoint::pending(
                &target,
                Some("0.2.0"),
                None,
                None,
            )
            .expect("capture pre-mutation evidence");

            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation
                .record_replacement_checkpoint_durable(&checkpoint)
                .expect("persist replacement intent");
            write_executable(&target, "#!/bin/sh\necho 'homeboy 0.2.0'\n# replacement\n");

            let status = load_upgrade_operation_status(Some(&id)).expect("reconcile status");
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("replacement_applied")
            );
            assert_eq!(status.phase, "controller_replacement_applied");
        });
    }

    #[cfg(unix)]
    #[test]
    fn panic_after_pending_remains_recoverable_by_the_next_mutation_owner() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let target_dir = tempfile::tempdir().expect("target directory");
            let target = target_dir.path().join("homeboy");
            write_executable(&target, "#!/bin/sh\necho 'homeboy 0.2.0'\n# old\n");
            let checkpoint = super::super::execution::ReplacementCheckpoint::pending(
                &target,
                Some("0.2.0"),
                None,
                None,
            )
            .expect("capture pre-mutation evidence");
            let operation_id = std::panic::catch_unwind(|| {
                let mut operation = UpgradeOperation::start("homeboy upgrade");
                let id = operation.id().expect("persisted operation").to_string();
                operation
                    .record_replacement_checkpoint_durable(&checkpoint)
                    .expect("persist replacement intent");
                std::panic::panic_any(id);
            })
            .expect_err("inject panic after pending")
            .downcast::<String>()
            .expect("panic carries operation id");

            let pending = load_upgrade_operation_status(Some(&operation_id))
                .expect("panic leaves pending operation inspectable");
            assert_eq!(pending.status, RunStatus::Running.as_str());
            freeze_prior_pending_replacements("next-controller-operation")
                .expect("next owner freezes panic evidence");
            let frozen =
                load_upgrade_operation_status(Some(&operation_id)).expect("load frozen operation");
            assert_eq!(
                frozen
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("replacement_not_applied")
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn crash_with_changed_unverifiable_bytes_freezes_distinct_evidence() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let target_dir = tempfile::tempdir().expect("target directory");
            let target = target_dir.path().join("homeboy");
            write_executable(&target, "#!/bin/sh\necho 'homeboy 0.2.0'\n# old\n");
            let checkpoint = super::super::execution::ReplacementCheckpoint::pending(
                &target,
                Some("0.2.0"),
                None,
                None,
            )
            .expect("capture pre-mutation evidence");
            let operation_id = {
                let mut operation = UpgradeOperation::start("homeboy upgrade");
                let id = operation.id().expect("persisted operation").to_string();
                operation
                    .record_replacement_checkpoint_durable(&checkpoint)
                    .expect("persist replacement intent");
                std::fs::write(&target, b"changed bytes that cannot execute")
                    .expect("simulate crash after byte transition");
                id
            };

            freeze_prior_pending_replacements("next-controller-operation")
                .expect("next owner freezes changed evidence");
            let frozen =
                load_upgrade_operation_status(Some(&operation_id)).expect("load frozen operation");
            assert_eq!(
                frozen
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("replacement_changed_unverified")
            );
            assert_eq!(frozen.phase, "controller_replacement_changed_unverified");
        });
    }

    #[cfg(unix)]
    #[test]
    fn spawning_failure_checkpoint_cannot_be_reclassified_by_later_swap() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let target_dir = tempfile::tempdir().expect("target directory");
            let target = target_dir.path().join("homeboy");
            write_executable(&target, "#!/bin/sh\necho 'homeboy 0.2.0'\n# old\n");
            let checkpoint = super::super::execution::ReplacementCheckpoint::pending(
                &target,
                Some("0.2.0"),
                None,
                None,
            )
            .expect("capture pre-mutation evidence")
            .with_state("not_applied");
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation
                .record_replacement_checkpoint_durable(&checkpoint)
                .expect("persist spawn failure");
            write_executable(
                &target,
                "#!/bin/sh\necho 'homeboy 0.2.0'\n# later operation\n",
            );

            let status = load_upgrade_operation_status(Some(&id)).expect("load status");
            assert_ne!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("replacement_applied")
            );
            assert_eq!(
                status
                    .controller
                    .as_ref()
                    .map(|component| component.status.as_str()),
                Some("replacement_not_applied")
            );
        });
    }

    fn replacement_checkpoint(
        state: &str,
        target: &str,
    ) -> super::super::execution::ReplacementCheckpoint {
        super::super::execution::ReplacementCheckpoint {
            state: state.to_string(),
            target: target.into(),
            expected_version: Some("0.2.0".to_string()),
            expected_build_identity: Some("0.2.0+candidate".to_string()),
            expected_sha256: None,
            previous_build_identity: Some("0.1.0+old".to_string()),
            previous_sha256: "old-digest".to_string(),
        }
    }

    #[cfg(unix)]
    fn write_executable(path: &std::path::Path, contents: &str) {
        use std::os::unix::fs::PermissionsExt;

        std::fs::write(path, contents).expect("write executable fixture");
        let mut permissions = std::fs::metadata(path)
            .expect("target metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).expect("make target executable");
    }

    #[test]
    fn lost_terminal_cas_is_a_conflict_not_success() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation
                .observation
                .as_ref()
                .expect("observation")
                .store()
                .finish_running_run(&id, RunStatus::Skipped, None)
                .expect("competing terminal write")
                .expect("competing CAS wins");

            let error = operation
                .finish_completed_durable(&completed_upgrade_result())
                .expect_err("lost terminal CAS cannot count as success");
            assert!(error.message.contains("terminal state changed"));
            let run = operation
                .observation
                .as_ref()
                .expect("observation")
                .store()
                .get_run(&id)
                .expect("read operation")
                .expect("operation exists");
            assert_eq!(run.status, RunStatus::Skipped.as_str());
        });
    }

    #[test]
    fn late_progress_cannot_overwrite_terminal_metadata() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let mut operation = UpgradeOperation::start("homeboy upgrade");
            let id = operation.id().expect("persisted operation").to_string();
            operation
                .finish_completed_durable(&completed_upgrade_result())
                .expect("terminalize operation");
            let store = operation.observation.as_ref().expect("observation").store();
            let before = store.get_run(&id).expect("read terminal").expect("run");
            let update = store
                .update_running_run_metadata(&id, json!({"phase": "late_progress"}))
                .expect("late progress CAS");
            assert!(update.is_none());
            let after = store.get_run(&id).expect("read terminal").expect("run");
            assert_eq!(after.metadata_json, before.metadata_json);
        });
    }

    fn completed_upgrade_result() -> UpgradeResult {
        UpgradeResult {
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
        }
    }
}
