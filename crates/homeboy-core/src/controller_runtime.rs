//! Immutable controller executable provenance for durable orchestration work.

use fs4::fs_std::FileExt;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::fs::OpenOptions;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Mutex, MutexGuard, OnceLock};
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

use crate::{build_identity, paths, Error, Result};

pub const CONTROLLER_RUNTIME_METADATA_KEY: &str = "controller_runtime";
#[cfg(any(test, feature = "test-support"))]
pub(crate) const TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV: &str =
    "HOMEBOY_TEST_CONTROLLER_RUNTIME_EXECUTABLE";

const ACTIVE_GENERATION_FILE: &str = "active.json";
const ADMISSION_LOCK_DIR: &str = "admission.lock";
const ADMISSION_OWNER_SCHEMA: &str = "homeboy/controller-admission-owner/v1";
const ADMISSION_LOCK_ATTEMPTS: usize = 500;
const ADMISSION_LOCK_RETRY: Duration = Duration::from_millis(10);
static ADMISSION_LOCK_PROCESS_GUARDS: OnceLock<Mutex<BTreeMap<PathBuf, &'static Mutex<()>>>> =
    OnceLock::new();

/// Report-only retention inventory for immutable controller runtime pins.
///
/// No current cleanup command deletes controller runtime pins. This report is
/// intentionally the eligibility primitive for a future narrowly-scoped pruner;
/// callers must retain every path in `retained` and may consider only `eligible`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerRuntimeRetentionReport {
    pub retained: Vec<PathBuf>,
    pub eligible: Vec<PathBuf>,
    pub snapshots: Vec<ControllerRuntimeSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ControllerRuntimeSnapshot {
    pub identity: String,
    pub path: PathBuf,
    pub size_bytes: u64,
    pub age_seconds: u64,
    pub pins: Vec<PathBuf>,
    pub retention_reasons: Vec<String>,
    pub eligible: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ControllerRuntimeCleanupOptions {
    pub apply: bool,
    pub min_age: Duration,
    pub max_total_bytes: u64,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ControllerRuntimePruneResult {
    pub retained: Vec<PathBuf>,
    pub eligible: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
    pub removed_identities: Vec<PathBuf>,
    pub reclaimed_bytes: u64,
    pub snapshots: Vec<ControllerRuntimeSnapshot>,
}

/// Discover pin references through the durable lifecycle store and classify the
/// content-addressed pins currently present on disk. Queued, running, and
/// recoverable partial records retain their pins because lifecycle recovery can
/// still operate on them. The active admission generation is retained as well.
pub fn retention_report() -> Result<ControllerRuntimeRetentionReport> {
    let referenced = crate::controller_pin_reference::referenced_controller_pins()?;
    retention_report_with_references_at(&referenced, SystemTime::now())
}

fn retention_report_with_references_at(
    referenced: &[PathBuf],
    now: SystemTime,
) -> Result<ControllerRuntimeRetentionReport> {
    let root = runtime_root()?;
    let mut retained = BTreeSet::new();

    for path in referenced {
        if content_addressed_pin_path(&root, &path) {
            retained.insert(path.clone());
        }
    }

    let active = root.join(ACTIVE_GENERATION_FILE);
    if active.exists() {
        let value = fs::read_to_string(&active).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read active controller generation".to_string()),
            )
        })?;
        let runtime: Value = serde_json::from_str(&value).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("parse active controller generation".to_string()),
                None,
            )
        })?;
        if let Some(path) = runtime
            .pointer("/originating/pinned_executable")
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| content_addressed_pin_path(&root, path))
        {
            retained.insert(path);
        }
    }

    let pins = discover_pin_paths(&root)?;
    let eligible = pins.difference(&retained).cloned().collect();
    let mut snapshots = Vec::new();
    for entry in fs::read_dir(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("list controller runtime identities".to_string()),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read controller runtime identity".to_string()),
            )
        })?;
        let path = entry.path();
        if !path.is_dir()
            || path
                .file_name()
                .is_some_and(|name| name == ADMISSION_LOCK_DIR)
        {
            continue;
        }
        let identity_pins = pins
            .iter()
            .filter(|pin| pin.starts_with(&path))
            .cloned()
            .collect::<Vec<_>>();
        if identity_pins.is_empty() {
            continue;
        }
        let mut reasons = Vec::new();
        if identity_pins.iter().any(|pin| retained.contains(pin)) {
            reasons.push("pinned_by_active_or_resumable_run_or_current_generation".to_string());
        }
        let modified = fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(now);
        let age_seconds = now.duration_since(modified).unwrap_or_default().as_secs();
        snapshots.push(ControllerRuntimeSnapshot {
            identity: path
                .file_name()
                .unwrap_or_default()
                .to_string_lossy()
                .to_string(),
            size_bytes: path_size(&path),
            age_seconds,
            pins: identity_pins,
            eligible: reasons.is_empty(),
            retention_reasons: reasons,
            path,
        });
    }
    snapshots.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(ControllerRuntimeRetentionReport {
        retained: retained.into_iter().collect(),
        eligible,
        snapshots,
    })
}

/// Remove only content-addressed pins not referenced by a nonterminal durable
/// record or the active generation. The caller chooses mutation explicitly.
pub fn prune_pins(apply: bool) -> Result<ControllerRuntimePruneResult> {
    let result = cleanup(ControllerRuntimeCleanupOptions {
        apply,
        min_age: Duration::ZERO,
        max_total_bytes: 0,
        limit: usize::MAX,
    })?;
    Ok(ControllerRuntimePruneResult {
        retained: result.retained,
        eligible: result.eligible,
        removed: result.removed,
        removed_identities: result.removed_identities,
        reclaimed_bytes: result.reclaimed_bytes,
        snapshots: result.snapshots,
    })
}

/// Inventory and reclaim immutable runtime identities. The admission lock makes
/// the final reachability check atomic with activation and materialization.
pub fn cleanup(options: ControllerRuntimeCleanupOptions) -> Result<ControllerRuntimePruneResult> {
    let root = runtime_root()?;
    // Lifecycle inventory may migrate legacy records, which itself needs the
    // admission lock. Collect reachability before taking the runtime lock.
    let referenced = crate::controller_pin_reference::referenced_controller_pins()?;
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    if options.apply {
        recover_cleanup_tombstones(&root)?;
    }
    let mut report = retention_report_with_references_at(&referenced, SystemTime::now())?;
    let mut total = report
        .snapshots
        .iter()
        .map(|snapshot| snapshot.size_bytes)
        .sum::<u64>();
    let mut candidates = report
        .snapshots
        .iter_mut()
        .filter(|snapshot| snapshot.eligible)
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .age_seconds
            .cmp(&left.age_seconds)
            .then_with(|| right.size_bytes.cmp(&left.size_bytes))
    });
    let mut removed = Vec::new();
    let mut removed_identities = Vec::new();
    let mut reclaimed_bytes: u64 = 0;
    for snapshot in candidates {
        let expired = snapshot.age_seconds >= options.min_age.as_secs();
        let pressured = total > options.max_total_bytes;
        if !(expired || pressured) {
            snapshot
                .retention_reasons
                .push("within_age_and_size_budget".to_string());
            continue;
        }
        if removed.len() >= options.limit {
            snapshot
                .retention_reasons
                .push("cleanup_limit_reached".to_string());
            continue;
        }
        if options.apply {
            // Rename first: an interrupted cleanup leaves a non-discoverable
            // tombstone rather than a partially materialized identity.
            let tombstone = root.join(format!(".cleanup-{}", Uuid::new_v4()));
            fs::rename(&snapshot.path, &tombstone).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("stage controller runtime identity cleanup".to_string()),
                )
            })?;
            fs::remove_dir_all(&tombstone).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("remove controller runtime identity".to_string()),
                )
            })?;
            removed.extend(snapshot.pins.clone());
            removed_identities.push(snapshot.path.clone());
            reclaimed_bytes = reclaimed_bytes.saturating_add(snapshot.size_bytes);
            total = total.saturating_sub(snapshot.size_bytes);
        }
    }
    let removed_set = removed.iter().collect::<BTreeSet<_>>();
    report.eligible.retain(|path| !removed_set.contains(path));
    Ok(ControllerRuntimePruneResult {
        retained: report.retained,
        eligible: report.eligible,
        removed,
        removed_identities,
        reclaimed_bytes,
        snapshots: report.snapshots,
    })
}

