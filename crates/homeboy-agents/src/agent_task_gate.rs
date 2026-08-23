use std::collections::BTreeMap;
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
const RUST_CACHE_SCHEMA: &str = "homeboy/gate-rust-cache/v2";
const RUST_CACHE_LOCK_TIMEOUT: Duration = Duration::from_secs(120);

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
    /// Rust cache hydration follows the enclosing dependency-hydration policy.
    #[serde(default = "default_hydrate_dependencies")]
    pub hydrate_rust_cache: bool,
    /// Explicit gate override. When omitted, inherit the component's declared
    /// managed-execution policy for the gate workspace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shared_cargo_target: Option<bool>,
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
            hydrate_rust_cache: true,
            shared_cargo_target: None,
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
            input_sources: Vec::new(),
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
    /// Controller-owned Rust cache setup evidence. This deliberately records
    /// cache state rather than exposing a controller filesystem path to gates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rust_cache: Option<AgentTaskGateRustCacheEvidence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cargo_target: Option<AgentTaskGateCargoTargetEvidence>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateCargoTargetEvidence {
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub elapsed_ms: Option<u128>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AgentTaskGateRustCacheEvidence {
    pub identity: String,
    pub state: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wait_ms: Option<u128>,
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
            && self.rust_cache.is_none()
            && self.cargo_target.is_none()
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
            hydrate_rust_cache: true,
            shared_cargo_target: self.cargo_target.as_ref().map(|_| true),
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
/// `--exact`; recording both the declared interpretation and test IDs makes
/// focused evidence independently reviewable.
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
    /// closed rather than proving exactly one selected test.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<AgentTaskGateCargoSelectionRecovery>,
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
            cwd: None,
            failure_evidence,
            test_result: None,
            cargo_selection: None,
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
            cwd: None,
            failure_evidence: None,
            test_result: None,
            cargo_selection: None,
            skip_reason: Some(skip_reason),
            baseline_comparison: None,
            candidate_checkout: None,
            environment: AgentTaskGateEnvironment::default(),
        }
    }

    pub(crate) fn accept_inherited_failure(&mut self) {
        // Only a candidate-code failure can be explained by an identical base
        // replay. Gate declarations and invalid focused selections are intrinsic
        // policy failures, even when the base has the same bad configuration.
        if !self.failure_evidence.as_ref().is_some_and(|evidence| {
            evidence.classification == AgentTaskGateFailureClassification::CandidateCode
        }) {
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
        assert!(
            !homeboy_engine_primitives::command::process_is_running(descendant_pid as u32),
            "bounded baseline gate left descendant {descendant_pid} runnable"
        );
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
    selected_environment.configure_rust_cache(cwd)?;
    selected_environment.configure_cargo_target(cwd, gate_environment.shared_cargo_target)?;
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
    let target_started = Instant::now();
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
    let runner_exit_code = match termination {
        AgentTaskGateTermination::TimedOut => 124,
        AgentTaskGateTermination::NoProgress => 125,
        _ => output.status.code().unwrap_or(1),
    };
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let test_result = cargo_test_result(command, runner_exit_code, &stdout, &stderr);
    let cargo_selection = cargo_selection(
        command,
        &command_vec,
        &stdout,
        &stderr,
        test_result.as_ref(),
    );
    let exit_code = effective_gate_exit_code(runner_exit_code, cargo_selection.as_ref());
    let failure_evidence = (exit_code != 0).then(|| {
        gate_failure_evidence(
            command,
            exit_code,
            &stdout,
            &stderr,
            cargo_selection.as_ref(),
        )
    });

    selected_environment.finish_cargo_target(target_started.elapsed())?;
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
    report.test_result = test_result;
    report.cargo_selection = cargo_selection;
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
    selected_environment.configure_rust_cache(cwd)?;
    selected_environment.configure_cargo_target(cwd, gate_environment.shared_cargo_target)?;
    selected_environment.report.package_artifacts = package_artifacts;
    selected_environment.apply(&mut process);
    homeboy_core::engine::command::isolate_process_tree(&mut process);
    let target_started = Instant::now();
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
    let runner_exit_code = if timed_out {
        stderr.push_str(&format!(
            "\nbaseline gate exceeded {} ms and was cancelled",
            timeout.as_millis()
        ));
        124
    } else {
        output.status.code().unwrap_or(1)
    };
    let test_result = cargo_test_result(command, runner_exit_code, &stdout, &stderr);
    let cargo_selection = cargo_selection(
        command,
        &command_vec,
        &stdout,
        &stderr,
        test_result.as_ref(),
    );
    let exit_code = effective_gate_exit_code(runner_exit_code, cargo_selection.as_ref());
    let failure_evidence = (exit_code != 0).then(|| {
        gate_failure_evidence(
            command,
            exit_code,
            &stdout,
            &stderr,
            cargo_selection.as_ref(),
        )
    });
    selected_environment.finish_cargo_target(target_started.elapsed())?;
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
    report.test_result = test_result;
    report.cargo_selection = cargo_selection;
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
    hydrate_rust_cache: bool,
    _cargo_target: Option<homeboy_core::cleanup::ManagedCargoTarget>,
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

    fn configure_rust_cache(&mut self, cwd: &Path) -> Result<()> {
        if !self.hydrate_rust_cache
            || !cwd.join("Cargo.lock").is_file()
            || !cwd.join("rust-toolchain.toml").is_file()
        {
            return Ok(());
        }

        let identity = rust_cache_identity(cwd)?;
        let root = homeboy_core::paths::homeboy_data()
            .map_err(|error| error.with_hint(rust_cache_repair_command()))?
            .join("controller-state/gate-rust-cache");
        let cache = root.join(&identity);
        ensure_safe_rust_cache_root(&root)?;
        fs::create_dir_all(&cache).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("create Rust gate cache {identity}")),
            )
            .with_hint(rust_cache_repair_command())
        })?;
        ensure_safe_rust_cache_root(&cache)?;

        let marker = cache.join("ready.json");
        let mut state = "hit";
        let mut wait_ms = None;
        let toolchain_cargo_relative;
        if let Some(relative) = valid_marker_toolchain_cargo(&marker, &identity, &cache)? {
            toolchain_cargo_relative = relative;
        } else {
            let lock_path = cache.join("hydrate.lock");
            let lock = OpenOptions::new()
                .create(true)
                .read(true)
                .write(true)
                .open(&lock_path)
                .map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some("open Rust gate cache lock".to_string()),
                    )
                })?;
            let started = Instant::now();
            loop {
                let acquired = match lock.try_lock_exclusive() {
                    Ok(acquired) => acquired,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => false,
                    Err(error) => {
                        return Err(Error::internal_io(
                            error.to_string(),
                            Some("lock Rust gate cache".to_string()),
                        )
                        .with_hint(rust_cache_repair_command()));
                    }
                };
                if acquired {
                    break;
                }
                if started.elapsed() >= RUST_CACHE_LOCK_TIMEOUT {
                    return Err(Error::internal_io(
                        format!("timed out waiting for Rust gate cache {identity}"),
                        Some("hydrate Rust gate cache".to_string()),
                    )
                    .with_hint(rust_cache_repair_command()));
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            let waited = started.elapsed().as_millis();
            if let Some(relative) = valid_marker_toolchain_cargo(&marker, &identity, &cache)? {
                state = if waited > 0 { "wait" } else { "hit" };
                wait_ms = (waited > 0).then_some(waited);
                toolchain_cargo_relative = relative;
            } else {
                let relative = hydrate_rust_cache(cwd, &cache)?;
                fs::write(
                    &marker,
                    serde_json::to_vec(&json!({
                        "schema": RUST_CACHE_SCHEMA,
                        "identity": identity,
                        "toolchain_cargo_relative": relative,
                    }))
                    .expect("serialize Rust cache marker"),
                )
                .map_err(|error| {
                    Error::internal_io(
                        error.to_string(),
                        Some("write Rust gate cache marker".to_string()),
                    )
                    .with_hint(rust_cache_repair_command())
                })?;
                state = "hydrated";
                wait_ms = (waited > 0).then_some(waited);
                toolchain_cargo_relative = relative;
            }
        }
        let home = self.values.get("HOME").map(PathBuf::from).ok_or_else(|| {
            Error::validation_invalid_argument(
                "gate_environment.isolate_home",
                "Rust gate caching requires HOME isolation",
                None,
                None,
            )
        })?;
        let overlay = home
            .join(".homeboy-rust-cache")
            .join(uuid::Uuid::new_v4().to_string());
        copy_tree_preserving_safe_symlinks(&cache.join("cargo"), &overlay.join("cargo"))?;
        copy_tree_preserving_safe_symlinks(&cache.join("rustup"), &overlay.join("rustup"))?;
        self.values.insert(
            "CARGO_HOME".to_string(),
            overlay.join("cargo").display().to_string(),
        );
        self.values.insert(
            "RUSTUP_HOME".to_string(),
            overlay.join("rustup").display().to_string(),
        );
        let toolchain_bin = overlay
            .join("rustup")
            .join(&toolchain_cargo_relative)
            .parent()
            .expect("validated toolchain Cargo path has a parent")
            .to_path_buf();
        let inherited_path = self
            .values
            .get("PATH")
            .map(std::ffi::OsString::from)
            .or_else(|| std::env::var_os("PATH"))
            .unwrap_or_default();
        let path = std::env::join_paths(
            std::iter::once(toolchain_bin).chain(std::env::split_paths(&inherited_path)),
        )
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("configure Rust gate toolchain PATH".to_string()),
            )
        })?;
        self.values
            .insert("PATH".to_string(), path.to_string_lossy().to_string());
        self.report.rust_cache = Some(AgentTaskGateRustCacheEvidence {
            identity,
            state: state.to_string(),
            wait_ms,
        });
        Ok(())
    }

    fn configure_cargo_target(&mut self, cwd: &Path, override_enabled: Option<bool>) -> Result<()> {
        let enabled = override_enabled.unwrap_or_else(|| {
            homeboy_core::component::resolve_effective(None, Some(&cwd.to_string_lossy()), None)
                .map(|component| component.managed_execution.shared_cargo_target)
                .unwrap_or(false)
        });
        if !enabled {
            return Ok(());
        }
        let explicit_target = self
            .values
            .get("CARGO_TARGET_DIR")
            .cloned()
            .or_else(|| std::env::var("CARGO_TARGET_DIR").ok());
        let target = homeboy_core::cleanup::acquire_managed_cargo_target_for_environment(
            "agent-task-gate",
            cwd,
            explicit_target.as_deref(),
            &self.values,
        )?;
        // Store sizing is evidence only. A concurrent gate may update the
        // shared target while this observation walks it.
        let bytes_before = target.size_bytes().ok();
        // The managed store identity is repository-scoped. Cargo separates all
        // source, feature, profile, target, and toolchain fingerprints within it.
        let identity = (target.resolution() == "shared")
            .then(|| target.target_dir().file_name())
            .flatten()
            .map(|name| name.to_string_lossy().to_string());
        self.values.insert(
            "CARGO_TARGET_DIR".to_string(),
            target.target_dir().to_string_lossy().to_string(),
        );
        self.report.cargo_target = Some(AgentTaskGateCargoTargetEvidence {
            path: target.target_dir().to_string_lossy().to_string(),
            resolution: target.resolution().to_string(),
            owner: target.evidence().owner,
            identity,
            state: bytes_before.map(|bytes| if bytes == 0 { "miss" } else { "hit" }.to_string()),
            bytes_before,
            bytes_after: None,
            elapsed_ms: None,
        });
        self._cargo_target = Some(target);
        Ok(())
    }

    fn finish_cargo_target(&mut self, elapsed: Duration) -> Result<()> {
        let (Some(target), Some(evidence)) = (&self._cargo_target, &mut self.report.cargo_target)
        else {
            return Ok(());
        };
        evidence.bytes_after = target.size_bytes().ok();
        evidence.elapsed_ms = Some(elapsed.as_millis());
        Ok(())
    }
}

