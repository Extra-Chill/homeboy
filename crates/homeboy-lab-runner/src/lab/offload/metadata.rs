//! Lab runner / source-checkout / materialization-proof metadata builders,
//! rig component path overrides, and the stale-runner-homeboy error.

use super::*;
use homeboy_core::runner_execution_envelope::PathMaterializationPlan;
#[cfg(test)]
use homeboy_core::secret_env_plan::SECRET_ENV_PLAN_ENV_DELTA_SOURCE;
use homeboy_lab_contract::{
    negotiate_lab_capability_handshake, required_lab_handoff_capabilities, LabCapabilityAdmission,
    LabCapabilityHandshake, LabRuntimeAncestry, LabRuntimeIdentity,
};
use std::path::{Path, PathBuf};

const ENV_RESOLUTION_SCHEMA: &str = "homeboy/env-resolution/v1";
const REDACTED_ENV_VALUE: &str = "<redacted>";

/// Capture one deterministic direct-runner admission. The runner command and
/// daemon each advertise capabilities; missing evidence deliberately refuses
/// the handoff rather than inferring compatibility from a release version.
pub(crate) fn direct_runner_capability_admission(
    runner: &Runner,
    status: &RunnerStatusReport,
    homeboy: &str,
) -> Result<LabCapabilityAdmission> {
    Ok(negotiate_lab_capability_handshake(
        &direct_runner_capability_handshake(runner, status, homeboy)?,
    ))
}

pub(crate) fn direct_runner_capability_handshake(
    runner: &Runner,
    status: &RunnerStatusReport,
    homeboy: &str,
) -> Result<LabCapabilityHandshake> {
    let controller = homeboy_product_identity::build_identity();
    let controller_identity = LabRuntimeIdentity {
        build_identity: controller.display,
        lab_contract_version: Some(homeboy_lab_contract::LAB_CONTRACT_VERSION),
        source_revision: controller.git_commit.unwrap_or_default(),
        clean: controller.git_dirty == Some(false),
    };
    let command_evidence = crate::configured_runner_homeboy_handshake_evidence(runner, homeboy)?;
    let Some(session) = status.session.as_ref() else {
        let (runner_command, runner_command_capabilities) =
            command_evidence.unwrap_or_else(|| (controller_identity.clone(), Vec::new()));
        return Ok(LabCapabilityHandshake {
            controller: controller_identity.clone(),
            required_capabilities: required_lab_handoff_capabilities(),
            runner_command,
            runner_command_capabilities,
            daemon: controller_identity,
            daemon_capabilities: Vec::new(),
            ancestry: LabRuntimeAncestry::Unknown,
        });
    };
    let daemon_source = session
        .homeboy_build_identity
        .as_deref()
        .and_then(build_commit)
        .unwrap_or_default()
        .to_string();
    let local_url = session.local_url.as_deref().ok_or_else(|| {
        crate::connection::admission_probe_unavailable("daemon_version", "session has no local URL")
    })?;
    let (daemon_capabilities, daemon_contract_version) =
        crate::daemon_lab_handoff_capabilities(local_url).map_err(|error| {
            crate::connection::admission_probe_unavailable("daemon_version", error)
        })?;
    let daemon = LabRuntimeIdentity {
        build_identity: session.homeboy_build_identity.clone().unwrap_or_default(),
        lab_contract_version: daemon_contract_version,
        source_revision: daemon_source,
        clean: !session
            .homeboy_build_identity
            .as_deref()
            .is_some_and(|value| value.ends_with("-dirty")),
    };
    let Some((runner_command, runner_command_capabilities)) = command_evidence else {
        return Ok(LabCapabilityHandshake {
            controller: controller_identity.clone(),
            required_capabilities: required_lab_handoff_capabilities(),
            runner_command: controller_identity.clone(),
            runner_command_capabilities: Vec::new(),
            daemon: controller_identity,
            daemon_capabilities,
            ancestry: LabRuntimeAncestry::Unknown,
        });
    };
    let ancestry = runtime_ancestry(
        &controller_identity.source_revision,
        &runner_command.source_revision,
    );
    Ok(LabCapabilityHandshake {
        controller: controller_identity,
        required_capabilities: required_lab_handoff_capabilities(),
        runner_command,
        runner_command_capabilities,
        daemon,
        daemon_capabilities,
        ancestry,
    })
}

pub(crate) fn capability_admission_has_blocking_drift(
    admission: &LabCapabilityAdmission,
    require_exact_runner_version: bool,
) -> bool {
    !admission.compatible
        || (require_exact_runner_version
            && admission.provenance.controller_requirement.build_identity
                != admission.provenance.executed_runner_command.build_identity)
}

