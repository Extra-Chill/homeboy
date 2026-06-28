//! System process probing.
//!
//! Generic, platform-aware probing of the live process table via `ps`. This is
//! pure system-resource orchestration (invoking `ps` and returning its raw
//! stdout) and therefore lives in core rather than the command layer. Callers
//! parse the raw snapshot into their own presentation types.

use homeboy::core::{Error, Result};
use std::process::Command;

/// Capture a snapshot of the live process table.
///
/// Invokes `ps -axo pid=,ppid=,command=` and returns its raw stdout. The
/// caller is responsible for parsing the columnar output. Returns an error if
/// the `ps` binary cannot be spawned or exits unsuccessfully.
pub fn capture_process_snapshot() -> Result<String> {
    let output = Command::new("ps")
        .args(["-axo", "pid=,ppid=,command="])
        .output()
        .map_err(|e| Error::internal_io(e.to_string(), Some("resources.process.ps".to_string())))?;
    if !output.status.success() {
        return Err(Error::internal_unexpected(format!(
            "ps failed while observing processes: {}",
            String::from_utf8_lossy(&output.stderr)
        )));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}
