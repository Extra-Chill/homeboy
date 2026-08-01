use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const ACTIONS_DETAILS_KEY: &str = "_homeboy_actions";

/// An explicit command the caller may execute to repair a reported condition.
///
/// This intentionally carries program and arguments separately. Consumers must
/// execute those fields directly; `render_command` is presentation only.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ExecutableAction {
    pub id: String,
    pub label: String,
    pub program: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    pub safety: ActionSafety,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub required_confirmations: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub evidence: Option<Value>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ActionSafety {
    ReadOnly,
    Mutating,
}

impl ExecutableAction {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        program: impl Into<String>,
        args: impl IntoIterator<Item = impl Into<String>>,
        safety: ActionSafety,
    ) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            program: program.into(),
            args: args.into_iter().map(Into::into).collect(),
            safety,
            required_confirmations: Vec::new(),
            evidence: None,
        }
    }

    pub fn requiring_confirmation(mut self, confirmation: impl Into<String>) -> Self {
        self.required_confirmations.push(confirmation.into());
        self
    }

    pub fn with_evidence(mut self, evidence: Value) -> Self {
        self.evidence = Some(evidence);
        self
    }

    pub fn render_command(&self) -> String {
        std::iter::once(self.program.as_str())
            .chain(self.args.iter().map(String::as_str))
            .map(posix_quote_arg)
            .collect::<Vec<_>>()
            .join(" ")
    }
}

// Kept byte-for-byte compatible with homeboy-engine-primitives::shell::quote_arg.
// homeboy-error is below that crate in the dependency graph, so it cannot import it.
fn posix_quote_arg(arg: &str) -> String {
    if arg.is_empty() {
        return "''".to_string();
    }

    const SHELL_META: &[char] = &[
        ' ', '\t', '\n', '\'', '"', '\\', '$', '`', '!', '*', '?', '[', ']', '(', ')', '{', '}',
        '<', '>', '|', '&', ';', '#', '~',
    ];
    if !arg.contains(SHELL_META) {
        return arg.to_string();
    }

    format!("'{}'", arg.replace('\'', "'\\''"))
}

