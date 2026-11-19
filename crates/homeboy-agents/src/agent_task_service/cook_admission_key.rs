//! Idempotent Cook admission by caller-supplied key (#18450).
//!
//! A Cook submitted with an admission key owns that key while it is in
//! flight. A second submission with the same key returns the in-flight Cook's
//! identity instead of admitting another. Once the Cook is terminal, the key is
//! free again. The check and the claim run under one exclusive per-key file
//! lock held across durable admission, so concurrent submitters admit exactly
//! one Cook.

use std::fs::{self, File, OpenOptions};
use std::path::PathBuf;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::cook_recipe::CookRecipeStore;
use crate::agent_task_lifecycle::{AgentTaskLifecycleStore, CookRunState};
use homeboy_core::{Error, Result};

/// Plan metadata field carrying the caller's admission key.
pub const COOK_ADMISSION_KEY_METADATA: &str = "cook_admission_key";
/// Error detail naming the in-flight Cook that already owns a key.
pub(super) const ADMITTED_COOK_DETAIL: &str = "admission_key_admitted_cook";

/// Exclusive ownership of one admission key for the duration of admission.
/// The OS releases the lock if the controller dies mid-admission.
pub(super) struct CookAdmissionKeyGuard {
    _lock: File,
    claim_path: PathBuf,
    key: String,
}

impl CookAdmissionKeyGuard {
    /// Record `cook_id` as the key's owner. Call once admission is durable.
    pub(super) fn claim(self, cook_id: &str) -> Result<()> {
        homeboy_core::engine::local_files::write_json_file_owner_only(
            &self.claim_path,
            &json!({ "admission_key": self.key, "cook_id": cook_id }),
        )
    }
}

/// Take the key's admission lock. Fails with [`ADMITTED_COOK_DETAIL`] when an
/// in-flight Cook already owns the key.
pub(super) fn acquire(
    store: &CookRecipeStore,
    lifecycle: &AgentTaskLifecycleStore,
    key: &str,
) -> Result<CookAdmissionKeyGuard> {
    use fs4::fs_std::FileExt;

    let root = store.data_root().join("agent-task-cook-admission-keys");
    fs::create_dir_all(&root)
        .map_err(|error| Error::internal_io(error.to_string(), Some(root.display().to_string())))?;
    let digest = format!("{:x}", Sha256::digest(key.as_bytes()));
    let lock_path = root.join(format!("{digest}.lock"));
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(&lock_path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(lock_path.display().to_string()))
        })?;
    lock.lock_exclusive().map_err(|error| {
        Error::internal_io(error.to_string(), Some(lock_path.display().to_string()))
    })?;
    let claim_path = root.join(format!("{digest}.json"));
    let owner = fs::read_to_string(&claim_path)
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|claim| claim["cook_id"].as_str().map(str::to_string));
    if let Some(admitted) = owner.and_then(|cook_id| in_flight_cook(store, lifecycle, &cook_id)) {
        let mut error = Error::validation_invalid_argument(
            "admission_key",
            format!(
                "admission key is held by in-flight Cook `{}`",
                admitted["cook_id"].as_str().unwrap_or_default()
            ),
            Some(key.to_string()),
            None,
        );
        error.details[ADMITTED_COOK_DETAIL] = admitted;
        return Err(error);
    }
    Ok(CookAdmissionKeyGuard {
        _lock: lock,
        claim_path,
        key: key.to_string(),
    })
}

/// The identity and status of `cook_id` while it is in flight. A Cook whose
/// admission never became durable, or that is terminal, frees the key.
fn in_flight_cook(
    store: &CookRecipeStore,
    lifecycle: &AgentTaskLifecycleStore,
    cook_id: &str,
) -> Option<Value> {
    let record = super::cook_observation_in_store(lifecycle, cook_id).ok()?;
    let state = CookRunState::of(&record);
    let driven = store.cook_driver_held(cook_id).unwrap_or(false);
    if !driven && !state.phase.is_in_flight() {
        return None;
    }
    let status = if state.phase.is_in_flight() {
        state.status
    } else {
        "in_flight".to_string()
    };
    Some(json!({ "cook_id": cook_id, "run_id": record.run_id, "status": status }))
}
