use std::collections::BTreeMap;
use std::io::{BufRead, BufReader, Read};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;

use serde::{Deserialize, Serialize};
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

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct WorktreeProviderCleanupOutput {
    pub command: &'static str,
    pub mode: WorktreeProviderCleanupMode,
    pub provider_count: usize,
    pub success_count: usize,
    pub failure_count: usize,
    pub providers: Vec<WorktreeProviderCleanupResult>,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
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
    pub phase: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_progress: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub run_refs: Vec<WorktreeProviderRunRef>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub follow_up_command: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorktreeProviderRunRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub run_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_command: Option<String>,
}

/// A workspace returned by a command worktree provider's `list` command.
///
/// The command must return JSON with a `worktrees` array (optionally nested in
/// `data`). Each matching row must include all fields below so Homeboy never
/// guesses safety state for an externally managed destination.
#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderHandle {
    pub handle: String,
    pub path: String,
    pub branch: String,
    pub safety: WorktreeProviderHandleSafety,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct WorktreeProviderHandleSafety {
    pub dirty: bool,
    pub unpushed: bool,
    pub primary: bool,
}

/// Resolve an externally managed worktree handle without creating or adopting
/// a Homeboy record. This is intentionally a lookup-only boundary.
pub fn resolve_worktree_provider_handle(handle: &str) -> Result<WorktreeProviderHandle> {
    resolve_worktree_provider_handle_from_config(handle, &defaults::load_config())
}

pub fn resolve_worktree_provider_handle_from_config(
    handle: &str,
    config: &HomeboyConfig,
) -> Result<WorktreeProviderHandle> {
    let mut provider_ids = config
        .worktree_providers
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    provider_ids.sort();
    let mut attempted = Vec::new();

    for provider_id in provider_ids {
        let provider = &config.worktree_providers[&provider_id];
        if !provider.enabled {
            continue;
        }
        let Some(command) = provider.commands.list.as_ref() else {
            continue;
        };
        attempted.push(provider_id.clone());
        let worktrees = run_provider_list_command(&provider_id, command)?;
        if let Some(worktree) = worktrees.into_iter().find(|item| item.handle == handle) {
            validate_provider_handle(&provider_id, &worktree)?;
            return Ok(worktree);
        }
    }

    let configured = if attempted.is_empty() {
        "no enabled worktree provider has commands.list configured".to_string()
    } else {
        format!("checked provider(s): {}", attempted.join(", "))
    };
    Err(Error::validation_invalid_argument(
        "to_worktree",
        format!(
            "worktree handle `{handle}` is not a Homeboy task worktree and was not returned by a configured worktree provider ({configured})"
        ),
        Some(handle.to_string()),
        Some(vec![
            "Create the destination through its workspace provider, or use an existing Homeboy task worktree handle.".to_string(),
            "Configure an enabled worktree provider commands.list command that returns typed worktree path, branch, and safety metadata.".to_string(),
        ]),
    ))
}

fn run_provider_list_command(
    provider_id: &str,
    command: &[String],
) -> Result<Vec<WorktreeProviderHandle>> {
    let (program, args) = command
        .split_first()
        .filter(|(program, _)| !program.trim().is_empty())
        .ok_or_else(|| {
            Error::validation_invalid_argument(
                "worktree_providers.commands.list",
                format!(
                    "worktree provider `{provider_id}` list command must include an executable"
                ),
                Some(provider_id.to_string()),
                None,
            )
        })?;
    let output = Command::new(program).args(args).output().map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree provider `{provider_id}` list command could not start: {error}"),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    if !output.status.success() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` list command failed with exit code {}",
                output.status.code().unwrap_or(1)
            ),
            Some(provider_id.to_string()),
            Some(vec![String::from_utf8_lossy(&output.stderr)
                .trim()
                .to_string()]),
        ));
    }
    let value: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` list command returned invalid JSON: {error}"
            ),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    let worktrees = value.get("worktrees").or_else(|| value.get("data").and_then(|data| data.get("worktrees"))).ok_or_else(|| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree provider `{provider_id}` list response must include a worktrees array"),
            Some(provider_id.to_string()),
            None,
        )
    })?;
    serde_json::from_value::<Vec<WorktreeProviderHandle>>(worktrees.clone()).map_err(|error| {
        Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` returned invalid worktree metadata: {error}"
            ),
            Some(provider_id.to_string()),
            None,
        )
    })
}