fn format_suggestions(suggestions: &[String]) -> String {
    if suggestions.len() == 1 {
        format!("Did you mean: {}?", suggestions[0])
    } else {
        format!("Did you mean: {}?", suggestions.join(", "))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ErrorCode {
    ConfigMissingKey,
    ConfigInvalidJson,
    ConfigInvalidValue,
    ConfigIdCollision,

    ValidationMissingArgument,
    ValidationInvalidArgument,
    ValidationInvalidJson,
    ValidationMultipleErrors,

    ProjectNotFound,
    ProjectNoActive,
    ServerNotFound,
    ComponentNotFound,
    ComponentNotAttached,
    FleetNotFound,
    ExtensionNotFound,
    ExtensionUnsupported,
    DocsTopicNotFound,
    RigNotFound,
    RunnerNotFound,
    RunnerPolicyDenied,
    RunnerCapabilityMissing,
    RunnerWorkspaceOwnershipConflict,
    BrokerAuthDenied,
    ScheduleNotFound,
    ServiceTunnelNotFound,
    RigPipelineFailed,
    RigServiceFailed,
    RigResourceConflict,
    RigSchemaUnsupported,
    RunnerLabTransportFailure,
    StackNotFound,
    StackApplyConflict,
    RunnerControllerDisconnected,
    RuntimePromotionContended,
    RuntimePromotionWaitTimeout,
    DependencyStepFailed,
    DependencyOutputMissing,

    SshServerInvalid,
    SshIdentityFileNotFound,
    SshAuthFailed,
    SshConnectFailed,

    RemoteCommandFailed,
    RemoteCommandTimeout,

    DeployNoComponentsConfigured,
    DeployBuildFailed,
    DeployUploadFailed,

    GitCommandFailed,

    ObservationStoreBusy,

    /// The filesystem could not accept a write because it is out of bytes or
    /// out of inodes. Distinct from [`ErrorCode::InternalIoError`] so callers
    /// can degrade instead of retrying, and so an operator reads "disk full"
    /// rather than "write failed" (#11127, #10603).
    StorageExhausted,

    InternalIoError,
    InternalJsonError,
    InternalUnexpected,
}

impl ErrorCode {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCode::ConfigMissingKey => "config.missing_key",
            ErrorCode::ConfigInvalidJson => "config.invalid_json",
            ErrorCode::ConfigInvalidValue => "config.invalid_value",
            ErrorCode::ConfigIdCollision => "config.id_collision",

            ErrorCode::ValidationMissingArgument => "validation.missing_argument",
            ErrorCode::ValidationInvalidArgument => "validation.invalid_argument",
            ErrorCode::ValidationInvalidJson => "validation.invalid_json",
            ErrorCode::ValidationMultipleErrors => "validation.multiple_errors",

            ErrorCode::ProjectNotFound => "project.not_found",
            ErrorCode::ProjectNoActive => "project.no_active",
            ErrorCode::ServerNotFound => "server.not_found",
            ErrorCode::ComponentNotFound => "component.not_found",
            ErrorCode::ComponentNotAttached => "component.not_attached",
            ErrorCode::FleetNotFound => "fleet.not_found",
            ErrorCode::ExtensionNotFound => "extension.not_found",
            ErrorCode::ExtensionUnsupported => "extension.unsupported",
            ErrorCode::DocsTopicNotFound => "docs.topic_not_found",
            ErrorCode::RigNotFound => "rig.not_found",
            ErrorCode::RunnerNotFound => "runner.not_found",
            ErrorCode::RunnerPolicyDenied => "runner.policy_denied",
            ErrorCode::RunnerCapabilityMissing => "runner.capability_missing",
            ErrorCode::RunnerWorkspaceOwnershipConflict => "runner.workspace_ownership_conflict",
            ErrorCode::BrokerAuthDenied => "broker.auth_denied",
            ErrorCode::ScheduleNotFound => "schedule.not_found",
            ErrorCode::ServiceTunnelNotFound => "service_tunnel.not_found",
            ErrorCode::RigPipelineFailed => "rig.pipeline_failed",
            ErrorCode::RigServiceFailed => "rig.service_failed",
            ErrorCode::RigResourceConflict => "rig.resource_conflict",
            ErrorCode::RigSchemaUnsupported => "rig.schema_unsupported",
            ErrorCode::RunnerLabTransportFailure => "runner.lab_transport_failure",
            ErrorCode::StackNotFound => "stack.not_found",
            ErrorCode::StackApplyConflict => "stack.apply_conflict",
            ErrorCode::RunnerControllerDisconnected => "runner.controller_disconnected",
            ErrorCode::RuntimePromotionContended => "runtime_promotion.contended",
            ErrorCode::RuntimePromotionWaitTimeout => "runtime_promotion.wait_timeout",
            ErrorCode::DependencyStepFailed => "dependency_step_failed",
            ErrorCode::DependencyOutputMissing => "dependency_output_missing",

            ErrorCode::SshServerInvalid => "ssh.server_invalid",
            ErrorCode::SshIdentityFileNotFound => "ssh.identity_file_not_found",
            ErrorCode::SshAuthFailed => "ssh.auth_failed",
            ErrorCode::SshConnectFailed => "ssh.connect_failed",

            ErrorCode::RemoteCommandFailed => "remote.command_failed",
            ErrorCode::RemoteCommandTimeout => "remote.command_timeout",

            ErrorCode::DeployNoComponentsConfigured => "deploy.no_components_configured",
            ErrorCode::DeployBuildFailed => "deploy.build_failed",
            ErrorCode::DeployUploadFailed => "deploy.upload_failed",

            ErrorCode::GitCommandFailed => "git.command_failed",

            ErrorCode::ObservationStoreBusy => "observation_store.busy",

            ErrorCode::StorageExhausted => "storage.exhausted",

            ErrorCode::InternalIoError => "internal.io_error",
            ErrorCode::InternalJsonError => "internal.json_error",
            ErrorCode::InternalUnexpected => "internal.unexpected",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]

pub struct Hint {
    pub message: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigMissingKeyDetails {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct ConfigInvalidJsonDetails {
    pub path: String,
    pub error: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigInvalidValueDetails {
    pub key: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub value: Option<String>,
    pub problem: String,
}

#[derive(Debug, Serialize)]

pub struct ConfigIdCollisionDetails {
    pub id: String,
    pub requested_type: String,
    pub existing_type: String,
}

#[derive(Debug, Serialize)]

pub struct NoActiveProjectDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub config_path: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Error {
    pub code: ErrorCode,
    pub message: String,
    pub details: Value,
    pub hints: Vec<Hint>,
    pub retryable: Option<bool>,
}

pub type Result<T> = std::result::Result<T, Error>;

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for Error {}

/// Let `?` carry a real `io::Error` instead of forcing a manual stringify.
///
/// Routed through [`Error::from_io_error`] on purpose: that is where #11188's
/// storage-exhaustion classification lives, and a conversion that bypassed it
/// would silently re-collapse every `ENOSPC` back into `internal.io_error`.
///
/// The conversion cannot attach context — `?` has nowhere to put it — so a
/// site that knows which file or which operation failed should still call
/// [`Error::from_io_error`] with a context string. This impl exists so that
/// *discarding the error entirely* (`.map_err(|_| ...)`) is never the path of
/// least resistance again.
impl From<std::io::Error> for Error {
    fn from(error: std::io::Error) -> Self {
        Self::from_io_error(&error, None)
    }
}

/// Let `?` carry a real `serde_json::Error`.
///
/// Delegates to [`Error::from_json_error`], so a serialization failure that is
/// really a failed *write* is reported as an IO failure (and classified for
/// storage exhaustion) rather than as a JSON failure.
impl From<serde_json::Error> for Error {
    fn from(error: serde_json::Error) -> Self {
        Self::from_json_error(&error, None)
    }
}

#[derive(Debug, Serialize)]

pub struct NotFoundDetails {
    pub id: String,
}

#[derive(Debug, Serialize)]

pub struct MissingArgumentDetails {
    pub args: Vec<String>,
}

#[derive(Debug, Serialize)]

pub struct InvalidArgumentDetails {
    pub field: String,
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tried: Option<Vec<String>>,
    /// Captured evidence for a failed command-backed validation (e.g. the
    /// release `preflight.test`/`preflight.lint` gates). Carries the exact
    /// resolved command, its working directory, exit code, and captured
    /// stdout/stderr so an operator can see *what ran and why it failed*
    /// directly in the structured error instead of having to reverse-engineer
    /// the gate. Generic across every extension/test backend.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_evidence: Option<CommandEvidence>,
}

#[derive(Debug, Serialize)]
pub struct SchemaMismatchDetails {
    pub field: String,
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    pub expected: String,
    pub cause: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fallback_cause: Option<String>,
}

/// Captured evidence describing a single command-backed validation failure.
///
/// Surfaced inside [`InvalidArgumentDetails::command_evidence`] so structured
/// error output (and the release step's `error_details`) carries the exact
/// command, cwd, exit code, and bounded stdout/stderr that produced the
/// failure. Intentionally ecosystem-agnostic: any extension test/lint backend
/// can populate it from its captured runner output.
#[derive(Debug, Clone, Serialize)]
pub struct CommandEvidence {
    /// Human-readable description of the resolved command that ran (e.g. the
    /// extension id plus script path, or a self-check command line).
    pub command: String,
    /// Working directory the command ran in.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Where the command executed: `"local"` controller, or a named runner.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Exit code the command returned.
    pub exit_code: i32,
    /// Captured stdout (already bounded by the caller). Empty when none.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stdout: String,
    /// Captured stderr (already bounded by the caller). Empty when none.
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stderr: String,
    /// Whether either captured stream was truncated to fit the error payload.
    #[serde(skip_serializing_if = "std::ops::Not::not", default)]
    pub truncated: bool,
}

#[derive(Debug, Clone)]
pub struct RigResourceConflictInfo {
    pub rig_id: String,
    pub command: String,
    pub resource_kind: String,
    pub resource_value: String,
    pub held_by_rig: String,
    pub held_by_command: String,
    pub held_by_pid: u32,
    pub held_since: String,
    pub held_by_run_id: Option<String>,
    pub held_by_runner_id: Option<String>,
    /// Wall-clock age of the holding lease in seconds, when it can be derived
    /// from `held_since`. Surfaced so operators can judge whether the holder is
    /// a fresh, legitimate run or a stale/wedged one worth reclaiming.
    pub held_age_seconds: Option<i64>,
}

/// Render a duration in seconds as a compact human string (e.g. `28m`, `2h 5m`).
fn humanize_age_seconds(seconds: i64) -> String {
    if seconds < 0 {
        return "unknown".to_string();
    }
    let hours = seconds / 3600;
    let minutes = (seconds % 3600) / 60;
    let secs = seconds % 60;
    if hours > 0 {
        format!("{hours}h {minutes}m")
    } else if minutes > 0 {
        format!("{minutes}m {secs}s")
    } else {
        format!("{secs}s")
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ValidationErrorItem {
    pub field: String,
    pub problem: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<serde_json::Value>,
}

#[derive(Debug, Serialize)]
pub struct MultipleValidationErrorsDetails {
    pub errors: Vec<ValidationErrorItem>,
}

#[derive(Debug, Serialize)]

pub struct InternalIoErrorDetails {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

/// Evidence for a write that failed because the filesystem had no capacity.
///
/// Every field beyond `error` is optional because the classifier is reachable
/// from two very different places: a live `io::Error` at the moment a write
/// failed (which knows nothing about reserves), and a deliberate capacity
/// preflight (which knows all of it). Both must produce the same code.
#[derive(Debug, Default, Serialize)]
pub struct StorageExhaustedDetails {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    /// Filesystem path the failing write targeted, when the caller knows it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_bytes: Option<u64>,
    /// Free inodes. A filesystem with free bytes and none of these still fails
    /// every write, which is precisely the state that produced #10603.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub available_inodes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reserve_inodes: Option<u64>,
}

/// `ENOSPC` on every unix target homeboy builds for.
///
/// Checked alongside [`std::io::ErrorKind::StorageFull`] rather than instead of
/// it: the mapped `ErrorKind` is the documented classification, and the raw
/// errno is the fallback for any error value that was reconstructed without
/// one (for example through `io::Error::from_raw_os_error` in a shim).
#[cfg(unix)]
const ENOSPC: i32 = 28;

/// Whether a live `io::Error` reports an out-of-capacity filesystem.
///
/// Inode exhaustion also surfaces as `ENOSPC`, so this covers the #10603 state
/// (free bytes, zero free inodes) without needing a separate probe.
pub fn io_error_is_storage_exhausted(error: &std::io::Error) -> bool {
    if error.kind() == std::io::ErrorKind::StorageFull {
        return true;
    }
    #[cfg(unix)]
    if error.raw_os_error() == Some(ENOSPC) {
        return true;
    }
    message_reports_storage_exhaustion(&error.to_string())
}

/// Whether an already-stringified io/SQLite error reports exhausted storage.
///
/// Most homeboy call sites stringify an `io::Error` before it ever reaches this
/// crate — `Error::internal_io` takes `impl Into<String>`, and the typed
/// [`From<std::io::Error>`] conversion added in #11134 only covers sites that
/// have been migrated onto `?` — so the typed classifier above cannot see them.
/// These markers are the stable operating-system and SQLite renderings of a
/// filesystem that cannot accept another byte or another inode.
pub fn message_reports_storage_exhaustion(message: &str) -> bool {
    const MARKERS: &[&str] = &[
        "no space left on device",
        "database or disk is full",
        "disk quota exceeded",
        "enospc",
        // `statvfs`-style phrasing used by homeboy's own capacity preflight.
        "filesystem has no free inodes",
    ];
    let lowered = message.to_ascii_lowercase();
    if MARKERS.iter().any(|marker| lowered.contains(marker)) {
        return true;
    }
    // `io::Error`'s Display appends the raw errno. Errno 28 is ENOSPC on unix
    // and something unrelated on Windows, so the numeric marker is unix-only.
    #[cfg(unix)]
    if lowered.contains("os error 28") {
        return true;
    }
    false
}

#[derive(Debug, Serialize)]
pub struct ObservationStoreBusyDetails {
    pub path: String,
    pub operation: String,
    pub timeout_ms: u64,
    /// SQLite does not expose the process owning its advisory lock.
    pub lock_owner: String,
    /// SQLite does not expose lock acquisition time.
    pub lock_age_ms: Option<u64>,
}

#[derive(Debug, Serialize)]

pub struct InternalJsonErrorDetails {
    pub error: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct TargetDetails {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,
}

#[derive(Debug, Serialize)]

pub struct RemoteCommandFailedDetails {
    pub command: String,
    pub exit_code: i32,
    pub stdout: String,
    pub stderr: String,
    pub target: TargetDetails,
}

#[derive(Debug, Serialize)]
pub struct GitCommandFailedDetails {
    pub command: String,
    pub cwd: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub io_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct DependencyFailureDetails {
    pub step_id: String,
    pub component_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub logs: Vec<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub artifact_refs: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub next_machine_action: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cause: Option<Value>,
}

#[derive(Debug, Serialize)]

pub struct SshServerInvalidDetails {
    pub server_id: String,
    pub missing_fields: Vec<String>,
}

#[derive(Debug, Serialize)]

pub struct SshIdentityFileNotFoundDetails {
    pub server_id: String,
    pub identity_file: String,
}

/// Serialize a details struct to JSON Value, falling back to empty object on failure.
fn to_details(details: impl Serialize) -> Value {
    serde_json::to_value(details).unwrap_or_else(|_| Value::Object(serde_json::Map::new()))
}

impl Error {
    pub fn new(code: ErrorCode, message: impl Into<String>, details: Value) -> Self {
        Self {
            code,
            message: message.into(),
            details,
            hints: Vec::new(),
            retryable: None,
        }
    }

    /// A write failed, or was refused, because the filesystem has no capacity.
    ///
    /// The hint names the degraded cleanup path on purpose. The reason #10603
    /// deadlocked is that the only tool which can free capacity could not
    /// start: opening the SQLite observation store needs an inode for its
    /// journal, and there was none. The store-independent categories do not,
    /// so they remain runnable in exactly the state that blocks everything
    /// else.
    pub fn storage_exhausted(error: impl Into<String>, context: Option<String>) -> Self {
        Self::storage_exhausted_detailed(StorageExhaustedDetails {
            error: error.into(),
            context,
            ..StorageExhaustedDetails::default()
        })
    }

    /// [`Error::storage_exhausted`] carrying measured capacity evidence.
    pub fn storage_exhausted_detailed(details: StorageExhaustedDetails) -> Self {
        Self::new(
            ErrorCode::StorageExhausted,
            "Filesystem capacity exhausted",
            to_details(details),
        )
        // Not retryable: the same write fails identically until capacity is
        // reclaimed, so an automatic retry only burns the remaining budget.
        .with_retryable(false)
        .with_hint("Reclaim capacity with `homeboy cleanup --apply`.")
        .with_hint(
            "If cleanup cannot start because the observation store will not open, run the \
             store-independent categories: `homeboy cleanup --include orphaned-artifact-bytes \
             --include runtime-tmp --apply`.",
        )
    }

    /// Classify a live `io::Error`, keeping storage exhaustion distinguishable.
    ///
    /// This is the contextful form. [`From<std::io::Error>`] delegates here
    /// with `None`, so every conversion — explicit or via `?` — goes through
    /// the same ENOSPC classification (#11188). Prefer this constructor
    /// wherever the call site knows *what it was doing*: a bare `?` yields a
    /// correctly coded error with no operation attached, which is enough to
    /// act on but not enough to locate.
    pub fn from_io_error(error: &std::io::Error, context: Option<String>) -> Self {
        if io_error_is_storage_exhausted(error) {
            return Self::storage_exhausted(error.to_string(), context);
        }
        Self::internal_io(error.to_string(), context)
    }

    /// Classify a `serde_json` failure, keeping storage exhaustion visible.
    ///
    /// `serde_json`'s writer-side entry points (`to_writer`,
    /// `to_writer_pretty`) wrap the underlying `io::Error` rather than
    /// returning it, so a full filesystem reaches a caller as a
    /// `serde_json::Error`. Reporting that as `internal.json_error` is how a
    /// disk-full write comes to look like malformed JSON — the exact
    /// misdirection #10603 was diagnosed through. Io-category failures are
    /// therefore routed to [`Error::internal_io`], which carries #11188's
    /// ENOSPC classification; only genuine syntax/data/EOF failures stay
    /// `internal.json_error`.
    pub fn from_json_error(error: &serde_json::Error, context: Option<String>) -> Self {
        if error.is_io() {
            if error.io_error_kind() == Some(std::io::ErrorKind::StorageFull) {
                return Self::storage_exhausted(error.to_string(), context);
            }
            return Self::internal_io(error.to_string(), context);
        }
        Self::internal_json(error.to_string(), context)
    }

    /// Whether this error reports an out-of-capacity filesystem.
    ///
    /// The predicate callers use to decide between "retry" and "degrade".
    pub fn is_storage_exhausted(&self) -> bool {
        self.code == ErrorCode::StorageExhausted
    }

    pub fn observation_store_busy(
        path: impl Into<String>,
        operation: impl Into<String>,
        timeout_ms: u64,
    ) -> Self {
        let path = path.into();
        let operation = operation.into();
        Self::new(
            ErrorCode::ObservationStoreBusy,
            format!("Observation store remained locked while attempting {operation}"),
            to_details(ObservationStoreBusyDetails {
                path,
                operation,
                timeout_ms,
                lock_owner: "unknown (SQLite does not expose lock ownership)".to_string(),
                lock_age_ms: None,
            }),
        )
        .with_retryable(true)
        .with_hint(
            "Retry `homeboy runs show <run-id>` after the active import or writer completes.",
        )
    }

    pub fn validation_missing_argument(args: Vec<String>) -> Self {
        let details = to_details(MissingArgumentDetails { args });
        Self::new(
            ErrorCode::ValidationMissingArgument,
            "Missing required argument",
            details,
        )
    }

    pub fn validation_invalid_argument(
        field: impl Into<String>,
        problem: impl Into<String>,
        id: Option<String>,
        tried: Option<Vec<String>>,
    ) -> Self {
        Self::validation_invalid_argument_with_evidence(field, problem, id, tried, None)
    }

    /// Ergonomic shorthand for the common invalid-argument case: just the field
    /// and the problem, with no offending id and no suggested alternatives.
    ///
    /// Equivalent to `validation_invalid_argument(field, problem, None, None)` —
    /// most call sites carry those two trailing `None`s as pure scaffolding.
    pub fn invalid_argument(field: impl Into<String>, problem: impl Into<String>) -> Self {
        Self::validation_invalid_argument(field, problem, None, None)
    }

    /// Ergonomic shorthand for an invalid argument that names the offending id
    /// (e.g. a path or value) but has no suggested alternatives.
    ///
    /// Equivalent to `validation_invalid_argument(field, problem, Some(id), None)`.
    pub fn invalid_argument_for(
        field: impl Into<String>,
        problem: impl Into<String>,
        id: impl Into<String>,
    ) -> Self {
        Self::validation_invalid_argument(field, problem, Some(id.into()), None)
    }

    /// Same as [`Self::validation_invalid_argument`] but attaches captured
    /// [`CommandEvidence`] (resolved command, cwd, exit code, stdout/stderr) so
    /// command-backed validation failures (e.g. the release `preflight.test`
    /// gate) surface *what ran and why it failed* in the structured error.
    pub fn validation_invalid_argument_with_evidence(
        field: impl Into<String>,
        problem: impl Into<String>,
        id: Option<String>,
        tried: Option<Vec<String>>,
        command_evidence: Option<CommandEvidence>,
    ) -> Self {
        let field_str = field.into();
        let problem_str = problem.into();
        let message = format!("Invalid argument '{}': {}", field_str, problem_str);
        let details = to_details(InvalidArgumentDetails {
            field: field_str,
            problem: problem_str,
            id,
            tried,
            command_evidence,
        });

        Self::new(ErrorCode::ValidationInvalidArgument, message, details)
    }

    pub fn validation_schema_mismatch(
        field: impl Into<String>,
        expected: impl Into<String>,
        id: Option<String>,
        cause: impl Into<String>,
        fallback_cause: Option<String>,
    ) -> Self {
        let field_str = field.into();
        let expected_str = expected.into();
        let cause_str = cause.into();
        let problem = match &fallback_cause {
            Some(fallback) => format!(
                "does not match expected schema ({expected_str}); primary cause: {cause_str}; fallback cause: {fallback}"
            ),
            None => format!("does not match expected schema ({expected_str}): {cause_str}"),
        };
        let details = to_details(SchemaMismatchDetails {
            field: field_str.clone(),
            problem: problem.clone(),
            id,
            expected: expected_str,
            cause: cause_str,
            fallback_cause,
        });

        Self::new(
            ErrorCode::ValidationInvalidArgument,
            format!("Invalid argument '{}': {}", field_str, problem),
            details,
        )
    }

    pub fn validation_invalid_json(
        err: serde_json::Error,
        context: Option<String>,
        received: Option<String>,
    ) -> Self {
        let error = err.to_string();
        let category = match err.classify() {
            serde_json::error::Category::Io => "io",
            serde_json::error::Category::Syntax => "syntax",
            serde_json::error::Category::Data => "data",
            serde_json::error::Category::Eof => "eof",
        };
        let mut details = serde_json::json!({
            "error": error,
            "category": category,
            "context": context,
        });
        if let Some(received_json) = received {
            details["received"] =
                serde_json::json!(received_json.chars().take(200).collect::<String>());
        }

        Self::new(
            ErrorCode::ValidationInvalidJson,
            format!("Invalid JSON: {error}"),
            details,
        )
    }

    pub fn validation_json_error(&self) -> Option<&str> {
        if self.code != ErrorCode::ValidationInvalidJson {
            return None;
        }
        self.details.get("error").and_then(Value::as_str)
    }

    pub fn validation_json_category(&self) -> Option<&str> {
        if self.code != ErrorCode::ValidationInvalidJson {
            return None;
        }
        self.details.get("category").and_then(Value::as_str)
    }

    pub fn dependency_step_failed(
        step_id: impl Into<String>,
        component_id: impl Into<String>,
        status: Option<i32>,
        logs: Vec<String>,
        artifact_refs: Vec<String>,
        next_machine_action: Option<String>,
        cause: Option<Value>,
    ) -> Self {
        let step_id = step_id.into();
        let component_id = component_id.into();
        Self::new(
            ErrorCode::DependencyStepFailed,
            format!("Dependency step '{step_id}' failed for component '{component_id}'"),
            to_details(DependencyFailureDetails {
                step_id,
                component_id,
                output_path_ref: None,
                status,
                logs,
                artifact_refs,
                next_machine_action,
                cause,
            }),
        )
    }

    pub fn dependency_output_missing(
        step_id: impl Into<String>,
        component_id: impl Into<String>,
        output_path_ref: impl Into<String>,
        artifact_refs: Vec<String>,
        next_machine_action: Option<String>,
    ) -> Self {
        let step_id = step_id.into();
        let component_id = component_id.into();
        let output_path_ref = output_path_ref.into();
        Self::new(
            ErrorCode::DependencyOutputMissing,
            format!(
                "Dependency output '{output_path_ref}' was missing after step '{step_id}' for component '{component_id}'"
            ),
            to_details(DependencyFailureDetails {
                step_id,
                component_id,
                output_path_ref: Some(output_path_ref),
                status: None,
                logs: Vec::new(),
                artifact_refs,
                next_machine_action,
                cause: None,
            }),
        )
    }

    /// A rig spec parsed as valid JSON but its shape doesn't match this
    /// binary's `RigSpec` schema — almost always a binary/spec version
    /// mismatch (e.g. a rig declaring the `component_id`/`path_setting`
    /// component schema against an older homeboy that only understood
    /// top-level `path`). This is deliberately *not* `validation.invalid_json`:
    /// the file isn't malformed, the running binary is just behind.
    pub fn rig_schema_unsupported(
        serde_error: impl Into<String>,
        context: impl Into<String>,
        component: Option<String>,
        active_version: &str,
    ) -> Self {
        // The active version is supplied by the caller: `env!(CARGO_PKG_VERSION)`
        // evaluated inside this error crate would report the crate's own version
        // (e.g. 0.1.0), not the running homeboy build the operator needs to see.
        let active = active_version;
        let serde_error = serde_error.into();
        let message = match &component {
            Some(name) => format!(
                "Rig component '{}' uses an unrecognized schema (expected `path` or `component_id`); \
                 this rig may require a newer homeboy — active version is {}.",
                name, active
            ),
            None => format!(
                "Rig spec uses a schema this homeboy build does not recognize ({}); \
                 this rig may require a newer homeboy — active version is {}.",
                serde_error, active
            ),
        };
        let mut details = serde_json::json!({
            "error": serde_error,
            "context": context.into(),
            "active_version": active,
        });
        if let Some(name) = component {
            details["component"] = serde_json::json!(name);
        }
        Self::new(ErrorCode::RigSchemaUnsupported, message, details).with_hint(
            "Run 'homeboy upgrade' to get a build that understands this rig schema, then retry. \
             If the binary is already current, the rig spec may genuinely be malformed — check the \
             named field against the current rig schema.",
        )
    }

    pub fn validation_multiple_errors(errors: Vec<ValidationErrorItem>) -> Self {
        let count = errors.len();
        let details = to_details(MultipleValidationErrorsDetails { errors });

        Self::new(
            ErrorCode::ValidationMultipleErrors,
            format!(
                "Found {} validation issue{}",
                count,
                if count == 1 { "" } else { "s" }
            ),
            details,
        )
    }

    /// Generic entity-not-found constructor. All entity-specific variants delegate here.
    pub fn entity_not_found(
        code: ErrorCode,
        entity_type: &str,
        id: impl Into<String>,
        suggestions: Vec<String>,
    ) -> Self {
        let mut err = Self::not_found(code, &format!("{} not found", entity_type), id);
        if !suggestions.is_empty() {
            err = err.with_hint(format_suggestions(&suggestions));
        }
        let list_cmd = entity_type.to_lowercase();
        err.with_hint(format!(
            "Run 'homeboy {} list' to see available {}s",
            list_cmd, list_cmd
        ))
    }

    pub fn project_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ProjectNotFound, "Project", id, suggestions)
    }

    pub fn server_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ServerNotFound, "Server", id, suggestions)
    }

    pub fn component_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ComponentNotFound, "Component", id, suggestions)
    }

    pub fn component_not_attached(
        id: impl Into<String>,
        local_path: impl Into<String>,
        project_suggestion: Option<String>,
    ) -> Self {
        let id = id.into();
        let lp = local_path.into();
        let details = to_details(NotFoundDetails { id: id.clone() });
        let mut err = Self::new(
            ErrorCode::ComponentNotAttached,
            format!(
                "Component '{}' is registered but not attached to any project. Release and deploy require project attachment.",
                id
            ),
            details,
        );
        if let Some(proj) = project_suggestion {
            err = err
                .with_hint(format!(
                    "Attach: homeboy project components attach-path {} {}",
                    proj, lp
                ))
                .with_hint(format!(
                    "If only one project exists, run: homeboy project components attach-path {} {}",
                    proj, lp
                ));
        } else {
            err = err.with_hint(format!(
                "Attach to a project: homeboy project components attach-path <project> {}",
                lp
            ));
        }
        err = err.with_hint("List projects: homeboy project list".to_string());
        err
    }

    pub fn extension_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ExtensionNotFound, "Extension", id, suggestions)
    }

    pub fn fleet_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::FleetNotFound, "Fleet", id, suggestions)
    }

    pub fn rig_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::RigNotFound, "Rig", id, suggestions)
    }

    pub fn runner_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::RunnerNotFound, "Runner", id, suggestions)
    }

    pub fn runner_capability_missing(
        runner_id: impl Into<String>,
        step: impl Into<String>,
        missing_capabilities: Vec<String>,
        missing_providers: Vec<String>,
    ) -> Self {
        let runner_id = runner_id.into();
        let step = step.into();
        let mut parts = Vec::new();
        if !missing_capabilities.is_empty() {
            parts.push(format!("capabilities: {}", missing_capabilities.join(", ")));
        }
        if !missing_providers.is_empty() {
            parts.push(format!("providers: {}", missing_providers.join(", ")));
        }
        Self::new(
            ErrorCode::RunnerCapabilityMissing,
            format!(
                "Runner '{}' is missing required capabilities for '{}': {}",
                runner_id,
                step,
                parts.join("; ")
            ),
            serde_json::json!({
                "runner_id": runner_id,
                "step": step,
                "missing_capabilities": missing_capabilities,
                "missing_providers": missing_providers,
            }),
        )
    }

    /// Reverse runner broker authentication/authorization rejection.
    ///
    /// Secrets (tokens) are never embedded in the message or details; only the
    /// non-sensitive reason and any matched runner id are surfaced so the broker
    /// can return a structured `broker.auth_denied` error.
    pub fn broker_auth_denied(
        reason: impl Into<String>,
        runner_id: Option<String>,
        hints: Vec<String>,
    ) -> Self {
        let reason_str = reason.into();
        let details = serde_json::json!({
            "reason": reason_str,
            "runner_id": runner_id,
        });
        let mut error = Self::new(
            ErrorCode::BrokerAuthDenied,
            format!("Reverse runner broker rejected request: {reason_str}"),
            details,
        );
        for hint in hints {
            error = error.with_hint(hint);
        }
        error
    }

    pub fn schedule_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::ScheduleNotFound, "Schedule", id, suggestions)
    }

    pub fn service_tunnel_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(
            ErrorCode::ServiceTunnelNotFound,
            "Tunnel service",
            id,
            suggestions,
        )
    }

    pub fn stack_not_found(id: impl Into<String>, suggestions: Vec<String>) -> Self {
        Self::entity_not_found(ErrorCode::StackNotFound, "Stack", id, suggestions)
    }

    /// Cherry-pick conflict during `stack apply`. Carries the offending PR
    /// number so callers can surface a "resume from here" message without
    /// re-walking the spec.
    pub fn stack_apply_conflict(
        stack_id: impl Into<String>,
        pr_number: u64,
        repo: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        let stack_id = stack_id.into();
        let repo = repo.into();
        let message = message.into();
        Self::new(
            ErrorCode::StackApplyConflict,
            format!(
                "Cherry-pick conflict in stack '{}' at PR {}#{}: {}",
                stack_id, repo, pr_number, message
            ),
            serde_json::json!({
                "stack_id": stack_id,
                "pr_number": pr_number,
                "repo": repo,
                "message": message,
            }),
        )
    }

    pub fn rig_pipeline_failed(
        rig_id: impl Into<String>,
        step: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        let rig_id = rig_id.into();
        let step = step.into();
        let reason = reason.into();
        Self::new(
            ErrorCode::RigPipelineFailed,
            format!(
                "Rig '{}' pipeline step '{}' failed: {}",
                rig_id, step, reason
            ),
            serde_json::json!({
                "rig_id": rig_id,
                "step": step,
                "reason": reason,
            }),
        )
    }

    pub fn rig_service_failed(
        rig_id: impl Into<String>,
        service_id: impl Into<String>,
        reason: impl Into<String>,
    ) -> Self {
        let rig_id = rig_id.into();
        let service_id = service_id.into();
        let reason = reason.into();
        Self::new(
            ErrorCode::RigServiceFailed,
            format!(
                "Rig '{}' service '{}' failed: {}",
                rig_id, service_id, reason
            ),
            serde_json::json!({
                "rig_id": rig_id,
                "service_id": service_id,
                "reason": reason,
            }),
        )
    }

    pub fn rig_resource_conflict(info: RigResourceConflictInfo) -> Self {
        let held_by_run_id = info.held_by_run_id.clone();
        let held_by_runner_id = info.held_by_runner_id.clone();
        let held_rig_id = info.held_by_rig.clone();
        let age_label = info
            .held_age_seconds
            .map(humanize_age_seconds)
            .unwrap_or_else(|| "unknown".to_string());
        let mut error = Self::new(
            ErrorCode::RigResourceConflict,
            format!(
                "Rig '{}' cannot run '{}': {} resource '{}' is already held by rig '{}' running '{}' (pid {}, since {}, held for {})",
                info.rig_id,
                info.command,
                info.resource_kind,
                info.resource_value,
                info.held_by_rig,
                info.held_by_command,
                info.held_by_pid,
                info.held_since,
                age_label
            ),
            serde_json::json!({
                "rig_id": info.rig_id,
                "command": info.command,
                "resource_kind": info.resource_kind,
                "resource_value": info.resource_value,
                "held_by": {
                    "rig_id": info.held_by_rig,
                    "command": info.held_by_command,
                    "pid": info.held_by_pid,
                    "since": info.held_since,
                    "age_seconds": info.held_age_seconds,
                    "run_id": info.held_by_run_id,
                    "runner_id": info.held_by_runner_id,
                }
            }),
        )
        .with_hint(
            "If this parallel run is intentional, give each run a distinct namespace or port range so their rig resources no longer overlap."
                .to_string(),
        )
        .with_hint(format!(
            "If the holder (pid {}) is dead or wedged and will never finish, reclaim the lock with `homeboy rig release-lock {} --force`; without `--force` the lock is only released when its holder is provably gone or past its TTL.",
            info.held_by_pid, held_rig_id
        ));
        if let Some(run_id) = held_by_run_id {
            error = error
                .with_hint(format!(
                    "Active holding run `{run_id}` is discoverable with `homeboy runs show {run_id}`."
                ))
                .with_hint(format!(
                    "Wait for the holding run with `homeboy runs show {run_id}` or `homeboy runs list --status running --limit 20` before retrying."
                ));
        } else {
            error = error.with_hint(
                "No active run id was recorded for the holding lease; inspect running work with `homeboy runs list --status running --limit 20`.".to_string(),
            );
        }
        if let Some(runner_id) = held_by_runner_id {
            error = error.with_hint(format!(
                "If the conflict came from a Lab daemon job, inspect active jobs with `homeboy runs list --runner {runner_id} --status running --limit 20`; cancel a known job with `homeboy runner job cancel {runner_id} <job-id>`."
            ));
        }
        error
    }

    pub fn docs_topic_not_found(topic: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::DocsTopicNotFound,
            "Documentation topic not found",
            serde_json::json!({ "topic": topic.into() }),
        )
        .with_hint("Run 'homeboy self docs list' to see available topics")
        .with_hint("Topics use path format: 'commands/deploy', 'architecture/hooks'")
    }

    fn not_found(code: ErrorCode, message: &str, id: impl Into<String>) -> Self {
        let details = to_details(NotFoundDetails { id: id.into() });
        Self::new(code, message, details)
    }

    pub fn ssh_server_invalid(server_id: impl Into<String>, missing_fields: Vec<String>) -> Self {
        let details = to_details(SshServerInvalidDetails {
            server_id: server_id.into(),
            missing_fields,
        });

        Self::new(
            ErrorCode::SshServerInvalid,
            "Server is not properly configured",
            details,
        )
    }

    pub fn ssh_identity_file_not_found(
        server_id: impl Into<String>,
        identity_file: impl Into<String>,
    ) -> Self {
        let details = to_details(SshIdentityFileNotFoundDetails {
            server_id: server_id.into(),
            identity_file: identity_file.into(),
        });

        Self::new(
            ErrorCode::SshIdentityFileNotFound,
            "SSH identity file not found",
            details,
        )
    }

    pub fn remote_command_failed(details: RemoteCommandFailedDetails) -> Self {
        Self::remote_command_failed_with_details("Remote command failed", details)
    }

    pub fn remote_command_failed_with_details(
        message: impl Into<String>,
        details: impl Serialize,
    ) -> Self {
        Self::new(ErrorCode::RemoteCommandFailed, message, to_details(details))
    }

    pub fn git_command_failed(message: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::GitCommandFailed,
            message,
            Value::Object(serde_json::Map::new()),
        )
    }

    pub fn git_command_failed_with_details(
        message: impl Into<String>,
        details: GitCommandFailedDetails,
    ) -> Self {
        Self::new(ErrorCode::GitCommandFailed, message, to_details(details))
    }

    pub fn config_missing_key(key: impl Into<String>, path: Option<String>) -> Self {
        let details = to_details(ConfigMissingKeyDetails {
            key: key.into(),
            path,
        });

        Self::new(
            ErrorCode::ConfigMissingKey,
            "Missing required configuration key",
            details,
        )
    }

    pub fn config_invalid_json(path: impl Into<String>, err: serde_json::Error) -> Self {
        let details = to_details(ConfigInvalidJsonDetails {
            path: path.into(),
            error: err.to_string(),
        });

        Self::new(
            ErrorCode::ConfigInvalidJson,
            "Invalid JSON in configuration",
            details,
        )
    }

    pub fn config_invalid_value(
        key: impl Into<String>,
        value: Option<String>,
        problem: impl Into<String>,
    ) -> Self {
        let details = to_details(ConfigInvalidValueDetails {
            key: key.into(),
            value,
            problem: problem.into(),
        });

        Self::new(
            ErrorCode::ConfigInvalidValue,
            "Invalid configuration value",
            details,
        )
    }

    pub fn config_id_collision(
        id: impl Into<String>,
        requested_type: impl Into<String>,
        existing_type: impl Into<String>,
    ) -> Self {
        let existing = existing_type.into();
        let id_str = id.into();
        let details = to_details(ConfigIdCollisionDetails {
            id: id_str.clone(),
            requested_type: requested_type.into(),
            existing_type: existing.clone(),
        });

        Self::new(
            ErrorCode::ConfigIdCollision,
            format!("ID '{}' already exists as a {}", id_str, existing),
            details,
        )
        .with_hint(format!(
            "Run 'homeboy {} rename {} <new-id>' to resolve the collision",
            existing, id_str
        ))
    }

    pub fn project_no_active(config_path: Option<String>) -> Self {
        let details = to_details(NoActiveProjectDetails { config_path });

        Self::new(ErrorCode::ProjectNoActive, "No active project set", details)
    }

    /// A generic IO failure.
    ///
    /// Storage exhaustion is split out here rather than at each of the ~1500
    /// call sites that funnel into this constructor. Those sites hand over
    /// `error.to_string()`, so this is the single place where an already
    /// stringified `ENOSPC` is still recoverable. Leaving it collapsed into
    /// `internal.io_error` is what made #10603 unreadable: every writer failed
    /// with the same undifferentiated code, so no caller could degrade and the
    /// operator was told "write failed" instead of "disk full" (#11127).
    pub fn internal_io(error: impl Into<String>, context: Option<String>) -> Self {
        let error = error.into();
        if message_reports_storage_exhaustion(&error) {
            return Self::storage_exhausted(error, context);
        }
        let details = to_details(InternalIoErrorDetails { error, context });

        Self::new(ErrorCode::InternalIoError, "IO error", details)
    }

    pub fn internal_json(error: impl Into<String>, context: Option<String>) -> Self {
        let details = to_details(InternalJsonErrorDetails {
            error: error.into(),
            context,
        });

        Self::new(ErrorCode::InternalJsonError, "JSON error", details)
    }

    pub fn internal_unexpected(error: impl Into<String>) -> Self {
        Self::new(
            ErrorCode::InternalUnexpected,
            error,
            Value::Object(serde_json::Map::new()),
        )
    }

    pub fn config(message: impl Into<String>) -> Self {
        Self::config_invalid_value("config", None, message)
    }

    pub fn with_hint(mut self, message: impl Into<String>) -> Self {
        self.hints.push(Hint {
            message: message.into(),
        });
        self
    }

    /// Attach an explicit action without inferring arguments from ambient error
    /// details. The CLI lifts this contract into its existing action envelope.
    pub fn with_action(mut self, action: ExecutableAction) -> Self {
        if !self.details.is_object() {
            self.details = serde_json::json!({ "details": self.details });
        }
        let details = self
            .details
            .as_object_mut()
            .expect("details was normalized to an object");
        let actions = match details.entry(ACTIONS_DETAILS_KEY) {
            serde_json::map::Entry::Vacant(entry) => entry.insert(Value::Array(Vec::new())),
            serde_json::map::Entry::Occupied(mut entry) if !entry.get().is_array() => {
                entry.insert(Value::Array(Vec::new()));
                entry.into_mut()
            }
            serde_json::map::Entry::Occupied(entry) => entry.into_mut(),
        }
        .as_array_mut()
        .expect("action entries are initialized as an array");
        actions.push(to_details(action));
        self
    }

    pub fn with_retryable(mut self, retryable: bool) -> Self {
        self.retryable = Some(retryable);
        self
    }

    pub fn with_contextual_hint(self) -> Self {
        match self.code {
            ErrorCode::ComponentNotFound
            | ErrorCode::ProjectNotFound
            | ErrorCode::ProjectNoActive => self.with_hint(
                "Run 'homeboy status --full' to see project context and available components",
            ),
            _ => self,
        }
    }
}

