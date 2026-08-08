use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};

use homeboy_core::gate::{
    HomeboyGateKind, HomeboyGateResult, HomeboyGateRevealPolicy, HomeboyGateStatus,
    HomeboyGateVisibility,
};
use homeboy_core::plan::{PlanStep, PlanStepStatus, PlanValues};
use homeboy_core::{Error, Result};

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

/// Temp-dir variables pinned to the invocation temp dir so every gate process
/// — preflight probes included — shares one temporary root.
const TMPDIR_ENV_VARS: &[&str] = &["TMPDIR", "TEMP", "TMP"];
const GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES: usize = 64 * 1024;

pub type AgentTaskGateVisibility = HomeboyGateVisibility;
pub type AgentTaskGateRevealPolicy = HomeboyGateRevealPolicy;
type GateSpawnCallback = Arc<dyn Fn(u32, &str) -> Result<()> + Send + Sync>;
type GateHeartbeatCallback = Arc<dyn Fn(&AgentTaskGateLiveStatus) -> Result<()> + Send + Sync>;

pub(crate) struct GateSupervision {
    pub timeout: Duration,
    pub no_progress_timeout: Duration,
    pub heartbeat_interval: Duration,
    pub on_spawn: GateSpawnCallback,
    pub on_heartbeat: GateHeartbeatCallback,
    pub is_cancelled: Arc<dyn Fn() -> bool + Send + Sync>,
}

/// Shared deterministic-gate verification fields used by every agent-task
/// options/report type that runs `--verify` / `--private-verify` gates. Embed
/// this via `#[serde(flatten)]` so the serialized JSON keeps the historical
/// flat `verify` / `private_verify` / `private_gate_reveal` shape while the
/// field group lives in exactly one place.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VerifyGateOptions {
    /// Deterministic verification commands run after each promotion/apply.
    #[serde(default)]
    pub verify: Vec<String>,
    /// Private deterministic verification commands whose full failure detail is
    /// gated from follow-up agents.
    #[serde(default)]
    pub private_verify: Vec<String>,
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
    /// Maximum time a gate may go without a `HOMEBOY_PROGRESS` marker.
    #[serde(default = "default_gate_no_progress_timeout_seconds")]
    pub gate_no_progress_timeout_seconds: u64,
    /// Completed adoption gates are reused by default; a recipe must opt in to
    /// rerunning them after restart.
    #[serde(default)]
    pub rerun_completed_gates: bool,
    /// Allow finalization after a required gate is proven red on both candidate
    /// and immutable baseline. The gate remains explicitly baseline-red in all
    /// reports; this only authorizes the finalization policy boundary.
    #[serde(default)]
    pub accept_inherited_failures: bool,
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
    /// Selected extension sources are copied under the isolated HOME. Gates
    /// never receive a writable path to the controller-owned source.
    #[serde(default)]
    pub extension_inputs: Vec<AgentTaskGateExtensionInput>,
}

/// A required executable and its non-mutating initialization probe.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateToolchainRequirement {
    pub command: String,
    #[serde(default = "default_toolchain_probe_arguments")]
    pub probe_arguments: Vec<String>,
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
    /// Optional identity pinned by a prior candidate gate when replaying a
    /// baseline. A changed source must fail rather than compare different input.
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
            mode: AgentTaskGateEnvironmentMode::Inherit,
            variables: BTreeMap::new(),
            preserve: BTreeMap::new(),
            isolate_home: true,
            isolate_xdg: true,
            extension_inputs: Vec::new(),
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

impl VerifyGateOptions {
    pub fn gate_timeout(&self) -> Duration {
        Duration::from_secs(self.gate_timeout_seconds.max(1))
    }

    pub fn gate_heartbeat_interval(&self) -> Duration {
        Duration::from_secs(self.gate_heartbeat_interval_seconds.max(1))
    }

    pub fn gate_no_progress_timeout(&self) -> Duration {
        Duration::from_secs(self.gate_no_progress_timeout_seconds.max(1))
    }

    /// Toolchain preflight is opt-in. Gate commands are shell programs and may
    /// be shell builtins, provider-owned aliases, or compound expressions, so
    /// treating their first token as an executable changes their semantics.
    pub(crate) fn required_toolchains(&self) -> Vec<AgentTaskGateToolchainRequirement> {
        self.gate_toolchains.clone()
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
            verify: Vec::new(),
            private_verify: Vec::new(),
            private_gate_reveal: default_private_gate_reveal(),
            execution_policy: AgentTaskGateExecutionPolicy::OrderedFailFast,
            gate_timeout_seconds: default_gate_timeout_seconds(),
            gate_heartbeat_interval_seconds: default_gate_heartbeat_interval_seconds(),
            gate_no_progress_timeout_seconds: default_gate_no_progress_timeout_seconds(),
            rerun_completed_gates: false,
            accept_inherited_failures: false,
            gate_environment: AgentTaskGateEnvironmentPolicy::default(),
            gate_toolchains: Vec::new(),
            gate_package_artifacts: Vec::new(),
            gate_diagnostic_sidecars: Vec::new(),
            hydrate_dependencies: default_hydrate_dependencies(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateSetupEvidence {
    pub schema: String,
    /// The workspace whose dependency state this setup evidence describes.
    pub workspace: String,
    pub package_root: String,
    pub lock_identity: String,
    pub setup_capability: String,
    pub duration_ms: u128,
    pub status: String,
    pub output: String,
}

const MAX_GATE_DEPENDENCY_ROOTS: usize = 64;

/// Discover only the checkout root and direct child roots. A directory is not a
/// dependency root merely because it exists: it must declare a supported,
/// content-addressed manifest or lock source. Provider resolution,
/// package-manager detection, and install commands remain outside Homeboy core.
pub(crate) fn hydrate_gate_dependency_roots(
    checkout: &Path,
    enabled: bool,
    workspace: &str,
) -> Result<Vec<AgentTaskGateSetupEvidence>> {
    if !enabled {
        return Ok(Vec::new());
    }
    let mut candidates = vec![checkout.to_path_buf()];
    for entry in fs::read_dir(checkout).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("read candidate dependency roots".to_string()),
        )
    })? {
        let entry = entry.map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read candidate dependency root".to_string()),
            )
        })?;
        let path = entry.path();
        let file_type = entry.file_type().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("read candidate dependency root type".to_string()),
            )
        })?;
        // A linked directory can escape the detached candidate checkout. Setup
        // is allowed only in the candidate root or a real direct child.
        if file_type.is_dir() && path.file_name().is_none_or(|name| name != ".git") {
            candidates.push(path);
        }
    }
    candidates.sort();
    let mut roots = Vec::new();
    let mut evidence = Vec::new();
    for candidate in candidates {
        let relative = dependency_root_relative(checkout, &candidate);
        let Some(lock_identity) = dependency_root_identity(&candidate)? else {
            evidence.push(AgentTaskGateSetupEvidence {
                schema: "homeboy/agent-task-gate-setup/v1".to_string(),
                workspace: workspace.to_string(),
                package_root: relative,
                lock_identity: "none".to_string(),
                setup_capability: "dependency.discovery".to_string(),
                duration_ms: 0,
                status: "skipped".to_string(),
                output: "skipped: no supported dependency manifest with a deterministic lock/source identity".to_string(),
            });
            continue;
        };
        roots.push(DependencyRoot {
            path: candidate,
            lock_identity,
        });
    }
    if roots.len() > MAX_GATE_DEPENDENCY_ROOTS {
        return Err(Error::validation_invalid_argument(
            "promotion.gate_setup",
            format!(
                "candidate declares more than {MAX_GATE_DEPENDENCY_ROOTS} dependency roots at the supported depth"
            ),
            Some(checkout.display().to_string()),
            None,
        ));
    }
    let hydrated = std::thread::scope(|scope| {
        roots
            .into_iter()
            .map(|root| scope.spawn(move || hydrate_dependency_root(root)))
            .collect::<Vec<_>>()
            .into_iter()
            .map(|worker| {
                worker.join().map_err(|_| {
                    Error::internal_unexpected("dependency root hydration worker panicked")
                })?
            })
            .collect::<Result<Vec<_>>>()
    })?;
    for setup in hydrated.into_iter().flatten() {
        evidence.push(AgentTaskGateSetupEvidence {
            schema: "homeboy/agent-task-gate-setup/v1".to_string(),
            workspace: workspace.to_string(),
            package_root: dependency_root_relative(checkout, &setup.root),
            lock_identity: setup.lock_identity,
            setup_capability: "dependency.install".to_string(),
            duration_ms: setup.duration_ms,
            status: "succeeded".to_string(),
            output: "provider-declared dependency setup completed".to_string(),
        });
    }
    evidence.sort_by(|left, right| left.package_root.cmp(&right.package_root));
    Ok(evidence)
}

struct DependencyRoot {
    path: PathBuf,
    lock_identity: String,
}

struct HydratedDependencyRoot {
    root: PathBuf,
    lock_identity: String,
    duration_ms: u128,
}

fn hydrate_dependency_root(root: DependencyRoot) -> Result<Option<HydratedDependencyRoot>> {
    // The identity is an input to setup, not a post-setup cache key. A provider
    // that rewrites it has not verified the declared candidate.
    let started = std::time::Instant::now();
    if !homeboy_core::hygiene::materialize_worktree_dependencies(&root.path)? {
        return Ok(None);
    }
    if dependency_root_identity(&root.path)?.as_deref() != Some(&root.lock_identity) {
        return Err(Error::validation_invalid_argument(
            "promotion.gate_setup",
            "dependency setup changed its declared lock identity",
            Some(root.path.display().to_string()),
            None,
        ));
    }
    Ok(Some(HydratedDependencyRoot {
        root: root.path,
        lock_identity: root.lock_identity,
        duration_ms: started.elapsed().as_millis(),
    }))
}

fn dependency_root_relative(checkout: &Path, root: &Path) -> String {
    let relative = root
        .strip_prefix(checkout)
        .unwrap_or(root)
        .display()
        .to_string();
    if relative.is_empty() {
        ".".to_string()
    } else {
        relative
    }
}