fn recover_cleanup_tombstones(root: &Path) -> Result<()> {
    for entry in fs::read_dir(root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("list interrupted controller runtime cleanup".to_string()),
        )
    })? {
        let path = entry
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("read interrupted controller runtime cleanup".to_string()),
                )
            })?
            .path();
        if path.is_dir()
            && path
                .file_name()
                .is_some_and(|name| name.to_string_lossy().starts_with(".cleanup-"))
        {
            fs::remove_dir_all(path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("complete interrupted controller runtime cleanup".to_string()),
                )
            })?;
        }
    }
    Ok(())
}

fn path_size(path: &Path) -> u64 {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return 0;
    };
    if metadata.is_file() {
        return metadata.len();
    }
    fs::read_dir(path)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok())
        .map(|entry| path_size(&entry.path()))
        .sum()
}

fn content_addressed_pin_path(root: &Path, path: &Path) -> bool {
    let Ok(relative) = path.strip_prefix(root) else {
        return false;
    };
    let components = relative.components().collect::<Vec<_>>();
    let primary_pin = matches!(
        components.as_slice(),
        [generation, executable]
            if generation.as_os_str() != "active.json" && executable.as_os_str() == "homeboy"
    );
    let recovered_pin = matches!(
        components.as_slice(),
        [generation, recovery, executable]
            if generation.as_os_str() != "active.json"
                && recovery.as_os_str().to_string_lossy().starts_with("recovery-")
                && executable.as_os_str() == "homeboy"
    );
    primary_pin || recovered_pin
}

fn discover_pin_paths(root: &Path) -> Result<BTreeSet<PathBuf>> {
    let mut pins = BTreeSet::new();
    for entry in fs::read_dir(root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("list controller runtime pins".to_string()),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read controller runtime pin".to_string()),
            )
        })?;
        let path = entry.path();
        if path
            .file_name()
            .is_some_and(|name| name.to_string_lossy().starts_with(".cleanup-"))
        {
            continue;
        }
        let direct_pin = path.join("homeboy");
        if content_addressed_pin_path(root, &direct_pin) && direct_pin.is_file() {
            pins.insert(direct_pin);
        }
        if !path.is_dir() {
            continue;
        }
        for recovery in fs::read_dir(&path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("list recovered controller runtime pins".to_string()),
            )
        })? {
            let recovery = recovery.map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("read recovered controller runtime pin".to_string()),
                )
            })?;
            let recovered_pin = recovery.path().join("homeboy");
            if content_addressed_pin_path(root, &recovered_pin) && recovered_pin.is_file() {
                pins.insert(recovered_pin);
            }
        }
    }
    Ok(pins)
}

/// Holds the short admission critical section.  Keeping selection and durable
/// record creation together prevents a submission from observing A after B is
/// published.
pub struct RuntimeAdmission {
    _lock: AdmissionLock,
    pub runtime: Value,
}

#[derive(Debug)]
struct AdmissionLock {
    path: PathBuf,
    token: String,
    _process_guard: MutexGuard<'static, ()>,
    file: fs::File,
}

impl Drop for AdmissionLock {
    fn drop(&mut self) {
        // The advisory lock serializes record updates, so a guard only clears
        // the owner record that it published. The file remains as the durable
        // coordination inode; deleting it would permit a second inode/lock.
        if admission_owner_token(&self.path).as_deref() == Some(self.token.as_str()) {
            let _ = fs::write(&self.path, b"");
        }
        let _ = self.file.unlock();
    }
}

pub fn pin_current() -> Result<Value> {
    let root = runtime_root()?;
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    pin_current_unlocked()
}

fn pin_current_unlocked() -> Result<Value> {
    let identity = build_identity::current();
    let executable = current_executable()?;
    pin_executable(&executable, &identity.display)
}

fn current_executable() -> Result<PathBuf> {
    #[cfg(any(test, feature = "test-support"))]
    if let Some(executable) = std::env::var_os(TEST_CONTROLLER_RUNTIME_EXECUTABLE_ENV) {
        return Ok(PathBuf::from(executable));
    }

    std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve controller executable".to_string()),
        )
    })
}

fn pin_executable(executable: &Path, identity: &str) -> Result<Value> {
    let digest = executable_digest(executable)?;
    let pinned_path = pinned_path(identity, &digest)?;
    publish_pin(executable, &pinned_path, &digest)?;

    let runtime = runtime_pin(&identity, executable, &pinned_path, &digest);
    validate_pin(&runtime)?;
    Ok(runtime)
}

fn runtime_pin(identity: &str, executable: &Path, pinned_path: &Path, digest: &str) -> Value {
    json!({
        "schema": "homeboy/controller-runtime-pin/v2",
        "requested": identity,
        "originating": {
            "build_identity": identity,
            "executable": executable,
            "pinned_executable": pinned_path,
            "sha256": digest,
            "source": source_provenance(),
        },
        "current": identity,
        "executed": identity,
    })
}

/// Pin the process submitting a durable run while serializing admission. The
/// active-generation pointer is diagnostic state only: every fresh run must
/// retain the executable that created it, rather than inherit a previous
/// controller's selection.
pub fn admit_current() -> Result<RuntimeAdmission> {
    let root = runtime_root()?;
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    let lock = acquire_admission_lock(&lock_path)?;
    let runtime = pin_current_unlocked()?;
    write_active_generation(&root.join(ACTIVE_GENERATION_FILE), &runtime)?;
    validate_pin(&runtime)?;
    Ok(RuntimeAdmission {
        _lock: lock,
        runtime,
    })
}

/// Publish the current executable as the generation selected for future
/// admissions. Existing records retain their own pinned runtime metadata.
pub fn activate_current_generation() -> Result<Value> {
    let executable = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve controller executable".to_string()),
        )
    })?;
    activate_installed_generation(&executable)
}

/// Publish the executable that installation just verified. This intentionally
/// does not use the upgrading process's executable: after an on-disk swap that
/// process can still be running the previous generation.
pub fn activate_installed_generation(executable: &Path) -> Result<Value> {
    let root = runtime_root()?;
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    let _lock = acquire_admission_lock(&lock_path)?;
    let runtime = pin_executable(executable, &activated_executable_identity(executable)?)?;
    validate_pin(&runtime)?;
    write_active_generation(&root.join(ACTIVE_GENERATION_FILE), &runtime)?;
    Ok(runtime)
}

