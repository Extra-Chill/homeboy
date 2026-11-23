mod admission;
pub use admission::{lab_admission_owner, lab_parent_job_id};
pub mod data_inputs;
pub mod execution_start;
pub mod failure_excerpt;
pub mod placement;
pub mod telemetry;
pub mod verdict;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use fs4::fs_std::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use homeboy_core::gate::{
    HomeboyGateKind, HomeboyGateResult, HomeboyGateRevealPolicy, HomeboyGateStatus,
    HomeboyGateVisibility,
};
use homeboy_core::plan::{PlanStep, PlanStepStatus, PlanValues};
use homeboy_core::{Error, Result};

use crate::agent_task_process_containment::AgentTaskProcessContainment;

// `Skipped` is a new durable terminal state with a structured blocker. Keep it
// out of v1 so typed consumers can distinguish the expanded state machine.
pub const AGENT_TASK_GATE_REPORT_SCHEMA: &str = "homeboy/agent-task-gate-report/v3";
const XDG_ENV_VARS: &[&str] = &[
    "XDG_CONFIG_HOME",
    "XDG_CACHE_HOME",
    "XDG_DATA_HOME",
    "XDG_STATE_HOME",
    "XDG_RUNTIME_DIR",
];
const PLAYWRIGHT_CACHE_SUBPATH: &str = if cfg!(target_os = "macos") {
    "Library/Caches/ms-playwright"
} else {
    ".cache/ms-playwright"
};

/// Identity a Cook supervisor process holds for itself. A process it spawns --
/// a step job or a deterministic gate -- is an ordinary invocation, so none of
/// these may reach it: a gate running Homeboy (or Homeboy's own tests) would
/// otherwise stamp its submissions with the supervisor's job.
pub(crate) const COOK_SUPERVISOR_PRIVATE_ENV_VARS: &[&str] = &[
    homeboy_core::daemon::step_job::LOCAL_COOK_SUPERVISOR_JOB_ID_ENV,
    "HOMEBOY_COOK_STARTUP_LAUNCHER_ID",
    "HOMEBOY_AGENT_TASK_PROVIDER_LAUNCH_CONTEXT_JSON",
];

/// Temp-dir variables pinned to the invocation temp dir so every gate process
/// — preflight probes included — shares one temporary root.
const TMPDIR_ENV_VARS: &[&str] = &["TMPDIR", "TEMP", "TMP"];
const GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES: usize = 64 * 1024;
const RUST_CACHE_SCHEMA: &str = "homeboy/gate-rust-cache/v2";
const RUST_CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(120);
// Cold toolchain acquisition is network work, not lock contention. Give it a
// separate bounded setup budget so lock wait cannot abort valid installations.
const RUST_CACHE_HYDRATION_TIMEOUT: Duration = Duration::from_secs(900);

pub type AgentTaskGateVisibility = HomeboyGateVisibility;
pub type AgentTaskGateRevealPolicy = HomeboyGateRevealPolicy;
type GateSpawnCallback = Arc<dyn Fn(u32, &str) -> Result<()> + Send + Sync>;
type GateHeartbeatCallback = Arc<dyn Fn(&AgentTaskGateLiveStatus) -> Result<()> + Send + Sync>;

pub struct GateSupervision {
    pub timeout: Duration,
    pub no_progress_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub on_spawn: GateSpawnCallback,
    pub on_heartbeat: GateHeartbeatCallback,
    pub is_cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// Process controls that differ between ordinary, durable, and immutable-base
/// gate runs. Declared plans always supply a timeout at this boundary.
struct GateExecution<'a> {
    runtime_tmpdir: Option<&'a Path>,
    timeout: Option<Duration>,
    supervision: Option<&'a GateSupervision>,
    baseline_timeout_diagnostic: bool,
    /// Absolute end of the whole gate. A Lab gate's dependency setup and its
    /// command share one budget, so the command's wall-clock limit (even a
    /// declared plan's own suite timeout) never runs past it (#16938).
    deadline: Option<Instant>,
}

/// Shared deterministic-gate verification fields used by every agent-task
/// options/report type that runs `--verify` / `--private-verify` gates. Embed
/// this via `#[serde(flatten)]` so the serialized JSON keeps the historical
/// flat `verify` / `private_verify` / `private_gate_reveal` shape while the
/// field group lives in exactly one place.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyGateOptions {
    /// Stop before provider dispatch when any base gate is red.
    #[serde(default)]
    pub baseline_policy: BaselinePolicy,
    /// Defer full verification to required PR checks after local gates pass.
    #[serde(default)]
    pub verify_ci: bool,
    /// Deterministic verification commands run after each promotion/apply.
    #[serde(default)]
    pub verify: Vec<String>,
    /// Private deterministic verification commands whose full failure detail is
    /// gated from follow-up agents.
    #[serde(default)]
    pub private_verify: Vec<String>,
    /// Repro gates: commands that must fail on the Cook's base before provider
    /// work starts and pass on the candidate. Cook also runs each one as a
    /// visible candidate gate. Empty means the declared gates do not prove the
    /// task's issue (`repro: none`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub repro: Vec<String>,
    /// A declared adapter-owned test plan. This stays typed in Cook recipes;
    /// `verify` and `private_verify` are the explicit legacy shell paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_execution_plan: Option<homeboy_engine_primitives::test_execution::TestExecutionPlan>,
    /// Provenance for each gate input captured by the controller before Cook
    /// becomes durable. Commands remain the execution contract for compatibility.
    #[serde(default)]
    pub input_sources: Vec<AgentTaskGateInputSource>,
    /// Feedback policy for failed private gates.
    #[serde(default = "default_private_gate_reveal")]
    pub private_gate_reveal: AgentTaskGateRevealPolicy,
    /// Ordered gates stop after the first failure unless exhaustive verification
    /// is explicitly requested. Persist this policy with Cook recipes.
    #[serde(default)]
    pub execution_policy: AgentTaskGateExecutionPolicy,
    /// Maximum duration for each deterministic gate. Persisted in cook recipes
    /// so adoption never silently changes its historical verification policy.
    #[serde(default = "default_gate_timeout_seconds")]
    pub gate_timeout_seconds: u64,
    /// Cadence for durable liveness and bounded output-tail updates.
    #[serde(default = "default_gate_heartbeat_interval_seconds")]
    pub gate_heartbeat_interval_seconds: u64,
    /// Maximum time a gate may go without a liveness signal. Any output, or
    /// CPU/I/O work by the gate's process tree, counts until the gate emits its
    /// first `HOMEBOY_PROGRESS` marker; after that, only markers count. Zero
    /// disables the no-progress deadline.
    #[serde(default = "default_gate_no_progress_timeout_seconds")]
    pub gate_no_progress_timeout_seconds: u64,
    /// Completed adoption gates are reused by default; a recipe must opt in to
    /// rerunning them after restart.
    #[serde(default)]
    pub rerun_completed_gates: bool,

    /// Declarative, non-secret process environment policy for every gate.
    #[serde(default)]
    pub gate_environment: AgentTaskGateEnvironmentPolicy,
    /// Required tools initialized in the final isolated environment before a
    /// provider can spend an execution budget.
    #[serde(default)]
    pub gate_toolchains: Vec<AgentTaskGateToolchainRequirement>,
    /// Caller-owned package resources required by deterministic gates. Homeboy
    /// validates declared paths and records provenance without interpreting the
    /// package, artifact, or remediation semantics.
    #[serde(default)]
    pub gate_package_artifacts: Vec<AgentTaskGatePackageArtifactRequirement>,
    /// Explicit mappings for producer-owned diagnostic sidecars. Homeboy only
    /// consumes declared schemas and paths; producer semantics remain opaque.
    #[serde(default)]
    pub gate_diagnostic_sidecars: Vec<AgentTaskGateDiagnosticSidecarMapping>,
    /// Hydrate provider-declared dependency roots in the isolated candidate
    /// checkout before deterministic verification.
    #[serde(default = "default_hydrate_dependencies")]
    pub hydrate_dependencies: bool,
}

