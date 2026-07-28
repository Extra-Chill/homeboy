//! Controller-owned durable deferral for portable workloads that have no runner yet.

use crate::error::{Error, Result};
use fs4::fs_std::FileExt;
use homeboy_engine_primitives::content_hash::sha256_hex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const SCHEMA: &str = "homeboy/deferred-workloads/v1";
pub const CLAIM_LEASE_MS: u64 = 60_000;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredWorkload {
    pub id: String,
    pub fingerprint: String,
    pub command_label: String,
    pub args: Vec<String>,
    pub placement: String,
    pub resource_requirement: String,
    pub portability: String,
    pub reason: String,
    pub ci_alternative: String,
    pub resolved_contract: serde_json::Value,
    pub resolved_resources: serde_json::Value,
    #[serde(default)]
    pub test_requirements: DeferredWorkloadRequirements,
    pub job_overrides: crate::lab_offload::LabJobOverrides,
    pub state: DeferredWorkloadState,
    pub created_at_ms: u64,
    pub updated_at_ms: u64,
    pub runner_id: Option<String>,
    pub claim_owner: Option<String>,
    pub claim_expires_at_ms: Option<u64>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DeferredWorkloadState {
    Deferred,
    Claimed,
    Dispatched,
    Failed,
}

#[derive(Clone, Debug)]
pub struct DeferredWorkloadInput {
    pub command_label: String,
    pub args: Vec<String>,
    pub placement: String,
    pub resource_requirement: String,
    pub portability: String,
    pub reason: String,
    pub ci_alternative: String,
    pub resolved_contract: serde_json::Value,
    pub resolved_resources: serde_json::Value,
    pub test_requirements: DeferredWorkloadRequirements,
    pub job_overrides: crate::lab_offload::LabJobOverrides,
}

/// Exact runner admission requirements persisted with a deferred workload.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredWorkloadRequirements {
    #[serde(default)]
    pub required_runtimes: BTreeSet<String>,
    #[serde(default)]
    pub required_capabilities: BTreeSet<String>,
}

