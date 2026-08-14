//! Lab runner / source-checkout / materialization-proof metadata builders,
//! rig component path overrides, and the stale-runner-homeboy error.

use super::*;
use homeboy_core::runner_execution_envelope::PathMaterializationPlan;
#[cfg(test)]
use homeboy_core::secret_env_plan::SECRET_ENV_PLAN_ENV_DELTA_SOURCE;
use homeboy_lab_runner_contract::{
    negotiate_lab_capability_handshake, required_lab_handoff_capabilities, LabCapabilityAdmission,
    LabCapabilityHandshake, LabRuntimeAncestry, LabRuntimeIdentity,
};
use std::path::{Path, PathBuf};
use std::process::Command;

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
    let controller = homeboy_product_identity::build_identity();
    let controller_identity = LabRuntimeIdentity {
        build_identity: controller.display,
        source_revision: controller.git_commit.unwrap_or_default(),
        clean: controller.git_dirty == Some(false),
    };
    let command_evidence = crate::configured_runner_homeboy_handshake_evidence(runner, homeboy)?;
    let Some(session) = status.session.as_ref() else {
        let (runner_command, runner_command_capabilities) =
            command_evidence.unwrap_or_else(|| (controller_identity.clone(), Vec::new()));
        return Ok(negotiate_lab_capability_handshake(
            &LabCapabilityHandshake {
                controller: controller_identity.clone(),
                required_capabilities: required_lab_handoff_capabilities(),
                runner_command,
                runner_command_capabilities,
                daemon: controller_identity,
                daemon_capabilities: Vec::new(),
                ancestry: LabRuntimeAncestry::Unknown,
            },
        ));
    };
    let daemon_source = session
        .homeboy_build_identity
        .as_deref()
        .and_then(build_commit)
        .unwrap_or_default()
        .to_string();
    let daemon = LabRuntimeIdentity {
        build_identity: session.homeboy_build_identity.clone().unwrap_or_default(),
        source_revision: daemon_source,
        clean: !session
            .homeboy_build_identity
            .as_deref()
            .is_some_and(|value| value.ends_with("-dirty")),
    };
    let daemon_capabilities = session
        .local_url
        .as_deref()
        .and_then(|url| crate::daemon_lab_handoff_capabilities(url).ok())
        .unwrap_or_default();
    let command_evidence = command_evidence
        .or_else(|| hash_bound_runner_command_evidence(status, homeboy, &daemon_capabilities));
    let Some((runner_command, runner_command_capabilities)) = command_evidence else {
        return Ok(negotiate_lab_capability_handshake(
            &LabCapabilityHandshake {
                controller: controller_identity.clone(),
                required_capabilities: required_lab_handoff_capabilities(),
                runner_command: controller_identity.clone(),
                runner_command_capabilities: Vec::new(),
                daemon: controller_identity,
                daemon_capabilities,
                ancestry: LabRuntimeAncestry::Unknown,
            },
        ));
    };
    let ancestry = runtime_ancestry(
        &controller_identity.source_revision,
        &runner_command.source_revision,
    );
    Ok(negotiate_lab_capability_handshake(
        &LabCapabilityHandshake {
            controller: controller_identity,
            required_capabilities: required_lab_handoff_capabilities(),
            runner_command,
            runner_command_capabilities,
            daemon,
            daemon_capabilities,
            ancestry,
        },
    ))
}

pub(super) fn hash_bound_runner_command_evidence(
    status: &RunnerStatusReport,
    homeboy: &str,
    daemon_capabilities: &[homeboy_lab_runner_contract::LabCapabilityVersion],
) -> Option<(
    LabRuntimeIdentity,
    Vec<homeboy_lab_runner_contract::LabCapabilityVersion>,
)> {
    let session = status.session.as_ref()?;
    let session_identity = session.homeboy_build_identity.as_deref()?;
    let freshness = status.daemon_freshness.as_ref()?;
    let binary_hash = freshness.binary_hash.as_deref()?;
    let slot_hash = Path::new(homeboy)
        .parent()?
        .file_name()?
        .to_str()?
        .strip_prefix("homeboy-")?;
    let hash_is_valid = slot_hash.len() == 64
        && slot_hash.bytes().all(|byte| byte.is_ascii_hexdigit())
        && slot_hash == binary_hash;
    if !freshness.fresh
        || session.mode != RunnerTunnelMode::DirectSsh
        || freshness.lease_id != session.remote_daemon_lease_id
        || freshness.pid != session.remote_daemon_pid
        || !hash_is_valid
        || daemon_capabilities.is_empty()
        || status
            .configured_job_binary_build_identity
            .as_deref()
            .is_some_and(|configured| configured != session_identity)
        || freshness.daemon_build_identity.as_deref() != Some(session_identity)
    {
        return None;
    }
    Some((
        LabRuntimeIdentity {
            build_identity: session_identity.to_string(),
            source_revision: build_commit(session_identity)?.to_string(),
            clean: !session_identity.ends_with("-dirty"),
        },
        daemon_capabilities.to_vec(),
    ))
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
    let Some(source) = std::env::current_exe()
        .ok()
        .and_then(|path| source_checkout_for_binary(&path))
    else {
        return LabRuntimeAncestry::Unknown;
    };
    match Command::new("git")
        .args([
            "-C",
            &source.display().to_string(),
            "merge-base",
            "--is-ancestor",
            controller,
            runner,
        ])
        .status()
    {
        Ok(status) if status.success() => LabRuntimeAncestry::VerifiedNewerDescendant,
        Ok(_) => LabRuntimeAncestry::Diverged,
        Err(_) => LabRuntimeAncestry::Unknown,
    }
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

/// Classification of controller↔runner Homeboy version drift for the Lab
/// offload dispatch gate.
///
/// Patch-level drift within the same `MAJOR.MINOR` is wire-compatible when
/// build provenance is unavailable. A complete build identity is authoritative:
/// a differing identity is refused even when its semver matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RunnerHomeboyVersionDrift {
    /// Controller and runner report the same version (no drift).
    None,
    /// Same `MAJOR.MINOR`, patch differs — wire-compatible; proceed with a warning.
    CompatiblePatch,
    /// `MAJOR`/`MINOR` differs, or a version string could not be parsed while the
    /// raw strings differ — the provider contract could genuinely differ; refuse.
    Incompatible,
}

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