/// Admission policy for known failures on the immutable Cook base.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BaselinePolicy {
    Strict,
    #[default]
    NewFailuresOnly,
}

/// Controller-captured provenance for an inline or file-backed gate program.
/// Private sources intentionally retain only path-independent metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateInputSource {
    pub visibility: AgentTaskGateVisibility,
    pub source_kind: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    pub sha256: String,
    pub size_bytes: u64,
    pub redaction_policy: AgentTaskGateRevealPolicy,
}

impl AgentTaskGateInputSource {
    /// Reviewer-facing label for a visible file-backed gate: the file name the
    /// controller snapshotted at admission. Inline and private sources have none.
    pub fn file_label(&self) -> Option<String> {
        if self.source_kind != "file" || self.visibility != AgentTaskGateVisibility::Visible {
            return None;
        }
        let path = self.path.as_deref()?;
        Path::new(path)
            .file_name()
            .map(|name| name.to_string_lossy().trim().to_string())
            .filter(|name| !name.is_empty())
    }

    fn describes_program(&self, program: &str) -> bool {
        self.sha256 == format!("sha256:{:x}", Sha256::digest(program.as_bytes()))
    }
}

/// Carry each visible `--verify-file` source's file name onto the gate report
/// that executed its program, matched by the admission-time content digest.
pub fn apply_gate_input_source_labels(
    gates: &mut [AgentTaskGateReport],
    sources: &[AgentTaskGateInputSource],
) {
    for gate in gates {
        if gate.visibility != AgentTaskGateVisibility::Visible {
            continue;
        }
        let Ok(invocation) = gate.invocation() else {
            continue;
        };
        let program = invocation.reviewer_command();
        if let Some(label) = sources
            .iter()
            .filter(|source| source.describes_program(&program))
            .find_map(AgentTaskGateInputSource::file_label)
        {
            gate.label = Some(label);
        }
    }
}

fn default_hydrate_dependencies() -> bool {
    true
}

/// Scheduling policy for a declared sequence of deterministic gates.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateExecutionPolicy {
    /// Each gate is a prerequisite for subsequent gates.
    #[default]
    OrderedFailFast,
    /// Run every gate even after failures, for independent or exhaustive suites.
    ContinueAll,
}

/// Whether a gate starts with the caller's environment or an empty one.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateEnvironmentMode {
    #[default]
    Inherit,
    Replace,
}

/// A portable gate environment contract. `variables` are deliberate non-secret
/// inputs; secrets remain outside reports and this policy.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateEnvironmentPolicy {
    /// Explicit controller-owned Lab placement. Omission preserves local gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lab_runner: Option<String>,
    #[serde(default)]
    pub mode: AgentTaskGateEnvironmentMode,
    #[serde(default)]
    pub variables: BTreeMap<String, String>,
    /// Explicit host environment sources retained for a required toolchain.
    /// A source may include a relative suffix, for example `HOME/.toolchain`.
    #[serde(default)]
    pub preserve: BTreeMap<String, String>,
    #[serde(default)]
    pub isolate_home: bool,
    #[serde(default)]
    pub isolate_xdg: bool,
    /// Rust cache hydration follows the enclosing dependency-hydration policy.
    #[serde(default = "default_hydrate_dependencies")]
    pub hydrate_rust_cache: bool,
    /// Runner-side dependency hydration follows the enclosing dependency-
    /// hydration policy: a Lab gate hydrates the materialized runner workspace
    /// before its command runs, with setup evidence on the receipt (#16262).
    #[serde(default = "default_hydrate_dependencies")]
    pub hydrate_dependencies: bool,
    /// Explicit gate override. When omitted, inherit the component's declared
    /// managed-execution policy for the gate workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_cargo_target: Option<bool>,
    /// Component identity recorded at Cook admission. Gate resolution uses this
    /// instead of re-deriving a component from the worktree path.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admitted_component_id: Option<String>,
    /// Cook-admitted repository-relative package path, used when the gate host
    /// has no component registration. Empty means the repository root.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub component_cwd: Option<String>,
    /// Selected extension sources are copied under the isolated HOME. Gates
    /// never receive a writable path to the controller-owned source.
    #[serde(default)]
    pub extension_inputs: Vec<AgentTaskGateExtensionInput>,
    /// Controller install sources for the gated components' declared
    /// extensions. A gate host missing one installs it from the same source
    /// and revision through the ordinary extension install path (#17769).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_sources: Vec<AgentTaskGateExtensionSource>,
    #[serde(default)]
    pub data_inputs: Vec<data_inputs::GateInput>,
    #[serde(default)]
    pub requirements: AgentTaskGateRequirements,
}

/// Where the controller installed a declared extension from, and at which
/// revision.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateExtensionSource {
    pub id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl AgentTaskGateExtensionSource {
    /// The command that provisions this extension on a gate host.
    fn provision_command(id: &str, source: Option<&Self>) -> String {
        match source {
            Some(source) => {
                let revision = source
                    .revision
                    .as_deref()
                    .map(|revision| format!(" --revision {revision}"))
                    .unwrap_or_default();
                format!(
                    "homeboy extension install {} --id {id}{revision}",
                    source.source
                )
            }
            None => format!("homeboy extension install <source> --id {id}"),
        }
    }
}

/// Explicit host inputs. Tool mappings belong to installed extensions.
/// Secret values are resolved at execution, never stored in policy.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskGateRequirements {
    #[serde(default)]
    pub host_tools: Vec<String>,
    #[serde(default)]
    pub secrets: Vec<String>,
}

/// A required executable and its non-mutating initialization probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateToolchainRequirement {
    pub command: String,
    #[serde(default = "default_toolchain_probe_arguments")]
    pub probe_arguments: Vec<String>,
    /// Resolve the executable on PATH without running it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolve_only: bool,
}

/// A caller-declared resource selected by a package or extension.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGatePackageArtifactRequirement {
    pub id: String,
    pub environment: AgentTaskGateArtifactEnvironmentMapping,
    #[serde(default)]
    pub required_paths: Vec<AgentTaskGateArtifactPathRequirement>,
    /// Opaque caller metadata returned with failures and gate provenance.
    pub remediation: serde_json::Value,
}

/// An extension directory selected as a gate input. `source` is deliberately
/// explicit so gate execution cannot broaden to ambient inputs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateExtensionInput {
    pub id: String,
    pub source: String,
    /// Optional identity of the extension and declared shared closure pinned by
    /// a prior candidate gate. Baselines must not compare different inputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
}

/// An explicit environment mapping for a package resource. `source` reads a
/// host setting; `default` supplies a stable value when no host setting is used.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateArtifactEnvironmentMapping {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<String>,
}

/// A resource path relative to the gate workspace, optionally pinned by SHA-256.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateArtifactPathRequirement {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

fn default_toolchain_probe_arguments() -> Vec<String> {
    vec!["--version".to_string()]
}

impl Default for AgentTaskGateEnvironmentPolicy {
    fn default() -> Self {
        Self {
            lab_runner: None,
            mode: AgentTaskGateEnvironmentMode::Inherit,
            variables: BTreeMap::new(),
            preserve: BTreeMap::new(),
            isolate_home: true,
            isolate_xdg: true,
            hydrate_rust_cache: true,
            hydrate_dependencies: true,
            shared_cargo_target: None,
            admitted_component_id: None,
            component_cwd: None,
            extension_inputs: Vec::new(),
            extension_sources: Vec::new(),
            data_inputs: Vec::new(),
            requirements: AgentTaskGateRequirements::default(),
        }
    }
}