/// Reject a direct Lab handoff for the capability evidence actually observed.
/// This is intentionally distinct from stale_runner_homeboy_error: matching
/// daemon and command identities do not prove the required capabilities exist.
pub(crate) fn capability_admission_error(
    runner_id: &str,
    admission: &LabCapabilityAdmission,
    require_exact_runner_version: bool,
) -> Error {
    debug_assert!(capability_admission_has_blocking_drift(
        admission,
        require_exact_runner_version
    ));
    let ancestry = admission.provenance.ancestry;
    let (cause_class, cause, remediation) = if !admission.compatible {
        (
            if admission.rejection_code
                == Some(homeboy_lab_contract::LabAdmissionRejection::LabContractVersionMismatch)
            {
                "lab_contract_version_mismatch"
            } else {
                "capability_incompatible"
            },
            admission
                .provenance
                .rejection_reason
                .as_deref()
                .unwrap_or("the runner did not provide a capability rejection reason"),
            if admission.rejection_code
                == Some(homeboy_lab_contract::LabAdmissionRejection::LabContractVersionMismatch)
            {
                "Update the runner with `homeboy runner connect <runner-id>`, which installs this controller's release on it, then retry."
            } else {
                "Upgrade or reconnect the runner so its command and daemon advertise the required Lab handoff capabilities, then retry."
            },
        )
    } else {
        (
            "exact_build_mismatch",
            "require_exact_homeboy_version requires the controller's exact build",
            "Update the runner with `homeboy runner connect <runner-id>`, which installs this controller's release on it, then retry. For an unreleased controller build, use `homeboy runner dev-sync`.",
        )
    };
    let message = if !admission.compatible {
        format!(
            "Lab offload refused runner `{runner_id}` because its capability admission is incompatible: {cause}. {remediation}"
        )
    } else {
        format!(
            "Lab offload refused runner `{runner_id}` because {cause}, but admission reported {ancestry:?}. {remediation}"
        )
    };

    Error::new(
        ErrorCode::RunnerCapabilityMissing,
        message,
        serde_json::json!({
            "runner_id": runner_id,
            "admission_cause_class": cause_class,
            "rejection_reason": admission.provenance.rejection_reason,
            "ancestry": ancestry,
            "require_exact_runner_version": require_exact_runner_version,
            "capability_admission": admission,
            "remediation": remediation,
        }),
    )
    .with_hint(remediation)
}

fn build_commit(identity: &str) -> Option<&str> {
    identity
        .split_once('+')
        .map(|(_, commit)| commit.trim_end_matches("-dirty"))
        .filter(|commit| !commit.is_empty())
}

fn runtime_ancestry(controller: &str, runner: &str) -> LabRuntimeAncestry {
    if controller.is_empty() || runner.is_empty() {
        return LabRuntimeAncestry::Unknown;
    }
    if controller == runner {
        return LabRuntimeAncestry::ExactSource;
    }
    LabRuntimeAncestry::Unknown
}

/// Insert generic `${components.<id>.path}` override env vars so a remote rig
/// check resolves component paths to the runner-side materialized checkout
/// instead of the controller path the rig spec declares (issue #3766/#3767).
pub(crate) fn apply_rig_component_path_overrides(
    env: &mut std::collections::HashMap<String, String>,
    overrides: &[(String, String)],
) {
    for (name, value) in overrides {
        if !value.trim().is_empty() {
            env.insert(name.clone(), value.clone());
        }
    }
}

/// Build diagnostics describing each rig component path override forwarded to
/// the runner, so bench artifacts show how `${components.<id>.path}` resolved.
pub(crate) fn rig_component_path_overrides_metadata(
    overrides: &[(String, String)],
) -> serde_json::Value {
    let forwarded = overrides
        .iter()
        .map(|(name, runner_path)| {
            serde_json::json!({
                "env_name": name,
                "runner_path": runner_path,
                "forwarded_to_runner": true,
            })
        })
        .collect::<Vec<_>>();
    serde_json::json!({
        "schema": "homeboy/lab-offload-rig-component-path-override/v1",
        "overrides": forwarded,
    })
}