/// Extract the `(MAJOR, MINOR)` pair from a Homeboy version string, tolerating a
/// leading label (e.g. `homeboy 0.266.1`) and trailing build/prerelease
/// metadata (e.g. `0.266.1+abc`). Mirrors the lenient parsing already used by
/// the runner version probe so the gate accepts the same shapes.
fn parse_version_triplet(version: &str) -> Option<(u64, u64, u64)> {
    let candidate = version
        .split_whitespace()
        .find(|token| token.chars().next().is_some_and(|c| c.is_ascii_digit()))
        .unwrap_or_else(|| version.trim());
    let mut parts = candidate.split('.');
    let mut parse_part = || {
        parts
            .next()?
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect::<String>()
            .parse()
            .ok()
    };
    Some((parse_part()?, parse_part()?, parse_part()?))
}

fn parse_major_minor(version: &str) -> Option<(u64, u64)> {
    parse_version_triplet(version).map(|(major, minor, _patch)| (major, minor))
}

fn canonical_build_identity(identity: &str) -> &str {
    identity
        .trim()
        .strip_prefix("homeboy ")
        .unwrap_or(identity.trim())
}

/// Classify controller↔runner Homeboy version drift using the same drift
/// evidence the metadata builder reports (runner session version vs the
/// compiled controller version).
pub(crate) fn classify_runner_homeboy_version_drift(
    status: &RunnerStatusReport,
) -> RunnerHomeboyVersionDrift {
    let controller = homeboy_product_identity::build_identity();
    let Some(session) = status.session.as_ref() else {
        // No connected session means no version evidence to compare; leave
        // connectivity gating to the dedicated preflight checks.
        return RunnerHomeboyVersionDrift::None;
    };

    if let Some(runner_identity) = session.homeboy_build_identity.as_deref() {
        // Build provenance resolves representation differences between `0.x.y`
        // and `homeboy 0.x.y+commit` without accepting distinct same-semver builds.
        return if canonical_build_identity(runner_identity)
            == canonical_build_identity(&controller.display)
        {
            RunnerHomeboyVersionDrift::None
        } else {
            RunnerHomeboyVersionDrift::Incompatible
        };
    }

    let runner_version = session.homeboy_version.as_str();
    let controller_version = controller.version.as_str();

    // Version-only sessions do not prove a build, so preserve compatible patch
    // handling for older runners that cannot report build provenance.
    if canonical_build_identity(runner_version) == canonical_build_identity(controller_version) {
        return RunnerHomeboyVersionDrift::None;
    }

    match (
        parse_major_minor(controller_version),
        parse_major_minor(runner_version),
    ) {
        (Some(controller), Some(runner)) if controller == runner => {
            RunnerHomeboyVersionDrift::CompatiblePatch
        }
        // Same-MAJOR.MINOR was handled above; any other parseable pair differs
        // at MAJOR/MINOR. Unparseable strings that already differ are refused
        // conservatively.
        _ => RunnerHomeboyVersionDrift::Incompatible,
    }
}

pub(crate) fn lab_runner_homeboy_has_blocking_drift(
    status: &RunnerStatusReport,
    require_exact: bool,
) -> bool {
    lab_runner_homeboy_has_blocking_drift_against_configured_identity(status, None, require_exact)
}