fn default_private_gate_reveal() -> AgentTaskGateRevealPolicy {
    AgentTaskGateRevealPolicy::SummaryOnly
}

fn default_gate_timeout_seconds() -> u64 {
    30 * 60
}
fn default_gate_heartbeat_interval_seconds() -> u64 {
    5
}
fn default_gate_no_progress_timeout_seconds() -> u64 {
    5 * 60
}

/// Warning for a Cook that declares no repro gate: nothing shows its gates
/// fail on base, so a passing candidate does not prove the issue is fixed.
pub const NO_REPRO_GATE_WARNING: &str = "no --repro gate declared: the declared gates are not shown to fail on base, so they do not prove this issue is fixed (repro: none). Add --repro <command> (or --repro-file <path>) that fails on base and passes once the issue is fixed";

impl VerifyGateOptions {
    /// Run every repro gate as a visible candidate gate too. Idempotent, so a
    /// reconstructed recipe that already carries them is unchanged.
    pub fn include_repro_in_candidate_gates(&mut self) {
        for command in &self.repro {
            if !self.verify.contains(command) {
                self.verify.push(command.clone());
            }
        }
    }

    /// Check declared gate toolchains and targets in their execution placement
    /// before admitting a run. Runner-backed environments use the runner probe.
    pub fn preflight(&self, workspace: &Path) -> Result<()> {
        preflight_verify_gate_options(workspace, self)
    }

    pub fn gate_timeout(&self) -> Duration {
        Duration::from_secs(self.gate_timeout_seconds.max(1))
    }

    pub fn gate_heartbeat_interval(&self) -> Duration {
        Duration::from_secs(self.gate_heartbeat_interval_seconds.max(1))
    }

    /// The no-progress window. Zero disables the no-progress deadline, so only
    /// the wall-clock gate timeout bounds the gate.
    pub fn gate_no_progress_timeout(&self) -> Duration {
        Duration::from_secs(self.gate_no_progress_timeout_seconds)
    }

    /// Supervision carrying exactly the declared gate budgets with no-op
    /// callbacks. Verified-base differential replays and replacement gates run
    /// under this scope so a Lab runner enforces the operator's declared
    /// budgets instead of its built-in defaults (#15856).
    pub(crate) fn declared_supervision(&self) -> GateSupervision {
        GateSupervision {
            timeout: self.gate_timeout(),
            no_progress_timeout: self.gate_no_progress_timeout(),
            heartbeat_interval: self.gate_heartbeat_interval(),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new(|_| Ok(())),
            is_cancelled: Arc::new(|| false),
        }
    }

    /// Explicit initialization probes supplement inferred executable discovery.
    pub(crate) fn required_toolchains(&self) -> Vec<AgentTaskGateToolchainRequirement> {
        self.gate_toolchains.clone()
    }

    /// Resolution probes for declared gates that are nested `homeboy review
    /// test` invocations, paired with the gate each one stands for. Such a gate
    /// fails before it checks any code when its component or test capability
    /// does not resolve in the isolated gate environment (Extra-Chill/homeboy#16157),
    /// so the same command with `--preflight-only` decides it before a provider
    /// is paid for (Extra-Chill/homeboy#16159).
    pub(crate) fn gate_target_probes(&self) -> Vec<(String, AgentTaskGateToolchainRequirement)> {
        self.verify
            .iter()
            .chain(&self.private_verify)
            .filter_map(|command| {
                homeboy_review_test_probe(command).map(|probe| (command.clone(), probe))
            })
            .collect()
    }

    /// Reject repository-owned npm gate declarations that no candidate patch can
    /// repair before a provider is admitted.
    pub(crate) fn preflight_declarations(&self, workspace: &Path) -> Result<()> {
        for command in self.verify.iter().chain(&self.private_verify) {
            let Some(script) = npm_run_script(command) else {
                continue;
            };
            let manifest_path = workspace.join("package.json");
            let manifest = fs::read_to_string(&manifest_path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(manifest_path.display().to_string()))
            })?;
            let manifest: serde_json::Value = serde_json::from_str(&manifest).map_err(|error| {
                Error::validation_invalid_argument(
                    "gate declaration",
                    format!("invalid package manifest: {error}"),
                    Some(manifest_path.display().to_string()),
                    None,
                )
            })?;
            if manifest.pointer(&format!("/scripts/{script}")).is_some() {
                continue;
            }
            let package = manifest
                .get("name")
                .and_then(serde_json::Value::as_str)
                .unwrap_or("unnamed package");
            let remediation = format!(
                "Add \"{script}\": \"<command>\" to {}/scripts for package `{package}`, or change/remove the declared Cook gate `{command}`.",
                manifest_path.display(),
            );
            let mut error = Error::validation_invalid_argument(
                "gate declaration",
                format!(
                    "declared npm gate `{command}` is invalid for package `{package}`: {} has no `scripts.{script}`. {remediation}",
                    manifest_path.display(),
                ),
                Some(manifest_path.display().to_string()),
                None,
            );
            error.details = json!({
                "failure_classification": "gate_declaration",
                "command": command,
                "package": package,
                "manifest": manifest_path,
                "missing_script": script,
                "remediation": remediation,
            });
            return Err(error);
        }
        Ok(())
    }

    pub(crate) fn has_npm_run_declaration(&self) -> bool {
        self.verify
            .iter()
            .chain(&self.private_verify)
            .any(|command| npm_run_script(command).is_some())
    }
}

/// `homeboy review test <args>` as an argv probe that stops after resolution.
/// Only plain word lists qualify: a gate with shell syntax is a program the
/// probe cannot reproduce faithfully, so it is left to run as declared.
fn homeboy_review_test_probe(command: &str) -> Option<AgentTaskGateToolchainRequirement> {
    if command
        .chars()
        .any(|character| "|&;<>()$`\\\"'*?[]{}~#".contains(character))
    {
        return None;
    }
    let tokens: Vec<&str> = command.split_whitespace().collect();
    if tokens.len() < 3 || tokens[..3] != ["homeboy", "review", "test"] {
        return None;
    }
    let passthrough = tokens
        .iter()
        .position(|token| *token == "--")
        .unwrap_or(tokens.len());
    let mut probe_arguments: Vec<String> = tokens[1..passthrough]
        .iter()
        .map(|token| token.to_string())
        .collect();
    probe_arguments.push("--preflight-only".to_string());
    Some(AgentTaskGateToolchainRequirement {
        resolve_only: false,
        command: tokens[0].to_string(),
        probe_arguments,
    })
}

fn npm_run_script(command: &str) -> Option<&str> {
    let tokens: Vec<_> = command.split_whitespace().collect();
    if tokens.len() == 3 && tokens[0] == "npm" && tokens[1] == "run" && !tokens[2].starts_with('-')
    {
        Some(tokens[2])
    } else {
        None
    }
}

impl Default for VerifyGateOptions {
    fn default() -> Self {
        Self {
            baseline_policy: BaselinePolicy::default(),
            verify_ci: false,
            verify: Vec::new(),
            private_verify: Vec::new(),
            repro: Vec::new(),
            test_execution_plan: None,
            input_sources: Vec::new(),
            private_gate_reveal: default_private_gate_reveal(),
            execution_policy: AgentTaskGateExecutionPolicy::OrderedFailFast,
            gate_timeout_seconds: default_gate_timeout_seconds(),
            gate_heartbeat_interval_seconds: default_gate_heartbeat_interval_seconds(),
            gate_no_progress_timeout_seconds: default_gate_no_progress_timeout_seconds(),
            rerun_completed_gates: false,

            gate_environment: AgentTaskGateEnvironmentPolicy::default(),
            gate_toolchains: Vec::new(),
            gate_package_artifacts: Vec::new(),
            gate_diagnostic_sidecars: Vec::new(),
            hydrate_dependencies: default_hydrate_dependencies(),
        }
    }
}