impl DeferredWorkloadRequirements {
    pub fn is_satisfied_by(
        &self,
        runtime_ids: &BTreeSet<String>,
        capabilities: &BTreeSet<String>,
    ) -> bool {
        self.required_runtimes.is_subset(runtime_ids)
            && self.required_capabilities.is_subset(capabilities)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct DeferredWorkloadWorkerStatus {
    pub schema: String,
    pub pid: u32,
    pub owner_token: String,
    pub linux_starttime_ticks: Option<u64>,
    pub state: String,
    pub updated_at_ms: u64,
    pub detail: String,
}

pub fn defer(input: DeferredWorkloadInput) -> Result<DeferredWorkload> {
    if input
        .job_overrides
        .secret_env_names
        .iter()
        .any(|name| input.job_overrides.env.contains_key(name))
    {
        return Err(Error::validation_invalid_argument(
            "job_overrides",
            "deferred workloads cannot persist inline values for runner secret identities",
            None,
            Some(vec![
                "Use a runner-owned secret reference instead of an inline environment value."
                    .to_string(),
            ]),
        ));
    }
    update(|records| {
        let fingerprint = fingerprint(&input)?;
        if let Some(existing) = records.iter().find(|record| {
            record.fingerprint == fingerprint
                && matches!(
                    record.state,
                    DeferredWorkloadState::Deferred | DeferredWorkloadState::Claimed
                )
        }) {
            return Ok(existing.clone());
        }
        let now = now_ms();
        let record = DeferredWorkload {
            id: format!("deferred-{}-{now}", &fingerprint[..16]),
            fingerprint,
            command_label: input.command_label,
            args: input.args,
            placement: input.placement,
            resource_requirement: input.resource_requirement,
            portability: input.portability,
            reason: input.reason,
            ci_alternative: input.ci_alternative,
            resolved_contract: input.resolved_contract,
            resolved_resources: input.resolved_resources,
            test_requirements: input.test_requirements,
            job_overrides: input.job_overrides,
            state: DeferredWorkloadState::Deferred,
            created_at_ms: now,
            updated_at_ms: now,
            runner_id: None,
            claim_owner: None,
            claim_expires_at_ms: None,
        };
        records.push(record.clone());
        Ok(record)
    })
}

pub fn claim(
    input: &DeferredWorkloadInput,
    runner_id: &str,
    owner: &str,
) -> Result<Option<DeferredWorkload>> {
    update(|records| {
        let fingerprint = fingerprint(input)?;
        let now = now_ms();
        for record in records
            .iter_mut()
            .filter(|record| record.fingerprint == fingerprint)
        {
            if record.state == DeferredWorkloadState::Claimed
                && record
                    .claim_expires_at_ms
                    .is_some_and(|expiry| expiry <= now)
            {
                record.state = DeferredWorkloadState::Deferred;
                record.runner_id = None;
                record.claim_owner = None;
                record.claim_expires_at_ms = None;
            }
        }
        let Some(record) = records.iter_mut().find(|record| {
            record.fingerprint == fingerprint && record.state == DeferredWorkloadState::Deferred
        }) else {
            return Ok(None);
        };
        record.state = DeferredWorkloadState::Claimed;
        record.runner_id = Some(runner_id.to_string());
        record.claim_owner = Some(owner.to_string());
        record.claim_expires_at_ms = Some(now + CLAIM_LEASE_MS);
        record.updated_at_ms = now;
        Ok(Some(record.clone()))
    })
}

/// Atomically claim the next eligible record. Expired claims are returned to
/// the queue before selection so a restarted worker can continue after a crash.
pub fn claim_next(runner_id: &str, owner: &str) -> Result<Option<DeferredWorkload>> {
    claim_next_at(runner_id, owner, now_ms())
}

/// Claim the next record using the supplied clock. The worker uses this seam to
/// make lease recovery deterministic without changing the durable protocol.
pub fn claim_next_at(runner_id: &str, owner: &str, now: u64) -> Result<Option<DeferredWorkload>> {
    claim_next_matching_at(runner_id, owner, now, |_| true)
}

/// Claim the next deferred workload accepted by the selected runner. Records
/// that require a different runtime or capability remain deferred for a later
/// compatible runner.
pub fn claim_next_matching_at(
    runner_id: &str,
    owner: &str,
    now: u64,
    accepts: impl Fn(&DeferredWorkload) -> bool,
) -> Result<Option<DeferredWorkload>> {
    update(|records| {
        for record in records.iter_mut() {
            if record.state == DeferredWorkloadState::Claimed
                && record
                    .claim_expires_at_ms
                    .is_some_and(|expiry| expiry <= now)
            {
                record.state = DeferredWorkloadState::Deferred;
                record.runner_id = None;
                record.claim_owner = None;
                record.claim_expires_at_ms = None;
                record.updated_at_ms = now;
            }
        }
        let Some(record) = records
            .iter_mut()
            .find(|record| record.state == DeferredWorkloadState::Deferred && accepts(record))
        else {
            return Ok(None);
        };
        record.state = DeferredWorkloadState::Claimed;
        record.runner_id = Some(runner_id.to_string());
        record.claim_owner = Some(owner.to_string());
        record.claim_expires_at_ms = Some(now + CLAIM_LEASE_MS);
        record.updated_at_ms = now;
        Ok(Some(record.clone()))
    })
}

pub fn heartbeat(id: &str, owner: &str) -> Result<bool> {
    update(|records| {
        let Some(record) = records.iter_mut().find(|record| record.id == id) else {
            return Ok(false);
        };
        if record.state != DeferredWorkloadState::Claimed
            || record.claim_owner.as_deref() != Some(owner)
        {
            return Ok(false);
        }
        let now = now_ms();
        record.claim_expires_at_ms = Some(now + CLAIM_LEASE_MS);
        record.updated_at_ms = now;
        Ok(true)
    })
}

pub fn terminalize(id: &str, succeeded: bool) -> Result<()> {
    update(|records| {
        if let Some(record) = records.iter_mut().find(|record| record.id == id) {
            record.state = if succeeded {
                DeferredWorkloadState::Dispatched
            } else {
                DeferredWorkloadState::Failed
            };
            record.updated_at_ms = now_ms();
            record.claim_expires_at_ms = None;
            record.claim_owner = None;
        }
        Ok(())
    })
}

/// Return a claimed workload to the queue when runner preflight discovers that
/// the selected runner no longer satisfies its persisted contract.
pub fn defer_claim(id: &str, owner: &str) -> Result<()> {
    update(|records| {
        if let Some(record) = records.iter_mut().find(|record| record.id == id) {
            if record.state == DeferredWorkloadState::Claimed
                && record.claim_owner.as_deref() == Some(owner)
            {
                record.state = DeferredWorkloadState::Deferred;
                record.runner_id = None;
                record.claim_owner = None;
                record.claim_expires_at_ms = None;
                record.updated_at_ms = now_ms();
            }
        }
        Ok(())
    })
}

pub fn records() -> Result<Vec<DeferredWorkload>> {
    read_store(&store_path()?)
}

pub fn worker_lock() -> Result<File> {
    let path = store_path()?.with_extension("worker.lock");
    OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some(format!("open {}", path.display())))
        })
}