fn rust_cache_identity(cwd: &Path) -> Result<String> {
    let mut hasher = Sha256::new();
    for path in [
        cwd.join("rust-toolchain.toml"),
        cwd.join("Cargo.lock"),
        cwd.join(".cargo/config.toml"),
        cwd.join(".cargo/config"),
    ] {
        let bytes = fs::read(&path).unwrap_or_default();
        if bytes.is_empty() && !path.is_file() {
            hasher.update(b"absent");
        }
        if path
            .file_name()
            .is_some_and(|name| name == "Cargo.lock" || name == "rust-toolchain.toml")
            && bytes.is_empty()
        {
            return Err(Error::internal_io(
                "required Rust cache identity input is unreadable".to_string(),
                Some(format!("read Rust cache identity {}", path.display())),
            ));
        }
        if !bytes.is_empty() {
            hasher.update((bytes.len() as u64).to_le_bytes());
            hasher.update(bytes);
        }
    }
    hasher.update(std::env::consts::OS.as_bytes());
    hasher.update(b"/");
    hasher.update(std::env::consts::ARCH.as_bytes());
    Ok(format!("{:x}", hasher.finalize()))
}

fn valid_marker_toolchain_cargo(
    marker: &Path,
    identity: &str,
    cache: &Path,
) -> Result<Option<PathBuf>> {
    let Ok(bytes) = fs::read(marker) else {
        return Ok(None);
    };
    let marker: serde_json::Value = serde_json::from_slice(&bytes).map_err(|_| {
        Error::internal_io(
            "Rust gate cache marker is corrupt".to_string(),
            Some("validate Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    if marker["schema"] != RUST_CACHE_SCHEMA || marker["identity"] != identity {
        Err(Error::internal_io(
            "Rust gate cache marker does not match its content address".to_string(),
            Some("validate Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command()))
    } else {
        let Some(relative) = marker["toolchain_cargo_relative"].as_str() else {
            return Ok(None);
        };
        let relative = PathBuf::from(relative);
        if relative.is_absolute()
            || !relative.starts_with("toolchains")
            || !cache.join("rustup").join(&relative).is_file()
        {
            return Ok(None);
        }
        Ok(Some(relative))
    }
}

fn hydrate_rust_cache(cwd: &Path, cache: &Path) -> Result<PathBuf> {
    hydrate_rust_cache_with_timeout(cwd, cache, RUST_CACHE_LOCK_TIMEOUT)
}

fn hydrate_rust_cache_with_timeout(cwd: &Path, cache: &Path, timeout: Duration) -> Result<PathBuf> {
    let environment = BTreeMap::from([
        ("HOME".to_string(), cache.join("home").display().to_string()),
        (
            "CARGO_HOME".to_string(),
            cache.join("cargo").display().to_string(),
        ),
        (
            "RUSTUP_HOME".to_string(),
            cache.join("rustup").display().to_string(),
        ),
    ]);
    // The host rustup proxy refuses to run after CARGO_HOME is isolated unless
    // the proxy itself lives under that new home. Seed the cache with a real
    // file rather than inheriting the host home into controller-owned state.
    let rustup_proxy = prepare_rustup_proxy(cache)?;
    let rustup = run_rust_cache_command(
        cwd,
        cache,
        &environment,
        "install_toolchain",
        &rustup_proxy,
        &["toolchain", "install"],
        timeout,
    )?;
    drop(rustup);
    let resolved = run_rust_cache_command(
        cwd,
        cache,
        &environment,
        "resolve_toolchain_cargo",
        &rustup_proxy,
        &["which", "cargo"],
        timeout,
    )?;
    let cargo = PathBuf::from(String::from_utf8_lossy(&resolved.stdout).trim());
    let rustup_home = cache.join("rustup");
    let relative = cargo.strip_prefix(&rustup_home).map_err(|_| {
        Error::internal_io(
            format!(
                "Rust cache hydration resolved Cargo outside isolated RUSTUP_HOME: {}",
                cargo.display()
            ),
            Some("resolve_toolchain_cargo".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    if !relative.starts_with("toolchains") || !cargo.is_file() {
        return Err(Error::internal_io(
            format!(
                "Rust cache hydration resolved an invalid toolchain Cargo executable: {}",
                cargo.display()
            ),
            Some("resolve_toolchain_cargo".to_string()),
        )
        .with_hint(rust_cache_repair_command()));
    }
    run_rust_cache_command(
        cwd,
        cache,
        &environment,
        "fetch_locked",
        &cargo,
        &["fetch", "--locked"],
        timeout,
    )?;
    Ok(relative.to_path_buf())
}

fn prepare_rustup_proxy(cache: &Path) -> Result<PathBuf> {
    let host_path = std::env::var_os("PATH").ok_or_else(|| {
        Error::validation_invalid_argument(
            "PATH",
            "unavailable for Rust gate cache hydration",
            None,
            None,
        )
    })?;
    let source = std::env::split_paths(&host_path)
        .map(|directory| directory.join("rustup"))
        .find(|candidate| candidate.is_file())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "rustup",
                "unavailable for Rust gate cache hydration",
                None,
                None,
            )
        })?;
    let destination = cache.join("cargo/bin/rustup");
    fs::create_dir_all(destination.parent().expect("rustup proxy has a parent")).map_err(
        |error| {
            Error::internal_io(
                error.to_string(),
                Some("prepare isolated Rust gate cache rustup proxy".to_string()),
            )
        },
    )?;
    fs::copy(&source, &destination).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("prepare isolated Rust gate cache rustup proxy".to_string()),
        )
    })?;
    Ok(destination)
}

fn run_rust_cache_command(
    cwd: &Path,
    cache: &Path,
    environment: &BTreeMap<String, String>,
    phase: &str,
    program: &Path,
    arguments: &[&str],
    timeout: Duration,
) -> Result<std::process::Output> {
    let mut process = Command::new(program);
    process
        .current_dir(cwd)
        .args(arguments)
        .env_clear()
        .envs(environment)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let path = std::env::var_os("PATH").ok_or_else(|| {
        Error::validation_invalid_argument(
            "PATH",
            "unavailable for Rust gate cache hydration",
            None,
            None,
        )
    })?;
    process.env("PATH", path);
    homeboy_core::engine::command::isolate_process_tree(&mut process);
    let mut child = process.spawn().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("Rust gate cache hydration phase {phase}")),
        )
    })?;
    let started = Instant::now();
    let mut timed_out = false;
    let output = homeboy_core::engine::command::wait_with_bounded_output_until_cancelled(
        &mut child,
        GATE_TOOLCHAIN_CAPTURE_LIMIT_BYTES,
        || {
            timed_out = started.elapsed() >= timeout;
            timed_out
        },
    )
    .map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some("hydrate Rust gate cache".to_string()),
        )
    })?
    .into_output();
    if timed_out || !output.status.success() {
        return Err(Error::internal_io(
            format!(
                "Rust gate cache hydration phase {phase} {} (cache {}, command `{} {}`): {}",
                if timed_out { "timed out" } else { "failed" },
                cache.display(),
                program.display(),
                arguments.join(" "),
                String::from_utf8_lossy(&output.stderr).trim()
            ),
            Some(format!("Rust gate cache hydration phase {phase}")),
        )
        .with_hint(rust_cache_repair_command()));
    }
    Ok(output)
}