pub fn pinned_executable_for_mutation(
    metadata: &Value,
    current_identity: &str,
) -> Result<Option<PathBuf>> {
    let Some(runtime) = metadata.get(CONTROLLER_RUNTIME_METADATA_KEY) else {
        return Ok(None);
    };
    validate_pin(runtime)?;
    let originating = runtime
        .pointer("/originating/build_identity")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if originating.is_empty() || originating == current_identity {
        return Ok(None);
    }
    let pinned = runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .unwrap_or("<pinned-controller-runtime>");
    Ok(Some(PathBuf::from(pinned)))
}

pub fn validate_for_mutation(metadata: &Value, current_identity: &str) -> Result<()> {
    let Some(pinned) = pinned_executable_for_mutation(metadata, current_identity)? else {
        return Ok(());
    };
    let originating = metadata
        .get(CONTROLLER_RUNTIME_METADATA_KEY)
        .and_then(|runtime| runtime.pointer("/originating/build_identity"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    Err(Error::validation_invalid_argument(
        "controller_runtime",
        format!(
            "durable run was created by controller runtime `{originating}`, but this command is `{current_identity}`"
        ),
        Some(current_identity.to_string()),
        Some(vec![format!(
            "Run the lifecycle mutation through the pinned compatible runtime: {} <original homeboy arguments>",
            pinned.display()
        )]),
    ))
}

/// Upgrade a legacy pin into the immutable content-addressed v2 format.
/// The caller persists the returned metadata only after this has completed.
pub fn migrate_legacy_pin(runtime: &Value) -> Result<Value> {
    let root = runtime_root()?;
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    migrate_legacy_pin_unlocked(runtime)
}

/// Publish a migrated pin and persist its durable reference while the admission
/// lock remains held, so cleanup cannot reclaim the new identity in between.
pub fn migrate_legacy_pin_and_persist(
    runtime: &Value,
    persist: impl FnOnce(&Value) -> Result<()>,
) -> Result<Value> {
    let root = runtime_root()?;
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    let migrated = migrate_legacy_pin_unlocked(runtime)?;
    if &migrated != runtime {
        persist(&migrated)?;
    }
    Ok(migrated)
}

fn migrate_legacy_pin_unlocked(runtime: &Value) -> Result<Value> {
    let identity =
        required_runtime_string(runtime, "/originating/build_identity", "build identity")?;
    let current = required_runtime_string(
        runtime,
        "/originating/pinned_executable",
        "immutable executable",
    )?;
    let current = Path::new(current);

    // v1 pins predate a content digest. The retained executable is the only
    // trusted migration source; never substitute the current binary or a checkout.
    if runtime.pointer("/originating/sha256").is_none() {
        verify_executable(current, "legacy controller runtime")?;
        verify_self_status_identity(current, identity)?;
        let digest = executable_digest(current)?;
        let destination = pinned_path(identity, &digest)?;
        publish_pin(current, &destination, &digest)?;

        let mut migrated = runtime.clone();
        migrated["schema"] = json!("homeboy/controller-runtime-pin/v2");
        migrated["originating"]["sha256"] = json!(digest);
        migrated["originating"]["pinned_executable"] = json!(destination);
        for field in ["requested", "current", "executed"] {
            if migrated.get(field).is_none() || migrated[field].is_null() {
                migrated[field] = json!(identity);
            }
        }
        validate_pin(&migrated)?;
        return Ok(migrated);
    }

    let digest = required_runtime_string(runtime, "/originating/sha256", "content digest")?;
    let destination = pinned_path(identity, digest)?;
    if current == destination {
        validate_pin(runtime)?;
        return Ok(runtime.clone());
    }

    // Validation includes the digest, executable bit, and advertised identity.
    // Never update durable metadata until the no-clobber publication succeeds.
    validate_pin(runtime)?;
    publish_pin(current, &destination, digest)?;
    let mut migrated = runtime.clone();
    migrated["originating"]["pinned_executable"] = json!(destination);
    validate_pin(&migrated)?;
    Ok(migrated)
}

pub fn validate(runtime: &Value) -> Result<()> {
    validate_pin(runtime)
}

/// Restore a missing or corrupted pin from one explicitly supplied trusted
/// artifact or source checkout without changing the durable identity or digest
/// contract.
pub fn recover_pin(
    runtime: &Value,
    artifact: Option<&Path>,
    source: Option<&Path>,
) -> Result<Value> {
    let root = runtime_root()?;
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    recover_pin_unlocked(runtime, artifact, source)
}

/// Publish a recovered pin and persist its durable reference under one
/// admission lock, closing the publication-to-record race with cleanup.
pub fn recover_pin_and_persist(
    runtime: &Value,
    artifact: Option<&Path>,
    source: Option<&Path>,
    persist: impl FnOnce(&Value) -> Result<()>,
) -> Result<Value> {
    let root = runtime_root()?;
    let _lock = acquire_admission_lock(&root.join(ADMISSION_LOCK_DIR))?;
    let recovered = recover_pin_unlocked(runtime, artifact, source)?;
    persist(&recovered)?;
    Ok(recovered)
}

fn recover_pin_unlocked(
    runtime: &Value,
    artifact: Option<&Path>,
    source: Option<&Path>,
) -> Result<Value> {
    let identity =
        required_runtime_string(runtime, "/originating/build_identity", "build identity")?;
    let expected = required_runtime_string(runtime, "/originating/sha256", "content digest")?;
    // Recovery never repairs an existing path in place. A corrupted canonical
    // path can still be referenced by another durable record, so this record
    // receives a distinct immutable snapshot after the artifact is verified.
    let destination = recovered_pinned_path(identity, expected)?;
    if let Some(artifact) = artifact {
        verify_artifact(artifact, expected, identity)?;
        publish_pin(artifact, &destination, expected)?;
        let mut recovered = runtime.clone();
        recovered["originating"]["pinned_executable"] = json!(destination);
        validate_pin(&recovered)?;
        return Ok(recovered);
    }
    let revision = runtime
        .pointer("/originating/source/revision")
        .and_then(Value::as_str)
        .map(str::to_string)
        .or_else(|| {
            identity
                .rsplit_once('+')
                .map(|(_, revision)| revision.to_string())
        })
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "controller runtime recovery needs recorded source revision",
                Some(identity.to_string()),
                None,
            )
        })?;
    let source = source.ok_or_else(|| {
        Error::validation_invalid_argument(
            "source",
            "controller runtime recovery requires --artifact or --source",
            None,
            None,
        )
    })?;
    let temporary = tempfile::tempdir().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime recovery workspace".to_string()),
        )
    })?;
    let checkout = temporary.path().join("source");
    run_command(
        "git",
        [
            "-C",
            &source.display().to_string(),
            "worktree",
            "add",
            "--detach",
            &checkout.display().to_string(),
            &revision,
        ],
    )?;
    let target =
        crate::cleanup::acquire_shared_cargo_target(&format!("controller-runtime:{revision}"))?;
    let build = Command::new("cargo")
        .args(["build", "--release", "--bin", "homeboy"])
        .env("CARGO_TARGET_DIR", target.target_dir())
        .current_dir(&checkout)
        .status()
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("build controller runtime recovery source".to_string()),
            )
        })?;
    if !build.success() {
        let _ = run_command(
            "git",
            [
                "-C",
                &source.display().to_string(),
                "worktree",
                "remove",
                "--force",
                &checkout.display().to_string(),
            ],
        );
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            "controller runtime recovery build failed",
            Some(identity.to_string()),
            None,
        ));
    }
    let built = target.target_dir().join("release/homeboy");
    let actual = executable_digest(&built)?;
    if actual != expected {
        let _ = run_command(
            "git",
            [
                "-C",
                &source.display().to_string(),
                "worktree",
                "remove",
                "--force",
                &checkout.display().to_string(),
            ],
        );
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "recovered controller runtime hash does not match durable pin: expected {expected}"
            ),
            Some(built.display().to_string()),
            None,
        ));
    }
    verify_artifact(&built, expected, identity)?;
    publish_pin(&built, &destination, expected)?;
    let _ = run_command(
        "git",
        [
            "-C",
            &source.display().to_string(),
            "worktree",
            "remove",
            "--force",
            &checkout.display().to_string(),
        ],
    );
    let mut recovered = runtime.clone();
    recovered["originating"]["pinned_executable"] = json!(destination);
    validate_pin(&recovered)?;
    Ok(recovered)
}