pub fn worker_status() -> Result<Option<DeferredWorkloadWorkerStatus>> {
    let path = store_path()?.with_extension("worker-status.json");
    if !path.exists() {
        return Ok(None);
    }
    let bytes = fs::read(&path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    serde_json::from_slice(&bytes)
        .map(Some)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))
}

pub fn worker_is_live(status: &DeferredWorkloadWorkerStatus) -> bool {
    if matches!(status.state.as_str(), "idle" | "stopped") {
        return false;
    }
    if status.owner_token.is_empty() {
        return false;
    }
    let Ok(lock) = worker_lock() else {
        return false;
    };
    // A status file is advisory. The singleton lock is the authority.
    if lock.try_lock_exclusive().is_ok() {
        let _ = lock.unlock();
        return false;
    }
    worker_identity_is_live(
        status,
        crate::process::process_identity_state,
        |pid, token| {
            crate::process::pid_has_ownership_token(pid, "HOMEBOY_DEFERRED_WORKLOAD_OWNER", token)
                .unwrap_or(false)
        },
    )
}

fn worker_identity_is_live(
    status: &DeferredWorkloadWorkerStatus,
    inspect_process: impl FnOnce(u32, Option<u64>) -> crate::process::ProcessIdentityState,
    owns_token: impl FnOnce(u32, &str) -> bool,
) -> bool {
    if cfg!(target_os = "linux") && status.linux_starttime_ticks.is_none() {
        return false;
    }
    inspect_process(status.pid, status.linux_starttime_ticks)
        == crate::process::ProcessIdentityState::Live
        && owns_token(status.pid, &status.owner_token)
}

pub fn write_worker_status(
    owner_token: &str,
    state: &str,
    detail: impl Into<String>,
) -> Result<()> {
    let path = store_path()?.with_extension("worker-status.json");
    let value = DeferredWorkloadWorkerStatus {
        schema: "homeboy/deferred-workload-worker-status/v1".to_string(),
        pid: std::process::id(),
        owner_token: owner_token.to_string(),
        linux_starttime_ticks: crate::process::linux_process_starttime_ticks(std::process::id())
            .ok()
            .flatten(),
        state: state.to_string(),
        updated_at_ms: now_ms(),
        detail: detail.into(),
    };
    let bytes = serde_json::to_vec(&value).map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize deferred workload worker status".to_string()),
        )
    })?;
    write_store(&path, &bytes)
}

pub fn append_worker_log(message: impl AsRef<str>) -> Result<()> {
    let path = store_path()?.with_extension("worker.log");
    let line = format!("{} {}\n", now_ms(), message.as_ref());
    use std::io::Write;
    OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut file| file.write_all(line.as_bytes()))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("append {}", path.display())),
            )
        })
}

fn fingerprint(input: &DeferredWorkloadInput) -> Result<String> {
    let value = serde_json::to_vec(&serde_json::json!({
        "command_label": input.command_label,
        "args": input.args,
        "placement": input.placement,
        "resource_requirement": input.resource_requirement,
        "job_overrides": input.job_overrides,
    }))
    .map_err(|error| {
        Error::internal_json(
            error.to_string(),
            Some("serialize deferred workload".to_string()),
        )
    })?;
    Ok(sha256_hex(&value))
}

fn store_path() -> Result<PathBuf> {
    Ok(crate::paths::homeboy()?.join("deferred-workloads.json"))
}

fn update<T>(mutate: impl FnOnce(&mut Vec<DeferredWorkload>) -> Result<T>) -> Result<T> {
    let path = store_path()?;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create deferred workload store".to_string()),
            )
        })?;
    }
    let lock = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .open(path.with_extension("lock"))
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open deferred workload lock".to_string()),
            )
        })?;
    lock.lock_exclusive().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("lock deferred workload store".to_string()),
        )
    })?;
    let mut records = read_store(&path)?;
    let output = mutate(&mut records)?;
    let bytes = serde_json::to_vec(&serde_json::json!({ "schema": SCHEMA, "records": records }))
        .map_err(|error| {
            Error::internal_json(
                error.to_string(),
                Some("serialize deferred workload store".to_string()),
            )
        })?;
    write_store(&path, &bytes)?;
    let _ = lock.unlock();
    Ok(output)
}

