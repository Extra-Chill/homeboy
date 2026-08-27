//! `agent-task auth` command tests: legacy secrets file migration disclosure.
//!
//! Regression coverage for #13627: a mutating `auth` subcommand used to
//! silently migrate mappings out of the standalone `agent-task-secrets.json`
//! file into the global config, reporting success while leaving that file on
//! disk looking live. These tests exercise the real command handler end to
//! end and assert on the reported outcome, not on internal helper shape.

use super::support::*;

use super::super::args::{AgentTaskAuthArgs, AgentTaskAuthCommand, AgentTaskAuthRemoveArgs};
use super::super::auth::auth;

use homeboy::core::defaults::AgentTaskSecretSource;

/// Write the legacy standalone secrets file an unmigrated install would still
/// have on disk, mirroring the format `agent-task auth` used to write before
/// mappings moved into the global config.
fn write_legacy_secrets_file(
    home: &std::path::Path,
    secrets: &[(&str, &str)],
) -> std::path::PathBuf {
    let legacy_path = home.join(".config/homeboy/agent-task-secrets.json");
    std::fs::create_dir_all(legacy_path.parent().expect("config root"))
        .expect("create config root");
    let secrets: std::collections::HashMap<String, AgentTaskSecretSource> = secrets
        .iter()
        .map(|(name, env_var)| {
            (
                (*name).to_string(),
                AgentTaskSecretSource {
                    source: "env".to_string(),
                    env_var: Some((*env_var).to_string()),
                    path: None,
                    scope: None,
                    name: None,
                    field: None,
                    value: None,
                },
            )
        })
        .collect();
    std::fs::write(
        &legacy_path,
        serde_json::json!({ "secrets": secrets }).to_string(),
    )
    .expect("write legacy secrets file");
    legacy_path
}

#[test]
fn auth_remove_discloses_legacy_file_migration_and_removes_it() {
    with_isolated_home(|home| {
        let legacy_path = write_legacy_secrets_file(
            home.path(),
            &[
                ("KEPT_SECRET_ENV", "KEPT_SOURCE_ENV"),
                ("REMOVED_SECRET_ENV", "REMOVED_SOURCE_ENV"),
            ],
        );
        assert!(
            legacy_path.is_file(),
            "fixture must place the legacy secrets file on disk before the command runs"
        );

        let (value, status) = auth(AgentTaskAuthArgs {
            command: AgentTaskAuthCommand::Remove(AgentTaskAuthRemoveArgs {
                secret_env: "REMOVED_SECRET_ENV".to_string(),
                keychain: false,
            }),
        })
        .expect("auth remove succeeds");

        assert_eq!(status, 0);

        // The command must disclose that it just migrated the legacy file's
        // remaining mappings into the global config and removed it — not just
        // report the removal as if nothing else happened.
        let storage = value
            .get("secrets_storage")
            .expect("auth remove discloses the legacy-file migration in its output");
        assert_eq!(storage["config_pointer"], "/agent_task/secrets");
        assert_eq!(
            storage["removed_legacy_file"],
            serde_json::json!(legacy_path.to_string_lossy())
        );
        assert!(storage["config_file"].is_string());

        // The legacy file is gone, so it can no longer be mistaken for a live,
        // authoritative source of secret mappings.
        assert!(
            !legacy_path.exists(),
            "superseded legacy secrets file must be removed once migrated"
        );

        // The surviving mapping was carried into the global config, and the
        // removed one is genuinely gone from it, not just from the legacy file.
        let raw = std::fs::read_to_string(home.path().join(".config/homeboy/homeboy.json"))
            .expect("global config written");
        let config: serde_json::Value = serde_json::from_str(&raw).expect("global config json");
        let secrets = config
            .pointer("/agent_task/secrets")
            .expect("secrets migrated into global config");
        assert!(secrets.get("KEPT_SECRET_ENV").is_some());
        assert!(secrets.get("REMOVED_SECRET_ENV").is_none());
    });
}

#[test]
fn auth_remove_omits_legacy_disclosure_when_no_legacy_file_exists() {
    with_isolated_home(|_home| {
        let (value, status) = auth(AgentTaskAuthArgs {
            command: AgentTaskAuthCommand::Remove(AgentTaskAuthRemoveArgs {
                secret_env: "NEVER_MAPPED_SECRET_ENV".to_string(),
                keychain: false,
            }),
        })
        .expect("auth remove succeeds even when nothing was mapped");

        assert_eq!(status, 0);
        assert!(
            value.get("secrets_storage").is_none(),
            "nothing to disclose when there was no legacy file to migrate away from"
        );
    });
}
