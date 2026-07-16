//! Immutable controller executable provenance for durable orchestration work.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use crate::{build_identity, paths, Error, Result};

pub(crate) const CONTROLLER_RUNTIME_METADATA_KEY: &str = "controller_runtime";

const ACTIVE_GENERATION_FILE: &str = "active.json";
const ADMISSION_LOCK_DIR: &str = "admission.lock";

/// Report-only retention inventory for immutable controller runtime pins.
///
/// No current cleanup command deletes controller runtime pins. This report is
/// intentionally the eligibility primitive for a future narrowly-scoped pruner;
/// callers must retain every path in `retained` and may consider only `eligible`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerRuntimeRetentionReport {
    pub retained: Vec<PathBuf>,
    pub eligible: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ControllerRuntimePruneResult {
    pub retained: Vec<PathBuf>,
    pub eligible: Vec<PathBuf>,
    pub removed: Vec<PathBuf>,
}

/// Discover pin references through the durable lifecycle store and classify the
/// content-addressed pins currently present on disk. Queued, running, and
/// recoverable partial records retain their pins because lifecycle recovery can
/// still operate on them. The active admission generation is retained as well.
pub fn retention_report() -> Result<ControllerRuntimeRetentionReport> {
    let root = runtime_root()?;
    let (records, _) = crate::agent_task_lifecycle::list_records_with_health()?;
    let mut retained = BTreeSet::new();

    for record in records {
        if !state_retains_pin(record.state) {
            continue;
        }
        if let Some(path) = record
            .metadata
            .pointer(&format!(
                "/{CONTROLLER_RUNTIME_METADATA_KEY}/originating/pinned_executable"
            ))
            .and_then(Value::as_str)
            .map(PathBuf::from)
            .filter(|path| content_addressed_pin_path(&root, path))
        {
            retained.insert(path);
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
    Ok(ControllerRuntimeRetentionReport {
        retained: retained.into_iter().collect(),
        eligible,
    })
}

/// Remove only content-addressed pins not referenced by a nonterminal durable
/// record or the active generation. The caller chooses mutation explicitly.
pub fn prune_pins(apply: bool) -> Result<ControllerRuntimePruneResult> {
    let report = retention_report()?;
    let mut removed = Vec::new();
    if apply {
        for path in &report.eligible {
            fs::remove_file(path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("prune unreferenced controller runtime pin".to_string()),
                )
            })?;
            removed.push(path.clone());
        }
    }
    Ok(ControllerRuntimePruneResult {
        retained: report.retained,
        eligible: report.eligible,
        removed,
    })
}

fn state_retains_pin(state: crate::agent_task_lifecycle::AgentTaskRunState) -> bool {
    matches!(
        state,
        crate::agent_task_lifecycle::AgentTaskRunState::Queued
            | crate::agent_task_lifecycle::AgentTaskRunState::Running
            | crate::agent_task_lifecycle::AgentTaskRunState::CandidateRecoverable
            | crate::agent_task_lifecycle::AgentTaskRunState::PartialRecoverable
            | crate::agent_task_lifecycle::AgentTaskRunState::PartialFailure
    )
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
pub(crate) struct RuntimeAdmission {
    lock_path: PathBuf,
    pub runtime: Value,
}

impl Drop for RuntimeAdmission {
    fn drop(&mut self) {
        let _ = fs::remove_dir(&self.lock_path);
    }
}

pub(crate) fn pin_current() -> Result<Value> {
    let identity = build_identity::current();
    let executable = std::env::current_exe().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("resolve controller executable".to_string()),
        )
    })?;
    pin_executable(&executable, &identity.display)
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
pub(crate) fn admit_current() -> Result<RuntimeAdmission> {
    let root = runtime_root()?;
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    acquire_admission_lock(&lock_path)?;
    let runtime = pin_current()?;
    write_active_generation(&root.join(ACTIVE_GENERATION_FILE), &runtime)?;
    if let Err(error) = validate_pin(&runtime) {
        let _ = fs::remove_dir(&lock_path);
        return Err(error);
    }
    Ok(RuntimeAdmission { lock_path, runtime })
}