/// Storage exhaustion must be distinguishable from every other IO failure.
///
/// #10603 was diagnosed only after the fact because every writer surfaced the
/// same `internal.io_error`. These pin the classification, not the wording.
#[cfg(test)]
mod storage_exhaustion_tests {
    use super::*;

    #[test]
    fn a_storage_full_io_error_does_not_collapse_into_a_generic_io_error() {
        let full = std::io::Error::from(std::io::ErrorKind::StorageFull);

        let error = Error::from_io_error(&full, Some("write evidence".to_string()));

        assert_eq!(error.code, ErrorCode::StorageExhausted);
        assert_eq!(error.code.as_str(), "storage.exhausted");
        assert!(error.is_storage_exhausted());
        assert_eq!(error.details["context"], "write evidence");
    }

    /// ENOSPC is what an out-of-*inodes* filesystem returns too, so the raw
    /// errno path is the one that actually covers the #10603 state.
    #[cfg(unix)]
    #[test]
    fn a_raw_enospc_errno_is_classified_as_storage_exhaustion() {
        let raw = std::io::Error::from_raw_os_error(ENOSPC);

        assert!(io_error_is_storage_exhausted(&raw));
        assert!(Error::from_io_error(&raw, None).is_storage_exhausted());
    }

    /// The ~1500 existing call sites stringify before they reach this crate.
    /// If the string form were not classified, the split would be invisible
    /// everywhere it matters until #11134 lands `From<io::Error>`.
    #[test]
    fn an_already_stringified_enospc_is_still_classified() {
        let error = Error::internal_io(
            "No space left on device (os error 28)",
            Some("persist artifact".to_string()),
        );

        assert_eq!(error.code, ErrorCode::StorageExhausted);
        assert_eq!(error.details["context"], "persist artifact");
    }