pub type AgentTaskGateSetupEvidence = homeboy_core::deps::DependencyHydrationOutcome;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskGateReport {
    /// Infrastructure warning after a completed root; does not replace its verdict.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cleanup_warning: Option<homeboy_engine_primitives::command::ExecutionCleanupWarning>,
    /// Durable original verdict and exact execution identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verdict: Option<verdict::GateVerdictEvidence>,
    /// Verified native runner admission and candidate-bound terminal evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lab_receipt: Option<serde_json::Value>,
    #[serde(default = "gate_report_schema")]
    pub schema: String,
    #[serde(skip, default = "default_gate_step")]
    pub step: PlanStep,
    pub id: String,
    #[serde(default)]
    pub visibility: AgentTaskGateVisibility,
    #[serde(default)]
    pub reveal_policy: AgentTaskGateRevealPolicy,
    pub status: AgentTaskGateStatus,
    pub command: Vec<String>,
    /// The durable execution contract. Older reports only have `command`; they
    /// are decoded from their recorded shell argv by `invocation()`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invocation: Option<AgentTaskGateInvocation>,
    pub exit_code: i32,
    /// Terminal classification distinguishes a command's semantic exit from
    /// controller wall-clock or progress-deadline termination.
    #[serde(default)]
    pub termination: AgentTaskGateTermination,
    /// Signal that terminated the gate process, when available.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal: Option<i32>,
    /// Adapter-owned terminal result for a declared test plan. Shell gates do
    /// not set this because their result is intentionally just process status.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_execution_outcome:
        Option<homeboy_engine_primitives::test_execution::TestExecutionOutcome>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default)]
    pub capture: AgentTaskGateCapture,
    /// The workspace requested by Cook and the component workspace that
    /// actually executed this gate. Root components retain identical paths.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<AgentTaskGateCwdEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_evidence: Option<AgentTaskGateFailureEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub test_result: Option<AgentTaskGateTestResult>,
    /// Cargo filter provenance for reviewer-visible focused gate evidence.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_selection: Option<AgentTaskGateCargoSelection>,
    /// Why this declared gate was not invoked. This remains durable evidence so
    /// downstream consumers do not have to infer skipped work from a missing row.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<AgentTaskGateSkipReason>,
    /// The same command's result against the immutable base when candidate
    /// adoption needs to distinguish inherited failures from regressions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub baseline_comparison: Option<AgentTaskGateBaselineComparison>,
    /// Immutable checkout identity used for this gate when verification runs
    /// against a materialized candidate rather than the promotion destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub candidate_checkout: Option<AgentTaskGateCandidateCheckout>,
    #[serde(default, skip_serializing_if = "AgentTaskGateEnvironment::is_empty")]
    pub environment: AgentTaskGateEnvironment,
    /// Runner-side dependency setup a Lab gate performed in the materialized
    /// runner workspace before its command ran. Local gates carry this
    /// evidence at the promotion level instead (#16262).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub setup: Vec<AgentTaskGateSetupEvidence>,
    /// Tracked candidate content a Lab gate rewrote while it ran. A gate that
    /// leaves the admitted tree changed is failed, whatever its exit code
    /// (#16920).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_mutation: Option<AgentTaskGateWorkspaceMutation>,
    /// Reviewer-facing name of the gate's declared source, such as the file
    /// name of a `--verify-file` program. Positional ids (`gate-1`) are
    /// meaningless to reviewers, so this becomes the gate result's name.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// The first run's failure when this gate failed once and passed on its
    /// one re-run: durable flake evidence, not a refusal (#18300).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub flaky: Option<AgentTaskGateFailureEvidence>,
    /// The first run's evidence when supervision stopped this gate (wall-clock
    /// or no-progress limit), or its environment failed before a verdict, and
    /// Homeboy re-ran it once on the same candidate under the same budgets.
    /// Recorded alongside the re-run's own result (#18554).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub supervision_regate: Option<AgentTaskGateFailureEvidence>,
}

/// Difference between the admitted candidate tree and the gate workspace
/// observed after the gate command ran.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AgentTaskGateWorkspaceMutation {
    pub differing_paths: Vec<String>,
    pub expected_tree: String,
    pub actual_tree: String,
    /// Why the tree diff could not name the paths, when it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_comparison_error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCwdEvidence {
    pub requested: String,
    pub effective: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCandidateCheckout {
    pub schema: String,
    pub commit: String,
    pub tree: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateBaselineComparison {
    pub base_ref: String,
    pub exit_code: i32,
    pub failure_fingerprint: String,
    pub matches_candidate_failure: bool,
    #[serde(default)]
    pub result: AgentTaskGateDifferentialResult,
    /// Why the comparison could not conclude.
    ///
    /// An `Inconclusive` result used to be indistinguishable from every other
    /// `Inconclusive` result: the baseline replay error was discarded at the
    /// point it was produced, so an operator was told to "repair the gate
    /// baseline setup" with nothing naming what was actually wrong. This
    /// carries that reason forward. Conclusive results leave it `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostic: Option<String>,
    /// Machine-readable attribution of the candidate's failures against the
    /// immutable baseline: whether the candidate broke the gate or the gate
    /// was already red, and which failure identities are inherited or new.
    /// Present only when the baseline run produced a conclusive comparison.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_attribution: Option<AgentTaskGateFailureAttribution>,
}

/// Who owns a failing gate once it has been reproduced on the immutable base.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateFailureOrigin {
    /// The candidate introduced at least one failure the baseline does not have.
    CandidateBrokeGate,
    /// Every candidate failure already exists on the baseline.
    GateAlreadyRedOnBaseline,
}

/// Per-failure attribution recorded in the gate receipt so an operator can
/// tell inherited breakage from candidate regressions without reading logs.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateFailureAttribution {
    pub classification: AgentTaskGateFailureOrigin,
    /// Complete failure identities on each side (fingerprints for opaque gates).
    #[serde(default)]
    pub base_failures: Vec<String>,
    #[serde(default)]
    pub head_failures: Vec<String>,
    /// Candidate failures that also fail on the baseline.
    #[serde(default)]
    pub inherited_failures: Vec<String>,
    /// Candidate failures absent from the baseline. These block promotion.
    #[serde(default)]
    pub new_failures: Vec<String>,
    /// Failures seen in only one of the two runs on either side.
    #[serde(default)]
    pub flaky_failures: Vec<String>,
}