pub(crate) fn job_scoped_overrides_metadata(overrides: &LabJobOverrides) -> serde_json::Value {
    let policy = RedactionPolicy::default();
    let mut names = overrides.env.keys().cloned().collect::<Vec<_>>();
    names.sort();
    let secret_env_names = overrides
        .secret_env_names
        .iter()
        .map(String::as_str)
        .collect::<std::collections::HashSet<_>>();
    let env = names
        .into_iter()
        .map(|name| {
            let value = overrides.env.get(&name).map(String::as_str).unwrap_or("");
            let redacted = secret_env_names.contains(name.as_str())
                || policy.is_sensitive_key(&name)
                || policy.redact_string(value) != value;
            serde_json::json!({
                "name": name,
                "source": "job_override",
                "forwarded_to_runner": true,
                "value_preview": if redacted { "<redacted>".to_string() } else { value.to_string() },
                "redacted": redacted,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "schema": "homeboy/lab-job-scoped-overrides/v1",
        "env": env,
        "workspace_root": overrides.workspace_root.as_ref().map(|path| serde_json::json!({
            "source": "job_override",
            "value": path,
        })),
    })
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub(crate) struct LabEnvResolutionLayer {
    pub(crate) source: &'static str,
    pub(crate) env: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub(crate) secret_names: Vec<String>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct LabEnvResolutionReport {
    schema: &'static str,
    values_redacted: bool,
    keys: Vec<LabEnvResolutionEntry>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct LabEnvResolutionEntry {
    key: String,
    classification: &'static str,
    value_status: &'static str,
    value_preview: &'static str,
    winning_source_layer: String,
    shadowed_source_layers: Vec<String>,
    source_layers: Vec<LabEnvResolutionSource>,
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
struct LabEnvResolutionSource {
    source: String,
    status: &'static str,
    classification: &'static str,
    value_status: &'static str,
}

pub(crate) fn lab_env_resolution_report(layers: Vec<LabEnvResolutionLayer>) -> serde_json::Value {
    let policy = RedactionPolicy::default();
    let mut entries_by_key: std::collections::BTreeMap<String, Vec<LabEnvResolutionSource>> =
        std::collections::BTreeMap::new();

    for layer in layers {
        let explicit_secret_names = layer
            .secret_names
            .iter()
            .map(String::as_str)
            .collect::<std::collections::HashSet<_>>();
        let mut names = layer.env.keys().cloned().collect::<Vec<_>>();
        names.sort();
        for name in names {
            let Some(value) = layer.env.get(&name) else {
                continue;
            };
            let secret = explicit_secret_names.contains(name.as_str())
                || policy.is_sensitive_key(&name)
                || policy.redact_string(value) != *value;
            entries_by_key
                .entry(name)
                .or_default()
                .push(LabEnvResolutionSource {
                    source: layer.source.to_string(),
                    status: "shadowed",
                    classification: if secret { "secret" } else { "public" },
                    value_status: if secret {
                        "secret_redacted"
                    } else {
                        "redacted"
                    },
                });
        }
    }

    let keys = entries_by_key
        .into_iter()
        .filter_map(|(key, mut source_layers)| {
            let winning_index = source_layers.len().checked_sub(1)?;
            source_layers[winning_index].status = "winner";
            let winning_source_layer = source_layers[winning_index].source.clone();
            let secret = source_layers
                .iter()
                .any(|source| source.classification == "secret");
            let shadowed_source_layers = source_layers[..winning_index]
                .iter()
                .map(|source| source.source.clone())
                .collect::<Vec<_>>();
            Some(LabEnvResolutionEntry {
                key,
                classification: if secret { "secret" } else { "public" },
                value_status: if secret {
                    "secret_redacted"
                } else {
                    "redacted"
                },
                value_preview: REDACTED_ENV_VALUE,
                winning_source_layer,
                shadowed_source_layers,
                source_layers,
            })
        })
        .collect::<Vec<_>>();

    serde_json::to_value(LabEnvResolutionReport {
        schema: ENV_RESOLUTION_SCHEMA,
        values_redacted: true,
        keys,
    })
    .unwrap_or_else(|_| {
        serde_json::json!({
            "schema": ENV_RESOLUTION_SCHEMA,
            "values_redacted": true,
            "keys": [],
        })
    })
}

pub(crate) fn lab_runner_homeboy_metadata(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
) -> serde_json::Value {
    let controller_version = homeboy_product_identity::product_version();
    let controller_build_identity = homeboy_product_identity::build_identity().display;
    // Status owns the controller-to-runner convergence target. Reusing its
    // recovery avoids dispatch telling an operator to reinstall the runner's
    // already-incompatible configured binary (#11360).
    let refresh_commands = runner_homeboy_refresh_commands(runner_id, status);
    let primary_remediation_command = refresh_commands.first().cloned();
    let topology_recovery_command = primary_remediation_command.clone();
    let controller_binary = std::env::current_exe()
        .ok()
        .map(|path| path.display().to_string());
    let stale_daemon = status.stale_daemon.as_ref();
    serde_json::json!({
        "schema": "homeboy/lab-runner-homeboy/v1",
        "runner_id": runner_id,
        "controller_version": controller_version,
        "controller_build_identity": controller_build_identity,
        "configured_executable": configured_executable,
        "active_daemon_version": status.session.as_ref().map(|session| session.homeboy_version.clone()),
        "active_daemon_build_identity": status.session.as_ref().and_then(|session| session.homeboy_build_identity.clone()),
        "job_command_binary_version": stale_daemon.map(|warning| warning.job_command_binary_version.clone()),
        "job_command_binary_build_identity": stale_daemon.and_then(|warning| warning.job_command_binary_build_identity.clone()),
        "stale_daemon_severity": stale_daemon.map(|warning| warning.severity),
        "stale_daemon_refresh_command": primary_remediation_command,
        "stale_daemon": stale_daemon.map(RunnerStaleDaemonWarning::sanitized_for_output),
        "version_drift": lab_runner_homeboy_version_drift(status),
        "primary_remediation_command": primary_remediation_command,
        "topology_recovery_command": topology_recovery_command,
        "controller_binary": controller_binary,
        "refresh_commands": refresh_commands,
        "local_upgrade_command": "homeboy upgrade",
        "upgrade_command": format!("homeboy upgrade --force --upgrade-runner {}", shell::quote_arg(runner_id)),
    })
}

/// Env var that forces strict exact-match for the controller↔runner version
/// gate for a single run, regardless of per-runner configuration. Accepts the
/// usual truthy spellings (`1`, `true`, `yes`, `on`).
pub(crate) const REQUIRE_EXACT_RUNNER_VERSION_ENV: &str = "HOMEBOY_REQUIRE_EXACT_RUNNER_VERSION";

/// Resolve whether the controller↔runner version gate should enforce exact
/// byte-identical match. Defaults to compatibility-aware (`false`). Operators
/// opt into strict exact-match either per-runner (the
/// `require_exact_homeboy_version` runner setting) or for a single run via the
/// `HOMEBOY_REQUIRE_EXACT_RUNNER_VERSION` env var.
pub(crate) fn require_exact_runner_version(
    settings: &homeboy_core::server::RunnerSettings,
) -> bool {
    if std::env::var(REQUIRE_EXACT_RUNNER_VERSION_ENV)
        .ok()
        .is_some_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes" | "on"
            )
        })
    {
        return true;
    }
    settings.require_exact_homeboy_version.unwrap_or(false)
}

fn canonical_build_identity(identity: &str) -> &str {
    identity
        .trim()
        .strip_prefix("homeboy ")
        .unwrap_or(identity.trim())
}

/// Compare the recorded daemon to the controller's compatibility identity.
pub(crate) fn lab_runner_homeboy_version_drift(status: &RunnerStatusReport) -> bool {
    observed_version_drift(status).is_some()
}

/// The daemon and controller identities, compared at the same precision,
/// when they differ.
fn observed_version_drift(status: &RunnerStatusReport) -> Option<(String, String)> {
    let controller = crate::controller_identity::compatibility_controller_identity();
    let session = status.session.as_ref()?;
    let (daemon, controller) = match session.homeboy_build_identity.as_deref() {
        Some(identity) => (identity, controller.display),
        None => (session.homeboy_version.as_str(), controller.version),
    };
    (canonical_build_identity(daemon) != canonical_build_identity(&controller))
        .then(|| (daemon.to_string(), controller))
}

/// Why a lab runner's daemon refuses admission: the first failing check, in
/// evaluation order. `None` means the daemon is fresh for admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum LabRunnerAdmissionRefusal {
    /// The stale-daemon comparison could not run, so nothing proves the
    /// daemon compatible.
    StaleDaemonUnverified { probe_error: Option<String> },
    /// Runtime paths the daemon loaded are stale or no longer configured.
    StaleRuntimePaths { paths: Vec<String> },
    /// Exact-build policy is on and the daemon build differs.
    VersionDrift { daemon: String, controller: String },
    /// The daemon's own lease/freshness observation does not authorize work.
    LeaseNotFresh { reason: String, transient: bool },
}

impl LabRunnerAdmissionRefusal {
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Self::StaleDaemonUnverified { .. } => "stale_daemon_unverified",
            Self::StaleRuntimePaths { .. } => "stale_runtime_paths",
            Self::VersionDrift { .. } => "version_drift",
            Self::LeaseNotFresh { .. } => "lease_not_fresh",
        }
    }

    /// Whether the same request can succeed once a transient observation
    /// settles, without anyone changing the daemon.
    pub(crate) fn retryable(&self) -> bool {
        match self {
            Self::StaleDaemonUnverified { .. } => true,
            Self::LeaseNotFresh { transient, .. } => *transient,
            Self::StaleRuntimePaths { .. } | Self::VersionDrift { .. } => false,
        }
    }

    /// The substrate readiness reason a retryable refusal clears with, so
    /// queued work waits through it instead of terminalizing (#17776).
    pub(crate) fn transient_reason(&self) -> Option<&'static str> {
        match self {
            Self::StaleDaemonUnverified { .. } => Some("daemon_unverified"),
            Self::LeaseNotFresh {
                transient: true, ..
            } => Some("daemon_freshness_unavailable"),
            _ => None,
        }
    }

    pub(crate) fn evidence(&self) -> String {
        match self {
            Self::StaleDaemonUnverified { probe_error } => format!(
                "the stale-daemon comparison is unverified ({})",
                probe_error.as_deref().unwrap_or("no comparison ran")
            ),
            Self::StaleRuntimePaths { paths } => {
                format!("stale or changed runtime paths: {}", paths.join(", "))
            }
            Self::VersionDrift { daemon, controller } => format!(
                "exact version required: daemon `{daemon}` differs from controller `{controller}`"
            ),
            Self::LeaseNotFresh { reason, .. } => format!("lease not fresh: {reason}"),
        }
    }

    pub(crate) fn next_action(&self, runner_id: &str) -> String {
        match self {
            Self::StaleDaemonUnverified { .. } => format!(
                "Retry once `homeboy runner status {runner_id}` can compare the daemon; check `homeboy runner doctor {runner_id}` if the probe keeps failing."
            ),
            Self::StaleRuntimePaths { .. } | Self::VersionDrift { .. } => format!(
                "Drain or recover known jobs, then run `homeboy runner refresh-homeboy {runner_id} --reconnect` once `homeboy runner status {runner_id}` reports an authoritative zero active-job count."
            ),
            Self::LeaseNotFresh { transient: true, .. } => format!(
                "Retry once `homeboy runner status {runner_id}` observes the daemon lease; `homeboy runner connect {runner_id}` republishes it."
            ),
            Self::LeaseNotFresh { transient: false, .. } => format!(
                "Restart the runner daemon with `homeboy runner refresh-homeboy {runner_id} --reconnect` once `homeboy runner status {runner_id}` reports an authoritative zero active-job count."
            ),
        }
    }
}

