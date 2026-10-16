//! Immutable execution facts captured after Lab admission and before dispatch.

use homeboy_engine_primitives::content_hash;
use std::collections::HashMap;

pub(crate) const LAB_EXECUTION_BUNDLE_ENV: &str = "HOMEBOY_LAB_EXECUTION_BUNDLE";
pub(crate) const LAB_EXECUTION_BUNDLE_SCHEMA: &str = "homeboy/lab-execution-bundle/v1";

pub(crate) fn binary(path: &str, verified_build_identity: Option<&str>) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "build_identity": verified_build_identity,
    })
}

/// Facts that identify one admitted Lab execution. Inline offload and durable
/// staging both supply these so the two paths cannot publish different bundles.
pub(crate) struct ExecutionBundleInputs<'a> {
    /// Runner Homeboy executable the command will run through.
    pub(crate) homeboy_path: &'a str,
    /// Build identity verified for `homeboy_path`; never inferred or defaulted.
    pub(crate) verified_build_identity: Option<&'a str>,
    pub(crate) remote_cwd: &'a str,
    /// Direct-daemon admission coordinates.
    pub(crate) daemon_lease_id: &'a str,
    pub(crate) reservation_job_id: &'a str,
    pub(crate) rig_registry_root: Option<&'a str>,
    pub(crate) rigs: serde_json::Value,
    pub(crate) extensions: serde_json::Value,
    pub(crate) extension_runtime_home: Option<&'a str>,
    pub(crate) workspace_mapping: serde_json::Value,
    pub(crate) required_extensions: &'a [String],
    pub(crate) requested_command: &'a [String],
    pub(crate) executed_argv: &'a [String],
    pub(crate) runner_id: &'a str,
}

/// Build the immutable execution bundle for an admitted Lab job.
pub(crate) fn build(inputs: ExecutionBundleInputs<'_>) -> serde_json::Value {
    serde_json::json!({
        "schema": LAB_EXECUTION_BUNDLE_SCHEMA,
        "binary": binary(inputs.homeboy_path, inputs.verified_build_identity),
        // The materialized workspace deletes this artifact root on every known
        // terminal result, bounding content-addressed transport retention.
        "provenance_dir": format!(
            "{}/provenance",
            crate::lab::offload::remote_lab_artifact_dir(inputs.remote_cwd)
        ),
        "admission": {
            "daemon_lease_id": inputs.daemon_lease_id,
            "reservation_job_id": inputs.reservation_job_id,
        },
        "rig_registry_root": inputs.rig_registry_root,
        "rigs": inputs.rigs,
        "extensions": inputs.extensions,
        "extension_runtime_home": inputs.extension_runtime_home,
        "workspace_mapping": inputs.workspace_mapping,
        "identities": {
            "requested": {
                "runner_id": inputs.runner_id,
                "extensions": inputs.required_extensions,
                "command": inputs.requested_command,
            },
            "resolved": {
                "binary": inputs.homeboy_path,
                "rig_registry_root": inputs.rig_registry_root,
                "rigs": inputs.rigs,
                "extensions": inputs.extensions,
                "workspace_mapping": inputs.workspace_mapping,
            },
            "executed": {
                "argv": inputs.executed_argv,
                "daemon_lease_id": inputs.daemon_lease_id,
                "reservation_job_id": inputs.reservation_job_id,
            },
        },
    })
}