    /// SQLite reports a full filesystem in its own words, and the observation
    /// store is the writer whose failure produced the deadlock.
    #[test]
    fn the_sqlite_rendering_of_a_full_filesystem_is_classified() {
        assert!(message_reports_storage_exhaustion(
            "database or disk is full"
        ));
        assert!(message_reports_storage_exhaustion("Disk quota exceeded"));
    }

    #[test]
    fn an_ordinary_io_failure_stays_a_generic_io_error() {
        let missing = std::io::Error::from(std::io::ErrorKind::NotFound);

        let typed = Error::from_io_error(&missing, None);
        let stringified = Error::internal_io("permission denied (os error 13)", None);

        assert_eq!(typed.code, ErrorCode::InternalIoError);
        assert_eq!(stringified.code, ErrorCode::InternalIoError);
        assert!(!typed.is_storage_exhausted());
    }

    /// A caller that sees this must degrade, not spin. The same write fails
    /// identically until capacity is reclaimed.
    #[test]
    fn storage_exhaustion_is_reported_as_not_retryable() {
        assert_eq!(
            Error::storage_exhausted("full", None).retryable,
            Some(false)
        );
    }

    /// The remedy has to name the *store-independent* categories. Pointing a
    /// zero-inode operator at plain `homeboy cleanup --apply` is the loop that
    /// #10603 could not escape.
    #[test]
    fn the_hint_names_a_remedy_that_survives_a_closed_observation_store() {
        let error = Error::storage_exhausted("full", None);

        let hints = error
            .hints
            .iter()
            .map(|hint| hint.message.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        assert!(hints.contains("orphaned-artifact-bytes"), "{hints}");
        assert!(hints.contains("runtime-tmp"), "{hints}");
    }

    #[test]
    fn measured_capacity_evidence_survives_into_the_details() {
        let error = Error::storage_exhausted_detailed(StorageExhaustedDetails {
            error: "reserve breached".to_string(),
            context: Some("preflight".to_string()),
            path: Some("/var/lib/homeboy".to_string()),
            available_bytes: Some(1024),
            available_inodes: Some(0),
            reserve_bytes: Some(5 * 1024 * 1024 * 1024),
            reserve_inodes: Some(100_000),
        });

        assert_eq!(error.details["available_inodes"], 0);
        assert_eq!(error.details["reserve_inodes"], 100_000);
        assert_eq!(error.details["path"], "/var/lib/homeboy");
    }

    /// Absent measurements must not serialize as nulls a consumer has to
    /// special-case.
    #[test]
    fn unmeasured_capacity_fields_are_omitted_entirely() {
        let error = Error::storage_exhausted("full", None);

        let details = error.details.as_object().expect("object details");
        assert!(!details.contains_key("available_bytes"));
        assert!(!details.contains_key("reserve_inodes"));
        assert!(!details.contains_key("context"));
    }
}

/// `?` must be able to carry a real error, and must not lose the #11188
/// storage classification on the way through (#11134).
#[cfg(test)]
mod conversion_tests {
    use super::*;

