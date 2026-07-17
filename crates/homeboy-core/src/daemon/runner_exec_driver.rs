//! Runner exec-driver hook.
//!
//! The daemon's `/exec` endpoint runs a runner job as a local child process:
//! it prepares a process plan (resolving the runner, cwd, command, and secret
//! environment) and then executes that child with the daemon's job callbacks
//! (cancellation, progress, child-identity acknowledgement, output capture).
//!
//! That prepare+execute path is the ONLY runner-specific behavior left in the
//! otherwise-general daemon (which also serves health, lifecycle, file, and
//! code-analysis job endpoints). It is inverted behind this driver trait so the
//! daemon stays in `homeboy-core` while the optional runner crate supplies the
//! process-execution implementation.
//!
//! With no driver registered (single-machine install with no runner) the
//! [`NoopRunnerExecDriver`] errors clearly — the `/exec` endpoint is only
//! reached when a runner job was actually dispatched.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use serde_json::Value;

use crate::error::{Error, Result};
use crate::source_snapshot::SourceSnapshot;

/// Everything the driver needs to build a runner process plan. Mirrors the
/// daemon's `ExecRequest`, but carries the runner descriptor as raw JSON so no
/// runner type crosses the core boundary.
pub struct RunnerExecPrepareRequest {
    pub runner_id: String,
    /// The optional inline runner descriptor from the exec request body,
    /// deserialized by the driver on the runner side.
    pub runner: Option<Value>,
    pub project_id: Option<String>,
    pub cwd: Option<String>,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    /// The already-computed secret-env plan, serialized. `None` when default.
    pub secret_env_plan: Option<Value>,
    pub capture_patch: bool,
    pub raw_exec: bool,
    pub source_snapshot: Option<SourceSnapshot>,
    pub require_paths: Vec<String>,
    pub validate_require_paths_on_host: bool,
}

/// A prepared runner process, slimmed to the fields the daemon reads while it
/// drives the child. The runner descriptor itself stays inside the driver.
#[derive(Debug, Clone)]
pub struct PreparedDaemonExec {
    pub runner_id: String,
    pub cwd: String,
    pub command: Vec<String>,
    pub env: HashMap<String, String>,
    pub secret_env_names: Vec<String>,
    pub source_snapshot: SourceSnapshot,
    pub require_paths: Vec<String>,
    /// Opaque driver-owned plan handle carried back into `execute`. The daemon
    /// never inspects it; it exists so the driver can reconstruct the full
    /// runner process without re-preparing.
    plan_token: Arc<dyn std::any::Any + Send + Sync>,
}

impl PreparedDaemonExec {
    /// Construct a prepared exec. `plan_token` is the driver's private full
    /// plan; the daemon-visible fields are the process descriptor.
    pub fn new(
        runner_id: String,
        cwd: String,
        command: Vec<String>,
        env: HashMap<String, String>,
        secret_env_names: Vec<String>,
        source_snapshot: SourceSnapshot,
        require_paths: Vec<String>,
        plan_token: Arc<dyn std::any::Any + Send + Sync>,
    ) -> Self {
        Self {
            runner_id,
            cwd,
            command,
            env,
            secret_env_names,
            source_snapshot,
            require_paths,
            plan_token,
        }
    }

    /// The driver-owned opaque plan handle.
    pub fn plan_token(&self) -> &Arc<dyn std::any::Any + Send + Sync> {
        &self.plan_token
    }
}

/// The output of running a runner child, slimmed to what the daemon serializes.
#[derive(Debug, Clone, Default)]
pub struct DaemonExecOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
    /// Serialized `RunnerResourceMetrics`, if any.
    pub metrics: Option<Value>,
    /// Serialized command-capture metadata, if any.
    pub capture: Option<Value>,
}