/// Validate the authority passed to the runner process. This is deliberately
/// stricter than checking for an environment key: only a complete bundle can
/// replace runner-global extension resolution.
pub(crate) fn validate_bundle_env(
    env: &HashMap<String, String>,
    command: &[String],
    required_extensions: &[String],
) -> bool {
    let Some(raw) = env.get(LAB_EXECUTION_BUNDLE_ENV) else {
        return false;
    };
    let Some(bundle) = homeboy_core::observation::resolve_json_value(raw) else {
        return false;
    };
    let non_empty = |pointer| {
        bundle
            .pointer(pointer)
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value| !value.trim().is_empty())
    };
    if bundle.get("schema").and_then(serde_json::Value::as_str) != Some(LAB_EXECUTION_BUNDLE_SCHEMA)
        || !non_empty("/binary/path")
        || command.first().map(String::as_str)
            != bundle
                .pointer("/binary/path")
                .and_then(serde_json::Value::as_str)
        || (!required_extensions.is_empty() && !non_empty("/extension_runtime_home"))
    {
        return false;
    }
    if !non_empty("/admission/daemon_lease_id") || !non_empty("/admission/reservation_job_id") {
        return false;
    }
    let Some(overlays) = bundle
        .get("extensions")
        .and_then(serde_json::Value::as_array)
    else {
        return required_extensions.is_empty();
    };
    required_extensions.iter().all(|id| {
        overlays.iter().any(|overlay| {
            overlay.get("id").and_then(serde_json::Value::as_str) == Some(id)
                && overlay
                    .get("remote_path")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(|path| !path.trim().is_empty())
                && overlay
                    .get("content_hash")
                    .and_then(serde_json::Value::as_str)
                    .is_some_and(content_hash::is_sha256_hex)
        })
    })
}

pub(crate) fn bundle_env(bundle: &serde_json::Value) -> HashMap<String, String> {
    HashMap::from([(
        LAB_EXECUTION_BUNDLE_ENV.to_string(),
        serde_json::to_string(bundle).expect("Lab execution bundle serializes"),
    )])
}