fn validate_provider_handle(provider_id: &str, worktree: &WorktreeProviderHandle) -> Result<()> {
    let path = std::path::PathBuf::from(&worktree.path);
    if !path.is_dir() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` resolved `{}` to a missing directory {}",
                worktree.handle,
                path.display()
            ),
            Some(worktree.handle.clone()),
            None,
        ));
    }
    if worktree.branch.trim().is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!(
                "worktree provider `{provider_id}` resolved `{}` without a branch",
                worktree.handle
            ),
            Some(worktree.handle.clone()),
            None,
        ));
    }
    let blocked = [
        (worktree.safety.dirty, "dirty"),
        (worktree.safety.unpushed, "unpushed"),
        (worktree.safety.primary, "primary"),
    ]
    .into_iter()
    .filter_map(|(blocked, name)| blocked.then_some(name))
    .collect::<Vec<_>>();
    if !blocked.is_empty() {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree provider `{provider_id}` marked `{}` as {}; refusing to cook into an unsafe destination", worktree.handle, blocked.join(", ")),
            Some(worktree.handle.clone()),
            Some(vec!["Use a clean, pushed, non-primary provider-managed worktree on the intended branch.".to_string()]),
        ));
    }
    if crate::core::git::current_branch(&path).as_deref() != Some(worktree.branch.as_str()) {
        return Err(Error::validation_invalid_argument(
            "to_worktree",
            format!("worktree provider `{provider_id}` branch metadata for `{}` does not match the checkout branch", worktree.handle),
            Some(worktree.handle.clone()),
            Some(vec![format!("Provider reported branch `{}`; refresh provider metadata and retry.", worktree.branch)]),
        ));
    }
    Ok(())
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

    match Command::new(&command[0])
        .args(&command[1..])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(mut child) => {
            let stdout = child.stdout.take();
            let stderr = child.stderr.take();
            let stdout_lines = Arc::new(Mutex::new(Vec::new()));
            let stderr_lines = Arc::new(Mutex::new(Vec::new()));

            let stdout_handle = stdout.map(|stream| {
                collect_provider_stream(
                    provider_id.to_string(),
                    "stdout",
                    stream,
                    Arc::clone(&stdout_lines),
                )
            });
            let stderr_handle = stderr.map(|stream| {
                collect_provider_stream(
                    provider_id.to_string(),
                    "stderr",
                    stream,
                    Arc::clone(&stderr_lines),
                )
            });

            let wait_result = child.wait();
            if let Some(handle) = stdout_handle {
                let _ = handle.join();
            }
            if let Some(handle) = stderr_handle {
                let _ = handle.join();
            }

            let stdout = joined_lines(&stdout_lines);
            let stderr = joined_lines(&stderr_lines);
            let parsed_payload = parse_json_stdout(&stdout);
            let phase = provider_phase(&parsed_payload, &mode);
            let last_progress = provider_last_progress(&parsed_payload)
                .or_else(|| last_non_empty_line(&stdout))
                .or_else(|| last_non_empty_line(&stderr));
            let run_refs = provider_run_refs(&parsed_payload);
            let follow_up_command = provider_follow_up_command(&run_refs);

            let output_status = match wait_result {
                Ok(status) => status,
                Err(err) => {
                    return WorktreeProviderCleanupResult {
                        provider_id: provider_id.to_string(),
                        success: false,
                        mode,
                        command_run: Some(command.clone()),
                        status: None,
                        stdout,
                        stderr,
                        parsed_payload,
                        phase,
                        last_progress,
                        run_refs,
                        follow_up_command,
                        error: Some(format!("failed to wait for provider command: {err}")),
                    };
                }
            };
            let status = output_status.code();
            WorktreeProviderCleanupResult {
                provider_id: provider_id.to_string(),
                success: output_status.success(),
                mode,
                command_run: Some(command.clone()),
                status,
                stdout,
                stderr,
                parsed_payload,
                phase,
                last_progress,
                run_refs,
                follow_up_command,
                error: output_status
                    .success()
                    .then_some(())
                    .is_none()
                    .then(|| "provider command failed".to_string()),
            }
        }
        Err(err) => {
            let phase = Some(mode_phase(&mode).to_string());
            WorktreeProviderCleanupResult {
                provider_id: provider_id.to_string(),
                success: false,
                mode,
                command_run: Some(command.clone()),
                status: None,
                stdout: String::new(),
                stderr: String::new(),
                parsed_payload: None,
                phase,
                last_progress: None,
                run_refs: Vec::new(),
                follow_up_command: None,
                error: Some(format!("failed to execute provider command: {err}")),
            }
        }
    }
}

fn collect_provider_stream<R>(
    provider_id: String,
    stream_name: &'static str,
    stream: R,
    lines: Arc<Mutex<Vec<String>>>,
) -> thread::JoinHandle<()>
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        let reader = BufReader::new(stream);
        for line in reader.lines().map_while(std::result::Result::ok) {
            eprintln!("[cleanup.worktrees provider={provider_id} stream={stream_name}] {line}");
            if let Ok(mut guard) = lines.lock() {
                guard.push(line);
            }
        }
    })
}

fn joined_lines(lines: &Arc<Mutex<Vec<String>>>) -> String {
    lines
        .lock()
        .map(|guard| guard.join("\n"))
        .unwrap_or_default()
}

fn last_non_empty_line(output: &str) -> Option<String> {
    output
        .lines()
        .rev()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .map(ToString::to_string)
}

fn provider_failure(
    provider_id: &str,
    mode: WorktreeProviderCleanupMode,
    error: &str,
) -> WorktreeProviderCleanupResult {
    let phase = Some(mode_phase(&mode).to_string());
    WorktreeProviderCleanupResult {
        provider_id: provider_id.to_string(),
        success: false,
        mode,
        command_run: None,
        status: None,
        stdout: String::new(),
        stderr: String::new(),
        parsed_payload: None,
        phase,
        last_progress: None,
        run_refs: Vec::new(),
        follow_up_command: None,
        error: Some(error.to_string()),
    }
}

fn parse_json_stdout(stdout: &str) -> Option<Value> {
    let trimmed = stdout.trim();
    if trimmed.is_empty() {
        return None;
    }
    serde_json::from_str(trimmed).ok().or_else(|| {
        trimmed
            .lines()
            .rev()
            .map(str::trim)
            .find_map(|line| serde_json::from_str(line).ok())
    })
}

fn provider_phase(payload: &Option<Value>, mode: &WorktreeProviderCleanupMode) -> Option<String> {
    payload
        .as_ref()
        .and_then(|payload| first_string_for_keys(payload, &["phase", "state", "status"]))
        .or_else(|| Some(mode_phase(mode).to_string()))
}

fn provider_last_progress(payload: &Option<Value>) -> Option<String> {
    payload.as_ref().and_then(|payload| {
        first_string_for_keys(
            payload,
            &[
                "last_progress",
                "progress",
                "message",
                "summary",
                "last_observed_progress",
            ],
        )
    })
}

fn provider_run_refs(payload: &Option<Value>) -> Vec<WorktreeProviderRunRef> {
    let Some(payload) = payload else {
        return Vec::new();
    };
    let mut run_ids = Vec::new();
    let mut status_commands = Vec::new();
    collect_strings_for_keys(
        payload,
        &["run_id", "runId", "durable_run_id"],
        &mut run_ids,
    );
    collect_strings_for_keys(
        payload,
        &["status_command", "statusCommand", "status_cmd"],
        &mut status_commands,
    );
    collect_status_commands_from_arrays(payload, &mut status_commands);

    let len = run_ids.len().max(status_commands.len());
    (0..len)
        .map(|index| WorktreeProviderRunRef {
            run_id: run_ids.get(index).cloned(),
            status_command: status_commands.get(index).cloned(),
        })
        .collect()
}

fn provider_follow_up_command(refs: &[WorktreeProviderRunRef]) -> Option<String> {
    refs.iter().find_map(|row| row.status_command.clone())
}

fn mode_phase(mode: &WorktreeProviderCleanupMode) -> &'static str {
    match mode {
        WorktreeProviderCleanupMode::Preview => "preview",
        WorktreeProviderCleanupMode::Apply => "apply",
    }
}

fn first_string_for_keys(value: &Value, keys: &[&str]) -> Option<String> {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    return Some(value.to_string());
                }
            }
            map.values()
                .find_map(|value| first_string_for_keys(value, keys))
        }
        Value::Array(values) => values
            .iter()
            .find_map(|value| first_string_for_keys(value, keys)),
        _ => None,
    }
}

fn collect_strings_for_keys(value: &Value, keys: &[&str], out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for key in keys {
                if let Some(value) = map.get(*key).and_then(Value::as_str) {
                    if !out.contains(&value.to_string()) {
                        out.push(value.to_string());
                    }
                }
            }
            for value in map.values() {
                collect_strings_for_keys(value, keys, out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_strings_for_keys(value, keys, out);
            }
        }
        _ => {}
    }
}

fn collect_status_commands_from_arrays(value: &Value, out: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for key in [
                "status_commands",
                "statusCommands",
                "next_commands",
                "nextCommands",
            ] {
                if let Some(values) = map.get(key).and_then(Value::as_array) {
                    for value in values {
                        if let Some(command) = value.as_str().or_else(|| {
                            value
                                .get("command")
                                .and_then(Value::as_str)
                                .or_else(|| value.get("status_command").and_then(Value::as_str))
                        }) {
                            if !out.contains(&command.to_string()) {
                                out.push(command.to_string());
                            }
                        }
                    }
                }
            }
            for value in map.values() {
                collect_status_commands_from_arrays(value, out);
            }
        }
        Value::Array(values) => {
            for value in values {
                collect_status_commands_from_arrays(value, out);
            }
        }
        _ => {}
    }
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

    #[test]
    fn resolves_a_clean_provider_managed_handle_without_a_homeboy_record() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": false, "unpushed": false, "primary": false }
            }]
        }));
        let handle = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    list: Some(vec![script]),
                    ..Default::default()
                },
            }),
        )
        .expect("provider handle resolves");

        assert_eq!(handle.path, workspace.path().display().to_string());
        assert_eq!(handle.branch, "cook-target");
    }

    #[test]
    fn rejects_provider_handles_with_unsafe_safety_metadata() {
        let workspace = tempfile::tempdir().expect("workspace");
        git_init(workspace.path(), "cook-target");
        let script = fake_list_provider_script(serde_json::json!({
            "worktrees": [{
                "handle": "fixture@cook-target",
                "path": workspace.path(),
                "branch": "cook-target",
                "safety": { "dirty": true, "unpushed": false, "primary": false }
            }]
        }));
        let err = resolve_worktree_provider_handle_from_config(
            "fixture@cook-target",
            &config_with_provider(WorktreeProviderConfig {
                enabled: true,
                kind: WorktreeProviderKind::Command,
                apply_enabled: false,
                commands: WorktreeProviderCommands {
                    list: Some(vec![script]),
                    ..Default::default()
                },
            }),
        )
        .expect_err("dirty provider handle must be rejected");

        assert_eq!(err.code.as_str(), "validation.invalid_argument");
        assert!(err.message.contains("dirty"));
    }

    #[test]
    fn provider_apply_captures_phase_progress_and_durable_refs() {
        let script = fake_provider_script_with_refs();
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
                    cleanup_apply: Some(vec![script]),
                    ..Default::default()
                },
            }),
        )
        .expect("cleanup succeeds");

        let provider = &output.providers[0];
        assert_eq!(provider.phase.as_deref(), Some("running"));
        assert_eq!(provider.last_progress.as_deref(), Some("removed 10/20"));
        assert_eq!(provider.run_refs.len(), 1);
        assert_eq!(
            provider.run_refs[0].run_id.as_deref(),
            Some("cleanup-run-1")
        );
        assert_eq!(
            provider.run_refs[0].status_command.as_deref(),
            Some("provider status cleanup-run-1")
        );
        assert_eq!(
            provider.follow_up_command.as_deref(),
            Some("provider status cleanup-run-1")
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

    fn fake_provider_script_with_refs() -> String {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let script = dir.join("provider");
        fs::write(
            &script,
            concat!(
                "#!/bin/sh\n",
                "printf 'starting cleanup\\n' >&2\n",
                "printf '{\"phase\":\"running\",\"last_progress\":\"removed 10/20\",\"run_id\":\"cleanup-run-1\",\"status_command\":\"provider status cleanup-run-1\"}\\n'\n"
            ),
        )
        .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn fake_list_provider_script(output: Value) -> String {
        let dir = tempfile::tempdir().expect("tempdir").keep();
        let script = dir.join("provider");
        fs::write(&script, format!("#!/bin/sh\nprintf '%s\\n' '{}'\n", output))
            .expect("write script");
        make_executable(&script);
        script.to_string_lossy().to_string()
    }

    fn git_init(path: &std::path::Path, branch: &str) {
        let output = std::process::Command::new("git")
            .args(["init", "-b", branch])
            .current_dir(path)
            .output()
            .expect("initialize git repository");
        assert!(output.status.success());
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