/// The one admission-freshness decision: the first failing check, or `None`.
/// Genuine daemon faults always refuse; controller build drift refuses only
/// when exact-build policy is explicitly requested.
pub(crate) fn lab_runner_admission_refusal(
    status: &RunnerStatusReport,
    require_exact: bool,
) -> Option<LabRunnerAdmissionRefusal> {
    if let Some(warning) = status.admission_blocking_stale_daemon() {
        if !warning.verification.is_compared() {
            return Some(LabRunnerAdmissionRefusal::StaleDaemonUnverified {
                probe_error: warning.probe_error.clone(),
            });
        }
        let paths = warning
            .stale_runtime_paths
            .iter()
            .map(|path| format!("{}={}", path.env, path.path))
            .chain(warning.changed_runtime_paths.iter().map(|path| {
                format!(
                    "{}: {} -> {}",
                    path.env,
                    path.loaded_path.as_deref().unwrap_or("<unset>"),
                    path.configured_path.as_deref().unwrap_or("<unset>")
                )
            }))
            .collect();
        return Some(LabRunnerAdmissionRefusal::StaleRuntimePaths { paths });
    }
    if require_exact {
        if let Some((daemon, controller)) = observed_version_drift(status) {
            return Some(LabRunnerAdmissionRefusal::VersionDrift { daemon, controller });
        }
    }
    let verdict = status.daemon_lease_verdict(None);
    if verdict.fresh {
        return None;
    }
    let (reason, transient) = if !verdict.freshness_observed {
        ("daemon freshness not observed yet".to_string(), true)
    } else if !verdict.lease_published {
        (
            "daemon freshness report publishes no lease".to_string(),
            true,
        )
    } else {
        let code = status
            .daemon_freshness
            .as_ref()
            .and_then(|freshness| freshness.stale_reason_code)
            .and_then(|code| serde_json::to_value(code).ok())
            .and_then(|code| code.as_str().map(str::to_string))
            .unwrap_or_else(|| "unspecified".to_string());
        (format!("daemon reports itself stale ({code})"), false)
    };
    Some(LabRunnerAdmissionRefusal::LeaseNotFresh { reason, transient })
}

