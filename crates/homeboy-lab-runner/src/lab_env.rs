use std::collections::HashMap;

use homeboy_core::redaction::RedactionPolicy;
use homeboy_core::{Error, Result};

use super::execution::RUNNER_EXEC_WAIT_TIMEOUT_ENV;
use super::lab_workspaces::{is_rig_component_path_env_name, LabWorkspaceMappingEntry};
use homeboy_core::observation::{
    LAB_OFFLOAD_METADATA_ENV, PREVIEW_METADATA_ENV, PREVIEW_PUBLIC_URL_ENV,
};

const SETTINGS_DIAGNOSTICS_SCHEMA: &str = "homeboy/lab-offload-settings-env/v1";
pub(super) const DECLARED_DEPENDENCY_PATHS_ENV: &str = "HOMEBOY_DECLARED_DEPENDENCY_PATHS";
const DECLARED_DEPENDENCY_PATHS_SCHEMA: &str = "homeboy/lab-offload-declared-dependency-paths/v1";

pub(super) fn forward_env_if_present(env: &mut HashMap<String, String>, name: &str) {
    if let Ok(value) = std::env::var(name) {
        if !value.trim().is_empty() {
            env.insert(name.to_string(), value);
        }
    }
}

pub(super) fn forward_release_ci_env(env: &mut HashMap<String, String>) {
    for name in ["GITHUB_ACTIONS", "RELEASE_BLOCKING_COMMANDS"] {
        forward_env_if_present(env, name);
    }
}

pub(crate) fn build_lab_offload_env(lab_metadata: &serde_json::Value) -> HashMap<String, String> {
    HashMap::from([(
        LAB_OFFLOAD_METADATA_ENV.to_string(),
        serde_json::to_string(&subprocess_compatibility_lab_metadata(lab_metadata))
            .unwrap_or_default(),
    )])
}

/// Keep complete metadata in the control plane while bounding the compatibility
/// copy inherited by subprocesses. The manifest inventory remains available to
/// workspace verification before this projection is created.
fn subprocess_compatibility_lab_metadata(lab_metadata: &serde_json::Value) -> serde_json::Value {
    let mut compatibility = lab_metadata.clone();
    let Some(manifest) = compatibility
        .pointer_mut("/workspace_verification/content_manifest")
        .and_then(serde_json::Value::as_object_mut)
    else {
        return compatibility;
    };

    if manifest.remove("entries").is_some() {
        manifest.insert(
            "entries_omitted_from_env".to_string(),
            serde_json::Value::Bool(true),
        );
    }
    compatibility
}

/// Forward the preview metadata/public-url passthroughs plus release CI context
/// into a Lab offload env. Centralizes the repeated forward sequence shared by
/// the offload dispatch paths so they stay in lock-step.
pub(super) fn forward_preview_and_release_ci_env(env: &mut HashMap<String, String>) {
    forward_env_if_present(env, PREVIEW_METADATA_ENV);
    forward_env_if_present(env, PREVIEW_PUBLIC_URL_ENV);
    forward_release_ci_env(env);
}

/// Build a fresh Lab offload env from `lab_metadata` and forward the preview and
/// release CI passthroughs in one step.
pub(super) fn build_lab_offload_env_with_passthroughs(
    lab_metadata: &serde_json::Value,
) -> HashMap<String, String> {
    let mut env = build_lab_offload_env(lab_metadata);
    forward_preview_and_release_ci_env(&mut env);
    env
}