/// Clone controller-owned cache bytes into a gate-owned overlay. Links may only
/// resolve within the authoritative source tree, and absolute links are rebased
/// so the gate never retains a path back into controller-owned cache state.
fn copy_tree_preserving_safe_symlinks(source: &Path, destination: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(source).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("inspect Rust cache {}", source.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::internal_io(
            "Rust cache contains an unsafe root".to_string(),
            Some("copy Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command()));
    }
    let source_root = source.canonicalize().map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("resolve Rust cache root {}", source.display())),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    copy_rust_cache_tree(source, destination, &source_root, destination)
}

fn copy_rust_cache_tree(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    fs::create_dir_all(destination).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create Rust overlay {}", destination.display())),
        )
    })?;
    let mut entries = fs::read_dir(source)
        .map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("read Rust cache {}", source.display())),
            )
        })?
        .collect::<std::io::Result<Vec<_>>>()
        .map_err(|error| {
            Error::internal_io(error.to_string(), Some("read Rust cache entry".to_string()))
        })?;
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        let file_type = entry.file_type().map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some("inspect Rust cache entry".to_string()),
            )
        })?;
        if file_type.is_symlink() {
            copy_safe_rust_cache_symlink(
                &source_path,
                &destination_path,
                source_root,
                destination_root,
            )?;
        } else if file_type.is_dir() {
            copy_rust_cache_tree(
                &source_path,
                &destination_path,
                source_root,
                destination_root,
            )?;
        } else if file_type.is_file() {
            fs::copy(&source_path, &destination_path).map_err(|error| {
                Error::internal_io(
                    error.to_string(),
                    Some("copy Rust gate cache entry".to_string()),
                )
            })?;
        } else {
            return Err(Error::internal_io(
                "Rust cache contains an unsupported file type".to_string(),
                Some("copy Rust gate cache".to_string()),
            )
            .with_hint(rust_cache_repair_command()));
        }
    }
    Ok(())
}

fn copy_safe_rust_cache_symlink(
    source: &Path,
    destination: &Path,
    source_root: &Path,
    destination_root: &Path,
) -> Result<()> {
    let target = fs::read_link(source).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("read Rust cache symlink {}", source.display())),
        )
    })?;
    let resolved = source.canonicalize().map_err(|error| {
        let kind = if error.raw_os_error() == Some(libc::ELOOP) {
            "cycle"
        } else if error.kind() == std::io::ErrorKind::NotFound {
            "dangling target"
        } else {
            "unresolvable target"
        };
        Error::internal_io(
            format!(
                "Rust cache symlink {} has a {kind}: {}",
                source.display(),
                target.display()
            ),
            Some("copy Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    let relative = resolved.strip_prefix(source_root).map_err(|_| {
        Error::internal_io(
            format!(
                "Rust cache symlink {} escapes its source root: {}",
                source.display(),
                resolved.display()
            ),
            Some("copy Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command())
    })?;
    let overlay_target = if target.is_absolute() {
        destination_root.join(relative)
    } else {
        target
    };
    create_rust_cache_symlink(&overlay_target, destination, resolved.is_dir()).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("copy Rust cache symlink {}", source.display())),
        )
    })
}

#[cfg(unix)]
fn create_rust_cache_symlink(
    target: &Path,
    link: &Path,
    _target_is_dir: bool,
) -> std::io::Result<()> {
    std::os::unix::fs::symlink(target, link)
}

#[cfg(windows)]
fn create_rust_cache_symlink(
    target: &Path,
    link: &Path,
    target_is_dir: bool,
) -> std::io::Result<()> {
    if target_is_dir {
        std::os::windows::fs::symlink_dir(target, link)
    } else {
        std::os::windows::fs::symlink_file(target, link)
    }
}

fn rust_cache_repair_command() -> String {
    "rm -rf \"$(homeboy paths data)/controller-state/gate-rust-cache\"".to_string()
}