/// Daemon compatibility faults, excluding the lease observation.
pub(crate) fn lab_runner_homeboy_has_blocking_status_drift(
    status: &RunnerStatusReport,
    require_exact: bool,
) -> bool {
    lab_runner_admission_refusal(status, require_exact)
        .is_some_and(|refusal| !matches!(refusal, LabRunnerAdmissionRefusal::LeaseNotFresh { .. }))
}

pub(crate) fn lab_runner_daemon_fresh_for_admission(
    status: &RunnerStatusReport,
    require_exact: bool,
) -> bool {
    lab_runner_admission_refusal(status, require_exact).is_none()
}

/// Direct SSH runners execute jobs with their configured executable rather
/// than the controller binary. When that executable's immutable identity is
/// available, it is the authoritative admission comparison for the active
/// daemon. An unavailable identity fails closed.
pub(crate) fn lab_runner_homeboy_has_blocking_drift_against_configured_identity(
    status: &RunnerStatusReport,
    require_exact: bool,
) -> bool {
    lab_runner_homeboy_has_blocking_status_drift(status, require_exact)
}

/// Recover the controller runtime when an independent handoff-stage check
/// proves that the controller, rather than a runner, must be upgraded.
pub(crate) fn controller_homeboy_recovery_command() -> String {
    let Ok(exe) = std::env::current_exe() else {
        return "homeboy upgrade".to_string();
    };
    controller_homeboy_recovery_command_for_binary(&exe)
}

pub(super) fn controller_homeboy_recovery_command_for_binary(exe: &Path) -> String {
    if let Some(source_checkout) = source_checkout_for_binary(exe) {
        return format!(
            "homeboy upgrade --method source --source-path {} --force",
            shell::quote_arg(&source_checkout.display().to_string())
        );
    }
    let exe_display = exe.display().to_string();
    if exe_display.contains("/Homebrew/")
        || exe_display.contains("/homebrew/")
        || exe_display.contains("/Cellar/homeboy/")
        || exe_display.contains("/.linuxbrew/")
    {
        return "homeboy upgrade --method homebrew --force".to_string();
    }
    "homeboy upgrade".to_string()
}

fn source_checkout_for_binary(binary: &Path) -> Option<PathBuf> {
    for ancestor in binary.ancestors() {
        if ancestor.file_name().and_then(|name| name.to_str()) == Some("target") {
            return ancestor.parent().map(Path::to_path_buf);
        }
        if ancestor.join("Cargo.toml").is_file() && ancestor.join("src/main.rs").is_file() {
            return Some(ancestor.to_path_buf());
        }
    }
    None
}