/// The durable outcome of replaying a candidate gate against its immutable base.
/// `InheritedRed` is deliberately distinct from a passing gate.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateDifferentialResult {
    BaselineRed,
    CandidateRegression,
    CandidateImprovement,
    #[default]
    Inconclusive,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateEnvironment {
    /// Time waiting for local host admission, excluded from execution budgets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub admission_wait_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resources: Option<telemetry::GateResources>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_pressure_start: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host_pressure_end: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lab_runner: Option<String>,
    #[serde(default)]
    pub mode: AgentTaskGateEnvironmentMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inherited: Vec<AgentTaskGateEnvironmentVariable>,
    /// Explicit source mappings retained from the host for required tools.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preserved: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sanitized: Vec<AgentTaskGateEnvironmentVariable>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub package_artifacts: Vec<AgentTaskGatePackageArtifactProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extension_inputs: Vec<AgentTaskGateExtensionInputProvenance>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub data_inputs: Vec<data_inputs::GateInput>,
    /// The admitted component's linked extensions, resolved on the host that
    /// runs the gate and exposed read-only (symlinked) under the isolated HOME
    /// (#15383). Explicit `extension_inputs` take precedence by id.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub linked_extensions: Vec<AgentTaskGateLinkedExtensionProvenance>,
    /// Names (never values) of the admitted component's declared test secrets
    /// carried from the executing process into the gate child (#15824).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_secret_env: Vec<String>,
    /// Controller-owned Rust cache setup evidence. This deliberately records
    /// cache state rather than exposing a controller filesystem path to gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_cache: Option<AgentTaskGateRustCacheEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_target: Option<AgentTaskGateCargoTargetEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCargoTargetEvidence {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub acquire_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seed_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub growth_bytes: Option<u64>,
    pub path: String,
    pub resolution: String,
    pub owner: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_before: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bytes_after: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateRustCacheEvidence {
    pub identity: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hydrate_ms: Option<u128>,
}

/// Generic provenance for caller-declared package resources.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGatePackageArtifactProvenance {
    pub id: String,
    pub environment: String,
    pub value: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    pub artifacts: Vec<AgentTaskGateArtifactPathProvenance>,
    pub remediation: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateArtifactPathProvenance {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256: Option<String>,
}

impl AgentTaskGateEnvironment {
    /// Reconstruct the candidate's package inputs for immutable-base replay.
    /// A replay never broadens to new ambient input: source mappings remain
    /// explicit, while fixed mappings retain their recorded resolved value.
    pub(crate) fn package_artifact_replay_requirements(
        &self,
    ) -> Option<Vec<AgentTaskGatePackageArtifactRequirement>> {
        self.package_artifacts
            .iter()
            .map(|artifact| {
                (!artifact.artifacts.is_empty()
                    && artifact.artifacts.iter().all(|path| path.sha256.is_some()))
                .then(|| AgentTaskGatePackageArtifactRequirement {
                    id: artifact.id.clone(),
                    environment: AgentTaskGateArtifactEnvironmentMapping {
                        name: artifact.environment.clone(),
                        source: artifact.source.clone(),
                        default: artifact.source.is_none().then(|| artifact.value.clone()),
                    },
                    required_paths: artifact
                        .artifacts
                        .iter()
                        .map(|path| AgentTaskGateArtifactPathRequirement {
                            path: path.path.clone(),
                            sha256: path.sha256.clone(),
                        })
                        .collect(),
                    remediation: artifact.remediation.clone(),
                })
            })
            .collect()
    }
}

/// Identity and source provenance for an extension directory made available to
/// a gate. The destination is stable beneath the gate's isolated HOME.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateExtensionInputProvenance {
    pub id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    /// Content identity including the declared shared assets, when present.
    pub identity: String,
    pub destination: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub shared_assets: Vec<AgentTaskGateSharedAssetProvenance>,
}

/// A runner-installed extension linked into an isolated gate HOME.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateLinkedExtensionProvenance {
    pub id: String,
    /// Canonical installed extension directory on the executing host.
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
    /// Destination relative to the isolated HOME.
    pub destination: String,
    /// Extension-declared HOME-relative runtime directories linked from the
    /// executing host's HOME (#15836).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub home_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateSharedAssetProvenance {
    pub path: String,
    pub source: String,
    pub identity: String,
    pub destination: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateEnvironmentVariable {
    pub name: String,
    pub value: String,
}

impl AgentTaskGateEnvironment {
    fn is_empty(&self) -> bool {
        self.mode == AgentTaskGateEnvironmentMode::Inherit
            && self.inherited.is_empty()
            && self.preserved.is_empty()
            && self.sanitized.is_empty()
            && self.package_artifacts.is_empty()
            && self.extension_inputs.is_empty()
            && self.data_inputs.is_empty()
            && self.rust_cache.is_none()
            && self.cargo_target.is_none()
            && self.resources.is_none()
            && self.admission_wait_ms.is_none()
            && self.host_pressure_start.is_none()
            && self.host_pressure_end.is_none()
    }

    pub fn replay_policy(&self) -> AgentTaskGateEnvironmentPolicy {
        let variables = self
            .inherited
            .iter()
            .map(|variable| (variable.name.clone(), variable.value.clone()))
            .collect();
        AgentTaskGateEnvironmentPolicy {
            lab_runner: self.lab_runner.clone(),
            requirements: AgentTaskGateRequirements {
                host_tools: Vec::new(),
                secrets: self.declared_secret_env.clone(),
            },
            mode: self.mode,
            variables,
            preserve: self.preserved.clone(),
            isolate_home: self
                .sanitized
                .iter()
                .any(|variable| variable.name == "HOME"),
            isolate_xdg: self
                .sanitized
                .iter()
                .any(|variable| XDG_ENV_VARS.contains(&variable.name.as_str())),
            hydrate_rust_cache: true,
            hydrate_dependencies: true,
            shared_cargo_target: self.cargo_target.as_ref().map(|_| true),
            admitted_component_id: None,
            component_cwd: None,
            data_inputs: self.data_inputs.clone(),
            extension_inputs: self
                .extension_inputs
                .iter()
                .map(|input| AgentTaskGateExtensionInput {
                    id: input.id.clone(),
                    source: input.source.clone(),
                    identity: Some(input.identity.clone()),
                })
                .collect(),
            extension_sources: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateStatus {
    Succeeded,
    Failed,
    Skipped,
    /// The candidate command failed, but the identical failure was reproduced
    /// against the controller-recorded immutable baseline.
    AcceptedInheritedFailure,
    /// The gate command declared a portable Lab route and could not execute
    /// under the resolved placement (e.g. no ready Lab runner), so it deferred
    /// rather than running. This is neither a pass nor a candidate-attributable
    /// failure: no evidence was produced either way, so the candidate stays
    /// unverified rather than rejected (#14731).
    Deferred,
    /// Runner admission/readiness prevented execution; never candidate blame.
    Unavailable,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateTermination {
    #[default]
    Completed,
    Cancelled,
    TimedOut,
    NoProgress,
}

/// One replayable deterministic-gate invocation. Shell source remains the
/// explicit legacy escape hatch while declared tests retain their typed plan.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentTaskGateInvocation {
    LegacyShell {
        command: String,
        /// Always serialized and required: an invocation without it is an
        /// older record shape and fails to load (#15742).
        shell: LegacyGateShell,
    },
    DeclaredTest {
        plan: homeboy_engine_primitives::test_execution::TestExecutionPlan,
    },
}

/// Shell mode of a legacy gate program. Only the non-login, noninteractive
/// contract exists; the historical `sh -lc` login mode is not replayed (#15742).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LegacyGateShell {
    NonInteractive,
}

impl LegacyGateShell {
    fn flag(self) -> &'static str {
        match self {
            Self::NonInteractive => "-c",
        }
    }
}

/// Canonical fresh legacy gate program: non-login, noninteractive POSIX sh.
/// Preserve the environment selected by the verification kernel, including PATH.
pub fn legacy_gate_argv(command: &str) -> Vec<String> {
    vec![
        "sh".to_string(),
        LegacyGateShell::NonInteractive.flag().to_string(),
        command.to_string(),
    ]
}

fn recorded_legacy_gate_invocation(argv: &[String]) -> Option<AgentTaskGateInvocation> {
    match argv {
        [shell, flag, command] if shell == "sh" && flag == "-c" => {
            Some(AgentTaskGateInvocation::LegacyShell {
                command: command.clone(),
                shell: LegacyGateShell::NonInteractive,
            })
        }
        _ => None,
    }
}

impl AgentTaskGateInvocation {
    pub(crate) fn reviewer_command(&self) -> String {
        match self {
            Self::LegacyShell { command, .. } => command.clone(),
            Self::DeclaredTest { plan } => homeboy_engine_primitives::shell::quote_args(
                plan.declared_command()
                    .expect("declared test invocation was validated before persistence"),
            ),
        }
    }