/// Publish the current executable as the generation selected for future
/// admissions. Existing records retain their own pinned runtime metadata.
pub(crate) fn activate_current_generation() -> Result<Value> {
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
pub(crate) fn activate_installed_generation(executable: &Path) -> Result<Value> {
    let root = runtime_root()?;
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    acquire_admission_lock(&lock_path)?;
    let result = (|| {
        let runtime = pin_executable(executable, &activated_executable_identity(executable)?)?;
        validate_pin(&runtime)?;
        write_active_generation(&root.join(ACTIVE_GENERATION_FILE), &runtime)?;
        Ok(runtime)
    })();
    let _ = fs::remove_dir(lock_path);
    result
}

pub(crate) fn pinned_executable_for_mutation(
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

pub(crate) fn validate_for_mutation(metadata: &Value, current_identity: &str) -> Result<()> {
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

/// Move a valid legacy v2 pin into its immutable content-addressed location.
/// The caller persists the returned metadata only after this has completed.
pub(crate) fn migrate_legacy_pin(runtime: &Value) -> Result<Value> {
    let identity =
        required_runtime_string(runtime, "/originating/build_identity", "build identity")?;
    let digest = required_runtime_string(runtime, "/originating/sha256", "content digest")?;
    let destination = pinned_path(identity, digest)?;
    let current = required_runtime_string(
        runtime,
        "/originating/pinned_executable",
        "immutable executable",
    )?;
    if Path::new(current) == destination {
        validate_pin(runtime)?;
        return Ok(runtime.clone());
    }

    // Validation includes the digest, executable bit, and advertised identity.
    // Never update durable metadata until the no-clobber publication succeeds.
    validate_pin(runtime)?;
    publish_pin(Path::new(current), &destination, digest)?;
    let mut migrated = runtime.clone();
    migrated["originating"]["pinned_executable"] = json!(destination);
    validate_pin(&migrated)?;
    Ok(migrated)
}

pub(crate) fn validate(runtime: &Value) -> Result<()> {
    validate_pin(runtime)
}

/// Restore a missing or corrupted pin from one explicitly supplied trusted
/// artifact or source checkout without changing the durable identity or digest
/// contract.
pub(crate) fn recover_pin(
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
    let build = Command::new("cargo")
        .args(["build", "--release", "--bin", "homeboy"])
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
    let built = checkout.join("target/release/homeboy");
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

fn acquire_admission_lock(path: &Path) -> Result<()> {
    for _ in 0..500 {
        match fs::create_dir(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                std::thread::sleep(std::time::Duration::from_millis(10))
            }
            Err(error) => {
                return Err(Error::internal_io(
                    error.to_string(),
                    Some("acquire controller admission lock".to_string()),
                ))
            }
        }
    }
    Err(Error::validation_invalid_argument(
        "controller_admission",
        "timed out waiting for controller generation admission",
        None,
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

fn executable_identity(path: &Path) -> Result<String> {
    #[cfg(test)]
    if std::env::current_exe().ok().is_some_and(|current| {
        executable_digest(&current)
            .ok()
            .zip(executable_digest(path).ok())
            .is_some_and(|(current, candidate)| current == candidate)
    }) {
        // Unit tests run inside the libtest executable, not the Homeboy CLI.
        // Pins are byte-identical copies, so accept them without recursively
        // launching the test harness. Explicit fake controllers still execute.
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
                "#!/bin/sh\n# {marker}\nif [ \"$1\" = self ] && [ \"$2\" = identity ]; then\n  printf '%s\\n' '{{\"data\":{{\"display\":{identity}}}}}'\n  exit 0\nfi\nexit 1\n"
            ),
        )
        .expect("write fake controller");
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
            .expect("make fake controller executable");
        executable_digest(path).expect("hash fake controller")
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
    fn legacy_pin_migration_publishes_before_returning_updated_metadata() {
        crate::test_support::with_isolated_home(|_| {
            let temporary = tempfile::tempdir().expect("temporary runtime directory");
            let legacy = temporary.path().join("legacy-homeboy");
            let identity = "homeboy test+legacy";
            let digest = fake_controller(&legacy, identity, "legacy");
            let runtime = json!({ "originating": {
                "build_identity": identity,
                "pinned_executable": legacy,
                "sha256": digest,
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
}