pub(crate) fn lab_source_checkout_metadata(source_path: &Path) -> serde_json::Value {
    let git_branch =
        super::super::super::workspace::git_output(source_path, &["branch", "--show-current"])
            .ok()
            .filter(|value| !value.is_empty())
            .or_else(|| {
                super::super::super::workspace::git_output(
                    source_path,
                    &["rev-parse", "--abbrev-ref", "HEAD"],
                )
                .ok()
            });
    let git_sha = super::super::super::workspace::git_output(source_path, &["rev-parse", "HEAD"])
        .ok()
        .filter(|value| !value.is_empty());
    let git_remote = super::super::super::workspace::git_output(
        source_path,
        &["config", "--get", "remote.origin.url"],
    )
    .ok()
    .filter(|value| !value.is_empty());
    let dirty =
        super::super::super::workspace::git_output(source_path, &["status", "--porcelain=v1"])
            .ok()
            .map(|status| !status.is_empty());

    serde_json::json!({
        "schema": "homeboy/lab-source-checkout/v1",
        "local_path": source_path.display().to_string(),
        "git_branch": git_branch,
        "git_sha": git_sha,
        "git_remote": git_remote,
        "dirty": dirty,
    })
}

pub(crate) fn lab_materialization_proof_metadata(
    source_snapshot: &SourceSnapshot,
    workspace_snapshot_identity: &str,
    remote_workspace: &str,
    runner_homeboy: &serde_json::Value,
    source_checkout: &serde_json::Value,
    workspace_mapping: &serde_json::Value,
    synced_rigs: &[rig_materialization::LabOffloadRigSync],
) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/lab-materialization-proof/v1",
        "remote_workspace": remote_workspace,
        "workload_hashes": {
            "source_snapshot_hash": source_snapshot.snapshot_hash,
            "workspace_snapshot_identity": workspace_snapshot_identity,
        },
        "source_snapshot": source_snapshot,
        "source_checkout": source_checkout,
        "runner_homeboy": runner_homeboy,
        "workspace_mapping": workspace_mapping,
        "rigs": synced_rigs,
    })
}

/// The two workspace plans intentionally have distinct types: the path plan is
/// legacy dispatch metadata, while the synced workspace binds verification.
pub(crate) struct LabWorkspaceMetadataInputs<'a> {
    pub(crate) source_snapshot: &'a SourceSnapshot,
    pub(crate) workspace_snapshots: &'a [SourceSnapshot],
    pub(crate) legacy_path_materialization_plan: &'a PathMaterializationPlan,
    pub(crate) primary_synced_workspace: &'a RunnerWorkspaceSyncOutput,
    pub(crate) extra_synced_workspaces: &'a [RunnerWorkspaceSyncOutput],
}

/// Add verification metadata without changing the registered path-materialization
/// metadata shape consumed by existing dispatch clients.
pub(crate) fn attach_lab_workspace_metadata(
    lab_metadata: &mut serde_json::Value,
    inputs: LabWorkspaceMetadataInputs<'_>,
) -> Result<()> {
    let source_snapshot = inputs.source_snapshot;
    let primary_workspace_plan = &inputs.primary_synced_workspace.materialization_plan;
    let (content_manifest, content_hash) = inputs
        .primary_synced_workspace
        .content
        .as_ref()
        .ok_or_else(|| Error::internal_unexpected("Lab workspace sync has no content inventory"))?;
    let permission_policy = crate::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY;
    let content_hash_algorithm = crate::workspace_content_hash_algorithm(permission_policy)
        .expect("default workspace content permission policy is supported");
    lab_metadata["source_snapshot"] =
        serde_json::to_value(source_snapshot).unwrap_or(serde_json::json!(null));
    lab_metadata["workspace_content_hash"] = serde_json::json!(content_hash);
    lab_metadata["workspace_materialization_plan"] =
        serde_json::to_value(inputs.legacy_path_materialization_plan)
            .unwrap_or(serde_json::json!(null));
    lab_metadata["workspace_verification"] = serde_json::json!({
        "schema": "homeboy/lab-workspace-verification/v2",
        "identity": primary_workspace_plan.identity,
        "content_hash_algorithm": content_hash_algorithm,
        "permission_policy": permission_policy,
        "content_hash": content_hash,
        "content_manifest": content_manifest,
        "sync_excludes": source_snapshot.sync_excludes,
        "source_snapshot": source_snapshot,
        "primary_workspace": primary_workspace_plan,
    });
    lab_metadata["workspace_provenance"] = serde_json::json!({
        "schema": "homeboy/lab-workspace-provenance/v1",
        "entries": inputs.workspace_snapshots.iter().map(|snapshot| {
            let workspace = std::iter::once(inputs.primary_synced_workspace)
                .chain(inputs.extra_synced_workspaces.iter())
                .find(|workspace| Some(workspace.remote_path.as_str()) == snapshot.remote_path.as_deref())
                .ok_or_else(|| Error::internal_unexpected("workspace provenance has no synced workspace"))?;
            let (content_manifest, content_hash) = workspace.content.as_ref()
                .ok_or_else(|| Error::internal_unexpected("workspace provenance has no content inventory"))?;
            Ok(serde_json::json!({
                "remote_path": snapshot.remote_path,
                "materialization_mode": inputs.legacy_path_materialization_plan.entries.iter()
                    .find(|entry| entry.remote_path == snapshot.remote_path.as_deref().unwrap_or_default())
                    .map(|entry| entry.materialization_mode.as_str())
                    .ok_or_else(|| Error::internal_unexpected("workspace provenance snapshot has no materialization plan entry"))?,
                "source_snapshot": snapshot,
                "workspace_verification": {
                    "schema": "homeboy/lab-workspace-verification/v2",
                    "identity": snapshot.workspace_snapshot_identity,
                    "content_hash_algorithm": content_hash_algorithm,
                    "permission_policy": permission_policy,
                    "content_hash": content_hash,
                    "content_manifest": content_manifest,
                    "sync_excludes": snapshot.sync_excludes,
                    "source_snapshot": snapshot,
                    "primary_workspace": {
                        "identity": snapshot.workspace_snapshot_identity,
                        "remote_path": snapshot.remote_path,
                    },
                },
            }))
        }).collect::<Result<Vec<_>>>()?,
    });
    Ok(())
}