    pub(crate) fn identity_digest(&self) -> Result<String> {
        match self {
            // Identity includes the actual shell mode. Missing serialized mode
            // retains historical receipts; fresh non-login execution is new proof.
            Self::LegacyShell { command, shell } => Ok(
                homeboy_engine_primitives::content_hash::nul_separated_digest([
                    "sh",
                    shell.flag(),
                    command,
                ]),
            ),
            Self::DeclaredTest { .. } => {
                let encoded = serde_json::to_vec(self)
                    .map_err(|error| Error::internal_json(error.to_string(), None))?;
                Ok(homeboy_engine_primitives::content_hash::sha256_hex(
                    &encoded,
                ))
            }
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCapture {
    pub stdout: homeboy_engine_primitives::command::CaptureMetadata,
    pub stderr: homeboy_engine_primitives::command::CaptureMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateLiveStatus {
    pub visibility: AgentTaskGateVisibility,
    pub reveal_policy: AgentTaskGateRevealPolicy,
    pub elapsed_ms: u128,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_progress_ms_ago: Option<u128>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<homeboy_engine_primitives::command::CommandProgress>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub output_tail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateSkipReason {
    pub blocking_gate_id: String,
    pub reason: String,
}

/// Canonical bridge from the binary agent-task gate status to the shared
/// `HomeboyGateStatus`. A matching red baseline explains a failure; it never
/// makes the candidate's required gate pass.
impl From<AgentTaskGateStatus> for HomeboyGateStatus {
    fn from(status: AgentTaskGateStatus) -> Self {
        match status {
            AgentTaskGateStatus::Succeeded => HomeboyGateStatus::Passed,
            AgentTaskGateStatus::Failed => HomeboyGateStatus::Failed,
            AgentTaskGateStatus::Skipped => HomeboyGateStatus::Skipped,
            // `Blocked` already names "could not execute under the resolved
            // environment" for a lab-capability preflight; a deferred command
            // is the same shape of evidence gap, so it reuses the variant
            // rather than adding a new one to the shared contract (#14731).
            AgentTaskGateStatus::Deferred | AgentTaskGateStatus::Unavailable => {
                HomeboyGateStatus::Blocked
            }
            AgentTaskGateStatus::AcceptedInheritedFailure => {
                HomeboyGateStatus::AcceptedInheritedFailure
            }
        }
    }
}

/// One failing test named by a failed deterministic gate.
pub use homeboy_core::extension::test::failure_digest::TestFailureDigestEntry as AgentTaskGateFailedTest;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateFailureEvidence {
    #[serde(default)]
    pub classification: AgentTaskGateFailureClassification,
    pub summary: String,
    pub command: String,
    pub exit_code: i32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout_tail: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr_tail: String,
    /// Failing test ids with bounded message/output excerpts, recovered from
    /// the full captured gate output before tails are cut (#18130).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub failed_tests: Vec<AgentTaskGateFailedTest>,
    pub agent_feedback: String,
    /// Producer-owned structured diagnostics carried in the gate sidecar.
    /// Homeboy treats their locations and suggested actions as opaque data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskGateDiagnosticRecord>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateTestResult {
    pub runner: String,
    pub total: u64,
    pub passed: u64,
    pub failed: u64,
    pub filtered: u64,
    pub runner_exit_code: i32,
}

/// The Cargo test population actually observed by a deterministic gate.
///
/// A positional Cargo filter is substring matching unless the test harness gets
/// `--exact`. Recording the interpretation and IDs keeps a broad selection
/// visible to reviewers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCargoSelection {
    pub effective_argv: Vec<String>,
    pub mode: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    pub filter_interpretation: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub discovered_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_ids: Vec<String>,
    pub selected_count: usize,
    /// Deterministic, redacted next action when a focused Cargo gate fails
    /// closed because its filter selected no tests.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<AgentTaskGateCargoSelectionRecovery>,
}

/// Why a focused Cargo selection cannot prove a candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FocusedCargoSelectionProblem {
    /// The filter selected no tests.
    NoTests,
}

impl AgentTaskGateCargoSelection {
    fn focused_problem(&self) -> Option<FocusedCargoSelectionProblem> {
        if self.mode != "focused" {
            None
        } else if self.selected_count == 0 {
            Some(FocusedCargoSelectionProblem::NoTests)
        } else {
            None
        }
    }
}

/// Reviewer-facing recovery for a focused Cargo selection failure.
///
/// Both the argv and rendered command are derived from the declared command,
/// then redacted before serialization. The rendered command quotes each argv
/// element rather than reparsing it through a shell.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCargoSelectionRecovery {
    pub action: String,
    pub command: String,
    pub argv: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub candidate_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateFailureClassification {
    #[default]
    CandidateCode,
    GateDeclaration,
    ZeroTestsSelected,
    /// A supervisor deadline or silence limit stopped the gate before a verdict.
    ExecutionBudget,
    /// Runner setup or infrastructure failed, not the candidate.
    Environment,
}

pub const AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA: &str = "homeboy/gate-diagnostic-record/v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateDiagnosticRecord {
    #[serde(default = "gate_diagnostic_record_schema")]
    pub schema: String,
    pub identity: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_location: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub suggested_actions: Vec<String>,
    pub producer: AgentTaskGateDiagnosticProducer,
    pub full_evidence_ref: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateDiagnosticProducer {
    pub id: String,
    pub schema: String,
}

/// A producer-declared sidecar contract mapped to Homeboy's normalized record.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateDiagnosticSidecarMapping {
    pub source_schema: String,
    #[serde(default = "gate_diagnostic_record_schema")]
    pub target_schema: String,
    pub path: String,
    pub producer: AgentTaskGateDiagnosticProducer,
}

const MAX_GATE_DIAGNOSTICS: usize = 8;
const MAX_GATE_DIAGNOSTIC_FIELD_BYTES: usize = 512;
const MAX_GATE_DIAGNOSTIC_ACTIONS: usize = 4;
const MAX_GATE_DIAGNOSTIC_SIDECAR_BYTES: u64 = 64 * 1024;
const MAX_CARGO_SELECTION_RECOVERY_CANDIDATES: usize = 8;

impl AgentTaskGateReport {
    pub(crate) fn invocation(&self) -> Result<AgentTaskGateInvocation> {
        if let Some(invocation) = &self.invocation {
            if let AgentTaskGateInvocation::DeclaredTest { plan } = invocation {
                plan.declared_command()
                    .map_err(|message| Error::invalid_argument("gate.invocation", message))?;
            }
            return Ok(invocation.clone());
        }
        recorded_legacy_gate_invocation(&self.command).ok_or_else(|| {
            Error::validation_invalid_argument(
                "gate.command",
                "legacy gate reports must contain a concrete `sh -c` invocation",
                Some(self.id.clone()),
                None,
            )
        })
    }

    /// Detect the exact marker a portable-Lab-route command emits when it
    /// could not run against the resolved placement and was queued as a daemon
    /// step waiting for a runner (schema `homeboy/runner-wait-step/v1`).
    ///
    /// This intentionally checks stdout content rather than exit code: the
    /// deferral exits `0` (it is not itself an
    /// error), which the exit-code-only classification this replaces
    /// misread as a passing gate. A gate whose declared command produces this
    /// exact shape never executed, so it can neither pass nor fail — it is a
    /// distinct `Deferred` outcome (#14731).
    fn gate_stdout_reports_runner_wait(stdout: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(stdout.trim()) else {
            return false;
        };
        value.get("schema").and_then(serde_json::Value::as_str)
            == Some("homeboy/runner-wait-step/v1")
            && value.get("status").and_then(serde_json::Value::as_str) == Some("deferred")
    }
    #[expect(
        clippy::too_many_arguments,
        reason = "constructor mirrors persisted gate result fields"
    )]
    pub fn new(
        id: impl Into<String>,
        command: Vec<String>,
        exit_code: i32,
        stdout: impl Into<String>,
        stderr: impl Into<String>,
        failure_evidence: Option<AgentTaskGateFailureEvidence>,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
        environment: AgentTaskGateEnvironment,
    ) -> Self {
        let id = id.into();
        let stdout = stdout.into();
        let invocation = recorded_legacy_gate_invocation(&command);
        let status = if Self::gate_stdout_reports_runner_wait(&stdout) {
            AgentTaskGateStatus::Deferred
        } else if exit_code == 0 {
            AgentTaskGateStatus::Succeeded
        } else {
            AgentTaskGateStatus::Failed
        };
        let gate_result = HomeboyGateResult::new(
            id.clone(),
            id.clone(),
            HomeboyGateKind::Command,
            HomeboyGateStatus::from(status),
        )
        .visibility(visibility)
        .reveal_policy(reveal_policy)
        .retryable(status == AgentTaskGateStatus::Failed);
        let step = PlanStep::builder(
            id.clone(),
            "agent_task.gate",
            match status {
                AgentTaskGateStatus::Succeeded => PlanStepStatus::Success,
                AgentTaskGateStatus::Failed | AgentTaskGateStatus::AcceptedInheritedFailure => {
                    PlanStepStatus::Failed
                }
                AgentTaskGateStatus::Skipped
                | AgentTaskGateStatus::Deferred
                | AgentTaskGateStatus::Unavailable => PlanStepStatus::Skipped,
            },
        )
        .inputs(PlanValues::new().json("command", &command))
        .output_value("exit_code", serde_json::json!(exit_code))
        .gate_result(gate_result)
        .build();

        Self {
            schema: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
            verdict: None,
            lab_receipt: None,
            step,
            id,
            visibility,
            reveal_policy,
            status,
            command,
            invocation,
            exit_code,
            termination: AgentTaskGateTermination::Completed,
            signal: None,
            test_execution_outcome: None,
            stdout,
            stderr: stderr.into(),
            capture: AgentTaskGateCapture::default(),
            cwd: None,
            failure_evidence,
            test_result: None,
            cargo_selection: None,
            skip_reason: None,
            baseline_comparison: None,
            candidate_checkout: None,
            environment,
            setup: Vec::new(),
            workspace_mutation: None,
            label: None,
            flaky: None,
            supervision_regate: None,
            cleanup_warning: None,
        }
    }