    fn io_via_question_mark(error: std::io::Error) -> Result<()> {
        Err(error)?;
        Ok(())
    }

    fn json_via_question_mark(text: &str) -> Result<Value> {
        Ok(serde_json::from_str(text)?)
    }

    /// A writer that fails the way a full or severed destination fails.
    ///
    /// Used instead of `serde_json::Error::io`, which is `#[doc(hidden)]` and
    /// documented as "Not public API". This produces a genuine io-category
    /// `serde_json::Error` through the same public path production code takes.
    struct FailingWriter {
        kind: std::io::ErrorKind,
        message: &'static str,
    }

    impl std::io::Write for FailingWriter {
        fn write(&mut self, _buffer: &[u8]) -> std::io::Result<usize> {
            Err(std::io::Error::new(self.kind, self.message))
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn json_write_failure(kind: std::io::ErrorKind, message: &'static str) -> serde_json::Error {
        serde_json::to_writer(
            FailingWriter { kind, message },
            &serde_json::json!({ "payload": true }),
        )
        .expect_err("the writer always fails")
    }

    /// The whole point: a bare `?` keeps the errno-derived text that
    /// `.map_err(|_| ...)` throws away.
    #[test]
    fn the_question_mark_conversion_preserves_the_underlying_io_message() {
        let error = io_via_question_mark(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            "keyfile is not readable",
        ))
        .expect_err("io failure converts");

        assert_eq!(error.code, ErrorCode::InternalIoError);
        assert!(
            error.details["error"]
                .as_str()
                .expect("io error text")
                .contains("keyfile is not readable"),
            "{:?}",
            error.details
        );
    }