fn dependency_root_identity(root: &Path) -> Result<Option<String>> {
    const SOURCE_FILES: &[&str] = &[
        "homeboy.json",
        "homeboy-deps.json",
        "package.json",
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
    ];
    let has_node_lock = [
        "package-lock.json",
        "pnpm-lock.yaml",
        "yarn.lock",
        "bun.lock",
        "bun.lockb",
    ]
    .iter()
    .any(|name| root.join(name).is_file());
    let has_homeboy_manifest = ["homeboy.json", "homeboy-deps.json"]
        .iter()
        .any(|name| root.join(name).is_file());
    let has_cargo_root = root.join("Cargo.toml").is_file() && root.join("Cargo.lock").is_file();
    let has_composer_root =
        root.join("composer.json").is_file() && root.join("composer.lock").is_file();
    if !has_node_lock && !has_homeboy_manifest && !has_cargo_root && !has_composer_root {
        return Ok(None);
    }

    let mut inputs = Vec::new();
    for name in SOURCE_FILES.iter().copied().chain([
        "Cargo.toml",
        "Cargo.lock",
        "composer.json",
        "composer.lock",
    ]) {
        let path = root.join(name);
        if path.is_file() {
            inputs.push((
                name.to_string(),
                fs::read(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(path.display().to_string()))
                })?,
            ));
        }
    }
    inputs.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (name, bytes) in inputs {
        hasher.update(name.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
    }
    Ok(Some(format!("sha256:{:x}", hasher.finalize())))
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AgentTaskGateReport {
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
    pub exit_code: i32,
    /// Terminal classification distinguishes a command's semantic exit from
    /// controller wall-clock or progress-deadline termination.
    #[serde(default)]
    pub termination: AgentTaskGateTermination,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stdout: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub stderr: String,
    #[serde(default)]
    pub capture: AgentTaskGateCapture,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure_evidence: Option<AgentTaskGateFailureEvidence>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCandidateCheckout {
    pub schema: String,
    pub commit: String,
    pub tree: String,
    pub candidate_sha256: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateBaselineComparison {
    pub base_ref: String,
    pub exit_code: i32,
    pub failure_fingerprint: String,
    pub matches_candidate_failure: bool,
    #[serde(default)]
    pub result: AgentTaskGateDifferentialResult,
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

/// Identity and source provenance for an extension directory made available to
/// a gate. The destination is stable beneath the gate's isolated HOME.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateExtensionInputProvenance {
    pub id: String,
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_revision: Option<String>,
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
    }

    pub(crate) fn replay_policy(&self) -> AgentTaskGateEnvironmentPolicy {
        let variables = self
            .inherited
            .iter()
            .map(|variable| (variable.name.clone(), variable.value.clone()))
            .collect();
        AgentTaskGateEnvironmentPolicy {
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
            extension_inputs: self
                .extension_inputs
                .iter()
                .map(|input| AgentTaskGateExtensionInput {
                    id: input.id.clone(),
                    source: input.source.clone(),
                    identity: Some(input.identity.clone()),
                })
                .collect(),
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

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCapture {
    pub stdout: homeboy_engine_primitives::command::CaptureMetadata,
    pub stderr: homeboy_engine_primitives::command::CaptureMetadata,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct AgentTaskGateLiveStatus {
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
            AgentTaskGateStatus::AcceptedInheritedFailure => {
                HomeboyGateStatus::AcceptedInheritedFailure
            }
        }
    }
}

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
    pub agent_feedback: String,
    /// Producer-owned structured diagnostics carried in the gate sidecar.
    /// Homeboy treats their locations and suggested actions as opaque data.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub diagnostics: Vec<AgentTaskGateDiagnosticRecord>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AgentTaskGateFailureClassification {
    #[default]
    CandidateCode,
    GateDeclaration,
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

/// Best-effort compatibility ingestion for a declared producer sidecar. Missing
/// or malformed sidecars never replace the command's authoritative gate result.
pub(crate) fn ingest_gate_diagnostic_sidecars(
    cwd: &Path,
    mappings: &[AgentTaskGateDiagnosticSidecarMapping],
    report: &mut AgentTaskGateReport,
    full_evidence_ref: &str,
) {
    let Some(evidence) = report.failure_evidence.as_mut() else {
        return;
    };
    for mapping in mappings {
        if evidence.diagnostics.len() >= MAX_GATE_DIAGNOSTICS
            || mapping.target_schema != AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA
            || mapping.source_schema.trim().is_empty()
            || mapping.producer.id.trim().is_empty()
            || mapping.producer.schema.trim().is_empty()
        {
            continue;
        }
        let Ok(path) = homeboy_core::resolve_contained_local_path(
            cwd,
            &mapping.path,
            "gate_diagnostic_sidecars.path",
        ) else {
            continue;
        };
        let Ok(metadata) = fs::metadata(&path) else {
            continue;
        };
        if metadata.len() > MAX_GATE_DIAGNOSTIC_SIDECAR_BYTES {
            continue;
        }
        let Ok(body) = fs::read_to_string(&path) else {
            continue;
        };
        let Ok(records) = serde_json::from_str::<Vec<serde_json::Value>>(&body) else {
            continue;
        };
        for value in records {
            if evidence.diagnostics.len() >= MAX_GATE_DIAGNOSTICS {
                break;
            }
            let Some(record) = normalize_gate_diagnostic_record(value, mapping, full_evidence_ref)
            else {
                continue;
            };
            if !evidence.diagnostics.iter().any(|existing| {
                existing.identity == record.identity && existing.producer == record.producer
            }) {
                evidence.diagnostics.push(record);
            }
        }
    }
}

fn normalize_gate_diagnostic_record(
    value: serde_json::Value,
    mapping: &AgentTaskGateDiagnosticSidecarMapping,
    full_evidence_ref: &str,
) -> Option<AgentTaskGateDiagnosticRecord> {
    let object = value.as_object()?;
    if object.get("schema")?.as_str()? != mapping.source_schema {
        return None;
    }
    let identity = bounded_diagnostic_text(object.get("identity")?.as_str()?);
    let summary = bounded_diagnostic_text(object.get("summary")?.as_str()?);
    if identity.is_empty() || summary.is_empty() {
        return None;
    }
    let source_location = object
        .get("source_location")
        .and_then(serde_json::Value::as_str)
        .map(bounded_diagnostic_text)
        .filter(|value| !value.is_empty());
    let suggested_actions = object
        .get("suggested_actions")
        .and_then(serde_json::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(serde_json::Value::as_str)
        .map(bounded_diagnostic_text)
        .filter(|value| !value.is_empty())
        .take(MAX_GATE_DIAGNOSTIC_ACTIONS)
        .collect();
    Some(AgentTaskGateDiagnosticRecord {
        schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
        identity,
        summary,
        source_location,
        suggested_actions,
        producer: mapping.producer.clone(),
        full_evidence_ref: full_evidence_ref.to_string(),
    })
}

fn bounded_diagnostic_text(value: &str) -> String {
    let mut result = String::new();
    for character in value.chars() {
        if result.len() + character.len_utf8() > MAX_GATE_DIAGNOSTIC_FIELD_BYTES {
            break;
        }
        result.push(character);
    }
    result
}

fn gate_diagnostic_record_schema() -> String {
    AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string()
}

impl AgentTaskGateReport {
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
        let status = if exit_code == 0 {
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
                AgentTaskGateStatus::Skipped => PlanStepStatus::Skipped,
            },
        )
        .inputs(PlanValues::new().json("command", &command))
        .output_value("exit_code", serde_json::json!(exit_code))
        .gate_result(gate_result)
        .build();

        Self {
            schema: AGENT_TASK_GATE_REPORT_SCHEMA.to_string(),
            step,
            id,
            visibility,
            reveal_policy,
            status,
            command,
            exit_code,
            termination: AgentTaskGateTermination::Completed,
            stdout: stdout.into(),
            stderr: stderr.into(),
            capture: AgentTaskGateCapture::default(),
            failure_evidence,
            skip_reason: None,
            baseline_comparison: None,
            candidate_checkout: None,
            environment,
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
            command,
            exit_code: 0,
            termination: AgentTaskGateTermination::Completed,
            stdout: String::new(),
            stderr: String::new(),
            capture: AgentTaskGateCapture::default(),
            failure_evidence: None,
            skip_reason: Some(skip_reason),
            baseline_comparison: None,
            candidate_checkout: None,
            environment: AgentTaskGateEnvironment::default(),
        }
    }

    pub(crate) fn accept_inherited_failure(&mut self) {
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

/// Produce a stable failure identity from the exit status, structured diagnostic
/// identities, and a digest of normalized output records. Output is evidence,
/// not the sole authority for accepting an inherited failure.
pub(crate) fn failure_fingerprint(
    exit_code: i32,
    stdout: &str,
    stderr: &str,
    diagnostics: &[AgentTaskGateDiagnosticRecord],
) -> String {
    let mut output_records = [stdout, stderr]
        .into_iter()
        .flat_map(str::lines)
        .map(normalize_failure_record)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();
    output_records.sort();
    output_records.dedup();
    let mut diagnostic_ids = diagnostics
        .iter()
        .map(|diagnostic| format!("{}:{}", diagnostic.producer.id, diagnostic.identity))
        .collect::<Vec<_>>();
    diagnostic_ids.sort();
    diagnostic_ids.dedup();
    let payload = json!({
        "exit_code": exit_code,
        "diagnostic_ids": diagnostic_ids,
        "output_records": output_records,
    });
    format!(
        "sha256:{:x}",
        Sha256::digest(payload.to_string().as_bytes())
    )
}

fn normalize_failure_record(line: &str) -> String {
    line.split_whitespace()
        .map(|token| {
            if token.contains('T')
                && token.contains(':')
                && token.chars().any(|c| c.is_ascii_digit())
            {
                "<timestamp>"
            } else if token.starts_with("0x") && token[2..].chars().all(|c| c.is_ascii_hexdigit()) {
                "<address>"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod baseline_tests {
    use super::{
        failure_fingerprint, run_gate_command_with_timeout, AgentTaskGateEnvironmentPolicy,
        AgentTaskGateRevealPolicy, AgentTaskGateStatus, AgentTaskGateVisibility,
    };
    use std::time::Duration;

    #[test]
    fn matching_baseline_failure_is_distinct_from_a_new_failure() {
        let baseline = failure_fingerprint(1, "test alpha ... FAILED\n", "", &[]);
        let matching_candidate = failure_fingerprint(1, "test alpha ... FAILED\n", "", &[]);
        let regressed_candidate =
            failure_fingerprint(1, "test alpha ... FAILED\ntest beta ... FAILED\n", "", &[]);

        assert_eq!(baseline, matching_candidate);
        assert_ne!(baseline, regressed_candidate);
    }

    #[test]
    fn failure_fingerprint_distinguishes_exit_codes_and_ignores_volatile_reordered_output() {
        let baseline = failure_fingerprint(
            1,
            "error E42 at 2026-08-04T12:00:00Z\nworker 0xabc failed\n",
            "",
            &[],
        );
        let reordered = failure_fingerprint(
            1,
            "worker 0xdef failed\nerror E42 at 2027-01-01T00:00:00Z\n",
            "",
            &[],
        );
        let divergent_exit = failure_fingerprint(
            2,
            "worker 0xdef failed\nerror E42 at 2027-01-01T00:00:00Z\n",
            "",
            &[],
        );

        assert_eq!(baseline, reordered);
        assert_ne!(baseline, divergent_exit);
    }

    #[test]
    fn bounded_baseline_gate_is_cancelled() {
        let temp = tempfile::tempdir().expect("tempdir");
        let report = run_gate_command_with_timeout(
            temp.path(),
            1,
            "sleep 1",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            temp.path(),
            Duration::from_millis(20),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("bounded gate report");

        assert_eq!(report.status, AgentTaskGateStatus::Failed);
        assert_eq!(report.exit_code, 124);
        assert!(report.stderr.contains("was cancelled"));
    }

    #[cfg(unix)]
    #[test]
    fn bounded_baseline_gate_reaps_background_descendants_before_reader_join() {
        let temp = tempfile::tempdir().expect("tempdir");
        let pid_file = temp.path().join("descendant.pid");
        let report = run_gate_command_with_timeout(
            temp.path(),
            1,
            &format!(
                "sh -c 'trap \"\" TERM; while :; do sleep 1; done' & echo $! > '{}'; wait",
                pid_file.display()
            ),
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            temp.path(),
            Duration::from_millis(20),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("bounded gate report");

        assert_eq!(report.exit_code, 124);
        let descendant_pid = std::fs::read_to_string(pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert_ne!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }
}

pub(crate) fn run_gate_command(
    cwd: &Path,
    index: usize,
    command: &str,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy(
        cwd,
        index,
        command,
        AgentTaskGateVisibility::Visible,
        AgentTaskGateRevealPolicy::FullEvidence,
    )
}

pub(crate) fn run_gate_command_with_policy(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy_and_runtime_tmpdir(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        None,
    )
}

pub(crate) fn run_gate_command_with_policy_and_runtime_tmpdir(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        &AgentTaskGateEnvironmentPolicy::default(),
        &[],
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "gate execution inputs remain explicit across lifecycle and provider boundaries"
)]
pub(crate) fn run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    run_gate_command_with_supervision(
        cwd,
        index,
        command,
        visibility,
        reveal_policy,
        runtime_tmpdir,
        None,
        gate_environment,
        package_artifacts,
    )
}

#[expect(
    clippy::too_many_arguments,
    reason = "supervision callbacks and durable gate inputs remain independently auditable"
)]
pub(crate) fn run_gate_command_with_supervision(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: Option<&Path>,
    supervision: Option<&GateSupervision>,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    let command_vec = vec!["sh".to_string(), "-lc".to_string(), command.to_string()];
    let mut process = Command::new(&command_vec[0]);
    process
        .args(&command_vec[1..])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (gate_environment, package_artifacts) =
        validate_package_artifacts(cwd, gate_environment, package_artifacts)?;
    let mut selected_environment = selected_gate_environment(&gate_environment, runtime_tmpdir)?;
    selected_environment.report.package_artifacts = package_artifacts;
    selected_environment.apply(&mut process);
    if supervision.is_some() {
        if !homeboy_core::engine::command::supports_process_tree_isolation() {
            return Err(Error::validation_invalid_argument(
                "gate_supervision",
                "durable gate cancellation requires Unix process-group isolation on this host",
                None,
                None,
            ));
        }
        homeboy_core::engine::command::isolate_process_tree(&mut process);
    }
    let mut child = process.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run deterministic gate {command}")),
        )
    })?;
    let (output, termination) = if let Some(supervision) = supervision {
        if let Err(error) = (supervision.on_spawn)(child.id(), command) {
            if let Err(cleanup_error) =
                homeboy_core::engine::command::terminate_process_tree_and_reap(&mut child)
            {
                return Err(Error::internal_io(
                    format!(
                        "durable gate registration failed ({error}); failed to terminate and reap its child: {cleanup_error}"
                    ),
                    Some(format!("supervise deterministic gate {command}")),
                ));
            }
            return Err(error);
        }
        let supervised =
            homeboy_core::engine::command::wait_with_bounded_output_supervised_with_progress(
                &mut child,
                65_536,
                supervision.timeout,
                Some(supervision.no_progress_timeout),
                supervision.heartbeat_interval,
                || (supervision.is_cancelled)(),
                |heartbeat| {
                    // Durable status must never become a private-gate output channel.
                    let live_status = AgentTaskGateLiveStatus {
                        visibility,
                        reveal_policy,
                        elapsed_ms: heartbeat.elapsed.as_millis(),
                        last_progress_ms_ago: heartbeat
                            .last_progress_elapsed
                            .map(|elapsed| elapsed.as_millis()),
                        progress: heartbeat.progress,
                        output_tail: if visibility == AgentTaskGateVisibility::Private {
                            "private gate output withheld".to_string()
                        } else {
                            heartbeat.output_tail
                        },
                    };
                    (supervision.on_heartbeat)(&live_status).map_err(|error| {
                        std::io::Error::other(format!(
                            "persist deterministic gate heartbeat: {error}"
                        ))
                    })
                },
            )
            .map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("supervise deterministic gate {command}")),
                )
            })?;
        (supervised.output, supervised.termination)
    } else {
        let output = homeboy_core::engine::command::wait_with_bounded_output_until_cancelled(
            &mut child,
            65_536,
            || false,
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("run deterministic gate {command}")),
            )
        })?;
        (
            output,
            homeboy_core::engine::command::SupervisedCommandTermination::Completed,
        )
    };
    let termination = match termination {
        homeboy_core::engine::command::SupervisedCommandTermination::Completed => {
            AgentTaskGateTermination::Completed
        }
        homeboy_core::engine::command::SupervisedCommandTermination::Cancelled => {
            AgentTaskGateTermination::Cancelled
        }
        homeboy_core::engine::command::SupervisedCommandTermination::TimedOut => {
            AgentTaskGateTermination::TimedOut
        }
        homeboy_core::engine::command::SupervisedCommandTermination::NoProgress => {
            AgentTaskGateTermination::NoProgress
        }
    };
    let exit_code = match termination {
        AgentTaskGateTermination::TimedOut => 124,
        AgentTaskGateTermination::NoProgress => 125,
        _ => output.status.code().unwrap_or(1),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let failure_evidence =
        (exit_code != 0).then(|| gate_failure_evidence(command, exit_code, &stdout, &stderr));

    let mut report = AgentTaskGateReport::new(
        format!("gate-{index}"),
        command_vec,
        exit_code,
        stdout,
        stderr,
        failure_evidence,
        visibility,
        reveal_policy,
        selected_environment.report,
    );
    report.termination = termination;
    report.capture = AgentTaskGateCapture {
        stdout: output.capture.stdout,
        stderr: output.capture.stderr,
    };
    Ok(report)
}

