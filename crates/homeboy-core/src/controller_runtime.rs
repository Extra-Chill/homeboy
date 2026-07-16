//! Immutable controller executable provenance for durable orchestration work.

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};

use crate::{build_identity, paths, Error, Result};

pub(crate) const CONTROLLER_RUNTIME_METADATA_KEY: &str = "controller_runtime";

const ACTIVE_GENERATION_FILE: &str = "active.json";
const ADMISSION_LOCK_DIR: &str = "admission.lock";

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
    let pinned_path = pinned_path(&identity.display)?;
    if !pinned_path.exists() {
        let parent = pinned_path.parent().expect("pinned runtime has parent");
        std::fs::create_dir_all(parent).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create controller runtime pin".to_string()),
            )
        })?;
        std::fs::copy(&executable, &pinned_path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("pin controller executable".to_string()),
            )
        })?;
        make_executable_read_only(&pinned_path)?;
    }

    let digest = executable_digest(&pinned_path)?;

    Ok(json!({
        "schema": "homeboy/controller-runtime-pin/v2",
        "requested": identity.display,
        "originating": {
            "build_identity": identity.display,
            "executable": executable,
            "pinned_executable": pinned_path,
            "sha256": digest,
        },
        "current": identity.display,
        "executed": identity.display,
    }))
}

/// Select the durable active generation while serializing only admission.  The
/// first admission initializes it; subsequent admissions must use the already
/// selected immutable artifact rather than whichever binary happened to start
/// the submitting process.
pub(crate) fn admit_current() -> Result<RuntimeAdmission> {
    let root = runtime_root()?;
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    acquire_admission_lock(&lock_path)?;
    let active = root.join(ACTIVE_GENERATION_FILE);
    let runtime = if active.exists() {
        let value = fs::read_to_string(&active).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read active controller generation".to_string()),
            )
        })?;
        serde_json::from_str(&value).map_err(|error| {
            Error::validation_invalid_json(
                error,
                Some("parse active controller generation".to_string()),
                None,
            )
        })?
    } else {
        let runtime = pin_current()?;
        write_active_generation(&active, &runtime)?;
        runtime
    };
    if let Err(error) = validate_pin(&runtime) {
        let _ = fs::remove_dir(&lock_path);
        return Err(error);
    }
    Ok(RuntimeAdmission { lock_path, runtime })
}

/// Publish the current executable as the generation selected for future
/// admissions. Existing records retain their own pinned runtime metadata.
pub(crate) fn activate_current_generation() -> Result<Value> {
    let root = runtime_root()?;
    let lock_path = root.join(ADMISSION_LOCK_DIR);
    acquire_admission_lock(&lock_path)?;
    let result = (|| {
        let runtime = pin_current()?;
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
            format!("pinned controller executable is unavailable: {pinned}"),
            Some(pinned.to_string()),
            None,
        )
    })?;
    if !metadata.is_file() || !is_executable(&metadata) || executable_digest(path)? != expected {
        return Err(Error::validation_invalid_argument(
            "controller_runtime",
            format!("pinned controller executable is not immutable and available: {pinned}"),
            Some(pinned.to_string()),
            None,
        ));
    }
    Ok(())
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

fn pinned_path(identity: &str) -> Result<PathBuf> {
    Ok(paths::homeboy_data()?
        .join("controller-runtimes")
        .join(paths::sanitize_path_segment(identity))
        .join("homeboy"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_mismatch_returns_pinned_runtime_recovery_command() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy-origin");
        fs::write(&pinned, b"origin").expect("write pinned executable");
        make_executable_read_only(&pinned).expect("seal executable");
        let metadata = json!({
            "controller_runtime": {
                "originating": {
                    "build_identity": "homeboy 1.0.0+origin",
                    "pinned_executable": pinned,
                    "sha256": executable_digest(&pinned).expect("hash executable"),
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
    fn identity_mismatch_resolves_the_verified_pinned_executable() {
        let temporary = tempfile::tempdir().expect("temporary runtime directory");
        let pinned = temporary.path().join("homeboy-origin");
        fs::write(&pinned, b"origin").expect("write pinned executable");
        make_executable_read_only(&pinned).expect("seal executable");
        let metadata = json!({
            "controller_runtime": {
                "originating": {
                    "build_identity": "homeboy 1.0.0+origin",
                    "pinned_executable": pinned,
                    "sha256": executable_digest(&pinned).expect("hash executable"),
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
        validate_pin(&runtime).expect("sealed pin is valid");

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
    fn admission_reuses_the_selected_generation() {
        crate::test_support::with_isolated_home(|_| {
            let first = admit_current().expect("first admission");
            let selected = first.runtime.clone();
            drop(first);
            let second = admit_current().expect("second admission");
            assert_eq!(second.runtime, selected);
        });
    }
}