/// Export the bundle as the child's authoritative environment: the bundle
/// itself, the admitted executable, and the job-private Homeboy config root.
/// Call this after every untrusted per-job override layer.
pub(crate) fn apply_authority_env(env: &mut HashMap<String, String>, bundle: &serde_json::Value) {
    env.extend(bundle_env(bundle));
    if let Some(path) = bundle
        .pointer("/binary/path")
        .and_then(serde_json::Value::as_str)
    {
        env.insert("HOMEBOY_COMMAND".to_string(), path.to_string());
    }
    if let Some(home) = bundle
        .get("extension_runtime_home")
        .and_then(serde_json::Value::as_str)
    {
        // Root only Homeboy's own config at the job overlay. Rehoming `HOME`
        // hid the runner user's toolchains (rustup, cargo, npm, git config),
        // so a runner where `cargo` works failed toolchain readiness (#14991).
        // A caller's `HOME` never applies on the runner.
        env.remove("HOME");
        env.insert(
            homeboy_core::paths::HOMEBOY_CONFIG_ROOT_ENV.to_string(),
            format!("{}/.config/homeboy", home.trim_end_matches('/')),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn binary_uses_the_verified_configured_identity_without_status_fallbacks() {
        let binary = binary("/runner/bin/homeboy", Some("homeboy 1.2.3+abc123"));

        assert_eq!(binary["path"], "/runner/bin/homeboy");
        assert_eq!(binary["build_identity"], "homeboy 1.2.3+abc123");
    }

    #[test]
    fn bundle_requires_matching_command_admission_and_private_extension_snapshot() {
        let bundle = serde_json::json!({
            "schema": LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": { "path": "/runner/bin/homeboy-a" },
            "admission": { "daemon_lease_id": "lease-a", "reservation_job_id": "job-a" },
            "extension_runtime_home": "/runner/job-a/home",
            "extensions": [{ "id": "fixture", "remote_path": "/runner/job-a/fixture", "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }]
        });
        let env = bundle_env(&bundle);
        assert!(validate_bundle_env(
            &env,
            &["/runner/bin/homeboy-a".to_string(), "test".to_string()],
            &["fixture".to_string()],
        ));
        assert!(!validate_bundle_env(
            &env,
            &["/runner/bin/homeboy-b".to_string(), "test".to_string()],
            &["fixture".to_string()],
        ));
    }

    #[test]
    fn concurrent_jobs_keep_their_own_rig_extension_and_promoted_binary_authority() {
        let job_a = serde_json::json!({
            "schema": LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": { "path": "/runner/releases/homeboy-a" },
            "admission": { "daemon_lease_id": "lease-a", "reservation_job_id": "job-a" },
            "rig_registry_root": "/runner/jobs/a/rigs",
            "rigs": [{ "rig_id": "same-id", "workload_hashes": { "source_snapshot_hash": "catalog-a" } }],
            "extension_runtime_home": "/runner/jobs/a/home",
            "extensions": [{ "id": "fixture", "remote_path": "/runner/jobs/a/extensions/fixture", "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }]
        });
        let job_b = serde_json::json!({
            "schema": LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": { "path": "/runner/releases/homeboy-b" },
            "admission": { "daemon_lease_id": "lease-b", "reservation_job_id": "job-b" },
            "rig_registry_root": "/runner/jobs/b/rigs",
            "rigs": [{ "rig_id": "same-id", "workload_hashes": { "source_snapshot_hash": "catalog-b" } }],
            "extension_runtime_home": "/runner/jobs/b/home",
            "extensions": [{ "id": "fixture", "remote_path": "/runner/jobs/b/extensions/fixture", "content_hash": "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb" }]
        });

        // A new runner default can point at B after A was admitted. A remains
        // executable only with its accepted command and private runtime facts.
        let default_binary_after_promotion = "/runner/releases/homeboy-b";
        let a_env = bundle_env(&job_a);
        let b_env = bundle_env(&job_b);
        assert!(validate_bundle_env(
            &a_env,
            &["/runner/releases/homeboy-a".to_string(), "fuzz".to_string()],
            &["fixture".to_string()],
        ));
        assert!(validate_bundle_env(
            &b_env,
            &[
                default_binary_after_promotion.to_string(),
                "fuzz".to_string()
            ],
            &["fixture".to_string()],
        ));
        assert!(!validate_bundle_env(
            &a_env,
            &[
                default_binary_after_promotion.to_string(),
                "fuzz".to_string()
            ],
            &["fixture".to_string()],
        ));
        assert_ne!(job_a["rig_registry_root"], job_b["rig_registry_root"]);
        assert_ne!(
            job_a["rigs"][0]["workload_hashes"],
            job_b["rigs"][0]["workload_hashes"]
        );
        assert_ne!(
            job_a["extension_runtime_home"],
            job_b["extension_runtime_home"]
        );
        assert_ne!(
            job_a["extensions"][0]["content_hash"],
            job_b["extensions"][0]["content_hash"]
        );
    }

    #[test]
    fn malformed_bundle_cannot_bypass_global_extension_parity() {
        let bundle = serde_json::json!({
            "schema": LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": { "path": "/runner/bin/homeboy" },
            "admission": { "daemon_lease_id": "lease", "reservation_job_id": "job" },
            "extension_runtime_home": "/runner/job/home",
            "extensions": [{ "id": "fixture", "remote_path": "/runner/job/fixture", "content_hash": "not-a-sha256" }]
        });
        assert!(!validate_bundle_env(
            &bundle_env(&bundle),
            &["/runner/bin/homeboy".to_string(), "test".to_string()],
            &["fixture".to_string()],
        ));
    }

    #[test]
    fn incomplete_or_invalid_bundle_cannot_bypass_global_extension_parity() {
        let command = vec!["/runner/bin/homeboy".to_string(), "test".to_string()];
        let required = vec!["fixture".to_string()];
        let valid = serde_json::json!({
            "schema": LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": { "path": "/runner/bin/homeboy" },
            "admission": { "daemon_lease_id": "lease", "reservation_job_id": "job" },
            "extension_runtime_home": "/runner/job/home",
            "extensions": [{ "id": "fixture", "remote_path": "/runner/job/fixture", "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }]
        });

        let mut missing_overlay = valid.clone();
        missing_overlay["extensions"] = serde_json::json!([]);
        let mut blank_home = valid.clone();
        blank_home["extension_runtime_home"] = serde_json::json!("  ");
        let mut missing_admission = valid;
        missing_admission["admission"]["daemon_lease_id"] = serde_json::Value::Null;

        for bundle in [missing_overlay, blank_home, missing_admission] {
            assert!(!validate_bundle_env(
                &bundle_env(&bundle),
                &command,
                &required
            ));
        }
        assert!(!validate_bundle_env(
            &HashMap::from([(LAB_EXECUTION_BUNDLE_ENV.to_string(), "not json".to_string())]),
            &command,
            &required,
        ));
    }

    fn sample_inputs<'a>(
        command: &'a [String],
        required: &'a [String],
        lease: &'a str,
        job: &'a str,
    ) -> ExecutionBundleInputs<'a> {
        ExecutionBundleInputs {
            homeboy_path: "/runner/bin/homeboy",
            verified_build_identity: Some("homeboy 1.2.3+abc123"),
            remote_cwd: "/srv/homeboy/_lab_workspaces/app-abc",
            daemon_lease_id: lease,
            reservation_job_id: job,
            rig_registry_root: Some("/srv/rigs"),
            rigs: serde_json::json!([{ "rig_id": "r" }]),
            extensions: serde_json::json!([{ "id": "fixture", "remote_path": "/runner/job/fixture", "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }]),
            extension_runtime_home: Some("/runner/job/home"),
            workspace_mapping: serde_json::json!({ "workspaces": [] }),
            required_extensions: required,
            requested_command: command,
            executed_argv: command,
            runner_id: "lab",
        }
    }

    #[test]
    fn build_publishes_direct_admission_bundle_with_run_owned_provenance_dir() {
        let command = vec!["/runner/bin/homeboy".to_string(), "test".to_string()];
        let required = vec!["fixture".to_string()];
        let bundle = build(sample_inputs(&command, &required, "lease-a", "job-a"));

        assert_eq!(bundle["schema"], LAB_EXECUTION_BUNDLE_SCHEMA);
        assert_eq!(bundle["binary"]["path"], "/runner/bin/homeboy");
        assert_eq!(bundle["binary"]["build_identity"], "homeboy 1.2.3+abc123");
        assert_eq!(
            bundle["provenance_dir"],
            format!(
                "{}/provenance",
                crate::lab::offload::remote_lab_artifact_dir(
                    "/srv/homeboy/_lab_workspaces/app-abc"
                )
            )
        );
        assert_eq!(bundle["admission"]["daemon_lease_id"], "lease-a");
        assert_eq!(bundle["admission"]["reservation_job_id"], "job-a");
        assert_eq!(bundle["rig_registry_root"], "/srv/rigs");
        assert_eq!(bundle["extension_runtime_home"], "/runner/job/home");
        assert_eq!(bundle["identities"]["requested"]["runner_id"], "lab");
        assert_eq!(bundle["identities"]["requested"]["command"][1], "test");
        assert_eq!(
            bundle["identities"]["resolved"]["extensions"],
            bundle["extensions"]
        );
        assert_eq!(bundle["identities"]["executed"]["argv"][0], command[0]);
        assert_eq!(
            bundle["identities"]["executed"]["reservation_job_id"],
            "job-a"
        );
        assert!(validate_bundle_env(
            &bundle_env(&bundle),
            &command,
            &required
        ));
    }

    #[test]
    fn authority_env_overrides_untrusted_values_and_roots_config_at_job_home() {
        let command = vec!["/runner/bin/homeboy".to_string()];
        let bundle = build(sample_inputs(&command, &[], "lease-a", "job-a"));
        let mut env = HashMap::from([
            ("HOMEBOY_COMMAND".to_string(), "/tmp/evil".to_string()),
            (LAB_EXECUTION_BUNDLE_ENV.to_string(), "{}".to_string()),
            ("HOME".to_string(), "/home/caller".to_string()),
        ]);

        apply_authority_env(&mut env, &bundle);

        assert_eq!(env["HOMEBOY_COMMAND"], "/runner/bin/homeboy");
        assert!(!env.contains_key("HOME"));
        assert_eq!(
            env[homeboy_core::paths::HOMEBOY_CONFIG_ROOT_ENV],
            "/runner/job/home/.config/homeboy"
        );
        assert!(validate_bundle_env(&env, &command, &[]));
    }
}