pub(crate) fn lab_runtime_dependency_manifest_metadata(
    command_prefix: &[String],
    required_extensions: &[String],
    runner_homeboy: &serde_json::Value,
    source_checkout: &serde_json::Value,
    workspace_mapping: &serde_json::Value,
    remapped_args: &[String],
) -> serde_json::Value {
    serde_json::json!({
        "schema": "homeboy/lab-runtime-dependency-manifest/v1",
        "homeboy_binary": runner_homeboy,
        "extension_runtime": {
            "required_extensions": required_extensions,
            "command_prefix": redact_argv(command_prefix),
        },
        "executor_runtime": provider_config_runtime_manifest(remapped_args),
        "provider_plugins": provider_config_runtime_manifest(remapped_args),
        "components": workspace_mapping,
        "source_checkout": source_checkout,
    })
}

pub(crate) fn source_checkout_ref_display(metadata: &serde_json::Value) -> String {
    let branch = metadata
        .get("git_branch")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty());
    let sha = metadata
        .get("git_sha")
        .and_then(|value| value.as_str())
        .filter(|value| !value.is_empty())
        .map(|value| value.chars().take(12).collect::<String>());
    let dirty = metadata
        .get("dirty")
        .and_then(|value| value.as_bool())
        .map(|value| if value { " dirty" } else { " clean" })
        .unwrap_or("");

    match (branch, sha) {
        (Some(branch), Some(sha)) => format!("{branch}@{sha}{dirty}"),
        (Some(branch), None) => format!("{branch}{dirty}"),
        (None, Some(sha)) => format!("{sha}{dirty}"),
        (None, None) => format!("unknown ref{dirty}"),
    }
}

pub(crate) fn stale_runner_homeboy_error(
    runner_id: &str,
    configured_executable: &str,
    status: &RunnerStatusReport,
) -> Error {
    let refresh_commands = runner_homeboy_refresh_commands(runner_id, status);
    let active_daemon = status
        .session
        .as_ref()
        .map(runner_session_homeboy_display)
        .unwrap_or_else(|| "<not connected>".to_string());
    let current_homeboy = status.stale_daemon.as_ref().map_or_else(
        || "configured runner executable".to_string(),
        runner_stale_daemon_current_display,
    );
    let drift_message = status
        .stale_daemon
        .as_ref()
        .map(|warning| warning.message.clone())
        .unwrap_or_else(|| {
            format!(
                "connected runner daemon reports Homeboy version `{}` while the controller is `{}`",
                status
                    .session
                    .as_ref()
                    .map(|session| session.homeboy_version.as_str())
                    .unwrap_or("<unknown>"),
                homeboy_product_identity::product_version()
            )
        });
    let mut tried = Vec::new();
    if let Some(refresh) = refresh_commands.first() {
        tried.push(format!("One-command topology recovery: {refresh}"));
        tried.push(format!(
            "Reconnect runner `{runner_id}` before retrying Lab offload: {}",
            refresh_commands.join(" && ")
        ));
    } else {
        tried.push(format!(
            "Runner `{runner_id}` has no ancestry-verified refresh target. Inspect exact build identities and recover only with an explicit rollback authorization when intended."
        ));
    }
    tried.push("Use --placement local only if you intentionally want to bypass Lab offload and run locally.".to_string());
    Error::validation_invalid_argument(
        "runner",
        format!(
            "Lab offload refused runner `{runner_id}` because its active daemon control plane differs from the configured job command binary `{configured_executable}`. Active daemon control plane: {active_daemon}; job command binary: {current_homeboy}. {drift_message} Stale runner runtimes can return malformed or misleading provider output; follow the first remediation hint before retrying."
        ),
        Some(runner_id.to_string()),
        Some(tried),
    )
}

pub(crate) fn runner_homeboy_refresh_commands(
    _runner_id: &str,
    status: &RunnerStatusReport,
) -> Vec<String> {
    status
        .stale_daemon
        .as_ref()
        .map(RunnerStaleDaemonWarning::safe_recovery_commands)
        .unwrap_or_default()
}