    /// A conversion that bypassed `from_io_error` would re-collapse ENOSPC
    /// into `internal.io_error` and undo #11188 everywhere `?` is used.
    #[test]
    fn the_question_mark_conversion_still_classifies_storage_exhaustion() {
        let error = io_via_question_mark(std::io::Error::from(std::io::ErrorKind::StorageFull))
            .expect_err("storage failure converts");

        assert_eq!(error.code, ErrorCode::StorageExhausted);
        assert!(error.is_storage_exhausted());
        assert_eq!(error.retryable, Some(false));
    }

    #[cfg(unix)]
    #[test]
    fn a_raw_enospc_survives_the_question_mark_conversion() {
        let error = io_via_question_mark(std::io::Error::from_raw_os_error(ENOSPC))
            .expect_err("enospc converts");

        assert!(error.is_storage_exhausted());
    }

    #[test]
    fn a_json_syntax_failure_converts_to_a_json_error() {
        let error = json_via_question_mark("{ not json").expect_err("syntax failure converts");

        assert_eq!(error.code, ErrorCode::InternalJsonError);
        assert!(!error.details["error"]
            .as_str()
            .expect("json error text")
            .is_empty());
    }

    /// `serde_json::to_writer` on a full disk returns a `serde_json::Error`
    /// wrapping the `io::Error`. Calling that "JSON error" is how a disk-full
    /// deploy gets misread as malformed payload.
    #[test]
    fn a_json_error_wrapping_a_full_disk_is_reported_as_storage_exhaustion() {
        let wrapped =
            json_write_failure(std::io::ErrorKind::StorageFull, "no space left on device");

        assert!(wrapped.is_io(), "expected an io-category json error");
        let error = Error::from(wrapped);

        assert_eq!(error.code, ErrorCode::StorageExhausted);
        assert!(error.is_storage_exhausted());
    }