/// Run a comparison gate with a hard wall-clock limit. The bounded path keeps
/// candidate adoption inspectable instead of allowing a known-red baseline to
/// consume an unbounded second broad-suite run.
#[expect(
    clippy::too_many_arguments,
    reason = "legacy gate provider entrypoint preserves mixed-version callers"
)]
pub(crate) fn run_gate_command_with_timeout(
    cwd: &Path,
    index: usize,
    command: &str,
    visibility: AgentTaskGateVisibility,
    reveal_policy: AgentTaskGateRevealPolicy,
    runtime_tmpdir: &Path,
    timeout: Duration,
    gate_environment: &AgentTaskGateEnvironmentPolicy,
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<AgentTaskGateReport> {
    let command_vec = vec!["sh".to_string(), "-lc".to_string(), command.to_string()];
    let mut process = Command::new("sh");
    process
        .args(["-lc", command])
        .current_dir(cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let (gate_environment, package_artifacts) =
        validate_package_artifacts(cwd, gate_environment, package_artifacts)?;
    let mut selected_environment =
        selected_gate_environment(&gate_environment, Some(runtime_tmpdir))?;
    selected_environment.report.package_artifacts = package_artifacts;
    selected_environment.apply(&mut process);
    homeboy_core::engine::command::isolate_process_tree(&mut process);
    let mut child = process.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run bounded deterministic gate {command}")),
        )
    })?;
    let started = std::time::Instant::now();
    let mut timed_out = false;
    let output = homeboy_core::engine::command::wait_with_bounded_output_until_cancelled(
        &mut child,
        65_536,
        || {
            timed_out = started.elapsed() >= timeout;
            timed_out
        },
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("run bounded deterministic gate {command}")),
        )
    })?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let exit_code = if timed_out {
        stderr.push_str(&format!(
            "\nbaseline gate exceeded {} ms and was cancelled",
            timeout.as_millis()
        ));
        124
    } else {
        output.status.code().unwrap_or(1)
    };
    let failure_evidence =
        (exit_code != 0).then(|| gate_failure_evidence(command, exit_code, &stdout, &stderr));
    let mut report = AgentTaskGateReport::new(
        format!("gate-{index}"),
        command_vec,
        exit_code,
        stdout,
        stderr,
        failure_evidence,
        visibility,
        reveal_policy,
        selected_environment.report,
    );
    report.termination = if timed_out {
        AgentTaskGateTermination::TimedOut
    } else {
        AgentTaskGateTermination::Completed
    };
    report.capture = AgentTaskGateCapture {
        stdout: output.capture.stdout,
        stderr: output.capture.stderr,
    };
    Ok(report)
}

struct SelectedGateEnvironment {
    report: AgentTaskGateEnvironment,
    values: BTreeMap<String, String>,
    _scratch: Option<tempfile::TempDir>,
}

impl SelectedGateEnvironment {
    fn apply(&self, process: &mut Command) {
        if self.report.mode == AgentTaskGateEnvironmentMode::Replace {
            process.env_clear();
        }
        for variable in &self.report.sanitized {
            process.env_remove(&variable.name);
        }
        process.envs(&self.values);
    }
}

/// Resolve the real directory every gate process receives as its temporary
/// root.
///
/// Two independent constraints apply, and canonicalizing satisfies only one of
/// them:
///
/// 1. **Socket budget (#10829).** `InvocationGuard` deliberately exports a
///    short symlink alias (`<runtime-root>/<short>.t`) and validates it with
///    `enforce_path_budget`, so gate workloads can bind `AF_UNIX` sockets
///    under `$TMPDIR`. The managed directory behind that alias is ~125 bytes —
///    already past the 108-byte Linux `sun_path` capacity before a socket name
///    is appended. Resolving the alias away discards the very budget the
///    invocation layer validated.
/// 2. **Non-symlink root (#11265).** Security-sensitive child tools reject a
///    symlinked temp root outright, before writing anything into it.
///
/// A real `tmp` subdirectory created *through* the alias satisfies both: the
/// path stays short, the leaf is a genuine directory rather than a symlink,
/// and the bytes still land in managed runtime-temp storage rather than on the
/// invocation root's volume (#11125).
///
/// Isolated `HOME`/XDG directories hang off this same root, so they inherit
/// both properties instead of re-deriving them.
fn gate_temp_root(runtime_tmpdir: &Path) -> Result<PathBuf> {
    let root = runtime_tmpdir.join("tmp");
    fs::create_dir_all(&root).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!(
                "create isolated gate temp directory {}",
                root.display()
            )),
        )
    })?;
    Ok(root)
}

fn selected_gate_environment(
    policy: &AgentTaskGateEnvironmentPolicy,
    runtime_tmpdir: Option<&Path>,
) -> Result<SelectedGateEnvironment> {
    let gate_tmpdir = runtime_tmpdir.map(gate_temp_root).transpose()?;
    let runtime_tmpdir = gate_tmpdir.as_deref();
    let mut report = AgentTaskGateEnvironment {
        mode: policy.mode,
        ..AgentTaskGateEnvironment::default()
    };
    let mut values = policy.variables.clone();
    for (name, value) in &policy.variables {
        report.inherited.push(AgentTaskGateEnvironmentVariable {
            name: name.clone(),
            value: value.clone(),
        });
    }
    for (name, source) in &policy.preserve {
        let value = preserved_environment_value(source)?;
        values.insert(name.clone(), value);
        report.preserved.insert(name.clone(), source.clone());
    }

    // The invocation temp dir belongs to the shared environment definition, not
    // to individual call sites. Preflight and execution must agree on it, or
    // preflight validates a different directory than the gate will use — and,
    // under the default inherit mode, silently falls back to the ambient TMPDIR
    // or fails outright when the host has none. (#10245)
    if let Some(runtime_tmpdir) = runtime_tmpdir {
        let runtime_tmpdir = runtime_tmpdir.display().to_string();
        for name in TMPDIR_ENV_VARS {
            values.insert((*name).to_string(), runtime_tmpdir.clone());
        }
    }

    let needs_scratch = policy.isolate_home || policy.isolate_xdg;
    let scratch = if needs_scratch && runtime_tmpdir.is_none() {
        Some(tempfile::tempdir().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("create isolated gate environment".to_string()),
            )
        })?)
    } else {
        None
    };
    let root = runtime_tmpdir.or_else(|| scratch.as_ref().map(|dir| dir.path()));
    if policy.isolate_home {
        let root = root.expect("isolated gate environment scratch root");
        let home = root.join("gate-home");
        fs::create_dir_all(&home).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create {}", home.display())),
            )
        })?;
        materialize_extension_inputs(&mut report, &home, &policy.extension_inputs)?;
        set_isolated_environment_variable(&mut report, &mut values, "HOME", home);
    } else if !policy.extension_inputs.is_empty() {
        return Err(Error::validation_invalid_argument(
            "gate_environment.extension_inputs",
            "extension inputs require HOME isolation",
            None,
            None,
        ));
    }
    if policy.isolate_xdg {
        let root = root.expect("isolated gate environment scratch root");
        for name in XDG_ENV_VARS {
            let path = root.join("gate-xdg").join(name.to_ascii_lowercase());
            fs::create_dir_all(&path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some(format!("create {}", path.display())),
                )
            })?;
            set_isolated_environment_variable(&mut report, &mut values, name, path);
        }
    }
    Ok(SelectedGateEnvironment {
        report,
        values,
        _scratch: scratch,
    })
}

fn materialize_extension_inputs(
    report: &mut AgentTaskGateEnvironment,
    home: &Path,
    inputs: &[AgentTaskGateExtensionInput],
) -> Result<()> {
    let extensions = home.join(".config/homeboy/extensions");
    let mut ids = std::collections::BTreeSet::new();
    for input in inputs {
        if input.id.trim().is_empty()
            || input.id.contains(['/', '\\'])
            || input.id == "."
            || input.id == ".."
        {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input ids must be non-empty path-free names",
                Some(input.id.clone()),
                None,
            ));
        }
        if !ids.insert(&input.id) {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input ids must be unique",
                Some(input.id.clone()),
                None,
            ));
        }
        let source = Path::new(&input.source);
        if !source.is_absolute() || !source.is_dir() {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input sources must be existing absolute directories",
                Some(input.source.clone()),
                None,
            ));
        }
        let identity = extension_tree_identity(source)?;
        let source_revision = homeboy_core::extension_update_check::read_source_revision_at(source);
        if input
            .identity
            .as_deref()
            .is_some_and(|expected| expected != identity)
        {
            return Err(Error::validation_invalid_argument(
                "gate_environment.extension_inputs",
                "extension input no longer matches the candidate gate identity",
                Some(input.id.clone()),
                None,
            ));
        }
        let destination = extensions.join(&input.id);
        copy_extension_input(source, &destination)?;
        report
            .extension_inputs
            .push(AgentTaskGateExtensionInputProvenance {
                id: input.id.clone(),
                source: input.source.clone(),
                source_revision,
                identity,
                destination: destination
                    .strip_prefix(home)
                    .unwrap_or(&destination)
                    .display()
                    .to_string(),
            });
    }
    Ok(())
}

