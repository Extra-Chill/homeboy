use serde::{Deserialize, Serialize};

/// Post-write verify contract for autofix. Runs from the component root after
/// `refactor --from ...` writes edits to disk. A non-zero exit code triggers a
/// full revert of the written files and marks every auto-applied fix as
/// declined (with the verify output captured on the chunk).
///
/// See #1167 for design rationale. Per-rule safety rails still live in the
/// fixers (see #1166); this is a general-purpose backstop that catches bugs
/// any individual rule's rails miss.
///
/// Typical extension configurations:
///
/// - Compile check: catches type errors after writes.
/// - Syntax check: validates changed files after writes.
/// - Generic: leave unset — verify is opt-in, absent config = no gate.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AutofixVerifyConfig {
    /// Executable to run. Resolved against `PATH` unless absolute.
    pub command: String,

    /// Arguments passed to the command. Each entry is a distinct argv slot —
    /// no shell splitting. To pass multiple arguments as one string, put them
    /// in a single entry and wrap the full invocation in `sh -c` yourself.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,

    /// Maximum seconds to wait before killing the verify process. Defaults to
    /// 120 when absent. A verify that times out is treated as a failure —
    /// the same as a non-zero exit code — so the autofix reverts.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_secs: Option<u64>,
}

impl AutofixVerifyConfig {
    /// Effective timeout in seconds (120 when unset).
    pub fn effective_timeout_secs(&self) -> u64 {
        self.timeout_secs.unwrap_or(120)
    }
}
