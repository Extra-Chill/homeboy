use std::collections::BTreeMap;
use std::process::Command;

use serde::Serialize;
use serde_json::Value;

use crate::core::defaults::{self, HomeboyConfig, WorktreeProviderConfig, WorktreeProviderKind};
use crate::core::error::{Error, Result};

#[derive(Debug, Clone)]
pub struct WorktreeProviderCleanupOptions {
    pub provider: Vec<String>,
    pub all_providers: bool,
    pub apply: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeProviderCleanupMode {
    Preview,
    Apply,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeProviderCleanupOutput {
    pub command: &'static str,
    pub mode: WorktreeProviderCleanupMode,
    pub provider_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub providers: Vec<WorktreeProviderCleanupResult>,
}

#[derive(Debug, Clone, Serialize)]
pub struct WorktreeProviderCleanupResult {
    pub provider_id: String,
    pub success: bool,
    pub mode: WorktreeProviderCleanupMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command_run: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<i32>,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stdout: String,
    #[serde(skip_serializing_if = "String::is_empty", default)]
    pub stderr: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub parsed_payload: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

pub fn cleanup_worktree_providers(
    options: WorktreeProviderCleanupOptions,
) -> Result<WorktreeProviderCleanupOutput> {
    cleanup_worktree_providers_from_config(options, defaults::load_config())
}

pub fn cleanup_worktree_providers_from_config(
    options: WorktreeProviderCleanupOptions,
    config: HomeboyConfig,
) -> Result<WorktreeProviderCleanupOutput> {
    validate_selection(&options)?;

    let mode = if options.apply {
        WorktreeProviderCleanupMode::Apply
    } else {
        WorktreeProviderCleanupMode::Preview
    };

    let providers = selected_providers(&options, &config)?;
    let mut results = Vec::new();

    for (provider_id, provider_config) in providers {
        results.push(run_provider_cleanup(
            &provider_id,
            provider_config,
            mode.clone(),
        ));
    }

    let success_count = results.iter().filter(|row| row.success).count();
    let failure_count = results.len().saturating_sub(success_count);

    Ok(WorktreeProviderCleanupOutput {
        command: "cleanup.worktrees",
        mode,
        provider_count: results.len(),
        success_count,
        failure_count,
        providers: results,
    })
}

fn validate_selection(options: &WorktreeProviderCleanupOptions) -> Result<()> {
    if options.all_providers && !options.provider.is_empty() {
        return Err(Error::validation_invalid_argument(
            "provider",
            "--provider cannot be combined with --all-providers",
            None,
            None,
        ));
    }
    if !options.all_providers && options.provider.is_empty() {
        return Err(Error::validation_missing_argument(vec![
            "--provider <id> or --all-providers".to_string(),
        ]));
    }
    Ok(())
}

fn selected_providers<'a>(
    options: &WorktreeProviderCleanupOptions,
    config: &'a HomeboyConfig,
) -> Result<Vec<(String, &'a WorktreeProviderConfig)>> {
    if options.all_providers {
        let sorted: BTreeMap<_, _> = config.worktree_providers.iter().collect();
        return Ok(sorted
            .into_iter()
            .filter_map(|(id, provider)| provider.enabled.then_some((id.clone(), provider)))
            .collect());
    }

    let mut providers = Vec::new();
    for provider_id in &options.provider {
        let Some(provider_config) = config.worktree_providers.get(provider_id) else {
            return Err(Error::validation_invalid_argument(
                "provider",
                format!("unknown worktree provider '{provider_id}'"),
                Some(provider_id.clone()),
                Some(config.worktree_providers.keys().cloned().collect()),
            ));
        };
        providers.push((provider_id.clone(), provider_config));
    }
    Ok(providers)
}

fn run_provider_cleanup(
    provider_id: &str,
    provider_config: &WorktreeProviderConfig,
    mode: WorktreeProviderCleanupMode,
) -> WorktreeProviderCleanupResult {
    if !provider_config.enabled {
        return provider_failure(provider_id, mode, "provider is disabled");
    }

    match provider_config.kind {
        WorktreeProviderKind::Command => {
            run_command_provider_cleanup(provider_id, provider_config, mode)
        }
    }
}

fn run_command_provider_cleanup(
    provider_id: &str,
    provider_config: &WorktreeProviderConfig,
    mode: WorktreeProviderCleanupMode,
) -> WorktreeProviderCleanupResult {
    if mode == WorktreeProviderCleanupMode::Apply && !provider_config.apply_enabled {
        return provider_failure(provider_id, mode, "provider apply is not enabled");
    }

    let command = match mode {
        WorktreeProviderCleanupMode::Preview => &provider_config.commands.cleanup_preview,
        WorktreeProviderCleanupMode::Apply => &provider_config.commands.cleanup_apply,
    };

    let Some(command) = command.as_ref() else {
        return provider_failure(
            provider_id,
            mode,
            "provider cleanup command is not configured",
        );
    };
    if command.is_empty() || command[0].trim().is_empty() {
        return provider_failure(
            provider_id,
            mode,
            "provider command argv must include an executable",
        );
    }

    match Command::new(&command[0]).args(&command[1..]).output() {
        Ok(output) => {
            let stdout = String::from_utf8_lossy(&output.stdout).to_string();
            let stderr = String::from_utf8_lossy(&output.stderr).to_string();
            let status = output.status.code();
            let parsed_payload = parse_json_stdout(&stdout);
            WorktreeProviderCleanupResult {
                provider_id: provider_id.to_string(),
                success: output.status.success(),
                mode,
                command_run: Some(command.clone()),
                status,
                stdout,
                stderr,
                parsed_payload,
                error: output
                    .status
                    .success()
                    .then_some(())
                    .is_none()
                    .then(|| "provider command failed".to_string()),
            }
        }
        Err(err) => WorktreeProviderCleanupResult {
            provider_id: provider_id.to_string(),
            success: false,
            mode,
            command_run: Some(command.clone()),
            status: None,
            stdout: String::new(),
            stderr: String::new(),
            parsed_payload: None,
            error: Some(format!("failed to execute provider command: {err}")),
        },
    }
}

fn provider_failure(
    provider_id: &str,
    mode: WorktreeProviderCleanupMode,
    error: &str,
) -> WorktreeProviderCleanupResult {
    WorktreeProviderCleanupResult {
        provider_id: provider_id.to_string(),
        success: false,
        mode,
        command_run: None,
        status: None,
        stdout: String::new(),
        stderr: String::new(),
        parsed_payload: None,
        error: Some(error.to_string()),
    }
}

fn parse_json_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok()
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::fs;

    use serde_json::json;

    use super::*;
    use crate::core::defaults::WorktreeProviderCommands;

    #[test]
    fn deserializes_worktree_provider_config() {
        let config: HomeboyConfig = serde_json::from_value(json!({
            "worktree_providers": {
                "fixture": {
                    "enabled": true,
                    "kind": "command",
                    "apply_enabled": true,
                    "commands": {
                        "cleanup_preview": ["fixture-bin", "preview"],
                        "cleanup_apply": ["fixture-bin", "apply"],
                        "artifacts_preview": ["fixture-bin", "artifacts-preview"]
                    }
                }
            }
        }))
        .expect("config deserializes");

        let provider = config.worktree_providers.get("fixture").expect("provider");
        assert!(provider.enabled);
        assert_eq!(provider.kind, WorktreeProviderKind::Command);
        assert!(provider.apply_enabled);
        assert_eq!(
            provider.commands.cleanup_preview.as_ref().expect("command"),
            &vec!["fixture-bin".to_string(), "preview".to_string()]
        );
    }

    #[test]
    fn dry_run_executes_preview_command_and_parses_json() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: false,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    cleanup_preview: Some(vec![script, "preview".to_string()]),
                    cleanup_apply: None,
                    ..Default::default()
                },
            }),
        )
        .expect("cleanup succeeds");

        assert_eq!(output.mode, WorktreeProviderCleanupMode::Preview);
        assert_eq!(output.success_count, 1);
        assert_eq!(output.failure_count, 0);
        assert_eq!(output.providers[0].status, Some(0));
        assert_eq!(
            output.providers[0].parsed_payload,
            Some(json!({ "mode": "preview" }))
        );
    }

    #[test]
    fn apply_refuses_when_provider_apply_is_disabled() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script, "apply".to_string()]),
                    ..Default::default()
                },
            }),
        )
        .expect("cleanup reports refusal");

        assert_eq!(output.success_count, 0);
        assert_eq!(output.failure_count, 1);
        assert_eq!(output.providers[0].command_run, None);
        assert_eq!(
            output.providers[0].error.as_deref(),
            Some("provider apply is not enabled")
        );
    }

    #[test]
    fn apply_executes_apply_command_when_enabled() {
        let script = fake_provider_script();
        let output = cleanup_worktree_providers_from_config(
            WorktreeProviderCleanupOptions {
                provider: vec!["fixture".to_string()],
                all_providers: false,
                apply: true,
            },
            config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: true,
                commands: WorktreeProviderCommands {
                    cleanup_apply: Some(vec![script, "apply".to_string()]),
                    ..Default::default()
                },
            }),
        )
        .expect("cleanup succeeds");

        assert_eq!(output.mode, WorktreeProviderCleanupMode::Apply);
        assert_eq!(output.success_count, 1);
        assert_eq!(
            output.providers[0].parsed_payload,
            Some(json!({ "mode": "apply" }))
        );
    }

    fn config_with_provider(provider: WorktreeProviderConfig) -> HomeboyConfig {
        let mut providers = HashMap::new();
        providers.insert("fixture".to_string(), provider);
        HomeboyConfig {
            worktree_providers: providers,
            ..HomeboyConfig::default()
        }
    }

    fn fake_provider_script() -> String {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let script = dir.join("provider");
        fs::write(&script, "#!/bin/sh\nprintf '{\"mode\":\"%s\"}\n' \"$1\"\n")
            .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    #[cfg(unix)]
    fn make_executable(path: &std::path::Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).expect("chmod");
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &std::path::Path) {}
}