    pub fn skipped(
        id: impl Into<String>,
        command: Vec<String>,
        visibility: AgentTaskGateVisibility,
        reveal_policy: AgentTaskGateRevealPolicy,
        blocking_gate_id: impl Into<String>,
    ) -> Self {
        let id = id.into();
        let invocation = recorded_legacy_gate_invocation(&command);
        let skip_reason = AgentTaskGateSkipReason {
            blocking_gate_id: blocking_gate_id.into(),
            reason: "ordered_fail_fast".to_string(),
        };
        let gate_result = HomeboyGateResult::new(
            id.clone(),
            id.clone(),
            HomeboyGateKind::Command,
            HomeboyGateStatus::Skipped,
        )
        .summary(format!(
            "deterministic gate skipped after prerequisite {} failed",
            skip_reason.blocking_gate_id
        ))
        .visibility(visibility)
        .reveal_policy(reveal_policy)
        .retryable(false);
        let step = PlanStep::builder(id.clone(), "agent_task.gate", PlanStepStatus::Skipped)
            .inputs(PlanValues::new().json("command", &command))
            .output_value("skip_reason", serde_json::json!(&skip_reason))
            .gate_result(gate_result)
            .build();
        Self {
            schema: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
            step,
            id,
            visibility,
            reveal_policy,
            status: AgentTaskGateStatus::Skipped,
            verdict: None,
            lab_receipt: None,
            command,
            invocation,
            exit_code: 0,
            termination: AgentTaskGateTermination::Completed,
            signal: None,
            test_execution_outcome: None,
            stdout: String::new(),
            stderr: String::new(),
            capture: AgentTaskGateCapture::default(),
            cwd: None,
            failure_evidence: None,
            test_result: None,
            cargo_selection: None,
            skip_reason: Some(skip_reason),
            baseline_comparison: None,
            candidate_checkout: None,
            environment: AgentTaskGateEnvironment::default(),
            setup: Vec::new(),
            workspace_mutation: None,
            label: None,
            flaky: None,
            supervision_regate: None,
            cleanup_warning: None,
        }
    }

    /// Whether supervision stopped this gate at its wall-clock or no-progress
    /// limit. The command never reached its own verdict, so this is an
    /// infrastructure outcome, never candidate blame (#17958, #18554).
    pub fn stopped_by_supervision(&self) -> bool {
        self.status == AgentTaskGateStatus::Failed
            && matches!(
                self.termination,
                AgentTaskGateTermination::TimedOut | AgentTaskGateTermination::NoProgress
            )
    }

    /// Whether the gate environment failed before the command reached a
    /// verdict (a structured Homeboy setup error, a missing tool, disk or
    /// toolchain exhaustion). Like a supervision stop, this is infrastructure:
    /// it is re-gated once, and a repeat stops the Cook at
    /// `gate_environment_unavailable` instead of blaming the candidate.
    pub fn environment_unavailable(&self) -> bool {
        self.status == AgentTaskGateStatus::Failed
            && self.failure_evidence.as_ref().is_some_and(|evidence| {
                evidence.classification == AgentTaskGateFailureClassification::Environment
            })
    }

    /// Classify a supervision stop as unavailable verification: the candidate
    /// stays unverified, not rejected, and the Cook stops at
    /// `gate_environment_unavailable` with a re-gate next step instead of
    /// `gate_failed`. The typed termination and budget evidence are kept.
    fn classify_supervision_stop_as_unavailable(&mut self) {
        if !self.stopped_by_supervision() {
            return;
        }
        self.status = AgentTaskGateStatus::Unavailable;
        placement::rebuild_terminal_step(self);
    }

    /// Whether a failure is the command's own verdict on the candidate, which
    /// one re-run may overturn. Budget stops, declarations, empty selections,
    /// and environment failures are not re-run.
    fn reruns_on_failure(&self) -> bool {
        self.status == AgentTaskGateStatus::Failed
            && self.termination == AgentTaskGateTermination::Completed
            && self.failure_evidence.as_ref().is_none_or(|evidence| {
                evidence.classification == AgentTaskGateFailureClassification::CandidateCode
            })
    }

