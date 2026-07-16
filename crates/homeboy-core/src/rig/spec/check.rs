//! Declarative check schema for rig specs — `CheckSpec` and the mtime/
//! staleness comparison sources it can reference.

use serde::{Deserialize, Serialize};

use super::DiscoverSpec;

/// A single declarative check. One-of semantics — exactly one of the
/// probe fields (`http`, `file`, `any_file_exists`, `command`, `newer_than`)
/// should be set.
/// Validated at check-time, not parse-time, because serde flattening
/// across tagged enums is awkward and explicit-field checks keep the
/// spec readable.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckSpec {
    /// HTTP GET — passes if status matches `expect_status` (default 200).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub http: Option<String>,

    /// Expected HTTP status for the `http` check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_status: Option<u16>,

    /// File path — passes if the file exists.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file: Option<String>,

    /// If set along with `file`, also requires the file contents to contain
    /// this substring. Cheap probe for verifying drop-ins / generated files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,

    /// File paths — passes when at least one path exists.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub any_file_exists: Vec<String>,

    /// Shell command — passes if exit code matches `expect_exit` (default 0).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// Expected exit code for the `command` check.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expect_exit: Option<i32>,

    /// Mtime / staleness comparison — passes when `left` is newer than
    /// `right`. Surfaces "I rebuilt but the daemon is still on the old
    /// bundle" failures the wiki preflight calls out as the #1 dev-env
    /// confusion source. If the `process_start` source resolves to no
    /// running process, the check passes (no stale daemon to recycle).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub newer_than: Option<NewerThanSpec>,
}

/// Mtime / staleness comparison check.
///
/// Each side picks one source. `left > right` ⇒ pass. Equal or `left < right`
/// ⇒ fail. "Source missing" semantics differ by side: if `left` is a
/// `process_start` and no process matches, the check passes (interpretation:
/// no stale daemon to fight with). Any other missing source is an error.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NewerThanSpec {
    pub left: TimeSource,
    pub right: TimeSource,
}

/// A time source for `newer_than` checks. One-of semantics enforced at
/// evaluate-time.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeSource {
    /// File mtime (seconds since epoch). Path supports `~` and `${...}`
    /// expansion.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_mtime: Option<String>,

    /// Process start time (seconds since epoch). Discovers the newest
    /// matching process by command-line substring (`ps -o args`). When no
    /// process matches and this source is on the `left`, the parent check
    /// passes — there's no stale process to flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start: Option<DiscoverSpec>,
}