fn ensure_safe_rust_cache_root(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("create {}", path.display())),
        )
    })?;
    let metadata = fs::symlink_metadata(path).map_err(|error| {
        Error::internal_io(
            error.to_string(),
            Some(format!("inspect {}", path.display())),
        )
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(Error::internal_io(
            "Rust gate cache root is not a real directory".to_string(),
            Some("validate Rust gate cache".to_string()),
        )
        .with_hint(rust_cache_repair_command()));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        if metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o022 != 0
        {
            return Err(Error::internal_io(
                "Rust gate cache root has unsafe ownership or permissions".to_string(),
                Some("validate Rust gate cache".to_string()),
            )
            .with_hint(rust_cache_repair_command()));
        }
        fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
            Error::internal_io(
                error.to_string(),
                Some(format!("secure {}", path.display())),
            )
        })?;
    }
    Ok(())
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
        hydrate_rust_cache: policy.hydrate_rust_cache,
        _cargo_target: None,
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
            let observed = if path.is_file() {
                let bytes = fs::read(&path).map_err(|error| {
                    Error::internal_io(error.to_string(), Some(path.display().to_string()))
                })?;
                Some(format!("sha256:{:x}", Sha256::digest(bytes)))
            } else {
                None
            };
            if let Some(expected) = &artifact.sha256 {
                let actual = observed.as_ref().ok_or_else(|| {
                    package_artifact_error(
                        requirement,
                        "required artifact digest cannot be verified for a non-file path",
                        vec![artifact.path.clone()],
                    )
                })?;
                if expected != actual {
                    missing.push(artifact.path.clone());
                    continue;
                }
            }
            artifacts.push(AgentTaskGateArtifactPathProvenance {
                path: artifact.path.clone(),
                sha256: observed.or_else(|| artifact.sha256.clone()),
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
    cargo_selection: Option<&AgentTaskGateCargoSelection>,
) -> AgentTaskGateFailureEvidence {
    let stdout_tail = text_tail(stdout, 20);
    let stderr_tail = text_tail(stderr, 20);
    let missing_script = npm_run_script(command).filter(|script| {
        stderr.contains(&format!("Missing script: \"{script}\""))
            || stderr.contains(&format!("Missing script: {script}"))
    });
    let invalid_focused_selection = cargo_selection.is_some_and(|selection| {
        selection.mode == "focused"
            && (selection.filter_interpretation != "exact" || selection.selected_count != 1)
    });
    let classification = invalid_focused_selection
        .then_some(AgentTaskGateFailureClassification::ZeroTestsSelected)
        .or_else(|| {
            missing_script
                .is_some()
                .then_some(AgentTaskGateFailureClassification::GateDeclaration)
        })
        .unwrap_or(AgentTaskGateFailureClassification::CandidateCode);
    let summary = if let Some(selection) = cargo_selection.filter(|_| invalid_focused_selection) {
        format!(
            "invalid_focused_cargo_selection: filter {:?} selected {} test IDs ({:?}) with {} interpretation",
            selection.filter,
            selection.selected_count,
            selection.selected_ids,
            selection.filter_interpretation,
        )
    } else {
        match missing_script {
            Some(script) => format!("declared npm gate is missing script `{script}`: {command}"),
            None => format!("deterministic gate failed with exit code {exit_code}: {command}"),
        }
    };
    let agent_feedback = if invalid_focused_selection {
        cargo_selection
            .and_then(|selection| selection.recovery.as_ref())
            .map(|recovery| match recovery.action.as_str() {
                "rerun_exact" => format!(
                    "The Cargo test gate must use an exact filter and execute exactly one test. Rerun the structured exact recovery command `{}` before rerunning Cook.",
                    recovery.command
                ),
                _ => format!(
                    "The Cargo test gate must use an exact filter and execute exactly one test. Run the structured discovery command `{}` and choose one of the bounded candidate IDs before rerunning Cook.",
                    recovery.command
                ),
            })
            .unwrap_or_else(|| {
                "The Cargo test gate must use an exact filter and execute exactly one test before rerunning Cook.".to_string()
            })
    } else {
        match missing_script {
            Some(script) => format!(
                "The declared gate is invalid, not candidate-code feedback. Add `scripts.{script}` to the relevant package.json or change/remove `{command}` before rerunning Cook."
            ),
            None => format!(
                "A deterministic verification gate failed after the candidate patch was applied. Fix the code so `{command}` passes, using the captured stdout/stderr tails as the primary failure evidence."
            ),
        }
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

fn effective_gate_exit_code(
    runner_exit_code: i32,
    cargo_selection: Option<&AgentTaskGateCargoSelection>,
) -> i32 {
    if cargo_selection.is_some_and(|selection| {
        selection.mode == "focused"
            && (selection.filter_interpretation != "exact" || selection.selected_count != 1)
    }) {
        1
    } else {
        runner_exit_code
    }
}

fn cargo_selection(
    command: &str,
    effective_argv: &[String],
    stdout: &str,
    stderr: &str,
    test_result: Option<&AgentTaskGateTestResult>,
) -> Option<AgentTaskGateCargoSelection> {
    let tokens = crate::agent_task::tokenize_command(command);
    let cargo_index = cargo_test_index(&tokens)?;
    let args = &tokens[cargo_index + 1..];
    let test_index = args.iter().position(|token| token == "test")?;
    let harness = args.iter().position(|token| token == "--");
    let filter = cargo_test_filter(&args[test_index + 1..harness.unwrap_or(args.len())]);
    let exact =
        harness.is_some_and(|index| args[index + 1..].iter().any(|token| token == "--exact"));
    let mode = if filter.is_some() { "focused" } else { "broad" };
    let mut selected_ids = cargo_test_ids(stdout, stderr);
    if selected_ids.is_empty() && exact && test_result.is_some_and(|result| result.total == 1) {
        if let Some(filter) = &filter {
            selected_ids.push(filter.clone());
        }
    }
    let discovered_ids = {
        let listed = cargo_test_list_ids(stdout, stderr);
        if listed.is_empty() {
            selected_ids.clone()
        } else {
            listed
        }
    };
    let selected_count = selected_ids.len();
    let recovery = cargo_selection_recovery(
        &tokens,
        cargo_index,
        &discovered_ids,
        selected_count,
        exact,
        has_shell_control_operator(command),
    );
    Some(AgentTaskGateCargoSelection {
        effective_argv: effective_argv.to_vec(),
        mode: mode.to_string(),
        filter: filter.clone(),
        filter_interpretation: match (filter.is_some(), exact) {
            (false, _) => "broad_explicit".to_string(),
            (true, true) => "exact".to_string(),
            (true, false) => "substring_ambiguous".to_string(),
        },
        discovered_ids,
        selected_count,
        selected_ids,
        recovery,
    })
}

fn cargo_selection_recovery(
    tokens: &[String],
    cargo_index: usize,
    discovered_ids: &[String],
    selected_count: usize,
    exact: bool,
    has_shell_control: bool,
) -> Option<AgentTaskGateCargoSelectionRecovery> {
    let test_index = tokens[cargo_index + 1..]
        .iter()
        .position(|token| token == "test")?
        + cargo_index
        + 1;
    let harness = tokens[test_index + 1..]
        .iter()
        .position(|token| token == "--")
        .map(|index| index + test_index + 1);
    let filter_index =
        cargo_test_filter_index(&tokens[test_index + 1..harness.unwrap_or(tokens.len())])
            .map(|index| index + test_index + 1);
    let focused = filter_index.is_some();
    let valid = focused && exact && selected_count == 1;
    if !focused || valid {
        return None;
    }

    let filter_index = filter_index?;
    let mut argv = tokens[..filter_index].to_vec();
    let action = if discovered_ids.len() == 1 && !has_shell_control {
        argv.push(discovered_ids[0].clone());
        "rerun_exact"
    } else {
        "discover_and_choose"
    };
    let before_harness_end = harness.unwrap_or(tokens.len());
    argv.extend_from_slice(&tokens[filter_index + 1..before_harness_end]);
    if let Some(harness) = harness {
        argv.push("--".to_string());
        argv.extend(
            tokens[harness + 1..]
                .iter()
                .filter(|argument| argument.as_str() != "--exact")
                .cloned(),
        );
    } else {
        argv.push("--".to_string());
    }
    argv.push(if action == "rerun_exact" {
        "--exact".to_string()
    } else {
        "--list".to_string()
    });
    let argv = redact_cargo_recovery_argv(argv);
    Some(AgentTaskGateCargoSelectionRecovery {
        action: action.to_string(),
        command: render_cargo_recovery_command(&argv),
        argv,
        candidate_ids: discovered_ids
            .iter()
            .take(MAX_CARGO_SELECTION_RECOVERY_CANDIDATES)
            .map(|id| homeboy_core::redaction::redact_string(id))
            .collect(),
    })
}

/// A tokenized shell command cannot faithfully retain control-flow syntax.
/// Recovery therefore declines an exact rerun whenever the declaration uses a
/// shell operator, while still exposing bounded discovery metadata.
fn has_shell_control_operator(command: &str) -> bool {
    let mut quote = None;
    let mut escaped = false;
    for character in command.chars() {
        if escaped {
            escaped = false;
            continue;
        }
        if character == '\\' && quote != Some('\'') {
            escaped = true;
            continue;
        }
        if matches!(character, '\'' | '"') {
            if quote == Some(character) {
                quote = None;
            } else if quote.is_none() {
                quote = Some(character);
            }
            continue;
        }
        if quote.is_none()
            && matches!(
                character,
                ';' | '|' | '&' | '(' | ')' | '{' | '}' | '<' | '>'
            )
        {
            return true;
        }
    }
    false
}

fn render_cargo_recovery_command(argv: &[String]) -> String {
    let assignment_count = argv
        .iter()
        .take_while(|argument| is_environment_assignment(argument))
        .count();
    let mut rendered = argv[..assignment_count]
        .iter()
        .filter_map(|assignment| {
            let (name, value) = assignment.split_once('=')?;
            Some(format!(
                "{name}={}",
                homeboy_core::engine::shell::quote_arg(value)
            ))
        })
        .collect::<Vec<_>>();
    rendered.extend(
        argv[assignment_count..]
            .iter()
            .map(|argument| homeboy_core::engine::shell::quote_arg(argument)),
    );
    rendered.join(" ")
}

fn redact_cargo_recovery_argv(argv: Vec<String>) -> Vec<String> {
    argv.into_iter()
        .map(|argument| homeboy_core::redaction::redact_string(&argument))
        .collect()
}

fn cargo_test_filter(args: &[String]) -> Option<String> {
    cargo_test_filter_index(args).map(|index| args[index].clone())
}

fn cargo_test_filter_index(args: &[String]) -> Option<usize> {
    let mut index = 0;
    while let Some(argument) = args.get(index) {
        if !argument.starts_with('-') {
            return Some(index);
        }
        let takes_value = matches!(
            argument.as_str(),
            "-p" | "--package"
                | "--exclude"
                | "--bin"
                | "--example"
                | "--test"
                | "--bench"
                | "--features"
                | "--target"
                | "--target-dir"
                | "--manifest-path"
                | "--profile"
                | "-j"
                | "--jobs"
                | "--config"
                | "--message-format"
                | "--timings"
        );
        index += 1 + usize::from(takes_value);
    }
    None
}

fn cargo_test_ids(stdout: &str, stderr: &str) -> Vec<String> {
    let mut ids = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| line.trim().strip_prefix("test "))
        .filter_map(|line| line.split_once(" ... "))
        .filter_map(|(id, outcome)| matches!(outcome, "ok" | "FAILED" | "ignored").then_some(id))
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn cargo_test_list_ids(stdout: &str, stderr: &str) -> Vec<String> {
    let mut ids = stdout
        .lines()
        .chain(stderr.lines())
        .filter_map(|line| line.trim().strip_suffix(": test"))
        .map(str::to_string)
        .collect::<Vec<_>>();
    ids.sort();
    ids.dedup();
    ids
}

fn cargo_test_result(
    command: &str,
    runner_exit_code: i32,
    stdout: &str,
    stderr: &str,
) -> Option<AgentTaskGateTestResult> {
    if !is_cargo_test_declaration(command) {
        return None;
    }

    let mut passed = 0;
    let mut failed = 0;
    let mut filtered = 0;
    let mut found_summary = false;
    for line in stdout.lines().chain(stderr.lines()) {
        let Some((line_passed, line_failed, line_filtered)) = cargo_test_summary(line) else {
            continue;
        };
        found_summary = true;
        passed += line_passed;
        failed += line_failed;
        filtered += line_filtered;
    }
    found_summary.then_some(AgentTaskGateTestResult {
        runner: "cargo".to_string(),
        total: passed + failed,
        passed,
        failed,
        filtered,
        runner_exit_code,
    })
}

fn is_cargo_test_declaration(command: &str) -> bool {
    let tokens = crate::agent_task::tokenize_command(command);
    cargo_test_index(&tokens).is_some()
}

fn cargo_test_index(tokens: &[String]) -> Option<usize> {
    let mut index = 0;
    while tokens
        .get(index)
        .is_some_and(|token| is_environment_assignment(token))
    {
        index += 1;
    }
    let Some(first_command) = tokens.get(index) else {
        return None;
    };
    let cargo_index = if first_command == "cargo" {
        index
    } else if first_command == "timeout" {
        let Some(cargo_index) = timeout_cargo_index(&tokens, index + 1) else {
            return None;
        };
        cargo_index
    } else {
        return None;
    };
    is_cargo_test_subcommand(&tokens[cargo_index + 1..]).then_some(cargo_index)
}

fn timeout_cargo_index(tokens: &[String], mut index: usize) -> Option<usize> {
    while tokens
        .get(index)
        .is_some_and(|token| token.starts_with('-'))
    {
        let takes_value = matches!(
            tokens[index].as_str(),
            "-k" | "--kill-after" | "-s" | "--signal"
        );
        index += 1 + usize::from(takes_value);
    }
    // `timeout` requires a duration immediately before the wrapped program.
    index += 1;
    tokens
        .get(index)
        .is_some_and(|token| token == "cargo")
        .then_some(index)
}

fn is_environment_assignment(token: &str) -> bool {
    let Some((name, _)) = token.split_once('=') else {
        return false;
    };
    !name.is_empty()
        && name
            .chars()
            .all(|character| character == '_' || character.is_ascii_alphanumeric())
}

fn is_cargo_test_subcommand(tokens: &[String]) -> bool {
    let mut index = 0;
    while let Some(token) = tokens.get(index) {
        if token == "test" {
            return true;
        }
        if token.starts_with('+') || token.starts_with('-') {
            let takes_value = matches!(token.as_str(), "--color" | "-C" | "--config" | "-Z");
            index += 1 + usize::from(takes_value);
            continue;
        }
        return false;
    }
    false
}

fn cargo_test_summary(line: &str) -> Option<(u64, u64, u64)> {
    let line = line.trim();
    let rest = line.strip_prefix("test result: ok. ")?;
    let (passed, rest) = cargo_summary_count(rest, " passed; ")?;
    let (failed, rest) = cargo_summary_count(rest, " failed; ")?;
    let (_, rest) = cargo_summary_count(rest, " ignored; ")?;
    let (_, rest) = cargo_summary_count(rest, " measured; ")?;
    let (filtered, duration) = cargo_summary_count(rest, " filtered out; finished in ")?;
    let duration = duration.strip_suffix('s')?;
    (!duration.is_empty()
        && duration
            .chars()
            .all(|character| character.is_ascii_digit() || character == '.'))
    .then_some((passed, failed, filtered))
}

fn cargo_summary_count<'a>(text: &'a str, suffix: &str) -> Option<(u64, &'a str)> {
    let (count, rest) = text.split_once(suffix)?;
    Some((count.parse().ok()?, rest))
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
        "test_result": report.test_result,
        "cargo_selection": report.cargo_selection,
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

    #[test]
    fn cargo_test_results_preserve_selected_counts_without_rejecting_broad_gates() {
        let summary = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 1675 filtered out; finished in 0.00s\n";
        let zero = cargo_test_result(
            "RUSTFLAGS=-Dwarnings timeout 30 cargo --quiet test selected_test -- --exact",
            0,
            summary,
            "",
        )
        .expect("Cargo summary is parsed");
        assert_eq!(zero.total, 0);
        assert_eq!(zero.filtered, 1675);
        assert_eq!(effective_gate_exit_code(0, None), 0);

        let empty = cargo_test_result(
            "cargo test --locked -p empty-crate",
            0,
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out; finished in 0.00s\n",
            "",
        )
        .expect("empty Cargo summary is parsed");
        assert_eq!(empty.total, 0);
        assert_eq!(empty.filtered, 0);
        assert_eq!(effective_gate_exit_code(0, None), 0);

        let selected = cargo_test_result(
            "cargo test --locked selected_test",
            0,
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1674 filtered out; finished in 0.00s\n",
            "",
        )
        .expect("Cargo summary is parsed");
        assert_eq!(selected.total, 1);
        assert_eq!(selected.passed, 1);
        assert_eq!(effective_gate_exit_code(0, None), 0);

        assert!(cargo_test_result("echo cargo test", 0, summary, "").is_none());
        assert!(cargo_test_result("timeout 30 echo cargo test", 0, summary, "").is_none());
        assert!(cargo_test_result(
            "cargo test",
            0,
            "test result: ok. 0 passed; 0 failed; arbitrary output",
            "",
        )
        .is_none());
    }

    #[test]
    fn cargo_selection_requires_one_exact_id_and_keeps_broad_gates_explicit() {
        let focused = cargo_selection(
            "cargo test selected_test -- --exact",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test selected_test -- --exact".to_string(),
            ],
            "selected_test: test\nselected_test_unrelated: test\ntest selected_test ... ok\n",
            "",
            None,
        )
        .expect("Cargo selection");
        assert_eq!(focused.mode, "focused");
        assert_eq!(focused.filter_interpretation, "exact");
        assert_eq!(
            focused.discovered_ids,
            vec!["selected_test", "selected_test_unrelated"]
        );
        assert_eq!(focused.selected_ids, vec!["selected_test"]);
        assert_eq!(focused.selected_count, 1);
        assert_eq!(effective_gate_exit_code(0, Some(&focused)), 0);

        let broad = cargo_selection(
            "cargo test --locked",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test --locked".to_string(),
            ],
            "selected_test: test\nselected_test_unrelated: test\ntest selected_test ... ok\ntest selected_test_unrelated ... ok\n",
            "",
            None,
        )
        .expect("Cargo selection");
        assert_eq!(broad.mode, "broad");
        assert_eq!(broad.filter_interpretation, "broad_explicit");
        assert_eq!(broad.selected_count, 2);
        assert_eq!(effective_gate_exit_code(0, Some(&broad)), 0);

        let ambiguous = cargo_selection(
            "cargo test selected_test",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test selected_test".to_string(),
            ],
            "selected_test: test\nselected_test_unrelated: test\ntest selected_test ... ok\ntest selected_test_unrelated ... ok\n",
            "",
            None,
        )
        .expect("Cargo selection");
        assert_eq!(ambiguous.filter_interpretation, "substring_ambiguous");
        assert_eq!(
            ambiguous.selected_ids,
            vec!["selected_test", "selected_test_unrelated"]
        );
        assert_eq!(ambiguous.selected_count, 2);
        assert_eq!(effective_gate_exit_code(0, Some(&ambiguous)), 1);
        let multiple_recovery = ambiguous.recovery.expect("discovery recovery metadata");
        assert_eq!(multiple_recovery.action, "discover_and_choose");
        assert_eq!(multiple_recovery.command, "cargo test -- --list");
        assert_eq!(
            multiple_recovery.candidate_ids,
            vec!["selected_test", "selected_test_unrelated"]
        );

        let non_exact_single = cargo_selection(
            "cargo test selected_test",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test selected_test".to_string(),
            ],
            "test selected_test ... ok\n",
            "",
            None,
        )
        .expect("Cargo selection");
        assert_eq!(non_exact_single.selected_count, 1);
        assert_eq!(effective_gate_exit_code(0, Some(&non_exact_single)), 1);
        let serialized_recovery = serde_json::to_value(&non_exact_single)
            .expect("reviewer-facing recovery metadata serializes");
        assert_eq!(
            serialized_recovery["recovery"]["command"],
            "cargo test selected_test -- --exact"
        );
        let one_recovery = non_exact_single.recovery.expect("exact recovery metadata");
        assert_eq!(one_recovery.action, "rerun_exact");
        assert_eq!(
            one_recovery.argv,
            vec!["cargo", "test", "selected_test", "--", "--exact"]
        );
        assert_eq!(one_recovery.command, "cargo test selected_test -- --exact");

        let prefixed = cargo_selection(
            "RUSTFLAGS=\"-D warnings\" timeout --signal TERM 30 cargo test --locked -p example --test integration --features feature-a selected --lib -- --ignored --nocapture",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "RUSTFLAGS=\"-D warnings\" timeout --signal TERM 30 cargo test --locked -p example --test integration --features feature-a selected --lib -- --ignored --nocapture".to_string(),
            ],
            "test crate::selected ... ok\n",
            "",
            None,
        )
        .expect("prefixed Cargo selection");
        let prefixed_recovery = prefixed.recovery.expect("prefix-preserving exact recovery");
        assert_eq!(prefixed_recovery.action, "rerun_exact");
        assert_eq!(
            prefixed_recovery.argv,
            vec![
                "RUSTFLAGS=-D warnings",
                "timeout",
                "--signal",
                "TERM",
                "30",
                "cargo",
                "test",
                "--locked",
                "-p",
                "example",
                "--test",
                "integration",
                "--features",
                "feature-a",
                "crate::selected",
                "--lib",
                "--",
                "--ignored",
                "--nocapture",
                "--exact",
            ]
        );
        assert_eq!(
            prefixed_recovery.command,
            "RUSTFLAGS='-D warnings' timeout --signal TERM 30 cargo test --locked -p example --test integration --features feature-a crate::selected --lib -- --ignored --nocapture --exact"
        );

        let zero_match = cargo_selection(
            "cargo test --test integration missing_test --lib -- --ignored --exact",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test --test integration missing_test --lib -- --ignored --exact".to_string(),
            ],
            "selected_test: test\nselected_test_unrelated: test\n",
            "",
            None,
        )
        .expect("Cargo selection");
        assert_eq!(zero_match.filter_interpretation, "exact");
        assert!(zero_match.selected_ids.is_empty());
        assert_eq!(zero_match.selected_count, 0);
        assert_eq!(effective_gate_exit_code(0, Some(&zero_match)), 1);
        let zero_recovery = zero_match.recovery.expect("discovery recovery metadata");
        assert_eq!(zero_recovery.action, "discover_and_choose");
        assert_eq!(
            zero_recovery.argv,
            vec![
                "cargo",
                "test",
                "--test",
                "integration",
                "--lib",
                "--",
                "--ignored",
                "--list",
            ]
        );
        assert_eq!(
            zero_recovery.candidate_ids,
            vec!["selected_test", "selected_test_unrelated"]
        );

        let redacted = cargo_selection_recovery(
            &[
                "cargo".to_string(),
                "test".to_string(),
                "unsafe_filter".to_string(),
            ],
            0,
            &["token=secret".to_string()],
            1,
            false,
            false,
        )
        .expect("redacted exact recovery");
        assert!(redacted.command.contains("[REDACTED]"));
        assert!(!redacted.command.contains("secret"));

        let quoted = cargo_selection_recovery(
            &[
                "cargo".to_string(),
                "test".to_string(),
                "unsafe_filter".to_string(),
            ],
            0,
            &["module::id; touch /tmp/pwn".to_string()],
            1,
            false,
            false,
        )
        .expect("shell-quoted exact recovery");
        assert!(quoted.command.contains("'module::id; touch /tmp/pwn'"));

        let shell_control = cargo_selection(
            "cargo test selected_test && printf unexpected",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test selected_test && printf unexpected".to_string(),
            ],
            "test selected_test ... ok\n",
            "",
            None,
        )
        .expect("shell-controlled Cargo selection");
        assert_eq!(
            shell_control
                .recovery
                .expect("discovery recovery for shell control")
                .action,
            "discover_and_choose"
        );

        let package_only = cargo_selection(
            "cargo test --locked -p empty-crate",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo test --locked -p empty-crate".to_string(),
            ],
            "",
            "",
            None,
        )
        .expect("Cargo selection");
        assert_eq!(package_only.mode, "broad");
        assert_eq!(effective_gate_exit_code(0, Some(&package_only)), 0);

        let quiet_exact = cargo_selection(
            "cargo --quiet test selected_test -- --exact",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo --quiet test selected_test -- --exact".to_string(),
            ],
            "test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 1 filtered out; finished in 0.00s\n",
            "",
            Some(&AgentTaskGateTestResult {
                runner: "cargo".to_string(),
                total: 1,
                passed: 1,
                failed: 0,
                filtered: 1,
                runner_exit_code: 0,
            }),
        )
        .expect("quiet exact Cargo selection");
        assert_eq!(quiet_exact.selected_ids, vec!["selected_test"]);
        assert_eq!(quiet_exact.selected_count, 1);
        assert_eq!(effective_gate_exit_code(0, Some(&quiet_exact)), 0);

        let quiet_zero_match = cargo_selection(
            "cargo --quiet test missing_test -- --exact",
            &[
                "sh".to_string(),
                "-lc".to_string(),
                "cargo --quiet test missing_test -- --exact".to_string(),
            ],
            "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 2 filtered out; finished in 0.00s\n",
            "",
            Some(&AgentTaskGateTestResult {
                runner: "cargo".to_string(),
                total: 0,
                passed: 0,
                failed: 0,
                filtered: 2,
                runner_exit_code: 0,
            }),
        )
        .expect("quiet zero-match Cargo selection");
        assert!(quiet_zero_match.selected_ids.is_empty());
        assert_eq!(quiet_zero_match.selected_count, 0);
        assert_eq!(effective_gate_exit_code(0, Some(&quiet_zero_match)), 1);
    }

    #[cfg(unix)]
    #[test]
    fn cargo_gate_rejects_zero_match_and_accepts_one_match_through_supported_declarations() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = env_mutex();
        let temp = tempfile::tempdir().expect("temporary Cargo fixture");
        std::fs::create_dir(temp.path().join("src")).expect("fixture source directory");
        std::fs::write(
            temp.path().join("Cargo.toml"),
            "[package]\nname = \"gate-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("fixture manifest");
        std::fs::write(
            temp.path().join("src/lib.rs"),
            "#[cfg(test)]\nmod tests { #[test] fn selected_test() {} }\n",
        )
        .expect("fixture test");
        let bin = temp.path().join("bin");
        std::fs::create_dir(&bin).expect("wrapper directory");
        let timeout = bin.join("timeout");
        std::fs::write(
            &timeout,
            "#!/bin/sh\nwhile [ \"$1\" = -s ] || [ \"$1\" = --signal ]; do shift; shift; done\nshift\nshift\nHOME=\"$HOMEBOY_ORIGINAL_HOME\"\nPATH=\"$HOMEBOY_ORIGINAL_PATH\"\nexport HOME PATH\nexec cargo \"$@\"\n",
        )
        .expect("timeout wrapper");
        let mut permissions = std::fs::metadata(&timeout)
            .expect("timeout wrapper metadata")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&timeout, permissions).expect("make timeout wrapper executable");
        let target = temp.path().join("target");
        let command = |timeout_options: &str, filter: &str| {
            format!(
                "RUSTFLAGS=\"-D warnings\" HOMEBOY_ORIGINAL_HOME='{}' HOMEBOY_ORIGINAL_PATH='{}' CARGO_TARGET_DIR='{}' PATH='{}' timeout {timeout_options} 30 cargo --quiet test {filter} -- --exact",
                std::env::var("HOME").expect("host HOME"),
                std::env::var("PATH").expect("host PATH"),
                target.display(),
                bin.display(),
            )
        };

        let zero = run_gate_command(temp.path(), 1, &command("-s TERM", "tests::missing_test"))
            .expect("zero-match Cargo gate report");
        assert_eq!(zero.status, AgentTaskGateStatus::Failed);
        assert_eq!(
            zero.failure_evidence
                .as_ref()
                .expect("zero-match failure evidence")
                .classification,
            AgentTaskGateFailureClassification::ZeroTestsSelected
        );
        let zero_counts = zero.test_result.as_ref().expect("zero-match test counts");
        assert_eq!(zero_counts.total, 0);
        assert_eq!(zero_counts.passed, 0);
        assert_eq!(zero_counts.failed, 0);
        assert!(
            zero_counts.filtered > 0,
            "zero match retained filter evidence"
        );

        let selected = run_gate_command(
            temp.path(),
            2,
            &command("--signal TERM", "tests::selected_test"),
        )
        .expect("one-match Cargo gate report");
        assert_eq!(selected.status, AgentTaskGateStatus::Succeeded);
        let selected_counts = selected
            .test_result
            .as_ref()
            .expect("one-match test counts");
        assert_eq!(selected_counts.total, 1);
        assert_eq!(selected_counts.passed, 1);
        assert_eq!(selected_counts.failed, 0);
    }

    #[test]
    fn declared_shared_cargo_target_is_reported_without_parsing_gate_shell_text() {
        let temp = tempfile::tempdir().expect("gate fixture");
        let mut policy = AgentTaskGateEnvironmentPolicy::default();
        policy.shared_cargo_target = Some(true);
        // The test runner's required Cargo target is an ambient override. An
        // explicit empty declaration clears it so this exercises the contract.
        policy
            .variables
            .insert("CARGO_TARGET_DIR".to_string(), String::new());

        let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            temp.path(),
            1,
            "test -n \"$CARGO_TARGET_DIR\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
            &[],
        )
        .expect("declared managed gate");

        assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
        let target = report.environment.cargo_target.expect("target evidence");
        assert_eq!(target.resolution, "shared");
        assert!(target.path.contains("cargo-target"));
        assert!(target.owner.starts_with("agent-task-gate"));
    }

    #[test]
    fn gate_inherits_shared_cargo_target_from_component_metadata() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("gate fixture");
            std::fs::write(
                temp.path().join("homeboy.json"),
                r#"{"id":"cargo-fixture","managed_execution":{"shared_cargo_target":true}}"#,
            )
            .expect("component manifest");
            let mut policy = AgentTaskGateEnvironmentPolicy::default();
            policy
                .variables
                .insert("CARGO_TARGET_DIR".to_string(), String::new());

            let report = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
                temp.path(),
                1,
                "test -n \"$CARGO_TARGET_DIR\"",
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                None,
                &policy,
                &[],
            )
            .expect("metadata-managed gate");

            assert_eq!(report.status, AgentTaskGateStatus::Succeeded);
            assert_eq!(
                report
                    .environment
                    .cargo_target
                    .expect("target evidence")
                    .resolution,
                "shared"
            );
        });
    }

    #[test]
    fn shared_cargo_target_reports_miss_then_hit_and_metrics() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("gate fixture");
            let mut policy = AgentTaskGateEnvironmentPolicy::default();
            policy.shared_cargo_target = Some(true);
            policy
                .variables
                .insert("CARGO_TARGET_DIR".to_string(), String::new());

            let first = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
                temp.path(),
                1,
                "printf artifact > \"$CARGO_TARGET_DIR/artifact\"",
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                None,
                &policy,
                &[],
            )
            .expect("first managed gate");
            let first = first
                .environment
                .cargo_target
                .expect("first target evidence");
            assert_eq!(first.state.as_deref(), Some("miss"));
            assert_eq!(first.bytes_before, Some(0));
            assert!(first.bytes_after.expect("target bytes") > 0);
            assert!(first.elapsed_ms.is_some());

            let second = run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
                temp.path(),
                2,
                "printf artifact > \"$CARGO_TARGET_DIR/artifact\"",
                AgentTaskGateVisibility::Visible,
                AgentTaskGateRevealPolicy::FullEvidence,
                None,
                &policy,
                &[],
            )
            .expect("reused managed gate");
            let second = second
                .environment
                .cargo_target
                .expect("second target evidence");
            assert_eq!(second.state.as_deref(), Some("hit"));
            assert_eq!(first.identity, second.identity);
            assert_eq!(first.path, second.path);
        });
    }

    #[test]
    fn concurrent_shared_cargo_target_gates_reuse_one_live_store() {
        homeboy_core::test_support::with_isolated_home(|_| {
            let temp = tempfile::tempdir().expect("gate fixture");
            let barrier = std::sync::Barrier::new(2);
            let reports = std::thread::scope(|scope| {
                let first = scope.spawn(|| {
                    barrier.wait();
                    run_shared_target_fixture_gate(temp.path(), 1)
                });
                let second = scope.spawn(|| {
                    barrier.wait();
                    run_shared_target_fixture_gate(temp.path(), 2)
                });
                [
                    first.join().expect("first gate thread"),
                    second.join().expect("second gate thread"),
                ]
            });

            let first = reports[0]
                .as_ref()
                .expect("first managed gate")
                .environment
                .cargo_target
                .as_ref()
                .expect("first target evidence");
            let second = reports[1]
                .as_ref()
                .expect("second managed gate")
                .environment
                .cargo_target
                .as_ref()
                .expect("second target evidence");
            assert_eq!(first.path, second.path);
            assert_eq!(first.identity, second.identity);
            assert!(std::path::Path::new(&first.path).is_dir());
        });
    }

    fn run_shared_target_fixture_gate(cwd: &Path, index: usize) -> Result<AgentTaskGateReport> {
        let mut policy = AgentTaskGateEnvironmentPolicy::default();
        policy.shared_cargo_target = Some(true);
        policy
            .variables
            .insert("CARGO_TARGET_DIR".to_string(), String::new());
        run_gate_command_with_policy_and_runtime_tmpdir_and_environment(
            cwd,
            index,
            "sleep 0.1; printf artifact > \"$CARGO_TARGET_DIR/artifact-$$\"",
            AgentTaskGateVisibility::Visible,
            AgentTaskGateRevealPolicy::FullEvidence,
            None,
            &policy,
            &[],
        )
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
            hydrate_rust_cache: true,
            shared_cargo_target: Some(false),
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
    fn rust_gate_cache_hydrates_once_coordinates_waiters_and_separates_identities() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = env_mutex();
        let root = tempfile::tempdir().expect("controller data");
        let workspace = tempfile::tempdir().expect("workspace");
        let runtime = tempfile::tempdir().expect("runtime");
        let bin = tempfile::tempdir().expect("tool bin");
        fs::write(workspace.path().join("Cargo.lock"), "version = 4\n").expect("lockfile");
        fs::write(
            workspace.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.95.0\"\n",
        )
        .expect("toolchain");
        let direct_cargo = bin.path().join("direct-cargo");
        fs::write(
            &direct_cargo,
            "#!/bin/sh\nprintf '%s\\n' cargo >> \"$CARGO_HOME/registry/hydration.log\"\n",
        )
        .expect("direct cargo fixture");
        let rustup = bin.path().join("rustup");
        fs::write(
            &rustup,
            format!(
                "#!/bin/sh\ntest \"$0\" = \"$CARGO_HOME/bin/rustup\"\nif test \"$1\" = toolchain; then\n  /bin/mkdir -p \"$CARGO_HOME/registry\" \"$RUSTUP_HOME/toolchains/1.95.0-test/bin\"\n  /bin/cp \"{}\" \"$RUSTUP_HOME/toolchains/1.95.0-test/bin/cargo\"\n  printf '%s\\n' rustup >> \"$CARGO_HOME/registry/hydration.log\"\nelif test \"$1\" = which; then\n  printf '%s\\n' \"$RUSTUP_HOME/toolchains/1.95.0-test/bin/cargo\"\nfi\n/bin/sleep 0.1\n",
                direct_cargo.display()
            ),
        )
        .expect("rustup fixture");
        let cargo = bin.path().join("cargo");
        fs::write(
            &cargo,
            "#!/bin/sh\necho 'host rustup proxy must not execute' >&2\nexit 70\n",
        )
        .expect("host cargo proxy fixture");
        for path in [&direct_cargo, &rustup, &cargo] {
            let mut permissions = fs::metadata(&path).expect("tool metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("tool executable");
        }
        let _environment = EnvVarGuard::set(&[
            (homeboy_core::paths::HOMEBOY_DATA_DIR_ENV, root.path()),
            ("PATH", bin.path()),
        ]);
        let policy = AgentTaskGateEnvironmentPolicy {
            isolate_home: true,
            isolate_xdg: true,
            ..AgentTaskGateEnvironmentPolicy::default()
        };
        let cache_identity = rust_cache_identity(workspace.path()).expect("cache identity");
        let hydration_log = root
            .path()
            .join("controller-state/gate-rust-cache")
            .join(&cache_identity)
            .join("cargo/registry/hydration.log");

        let states = std::thread::scope(|scope| {
            let first = scope.spawn(|| {
                let mut environment = selected_gate_environment(&policy, Some(runtime.path()))
                    .expect("first environment");
                environment
                    .configure_rust_cache(workspace.path())
                    .expect("first hydration");
                environment.report.rust_cache.expect("first evidence").state
            });
            let second = scope.spawn(|| {
                let mut environment = selected_gate_environment(&policy, Some(runtime.path()))
                    .expect("second environment");
                environment
                    .configure_rust_cache(workspace.path())
                    .expect("second hydration");
                environment
                    .report
                    .rust_cache
                    .expect("second evidence")
                    .state
            });
            [
                first.join().expect("first thread"),
                second.join().expect("second thread"),
            ]
        });
        assert!(states.contains(&"hydrated".to_string()), "{states:?}");
        assert!(
            states.iter().any(|state| state == "wait" || state == "hit"),
            "{states:?}"
        );
        assert_eq!(
            fs::read_to_string(&hydration_log)
                .expect("hydration log")
                .lines()
                .count(),
            2
        );

        let mut repeated =
            selected_gate_environment(&policy, Some(runtime.path())).expect("repeat environment");
        repeated
            .configure_rust_cache(workspace.path())
            .expect("cache hit");
        for name in ["CARGO_HOME", "RUSTUP_HOME"] {
            let value = repeated.values.get(name).expect("gate overlay path");
            assert!(value.starts_with(repeated.values.get("HOME").expect("isolated HOME")));
            assert!(!value.starts_with(root.path().to_string_lossy().as_ref()));
        }
        let cargo_on_path = std::env::split_paths(&std::ffi::OsString::from(
            repeated.values.get("PATH").expect("gate PATH"),
        ))
        .next()
        .expect("toolchain bin on PATH")
        .join("cargo");
        assert!(cargo_on_path.is_file());
        assert!(cargo_on_path.starts_with(
            repeated
                .values
                .get("RUSTUP_HOME")
                .expect("isolated RUSTUP_HOME")
        ));
        assert_eq!(
            repeated.report.rust_cache.expect("hit evidence").state,
            "hit"
        );
        assert_eq!(
            fs::read_to_string(&hydration_log)
                .expect("hydration log")
                .lines()
                .count(),
            2
        );

        let first_identity = rust_cache_identity(workspace.path()).expect("first identity");
        fs::write(workspace.path().join("Cargo.lock"), "version = 5\n")
            .expect("different lockfile");
        assert_ne!(
            first_identity,
            rust_cache_identity(workspace.path()).expect("second identity")
        );
        fs::write(
            workspace.path().join("rust-toolchain.toml"),
            "[toolchain]\nchannel = \"1.96.0\"\n",
        )
        .expect("different toolchain");
        assert_ne!(
            first_identity,
            rust_cache_identity(workspace.path()).expect("third identity")
        );
        fs::create_dir_all(workspace.path().join(".cargo")).expect("cargo config directory");
        fs::write(
            workspace.path().join(".cargo/config.toml"),
            "[net]\noffline = true\n",
        )
        .expect("cargo config");
        assert_ne!(
            first_identity,
            rust_cache_identity(workspace.path()).expect("config identity")
        );
    }

    #[cfg(unix)]
    #[test]
    fn rust_gate_cache_rejects_corrupt_markers_and_unsafe_roots() {
        use std::os::unix::fs::PermissionsExt;

        let marker = tempfile::NamedTempFile::new().expect("marker");
        fs::write(marker.path(), "not json").expect("corrupt marker");
        let cache = tempfile::tempdir().expect("cache");
        assert!(valid_marker_toolchain_cargo(marker.path(), "identity", cache.path()).is_err());

        let root = tempfile::tempdir().expect("cache root");
        let mut permissions = fs::metadata(root.path()).expect("metadata").permissions();
        permissions.set_mode(0o777);
        fs::set_permissions(root.path(), permissions).expect("unsafe permissions");
        assert!(ensure_safe_rust_cache_root(root.path()).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn rust_gate_cache_preserves_safe_symlinks_and_rejects_unsafe_content() {
        use std::os::unix::fs::{symlink, PermissionsExt};

        let source = tempfile::tempdir().expect("source");
        let overlay = tempfile::tempdir().expect("overlay");
        fs::create_dir_all(source.path().join("bin")).expect("cache bin");
        let rustup = source.path().join("bin/rustup");
        fs::write(&rustup, "#!/bin/sh\nprintf 'rustup\\n'\n").expect("rustup proxy");
        let mut permissions = fs::metadata(&rustup)
            .expect("rustup metadata")
            .permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(&rustup, permissions).expect("rustup executable");
        symlink("rustup", source.path().join("bin/cargo")).expect("normal cargo proxy");
        symlink(&rustup, source.path().join("absolute-rustup")).expect("absolute safe link");
        copy_tree_preserving_safe_symlinks(source.path(), overlay.path()).expect("copy cache");
        assert_eq!(
            fs::read_link(overlay.path().join("bin/cargo")).expect("relative cargo link"),
            PathBuf::from("rustup")
        );
        assert_eq!(
            fs::read_link(overlay.path().join("absolute-rustup")).expect("absolute rustup link"),
            overlay.path().join("bin/rustup")
        );
        assert_eq!(
            Command::new(overlay.path().join("bin/cargo"))
                .output()
                .expect("execute copied cargo proxy")
                .stdout,
            b"rustup\n"
        );

        let reused_overlay = tempfile::tempdir().expect("reused overlay");
        copy_tree_preserving_safe_symlinks(source.path(), reused_overlay.path())
            .expect("reuse cache");
        assert_eq!(
            fs::read_link(reused_overlay.path().join("bin/cargo")).expect("reused cargo link"),
            PathBuf::from("rustup")
        );

        for (name, target, expected) in [
            ("escape", PathBuf::from("/tmp"), "escapes its source root"),
            ("dangling", PathBuf::from("missing"), "dangling target"),
            ("cycle-a", PathBuf::from("cycle-b"), "cycle"),
        ] {
            let malicious = tempfile::tempdir().expect("malicious cache");
            symlink(&target, malicious.path().join(name)).expect("malicious symlink");
            if name == "cycle-a" {
                symlink("cycle-a", malicious.path().join("cycle-b")).expect("cycle link");
            }
            let error = copy_tree_preserving_safe_symlinks(
                malicious.path(),
                tempfile::tempdir().expect("malicious overlay").path(),
            )
            .expect_err("reject malicious cache link");
            let diagnostic = error.details["error"].as_str().unwrap_or_default();
            assert!(diagnostic.contains(name), "{diagnostic}");
            assert!(diagnostic.contains(expected), "{diagnostic}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn rust_gate_cache_bounds_stalled_hydration() {
        use std::os::unix::fs::PermissionsExt;

        let _guard = env_mutex();
        let bin = tempfile::tempdir().expect("tool bin");
        let workspace = tempfile::tempdir().expect("workspace");
        let cache = tempfile::tempdir().expect("cache");
        for tool in ["rustup", "cargo"] {
            let path = bin.path().join(tool);
            fs::write(&path, "#!/bin/sh\n/bin/sleep 1\n").expect("tool fixture");
            let mut permissions = fs::metadata(&path).expect("tool metadata").permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(path, permissions).expect("tool executable");
        }
        let _environment = EnvVarGuard::set(&[("PATH", bin.path())]);
        let started = Instant::now();
        assert!(hydrate_rust_cache_with_timeout(
            workspace.path(),
            cache.path(),
            Duration::from_millis(50),
        )
        .is_err());
        assert!(started.elapsed() < Duration::from_secs(1));
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
        assert!(
            !homeboy_engine_primitives::command::process_is_running(descendant_pid as u32),
            "toolchain preflight left descendant {descendant_pid} runnable"
        );
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