fn runtime_root() -> Result<PathBuf> {
    let root = paths::homeboy_data()?.join("controller-runtimes");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime directory".to_string()),
        )
    })?;
    Ok(root)
}

fn write_active_generation(path: &Path, runtime: &Value) -> Result<()> {
    let temporary = path.with_extension("tmp");
    fs::write(
        &temporary,
        serde_json::to_vec(runtime)
            .map_err(|error| Error::internal_json(error.to_string(), None))?,
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("write active controller generation".to_string()),
        )
    })?;
    fs::rename(temporary, path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("publish active controller generation".to_string()),
        )
    })
}

fn acquire_admission_lock(path: &Path) -> Result<AdmissionLock> {
    acquire_admission_lock_with_retry(path, ADMISSION_LOCK_ATTEMPTS, ADMISSION_LOCK_RETRY)
}

fn acquire_admission_lock_with_retry(
    path: &Path,
    attempts: usize,
    retry: Duration,
) -> Result<AdmissionLock> {
    reject_legacy_admission_lock(path)?;
    let started = Instant::now();
    let mut file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .open(path)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("open controller admission lock".to_string()),
            )
        })?;

    for _ in 0..attempts {
        let Some(process_guard) = try_acquire_admission_process_guard(path) else {
            std::thread::sleep(retry);
            continue;
        };
        let acquired = file.try_lock_exclusive().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("acquire controller admission lock".to_string()),
            )
        })?;

        if acquired {
            let token = Uuid::new_v4().to_string();
            let owner = admission_owner_record(&token);
            file.set_len(0).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("clear controller admission owner record".to_string()),
                )
            })?;
            file.write_all(&serde_json::to_vec(&owner).map_err(|error| {
                Error::internal_json(
                    error.to_string(),
                    Some("serialize controller admission owner".to_string()),
                )
            })?)
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("write controller admission owner record".to_string()),
                )
            })?;
            file.sync_data().map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("sync controller admission owner record".to_string()),
                )
            })?;
            return Ok(AdmissionLock {
                path: path.to_path_buf(),
                token,
                _process_guard: process_guard,
                file,
            });
        }
        std::thread::sleep(retry);
    }

    Err(Error::validation_invalid_argument(
        "controller_admission",
        format!(
            "controller generation admission timed out; waited {}ms; current owner: {}",
            started.elapsed().as_millis(),
            admission_owner_summary(path)
        ),
        None,
        None,
    ))
}

fn try_acquire_admission_process_guard(path: &Path) -> Option<MutexGuard<'static, ()>> {
    let guard = {
        let mut guards = ADMISSION_LOCK_PROCESS_GUARDS
            .get_or_init(|| Mutex::new(BTreeMap::new()))
            .lock()
            .expect("controller admission process guard registry is not poisoned");
        *guards
            .entry(path.to_path_buf())
            .or_insert_with(|| Box::leak(Box::new(Mutex::new(()))))
    };
    guard.try_lock().ok()
}

fn admission_owner_record(token: &str) -> Value {
    let pid = std::process::id();
    let starttime_ticks = crate::process::linux_process_starttime_ticks(pid)
        .ok()
        .flatten();
    json!({
        "schema": ADMISSION_OWNER_SCHEMA,
        "token": token,
        "pid": pid,
        "linux_starttime_ticks": starttime_ticks,
    })
}

fn admission_owner_token(path: &Path) -> Option<String> {
    serde_json::from_slice::<Value>(&fs::read(path).ok()?)
        .ok()?
        .get("token")?
        .as_str()
        .map(str::to_string)
}

fn admission_owner_summary(path: &Path) -> String {
    let Ok(owner) = serde_json::from_slice::<Value>(&fs::read(path).unwrap_or_default()) else {
        return "unavailable".to_string();
    };
    let pid = owner.get("pid").and_then(Value::as_u64);
    let token = owner.get("token").and_then(Value::as_str);
    let starttime = owner.get("linux_starttime_ticks").and_then(Value::as_u64);
    match (pid, token, starttime) {
        (Some(pid), Some(token), Some(starttime)) => {
            format!("pid={pid}, linux_starttime_ticks={starttime}, token={token}")
        }
        (Some(pid), Some(token), None) => format!("pid={pid}, token={token}"),
        _ => "unavailable".to_string(),
    }
}

fn reject_legacy_admission_lock(path: &Path) -> Result<()> {
    if !path.is_dir() {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "controller_admission",
        format!(
            "legacy controller admission lock directory exists at {}; it may be held by an older controller. Stop confirmed old controllers, then remove the abandoned directory explicitly before retrying",
            path.display()
        ),
        Some(path.display().to_string()),
        None,
    ))
}

fn validate_pin(runtime: &Value) -> Result<()> {
    let pinned = runtime
        .pointer("/originating/pinned_executable")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "controller runtime pin has no immutable executable",
                None,
                None,
            )
        })?;
    let expected = runtime
        .pointer("/originating/sha256")
        .and_then(Value::as_str)
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                "controller runtime pin has no content digest",
                Some(pinned.to_string()),
                None,
            )
        })?;
    let path = Path::new(pinned);
    let metadata = fs::metadata(path).map_err(|_| {
        Error::validation_invalid_argument(
            "controller_runtime",
            format!("pinned controller executable is missing: {pinned}"),
            Some(pinned.to_string()),
            None,
        )
    })?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("pinned controller executable is not executable: {pinned}"),
            Some(pinned.to_string()),
            None,
        ));
    }
    let actual = executable_digest(path)?;
    if actual != expected {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "pinned controller executable hash mismatch: expected {expected}, found {actual}"
            ),
            Some(pinned.to_string()),
            None,
        ));
    }
    let identity =
        required_runtime_string(runtime, "/originating/build_identity", "build identity")?;
    verify_self_identity(path, identity)?;
    Ok(())
}

fn verify_artifact(path: &Path, expected: &str, identity: &str) -> Result<()> {
    verify_executable(path, "recovery artifact")?;
    let actual = executable_digest(path)?;
    if actual != expected {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("recovery artifact hash mismatch: expected {expected}, found {actual}"),
            Some(path.display().to_string()),
            None,
        ));
    }
    verify_self_identity(path, identity)
}