pub(super) fn forward_rig_component_path_env(
    env: &mut HashMap<String, String>,
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> Result<serde_json::Value> {
    forward_rig_component_path_env_entries(env, workspace_mapping, std::env::vars())
}

pub(super) fn forward_declared_dependency_paths_env(
    env: &mut HashMap<String, String>,
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> serde_json::Value {
    let dependencies = declared_dependency_paths(workspace_mapping);
    if !dependencies.is_empty() {
        env.insert(
            DECLARED_DEPENDENCY_PATHS_ENV.to_string(),
            serde_json::to_string(&dependencies).unwrap_or_else(|_| "{}".to_string()),
        );
    }

    serde_json::json!({
        "schema": DECLARED_DEPENDENCY_PATHS_SCHEMA,
        "env_name": DECLARED_DEPENDENCY_PATHS_ENV,
        "forwarded_to_runner": !dependencies.is_empty(),
        "count": dependencies.len(),
        "dependencies": dependencies,
    })
}

fn declared_dependency_paths(
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> serde_json::Map<String, serde_json::Value> {
    workspace_mapping
        .iter()
        .filter_map(|entry| {
            let freshness = entry.dependency_freshness()?;
            if entry.role() != "validation_dependency" {
                return None;
            }
            let id = freshness.get("id")?.as_str()?.trim();
            if id.is_empty() {
                return None;
            }
            Some((
                id.to_string(),
                serde_json::json!({
                    "local_path": entry.local_path(),
                    "remote_path": entry.remote_path(),
                    "evidence_path": freshness.get("evidence_path").cloned().unwrap_or(serde_json::Value::Null),
                }),
            ))
        })
        .collect()
}

fn forward_rig_component_path_env_entries(
    env: &mut HashMap<String, String>,
    workspace_mapping: &[LabWorkspaceMappingEntry],
    entries: impl IntoIterator<Item = (String, String)>,
) -> Result<serde_json::Value> {
    let mut forwarded = Vec::new();
    let mut entries = entries
        .into_iter()
        .filter(|(name, value)| is_rig_component_path_env_name(name) && !value.trim().is_empty())
        .collect::<Vec<_>>();
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    for (name, value) in entries {
        let Some(remote_value) = remap_rig_component_path_env_value(&value, workspace_mapping)
        else {
            return Err(Error::validation_invalid_argument(
                name.clone(),
                format!(
                    "Lab offload cannot forward `{name}` because its controller-side path was not synced to the runner"
                ),
                Some(value.clone()),
                Some(vec![
                    format!("Controller-side value: {value}"),
                    "Use an existing local checkout/component path so Lab can sync and translate it, unset the variable to use the rig default, or use --placement local to keep the check local.".to_string(),
                ]),
            ));
        };
        env.insert(name.clone(), remote_value.clone());
        forwarded.push(serde_json::json!({
            "name": name,
            "forwarded_to_runner": true,
            "translated": true,
            "controller_value": value,
            "runner_value": remote_value,
        }));
    }

    Ok(serde_json::json!({
        "schema": "homeboy/lab-offload-rig-component-path-env/v1",
        "forwarded": forwarded,
    }))
}

fn remap_rig_component_path_env_value(
    value: &str,
    workspace_mapping: &[LabWorkspaceMappingEntry],
) -> Option<String> {
    let expanded = shellexpand::tilde(value).to_string();
    let canonical = std::path::Path::new(&expanded)
        .canonicalize()
        .ok()
        .map(|path| path.to_string_lossy().to_string());

    let mut ordered = workspace_mapping.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|entry| std::cmp::Reverse(entry.local_path().len()));
    for entry in ordered {
        let mut candidates = vec![value];
        if let Some(canonical) = canonical.as_deref() {
            candidates.insert(0, canonical);
        }
        for candidate in candidates {
            if candidate == entry.local_path() {
                return Some(entry.remote_path().to_string());
            }
            let prefix = format!("{}/", entry.local_path().trim_end_matches('/'));
            if let Some(rest) = candidate.strip_prefix(&prefix) {
                return Some(format!(
                    "{}/{}",
                    entry.remote_path().trim_end_matches('/'),
                    rest
                ));
            }
        }
    }
    None
}

pub(super) fn settings_env_diagnostics(
    normalized_args: &[String],
    forwarded_env: &HashMap<String, String>,
) -> serde_json::Value {
    let settings = parsed_setting_args(normalized_args)
        .into_iter()
        .map(|setting| {
            let env_name = format!("HOMEBOY_SETTINGS_{}", setting.key.to_uppercase());
            let redacted = should_redact_setting(&setting.key, &setting.value);
            serde_json::json!({
                "key": setting.key,
                "source": setting.source,
                "env_name": env_name,
                "forwarded_to_runner": true,
                "forwarded_as": "argv",
                "remote_export_expected": true,
                "value_preview": redacted_value_preview(&setting.value, redacted),
                "redacted": redacted,
            })
        })
        .collect::<Vec<_>>();

    let mut env_names = forwarded_env.keys().cloned().collect::<Vec<_>>();
    env_names.sort();
    let forwarded_environment = env_names
        .into_iter()
        .map(|name| {
            serde_json::json!({
                "name": name,
                "forwarded_to_runner": true,
                "value_preview": "<redacted>",
                "redacted": true,
            })
        })
        .collect::<Vec<_>>();

    serde_json::json!({
        "schema": SETTINGS_DIAGNOSTICS_SCHEMA,
        "settings": settings,
        "forwarded_environment": forwarded_environment,
    })
}

pub(super) fn misplaced_runner_exec_wait_timeout_warning(
    normalized_args: &[String],
) -> Option<String> {
    if std::env::var_os(RUNNER_EXEC_WAIT_TIMEOUT_ENV).is_some() {
        return None;
    }

    parsed_setting_args(normalized_args)
        .into_iter()
        .any(|setting| setting.key == RUNNER_EXEC_WAIT_TIMEOUT_ENV)
        .then(|| {
            format!(
                "Lab offload: `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}` was supplied as a workload setting, but runner wait timeout is controlled by the controller process. Set it before invoking homeboy instead, e.g. `{RUNNER_EXEC_WAIT_TIMEOUT_ENV}=2400 homeboy ...`."
            )
        })
}

#[derive(Debug, PartialEq, Eq)]
struct ParsedSettingArg {
    source: &'static str,
    key: String,
    value: String,
}

fn parsed_setting_args(args: &[String]) -> Vec<ParsedSettingArg> {
    let mut parsed = Vec::new();
    let mut index = 0;

    while index < args.len() {
        let arg = &args[index];
        if arg == "--setting" || arg == "--setting-json" {
            if let Some(raw) = args.get(index + 1) {
                if let Some(setting) = parse_setting_pair(setting_source(arg), raw) {
                    parsed.push(setting);
                }
            }
            index += 2;
            continue;
        }

        if let Some(raw) = arg.strip_prefix("--setting=") {
            if let Some(setting) = parse_setting_pair("setting", raw) {
                parsed.push(setting);
            }
        } else if let Some(raw) = arg.strip_prefix("--setting-json=") {
            if let Some(setting) = parse_setting_pair("setting_json", raw) {
                parsed.push(setting);
            }
        }

        index += 1;
    }

    parsed
}

fn setting_source(arg: &str) -> &'static str {
    if arg == "--setting-json" {
        "setting_json"
    } else {
        "setting"
    }
}

fn parse_setting_pair(source: &'static str, raw: &str) -> Option<ParsedSettingArg> {
    let (key, value) = raw.split_once('=')?;
    if key.trim().is_empty() {
        return None;
    }

    Some(ParsedSettingArg {
        source,
        key: key.to_string(),
        value: value.to_string(),
    })
}

fn should_redact_setting(key: &str, value: &str) -> bool {
    let policy = RedactionPolicy::default();
    policy.is_sensitive_key(key) || policy.redact_string(value) != value
}

fn redacted_value_preview(value: &str, redacted: bool) -> String {
    if redacted {
        return "<redacted>".to_string();
    }

    const MAX_PREVIEW_CHARS: usize = 160;
    if value.chars().count() <= MAX_PREVIEW_CHARS {
        return value.to_string();
    }

    let mut preview = value.chars().take(MAX_PREVIEW_CHARS).collect::<String>();
    preview.push_str("...");
    preview
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lab_workspaces::workspace_mapping_entry;
    use crate::{RunnerWorkspaceSyncMode, RunnerWorkspaceSyncOutput};
    use std::sync::Mutex;

    static ENV_TEST_LOCK: Mutex<()> = Mutex::new(());

    struct EnvVarGuard {
        name: &'static str,
        prior: Option<String>,
    }

    impl EnvVarGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::set_var(name, value);
            Self { name, prior }
        }

        fn unset(name: &'static str) -> Self {
            let prior = std::env::var(name).ok();
            std::env::remove_var(name);
            Self { name, prior }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            match &self.prior {
                Some(value) => std::env::set_var(self.name, value),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn parsed_setting_args_reads_split_and_equals_forms() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "tool_bin=/tmp/tool.js".to_string(),
            "--setting-json={\"ignored\":true}".to_string(),
            "--setting-json".to_string(),
            "retries=3".to_string(),
            "--setting=mode=fast".to_string(),
        ];

        assert_eq!(
            parsed_setting_args(&args),
            vec![
                ParsedSettingArg {
                    source: "setting",
                    key: "tool_bin".to_string(),
                    value: "/tmp/tool.js".to_string(),
                },
                ParsedSettingArg {
                    source: "setting_json",
                    key: "retries".to_string(),
                    value: "3".to_string(),
                },
                ParsedSettingArg {
                    source: "setting",
                    key: "mode".to_string(),
                    value: "fast".to_string(),
                },
            ]
        );
    }

    #[test]
    fn settings_env_diagnostics_reports_expected_env_names_and_redacts_secrets() {
        let args = vec![
            "homeboy".to_string(),
            "trace".to_string(),
            "--setting".to_string(),
            "tool_bin=/tmp/tool.js".to_string(),
            "--setting".to_string(),
            "api_token=secret-value".to_string(),
        ];
        let mut env = HashMap::new();
        env.insert(
            LAB_OFFLOAD_METADATA_ENV.to_string(),
            "{\"schema\":\"homeboy/lab-offload/v1\"}".to_string(),
        );

        let diagnostics = settings_env_diagnostics(&args, &env);

        assert_eq!(diagnostics["schema"], SETTINGS_DIAGNOSTICS_SCHEMA);
        assert_eq!(diagnostics["settings"][0]["key"], "tool_bin");
        assert_eq!(
            diagnostics["settings"][0]["env_name"],
            "HOMEBOY_SETTINGS_TOOL_BIN"
        );
        assert_eq!(diagnostics["settings"][0]["value_preview"], "/tmp/tool.js");
        assert_eq!(diagnostics["settings"][0]["forwarded_as"], "argv");
        assert_eq!(diagnostics["settings"][0]["remote_export_expected"], true);
        assert_eq!(
            diagnostics["settings"][1]["env_name"],
            "HOMEBOY_SETTINGS_API_TOKEN"
        );
        assert_eq!(diagnostics["settings"][1]["value_preview"], "<redacted>");
        assert_eq!(diagnostics["settings"][1]["redacted"], true);
        assert_eq!(
            diagnostics["forwarded_environment"][0]["name"],
            LAB_OFFLOAD_METADATA_ENV
        );
        assert_eq!(
            diagnostics["forwarded_environment"][0]["value_preview"],
            "<redacted>"
        );
    }

    #[test]
    fn lab_offload_env_compacts_workspace_manifest_entries() {
        let entries = (0..12_000)
            .map(|index| serde_json::json!({"path": format!("fixtures/{index:05}/{}.txt", "x".repeat(200)), "kind": "file"}))
            .collect::<Vec<_>>();
        let metadata = serde_json::json!({
            "schema": "homeboy/lab-offload/v1",
            "workspace_verification": {
                "schema": "homeboy/lab-workspace-verification/v2",
                "content_hash_algorithm": "homeboy-workspace-content-v3",
                "permission_policy": "portable-content-only",
                "content_hash": "sha256:full-workspace",
                "content_manifest": {
                    "entry_count": entries.len(),
                    "digest": "sha256:manifest",
                    "entries": entries,
                },
            },
        });
        let full = serde_json::to_string(&metadata).expect("full metadata serializes");
        let env = build_lab_offload_env(&metadata);
        let compact = env
            .get(LAB_OFFLOAD_METADATA_ENV)
            .expect("subprocess metadata is present");
        let parsed: serde_json::Value = serde_json::from_str(compact).expect("metadata parses");

        assert!(full.len() > 1_000_000);
        assert!(compact.len() < 2_000);
        assert_eq!(
            metadata["workspace_verification"]["content_manifest"]["entries"]
                .as_array()
                .map(Vec::len),
            Some(12_000)
        );
        assert_eq!(
            parsed["workspace_verification"]["schema"],
            "homeboy/lab-workspace-verification/v2"
        );
        assert_eq!(
            parsed["workspace_verification"]["content_hash_algorithm"],
            "homeboy-workspace-content-v3"
        );
        assert_eq!(
            parsed["workspace_verification"]["permission_policy"],
            "portable-content-only"
        );
        assert_eq!(
            parsed["workspace_verification"]["content_hash"],
            "sha256:full-workspace"
        );
        assert_eq!(
            parsed["workspace_verification"]["content_manifest"]["entry_count"],
            12_000
        );
        assert_eq!(
            parsed["workspace_verification"]["content_manifest"]["digest"],
            "sha256:manifest"
        );
        assert_eq!(
            parsed["workspace_verification"]["content_manifest"]["entries_omitted_from_env"],
            true
        );
        assert!(parsed["workspace_verification"]["content_manifest"]
            .get("entries")
            .is_none());
        assert!(!compact.contains("fixtures/00000/"));
    }

    #[test]
    fn rig_component_path_env_is_forwarded_with_runner_path() {
        let mapping = vec![workspace_mapping_entry(
            "rig_component_path_env",
            &RunnerWorkspaceSyncOutput {
                variant: "workspace_sync",
                command: "runner.workspace.sync",
                runner_id: "homeboy-lab".to_string(),
                local_path: "/Users/user/Developer/example-component".to_string(),
                remote_path: "/home/user/Developer/example-component".to_string(),
                materialization_plan: crate::RunnerWorkspaceMaterializationPlan::from_test_parts(
                    "/home/user/Developer",
                    "/Users/user/Developer/example-component",
                    "example-component",
                    "/home/user/Developer/example-component",
                    crate::RunnerWorkspaceSyncMode::Snapshot,
                    "snapshot",
                ),
                current_workspace: crate::RunnerWorkspaceCurrentSummary {
                    local_path: "/Users/user/Developer/example-component".to_string(),
                    remote_path: "/home/user/Developer/example-component".to_string(),
                    sync_mode: crate::RunnerWorkspaceSyncMode::Snapshot,
                    materialized: true,
                    source_commit: None,
                    source_ref: None,
                    source_dirty: None,
                    synthetic_checkout_commit: None,
                    synthetic_checkout_ref: None,
                    synthetic_checkout_tree: None,
                },
                workspace_lease: crate::RunnerWorkspaceLease {
                    runner_id: "homeboy-lab".to_string(),
                    local_path: "/Users/user/Developer/example-component".to_string(),
                    remote_path: "/home/user/Developer/example-component".to_string(),
                    sync_mode: "snapshot".to_string(),
                    materialized: true,
                    lifecycle_owner: crate::RunnerLifecycleOwner::Controller,
                    source_commit: None,
                    source_ref: None,
                    source_dirty: None,
                },
                resource_lifecycle: crate::workspace_resource_lifecycle(
                    "homeboy-lab",
                    "/home/user/Developer/example-component",
                    None,
                    homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
                ),
                sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                snapshot_identity: "snapshot".to_string(),
                counts: crate::ByteFileCounts { files: 1, bytes: 1 },
                excludes: Vec::new(),
                includes: Vec::new(),
                workspace_cleanliness: "snapshot_unique_workspace".to_string(),
                validation_dependencies: Vec::new(),
            },
        )];

        let mut env = HashMap::new();
        let metadata = forward_rig_component_path_env_entries(
            &mut env,
            &mapping,
            [(
                "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__COMPONENT".to_string(),
                "/Users/user/Developer/example-component/includes".to_string(),
            )],
        )
        .expect("forward env");

        assert_eq!(
            env.get("HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__COMPONENT")
                .map(String::as_str),
            Some("/home/user/Developer/example-component/includes")
        );
        assert_eq!(
            metadata["forwarded"][0]["runner_value"],
            "/home/user/Developer/example-component/includes"
        );
    }

    #[test]
    fn rig_component_path_env_fails_when_path_was_not_synced() {
        let mut env = HashMap::new();

        let err = forward_rig_component_path_env_entries(
            &mut env,
            &[],
            [(
                "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__COMPONENT".to_string(),
                "/Users/user/Developer/unsynced-component".to_string(),
            )],
        )
        .expect_err("unsynced path");

        assert_eq!(
            err.details["field"],
            "HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__COMPONENT"
        );
        assert!(err.message.contains("was not synced to the runner"));
        assert!(!env.contains_key("HOMEBOY_RIG_COMPONENT_PATH__TEST_RIG__COMPONENT"));
    }

    #[test]
    fn declared_dependency_paths_env_exports_validation_dependency_mapping() {
        let mapping = vec![
            crate::lab_workspaces::workspace_mapping_entry_for_validation_dependency(
                &crate::RunnerValidationDependencySyncOutput {
                    id: "static-site-importer".to_string(),
                    role: "validation_dependency".to_string(),
                    local_path: "/Users/user/Developer/static-site-importer".to_string(),
                    remote_path: "/home/user/Developer/job-123/static-site-importer".to_string(),
                    evidence_path: "/home/user/Developer/job-123/static-site-importer/.homeboy/lab-source-evidence.json".to_string(),
                },
            ),
            workspace_mapping_entry(
                "extra",
                &RunnerWorkspaceSyncOutput {
                    variant: "workspace_sync",
                    command: "runner.workspace.sync",
                    runner_id: "homeboy-lab".to_string(),
                    local_path: "/Users/user/Developer/other".to_string(),
                    remote_path: "/home/user/Developer/other".to_string(),
                    materialization_plan: crate::RunnerWorkspaceMaterializationPlan::from_test_parts(
                        "/home/user/Developer",
                        "/Users/user/Developer/other",
                        "other",
                        "/home/user/Developer/other",
                        crate::RunnerWorkspaceSyncMode::Snapshot,
                        "snapshot",
                    ),
                    current_workspace: crate::RunnerWorkspaceCurrentSummary {
                        local_path: "/Users/user/Developer/other".to_string(),
                        remote_path: "/home/user/Developer/other".to_string(),
                        sync_mode: crate::RunnerWorkspaceSyncMode::Snapshot,
                        materialized: true,
                        source_commit: None,
                        source_ref: None,
                        source_dirty: None,
                        synthetic_checkout_commit: None,
                        synthetic_checkout_ref: None,
                        synthetic_checkout_tree: None,
                    },
                    workspace_lease: crate::RunnerWorkspaceLease {
                        runner_id: "homeboy-lab".to_string(),
                        local_path: "/Users/user/Developer/other".to_string(),
                        remote_path: "/home/user/Developer/other".to_string(),
                        sync_mode: "snapshot".to_string(),
                        materialized: true,
                        lifecycle_owner: crate::RunnerLifecycleOwner::Controller,
                        source_commit: None,
                        source_ref: None,
                        source_dirty: None,
                    },
                    resource_lifecycle: crate::workspace_resource_lifecycle(
                        "homeboy-lab",
                        "/home/user/Developer/other",
                        None,
                        homeboy_core::resource_lifecycle_index::ResourceCleanupPolicy::DeleteOnSuccess,
                    ),
                    sync_mode: RunnerWorkspaceSyncMode::Snapshot,
                    snapshot_identity: "snapshot".to_string(),
                    counts: crate::ByteFileCounts { files: 1, bytes: 1 },
                    excludes: Vec::new(),
                    includes: Vec::new(),
                    workspace_cleanliness: "snapshot_unique_workspace".to_string(),
                    validation_dependencies: Vec::new(),
                },
            ),
        ];
        let mut env = HashMap::new();

        let metadata = forward_declared_dependency_paths_env(&mut env, &mapping);
        let exported: serde_json::Value = serde_json::from_str(
            env.get(DECLARED_DEPENDENCY_PATHS_ENV)
                .expect("dependency paths env"),
        )
        .expect("env json");

        assert_eq!(metadata["schema"], DECLARED_DEPENDENCY_PATHS_SCHEMA);
        assert_eq!(metadata["forwarded_to_runner"], true);
        assert_eq!(metadata["count"], 1);
        assert_eq!(
            exported["static-site-importer"]["remote_path"],
            "/home/user/Developer/job-123/static-site-importer"
        );
        assert_eq!(
            exported["static-site-importer"]["local_path"],
            "/Users/user/Developer/static-site-importer"
        );
        assert!(exported.get("other").is_none());
    }

    #[test]
    fn declared_dependency_paths_env_is_absent_without_dependencies() {
        let mut env = HashMap::new();

        let metadata = forward_declared_dependency_paths_env(&mut env, &[]);

        assert_eq!(metadata["forwarded_to_runner"], false);
        assert_eq!(metadata["count"], 0);
        assert!(!env.contains_key(DECLARED_DEPENDENCY_PATHS_ENV));
    }

    #[test]
    fn forward_release_ci_env_preserves_release_gate_context() {
        let _github_actions = EnvVarGuard::set("GITHUB_ACTIONS", "true");
        let _blocking = EnvVarGuard::set("RELEASE_BLOCKING_COMMANDS", "lint,test");
        let mut env = HashMap::new();

        forward_release_ci_env(&mut env);

        assert_eq!(env.get("GITHUB_ACTIONS").map(String::as_str), Some("true"));
        assert_eq!(
            env.get("RELEASE_BLOCKING_COMMANDS").map(String::as_str),
            Some("lint,test")
        );
    }

    #[test]
    fn warns_when_runner_exec_wait_timeout_is_only_a_workload_setting() {
        let _lock = ENV_TEST_LOCK.lock().expect("env test lock");
        let _guard = EnvVarGuard::unset(RUNNER_EXEC_WAIT_TIMEOUT_ENV);
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting".to_string(),
            format!("{RUNNER_EXEC_WAIT_TIMEOUT_ENV}=2400"),
        ];

        let warning = misplaced_runner_exec_wait_timeout_warning(&args).expect("warning");

        assert!(warning.contains(RUNNER_EXEC_WAIT_TIMEOUT_ENV));
        assert!(warning.contains("controller process"));
        assert!(warning.contains("workload setting"));
    }

    #[test]
    fn skips_runner_exec_wait_timeout_setting_warning_when_controller_env_is_set() {
        let _lock = ENV_TEST_LOCK.lock().expect("env test lock");
        let _guard = EnvVarGuard::set(RUNNER_EXEC_WAIT_TIMEOUT_ENV, "2400");
        let args = vec![
            "homeboy".to_string(),
            "bench".to_string(),
            "--setting".to_string(),
            format!("{RUNNER_EXEC_WAIT_TIMEOUT_ENV}=2400"),
        ];

        assert_eq!(misplaced_runner_exec_wait_timeout_warning(&args), None);
    }
}