fn read_store(path: &Path) -> Result<Vec<DeferredWorkload>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let bytes = fs::read(path).map_err(|error| {
        Error::internal_io(error.to_string(), Some(format!("read {}", path.display())))
    })?;
    let value: serde_json::Value = serde_json::from_slice(&bytes)
        .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))?;
    if value.get("schema").and_then(serde_json::Value::as_str) != Some(SCHEMA) {
        return Err(Error::validation_invalid_argument(
            "deferred_workload_store",
            "unrecognized deferred workload store schema",
            Some(path.display().to_string()),
            None,
        ));
    }
    serde_json::from_value(value.get("records").cloned().ok_or_else(|| {
        Error::validation_invalid_argument(
            "deferred_workload_store",
            "missing records",
            Some(path.display().to_string()),
            None,
        )
    })?)
    .map_err(|error| Error::config_invalid_json(path.display().to_string(), error))
}

fn write_store(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let temporary = parent.join(format!(
        ".{}.tmp-{}-{}",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("deferred-workloads.json"),
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::write(&temporary, bytes).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("write {}", temporary.display())),
        )
    })?;
    File::open(&temporary)
        .and_then(|file| file.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", temporary.display())),
            )
        })?;
    fs::rename(&temporary, path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("rename {}", temporary.display())),
        )
    })?;
    #[cfg(unix)]
    File::open(parent)
        .and_then(|directory| directory.sync_all())
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("sync {}", parent.display())),
            )
        })?;
    Ok(())
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input() -> DeferredWorkloadInput {
        DeferredWorkloadInput {
            command_label: "review test".to_string(),
            args: vec![
                "homeboy".to_string(),
                "review".to_string(),
                "test".to_string(),
            ],
            placement: "auto".to_string(),
            resource_requirement: "eligible_lab_runner".to_string(),
            portability: "portable_lab_route".to_string(),
            reason: "no ready runner".to_string(),
            ci_alternative: "run in CI".to_string(),
            resolved_contract: serde_json::json!({ "portability": "portable_lab_route" }),
            resolved_resources: serde_json::json!({ "severity": "warm" }),
            test_requirements: DeferredWorkloadRequirements {
                required_runtimes: ["homeboy".to_string()].into(),
                required_capabilities: ["review test".to_string()].into(),
            },
            job_overrides: crate::lab_offload::LabJobOverrides::default(),
        }
    }

    #[test]
    fn deferred_workload_is_idempotent_and_survives_restart_before_claim() {
        crate::test_support::with_isolated_home(|_| {
            let first = defer(input()).expect("defer workload");
            let replay = defer(input()).expect("replay deferred workload");
            assert_eq!(first.id, replay.id);
            assert_eq!(replay.state, DeferredWorkloadState::Deferred);

            let claimed = claim(&input(), "warm-lab", "first-owner")
                .expect("claim workload")
                .expect("pending workload");
            assert_eq!(claimed.id, first.id);
            assert_eq!(claimed.runner_id.as_deref(), Some("warm-lab"));
            assert!(claim(&input(), "other-lab", "second-owner")
                .expect("idempotent claim")
                .is_none());
        });
    }

    #[test]
    fn reading_an_absent_store_does_not_create_runtime_state() {
        crate::test_support::with_isolated_home(|_| {
            let path = store_path().expect("store path");

            assert!(records().expect("read absent store").is_empty());
            assert!(!path.exists(), "read created deferred workload store");
            assert!(
                !path.with_extension("lock").exists(),
                "read created deferred workload lock"
            );
        });
    }

    #[test]
    fn terminalized_workload_does_not_reappear_as_a_ghost() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = defer(input()).expect("defer workload");
            let claimed = claim(&input(), "warm-lab", "owner")
                .expect("claim workload")
                .expect("pending workload");
            terminalize(&claimed.id, true).expect("terminalize workload");
            assert!(claim(&input(), "warm-lab", "other-owner")
                .expect("claim after terminal state")
                .is_none());

            let next = defer(input()).expect("new explicit workload after terminal state");
            assert_ne!(
                next.id, deferred.id,
                "terminal work must not be revived by a replay"
            );
        });
    }

    #[test]
    fn expired_claim_is_reclaimed_after_a_post_claim_crash() {
        crate::test_support::with_isolated_home(|_| {
            defer(input()).expect("defer workload");
            let claimed = claim(&input(), "first-lab", "crashed-owner")
                .expect("claim workload")
                .expect("pending workload");
            update(|records| {
                let record = records
                    .iter_mut()
                    .find(|record| record.id == claimed.id)
                    .expect("claimed record");
                record.claim_expires_at_ms = Some(0);
                Ok(())
            })
            .expect("expire crashed claim");

            let recovered = claim(&input(), "warm-lab", "recovery-owner")
                .expect("reclaim workload")
                .expect("expired claim is reclaimable");
            assert_eq!(recovered.runner_id.as_deref(), Some("warm-lab"));
            assert_eq!(recovered.claim_owner.as_deref(), Some("recovery-owner"));
        });
    }

    #[test]
    fn next_claim_heartbeats_and_publishes_durable_worker_status() {
        crate::test_support::with_isolated_home(|_| {
            let deferred = defer(input()).expect("defer workload");
            let claimed = claim_next("ready-runner", "worker-a")
                .expect("claim next")
                .expect("deferred workload");
            assert_eq!(claimed.id, deferred.id);
            assert_eq!(claimed.runner_id.as_deref(), Some("ready-runner"));
            assert!(heartbeat(&claimed.id, "worker-a").expect("heartbeat"));
            assert!(!heartbeat(&claimed.id, "worker-b").expect("wrong worker heartbeat"));

            write_worker_status("test-owner", "dispatching", "replaying deferred workload")
                .expect("write worker status");
            let status = worker_status()
                .expect("read worker status")
                .expect("status exists");
            assert_eq!(status.state, "dispatching");
            assert_eq!(status.detail, "replaying deferred workload");
            assert_eq!(status.owner_token, "test-owner");
        });
    }

    #[test]
    fn matching_claim_skips_a_live_claimed_head_for_a_later_deferred_record() {
        crate::test_support::with_isolated_home(|_| {
            let first = defer(input()).expect("first workload");
            let mut later_input = input();
            later_input.args.push("later".to_string());
            let later = defer(later_input).expect("later workload");
            claim_next_at("other-runner", "other-worker", 1)
                .expect("claim first")
                .expect("first workload is claimed");

            let claimed =
                claim_next_matching_at("ready-runner", "worker", 2, |record| record.id == later.id)
                    .expect("claim matching workload")
                    .expect("later deferred workload is claimable");

            assert_eq!(claimed.id, later.id);
            assert_ne!(claimed.id, first.id);
        });
    }

    #[test]
    fn reused_pid_with_a_different_start_identity_is_not_live() {
        let status = DeferredWorkloadWorkerStatus {
            schema: "homeboy/deferred-workload-worker-status/v1".to_string(),
            pid: 42,
            owner_token: "worker-token".to_string(),
            linux_starttime_ticks: Some(100),
            state: "dispatching".to_string(),
            updated_at_ms: 0,
            detail: String::new(),
        };

        assert!(!worker_identity_is_live(
            &status,
            |_, _| crate::process::ProcessIdentityState::IdentityMismatch,
            |_, _| true,
        ));
    }

    #[test]
    fn corrupt_store_fails_closed_without_resetting_records() {
        crate::test_support::with_isolated_home(|_| {
            let path = store_path().expect("store path");
            std::fs::create_dir_all(path.parent().expect("store parent")).expect("create parent");
            std::fs::write(&path, b"not-json").expect("write corrupt store");
            assert!(defer(input()).is_err());
            assert_eq!(
                std::fs::read(&path).expect("corrupt bytes remain"),
                b"not-json"
            );
        });
    }

    #[test]
    fn refuses_inline_values_for_runner_secret_identities() {
        crate::test_support::with_isolated_home(|_| {
            let mut input = input();
            input.job_overrides.env.insert(
                "DB_SERVICE_PASSWORD".to_string(),
                "fixture-password".to_string(),
            );
            input.job_overrides.secret_env_names = vec!["DB_SERVICE_PASSWORD".to_string()];

            assert!(defer(input).is_err());
            assert!(records().expect("records").is_empty());
        });
    }
}