    pub(crate) fn accept_inherited_failure(&mut self) {
        // Only a candidate-code failure can be explained by an identical base
        // replay. Gate declarations and invalid focused selections are intrinsic
        // policy failures, even when the base has the same bad configuration.
        // A stalled dependency setup never reached a candidate verdict, so an
        // identical stall on the baseline proves nothing about either (#17423).
        if self.setup_stalled()
            || !self.failure_evidence.as_ref().is_some_and(|evidence| {
                evidence.classification == AgentTaskGateFailureClassification::CandidateCode
            })
        {
            return;
        }
        self.status = AgentTaskGateStatus::AcceptedInheritedFailure;
        let gate_result = HomeboyGateResult::new(
            self.id.clone(),
            self.id.clone(),
            HomeboyGateKind::Command,
            HomeboyGateStatus::AcceptedInheritedFailure,
        )
        .summary(
            "candidate failure matches the immutable baseline; required gate remains failed because the shared environment is red",
        )
        .visibility(self.visibility)
        .reveal_policy(self.reveal_policy)
        .retryable(false);
        self.step = PlanStep::builder(self.id.clone(), "agent_task.gate", PlanStepStatus::Failed)
            .inputs(PlanValues::new().json("command", &self.command))
            .output_value("exit_code", serde_json::json!(self.exit_code))
            .output_value("accepted_inherited_failure", serde_json::json!(true))
            .output_value("baseline_red", serde_json::json!(true))
            .output_value(
                "failure_origin",
                serde_json::json!("inherited_infrastructure"),
            )
            .gate_result(gate_result)
            .build();
    }
}

impl AgentTaskGateReport {
    /// Whether this gate's failure is its `promotion.gate_setup` dependency
    /// step stalling or timing out (for example `composer install stopped
    /// making progress`) rather than a verdict on the candidate (#17423).
    pub(crate) fn setup_stalled(&self) -> bool {
        if !matches!(
            self.status,
            AgentTaskGateStatus::Failed | AgentTaskGateStatus::AcceptedInheritedFailure
        ) {
            return false;
        }
        let stalled_outcome = self.setup.iter().any(|outcome| {
            outcome.status == homeboy_core::deps::DependencyHydrationStatus::Failed
                && matches!(
                    outcome.termination,
                    homeboy_core::deps::DependencyHydrationTermination::NoProgress
                        | homeboy_core::deps::DependencyHydrationTermination::TimedOut
                )
        });
        // Older reports recorded only the setup error text.
        let stalled_text = [
            self.stderr.as_str(),
            self.failure_evidence
                .as_ref()
                .map_or("", |evidence| evidence.summary.as_str()),
        ]
        .iter()
        .any(|text| {
            text.contains("Dependency step 'promotion.gate_setup' failed")
                && (text.contains("stopped making progress") || text.contains("timed out"))
        });
        stalled_outcome || stalled_text
    }

    /// Reclassify a stalled gate setup as the infrastructure fault it is: the
    /// gate is `Unavailable` (Cook status `gate_environment_unavailable`), so
    /// recovery re-runs it instead of counting a required-gate failure or an
    /// inherited baseline failure. Returns whether the gate changed.
    pub(crate) fn reclassify_setup_stall(&mut self) -> bool {
        if !self.setup_stalled() {
            return false;
        }
        self.status = AgentTaskGateStatus::Unavailable;
        self.failure_evidence = None;
        // The stall belongs to setup supervision, not to the gate's own budget.
        self.termination = AgentTaskGateTermination::Completed;
        placement::rebuild_terminal_step(self);
        true
    }
}

fn gate_report_schema() -> String {
    AGENT_TASK_GATE_REPORT_SCHEMA.to_string()
}

fn default_gate_step() -> PlanStep {
    PlanStep::builder("gate", "agent_task.gate", PlanStepStatus::Skipped).build()
}

/// The captured output of a failed `homeboy review test` over a large suite
/// (#18130): stdout is the tail-capped (64 KiB) sorted-key JSON result whose
/// window holds only executed test ids, and stderr carries progress, the
/// `[test-results]` summary and the failure digest printed after the run.
#[cfg(test)]
pub(crate) fn huge_review_test_failure_capture() -> (String, String) {
    use homeboy_core::extension::test::{failure_digest, TestAnalysisInput, TestFailure};
    let failing = "homeboy-agents::agent_task_gate::tests::needle_gate_regression";
    let tests = (0..14_281)
        .map(|index| {
            format!("      {{ \"id\": \"homeboy-version::tests::passing_case_{index:05}\" }}")
        })
        .collect::<Vec<_>>()
        .join(",\n");
    let stdout = format!(
        "{{\n  \"data\": {{\n    \"test_runtime_evidence\": {{\n      \"failed_test_ids\": [\"{failing}\"],\n      \"status\": \"complete\",\n      \"tests\": [\n{tests}\n      ],\n      \"workspace_fingerprint\": \"abc\"\n    }}\n  }},\n  \"success\": false\n}}\n"
    );
    let mut start = stdout.len() - 65_536;
    while !stdout.is_char_boundary(start) {
        start += 1;
    }
    let stdout = stdout[start..].to_string();
    assert!(!stdout.contains(failing), "fixture must reproduce the cap");
    let digest = failure_digest::render_test_failure_digest(&TestAnalysisInput {
        failures: vec![TestFailure {
            test_name: failing.to_string(),
            test_file: "crates/homeboy-agents/src/agent_task_gate.rs".to_string(),
            error_type: "panic".to_string(),
            message: "assertion `left == right` failed: needle mismatch\n  left: 1\n right: 2"
                .to_string(),
            stdout_excerpt:
                "thread 'needle' panicked at crates/homeboy-agents/src/agent_task_gate.rs:42:9"
                    .to_string(),
            stderr_excerpt: String::new(),
            source_file: String::new(),
            source_line: 0,
        }],
        total: 14_281,
        passed: 14_255,
    });
    let stderr = format!(
        "[test] phase=child command=test runner timeout=3600s\n[test-results] Total: 14281, Passed: 14255, Failed: 1, Skipped: 25\n{}\n",
        digest.join("\n")
    );
    (stdout, stderr)
}

mod dependency_setup;
pub(crate) use dependency_setup::hydrate_gate_dependency_roots_for_component;

mod diagnostics;
pub(crate) use diagnostics::ingest_gate_diagnostic_sidecars;
use diagnostics::*;

mod failure_records;
#[cfg(test)]
use failure_records::*;
pub(crate) use failure_records::{failure_fingerprint, failure_records};

mod execution;
use execution::*;
pub(crate) use execution::{
    cached_gate, run_declared_test_with_timeout,
    run_gate_command_with_policy_and_runtime_tmpdir_and_environment, run_gate_command_with_timeout,
};
pub use execution::{
    component_lint_base, run_declared_test_with_supervision, run_gate_command_with_supervision,
};

mod extension_sources;
use extension_sources::*;

mod environment;
use environment::*;

mod rust_cache;
use rust_cache::*;

mod environment_setup;
pub(crate) use environment_setup::GateHydrationEnvironment;
use environment_setup::*;

mod cargo_selection_admission;

mod preflight;
pub use preflight::preflight_gate_toolchains;
pub(crate) use preflight::preflight_verify_gate_options;
use preflight::*;

mod failure_evidence;
use failure_evidence::*;
pub(crate) use failure_evidence::{
    gate_environment_error, gate_failure_evidence, gate_missing_executable, text_tail,
};

mod projections;
pub use projections::{lab_non_execution_cause, NON_EXECUTION_NEXT_STEP};

#[cfg(test)]
pub(crate) use execution::{
    run_gate_command, run_gate_command_with_policy, run_gate_command_with_policy_and_runtime_tmpdir,
};

mod gate_runtime;

#[cfg(test)]
#[path = "agent_task_gate/baseline_tests.rs"]
mod baseline_tests;

#[cfg(test)]
#[path = "agent_task_gate/toolchain_requirement_serde_tests.rs"]
mod toolchain_requirement_serde_tests;

#[cfg(test)]
#[path = "agent_task_gate/rustup_location_tests.rs"]
mod rustup_location_tests;

#[cfg(test)]
#[path = "agent_task_gate/tests.rs"]
mod tests;