fn verify_executable(path: &Path, label: &str) -> Result<()> {
    let metadata = fs::metadata(path).map_err(|_| {
        Error::validation_invalid_argument(
            "controller_runtime",
            format!("{label} is missing: {}", path.display()),
            Some(path.display().to_string()),
            None,
        )
    })?;
    if !metadata.is_file() || !is_executable(&metadata) {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("{label} is not executable: {}", path.display()),
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(())
}

fn verify_self_identity(path: &Path, expected: &str) -> Result<()> {
    let actual = executable_identity(path)?;
    if actual == expected {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "controller_runtime",
        format!(
            "pinned controller executable build identity mismatch: expected {expected}, found {actual}"
        ),
        Some(path.display().to_string()),
        None,
    ))
}

/// v1 records have no digest, so require the historical executable's full
/// status report to advertise the identity retained by the durable record.
fn verify_self_status_identity(path: &Path, expected: &str) -> Result<()> {
    let output = Command::new(path)
        .args(["self", "status"])
        .output()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "controller_runtime",
                format!("legacy controller runtime status check failed: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let actual = serde_json::from_str::<Value>(&stdout)
        .ok()
        .and_then(|value| {
            value
                .pointer("/data/active_build_identity/display")
                .or_else(|| value.pointer("/active_build_identity/display"))
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if !output.status.success() || actual.is_none() {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("legacy controller runtime status check returned invalid output: {stdout}"),
            Some(path.display().to_string()),
            None,
        ));
    }
    let actual = actual.expect("status identity was checked above");
    if actual == expected {
        return Ok(());
    }
    Err(Error::validation_invalid_argument(
        "controller_runtime",
        format!(
            "legacy controller runtime build identity mismatch: expected {expected}, found {actual}"
        ),
        Some(path.display().to_string()),
        None,
    ))
}

fn executable_identity(path: &Path) -> Result<String> {
    #[cfg(all(test, not(unix)))]
    if std::env::current_exe().ok().is_some_and(|current| {
        executable_digest(&current)
            .ok()
            .zip(executable_digest(path).ok())
            .is_some_and(|(current, candidate)| current == candidate)
    }) {
        // Non-Unix tests cannot use the shell executable fixture supplied by
        // test support. Pins are byte-identical copies, so avoid recursively
        // launching the test harness there. Explicit fake controllers execute.
        return Ok(build_identity::current().display);
    }
    let output = Command::new(path)
        .args(["self", "identity"])
        .output()
        .map_err(|error| {
            Error::validation_invalid_argument(
                "controller_runtime",
                format!("pinned controller executable identity check failed: {error}"),
                Some(path.display().to_string()),
                None,
            )
        })?;
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_string();
    let actual = serde_json::from_str::<Value>(&stdout)
        .ok()
        .and_then(|value| {
            value
                .pointer("/data/display")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
    if !output.status.success() || actual.is_none() {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "pinned controller executable identity check returned invalid output: {stdout}"
            ),
            Some(path.display().to_string()),
            None,
        ));
    }
    Ok(actual.expect("identity was checked above"))
}

fn activated_executable_identity(path: &Path) -> Result<String> {
    executable_identity(path)
}

fn executable_digest(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("hash pinned controller executable".to_string()),
        )
    })?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn make_executable_read_only(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o500)).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("seal controller runtime pin".to_string()),
            )
        })?;
    }
    Ok(())
}

fn is_executable(metadata: &fs::Metadata) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        true
    }
}

fn pinned_path(identity: &str, digest: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("controller-runtimes")
        .join(format!(
            "{}-{}",
            paths::sanitize_path_segment(identity),
            digest
        ))
        .join("homeboy"))
}

fn recovered_pinned_path(identity: &str, digest: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("controller-runtimes")
        .join(format!(
            "{}-{}",
            paths::sanitize_path_segment(identity),
            digest
        ))
        .join(format!("recovery-{}", uuid::Uuid::new_v4()))
        .join("homeboy"))
}

fn publish_pin(source: &Path, destination: &Path, expected_digest: &str) -> Result<()> {
    if destination.exists() {
        let actual = executable_digest(destination)?;
        if actual == expected_digest {
            return Ok(());
        }
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "immutable controller runtime path already contains different bytes: {}",
                destination.display()
            ),
            Some(destination.display().to_string()),
            None,
        ));
    }
    let parent = destination.parent().expect("pinned runtime has parent");
    fs::create_dir_all(parent).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("create controller runtime pin".to_string()),
        )
    })?;
    let staging = parent.join(format!(
        ".homeboy-{}-{}",
        std::process::id(),
        uuid::Uuid::new_v4()
    ));
    fs::copy(source, &staging).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("stage controller runtime pin".to_string()),
        )
    })?;
    let actual = executable_digest(&staging)?;
    if actual != expected_digest {
        let _ = fs::remove_file(&staging);
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!(
                "controller runtime source hash mismatch while publishing: expected {expected_digest}, found {actual}"
            ),
            Some(source.display().to_string()),
            None,
        ));
    }
    make_executable_read_only(&staging)?;
    match fs::hard_link(&staging, destination) {
        Ok(()) => {
            let _ = fs::remove_file(&staging);
            Ok(())
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let _ = fs::remove_file(&staging);
            let actual = executable_digest(destination)?;
            if actual == expected_digest {
                Ok(())
            } else {
                Err(Error::validation_invalid_argument(
                    "controller_runtime",
                    format!(
                        "immutable controller runtime path already contains different bytes: {}",
                        destination.display()
                    ),
                    Some(destination.display().to_string()),
                    None,
                ))
            }
        }
        Err(error) => {
            let _ = fs::remove_file(&staging);
            Err(Error::internal_io(
                error.to_string(),
                Some("publish controller runtime pin".to_string()),
            ))
        }
    }
}

fn required_runtime_string<'a>(runtime: &'a Value, pointer: &str, label: &str) -> Result<&'a str> {
    runtime
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "controller_runtime",
                format!("controller runtime pin has no {label}"),
                None,
                None,
            )
        })
}

fn source_provenance() -> Value {
    let cwd = std::env::current_dir().ok();
    let revision = cwd
        .as_ref()
        .and_then(|path| git_output(path, ["rev-parse", "HEAD"]));
    let repository = cwd
        .as_ref()
        .and_then(|path| git_output(path, ["config", "--get", "remote.origin.url"]));
    json!({ "revision": revision, "repository": repository, "verification": "observed_from_process_cwd" })
}