pub(crate) fn runner_session_homeboy_display(
    session: &super::super::super::RunnerSession,
) -> String {
    session
        .homeboy_build_identity
        .as_deref()
        .unwrap_or(&session.homeboy_version)
        .to_string()
}

pub(crate) fn runner_stale_daemon_current_display(
    warning: &super::super::super::RunnerStaleDaemonWarning,
) -> String {
    warning
        .current_homeboy_build_identity
        .as_deref()
        .unwrap_or(&warning.current_homeboy_version)
        .to_string()
}

pub(crate) fn runner_homeboy_daemon_display(metadata: &serde_json::Value) -> String {
    metadata
        .get("active_daemon_build_identity")
        .and_then(|value| value.as_str())
        .or_else(|| {
            metadata
                .get("active_daemon_version")
                .and_then(|value| value.as_str())
        })
        .unwrap_or("<not connected>")
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn job_scoped_overrides_metadata_redacts_sensitive_env_values() {
        let overrides = LabJobOverrides {
            env: std::collections::HashMap::from([
                ("PLAIN_PATH".to_string(), "/tmp/plugin".to_string()),
                ("API_TOKEN".to_string(), "super-secret".to_string()),
            ]),
            secret_env_names: vec!["API_TOKEN".to_string()],
            workspace_root: Some("/srv/job-root".to_string()),
        };

        let metadata = job_scoped_overrides_metadata(&overrides);

        assert_eq!(metadata["schema"], "homeboy/lab-job-scoped-overrides/v1");
        assert_eq!(metadata["workspace_root"]["value"], "/srv/job-root");
        let env = metadata["env"].as_array().expect("env array");
        let plain = env
            .iter()
            .find(|entry| entry["name"] == "PLAIN_PATH")
            .expect("plain path");
        assert_eq!(plain["value_preview"], "/tmp/plugin");
        assert_eq!(plain["redacted"], false);
        let secret = env
            .iter()
            .find(|entry| entry["name"] == "API_TOKEN")
            .expect("secret");
        assert_eq!(secret["value_preview"], "<redacted>");
        assert_eq!(secret["redacted"], true);
    }

    #[test]
    fn lab_env_resolution_report_records_runtime_overlay_secret_delta_and_job_override_precedence()
    {
        let report = lab_env_resolution_report(vec![
            LabEnvResolutionLayer {
                source: "env_delta",
                env: std::collections::HashMap::from([
                    ("SHARED".to_string(), "from-env-delta".to_string()),
                    ("ENV_ONLY".to_string(), "public".to_string()),
                ]),
                secret_names: Vec::new(),
            },
            LabEnvResolutionLayer {
                source: "runtime_overlay",
                env: std::collections::HashMap::from([
                    ("SHARED".to_string(), "from-runtime-overlay".to_string()),
                    ("RUNTIME_ONLY".to_string(), "/runner/runtime".to_string()),
                ]),
                secret_names: Vec::new(),
            },
            LabEnvResolutionLayer {
                source: SECRET_ENV_PLAN_ENV_DELTA_SOURCE,
                env: std::collections::HashMap::from([
                    ("SHARED".to_string(), "from-secret-plan".to_string()),
                    ("API_TOKEN".to_string(), "super-secret".to_string()),
                ]),
                secret_names: vec!["API_TOKEN".to_string()],
            },
            LabEnvResolutionLayer {
                source: "job_override",
                env: std::collections::HashMap::from([(
                    "SHARED".to_string(),
                    "from-job-override".to_string(),
                )]),
                secret_names: Vec::new(),
            },
        ]);

        assert_eq!(report["schema"], ENV_RESOLUTION_SCHEMA);
        assert_eq!(report["values_redacted"], true);
        let keys = report["keys"].as_array().expect("keys array");
        let shared = keys
            .iter()
            .find(|entry| entry["key"] == "SHARED")
            .expect("shared entry");
        assert_eq!(shared["winning_source_layer"], "job_override");
        assert_eq!(
            shared["shadowed_source_layers"],
            serde_json::json!([
                "env_delta",
                "runtime_overlay",
                SECRET_ENV_PLAN_ENV_DELTA_SOURCE
            ])
        );
        assert_eq!(shared["classification"], "public");
        assert_eq!(shared["value_preview"], REDACTED_ENV_VALUE);

        let api_token = keys
            .iter()
            .find(|entry| entry["key"] == "API_TOKEN")
            .expect("api token entry");
        assert_eq!(
            api_token["winning_source_layer"],
            SECRET_ENV_PLAN_ENV_DELTA_SOURCE
        );
        assert_eq!(api_token["classification"], "secret");
        assert_eq!(api_token["value_status"], "secret_redacted");
        assert_eq!(api_token["value_preview"], REDACTED_ENV_VALUE);

        let runtime_only = keys
            .iter()
            .find(|entry| entry["key"] == "RUNTIME_ONLY")
            .expect("runtime entry");
        assert_eq!(runtime_only["winning_source_layer"], "runtime_overlay");
        assert_eq!(
            runtime_only["shadowed_source_layers"],
            serde_json::json!([])
        );
    }
}