fn extension_tree_identity(source: &Path) -> Result<String> {
    fn visit(root: &Path, path: &Path, files: &mut Vec<(String, Vec<u8>)>) -> Result<()> {
        for entry in fs::read_dir(path).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read extension input {}", path.display())),
            )
        })? {
            let entry = entry.map_err(|error| Error::internal_io(error.to_string(), None))?;
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|error| {
                Error::internal_io(error.to_string(), Some(path.display().to_string()))
            })?;
            if metadata.file_type().is_symlink() {
                return Err(Error::validation_invalid_argument(
                    "gate_environment.extension_inputs",
                    "extension inputs cannot contain symlinks",
                    Some(path.display().to_string()),
                    None,
                ));
            }
            if metadata.is_dir() {
                visit(root, &path, files)?;
            } else if metadata.is_file() {
                files.push((
                    path.strip_prefix(root)
                        .unwrap_or(&path)
                        .display()
                        .to_string(),
                    fs::read(&path).map_err(|error| {
                        Error::internal_io(error.to_string(), Some(path.display().to_string()))
                    })?,
                ));
            }
        }
        Ok(())
    }

    let mut files = Vec::new();
    visit(source, source, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (path, bytes) in files {
        hasher.update(path.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

fn copy_extension_input(source: &Path, destination: &Path) -> Result<()> {
    // A private copy is the security boundary: the controller source is never
    // exposed to the gate. The copy is gate-owned because file permissions do
    // not provide a portable immutable boundary (and privileged processes can
    // always change them).
    homeboy_core::io::copy_tree::copy_tree(
        source,
        destination,
        "gate.extension_input.copy",
        homeboy_core::io::copy_tree::EntryPolicy::CopyRegularFilesOnly,
    )
}

fn preserved_environment_value(source: &str) -> Result<String> {
    let (source_name, suffix) = source.split_once('/').unwrap_or((source, ""));
    let value = std::env::var(source_name).map_err(|_| {
        Error::validation_invalid_argument(
            "gate_environment.preserve",
            format!("declared gate environment source {source_name} is unavailable"),
            Some(source.to_string()),
            Some(vec![format!(
                "Set {source_name} before Cook or replace this mapping with an available toolchain home."
            )]),
        )
    })?;
    Ok(if suffix.is_empty() {
        value
    } else {
        PathBuf::from(value).join(suffix).display().to_string()
    })
}

/// Validate declared tools in the exact environment candidate gates will use.
/// This is deliberately generic: callers declare executables and environment
/// mappings while extensions own language-specific discovery.
pub(crate) fn preflight_gate_toolchains(
    cwd: &Path,
    policy: &AgentTaskGateEnvironmentPolicy,
    requirements: &[AgentTaskGateToolchainRequirement],
    package_artifacts: &[AgentTaskGatePackageArtifactRequirement],
    runtime_tmpdir: Option<&Path>,
    timeout: Duration,
) -> Result<()> {
    let (policy, _) = validate_package_artifacts(cwd, policy, package_artifacts)?;
    let selected_environment = selected_gate_environment(&policy, runtime_tmpdir)?;
    // The deadline covers declared probe execution. Controller scheduling before
    // the first child starts cannot make an otherwise successful probe fail.
    let mut started = None;
    for requirement in requirements {
        let elapsed = started.map(|started: std::time::Instant| started.elapsed());
        let remaining = timeout.saturating_sub(elapsed.unwrap_or_default());
        if started.is_some() && remaining.is_zero() {
            return Err(toolchain_preflight_error(
                requirement,
                elapsed.expect("started preflight deadline"),
                timeout,
                Duration::ZERO,
                None,
                true,
                "the total gate verification deadline was exhausted before this probe started",
            ));
        }
        let mut process = Command::new(&requirement.command);
        process
            .args(&requirement.probe_arguments)
            .current_dir(cwd)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        selected_environment.apply(&mut process);
        homeboy_core::engine::command::isolate_process_tree(&mut process);
        let mut child = process.spawn().map_err(|error| {
            toolchain_preflight_error(
                requirement,
                elapsed.unwrap_or_default(),
                timeout,
                remaining,
                None,
                false,
                &format!("could not resolve or initialize the executable: {error}"),
            )
        })?;
        let started = *started.get_or_insert_with(std::time::Instant::now);
        let elapsed = started.elapsed();
        let remaining = timeout.saturating_sub(elapsed);
        let supervised = homeboy_core::engine::command::wait_with_bounded_output_supervised(
            &mut child,
            GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES,
            remaining,
            remaining,
            || false,
            |_, _| Ok(()),
        )
        .map_err(|error| {
            toolchain_preflight_error(
                requirement,
                started.elapsed(),
                timeout,
                remaining,
                None,
                false,
                &format!("could not collect bounded probe output: {error}"),
            )
        })?;
        let timed_out = supervised.termination
            == homeboy_core::engine::command::SupervisedCommandTermination::TimedOut;
        let output = supervised.output;
        if timed_out || !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            let reason = if timed_out {
                "the probe exceeded its remaining gate verification deadline".to_string()
            } else {
                format!(
                    "the probe exited {}: {}",
                    output.status.code().unwrap_or(1),
                    text_tail(&stderr, 5),
                )
            };
            return Err(toolchain_preflight_error(
                requirement,
                started.elapsed(),
                timeout,
                remaining,
                Some(&output),
                timed_out,
                &reason,
            ));
        }
    }
    Ok(())
}

fn validate_package_artifacts(
    cwd: &Path,
    policy: &AgentTaskGateEnvironmentPolicy,
    requirements: &[AgentTaskGatePackageArtifactRequirement],
) -> Result<(
    AgentTaskGateEnvironmentPolicy,
    Vec<AgentTaskGatePackageArtifactProvenance>,
)> {
    let mut policy = policy.clone();
    let mut provenance = Vec::with_capacity(requirements.len());
    for requirement in requirements {
        if requirement.id.trim().is_empty()
            || requirement.environment.name.trim().is_empty()
            || requirement.required_paths.is_empty()
            || requirement.remediation.is_null()
        {
            return Err(package_artifact_error(
                requirement,
                "each declaration requires an id, environment mapping, required path, and remediation metadata",
                Vec::new(),
            ));
        }
        let mapping = &requirement.environment;
        if mapping.source.is_some() == mapping.default.is_some() {
            return Err(package_artifact_error(
                requirement,
                "environment mapping requires exactly one of source or default",
                Vec::new(),
            ));
        }
        if (mapping.name == "HOME" && policy.isolate_home)
            || (XDG_ENV_VARS.contains(&mapping.name.as_str()) && policy.isolate_xdg)
        {
            return Err(package_artifact_error(
                requirement,
                "environment mapping conflicts with the declared HOME/XDG isolation policy",
                Vec::new(),
            ));
        }
        let value = match (&mapping.source, &mapping.default) {
            (Some(source), None) => preserved_environment_value(source)?,
            (None, Some(default)) if !default.is_empty() => default.clone(),
            _ => {
                return Err(package_artifact_error(
                    requirement,
                    "environment mapping resolved to an empty value",
                    Vec::new(),
                ));
            }
        };
        policy.variables.insert(mapping.name.clone(), value.clone());
        let mut artifacts = Vec::with_capacity(requirement.required_paths.len());
        let mut missing = Vec::new();
        for artifact in &requirement.required_paths {
            let path = Path::new(&artifact.path);
            if path.is_absolute() || artifact.path.trim().is_empty() || artifact.path.contains("..")
            {
                missing.push(artifact.path.clone());
                continue;
            }
            let path = cwd.join(path);
            if !path.exists() {
                missing.push(artifact.path.clone());
                continue;
            }
            if let Some(expected) = &artifact.sha256 {
                let bytes = fs::read(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(path.display().to_string()))
                })?;
                let actual = format!("sha256:{:x}", Sha256::digest(bytes));
                if expected != &actual {
                    missing.push(artifact.path.clone());
                    continue;
                }
            }
            artifacts.push(AgentTaskGateArtifactPathProvenance {
                path: artifact.path.clone(),
                sha256: artifact.sha256.clone(),
            });
        }
        if !missing.is_empty() {
            return Err(package_artifact_error(
                requirement,
                "required artifact paths are unavailable or do not match their declared digest",
                missing,
            ));
        }
        provenance.push(AgentTaskGatePackageArtifactProvenance {
            id: requirement.id.clone(),
            environment: mapping.name.clone(),
            value,
            source: mapping.source.clone(),
            artifacts,
            remediation: requirement.remediation.clone(),
        });
    }
    Ok((policy, provenance))
}

fn package_artifact_error(
    requirement: &AgentTaskGatePackageArtifactRequirement,
    message: &str,
    invalid_paths: Vec<String>,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "gate_package_artifacts",
        format!(
            "package artifact readiness for `{}` failed: {message}",
            requirement.id
        ),
        Some(requirement.id.clone()),
        None,
    );
    error.retryable = Some(true);
    error.details["package_artifact_readiness"] = json!({
        "id": requirement.id,
        "environment": requirement.environment,
        "invalid_paths": invalid_paths,
        "remediation": requirement.remediation,
    });
    error
}

fn toolchain_preflight_error(
    requirement: &AgentTaskGateToolchainRequirement,
    elapsed: Duration,
    timeout: Duration,
    probe_timeout: Duration,
    output: Option<&homeboy_core::engine::command::BoundedCommandOutput>,
    timed_out: bool,
    reason: &str,
) -> Error {
    let mut error = Error::validation_invalid_argument(
        "gate_toolchains",
        format!(
            "gate toolchain preflight failed for `{}` after {} ms (deadline {} ms): {reason}",
            requirement.command,
            elapsed.as_millis(),
            timeout.as_millis(),
        ),
        Some(requirement.command.clone()),
        Some(vec!["Declare the required toolchain environment with --gate-env-from NAME=SOURCE[/suffix], then retry Cook.".to_string()]),
    );
    error.retryable = Some(true);
    error.details["toolchain_preflight"] = json!({
        "command": requirement.command,
        "arguments": requirement.probe_arguments,
        "elapsed_ms": elapsed.as_millis(),
        "timeout_ms": timeout.as_millis(),
        "probe_timeout_ms": probe_timeout.as_millis(),
        "timed_out": timed_out,
        "capture_limit_bytes": GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES,
        "stdout": output.map(|output| json!({
            "tail": String::from_utf8_lossy(&output.stdout),
            "bytes_seen": output.capture.stdout.bytes_seen,
            "bytes_retained": output.capture.stdout.bytes_retained,
            "truncated": output.capture.stdout.truncated,
        })),
        "stderr": output.map(|output| json!({
            "tail": String::from_utf8_lossy(&output.stderr),
            "bytes_seen": output.capture.stderr.bytes_seen,
            "bytes_retained": output.capture.stderr.bytes_retained,
            "truncated": output.capture.stderr.truncated,
        })),
        "remediation": "Declare the required toolchain environment with --gate-env-from NAME=SOURCE[/suffix], then retry Cook.",
    });
    error
}

fn set_isolated_environment_variable(
    report: &mut AgentTaskGateEnvironment,
    values: &mut BTreeMap<String, String>,
    name: &str,
    path: PathBuf,
) {
    let value = path.display().to_string();
    values.insert(name.to_string(), value.clone());
    report.sanitized.push(AgentTaskGateEnvironmentVariable {
        name: name.to_string(),
        value,
    });
}

fn gate_report_schema() -> String {
    AGENT_TASK_GATE_REPORT_SCHEMA.to_string()
}

fn default_gate_step() -> PlanStep {
    PlanStep::builder("gate", "agent_task.gate", PlanStepStatus::Skipped).build()
}

fn gate_failure_evidence(
    command: &str,
    exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> AgentTaskGateFailureEvidence {
    let stdout_tail = text_tail(stdout, 20);
    let stderr_tail = text_tail(stderr, 20);
    let missing_script = npm_run_script(command).filter(|script| {
        stderr.contains(&format!("Missing script: \"{script}\""))
            || stderr.contains(&format!("Missing script: {script}"))
    });
    let classification = missing_script
        .is_some()
        .then_some(AgentTaskGateFailureClassification::GateDeclaration)
        .unwrap_or(AgentTaskGateFailureClassification::CandidateCode);
    let summary = match missing_script {
        Some(script) => format!("declared npm gate is missing script `{script}`: {command}"),
        None => format!("deterministic gate failed with exit code {exit_code}: {command}"),
    };
    let agent_feedback = match missing_script {
        Some(script) => format!(
            "The declared gate is invalid, not candidate-code feedback. Add `scripts.{script}` to the relevant package.json or change/remove `{command}` before rerunning Cook."
        ),
        None => format!(
            "A deterministic verification gate failed after the candidate patch was applied. Fix the code so `{command}` passes, using the captured stdout/stderr tails as the primary failure evidence."
        ),
    };

    AgentTaskGateFailureEvidence {
        classification,
        summary,
        command: command.to_string(),
        exit_code,
        stdout_tail,
        stderr_tail,
        agent_feedback,
        diagnostics: Vec::new(),
    }
}

pub(crate) fn text_tail(text: &str, max_lines: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(max_lines);
    lines[start..].join("\n")
}

impl From<AgentTaskGateReport> for HomeboyGateResult {
    fn from(report: AgentTaskGateReport) -> Self {
        let status = HomeboyGateStatus::from(report.status);
        let command = report.command.join(" ");
        let summary = gate_result_summary(&report, &command);
        let agent_feedback = gate_result_agent_feedback(&report);
        let evidence = gate_result_evidence(&report);

        HomeboyGateResult::new(
            report.id.clone(),
            report.id.clone(),
            HomeboyGateKind::Command,
            status,
        )
        .summary(summary)
        .evidence(evidence)
        .visibility(report.visibility)
        .reveal_policy(report.reveal_policy)
        .retryable(status == HomeboyGateStatus::Failed)
        .agent_feedback(agent_feedback)
        .provenance(json!({
            "source_schema": report.schema,
            "source_type": "AgentTaskGateReport",
        }))
    }
}

fn gate_result_summary(report: &AgentTaskGateReport, command: &str) -> String {
    if report.status == AgentTaskGateStatus::Skipped {
        let blocker = report
            .skip_reason
            .as_ref()
            .map(|reason| reason.blocking_gate_id.as_str())
            .unwrap_or("an earlier gate");
        return format!("deterministic gate skipped after prerequisite {blocker} failed");
    }
    if report.status == AgentTaskGateStatus::Failed
        && report.visibility == AgentTaskGateVisibility::Private
    {
        match report.reveal_policy {
            AgentTaskGateRevealPolicy::SummaryOnly => {
                return format!(
                    "private deterministic gate {} failed; detailed evidence is withheld by policy",
                    report.id
                );
            }
            AgentTaskGateRevealPolicy::Redacted => {
                return "private deterministic gate failed; evidence redacted".to_string();
            }
            AgentTaskGateRevealPolicy::NoDetail => {
                return "private deterministic gate failed".to_string();
            }
            AgentTaskGateRevealPolicy::FullEvidence => {}
        }
    }

    report
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.summary.clone())
        .unwrap_or_else(|| format!("deterministic gate passed: {command}"))
}

