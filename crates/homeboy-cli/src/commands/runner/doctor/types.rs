use super::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RunnerDoctorStatus {
    Ok,
    #[serde(rename = "warn")]
    Warning,
    Error,
}

impl RunnerDoctorStatus {
    /// The process result for a completed doctor report. Warnings remain
    /// diagnostic-only; error-level checks make the runner not ready.
    pub const fn operational_exit_code(self) -> i32 {
        match self {
            Self::Ok | Self::Warning => 0,
            Self::Error => 1,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct RunnerDoctorOutput {
    pub variant: &'static str,
    pub command: &'static str,
    pub runner_id: String,
    pub runner: RunnerTargetSummary,
    pub status: RunnerDoctorStatus,
    /// The selected, actionable root cause when doctor reports a nonzero
    /// readiness result. This is lifted into the command-result envelope.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<RunnerDoctorFailure>,
    pub capabilities: RunnerCapabilities,
    pub resources: RunnerResources,
    pub checks: Vec<RunnerCheck>,
    /// Value-free legacy migration plan. Entries contain only key names,
    /// locations, and secret references; never resolved values.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub secret_env_migration: Option<homeboy::runner::runners::RunnerSecretEnvMigrationPlan>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<RunnerDoctorDiagnostics>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub daemon_recovery: Option<homeboy::core::daemon::DaemonFreshnessReport>,
    /// The same retained-job ownership projection used by status and reconcile.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub admission_summary: Option<homeboy::runner::runners::RunnerAdmissionSummary>,
    /// Provider-specific Lab readiness, separate from runner substrate checks.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider_readiness: Option<RunnerDoctorProviderReadiness>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub repairs: Vec<RunnerRepair>,
}

#[derive(Debug, Serialize)]
pub struct RunnerDoctorFailure {
    pub code: String,
    pub message: String,
    pub details: BTreeMap<String, String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub next_actions: Vec<crate::commands::utils::response::CommandNextAction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retryable: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct RunnerDoctorDiagnostics {
    pub status: &'static str,
    pub completed_checks: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub timed_out_probes: Vec<RunnerDoctorTimedOutProbe>,
}

#[derive(Debug, Serialize)]
pub struct RunnerDoctorProviderReadiness {
    /// Providers with a selected, successful provider-owned live auth check.
    pub ready_for: Vec<String>,
    /// Providers whose selected live auth check failed or whose runner substrate failed.
    pub blocked_for: Vec<String>,
    /// Providers whose runtime/load checks may have passed but did not run a
    /// provider-owned live auth check, so they are not dispatch-ready.
    pub unverified_for: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub unverified_remediation: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct RunnerDoctorTimedOutProbe {
    pub reason_code: String,
    pub command: String,
    pub replay_command: String,
}

#[derive(Debug, Serialize)]
pub struct RunnerRepair {
    pub id: String,
    pub status: RunnerDoctorStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct RunnerTargetSummary {
    #[serde(rename = "type")]
    pub target_type: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registry: Option<RunnerRegistrySummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub server: Option<RunnerServerSummary>,
}

#[derive(Debug, Serialize)]
pub struct RunnerRegistrySummary {
    pub id: String,
    pub kind: RunnerKind,
}

#[derive(Debug, Serialize)]
pub struct RunnerServerSummary {
    pub id: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub is_localhost: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct RunnerCapabilities {
    pub local_execution: bool,
    pub ssh_execution: bool,
    pub homeboy_available: bool,
    pub workspace_writable: bool,
    pub artifact_store_available: bool,
}

#[derive(Debug, Default, Serialize)]
pub struct RunnerResources {
    pub homeboy: HomeboyProbe,
    pub system: SystemProbe,
    pub cpu: CpuProbe,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub memory: Option<MemoryProbe>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskProbe>,
    pub workspace_root: String,
    pub artifact_root: String,
    pub tools: BTreeMap<String, ToolProbe>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub declared_tools: BTreeMap<String, BTreeMap<String, ToolProbe>>,
}

#[derive(Debug, Default, Serialize)]
pub struct HomeboyProbe {
    pub version: String,
    pub path: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct SystemProbe {
    pub os: String,
    pub arch: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kernel: Option<String>,
}

#[derive(Debug, Default, Serialize)]
pub struct CpuProbe {
    pub count: usize,
}

#[derive(Debug, Serialize)]
pub struct MemoryProbe {
    pub total_mb: u64,
    pub available_mb: Option<u64>,
}

#[derive(Debug, Serialize)]
pub struct DiskProbe {
    pub path: String,
    pub total_mb: u64,
    pub available_mb: u64,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolProbe {
    pub available: bool,
    /// True when the lookup itself could not run, so absence was never
    /// established. `available: false` alone cannot carry this: reporting a
    /// broken probe as "not found on PATH" is the readiness lie #14374 is about.
    #[serde(skip_serializing_if = "is_false")]
    pub probe_failed: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn is_false(value: &bool) -> bool {
    !*value
}

impl ToolProbe {
    pub fn found(path: String, version: Option<String>) -> Self {
        Self {
            available: true,
            probe_failed: false,
            path: Some(path),
            version,
            error: None,
        }
    }

    pub fn not_found() -> Self {
        Self {
            available: false,
            probe_failed: false,
            path: None,
            version: None,
            error: Some(TOOL_NOT_FOUND_ERROR.to_string()),
        }
    }

    pub fn probe_failed(reason: String) -> Self {
        Self {
            available: false,
            probe_failed: true,
            path: None,
            version: None,
            error: Some(reason),
        }
    }
}

pub const TOOL_NOT_FOUND_ERROR: &str = "not found on PATH";

#[derive(Debug, Serialize)]
pub struct RunnerCheck {
    pub id: String,
    pub status: RunnerDoctorStatus,
    pub message: String,
    /// The fix, written for an operator to read.
    ///
    /// Kept alongside [`RunnerCheck::remediation_action`] rather than replaced
    /// by it. These are two vocabularies, not two formats of one value: a
    /// person reading a failed check needs the sentence and its fallbacks, and
    /// a repair driver needs the arguments. Deriving one from the other is how
    /// the current defect arose -- the action was computed, formatted into
    /// prose, and then only a human could execute it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
    /// The same fix, in a form something other than a person can run.
    ///
    /// `None` means no automatic repair is known for this check, which is not
    /// the same as no remediation: plenty of checks are fixed by a human
    /// decision that homeboy should not make.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation_action: Option<RunnerRepairAction>,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub details: BTreeMap<String, String>,
}

impl RunnerCheck {
    /// Attach the machine-runnable form of this check's remediation.
    ///
    /// A builder rather than a sixth constructor parameter: the four `checks::`
    /// constructors are called from dozens of sites that have no action to
    /// give, and widening their signatures would edit all of them to pass
    /// `None`.
    pub(crate) fn with_action(mut self, action: RunnerRepairAction) -> Self {
        self.remediation_action = Some(action);
        self
    }
}

/// A repair a driver may perform on a runner without asking.
///
/// Closed on purpose. Each variant corresponds to an executor that already
/// exists in `super::repair`, so this type is the missing link between a check
/// and a capability rather than a new capability of its own. Keeping it closed
/// means the set of things an unattended repair loop may do is reviewable, and
/// cannot grow by someone formatting a new command into a string.
///
/// Adding a variant is therefore a deliberate decision about what homeboy is
/// allowed to do to a runner on its own.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum RunnerRepairAction {
    /// Materialize the controller's Homeboy build onto the runner and
    /// reconnect. Executor: `repair::refresh_outcome`.
    RefreshHomeboy {
        /// The git ref to materialize. `None` lets the executor choose.
        #[serde(skip_serializing_if = "Option::is_none")]
        git_ref: Option<String>,
        allow_downgrade: bool,
    },
    /// Re-establish the runner session. Executor: `repair::connect_outcome`.
    Reconnect,
    /// Re-materialize provider-declared managed source checkouts.
    /// Executor: `repair::repair_managed_sources`.
    RefreshManagedSources,
}
