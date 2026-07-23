//! Immutable execution facts captured after Lab admission and before dispatch.

use std::collections::HashMap;

pub(crate) const LAB_EXECUTION_BUNDLE_ENV: &str = "HOMEBOY_LAB_EXECUTION_BUNDLE";
pub(crate) const LAB_EXECUTION_BUNDLE_SCHEMA: &str = "homeboy/lab-execution-bundle/v1";

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
    let direct_admission =
        non_empty("/admission/daemon_lease_id") && non_empty("/admission/reservation_job_id");
    let reverse_broker_admission = bundle
        .pointer("/admission/authority")
        .and_then(serde_json::Value::as_str)
        == Some("reverse_broker");
    if !direct_admission && !reverse_broker_admission {
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
                    .is_some_and(|hash| {
                        hash.len() == 64 && hash.bytes().all(|byte| byte.is_ascii_hexdigit())
                    })
        })
    })
}

pub(crate) fn bundle_env(bundle: &serde_json::Value) -> HashMap<String, String> {
    HashMap::from([(
        LAB_EXECUTION_BUNDLE_ENV.to_string(),
        serde_json::to_string(bundle).expect("Lab execution bundle serializes"),
    )])
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn reverse_broker_authority_preserves_the_job_private_bundle() {
        let bundle = serde_json::json!({
            "schema": LAB_EXECUTION_BUNDLE_SCHEMA,
            "binary": { "path": "/runner/bin/homeboy" },
            "admission": { "authority": "reverse_broker" },
            "extension_runtime_home": "/runner/job/home",
            "extensions": [{ "id": "fixture", "remote_path": "/runner/job/fixture", "content_hash": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" }]
        });

        assert!(validate_bundle_env(
            &bundle_env(&bundle),
            &["/runner/bin/homeboy".to_string(), "test".to_string()],
            &["fixture".to_string()],
        ));
    }
}