/// Direct SSH runners execute jobs with their configured executable rather
/// than the controller binary. When that executable's immutable identity is
/// available, it is the authoritative admission comparison for the active
/// daemon. An unavailable identity fails closed.
pub(crate) fn lab_runner_homeboy_has_blocking_drift_against_configured_identity(
    status: &RunnerStatusReport,
    configured_build_identity: Option<&str>,
    require_exact: bool,
) -> bool {
    if status
        .session
        .as_ref()
        .is_some_and(|session| session.mode == RunnerTunnelMode::DirectSsh)
    {
        let Some(configured_build_identity) = configured_build_identity else {
            return true;
        };
        let Some(active_daemon_identity) = status
            .session
            .as_ref()
            .and_then(|session| session.homeboy_build_identity.as_deref())
        else {
            return true;
        };
        return canonical_build_identity(active_daemon_identity)
            != canonical_build_identity(configured_build_identity);
    }

    // Build-identity drift within the runner (active daemon control plane vs the
    // configured job command binary) is an internal-inconsistency signal that is
    // always blocking regardless of the controller↔runner version policy.
    //
    // Drift is an *observed* inconsistency. A report that compared nothing
    // observed no drift, and this branch is exactly where every reverse runner
    // now lands (#11106) — blocking on it would take the whole class out of
    // service on the strength of a gap rather than a mismatch.
    if status.admission_blocking_stale_daemon().is_some() {
        return true;
    }
    match classify_runner_homeboy_version_drift(status) {
        RunnerHomeboyVersionDrift::None => false,
        RunnerHomeboyVersionDrift::CompatiblePatch => require_exact,
        RunnerHomeboyVersionDrift::Incompatible => true,
    }
}

/// Warning to surface when the runner is on a wire-compatible patch-drifted
/// version and the gate is allowing the run to proceed. Returns `None` when
/// there is nothing to warn about (no drift, refused incompatibility, or strict
/// mode where the drift is instead surfaced as an error).
pub(crate) fn lab_runner_homeboy_compatible_drift_warning(
    status: &RunnerStatusReport,
    require_exact: bool,
) -> Option<String> {
    if require_exact {
        return None;
    }
    if !matches!(
        classify_runner_homeboy_version_drift(status),
        RunnerHomeboyVersionDrift::CompatiblePatch
    ) {
        return None;
    }
    let controller_version = homeboy_product_identity::product_version();
    let runner_version = status
        .session
        .as_ref()
        .map(|session| session.homeboy_version.as_str())
        .unwrap_or("<unknown>");
    let remediation = runner_homeboy_align_to_controller_command(
        status
            .session
            .as_ref()
            .map(|session| session.runner_id.as_str())
            .unwrap_or("<runner-id>"),
    );
    Some(format!(
        "Lab offload: runner reports Homeboy `{runner_version}` while the controller is `{controller_version}`; same MAJOR.MINOR (patch drift only) is wire-compatible, proceeding. Align the runner with `{remediation}`. Set runner setting `require_exact_homeboy_version` or export `{REQUIRE_EXACT_RUNNER_VERSION_ENV}=1` to enforce exact-match."
    ))
}

fn lab_runner_homeboy_version_drift(status: &RunnerStatusReport) -> bool {
    classify_runner_homeboy_version_drift(status) != RunnerHomeboyVersionDrift::None
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
    if exe_display.contains(&format!(
        "/.{}/bin/",
        homeboy_core::defaults::secondary_install_method_key()
    )) {
        return format!(
            "homeboy upgrade --method {} --force",
            homeboy_core::defaults::secondary_install_method_key()
        );
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
    pub(crate) legacy_path_materialization_plan: &'a PathMaterializationPlan,
    pub(crate) primary_synced_workspace: &'a RunnerWorkspaceSyncOutput,
}

/// Add verification metadata without changing the registered path-materialization
/// metadata shape consumed by existing dispatch clients.
pub(crate) fn attach_lab_workspace_metadata(
    lab_metadata: &mut serde_json::Value,
    inputs: LabWorkspaceMetadataInputs<'_>,
) -> Result<()> {
    let source_snapshot = inputs.source_snapshot;
    let primary_workspace_plan = &inputs.primary_synced_workspace.materialization_plan;
    let source_path = source_snapshot.local_path.as_deref().ok_or_else(|| {
        Error::validation_invalid_argument(
            "source_snapshot.local_path",
            "Lab workspace verification requires a controller source path",
            None,
            None,
        )
    })?;
    let permission_policy = crate::WORKSPACE_CONTENT_DEFAULT_PERMISSION_POLICY;
    let content_hash =
        crate::workspace_content_hash(Path::new(source_path), &source_snapshot.sync_excludes)?;
    let content_hash_algorithm = crate::workspace_content_hash_algorithm(permission_policy)
        .expect("default workspace content permission policy is supported");
    let content_manifest = crate::workspace_content_manifest_for_policy(
        Path::new(source_path),
        &source_snapshot.sync_excludes,
        permission_policy,
    )?;
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

pub(crate) fn runner_homeboy_align_to_controller_command(runner_id: &str) -> String {
    format!(
        "homeboy runner refresh-homeboy {} --ref {} --reconnect",
        shell::quote_arg(runner_id),
        controller_refresh_ref()
    )
}

fn controller_refresh_ref() -> String {
    homeboy_product_identity::build_identity()
        .git_commit
        .unwrap_or_else(|| format!("v{}", homeboy_product_identity::product_version()))
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
