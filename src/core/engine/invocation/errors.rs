use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use crate::core::error::Error;

use super::runtime::HOMEBOY_INVOCATION_RUNTIME_DIR_ENV;

pub(super) fn invocation_dir_create_error(dir: &Path, runtime_root: &Path, error: io::Error) -> Error {
    if error.kind() == io::ErrorKind::PermissionDenied {
        return Error::internal_unexpected(format!(
            "Homeboy cannot create invocation dir {} because runtime temp root {} is not writable by the current user ({}). {} Remediation: remove or chown the runtime temp root, or set {} to a writable short path.",
            dir.display(),
            runtime_root.display(),
            current_user_label(),
            runtime_root_owner_label(runtime_root),
            HOMEBOY_INVOCATION_RUNTIME_DIR_ENV,
        ));
    }

    Error::internal_io(
        format!(
            "Failed to create invocation dir {}: {}",
            dir.display(),
            error
        ),
        Some("invocation.dir".to_string()),
    )
}

fn current_user_label() -> String {
    let name = std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .ok()
        .filter(|value| !value.trim().is_empty());

    #[cfg(unix)]
    {
        let uid = unsafe { libc::getuid() };
        match name {
            Some(name) => format!("{name}, uid {uid}"),
            None => format!("uid {uid}"),
        }
    }

    #[cfg(not(unix))]
    {
        name.unwrap_or_else(|| "unknown user".to_string())
    }
}

fn runtime_root_owner_label(runtime_root: &Path) -> String {
    match fs::metadata(runtime_root) {
        #[cfg(unix)]
        Ok(metadata) => format!("Runtime temp root owner: uid {}.", metadata.uid()),
        #[cfg(not(unix))]
        Ok(_) => "Runtime temp root exists but ownership could not be inspected on this platform."
            .to_string(),
        Err(e) => format!("Runtime temp root owner could not be inspected: {e}."),
    }
}