fn git_output(path: &Path, args: impl IntoIterator<Item = &'static str>) -> Option<String> {
    Command::new("git")
        .args(["-C", &path.display().to_string()])
        .args(args)
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn run_command<const N: usize>(program: &str, args: [&str; N]) -> Result<()> {
    let status = Command::new(program)
        .args(args)
        .status()
        .map_err(|error| Error::internal_io(error.to_string(), Some(format!("run {program}"))))?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("{program} command failed during runtime recovery"),
            None,
            None,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    fn fake_controller(path: &Path, identity: &str, marker: &str) -> String {
        use std::os::unix::fs::PermissionsExt;

        let identity = serde_json::to_string(identity).expect("serialize fake identity");
        fs::write(
            path,
            format!(
                "#!/bin/sh\n# {marker}\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nif [ \"$1\" = self ] && [ \"$2\" = status ]; then\n  printf '%s\\n' '{{\"data\":{{\"active_build_identity\":{{\"display\":{identity}}}}}}}'\n  exit 0\nfi\nexit 1\n"
            ),
        )
        .expect("write fake controller");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("make fake controller executable");
        executable_digest(path).expect("hash fake controller")
    }

    #[test]
    fn admission_lock_holder() {
        let Ok(path) = std::env::var("HOMEBOY_ADMISSION_LOCK_HELPER_PATH") else {
            return;
        };
        let ready = PathBuf::from(
            std::env::var("HOMEBOY_ADMISSION_LOCK_HELPER_READY").expect("helper ready path"),
        );
        let _guard = acquire_admission_lock_with_retry(Path::new(&path), 1, Duration::ZERO)
            .expect("helper admission guard");
        fs::write(&ready, b"ready").expect("signal helper readiness");
        if std::env::var_os("HOMEBOY_ADMISSION_LOCK_HELPER_EXIT").is_some() {
            std::process::exit(0);
        }
        let release = PathBuf::from(
            std::env::var("HOMEBOY_ADMISSION_LOCK_HELPER_RELEASE").expect("helper release path"),
        );
        for _ in 0..1_000 {
            if release.exists() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("admission lock helper was not released");
    }

    fn spawn_admission_lock_holder(
        path: &Path,
        temporary: &Path,
        exit_without_drop: bool,
    ) -> std::process::Child {
        let ready = temporary.join("ready");
        let release = temporary.join("release");
        let mut command = Command::new(std::env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "controller_runtime::tests::admission_lock_holder",
                "--nocapture",
            ])
            .env("HOMEBOY_ADMISSION_LOCK_HELPER_PATH", path)
            .env("HOMEBOY_ADMISSION_LOCK_HELPER_READY", &ready)
            .env("HOMEBOY_ADMISSION_LOCK_HELPER_RELEASE", &release);
        if exit_without_drop {
            command.env("HOMEBOY_ADMISSION_LOCK_HELPER_EXIT", "1");
        }
        let child = command.spawn().expect("spawn admission lock holder");
        for _ in 0..500 {
            if ready.exists() {
                return child;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("admission lock holder did not become ready");
    }

    fn release_admission_lock_holder(mut child: std::process::Child, temporary: &Path) {
        fs::write(temporary.join("release"), b"release").expect("release admission lock holder");
        assert!(child
            .wait()
            .expect("wait for admission lock holder")
            .success());
    }

    #[test]
    fn live_admission_guard_cannot_be_stolen() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let child = spawn_admission_lock_holder(&path, temporary.path(), false);

        let attempt = acquire_admission_lock_with_retry(&path, 2, Duration::ZERO);
        release_admission_lock_holder(child, temporary.path());
        let error = attempt.expect_err("live admission guard must remain exclusive");

        assert!(error.message.contains("admission timed out"));
        assert!(error.message.contains("pid="));
        assert!(error.message.contains("waited"));
    }

    #[test]
    fn legacy_admission_lock_fails_closed() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        fs::create_dir(&path).expect("create legacy lock directory");

        let error = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect_err("legacy directory lock must not be stolen");

        assert!(error
            .message
            .contains("remove the abandoned directory explicitly"));
        assert!(path.is_dir());
    }

    #[test]
    fn admission_lock_is_released_when_holder_exits_without_drop() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let mut child = spawn_admission_lock_holder(&path, temporary.path(), true);

        assert!(child
            .wait()
            .expect("wait for exiting lock holder")
            .success());
        acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect("kernel releases lock after holder exits");
    }

    #[test]
    fn admission_guard_releases_after_post_acquisition_failure() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let result: Result<()> = (|| {
            let _guard = acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)?;
            Err(Error::internal_unexpected("simulated pinning failure"))
        })();
        result.expect_err("simulated post-acquisition failure");

        acquire_admission_lock_with_retry(&path, 1, Duration::ZERO)
            .expect("next admission acquires released guard");
    }

    #[test]
    fn admission_timeout_reports_owner_and_wait_duration() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let path = temporary.path().join(ADMISSION_LOCK_DIR);
        let child = spawn_admission_lock_holder(&path, temporary.path(), false);

        let attempt = acquire_admission_lock_with_retry(&path, 3, Duration::from_millis(1));
        release_admission_lock_holder(child, temporary.path());
        let error = attempt.expect_err("second admission times out");

        assert!(error.message.contains("waited"));
        assert!(error.message.contains("pid="));
        assert!(error.message.contains("token="));
    }

    #[test]
    #[cfg(unix)]
    fn identity_mismatch_returns_pinned_runtime_recovery_command() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy-origin");
        let digest = fake_controller(&pinned, "homeboy 1.0.0+origin", "origin");
        make_executable_read_only(&pinned).expect("seal executable");
        let metadata = json!({
            "controller_runtime": {
                "originating": {
                    "build_identity": "homeboy 1.0.0+origin",
                    "pinned_executable": pinned,
                    "sha256": digest,
                }
            }
        });

        let error = validate_for_mutation(&metadata, "homeboy 1.0.0+replacement")
            .expect_err("replacement runtime must not mutate the originating lifecycle");

        assert!(error.message.contains("homeboy 1.0.0+origin"));
        assert!(error.details["tried"][0]
            .as_str()
            .is_some_and(|command| command.contains("homeboy-origin")));
    }

    #[test]
    #[cfg(unix)]
    fn identity_mismatch_resolves_the_verified_pinned_executable() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy-origin");
        let digest = fake_controller(&pinned, "homeboy 1.0.0+origin", "origin");
        make_executable_read_only(&pinned).expect("seal executable");
        let metadata = json!({
            "controller_runtime": {
                "originating": {
                    "build_identity": "homeboy 1.0.0+origin",
                    "pinned_executable": pinned,
                    "sha256": digest,
                }
            }
        });

        assert_eq!(
            pinned_executable_for_mutation(&metadata, "homeboy 1.0.0+replacement")
                .expect("verified pin")
                .as_deref(),
            Some(pinned.as_path())
        );
        assert!(
            pinned_executable_for_mutation(&metadata, "homeboy 1.0.0+origin")
                .expect("origin runtime")
                .is_none()
        );
    }

    #[test]
    fn altered_or_missing_pinned_executable_fails_closed() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy");
        fs::write(&pinned, b"generation-a").expect("write pinned executable");
        make_executable_read_only(&pinned).expect("seal executable");
        let runtime = json!({
            "originating": {
                "pinned_executable": pinned,
                "sha256": executable_digest(&pinned).expect("hash executable")
            }
        });
        fs::remove_file(
            runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .expect("path"),
        )
        .expect("remove pinned executable");
        assert!(validate_pin(&runtime).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn installed_generation_switch_publishes_b_and_retains_a_pin() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let generation_a = temporary.path().join("homeboy-a");
            let generation_b = temporary.path().join("homeboy-b");
            for (path, identity) in [
                (&generation_a, "homeboy 0.1.0+generation-a"),
                (&generation_b, "homeboy 0.1.0+generation-b"),
            ] {
                let identity = serde_json::to_string(identity).expect("serialize identity");
                fs::write(
                    path,
                    format!(
                        "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nexit 1\n"
                    ),
                )
                .expect("write generation executable");
                fs::set_permissions(path, fs::Permissions::from_mode(0o755))
                    .expect("make generation executable");
            }

            let runtime_a = activate_installed_generation(&generation_a)
                .expect("activate installed generation A");
            let runtime_b = activate_installed_generation(&generation_b)
                .expect("activate installed generation B");

            assert_eq!(
                runtime_a["originating"]["build_identity"],
                "homeboy 0.1.0+generation-a"
            );
            assert_eq!(
                runtime_b["originating"]["build_identity"],
                "homeboy 0.1.0+generation-b"
            );
            validate_pin(&runtime_a).expect("generation A pin remains valid");
            validate_pin(&runtime_b).expect("generation B pin is valid");

            let active: Value = serde_json::from_str(
                &fs::read_to_string(
                    runtime_root()
                        .expect("runtime root")
                        .join(ACTIVE_GENERATION_FILE),
                )
                .expect("read active generation"),
            )
            .expect("parse active generation");
            assert_eq!(
                active["originating"]["build_identity"],
                "homeboy 0.1.0+generation-b"
            );
        });
    }

    #[test]
    fn admission_replaces_a_stale_active_generation_with_the_submitting_runtime() {
        crate::test_support::with_isolated_home(|_| {
            let mut runtime_a = pin_current().expect("pin runtime A");
            runtime_a["originating"]["build_identity"] = json!("homeboy runtime-a");
            runtime_a["requested"] = json!("homeboy runtime-a");
            runtime_a["current"] = json!("homeboy runtime-a");
            runtime_a["executed"] = json!("homeboy runtime-a");
            let active = runtime_root()
                .expect("runtime root")
                .join(ACTIVE_GENERATION_FILE);
            write_active_generation(&active, &runtime_a).expect("write stale runtime A");

            let runtime_b = admit_current().expect("runtime B admission");
            let current = build_identity::current().display;

            assert_eq!(runtime_b.runtime["originating"]["build_identity"], current);
            assert_eq!(runtime_b.runtime["requested"], current);
            validate_for_mutation(
                &json!({ CONTROLLER_RUNTIME_METADATA_KEY: runtime_a }),
                &current,
            )
            .expect_err("runtime B must retain runtime A's immutable pin");
            validate_for_mutation(
                &json!({ CONTROLLER_RUNTIME_METADATA_KEY: runtime_b.runtime }),
                &current,
            )
            .expect("runtime B can mutate its fresh run");

            let active: Value = serde_json::from_str(
                &fs::read_to_string(active).expect("read refreshed active generation"),
            )
            .expect("parse refreshed active generation");
            assert_eq!(active["originating"]["build_identity"], current);
        });
    }

    #[cfg(unix)]
    #[test]
    fn pin_current_uses_the_explicit_test_controller_fixture() {
        crate::test_support::with_isolated_home(|_| {
            let runtime = pin_current().expect("pin explicit controller fixture");
            let pinned = runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .map(PathBuf::from)
                .expect("pinned executable");

            assert_ne!(
                pinned,
                std::env::current_exe().expect("current test executable")
            );
            assert_eq!(
                executable_identity(&pinned).expect("fixture identity"),
                build_identity::current().display
            );
        });
    }

    #[cfg(unix)]
    #[test]
    fn pinned_runtime_executes_original_controller_after_global_binary_replacement() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary executable directory");
            let global = temporary.path().join("homeboy");
            let write_controller = |identity: &str| {
                let identity = serde_json::to_string(identity).expect("serialize identity");
                fs::write(
                    &global,
                    format!(
                        "#!/bin/sh\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nif [ \"$1\" = controller ] && [ \"$2\" = admission ]; then\n  printf '%s\\n' {identity}\n  exit 0\nfi\nexit 1\n"
                    ),
                )
                .expect("write global controller");
                fs::set_permissions(&global, fs::Permissions::from_mode(0o755))
                    .expect("make global controller executable");
            };

            write_controller("homeboy 0.288.13+original");
            let runtime = pin_executable(&global, "homeboy 0.288.13+original")
                .expect("pin original controller");
            let pinned = runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .expect("pinned executable");

            // Simulate a concurrent global install after pin creation and before admission.
            write_controller("homeboy 0.288.13+replacement");
            let output = Command::new(pinned)
                .args(["controller", "admission"])
                .output()
                .expect("execute pinned controller admission");

            assert!(output.status.success());
            assert_eq!(
                String::from_utf8_lossy(&output.stdout).trim(),
                "homeboy 0.288.13+original"
            );
            assert_eq!(
                executable_identity(&global).expect("global replacement identity"),
                "homeboy 0.288.13+replacement"
            );
        });
    }

    #[test]
    fn publication_is_no_clobber_and_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("runtime/homeboy");
        fs::write(&source, b"generation-a").expect("write source");
        let digest = executable_digest(&source).expect("hash source");

        publish_pin(&source, &destination, &digest).expect("publish first pin");
        publish_pin(&source, &destination, &digest).expect("reuse exact pin");
        fs::write(&source, b"generation-b").expect("replace source");
        let error = publish_pin(
            &source,
            &destination,
            &executable_digest(&source).expect("hash replacement"),
        )
        .expect_err("different bytes must never replace a pin");

        assert!(error.message.contains("different bytes"));
        assert_eq!(fs::read(&destination).expect("read pin"), b"generation-a");
    }

    #[test]
    fn concurrent_publication_is_no_clobber_and_idempotent() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let source = temporary.path().join("source");
        let destination = temporary.path().join("runtime/homeboy");
        fs::write(&source, b"generation-a").expect("write source");
        let digest = executable_digest(&source).expect("hash source");

        std::thread::scope(|scope| {
            let mut publications = Vec::new();
            for _ in 0..8 {
                publications.push(scope.spawn(|| publish_pin(&source, &destination, &digest)));
            }
            for publication in publications {
                publication
                    .join()
                    .expect("publication thread completes")
                    .expect("concurrent publication succeeds");
            }
        });
        assert_eq!(fs::read(&destination).expect("read pin"), b"generation-a");
    }

    #[cfg(unix)]
    #[test]
    fn legacy_v1_pin_migration_publishes_before_returning_updated_metadata() {
        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary runtime directory");
            let legacy = temporary.path().join("legacy-homeboy");
            let identity = "homeboy test+legacy";
            fake_controller(&legacy, identity, "legacy");
            let runtime = json!({ "originating": {
                "build_identity": identity,
                "pinned_executable": legacy,
            }});

            let migrated = migrate_legacy_pin(&runtime).expect("migrate legacy pin");
            let destination = PathBuf::from(
                migrated["originating"]["pinned_executable"]
                    .as_str()
                    .expect("migrated path"),
            );
            assert_ne!(destination, legacy);
            assert!(legacy.exists());
            assert!(destination.is_file());
            assert_eq!(migrated["schema"], "homeboy/controller-runtime-pin/v2");
            assert_eq!(migrated["requested"], identity);
            assert_eq!(migrated["current"], identity);
            assert_eq!(migrated["executed"], identity);
            validate_pin(&migrated).expect("migrated pin validates");
        });
    }

    #[test]
    fn pin_diagnostics_distinguish_missing_and_hash_mismatch() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy");
        fs::write(&pinned, b"generation-a").expect("write pin");
        make_executable_read_only(&pinned).expect("seal pin");
        let runtime = json!({ "originating": { "pinned_executable": pinned, "sha256": "00" } });
        let mismatch = validate_pin(&runtime).expect_err("hash mismatch");
        assert!(mismatch.message.contains("hash mismatch"));
        fs::remove_file(
            runtime
                .pointer("/originating/pinned_executable")
                .and_then(Value::as_str)
                .expect("path"),
        )
        .expect("remove pin");
        let missing = validate_pin(&runtime).expect_err("missing pin");
        assert!(missing.message.contains("missing"));
    }

    #[cfg(unix)]
    #[test]
    fn artifact_diagnostics_distinguish_missing_non_executable_hash_and_identity() {
        use std::os::unix::fs::PermissionsExt;

        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let artifact = temporary.path().join("homeboy");
        let missing = verify_artifact(&artifact, "00", "homeboy test+one")
            .expect_err("missing artifact fails");
        assert!(missing.message.contains("missing"));

        fs::write(&artifact, b"not executable").expect("write artifact");
        let non_executable = verify_artifact(&artifact, "00", "homeboy test+one")
            .expect_err("non-executable artifact fails");
        assert!(non_executable.message.contains("not executable"));

        let digest = fake_controller(&artifact, "homeboy test+one", "artifact");
        let hash =
            verify_artifact(&artifact, "00", "homeboy test+one").expect_err("hash mismatch fails");
        assert!(hash.message.contains("hash mismatch"));
        let identity = verify_artifact(&artifact, &digest, "homeboy test+two")
            .expect_err("identity mismatch fails");
        assert!(identity.message.contains("build identity mismatch"));
        fs::set_permissions(&artifact, fs::Permissions::from_mode(0o500)).expect("seal artifact");
    }

    #[cfg(unix)]
    #[test]
    fn durable_pin_rejects_a_matching_hash_with_the_wrong_build_identity() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy");
        let digest = fake_controller(&pinned, "homeboy 0.288.4+older", "wrong identity");
        make_executable_read_only(&pinned).expect("seal executable");
        let runtime = json!({ "originating": {
            "build_identity": "homeboy 0.288.6+expected",
            "pinned_executable": pinned,
            "sha256": digest,
        }});

        let error =
            validate_pin(&runtime).expect_err("identity mismatch must fail after hash validation");

        assert!(error.message.contains("build identity mismatch"));
        assert!(error.message.contains("homeboy 0.288.6+expected"));
        assert!(error.message.contains("homeboy 0.288.4+older"));
    }

    #[cfg(unix)]
    #[test]
    fn recovery_preserves_runtime_a_after_generation_b_activation() {
        use std::os::unix::fs::PermissionsExt;

        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary controller directory");
            let artifact_a = temporary.path().join("homeboy-a");
            let artifact_b = temporary.path().join("homeboy-b");
            let identity_a = "homeboy test+runtime-a";
            let identity_b = "homeboy test+runtime-b";
            let digest_a = fake_controller(&artifact_a, identity_a, "runtime A");
            let digest_b = fake_controller(&artifact_b, identity_b, "runtime B");
            let pin_a = pinned_path(identity_a, &digest_a).expect("runtime A path");
            let pin_b = pinned_path(identity_b, &digest_b).expect("runtime B path");
            publish_pin(&artifact_a, &pin_a, &digest_a).expect("publish runtime A");
            let runtime_a = json!({ "originating": {
                "build_identity": identity_a,
                "pinned_executable": pin_a,
                "sha256": digest_a,
            }});
            validate_pin(&runtime_a).expect("runtime A validates before upgrade");
            let runtime_a_bytes = fs::read(&pin_a).expect("read runtime A");

            publish_pin(&artifact_b, &pin_b, &digest_b).expect("publish runtime B");
            let runtime_b = json!({ "originating": {
                "build_identity": identity_b,
                "pinned_executable": pin_b,
                "sha256": digest_b,
            }});
            write_active_generation(
                &runtime_root()
                    .expect("runtime root")
                    .join(ACTIVE_GENERATION_FILE),
                &runtime_b,
            )
            .expect("activate runtime B");
            assert_eq!(
                fs::read(&pin_a).expect("read runtime A after upgrade"),
                runtime_a_bytes
            );
            validate_pin(&runtime_a)
                .expect("runtime A remains executable after runtime B activation");

            fs::set_permissions(&pin_a, fs::Permissions::from_mode(0o700))
                .expect("allow test corruption");
            fs::write(&pin_a, b"corrupted runtime A").expect("corrupt runtime A");
            let error = validate_pin(&runtime_a).expect_err("corruption fails closed");
            assert!(error.message.contains("hash mismatch"));
            assert!(error.message.contains(&digest_a));

            let recovered = recover_pin(&runtime_a, Some(&artifact_a), None)
                .expect("recover runtime A from trusted artifact");
            let recovered_pin = PathBuf::from(
                recovered["originating"]["pinned_executable"]
                    .as_str()
                    .expect("recovered runtime A path"),
            );
            assert_ne!(recovered_pin, pin_a);
            assert_eq!(
                fs::read(&recovered_pin).expect("read recovered runtime A"),
                runtime_a_bytes
            );
            validate_pin(&recovered).expect("recovered runtime A validates");
        });
    }

    #[cfg(unix)]
    #[test]
    fn cleanup_preserves_active_generation_and_reclaims_unpinned_identity_under_size_pressure() {
        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary controller directory");
            let current = temporary.path().join("current");
            let stale = temporary.path().join("stale");
            let current_digest = fake_controller(&current, "homeboy test+current", "current");
            let stale_digest = fake_controller(&stale, "homeboy test+stale", "stale");
            let current_pin =
                pinned_path("homeboy test+current", &current_digest).expect("current path");
            let stale_pin = pinned_path("homeboy test+stale", &stale_digest).expect("stale path");
            publish_pin(&current, &current_pin, &current_digest).expect("publish current");
            publish_pin(&stale, &stale_pin, &stale_digest).expect("publish stale");
            write_active_generation(
                &runtime_root().expect("root").join(ACTIVE_GENERATION_FILE),
                &json!({ "originating": { "pinned_executable": current_pin } }),
            )
            .expect("activate current");

            let inventory = cleanup(ControllerRuntimeCleanupOptions {
                apply: false,
                min_age: Duration::from_secs(u64::MAX),
                max_total_bytes: 0,
                limit: 10,
            })
            .expect("inventory");
            assert!(inventory
                .snapshots
                .iter()
                .any(|snapshot| snapshot.pins.contains(&current_pin) && !snapshot.eligible));
            assert!(inventory
                .snapshots
                .iter()
                .any(|snapshot| snapshot.pins.contains(&stale_pin) && snapshot.eligible));
            let applied = cleanup(ControllerRuntimeCleanupOptions {
                apply: true,
                min_age: Duration::from_secs(u64::MAX),
                max_total_bytes: 0,
                limit: 10,
            })
            .expect("apply");
            assert!(applied.removed.contains(&stale_pin));
            assert!(current_pin.exists());
            assert!(!stale_pin.exists());
        });
    }

    #[test]
    fn cleanup_dry_run_preserves_tombstones_and_apply_recovers_them() {
        crate::test_support::with_isolated_home(|_| {
            let root = runtime_root().expect("runtime root");
            let tombstone = root.join(".cleanup-interrupted");
            fs::create_dir_all(&tombstone).expect("create tombstone");
            fs::write(tombstone.join("homeboy"), b"interrupted").expect("write tombstone");

            cleanup(ControllerRuntimeCleanupOptions {
                apply: false,
                min_age: Duration::ZERO,
                max_total_bytes: 0,
                limit: 1,
            })
            .expect("dry run");
            assert!(tombstone.exists());
            cleanup(ControllerRuntimeCleanupOptions {
                apply: true,
                min_age: Duration::ZERO,
                max_total_bytes: 0,
                limit: 1,
            })
            .expect("apply");
            assert!(!tombstone.exists());
        });
    }
}