    /// An io-category `serde_json::Error` is a failed write, not bad JSON.
    #[test]
    fn a_json_error_wrapping_an_ordinary_io_failure_is_reported_as_an_io_error() {
        let wrapped = json_write_failure(std::io::ErrorKind::BrokenPipe, "provider closed stdin");

        let error = Error::from_json_error(&wrapped, Some("write provider input".to_string()));

        assert_eq!(error.code, ErrorCode::InternalIoError);
        assert_eq!(error.details["context"], "write provider input");
        assert!(error.details["error"]
            .as_str()
            .expect("io error text")
            .contains("provider closed stdin"));
    }

    /// `?` cannot attach context, so the contextful constructors must stay the
    /// preferred form for sites that know which operation failed.
    #[test]
    fn the_contextful_constructor_records_what_the_caller_was_doing() {
        let error = Error::from_io_error(
            &std::io::Error::from(std::io::ErrorKind::NotFound),
            Some("read deployment provider policy".to_string()),
        );

        assert_eq!(error.details["context"], "read deployment provider policy");
    }
}

#[cfg(test)]
mod ergonomic_constructor_tests {
    use super::*;

    #[test]
    fn invalid_argument_equals_the_verbose_none_none_form() {
        let short = Error::invalid_argument("field", "problem");
        let verbose = Error::validation_invalid_argument("field", "problem", None, None);
        assert_eq!(short.code, verbose.code);
        assert_eq!(short.message, verbose.message);
        assert_eq!(short.details, verbose.details);
    }