fn gate_result_agent_feedback(report: &AgentTaskGateReport) -> String {
    if report.status == AgentTaskGateStatus::Skipped {
        return String::new();
    }
    if report.status == AgentTaskGateStatus::Failed
        && report.visibility == AgentTaskGateVisibility::Private
    {
        match report.reveal_policy {
            AgentTaskGateRevealPolicy::SummaryOnly => {
                return "A private deterministic verification gate failed. Generalize the fix against the public objective and visible evidence; hidden evaluator details are withheld.".to_string();
            }
            AgentTaskGateRevealPolicy::Redacted => {
                return "A private deterministic verification gate failed. Details are redacted; continue from the public task objective and visible gate evidence.".to_string();
            }
            AgentTaskGateRevealPolicy::NoDetail => {
                return "A private deterministic verification gate failed.".to_string();
            }
            AgentTaskGateRevealPolicy::FullEvidence => {}
        }
    }

    report
        .failure_evidence
        .as_ref()
        .map(|evidence| evidence.agent_feedback.clone())
        .unwrap_or_default()
}

fn gate_result_evidence(report: &AgentTaskGateReport) -> serde_json::Value {
    if report.status == AgentTaskGateStatus::Skipped {
        return json!({ "skipped": true, "skip_reason": report.skip_reason });
    }
    if report.visibility == AgentTaskGateVisibility::Private {
        match report.reveal_policy {
            AgentTaskGateRevealPolicy::SummaryOnly => {
                return json!({
                    "exit_code": report.exit_code,
                    "withheld": true,
                    "reason": "summary_only",
                });
            }
            AgentTaskGateRevealPolicy::Redacted => {
                return json!({
                    "exit_code": report.exit_code,
                    "redacted": true,
                });
            }
            AgentTaskGateRevealPolicy::NoDetail => {
                return json!({
                    "withheld": true,
                    "reason": "no_detail",
                });
            }
            AgentTaskGateRevealPolicy::FullEvidence => {}
        }
    }

    json!({
        "command": report.command,
        "exit_code": report.exit_code,
        "termination": report.termination,
        "stdout": report.stdout,
        "stderr": report.stderr,
        "capture": report.capture,
        "failure_evidence": report.failure_evidence,
        "environment": report.environment,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    /// Serializes tests that mutate process-global environment state.
    ///
    /// This must be the lock core hands out, not a module-local one. Rust runs
    /// a crate's tests as threads in a single process, so a `Mutex` scoped to
    /// this module only orders `agent_task_gate`'s own tests against each
    /// other, while every other module in the binary keeps running against the
    /// same environment. That is how a `HOME` mutation here went unnoticed.
    fn env_mutex() -> std::sync::MutexGuard<'static, ()> {
        homeboy_core::test_support::env_lock()
    }

    /// Restores process-global environment variables when dropped.
    ///
    /// Save/restore has to be panic-safe. A failing assertion between the
    /// mutation and a manual restore would leave the variable altered for every
    /// test that runs afterwards, turning one red test into a cascade.
    struct EnvVarGuard {
        saved: Vec<(&'static str, Option<std::ffi::OsString>)>,
    }

    impl EnvVarGuard {
        fn set(names_and_values: &[(&'static str, &std::path::Path)]) -> Self {
            let saved = names_and_values
                .iter()
                .map(|(name, value)| {
                    let prior = std::env::var_os(name);
                    std::env::set_var(name, value);
                    (*name, prior)
                })
                .collect();
            Self { saved }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            for (name, prior) in self.saved.drain(..) {
                match prior {
                    Some(value) => std::env::set_var(name, value),
                    // Only remove what was genuinely absent before.
                    None => std::env::remove_var(name),
                }
            }
        }
    }

    #[test]
    fn env_var_guard_restores_a_prior_value() {
        let _lock = env_mutex();
        let scratch = tempfile::tempdir().expect("scratch");
        std::env::set_var("__HOMEBOY_TEST_ENV_GUARD__", "original");

        {
            let _guard = EnvVarGuard::set(&[("__HOMEBOY_TEST_ENV_GUARD__", scratch.path())]);
            assert_eq!(
                std::env::var_os("__HOMEBOY_TEST_ENV_GUARD__"),
                Some(scratch.path().as_os_str().to_owned())
            );
        }

        assert_eq!(
            std::env::var("__HOMEBOY_TEST_ENV_GUARD__").ok().as_deref(),
            Some("original"),
            "a variable that existed before must be put back, not deleted"
        );
        std::env::remove_var("__HOMEBOY_TEST_ENV_GUARD__");
    }

    #[test]
    fn env_var_guard_removes_a_variable_that_was_absent() {
        let _lock = env_mutex();
        let scratch = tempfile::tempdir().expect("scratch");
        std::env::remove_var("__HOMEBOY_TEST_ENV_GUARD_ABSENT__");

        {
            let _guard = EnvVarGuard::set(&[("__HOMEBOY_TEST_ENV_GUARD_ABSENT__", scratch.path())]);
            assert!(std::env::var_os("__HOMEBOY_TEST_ENV_GUARD_ABSENT__").is_some());
        }

        assert!(std::env::var_os("__HOMEBOY_TEST_ENV_GUARD_ABSENT__").is_none());
    }

    #[test]
    fn env_var_guard_restores_even_when_the_test_body_panics() {
        // The property that matters. An assertion failing between the mutation
        // and a hand-written restore is what turns one red test into a cascade
        // across every test scheduled after it in the same process.
        let _lock = env_mutex();
        let scratch = tempfile::tempdir().expect("scratch");
        std::env::set_var("__HOMEBOY_TEST_ENV_GUARD_PANIC__", "original");

        let panicked = std::panic::catch_unwind(|| {
            let _guard = EnvVarGuard::set(&[("__HOMEBOY_TEST_ENV_GUARD_PANIC__", scratch.path())]);
            panic!("assertion failed mid-test");
        })
        .is_err();

        assert!(panicked);
        assert_eq!(
            std::env::var("__HOMEBOY_TEST_ENV_GUARD_PANIC__")
                .ok()
                .as_deref(),
            Some("original"),
            "a panicking test must not leak its environment mutation"
        );
        std::env::remove_var("__HOMEBOY_TEST_ENV_GUARD_PANIC__");
    }

    #[test]
    fn isolated_gate_test_leaves_home_intact() {
        // Regression: `isolated_gate_does_not_observe_ambient_...` used to end
        // with an unconditional `remove_var("HOME")`, deleting HOME for the
        // rest of the process. Roughly ninety tests in modules that sort after
        // `agent_task_gate` then failed, many with "HOME environment variable
        // not set on Unix-like system".
        let _lock = env_mutex();
        let before = std::env::var_os("HOME");

        {
            let scratch = tempfile::tempdir().expect("scratch");
            let _guard = EnvVarGuard::set(&[("HOME", scratch.path())]);
        }

        assert_eq!(
            std::env::var_os("HOME"),
            before,
            "HOME must survive a gate test that isolates it"
        );
    }

    #[test]
    fn root_package_lock_hydrates_once_and_skips_unrelated_directories() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let checkout = tempfile::tempdir().expect("checkout");
            let root = checkout.path();
            fs::write(root.join("package-lock.json"), "{\"lockfileVersion\":3}\n")
                .expect("package lock");
            assert!(
                dependency_root_identity(root)
                    .expect("package-lock identity")
                    .is_some(),
                "a package lock is a deterministic Node dependency source"
            );
            fs::write(
                root.join("homeboy-deps.json"),
                r#"{"provider":"fixture","commands":{"install":{"argv":["sh","-c","printf hydrated >> hydration-count"]}}}"#,
            )
            .expect("dependency provider");
            for directory in [".claude", "docs", "packages", "scripts", "tests"] {
                fs::create_dir(root.join(directory)).expect("unrelated directory");
            }

            let evidence =
                hydrate_gate_dependency_roots(root, true, "fixture").expect("dependency hydration");
            let succeeded = evidence
                .iter()
                .filter(|setup| setup.status == "succeeded")
                .collect::<Vec<_>>();
            assert_eq!(succeeded.len(), 1);
            assert_eq!(succeeded[0].package_root, ".");
            assert_ne!(succeeded[0].lock_identity, "none");
            assert_eq!(
                fs::read_to_string(root.join("hydration-count")).unwrap(),
                "hydrated"
            );

            let skipped = evidence
                .iter()
                .filter(|setup| setup.status == "skipped")
                .map(|setup| setup.package_root.as_str())
                .collect::<Vec<_>>();
            assert_eq!(
                skipped,
                vec![".claude", "docs", "packages", "scripts", "tests"]
            );
            assert!(evidence
                .iter()
                .filter(|setup| setup.status == "skipped")
                .all(|setup| setup.setup_capability == "dependency.discovery"));
        });
    }

    #[cfg(unix)]
    #[test]
    fn failed_durable_spawn_registration_reaps_the_isolated_gate_group() {
        let worktree = tempfile::tempdir().expect("worktree");
        let child_pid = Arc::new(Mutex::new(None));
        let supervision = GateSupervision {
            timeout: Duration::from_secs(3),
            no_progress_timeout: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new({
                let child_pid = Arc::clone(&child_pid);
                move |pid, _| {
                    *child_pid.lock().expect("child pid") = Some(pid);
                    Err(Error::internal_unexpected("durable write failed"))
                }
            }),
            on_heartbeat: Arc::new(|_| Ok(())),
            is_cancelled: Arc::new(|| false),
        };
        assert!(run_gate_command_with_supervision(
            worktree.path(),
            1,
            "sleep 30",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .is_err());
        let pid = child_pid
            .lock()
            .expect("child pid")
            .expect("durable registration received child pid");
        assert!(!homeboy_core::process::pid_is_running(pid));
    }

    #[cfg(unix)]
    #[test]
    fn supervised_gate_heartbeats_with_a_bounded_live_output_tail() {
        let worktree = tempfile::tempdir().expect("worktree");
        let tails = Arc::new(Mutex::new(Vec::new()));
        let supervision = GateSupervision {
            timeout: Duration::from_secs(1),
            no_progress_timeout: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new({
                let tails = Arc::clone(&tails);
                move |status| {
                    tails
                        .lock()
                        .expect("tails")
                        .push(status.output_tail.clone());
                    Ok(())
                }
            }),
            is_cancelled: Arc::new(|| false),
        };
        run_gate_command_with_supervision(
            worktree.path(),
            1,
            "printf stdout; printf stderr >&2; sleep 0.5",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate");
        assert!(tails
            .lock()
            .expect("tails")
            .iter()
            .any(|tail| tail.contains("stdout") && tail.contains("stderr")));
    }

    #[cfg(unix)]
    #[test]
    fn supervised_private_gate_never_persists_live_output() {
        let worktree = tempfile::tempdir().expect("worktree");
        let tails = Arc::new(Mutex::new(Vec::new()));
        let supervision = GateSupervision {
            timeout: Duration::from_secs(1),
            no_progress_timeout: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new({
                let tails = Arc::clone(&tails);
                move |status| {
                    tails
                        .lock()
                        .expect("tails")
                        .push(status.output_tail.clone());
                    Ok(())
                }
            }),
            is_cancelled: Arc::new(|| false),
        };
        run_gate_command_with_supervision(
            worktree.path(),
            1,
            "printf secret-stdout; printf secret-stderr >&2; sleep 0.5",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("private gate");
        let tails = tails.lock().expect("tails");
        assert!(!tails.is_empty());
        assert!(tails
            .iter()
            .all(|tail| tail == "private gate output withheld"));
    }

    #[test]
    fn gate_retains_bounded_tails_with_full_stream_metadata() {
        let worktree = tempfile::tempdir().expect("worktree");
        let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            "yes x | head -c 100000; yes y | head -c 100000 >&2",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            None,
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate");
        assert!(report.capture.stdout.truncated);
        assert!(report.capture.stderr.truncated);
        assert_eq!(report.capture.stdout.bytes_seen, 100_000);
        assert_eq!(report.capture.stdout.bytes_retained, 65_536);
        assert_eq!(report.capture.stdout.bytes_truncated, 34_464);
        assert!(report.capture.stdout.sha256.starts_with("sha256:"));
        assert!(report.stdout.len() <= 65_536);
    }

    #[cfg(unix)]
    #[test]
    fn silent_gate_stall_is_not_a_semantic_command_failure() {
        let worktree = tempfile::tempdir().expect("worktree");
        let supervision = GateSupervision {
            timeout: Duration::from_secs(1),
            no_progress_timeout: Duration::from_millis(30),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new(|_| Ok(())),
            is_cancelled: Arc::new(|| false),
        };
        let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            "sleep 1",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate report");
        assert_eq!(report.termination, AgentTaskGateTermination::NoProgress);
        assert_eq!(report.exit_code, 125);
    }

    #[cfg(unix)]
    #[test]
    fn progress_markers_keep_an_active_gate_alive_and_are_persisted() {
        let worktree = tempfile::tempdir().expect("worktree");
        let statuses = Arc::new(Mutex::new(Vec::new()));
        let supervision = GateSupervision {
            timeout: Duration::from_secs(3),
            no_progress_timeout: Duration::from_millis(200),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new({
                let statuses = Arc::clone(&statuses);
                move |status| {
                    statuses.lock().expect("statuses").push(status.clone());
                    Ok(())
                }
            }),
            is_cancelled: Arc::new(|| false),
        };
        let report = run_gate_command_with_supervision(
            worktree.path(), 1,
            "for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15; do printf 'HOMEBOY_PROGRESS {\"phase\":\"test\",\"current\":\"%s\"}\\n' \"$i\"; sleep 0.02; done",
            AgentTaskGateVisibility::Visible, AgentTaskGateRevealPolicy::FullEvidence,
            None, Some(&supervision), &AgentTaskGateEnvironmentPolicy::default(), &[],
        ).expect("gate report");
        assert_eq!(report.termination, AgentTaskGateTermination::Completed);
        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert!(statuses.lock().expect("statuses").iter().any(|status| {
            status
                .progress
                .as_ref()
                .is_some_and(|progress| progress.phase == "test")
                && status.last_progress_ms_ago.is_some()
        }));
    }

    #[test]
    fn semantic_failure_remains_completed_with_reviewer_resolvable_command() {
        let worktree = tempfile::tempdir().expect("worktree");
        let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            "printf failure >&2; exit 7",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            None,
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate report");
        assert_eq!(report.termination, AgentTaskGateTermination::Completed);
        assert_eq!(report.exit_code, 7);
        assert_eq!(
            report.failure_evidence.as_ref().expect("evidence").command,
            "printf failure >&2; exit 7"
        );
    }

    #[cfg(unix)]
    #[test]
    fn timeout_is_distinct_from_no_progress() {
        let worktree = tempfile::tempdir().expect("worktree");
        let supervision = GateSupervision {
            timeout: Duration::from_millis(30),
            no_progress_timeout: Duration::from_secs(1),
            heartbeat_interval: Duration::from_millis(10),
            on_spawn: Arc::new(|_, _| Ok(())),
            on_heartbeat: Arc::new(|_| Ok(())),
            is_cancelled: Arc::new(|| false),
        };
        let report = run_gate_command_with_supervision(
            worktree.path(),
            1,
            "sleep 1",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            Some(&supervision),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
        )
        .expect("gate report");
        assert_eq!(report.termination, AgentTaskGateTermination::TimedOut);
        assert_eq!(report.exit_code, 124);
    }

    #[test]
    fn legacy_gate_report_deserializes_with_new_defaults() {
        let report: AgentTaskGateReport = serde_json::from_value(serde_json::json!({
            "schema": "homeboy/agent-task-gate-report/v2", "id": "gate-1",
            "status": "succeeded", "command": ["sh", "-lc", "true"], "exit_code": 0
        }))
        .expect("legacy report");
        assert_eq!(report.termination, AgentTaskGateTermination::Completed);
        assert_eq!(report.capture.stdout.bytes_seen, 0);
    }

    #[test]
    fn agent_task_gate_status_bridges_to_homeboy_gate_status() {
        assert_eq!(
            HomeboyGateStatus::from(AgentTaskGateStatus::Succeeded),
            HomeboyGateStatus::Passed
        );
        assert_eq!(
            HomeboyGateStatus::from(AgentTaskGateStatus::Failed),
            HomeboyGateStatus::Failed
        );
        assert_eq!(
            HomeboyGateStatus::from(AgentTaskGateStatus::Skipped),
            HomeboyGateStatus::Skipped
        );
    }

    #[test]
    fn declared_opaque_sidecar_is_normalized_with_durable_evidence_reference() {
        let temp = tempfile::tempdir().expect("tempdir");
        let fixture = include_str!(
            "../../../tests/fixtures/agent_task_gate_feedback/opaque-producer-diagnostics.json"
        );
        fs::write(temp.path().join("diagnostics.json"), fixture).expect("write sidecar");
        let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");
        ingest_gate_diagnostic_sidecars(
            temp.path(),
            &[AgentTaskGateDiagnosticSidecarMapping {
                source_schema: "example/producer-diagnostic/v1".to_string(),
                target_schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
                path: "diagnostics.json".to_string(),
                producer: AgentTaskGateDiagnosticProducer {
                    id: "opaque-producer".to_string(),
                    schema: "example/producer-output/v1".to_string(),
                },
            }],
            &mut report,
            "homeboy://agent-task/run/run-1/promotion/gates/gate-1",
        );
        let diagnostic = &report.failure_evidence.unwrap().diagnostics[0];
        assert_eq!(diagnostic.schema, AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA);
        assert_eq!(diagnostic.identity, "opaque:stable-identity");
        assert_eq!(diagnostic.producer.id, "opaque-producer");
        assert_eq!(
            diagnostic.full_evidence_ref,
            "homeboy://agent-task/run/run-1/promotion/gates/gate-1"
        );
    }

    #[test]
    fn absent_or_malformed_declared_sidecars_preserve_gate_failure() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");
        let mapping = AgentTaskGateDiagnosticSidecarMapping {
            source_schema: "example/producer-diagnostic/v1".to_string(),
            target_schema: AGENT_TASK_GATE_DIAGNOSTIC_RECORD_SCHEMA.to_string(),
            path: "diagnostics.json".to_string(),
            producer: AgentTaskGateDiagnosticProducer {
                id: "opaque-producer".to_string(),
                schema: "example/producer-output/v1".to_string(),
            },
        };
        ingest_gate_diagnostic_sidecars(
            temp.path(),
            std::slice::from_ref(&mapping),
            &mut report,
            "evidence",
        );
        fs::write(temp.path().join("diagnostics.json"), "not json")
            .expect("write malformed sidecar");
        ingest_gate_diagnostic_sidecars(temp.path(), &[mapping], &mut report, "evidence");
        assert!(report.failure_evidence.unwrap().diagnostics.is_empty());
    }

    #[test]
    fn verify_gate_policy_defaults_are_durable_and_backward_compatible() {
        let defaults = VerifyGateOptions::default();
        assert_eq!(defaults.gate_timeout(), Duration::from_secs(30 * 60));
        assert_eq!(defaults.gate_heartbeat_interval(), Duration::from_secs(5));
        assert!(!defaults.rerun_completed_gates);
        assert_eq!(
            defaults.execution_policy,
            AgentTaskGateExecutionPolicy::OrderedFailFast
        );

        let legacy: VerifyGateOptions = serde_json::from_value(serde_json::json!({
            "verify": ["cargo test"],
            "private_verify": [],
            "private_gate_reveal": "summary_only"
        }))
        .expect("deserialize legacy gate policy");
        assert_eq!(
            legacy,
            VerifyGateOptions {
                verify: vec!["cargo test".to_string()],
                ..VerifyGateOptions::default()
            }
        );
    }

    #[test]
    fn gate_command_reports_success_without_failure_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(temp.path(), 1, "printf 'ok'").expect("gate report");

        assert_eq!(report.schema, AGENT_TASK_GATE_REPORT_SCHEMA);
        assert_eq!(report.id, "gate-1");
        assert_eq!(report.visibility, AgentTaskGateVisibility::Visible);
        assert_eq!(
            report.reveal_policy,
            AgentTaskGateRevealPolicy::FullEvidence
        );
        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(report.exit_code, 0);
        assert_eq!(report.stdout, "ok");
        assert!(report.failure_evidence.is_none());
    }

    #[test]
    fn accepted_inherited_failure_rebuilds_the_embedded_plan_step() {
        let temp = tempfile::tempdir().expect("tempdir");
        let mut report = run_gate_command(temp.path(), 1, "exit 1").expect("failed gate");

        report.accept_inherited_failure();

        assert_eq!(report.status, AgentTaskGateStatus::AcceptedInheritedFailure);
        let step = serde_json::to_value(&report.step).expect("serialize step");
        assert_eq!(step["status"], "failed");
        assert_eq!(step["outputs"]["accepted_inherited_failure"], true);
        assert_eq!(step["outputs"]["baseline_red"], true);
        assert_eq!(
            step["outputs"]["gate_result"]["status"],
            "accepted_inherited_failure"
        );
    }

    #[test]
    fn gate_command_uses_the_declared_runtime_scratch_root() {
        let worktree = tempfile::tempdir().expect("worktree");
        let scratch = tempfile::tempdir().expect("scratch");

        let report = run_gate_command_with_policy_and_runtime_tmpdir(
            worktree.path(),
            8,
            "printf '%s:%s:%s' \"$TMPDIR\" \"$TEMP\" \"$TMP\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(scratch.path()),
        )
        .expect("gate report");

        let expected = scratch.path().join("tmp").display().to_string();
        assert_eq!(report.stdout, format!("{expected}:{expected}:{expected}"));
    }

    #[test]
    fn gate_command_normalizes_failure_for_agent_feedback() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(
            temp.path(),
            2,
            "printf 'line one\nline two\n'; printf 'boom\n' >&2; exit 42",
        )
        .expect("gate report");
        let evidence = report.failure_evidence.as_ref().expect("failure evidence");

        assert_eq!(report.status, AgentTaskGateStatus::Failed);
        assert_eq!(report.exit_code, 42);
        assert_eq!(
            evidence.command,
            "printf 'line one\nline two\n'; printf 'boom\n' >&2; exit 42"
        );
        assert_eq!(evidence.stdout_tail, "line one\nline two");
        assert_eq!(evidence.stderr_tail, "boom");
        assert!(evidence.summary.contains("deterministic gate failed"));
        assert!(evidence.agent_feedback.contains("Fix the code"));
    }

    #[test]
    fn gate_command_records_private_visibility_and_reveal_policy() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command_with_policy(
            temp.path(),
            3,
            "printf 'hidden failure'; exit 1",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::SummaryOnly,
        )
        .expect("gate report");

        assert_eq!(report.status, AgentTaskGateStatus::Failed);
        assert_eq!(report.visibility, AgentTaskGateVisibility::Private);
        assert_eq!(report.reveal_policy, AgentTaskGateRevealPolicy::SummaryOnly);
        assert_eq!(report.stdout, "hidden failure");
    }

    #[test]
    fn agent_task_gate_report_normalizes_to_homeboy_gate_result() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command_with_policy(
            temp.path(),
            4,
            "printf 'hidden failure'; exit 1",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::SummaryOnly,
        )
        .expect("gate report");
        let result: HomeboyGateResult = report.into();

        assert_eq!(
            result.schema,
            homeboy_core::gate::HOMEBOY_GATE_RESULT_SCHEMA
        );
        assert_eq!(result.id, "gate-4");
        assert_eq!(result.kind, HomeboyGateKind::Command);
        assert_eq!(result.status, HomeboyGateStatus::Failed);
        assert_eq!(result.visibility, HomeboyGateVisibility::Private);
        assert_eq!(result.reveal_policy, HomeboyGateRevealPolicy::SummaryOnly);
        assert_eq!(result.retryable, Some(true));
        assert!(result.summary.contains("detailed evidence is withheld"));
        assert!(result
            .agent_feedback
            .contains("hidden evaluator details are withheld"));
        assert_eq!(result.evidence["exit_code"], 1);
        assert_eq!(result.evidence["withheld"], true);
        assert_eq!(result.evidence.get("stdout"), None);
        assert_eq!(result.evidence.get("stderr"), None);
        assert_eq!(result.provenance["source_type"], "AgentTaskGateReport");
    }

    #[test]
    fn skipped_private_gate_preserves_blocker_evidence_without_command_disclosure() {
        let report = AgentTaskGateReport::skipped(
            "gate-2",
            vec![
                "sh".to_string(),
                "-lc".to_string(),
                "private-command --secret".to_string(),
            ],
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::Redacted,
            "gate-1",
        );

        assert_eq!(report.schema, AGENT_TASK_GATE_REPORT_SCHEMA);
        assert_eq!(report.step.status, PlanStepStatus::Skipped);
        assert_eq!(
            report
                .skip_reason
                .as_ref()
                .map(|reason| reason.blocking_gate_id.as_str()),
            Some("gate-1")
        );

        let result: HomeboyGateResult = report.into();
        assert_eq!(result.status, HomeboyGateStatus::Skipped);
        assert_eq!(result.visibility, HomeboyGateVisibility::Private);
        assert_eq!(result.evidence["skip_reason"]["blocking_gate_id"], "gate-1");
        assert_eq!(result.evidence.get("command"), None);
        assert!(!result.summary.contains("private-command"));
        assert!(result.agent_feedback.is_empty());
    }

    #[test]
    fn private_redacted_agent_task_gate_result_omits_command_and_output_evidence() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command_with_policy(
            temp.path(),
            6,
            "printf 'secret stdout'; printf 'secret stderr' >&2; exit 1",
            AgentTaskGateVisibility::Private,
            AgentTaskGateRevealPolicy::Redacted,
        )
        .expect("gate report");
        let result: HomeboyGateResult = report.into();

        assert_eq!(result.status, HomeboyGateStatus::Failed);
        assert_eq!(result.reveal_policy, HomeboyGateRevealPolicy::Redacted);
        assert_eq!(result.evidence["redacted"], true);
        assert_eq!(result.evidence.get("command"), None);
        assert_eq!(result.evidence.get("stdout"), None);
        assert_eq!(result.evidence.get("stderr"), None);
        assert!(result.summary.contains("evidence redacted"));
    }

    #[test]
    fn successful_agent_task_gate_report_normalizes_to_passed_gate_result() {
        let temp = tempfile::tempdir().expect("tempdir");

        let report = run_gate_command(temp.path(), 5, "printf 'ok'").expect("gate report");
        let result: HomeboyGateResult = report.into();

        assert_eq!(result.id, "gate-5");
        assert_eq!(result.kind, HomeboyGateKind::Command);
        assert_eq!(result.status, HomeboyGateStatus::Passed);
        assert_eq!(result.retryable, Some(false));
        assert_eq!(result.evidence["exit_code"], 0);
        assert_eq!(result.evidence["stdout"], "ok");
        assert!(result.agent_feedback.is_empty());
        assert!(result.summary.contains("deterministic gate passed"));
    }

    #[test]
    fn isolated_gate_does_not_observe_ambient_durable_recipes_runs_or_runtime_liveness() {
        let _guard = env_mutex();
        let worktree = tempfile::tempdir().expect("worktree");
        let ambient = tempfile::tempdir().expect("ambient state");
        let home = ambient.path().join("home");
        let xdg_state = ambient.path().join("state");
        let xdg_runtime = ambient.path().join("runtime");
        fs::create_dir_all(home.join(".homeboy/recipes")).expect("ambient recipe directory");
        fs::create_dir_all(xdg_state.join("runs/active")).expect("ambient run directory");
        fs::create_dir_all(xdg_runtime.join("homeboy-live")).expect("ambient runtime directory");
        let _env = EnvVarGuard::set(&[
            ("HOME", home.as_path()),
            ("XDG_STATE_HOME", xdg_state.as_path()),
            ("XDG_RUNTIME_DIR", xdg_runtime.as_path()),
        ]);

        let report = run_gate_command(
            worktree.path(),
            7,
            "test ! -e \"$HOME/.homeboy/recipes\" && test ! -e \"$XDG_STATE_HOME/runs/active\" && test ! -e \"$XDG_RUNTIME_DIR/homeboy-live\"",
        )
        .expect("isolated gate report");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "HOME"));
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "XDG_STATE_HOME"));
        assert!(report
            .environment
            .sanitized
            .iter()
            .any(|variable| variable.name == "XDG_RUNTIME_DIR"));
    }

    #[test]
    fn isolated_gate_discovers_only_selected_extension_copies_with_replayable_provenance() {
        let _guard = env_mutex();
        let worktree = tempfile::tempdir().expect("worktree");
        let ambient = tempfile::tempdir().expect("ambient home");
        let extensions = ambient.path().join(".config/homeboy/extensions");
        let selected = extensions.join("selected-fixture");
        let unselected = extensions.join("ambient-fixture");
        fs::create_dir_all(&selected).expect("selected extension");
        fs::create_dir_all(&unselected).expect("unselected extension");
        fs::write(
            selected.join("selected-fixture.json"),
            r#"{"id":"selected-fixture","name":"Selected","version":"1.0.0"}"#,
        )
        .expect("selected manifest");
        fs::write(selected.join(".source-revision"), "revision-123\n").expect("selected revision");
        fs::write(
            unselected.join("ambient-fixture.json"),
            r#"{"id":"ambient-fixture","name":"Ambient","version":"1.0.0"}"#,
        )
        .expect("ambient manifest");
        let _env = EnvVarGuard::set(&[("HOME", ambient.path())]);
        let policy = AgentTaskGateEnvironmentPolicy {
            extension_inputs: vec![AgentTaskGateExtensionInput {
                id: "selected-fixture".to_string(),
                source: selected.display().to_string(),
                identity: None,
            }],
            ..AgentTaskGateEnvironmentPolicy::default()
        };
        let command = "test -f \"$HOME/.config/homeboy/extensions/selected-fixture/selected-fixture.json\" && test ! -e \"$HOME/.config/homeboy/extensions/ambient-fixture\" && printf gate-owned > \"$HOME/.config/homeboy/extensions/selected-fixture/gate-owned\"";

        let candidate = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            worktree.path(),
            1,
            command,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(worktree.path()),
            &policy,
            &[],
        )
        .expect("selected extension gate");
        let baseline = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            worktree.path(),
            1,
            command,
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(worktree.path()),
            &candidate.environment.replay_policy(),
            &[],
        )
        .expect("replayed selected extension gate");

        assert_eq!(candidate.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(candidate.environment.extension_inputs.len(), 1);
        assert_eq!(
            candidate.environment.extension_inputs[0].id,
            "selected-fixture"
        );
        assert!(candidate.environment.extension_inputs[0]
            .identity
            .starts_with("sha256:"));
        assert_eq!(
            candidate.environment.extension_inputs[0]
                .source_revision
                .as_deref(),
            Some("revision-123")
        );
        let copied = worktree
            .path()
            .join("tmp/gate-home/.config/homeboy/extensions/selected-fixture");
        assert!(!fs::symlink_metadata(&copied)
            .expect("copied extension metadata")
            .file_type()
            .is_symlink());
        assert!(copied.join("gate-owned").exists());
        assert!(!selected.join("gate-owned").exists());
        let gate_home = worktree.path().join("tmp/gate-home");
        let _gate_home = EnvVarGuard::set(&[("HOME", gate_home.as_path())]);
        assert_eq!(
            homeboy_core::extension_store::available_extension_ids(),
            vec!["selected-fixture"]
        );
        assert_eq!(
            candidate.environment.extension_inputs,
            baseline.environment.extension_inputs
        );
    }

    #[cfg(unix)]
    #[test]
    fn isolated_gate_resolves_a_symlinked_extension_root_with_source_provenance() {
        let _guard = env_mutex();
        let worktree = tempfile::tempdir().expect("worktree");
        let source_root = tempfile::tempdir().expect("extension source root");
        let source = source_root.path().join("selected-fixture");
        let linked_root = tempfile::tempdir().expect("linked extension root");
        let link = linked_root.path().join("selected-fixture");
        fs::create_dir_all(&source).expect("selected extension");
        fs::write(
            source.join("selected-fixture.json"),
            r#"{"id":"selected-fixture","name":"Selected","version":"1.0.0"}"#,
        )
        .expect("selected manifest");
        fs::write(
            linked_root.path().join(".selected-fixture.source-revision"),
            "revision-123\n",
        )
        .expect("linked source revision");
        std::os::unix::fs::symlink(&source, &link).expect("linked extension root");
        let policy = AgentTaskGateEnvironmentPolicy {
            extension_inputs: vec![AgentTaskGateExtensionInput {
                id: "selected-fixture".to_string(),
                source: link.display().to_string(),
                identity: None,
            }],
            ..AgentTaskGateEnvironmentPolicy::default()
        };

        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            worktree.path(),
            1,
            "test -f \"$HOME/.config/homeboy/extensions/selected-fixture/selected-fixture.json\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(worktree.path()),
            &policy,
            &[],
        )
        .expect("symlinked extension gate");

        let input = &report.environment.extension_inputs[0];
        assert_eq!(input.source_revision.as_deref(), Some("revision-123"));
        assert_eq!(
            input.identity,
            extension_tree_identity(&source).expect("source identity")
        );
        let copied = worktree
            .path()
            .join("tmp/gate-home/.config/homeboy/extensions/selected-fixture");
        assert!(!fs::symlink_metadata(&copied)
            .expect("copied extension metadata")
            .file_type()
            .is_symlink());
        assert!(copied.join("selected-fixture.json").is_file());
    }

    #[test]
    fn replay_policy_rejects_extension_content_drift_after_candidate_evidence() {
        let _guard = env_mutex();
        let worktree = tempfile::tempdir().expect("worktree");
        let source = tempfile::tempdir().expect("extension source");
        fs::write(
            source.path().join("selected-fixture.json"),
            "candidate content",
        )
        .expect("candidate content");
        let policy = AgentTaskGateEnvironmentPolicy {
            extension_inputs: vec![AgentTaskGateExtensionInput {
                id: "selected-fixture".to_string(),
                source: source.path().display().to_string(),
                identity: None,
            }],
            ..AgentTaskGateEnvironmentPolicy::default()
        };
        let candidate = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            worktree.path(),
            1,
            "true",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(worktree.path()),
            &policy,
            &[],
        )
        .expect("candidate gate evidence");
        fs::write(
            source.path().join("selected-fixture.json"),
            "drifted content",
        )
        .expect("drifted content");

        let error = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            worktree.path(),
            1,
            "true",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            Some(worktree.path()),
            &candidate.environment.replay_policy(),
            &[],
        )
        .expect_err("replay must reject source content drift");

        assert_eq!(
            error.message,
            "Invalid argument 'gate_environment.extension_inputs': extension input no longer matches the candidate gate identity"
        );
        assert_eq!(error.details["field"], "gate_environment.extension_inputs");
    }

    #[test]
    fn replacing_gate_environment_preserves_declared_variables_and_reports_policy() {
        let temp = tempfile::tempdir().expect("tempdir");
        let policy = AgentTaskGateEnvironmentPolicy {
            mode: AgentTaskGateEnvironmentMode::Replace,
            variables: BTreeMap::from([("DECLARED_INPUT".to_string(), "kept".to_string())]),
            preserve: BTreeMap::new(),
            isolate_home: false,
            isolate_xdg: false,
            extension_inputs: Vec::new(),
        };
        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            temp.path(),
            8,
            "test \"$DECLARED_INPUT\" = kept && test -z \"${HOME:-}\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
            &[],
        )
        .expect("gate report");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(
            report.environment.mode,
            AgentTaskGateEnvironmentMode::Replace
        );
        assert_eq!(report.environment.inherited.len(), 1);
        assert_eq!(report.environment.inherited[0].name, "DECLARED_INPUT");
        assert_eq!(report.environment.inherited[0].value, "kept");
    }

    #[test]
    fn declared_package_artifact_maps_resource_preserves_isolation_and_records_provenance() {
        let workspace = tempfile::tempdir().expect("workspace");
        let artifact = workspace.path().join("fixtures/ready.bin");
        fs::create_dir_all(artifact.parent().expect("artifact parent")).expect("artifact parent");
        fs::write(&artifact, b"neutral fixture").expect("artifact");
        let digest = format!("sha256:{:x}", Sha256::digest(b"neutral fixture"));
        let requirement = AgentTaskGatePackageArtifactRequirement {
            id: "fixture-resource".to_string(),
            environment: AgentTaskGateArtifactEnvironmentMapping {
                name: "FIXTURE_RESOURCE_ROOT".to_string(),
                source: None,
                default: Some("fixtures".to_string()),
            },
            required_paths: vec![AgentTaskGateArtifactPathRequirement {
                path: "fixtures/ready.bin".to_string(),
                sha256: Some(digest),
            }],
            remediation: json!({"action": "refresh_fixture_resource"}),
        };

        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            workspace.path(),
            1,
            "test \"$FIXTURE_RESOURCE_ROOT\" = fixtures && test \"$HOME\" != fixtures",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &AgentTaskGateEnvironmentPolicy::default(),
            &[requirement],
        )
        .expect("declared resource is gate-ready");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        assert_eq!(report.environment.package_artifacts.len(), 1);
        assert_eq!(
            report.environment.package_artifacts[0].id,
            "fixture-resource"
        );
        assert_eq!(
            report.environment.package_artifacts[0].artifacts[0].path,
            "fixtures/ready.bin"
        );
    }

    #[test]
    fn missing_declared_package_artifact_stops_preflight_with_caller_remediation() {
        let workspace = tempfile::tempdir().expect("workspace");
        let requirement = AgentTaskGatePackageArtifactRequirement {
            id: "fixture-resource".to_string(),
            environment: AgentTaskGateArtifactEnvironmentMapping {
                name: "FIXTURE_RESOURCE_ROOT".to_string(),
                source: None,
                default: Some("fixtures".to_string()),
            },
            required_paths: vec![AgentTaskGateArtifactPathRequirement {
                path: "fixtures/missing.bin".to_string(),
                sha256: None,
            }],
            remediation: json!({"action": "refresh_fixture_resource"}),
        };

        let error = preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[],
            &[requirement],
            None,
            Duration::from_secs(1),
        )
        .expect_err("missing resource must stop provider preflight");

        assert_eq!(
            error.details["package_artifact_readiness"]["invalid_paths"],
            json!(["fixtures/missing.bin"])
        );
        assert_eq!(
            error.details["package_artifact_readiness"]["remediation"]["action"],
            "refresh_fixture_resource"
        );
    }

    /// Toolchain preflight is declared, never inferred. A gate command is a
    /// shell program: its first token can be a builtin, a provider-owned alias,
    /// or part of a compound expression, so probing it as an executable would
    /// change what the gate means. `c3462d472` made preflight opt-in for that
    /// reason and this test tracked the removed inference until #10658.
    #[test]
    fn gate_toolchain_requirements_are_explicit() {
        let options = VerifyGateOptions {
            verify: vec!["cargo test --lib".to_string()],
            private_verify: vec!["npm test".to_string()],
            gate_toolchains: vec![AgentTaskGateToolchainRequirement {
                command: "cargo".to_string(),
                probe_arguments: vec!["metadata".to_string()],
            }],
            ..VerifyGateOptions::default()
        };

        assert_eq!(
            options.required_toolchains(),
            vec![AgentTaskGateToolchainRequirement {
                command: "cargo".to_string(),
                probe_arguments: vec!["metadata".to_string()],
            }]
        );
    }

    /// A gate command whose first token is a shell builtin or part of a
    /// compound expression declares no toolchain at all. `c3462d472` made
    /// preflight opt-in precisely because a gate command is a shell program,
    /// so its first token is not necessarily an executable to probe.
    #[test]
    fn shell_gate_commands_contribute_no_inferred_toolchain() {
        let options = VerifyGateOptions {
            verify: vec![
                "test -f Cargo.toml && cargo test --lib".to_string(),
                "[ -d target ]".to_string(),
            ],
            private_verify: vec!["npm test".to_string()],
            ..VerifyGateOptions::default()
        };

        assert!(options.required_toolchains().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_preserves_only_declared_homes_for_cargo_on_path() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = env_mutex();
        let workspace = tempfile::tempdir().expect("workspace");
        let original_home = tempfile::tempdir().expect("original home");
        let cargo_bin = original_home.path().join(".cargo/bin");
        fs::create_dir_all(&cargo_bin).expect("cargo bin");
        let cargo = cargo_bin.join("cargo");
        fs::write(
            &cargo,
            format!(
                "#!/bin/sh\ntest \"$RUSTUP_HOME\" = \"{}/.rustup\" && test \"$CARGO_HOME\" = \"{}/.cargo\"\n",
                original_home.path().display(),
                original_home.path().display(),
            ),
        )
        .expect("cargo probe");
        let mut permissions = fs::metadata(&cargo).expect("cargo metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&cargo, permissions).expect("cargo executable");
        let _env = EnvVarGuard::set(&[
            ("HOME", original_home.path()),
            ("PATH", cargo_bin.as_path()),
        ]);

        let policy = AgentTaskGateEnvironmentPolicy {
            preserve: BTreeMap::from([
                ("RUSTUP_HOME".to_string(), "HOME/.rustup".to_string()),
                ("CARGO_HOME".to_string(), "HOME/.cargo".to_string()),
            ]),
            ..AgentTaskGateEnvironmentPolicy::default()
        };
        let result = preflight_gate_toolchains(
            workspace.path(),
            &policy,
            &[AgentTaskGateToolchainRequirement {
                command: "cargo".to_string(),
                probe_arguments: vec!["--version".to_string()],
            }],
            &[],
            None,
            Duration::from_secs(10),
        );

        result.expect("declared cargo homes initialize the toolchain");
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_reports_generic_initialization_failures_without_code_feedback() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = env_mutex();
        let workspace = tempfile::tempdir().expect("workspace");
        let bin = tempfile::tempdir().expect("bin");
        let tool = bin.path().join("other-tool");
        fs::write(&tool, "#!/bin/sh\necho unavailable >&2\nexit 7\n").expect("tool");
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("tool executable");
        let prior_path = std::env::var_os("PATH");
        std::env::set_var("PATH", bin.path());

        let error = preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[AgentTaskGateToolchainRequirement {
                command: "other-tool".to_string(),
                probe_arguments: vec!["initialize".to_string()],
            }],
            &[],
            None,
            Duration::from_secs(10),
        )
        .expect_err("unusable declared toolchain fails preflight");

        match prior_path {
            Some(value) => std::env::set_var("PATH", value),
            None => std::env::remove_var("PATH"),
        }
        assert!(error.message.contains("gate toolchain preflight"));
        assert!(!error.message.contains("Fix the code"));
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_runs_in_the_invocation_tmpdir() {
        use std::os::unix::fs::PermissionsExt;

        // Deliberately hermetic: the probe is addressed by absolute path so the
        // test never mutates PATH. Sibling tests spawn `sh` without holding
        // ENV_MUTEX, so clobbering PATH here would break them under the test
        // harness's parallelism.
        let workspace = tempfile::tempdir().expect("workspace");
        let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
        let bin = tempfile::tempdir().expect("bin");
        let gate_tmpdir = runtime_tmpdir.path().join("tmp");

        // Fails unless every temp-dir variable points at the invocation temp
        // dir. An unset variable compares as empty and exits non-zero too.
        let tool = bin.path().join("record-tmpdir");
        fs::write(
            &tool,
            format!(
                "#!/bin/sh\n[ \"$TMPDIR\" = '{dir}' ] || exit 3\n[ \"$TEMP\" = '{dir}' ] || exit 4\n[ \"$TMP\" = '{dir}' ] || exit 5\nexit 0\n",
                dir = gate_tmpdir.display()
            ),
        )
        .expect("tool");
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("tool executable");

        preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[AgentTaskGateToolchainRequirement {
                command: tool.display().to_string(),
                probe_arguments: vec!["initialize".to_string()],
            }],
            &[],
            Some(runtime_tmpdir.path()),
            Duration::from_secs(10),
        )
        .expect("preflight probes run in the invocation temp dir");
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_times_out_and_reaps_descendants() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().expect("workspace");
        let bin = tempfile::tempdir().expect("bin");
        let pid_file = workspace.path().join("descendant.pid");
        let tool = bin.path().join("hung-tool");
        fs::write(
            &tool,
            format!(
                "#!/bin/sh\n/bin/sleep 30 &\necho $! > '{}'\nwait\n",
                pid_file.display()
            ),
        )
        .expect("tool");
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("tool executable");

        let started = std::time::Instant::now();
        let error = preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[AgentTaskGateToolchainRequirement {
                command: tool.display().to_string(),
                probe_arguments: Vec::new(),
            }],
            &[],
            None,
            Duration::from_secs(2),
        )
        .expect_err("hung toolchain probe must time out");

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(error.retryable, Some(true));
        assert_eq!(
            error.details["toolchain_preflight"]["timed_out"], true,
            "{:#}",
            error.details
        );
        assert_eq!(error.details["toolchain_preflight"]["timeout_ms"], 2_000);
        let descendant_pid = fs::read_to_string(pid_file)
            .expect("descendant pid")
            .trim()
            .parse::<libc::pid_t>()
            .expect("numeric descendant pid");
        assert_ne!(unsafe { libc::kill(descendant_pid, 0) }, 0);
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_honors_a_100ms_limit() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().expect("workspace");
        let tool = workspace.path().join("hung-tool");
        fs::write(&tool, "#!/bin/sh\n/bin/sleep 30\n").expect("tool");
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("tool executable");

        let started = std::time::Instant::now();
        let error = preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[AgentTaskGateToolchainRequirement {
                command: tool.display().to_string(),
                probe_arguments: Vec::new(),
            }],
            &[],
            None,
            Duration::from_millis(100),
        )
        .expect_err("hung toolchain probe must time out");

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(error.details["toolchain_preflight"]["timed_out"], true);
        assert_eq!(error.details["toolchain_preflight"]["timeout_ms"], 100);
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_shares_one_total_deadline_across_probes() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().expect("workspace");
        let bin = tempfile::tempdir().expect("bin");
        let first = bin.path().join("first-tool");
        let second = bin.path().join("second-tool");
        fs::write(&first, "#!/bin/sh\n/bin/sleep 0.1\n").expect("first tool");
        fs::write(&second, "#!/bin/sh\n/bin/sleep 0.05\nexit 7\n").expect("second tool");
        for tool in [&first, &second] {
            let mut permissions = fs::metadata(tool).expect("tool metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(tool, permissions).expect("tool executable");
        }

        let started = std::time::Instant::now();
        let error = preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[
                AgentTaskGateToolchainRequirement {
                    command: first.display().to_string(),
                    probe_arguments: Vec::new(),
                },
                AgentTaskGateToolchainRequirement {
                    command: second.display().to_string(),
                    probe_arguments: Vec::new(),
                },
            ],
            &[],
            None,
            Duration::from_secs(10),
        )
        .expect_err("second probe must consume only the first probe's remaining deadline");

        assert!(started.elapsed() < Duration::from_secs(3));
        assert_eq!(error.details["toolchain_preflight"]["timed_out"], false);
        assert_eq!(
            error.details["toolchain_preflight"]["command"],
            second.display().to_string()
        );
        assert!(
            error.details["toolchain_preflight"]["probe_timeout_ms"]
                .as_u64()
                .expect("per-probe timeout")
                < 10_000
        );
    }

    #[cfg(unix)]
    #[test]
    fn toolchain_preflight_bounds_output_and_reports_capture_metadata() {
        use std::os::unix::fs::PermissionsExt;

        let workspace = tempfile::tempdir().expect("workspace");
        let bin = tempfile::tempdir().expect("bin");
        let tool = bin.path().join("noisy-tool");
        fs::write(
            &tool,
            "#!/bin/sh\nyes stdout | head -c 70000\nyes stderr | head -c 70000 >&2\nexit 7\n",
        )
        .expect("tool");
        let mut permissions = fs::metadata(&tool).expect("tool metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&tool, permissions).expect("tool executable");

        let error = preflight_gate_toolchains(
            workspace.path(),
            &AgentTaskGateEnvironmentPolicy::default(),
            &[AgentTaskGateToolchainRequirement {
                command: tool.display().to_string(),
                probe_arguments: Vec::new(),
            }],
            &[],
            None,
            Duration::from_secs(10),
        )
        .expect_err("noisy failed probe must return diagnostics");

        let diagnostics = &error.details["toolchain_preflight"];
        assert_eq!(diagnostics["capture_limit_bytes"], 65_536);
        assert_eq!(diagnostics["stdout"]["bytes_seen"], 70_000);
        assert_eq!(diagnostics["stderr"]["bytes_seen"], 70_000);
        assert_eq!(diagnostics["stdout"]["bytes_retained"], 65_536);
        assert_eq!(diagnostics["stderr"]["bytes_retained"], 65_536);
        assert_eq!(diagnostics["stdout"]["truncated"], true);
        assert_eq!(diagnostics["stderr"]["truncated"], true);
        assert_eq!(
            diagnostics["stdout"]["tail"]
                .as_str()
                .expect("stdout tail")
                .len(),
            65_536
        );
        assert_eq!(
            diagnostics["stderr"]["tail"]
                .as_str()
                .expect("stderr tail")
                .len(),
            65_536
        );
    }

    #[test]
    fn gate_environment_pins_temp_dir_variables_to_the_invocation_tmpdir() {
        let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
        let selected = selected_gate_environment(
            &AgentTaskGateEnvironmentPolicy::default(),
            Some(runtime_tmpdir.path()),
        )
        .expect("gate environment");

        let expected = runtime_tmpdir.path().join("tmp").display().to_string();
        for name in TMPDIR_ENV_VARS {
            assert_eq!(
                selected.values.get(*name),
                Some(&expected),
                "{name} must resolve to the invocation temp dir"
            );
        }
    }

    #[test]
    fn npm_missing_script_is_a_non_retryable_gate_declaration_failure() {
        let workspace = tempfile::tempdir().expect("workspace");
        fs::write(
            workspace.path().join("package.json"),
            r#"{"name":"fixture-package","scripts":{"test":"true"}}"#,
        )
        .expect("manifest");
        let gates = VerifyGateOptions {
            verify: vec!["npm run typecheck".to_string()],
            ..Default::default()
        };

        let error = gates
            .preflight_declarations(workspace.path())
            .expect_err("missing script is a declaration failure");

        assert_eq!(error.details["failure_classification"], "gate_declaration");
        assert_eq!(error.details["package"], "fixture-package");
        assert_eq!(error.details["missing_script"], "typecheck");
        assert!(error.details["remediation"]
            .as_str()
            .expect("remediation")
            .contains("scripts"));
    }

    /// A symlinked invocation temp alias must yield sandbox paths that are
    /// simultaneously **short** and **non-symlink**.
    ///
    /// Resolving the alias away (canonicalizing) would satisfy the non-symlink
    /// half while discarding the `sockaddr_un` budget the invocation layer
    /// validated on the alias, so this asserts the exported paths stay *under
    /// the alias* rather than under its canonical target.
    #[cfg(unix)]
    #[test]
    fn gate_environment_exports_a_real_directory_beneath_the_invocation_tmp_alias() {
        let runtime_tmpdir = tempfile::tempdir().expect("runtime tmpdir");
        let alias_root = tempfile::tempdir().expect("alias root");
        let alias = alias_root.path().join("runtime-tmp-alias");
        std::os::unix::fs::symlink(runtime_tmpdir.path(), &alias).expect("runtime temp alias");

        let selected =
            selected_gate_environment(&AgentTaskGateEnvironmentPolicy::default(), Some(&alias))
                .expect("gate environment");
        let expected_root = alias.join("tmp");

        for name in TMPDIR_ENV_VARS {
            assert_eq!(
                selected.values.get(*name).map(PathBuf::from),
                Some(expected_root.clone()),
                "{name} must stay beneath the short invocation temp alias"
            );
        }

        // The exported root is reached *through* the alias, but is itself a
        // real directory — the property security-sensitive child tools check.
        assert!(!fs::symlink_metadata(&expected_root)
            .expect("gate temp root metadata")
            .file_type()
            .is_symlink());
        assert!(expected_root.is_dir());

        for variable in &selected.report.sanitized {
            let path = PathBuf::from(&variable.value);
            assert!(path.starts_with(&expected_root));
            assert!(!fs::symlink_metadata(path)
                .expect("isolated path metadata")
                .file_type()
                .is_symlink());
        }
    }
}