/// A cancellation probe the driver polls while the child runs.
pub type ExecCancellationProbe = Box<dyn FnMut() -> bool + Send>;
/// A progress sink the driver invokes with structured progress events.
pub type ExecProgressSink = Arc<dyn Fn(Value) -> Result<()> + Send + Sync + 'static>;
/// A child-started acknowledgement callback invoked with the child pid.
pub type ExecChildStarted = Arc<dyn Fn(u32) -> Result<()> + Send + Sync + 'static>;

/// The runner process prepare+execute contract the daemon depends on.
pub trait RunnerExecDriver: Send + Sync {
    /// Prepare a runner process plan from the exec request.
    fn prepare(&self, request: RunnerExecPrepareRequest) -> Result<PreparedDaemonExec>;

    /// Execute a prepared runner child, driving it with the daemon's
    /// cancellation, progress, and child-identity callbacks.
    ///
    /// The daemon may mutate `prepared.env` (e.g. to inject the child
    /// reservation id) between `prepare` and `execute`, so implementations MUST
    /// honor `prepared.env` rather than any environment captured at prepare
    /// time.
    fn execute(
        &self,
        prepared: &PreparedDaemonExec,
        is_cancelled: ExecCancellationProbe,
        progress_sink: Option<ExecProgressSink>,
        require_child_identity_acknowledgement: bool,
        child_started: Option<ExecChildStarted>,
    ) -> Result<DaemonExecOutput>;
}

/// Default driver used when no runner layer is registered.
struct NoopRunnerExecDriver;

const NO_DRIVER: &str = "runner subsystem is unavailable: cannot execute a runner exec job";

impl RunnerExecDriver for NoopRunnerExecDriver {
    fn prepare(&self, _request: RunnerExecPrepareRequest) -> Result<PreparedDaemonExec> {
        Err(Error::internal_unexpected(NO_DRIVER))
    }

    fn execute(
        &self,
        _prepared: &PreparedDaemonExec,
        _is_cancelled: ExecCancellationProbe,
        _progress_sink: Option<ExecProgressSink>,
        _require_child_identity_acknowledgement: bool,
        _child_started: Option<ExecChildStarted>,
    ) -> Result<DaemonExecOutput> {
        Err(Error::internal_unexpected(NO_DRIVER))
    }
}

fn driver_slot() -> &'static Mutex<Option<Arc<dyn RunnerExecDriver>>> {
    static DRIVER: Mutex<Option<Arc<dyn RunnerExecDriver>>> = Mutex::new(None);
    &DRIVER
}

/// Resolve the active driver, cloning the `Arc` so the registry lock is not
/// held while a (potentially long-running) child process executes.
fn active_driver() -> Arc<dyn RunnerExecDriver> {
    let slot = driver_slot().lock().expect("runner exec driver lock");
    match slot.as_ref() {
        Some(driver) => Arc::clone(driver),
        None => Arc::new(NoopRunnerExecDriver),
    }
}

/// Register the runner exec driver. Called once at startup by the runner layer.
pub fn register_runner_exec_driver(driver: Arc<dyn RunnerExecDriver>) {
    let mut slot = driver_slot().lock().expect("runner exec driver lock");
    *slot = Some(driver);
}

/// Prepare a runner process via the registered driver (or the no-op driver).
pub(crate) fn prepare_exec(request: RunnerExecPrepareRequest) -> Result<PreparedDaemonExec> {
    active_driver().prepare(request)
}

/// Execute a prepared runner child via the registered driver. The registry lock
/// is released before the child runs.
pub(crate) fn execute_exec(
    prepared: &PreparedDaemonExec,
    is_cancelled: ExecCancellationProbe,
    progress_sink: Option<ExecProgressSink>,
    require_child_identity_acknowledgement: bool,
    child_started: Option<ExecChildStarted>,
) -> Result<DaemonExecOutput> {
    active_driver().execute(
        prepared,
        is_cancelled,
        progress_sink,
        require_child_identity_acknowledgement,
        child_started,
    )
}