    #[test]
    fn invalid_argument_for_equals_the_verbose_some_id_none_form() {
        let short = Error::invalid_argument_for("field", "problem", "the-id");
        let verbose = Error::validation_invalid_argument(
            "field",
            "problem",
            Some("the-id".to_string()),
            None,
        );
        assert_eq!(short.code, verbose.code);
        assert_eq!(short.message, verbose.message);
        assert_eq!(short.details, verbose.details);
    }

    #[test]
    fn executable_action_renders_arguments_with_the_shared_posix_quoting_contract() {
        let values = ["apostrophe: it's", "two words", "$HOME;*[]"];
        let action = ExecutableAction::new(
            "test.argv",
            "test argv",
            "sh",
            std::iter::once("-c")
                .chain(std::iter::once("printf '%s\\0' \"$@\""))
                .chain(std::iter::once("action"))
                .chain(values),
            ActionSafety::ReadOnly,
        );
        let output = std::process::Command::new("sh")
            .args(["-c", &action.render_command()])
            .output()
            .expect("run rendered command");

        assert!(output.status.success());
        let actual: Vec<_> = output
            .stdout
            .split(|byte| *byte == b'\0')
            .filter(|value| !value.is_empty())
            .map(|value| std::str::from_utf8(value).expect("utf8 action argument"))
            .collect();
        assert_eq!(actual, values);
    }

    #[test]
    fn with_action_replaces_a_malformed_reserved_key_deterministically() {
        let action = ExecutableAction::new(
            "test.repair",
            "repair",
            "homeboy",
            ["status"],
            ActionSafety::ReadOnly,
        );
        let error = Error::new(
            ErrorCode::InternalUnexpected,
            "fixture",
            serde_json::json!({ ACTIONS_DETAILS_KEY: "foreign value", "kept": true }),
        )
        .with_action(action);

        assert_eq!(error.details["kept"], true);
        assert_eq!(error.details[ACTIONS_DETAILS_KEY][0]["id"], "test.repair");
        assert!(error.details[ACTIONS_DETAILS_KEY].is_array());
    }
}
