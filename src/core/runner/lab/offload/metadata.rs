//! Lab runner / source-checkout / materialization-proof metadata builders,
//! rig component path overrides, and the stale-runner-homeboy error.

use super::*;

const ENV_RESOLUTION_SCHEMA: &str = "homeboy/env-resolution/v1";
const REDACTED_ENV_VALUE: &str = "<redacted>";

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
    let controller_version = env!("CARGO_PKG_VERSION");
    let controller_build_identity = crate::core::build_identity::current().display;
    let refresh_commands = vec![
        format!(
            "homeboy runner refresh-homeboy {} --ref main --reconnect",
            shell::quote_arg(runner_id)
        ),
        format!("homeboy runner disconnect {}", shell::quote_arg(runner_id)),
        format!("homeboy runner connect {}", shell::quote_arg(runner_id)),
    ];
    serde_json::json!({
        "schema": "homeboy/lab-runner-homeboy/v1",
        "runner_id": runner_id,
        "controller_version": controller_version,
        "controller_build_identity": controller_build_identity,
        "configured_executable": configured_executable,
        "active_daemon_version": status.session.as_ref().map(|session| session.homeboy_version.clone()),
        "active_daemon_build_identity": status.session.as_ref().and_then(|session| session.homeboy_build_identity.clone()),
        "stale_daemon": status.stale_daemon,
        "version_drift": lab_runner_homeboy_version_drift(status),
        "refresh_commands": refresh_commands,
        "upgrade_command": format!("homeboy upgrade --force --upgrade-runner {}", shell::quote_arg(runner_id)),
    })
}

pub(crate) fn lab_runner_homeboy_has_blocking_drift(status: &RunnerStatusReport) -> bool {
    status.stale_daemon.is_some() || lab_runner_homeboy_version_drift(status)
}

fn lab_runner_homeboy_version_drift(status: &RunnerStatusReport) -> bool {
    let controller_version = env!("CARGO_PKG_VERSION");
    status
        .session
        .as_ref()
        .map(|session| session.homeboy_version.as_str())
        .is_some_and(|version| version != controller_version)
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
                env!("CARGO_PKG_VERSION")
            )
        });
    let refresh = refresh_commands.join(" && ");
    Error::validation_invalid_argument(
        "runner",
        format!(
            "Lab offload refused runner `{runner_id}` because its active daemon Homeboy/runtime differs from the configured runner executable `{configured_executable}`. Active daemon: {active_daemon}; configured runtime: {current_homeboy}. {drift_message} Stale runner runtimes can return malformed or misleading provider output; reconnect the runner before retrying."
        ),
        Some(runner_id.to_string()),
        Some(vec![
            format!("Reconnect runner `{runner_id}` before retrying Lab offload: {refresh}"),
            format!("If the runner binary itself is stale, refresh or select a clean runner binary with `homeboy runner refresh-homeboy {} --ref main --reconnect`.", shell::quote_arg(runner_id)),
            "Use --force-hot --allow-local-hot only if you intentionally want to bypass Lab offload and run locally.".to_string(),
        ]),
    )
}

pub(crate) fn runner_homeboy_refresh_commands(
    runner_id: &str,
    status: &RunnerStatusReport,
) -> Vec<String> {
    let commands = status
        .stale_daemon
        .as_ref()
        .map(|warning| warning.recovery_commands.clone())
        .unwrap_or_default();
    if !commands.is_empty() && !runner_id.contains(char::is_whitespace) {
        return commands;
    }
    vec![
        format!(
            "homeboy runner refresh-homeboy {} --ref main --reconnect",
            shell::quote_arg(runner_id)
        ),
        format!("homeboy runner disconnect {}", shell::quote_arg(runner_id)),
        format!("homeboy runner connect {}", shell::quote_arg(runner_id)),
    ]
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
                source: "secret_env_plan_env_delta",
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
            serde_json::json!(["env_delta", "runtime_overlay", "secret_env_plan_env_delta"])
        );
        assert_eq!(shared["classification"], "public");
        assert_eq!(shared["value_preview"], REDACTED_ENV_VALUE);

        let api_token = keys
            .iter()
            .find(|entry| entry["key"] == "API_TOKEN")
            .expect("api token entry");
        assert_eq!(
            api_token["winning_source_layer"],
            "secret_env_plan_env_delta"
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
